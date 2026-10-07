//! FunASR 本地 sidecar driver（M4）。
//!
//! SeaSnail 不直接使用 FunASR 附带的 `funasr-server`：它的 CLI 无法为 ASR、VAD、
//! 标点和 CAM++ 同时指定本地模型目录，缺模型时会访问模型仓库。这里启动我们随
//! bundle 分发的 `sidecar.py`，该进程只接受四个已校验的本地目录，并返回 OpenAI
//! verbose JSON 形状，故 daemon 不需要特殊翻译层。

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

pub struct FunAsrDriver {
    id: String,
    python: PathBuf,
    sidecar: PathBuf,
    models_root: PathBuf,
    device: String,
    /// 用户目录（下载的 punc）作 extra-root 传给 sidecar；None=不传（仅内置根）。
    extra_root: Option<PathBuf>,
    proc: Mutex<Option<FunAsrHandle>>,
}

struct FunAsrHandle {
    proc: SidecarProcess,
    port: u16,
}

impl FunAsrDriver {
    pub fn new(
        id: &str,
        python: PathBuf,
        sidecar: PathBuf,
        models_root: PathBuf,
        device: impl Into<String>,
        extra_root: Option<PathBuf>,
    ) -> Self {
        Self {
            id: id.into(),
            python,
            sidecar,
            models_root,
            device: device.into(),
            extra_root,
            proc: Mutex::new(None),
        }
    }
}

#[async_trait]
impl ModelRuntime for FunAsrDriver {
    fn id(&self) -> &str {
        &self.id
    }

    fn runtime_kind(&self) -> RuntimeKind {
        RuntimeKind::FunAsr
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            diarization: Diarization::Builtin,
            streaming: false,
            languages: vec!["zh".into(), "en".into(), "mixed".into()],
            max_audio_seconds: 3600,
            word_timestamps: true,
        }
    }

    async fn start(&self, port: u16) -> io::Result<()> {
        let mut guard = self.proc.lock().await;
        if guard.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "funasr runtime already started; stop() first",
            ));
        }
        // 仅预检 asr+vad（最小常驻集）；punc/spk 可选，缺省不阻断启动（M2.3 放宽）。
        for (name, path) in [
            ("Python", &self.python),
            ("FunASR sidecar", &self.sidecar),
            ("ASR model", &self.models_root.join("asr")),
            ("VAD model", &self.models_root.join("vad")),
        ] {
            if !path.exists() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{name} missing: {}", path.display()),
                ));
            }
        }
        let port = if port == 0 {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            listener.local_addr()?.port()
        } else {
            port
        };
        let models_root = self
            .models_root
            .to_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "models root not utf-8"))?;
        let mut cmd = tokio::process::Command::new(&self.python);
        cmd.args([
            &self.sidecar.to_string_lossy(),
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--device",
            &self.device,
            "--models-root",
            models_root,
        ]);
        // 用户目录（下载的 punc）作 extra-root 传 sidecar；None 不传。
        if let Some(extra) = &self.extra_root {
            cmd.args(["--models-extra-root", &extra.to_string_lossy()]);
        }
        let mut sidecar = SidecarProcess::spawn(cmd, "funasr").await?;
        if !SidecarProcess::wait_health_at(port, "/health", Duration::from_secs(90)).await {
            let _ = sidecar.stop().await;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "funasr health timeout",
            ));
        }
        *guard = Some(FunAsrHandle {
            proc: sidecar,
            port,
        });
        Ok(())
    }

    async fn stop(&self) -> io::Result<()> {
        if let Some(mut handle) = self.proc.lock().await.take() {
            handle.proc.stop().await?;
        }
        Ok(())
    }

    async fn health(&self) -> bool {
        match self.proc.lock().await.as_ref() {
            Some(handle) => {
                SidecarProcess::wait_health_at(handle.port, "/health", Duration::from_millis(500))
                    .await
            }
            None => false,
        }
    }

    async fn transcribe(&self, req: TranscribeReq) -> Result<OpenAiSegments, RuntimeError> {
        let port = self
            .proc
            .lock()
            .await
            .as_ref()
            .map(|handle| handle.port)
            .ok_or(RuntimeError::NotStarted)?;
        let url = format!("http://127.0.0.1:{port}/v1/audio/transcriptions");
        post_transcription(&url, &req, &self.id).await
    }
}

