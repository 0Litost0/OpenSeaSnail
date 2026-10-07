//! whisper driver（ST-M3.2 上机接线 + ST-M3.1 trait impl）。
//!
//! 生命周期用 [`SidecarProcess`]；`transcribe` POST /inference。实测（server.cpp
//! 源码 + 本 ST 上机）whisper-server /inference 响应已是 OpenAI 形状（{text,
//! segments:[{id,text,start,end}]}，start/end 秒），driver 透传解析即可——设计
//! 原假设"原生 {transcription,segments:[{t0,t1,text}]} 需翻译归一"不成立，normalize 已删。

use crate::contract::{
    has_visible_text, Capabilities, Diarization, OpenAiSegments, RuntimeKind, TranscribeReq,
};
use crate::error::RuntimeError;
use crate::sidecar::SidecarProcess;
use crate::ModelRuntime;
use async_trait::async_trait;
use std::io;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::Mutex;

pub struct WhisperDriver {
    id: String,
    binary: PathBuf,
    model_path: PathBuf,
    proc: Mutex<Option<WhisperHandle>>,
}

struct WhisperHandle {
    proc: SidecarProcess,
    port: u16,
}

impl WhisperDriver {
    pub fn new(id: &str, binary: PathBuf, model_path: PathBuf) -> Self {
        Self {
            id: id.into(),
            binary,
            model_path,
            proc: Mutex::new(None),
        }
    }
}

#[async_trait]
impl ModelRuntime for WhisperDriver {
    fn id(&self) -> &str {
        &self.id
    }
    fn runtime_kind(&self) -> RuntimeKind {
        RuntimeKind::Whisper
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            diarization: Diarization::External,
            streaming: false,
            languages: vec!["en".into(), "zh".into()],
            max_audio_seconds: 3600,
            word_timestamps: false,
        }
    }

    async fn start(&self, port: u16) -> io::Result<()> {
        // 双调守卫：已有活跃实例则拒绝（调用方应先 stop），防止旧 SidecarProcess
        // 被覆写丢弃而泄漏（kill_on_drop=true 兜底，但仍应显式拒绝暴露调用方 bug）。
        // 注：持锁跨 spawn+wait_health 会串行化并发 health()——mock 立即 ready 无此
        // 问题；真二进制 30s wait_health 期间并发 health 会阻塞，ST-M3.2 上机若影响
        // GUI 健康检查再拆为 spawn 持锁/wait 放锁/设值再检查。
        let mut guard = self.proc.lock().await;
        if guard.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "whisper runtime already started; stop() first",
            ));
        }
        let model_str = self
            .model_path
            .to_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "model path not utf-8"))?;
        // port=0 → driver 自选空闲端口：bind 0 → 取 actual_port → drop listener →
        // 交 whisper-server 绑同端口。修 ST-M3.10 review B#1：原 start(0) 把 0 透传
        // 给 --port 与 wait_health(0)，而 probe_health(0)=connect port 0 永远
        // EADDRNOTAVAIL → start(0) 必 30s 超时失败（activate 切换 + retry 重启均受影响）。
        // 单活跃 runtime 下 drop→bind 间窗极小且无同端口竞争；镜像 MockRuntime::start
        // 的 port-0 解析。start(非 0) 行为不变（ST-M3.2 上机 start(8098) 仍直通）。
        let port = if port == 0 {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            l.local_addr()?.port()
        } else {
            port
        };
        let mut cmd = tokio::process::Command::new(&self.binary);
        cmd.args([
            "--model",
            model_str,
            "--port",
            &port.to_string(),
            // --inference-path 默认 /inference（ST-M3.2 实测确认），无需显式重映射。
        ]);
        let mut sidecar = SidecarProcess::spawn(cmd, "whisper").await?;
        if !SidecarProcess::wait_health(port, Duration::from_secs(30)).await {
            let _ = sidecar.stop().await;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "whisper health timeout",
            ));
        }
        *guard = Some(WhisperHandle {
            proc: sidecar,
            port,
        });
        Ok(())
    }

    async fn stop(&self) -> io::Result<()> {
        if let Some(mut h) = self.proc.lock().await.take() {
            h.proc.stop().await?;
        }
        Ok(())
    }

    async fn health(&self) -> bool {
        let port = match self.proc.lock().await.as_ref() {
            Some(h) => h.port,
            None => return false,
        };
        SidecarProcess::wait_health(port, Duration::from_millis(500)).await
    }

    async fn transcribe(&self, req: TranscribeReq) -> Result<OpenAiSegments, RuntimeError> {
        let port = self
            .proc
            .lock()
            .await
            .as_ref()
            .map(|h| h.port)
            .ok_or(RuntimeError::NotStarted)?;
        // ST-M3.2 实测（server.cpp 源码 + 上机）：whisper-server /inference 响应已是
        // OpenAI 形状（{text, segments:[{id,text,start,end}]}，start/end 秒）——设计原假设
        // 的"原生 {transcription,segments:[{t0,t1,text}]} 需 driver 翻译归一"不成立，
        // 透传解析即可，normalize_whisper_inference 已删。上机实测（whisper-server +
        // ggml-tiny）：response_format=json 只返 {text}（无 segments）；verbose_json 才返
        // {text, segments:[{id,text,start,end,tokens,words,...}]}——故用 verbose_json，
        // OpenAiSegment 取 start/end/text（speaker=None），其余字段 serde 忽略。注：
        // whisper-server form field 不含 language（语言经 start -l 或自动检测），
        // TranscribeReq.language 在 whisper 路径暂忽略，per-request language 后置。
        let url = format!("http://127.0.0.1:{port}/inference");
        let bytes = tokio::fs::read(&req.wav).await?;
        let part = reqwest::multipart::Part::bytes(bytes).file_name("audio.wav");
        let mut form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("response_format", "verbose_json");
        if let Some(p) = &req.prompt {
            form = form.text("prompt", p.clone());
        }
        let resp = reqwest::Client::new()
            .post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(RuntimeError::Http)?;
        if !resp.status().is_success() {
            return Err(RuntimeError::BadStatus(resp.status()));
        }
        let mut response = resp
            .json::<OpenAiSegments>()
            .await
            .map_err(RuntimeError::Http)?;
        validate_response(&mut response)?;
        response.validate_words();
        Ok(response)
    }
}

