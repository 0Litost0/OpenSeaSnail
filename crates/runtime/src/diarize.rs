//! 说话人分离 driver + 合并算法（ST-M3.6）。
//!
//! `capabilities.diarization == External` 时（whisper.cpp 后端），转写链路另起
//! `sherpa-onnx-diarize` 一次性进程产 speaker 标签：stdin 喂归一 WAV、stdout
//! 返「时间戳 + 说话人标签」行（设计「说话人分离设计」）。driver 解析 stdout
//! 行 → [`DiarizationSegment`]，再由 [`merge_speakers`] 按时间窗重叠对齐进
//! whisper segments 的 `speaker` 字段（MVP 标 A/B/C，不做声纹——设计「MVP 仅
//! 标注说话人 A/B/C」）。
//!
//! **测试边界**（设计「测试策略」line 870）：[`merge_speakers`] +
//! [`parse_diarize_stdout`] 用假 segments + 假 stdout 行单测覆盖（无二进制依赖）；
//! [`DiarizeDriver::run`] 真二进制 + stdout 行格式属上机项（roadmap line 192，
//! 类比 whisper-server 的 `/inference` 经 M3.2 上机确认），走 `#[ignore]` 上机
//! 回归。pipeline 编排接线（[`seasnail_daemon::api::sessions`]）为 best-effort
//! 胶水——路径未配 / 进程失败 / 解析失败 → warn + `speaker` 缺省，转写不致失败
//! （diarization 是富化，非转写成败条件）。

use crate::contract::OpenAiSegments;
use crate::error::RuntimeError;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// sherpa-onnx-diarize 解析后的一条说话人片段：时间窗（秒）+ 原始说话人标签。
#[derive(Debug, Clone, PartialEq)]
pub struct DiarizationSegment {
    pub start: f64,
    pub end: f64,
    pub speaker: String,
}

/// 解析 sherpa-onnx-diarize stdout 行 → [`DiarizationSegment`] 序列。
///
/// **best-guess 行格式**（上机确认前）：每行 `start end speaker` 或
/// `start,end,speaker`——逗号统一替空白后按空白切分；`start`/`end` 秒（浮点），
/// 其余 token 拼为说话人标签。空行 / `#` 注释 / 字段不足 3 / 前两段非数值 → 跳过
/// （容忍上机格式微调）。**有非空行却无一可解析 → `Decode` 错**（信号格式不符，
/// 调用方 best-effort 跳过）。**TODO 上机确认** sherpa-onnx-diarize 真实行格式
/// （roadmap line 192），据实调整切分/字段顺序。
pub fn parse_diarize_stdout(s: &str) -> Result<Vec<DiarizationSegment>, RuntimeError> {
    let mut out = Vec::new();
    let mut non_empty = 0u32;
    for raw in s.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        non_empty += 1;
        // 逗号→空白统一，兼容 "0.0,1.0,spk0" 与 "0.0 1.0 spk0"。
        let normalized = line.replace(',', " ");
        let parts: Vec<&str> = normalized.split_whitespace().collect();
        if parts.len() < 3 {
            continue;
        }
        let start = match parts[0].parse::<f64>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let end = match parts[1].parse::<f64>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        out.push(DiarizationSegment {
            start,
            end,
            speaker: parts[2..].join(" "),
        });
    }
    if non_empty > 0 && out.is_empty() {
        return Err(RuntimeError::Decode(format!(
            "diarize stdout had {non_empty} non-empty line(s) but none parsed"
        )));
    }
    Ok(out)
}