/// POST 音频到 sidecar 的 OpenAI 形状端点，返回归一后的 segments。抽出独立函数便于
/// 透传单测（见 `tests`）：`punc`/`spk` 特性标志 → multipart 字段一一对应，None→false。
async fn post_transcription(
    url: &str,
    req: &TranscribeReq,
    model: &str,
) -> Result<OpenAiSegments, RuntimeError> {
    let bytes = tokio::fs::read(&req.wav).await?;
    let part = reqwest::multipart::Part::bytes(bytes).file_name("audio.wav");
    let mut form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", model.to_string())
        .text("response_format", "verbose_json")
        .text("punc", flag_str(req.punc))
        .text("spk", flag_str(req.spk));
    if let Some(language) = &req.language {
        form = form.text("language", language.clone());
    }
    let response = reqwest::Client::new()
        .post(url)
        .multipart(form)
        .send()
        .await
        .map_err(RuntimeError::Http)?;
    if !response.status().is_success() {
        return Err(RuntimeError::BadStatus(response.status()));
    }
    let mut segs = response
        .json::<OpenAiSegments>()
        .await
        .map_err(RuntimeError::Http)?;
    validate_response(&mut segs)?;
    // sidecar 导出的 words 经校验后才可信；不满足（非空/单调/对齐）则置空，composer 走句段降级。
    segs.validate_words();
    Ok(segs)
}

fn validate_response(response: &mut OpenAiSegments) -> Result<(), RuntimeError> {
    if !has_visible_text(&response.text) {
        if response.segments.is_empty() && response.words.is_empty() {
            return Ok(());
        }
        return Err(RuntimeError::Protocol(
            "FunASR empty text must not carry segments or words".into(),
        ));
    }
    let has_visible_segment = response
        .segments
        .iter()
        .any(|segment| has_visible_text(&segment.text));
    let has_invalid_boundary = response.segments.iter().any(|segment| {
        !segment.start.is_finite()
            || !segment.end.is_finite()
            || segment.start < 0.0
            || segment.end < segment.start
    });
    if !has_visible_segment || has_invalid_boundary {
        return Err(RuntimeError::Protocol(
            "FunASR visible text requires valid visible segments".into(),
        ));
    }
    response
        .segments
        .retain(|segment| has_visible_text(&segment.text));
    Ok(())
}

/// 特性标志 → multipart 字段串。None 视为 false（不启用），与设计默认（punc/spk 不加载）一致。
fn flag_str(flag: Option<bool>) -> &'static str {
    match flag {
        Some(true) => "true",
        _ => "false",
    }
}

#[cfg(test)]
mod tests {
    //! ST-M1.1 透传单测：进程内 axum 捕获服务器验证 req.punc/spk → multipart 字段一一对应。
    use super::post_transcription;
    use crate::contract::{OpenAiSegment, OpenAiSegments, OpenAiWord, TranscribeReq};
    use crate::RuntimeError;
    use std::sync::{Arc, Mutex};

    /// 捕获 app：读全部 multipart 字段进共享 vec，回 canned OpenAI 形状。
    fn build_capture_app(captured: Arc<Mutex<Vec<(String, String)>>>) -> axum::Router {
        use axum::extract::Multipart;
        use axum::routing::post;
        use axum::Json;
        axum::Router::new().route(
            "/v1/audio/transcriptions",
            post(move |mut mp: Multipart| {
                let captured = captured.clone();
                async move {
                    let mut fields = Vec::new();
                    while let Ok(Some(field)) = mp.next_field().await {
                        let name = field.name().unwrap_or("").to_string();
                        let val = field.text().await.unwrap_or_default();
                        fields.push((name, val));
                    }
                    captured.lock().unwrap().extend(fields);
                    Json(serde_json::json!({"text":"ok","segments":[{"start":0.0,"end":0.1,"text":"ok","speaker":null}],"words":[]}))
                }
            }),
        )
    }