fn validate_response(response: &mut OpenAiSegments) -> Result<(), RuntimeError> {
    let text_visible = has_visible_text(&response.text);
    let segment_visible = response
        .segments
        .iter()
        .any(|segment| has_visible_text(&segment.text));
    let word_visible = response
        .words
        .iter()
        .any(|word| has_visible_text(&word.text));

    if !text_visible && (segment_visible || word_visible) {
        return Err(RuntimeError::Protocol(
            "Whisper child text contradicts empty top-level text".into(),
        ));
    }
    if response.segments.iter().any(|segment| {
        !segment.start.is_finite()
            || !segment.end.is_finite()
            || segment.start < 0.0
            || segment.end < segment.start
    }) {
        return Err(RuntimeError::Protocol(
            "Whisper response contains an invalid segment".into(),
        ));
    }
    if text_visible && !response.segments.is_empty() && !segment_visible {
        return Err(RuntimeError::Protocol(
            "Whisper visible text contradicts blank segments".into(),
        ));
    }
    response
        .segments
        .retain(|segment| has_visible_text(&segment.text));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{OpenAiSegment, OpenAiWord};

    #[test]
    fn response_contract_accepts_empty_text_only_and_segmented_results() {
        let cases = [
            OpenAiSegments {
                text: String::new(),
                segments: vec![],
                words: vec![],
            },
            OpenAiSegments {
                text: "text only".into(),
                segments: vec![],
                words: vec![],
            },
            OpenAiSegments {
                text: "segmented".into(),
                segments: vec![OpenAiSegment {
                    start: 0.0,
                    end: 0.5,
                    text: "segmented".into(),
                    speaker: None,
                }],
                words: vec![],
            },
        ];
        for mut response in cases {
            assert!(validate_response(&mut response).is_ok());
        }
    }

    #[test]
    fn response_contract_rejects_contradictions_and_invalid_boundaries() {
        let cases = [
            OpenAiSegments {
                text: String::new(),
                segments: vec![OpenAiSegment {
                    start: 0.0,
                    end: 0.5,
                    text: "child".into(),
                    speaker: None,
                }],
                words: vec![],
            },
            OpenAiSegments {
                text: String::new(),
                segments: vec![],
                words: vec![OpenAiWord {
                    start: 0.0,
                    end: 0.1,
                    text: "word".into(),
                }],
            },
            OpenAiSegments {
                text: "text".into(),
                segments: vec![OpenAiSegment {
                    start: f64::NAN,
                    end: 0.5,
                    text: "text".into(),
                    speaker: None,
                }],
                words: vec![],
            },
            OpenAiSegments {
                text: "text".into(),
                segments: vec![OpenAiSegment {
                    start: 0.0,
                    end: 0.5,
                    text: " \t".into(),
                    speaker: None,
                }],
                words: vec![],
            },
        ];
        for mut response in cases {
            assert!(matches!(
                validate_response(&mut response),
                Err(RuntimeError::Protocol(_))
            ));
        }
    }

    #[test]
    fn invalid_word_alignment_remains_a_nonfatal_downgrade() {
        let mut response = OpenAiSegments {
            text: "hello".into(),
            segments: vec![],
            words: vec![OpenAiWord {
                start: 1.0,
                end: 0.5,
                text: "hello".into(),
            }],
        };
        validate_response(&mut response).unwrap();
        response.validate_words();
        assert!(response.words.is_empty());
    }

    #[test]
    fn response_contract_filters_blank_segments_when_visible_evidence_remains() {
        let mut response = OpenAiSegments {
            text: "hello".into(),
            segments: vec![
                OpenAiSegment {
                    start: 0.0,
                    end: 0.1,
                    text: "\u{2003}".into(),
                    speaker: None,
                },
                OpenAiSegment {
                    start: 0.1,
                    end: 0.5,
                    text: "hello".into(),
                    speaker: None,
                },
            ],
            words: vec![],
        };
        validate_response(&mut response).unwrap();
        assert_eq!(response.segments.len(), 1);
        assert_eq!(response.segments[0].text, "hello");
    }
}
