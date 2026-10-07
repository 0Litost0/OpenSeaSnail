//! ST-M3.6 上机回归：真实 `sherpa-onnx-diarize` 一次性进程 + stdout 行格式确认。
//!
//! 需 `SHERPA_ONNX_DIARIZE_PATH`（二进制）、`SHERPA_DIARIZE_MODEL_DIR`（模型目录）、
//! `SHERPA_DIARIZE_TEST_WAV`（含多人对话的 WAV）三项 env；`cargo test --ignored` 手动跑。
//! **stdout 行格式上机确认**（roadmap line 192）：若 [`DiarizeDriver::run`] 产出空
//! segments 或报 Decode 错，据 sherpa 实测 stdout 调整 `parse_diarize_stdout` 切分
//! /字段顺序（类比 M3.2 据 `/inference` 实测调整 whisper driver）。

use seasnail_runtime::contract::{OpenAiSegment, OpenAiSegments};
use seasnail_runtime::{merge_speakers, DiarizeDriver};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

#[ignore]
#[tokio::test]
async fn diarize_driver_real_runs() {
    let (binary, model_dir, wav) = match (
        env("SHERPA_ONNX_DIARIZE_PATH"),
        env("SHERPA_DIARIZE_MODEL_DIR"),
        env("SHERPA_DIARIZE_TEST_WAV"),
    ) {
        (Some(b), Some(m), Some(w)) => (b, m, w),
        _ => {
            eprintln!(
                "跳过：未设 SHERPA_ONNX_DIARIZE_PATH / SHERPA_DIARIZE_MODEL_DIR / \
                 SHERPA_DIARIZE_TEST_WAV"
            );
            return;
        }
    };
    let drv = DiarizeDriver::new(binary.into(), model_dir.into());
    let diar = drv
        .run(std::path::Path::new(&wav))
        .await
        .expect("diarize run 应成功（上机：若失败据 sherpa 实测调 CLI/stdin/行格式）");
    assert!(
        !diar.is_empty(),
        "应产非空 diarization segments（若空，parse 行格式不符 → 调 parse_diarize_stdout）"
    );
    for d in &diar {
        eprintln!("diar: {:.2}..{:.2} {}", d.start, d.end, d.speaker);
    }

    // 端到端 merge（真数据）：以 diar 自身为 segments 占位，验归一 A/B/C。
    let mut segs = OpenAiSegments {
        text: String::new(),
        segments: diar
            .iter()
            .map(|d| OpenAiSegment {
                start: d.start,
                end: d.end,
                text: String::new(),
                speaker: None,
            })
            .collect(),
        words: Vec::new(),
    };
    merge_speakers(&mut segs, &diar);
    let speakers: Vec<_> = segs
        .segments
        .iter()
        .filter_map(|s| s.speaker.clone())
        .collect();
    assert!(!speakers.is_empty(), "merge 后应有 speaker 标签");
    eprintln!("归一 speakers: {speakers:?}");
}