/// 按 diarization 时间窗对齐合并 speaker 标签进 whisper segments。
///
/// 对每个 segment，取与 `[start,end]` **时间重叠最大**的 diarization 片段说话人
/// （重叠 = `max(0, min(seg.end, dia.end) - max(seg.start, dia.start))`）；全无
/// 重叠（间隙）→ 取中点最近的片段兜底（whisper segments 连续覆盖音频，罕见真
/// 间隙；兜底确保 diar 非空时每段有 speaker）。同重叠取先见者。说话人标签归一
/// 为 `A`/`B`/`C`…——按 diarization 片段 `start` 升序的**首次出现顺序**编号
/// （`speaker_00` 不必是 A；先开口者为 A），对齐设计「MVP 仅标注说话人 A/B/C」。
///
/// diarization 空 → segments `speaker` 保持 `None`（builtin 后端由 runtime 自带
/// speaker 不经本函数；none 后端 pipeline 不调本函数）。
pub fn merge_speakers(segs: &mut OpenAiSegments, diar: &[DiarizationSegment]) {
    if diar.is_empty() {
        return;
    }
    // 归一表：原始标签 → A/B/C…（按 start 升序首次出现顺序）。
    let mut order: Vec<&DiarizationSegment> = diar.iter().collect();
    order.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut labels: Vec<String> = Vec::new();
    for d in &order {
        if !labels.iter().any(|l| l == &d.speaker) {
            labels.push(d.speaker.clone());
        }
    }
    let label = |raw: &str| -> String {
        labels
            .iter()
            .position(|l| l == raw)
            .map(|i| {
                char::from_u32(b'A' as u32 + i as u32)
                    .unwrap_or('?')
                    .to_string()
            })
            .unwrap_or_default()
    };

    for seg in segs.segments.iter_mut() {
        // 第一遍：时间重叠最大者。
        let mut chosen: Option<&DiarizationSegment> = None;
        let mut best_overlap = 0.0_f64;
        for d in diar {
            let overlap = (seg.end.min(d.end) - seg.start.max(d.start)).max(0.0);
            if overlap > best_overlap {
                best_overlap = overlap;
                chosen = Some(d);
            }
        }
        // 第二遍（兜底）：全无重叠 → 中点最近者。
        if chosen.is_none() {
            let mid = (seg.start + seg.end) / 2.0;
            let mut best_dist = f64::INFINITY;
            for d in diar {
                let dist = ((d.start + d.end) / 2.0 - mid).abs();
                if dist < best_dist {
                    best_dist = dist;
                    chosen = Some(d);
                }
            }
        }
        if let Some(d) = chosen {
            seg.speaker = Some(label(&d.speaker));
        }
    }
}

/// sherpa-onnx-diarize 一次性进程 driver（external diarization 后端）。
///
/// `run`：spawn `binary --model-dir <dir>`，stdin 喂归一 WAV 字节，capture stdout，
/// [`parse_diarize_stdout`] 解析。stdin 写 / stdout 读 / stderr 读 三者并发防管道
/// 缓冲死锁（音频可能数 MB）。**调用链上机确认**（roadmap line 192）：二进制
/// 路径 / CLI args / stdin 协议 / stdout 行格式均 best-guess，待真实
/// sherpa-onnx-diarize 上机后据实调整（类比 whisper-server `--port`/`--model`
/// 经 M3.2 上机确认）。
pub struct DiarizeDriver {
    binary: PathBuf,
    model_dir: PathBuf,
}

impl DiarizeDriver {
    pub fn new(binary: PathBuf, model_dir: PathBuf) -> Self {
        Self { binary, model_dir }
    }