    fn last_value<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
        fields
            .iter()
            .rev()
            .find(|(n, _)| n == key)
            .map(|(_, v)| v.as_str())
    }

    #[tokio::test]
    async fn post_transcription_passes_punc_spk_flags() {
        let captured: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(vec![]));
        let app = build_capture_app(captured.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let h = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("a.wav");
        std::fs::write(&wav, b"y").unwrap();
        let url = format!("http://127.0.0.1:{port}/v1/audio/transcriptions");

        // 显式标志：punc on / spk off。
        let req = TranscribeReq {
            wav: wav.clone(),
            language: None,
            prompt: None,
            punc: Some(true),
            spk: Some(false),
        };
        post_transcription(&url, &req, "funasr-default")
            .await
            .unwrap();
        let f1 = captured.lock().unwrap().clone();
        assert_eq!(last_value(&f1, "punc"), Some("true"));
        assert_eq!(last_value(&f1, "spk"), Some("false"));
        assert_eq!(last_value(&f1, "model"), Some("funasr-default"));
        assert_eq!(last_value(&f1, "response_format"), Some("verbose_json"));

        // 默认（None）→ false，符合设计"punc/spk 不加载"默认。
        let req2 = TranscribeReq {
            wav,
            language: None,
            prompt: None,
            punc: None,
            spk: None,
        };
        post_transcription(&url, &req2, "funasr-default")
            .await
            .unwrap();
        let f2 = captured.lock().unwrap().clone();
        assert_eq!(last_value(&f2, "punc"), Some("false"));
        assert_eq!(last_value(&f2, "spk"), Some("false"));

        h.abort();
    }

    #[test]
    fn response_contract_accepts_empty_and_visible_results() {
        let mut empty = OpenAiSegments {
            text: String::new(),
            segments: vec![],
            words: vec![],
        };
        assert!(super::validate_response(&mut empty).is_ok());

        let mut visible = OpenAiSegments {
            text: "hello".into(),
            segments: vec![OpenAiSegment {
                start: 0.0,
                end: 0.5,
                text: "hello".into(),
                speaker: None,
            }],
            words: vec![],
        };
        assert!(super::validate_response(&mut visible).is_ok());
    }

    #[test]
    fn response_contract_rejects_visibility_contradictions_and_bad_segments() {
        let mut child_without_text = OpenAiSegments {
            text: String::new(),
            segments: vec![OpenAiSegment {
                start: 0.0,
                end: 0.5,
                text: "child".into(),
                speaker: None,
            }],
            words: vec![],
        };
        assert!(matches!(
            super::validate_response(&mut child_without_text),
            Err(RuntimeError::Protocol(_))
        ));

        let mut word_without_text = OpenAiSegments {
            text: String::new(),
            segments: vec![],
            words: vec![OpenAiWord {
                start: 0.0,
                end: 0.1,
                text: "word".into(),
            }],
        };
        assert!(matches!(
            super::validate_response(&mut word_without_text),
            Err(RuntimeError::Protocol(_))
        ));

        let mut invalid_segment = OpenAiSegments {
            text: "hello".into(),
            segments: vec![OpenAiSegment {
                start: 1.0,
                end: 0.5,
                text: "hello".into(),
                speaker: None,
            }],
            words: vec![],
        };
        assert!(matches!(
            super::validate_response(&mut invalid_segment),
            Err(RuntimeError::Protocol(_))
        ));
    }

    #[test]
    fn response_contract_filters_blank_segments_when_visible_evidence_remains() {
        let mut response = OpenAiSegments {
            text: "hello".into(),
            segments: vec![
                OpenAiSegment {
                    start: 0.0,
                    end: 0.1,
                    text: " \t".into(),
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
        super::validate_response(&mut response).unwrap();
        assert_eq!(response.segments.len(), 1);
        assert_eq!(response.segments[0].text, "hello");
    }
}