    /// 一次性 diarize：spawn → stdin 喂 WAV → 读 stdout → 解析。
    pub async fn run(
        &self,
        wav: &std::path::Path,
    ) -> Result<Vec<DiarizationSegment>, RuntimeError> {
        let model_dir = self
            .model_dir
            .to_str()
            .ok_or_else(|| RuntimeError::Decode("diarize model_dir not utf-8".into()))?;
        let mut cmd = Command::new(&self.binary);
        cmd.arg("--model-dir")
            .arg(model_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let wav_bytes = tokio::fs::read(wav).await?;
        // 三并发：写 stdin（含 EOF 关闭）/ 读 stdout / 读 stderr，防管道死锁。
        let write_task = tokio::spawn(async move {
            if let Some(mut s) = stdin {
                let _ = s.write_all(&wav_bytes).await;
            }
            // stdin Option drop → child 见 EOF。
        });
        let stdout_task = tokio::spawn(async move {
            match stdout {
                Some(mut o) => {
                    let mut b = Vec::new();
                    let _ = AsyncReadExt::read_to_end(&mut o, &mut b).await;
                    b
                }
                None => Vec::new(),
            }
        });
        let stderr_task = tokio::spawn(async move {
            match stderr {
                Some(mut e) => {
                    let mut b = Vec::new();
                    let _ = AsyncReadExt::read_to_end(&mut e, &mut b).await;
                    String::from_utf8_lossy(&b).to_string()
                }
                None => String::new(),
            }
        });

        let _ = write_task.await;
        let stdout_bytes = stdout_task.await.unwrap_or_default();
        let stderr_text = stderr_task.await.unwrap_or_default();
        let status = child.wait().await?;
        if !status.success() {
            // stderr 尾 5 行入错（便于上机排查 sherpa 参数/模型问题）。
            let tail: String = stderr_text
                .lines()
                .rev()
                .take(5)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            return Err(RuntimeError::Decode(format!(
                "diarize process exited {status}: {tail}"
            )));
        }
        parse_diarize_stdout(&String::from_utf8_lossy(&stdout_bytes))
    }
}

/// env 解析 diarize driver：`SHERPA_ONNX_DIARIZE_PATH`（二进制）+
/// `SHERPA_DIARIZE_MODEL_DIR`（模型目录）。任一未设/空 → `None`（pipeline 据此
/// best-effort 跳过 speaker 标注）。**env-only（dev/上机）**：prod 应指向 app
/// bundle `Resources/`（M6 打包期落地；类比 [`crate::WhisperDriver`] 的 env 探测）。
pub fn resolve_diarize_driver() -> Option<DiarizeDriver> {
    let binary = std::env::var("SHERPA_ONNX_DIARIZE_PATH")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)?;
    let model_dir = std::env::var("SHERPA_DIARIZE_MODEL_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)?;
    Some(DiarizeDriver::new(binary, model_dir))
}

#[cfg(test)]
mod tests {
    //! ST-M3.6 单测：parse + merge 用假 stdout 行 + 假 segments 覆盖（设计「测试
    //! 策略」line 870：diarization 时间戳合并「假 segments + 假 sherpa stdout 行」，
    //! 无二进制依赖）。真二进制 + 行格式上机 → `tests/diarize_real.rs` `#[ignore]`。
    use super::*;

    /// 造一条 OpenAiSegments（speaker 均 None，模拟 whisper 输出）。
    fn segs(pairs: &[(f64, f64, &str)]) -> OpenAiSegments {
        use crate::contract::OpenAiSegment;
        OpenAiSegments {
            text: String::new(),
            segments: pairs
                .iter()
                .map(|(s, e, t)| OpenAiSegment {
                    start: *s,
                    end: *e,
                    text: (*t).into(),
                    speaker: None,
                })
                .collect(),
            words: Vec::new(),
        }
    }

    /// parse：CSV 与空白两种行格式均解析。
    #[test]
    fn parse_csv_and_space_lines() {
        let out = parse_diarize_stdout("0.0,1.0,speaker_00\n1.5 2.5 speaker_01\n").unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0],
            DiarizationSegment {
                start: 0.0,
                end: 1.0,
                speaker: "speaker_00".into()
            }
        );
        assert_eq!(
            out[1],
            DiarizationSegment {
                start: 1.5,
                end: 2.5,
                speaker: "speaker_01".into()
            }
        );
    }

    /// parse：跳过空行 / 注释 / 字段不足行，保留可解析者。
    #[test]
    fn parse_skips_blank_comment_short_lines() {
        let out = parse_diarize_stdout("# comment\n\n0.0 1.0 spk0\noops\n").unwrap();
        assert_eq!(out.len(), 1, "仅 spk0 行可解析（oops 字段不足 3 跳过）");
        assert_eq!(out[0].speaker, "spk0");
    }

    /// parse：有非空行却无一可解析 → Decode 错（信号格式不符）。
    #[test]
    fn parse_all_unparsable_errors() {
        let err = parse_diarize_stdout("garbage line here\nanother bad line").unwrap_err();
        assert!(format!("{err}").contains("none parsed"), "{err}");
    }

    /// parse：空输入 / 全空行 → Ok(空)（非错误，merge 早返）。
    #[test]
    fn parse_empty_is_ok_empty() {
        assert!(parse_diarize_stdout("").unwrap().is_empty());
        assert!(parse_diarize_stdout("\n  \n").unwrap().is_empty());
    }

    /// merge：单一说话人 → 全 A。
    #[test]
    fn merge_single_speaker_all_a() {
        let mut s = segs(&[(0.0, 1.0, "a"), (1.0, 2.0, "b")]);
        merge_speakers(&mut s, &[seg(0.0, 2.0, "spk0")]);
        assert_eq!(s.segments[0].speaker.as_deref(), Some("A"));
        assert_eq!(s.segments[1].speaker.as_deref(), Some("A"));
    }

    /// merge：两说话人按时间重叠对齐（不取首个）。
    #[test]
    fn merge_two_speakers_by_max_overlap() {
        let mut s = segs(&[(0.0, 1.0, "a"), (1.0, 2.0, "b")]);
        merge_speakers(&mut s, &[seg(0.0, 1.0, "spk0"), seg(1.0, 2.0, "spk1")]);
        assert_eq!(
            s.segments[0].speaker.as_deref(),
            Some("A"),
            "seg[0,1] ⊂ spk0"
        );
        assert_eq!(
            s.segments[1].speaker.as_deref(),
            Some("B"),
            "seg[1,2] ⊂ spk1"
        );
    }

    /// merge：跨两说话人的 segment 取重叠更大者（非先见）。
    #[test]
    fn merge_spanning_two_picks_max_overlap() {
        let mut s = segs(&[(0.0, 2.0, "a")]);
        merge_speakers(&mut s, &[seg(0.0, 1.5, "spk0"), seg(1.5, 3.0, "spk1")]);
        // seg[0,2] 与 spk0 重叠 1.5 > 与 spk1 重叠 0.5 → A。
        assert_eq!(s.segments[0].speaker.as_deref(), Some("A"));
    }

    /// merge：归一按 start 升序首次出现，非按原始标签字典序（先开口者 A）。
    #[test]
    fn merge_normalizes_by_first_seen_order() {
        // spk1 先开口（start=0）→ A；spk0 后（start=1）→ B（尽管 "spk0" 字典序在前）。
        let mut s = segs(&[(0.0, 1.0, "a"), (1.0, 2.0, "b")]);
        merge_speakers(&mut s, &[seg(0.0, 1.0, "spk1"), seg(1.0, 2.0, "spk0")]);
        assert_eq!(s.segments[0].speaker.as_deref(), Some("A"), "spk1 先开口→A");
        assert_eq!(s.segments[1].speaker.as_deref(), Some("B"), "spk0→B");
    }

    /// merge：diar 空 → speaker 保持 None。
    #[test]
    fn merge_empty_diar_keeps_none() {
        let mut s = segs(&[(0.0, 1.0, "a")]);
        merge_speakers(&mut s, &[]);
        assert!(s.segments[0].speaker.is_none(), "空 diar 不动 speaker");
    }

    /// merge：segment 落在 diar 间隙 → 兜底取中点最近者。
    #[test]
    fn merge_segment_in_gap_uses_nearest() {
        // seg[5,6] 在 (0,4) 与 (7,10) 间隙；中点 5.5 距 (7,10) 中点 8.5=3.0 <
        // 距 (0,4) 中点 2=3.5 → 取 spk1。归一：spk0(start=0)→A，spk1(start=7)→B。
        let mut s = segs(&[(5.0, 6.0, "a")]);
        merge_speakers(&mut s, &[seg(0.0, 4.0, "spk0"), seg(7.0, 10.0, "spk1")]);
        assert_eq!(
            s.segments[0].speaker.as_deref(),
            Some("B"),
            "间隙兜底取最近 spk1→B"
        );
    }

    /// parse→merge 端到端（设计 line 870 形态：假 stdout 行 + 假 segments）。
    #[test]
    fn parse_then_merge_end_to_end() {
        let stdout = "0.0,1.0,speaker_00\n1.0,2.0,speaker_01";
        let diar = parse_diarize_stdout(stdout).unwrap();
        let mut s = segs(&[(0.0, 1.0, "hello"), (1.0, 2.0, "world")]);
        merge_speakers(&mut s, &diar);
        assert_eq!(s.segments[0].speaker.as_deref(), Some("A"));
        assert_eq!(s.segments[1].speaker.as_deref(), Some("B"));
    }

    fn seg(start: f64, end: f64, speaker: &str) -> DiarizationSegment {
        DiarizationSegment {
            start,
            end,
            speaker: speaker.into(),
        }
    }
}
