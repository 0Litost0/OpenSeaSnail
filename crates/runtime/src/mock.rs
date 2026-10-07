//! mock sidecar（ST-M3.8）：进程内 axum 返回 canned OpenAI 形状。
//!
//! `start` 起 axum::serve task，`health` GET /，`transcribe` POST 自己的
//! /v1/audio/transcriptions（reqwest multipart）——验证 driver→sidecar HTTP 链路。
//! 不碰真模型。canned 响应本轮用常量；golden 文件后置 ST-M3.2 上机后。

use crate::contract::{
    Capabilities, Diarization, OpenAiSegment, OpenAiSegments, OpenAiWord, RuntimeKind,
    TranscribeReq,
};
use crate::error::RuntimeError;
use crate::sidecar::SidecarProcess;
use crate::ModelRuntime;
use async_trait::async_trait;
use std::io;
use tokio::sync::Mutex;
use tokio::task::AbortHandle;

/// mock 的 canned 响应。
#[derive(Debug, Clone)]
pub struct CannedResponse {
    pub text: String,
    pub segments: Vec<OpenAiSegment>,
    /// 词/字级时间戳（M4.1，可选）。置入后 mock 经 `validate_words` 校验，与真 driver 一致。
    pub words: Vec<OpenAiWord>,
}

impl Default for CannedResponse {
    fn default() -> Self {
        Self {
            text: "你好世界".into(),
            segments: vec![OpenAiSegment {
                start: 0.0,
                end: 1.0,
                text: "你好世界".into(),
                speaker: None,
            }],
            words: Vec::new(),
        }
    }
}

pub struct MockRuntime {
    id: String,
    kind: RuntimeKind,
    canned: CannedResponse,
    server: Mutex<Option<MockServerHandle>>,
}

struct MockServerHandle {
    port: u16,
    abort: AbortHandle,
}

impl MockRuntime {
    pub fn new(id: &str, kind: RuntimeKind, canned: CannedResponse) -> Self {
        Self {
            id: id.into(),
            kind,
            canned,
            server: Mutex::new(None),
        }
    }

    /// 便捷构造：默认 canned、Whisper kind。
    pub fn openai_default() -> Self {
        Self::new(
            "mock-openai",
            RuntimeKind::Whisper,
            CannedResponse::default(),
        )
    }

    async fn port(&self) -> Result<u16, RuntimeError> {
        self.server
            .lock()
            .await
            .as_ref()
            .map(|s| s.port)
            .ok_or(RuntimeError::NotStarted)
    }
}

#[async_trait]
impl ModelRuntime for MockRuntime {
    fn id(&self) -> &str {
        &self.id
    }
    fn runtime_kind(&self) -> RuntimeKind {
        self.kind
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            diarization: Diarization::None,
            streaming: false,
            languages: vec!["zh".into(), "en".into()],
            max_audio_seconds: 600,
            word_timestamps: !self.canned.words.is_empty(),
        }
    }

    async fn start(&self, port: u16) -> io::Result<()> {
        // 双调守卫：已有活跃实例则拒绝（调用方应先 stop），防止旧 axum task
        // 句柄被覆写丢弃而泄漏——AbortHandle drop 不会 abort，必须显式 stop。
        let mut guard = self.server.lock().await;
        if guard.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "mock runtime already started; stop() first",
            ));
        }
        let app = build_mock_app(self.canned.clone());
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
        let actual_port = listener.local_addr()?.port();
        let h = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        *guard = Some(MockServerHandle {
            port: actual_port,
            abort: h.abort_handle(),
        });
        Ok(())
    }

    async fn stop(&self) -> io::Result<()> {
        if let Some(s) = self.server.lock().await.take() {
            s.abort.abort();
        }
        Ok(())
    }

    async fn health(&self) -> bool {
        match self.port().await {
            Ok(port) => {
                SidecarProcess::wait_health(port, std::time::Duration::from_millis(500)).await
            }
            Err(_) => false,
        }
    }

    async fn transcribe(&self, req: TranscribeReq) -> Result<OpenAiSegments, RuntimeError> {
        let port = self.port().await?;
        let url = format!("http://127.0.0.1:{port}/v1/audio/transcriptions");
        let bytes = tokio::fs::read(&req.wav).await?;
        let part = reqwest::multipart::Part::bytes(bytes).file_name("audio.wav");
        let mut form = reqwest::multipart::Form::new().part("file", part);
        if let Some(l) = &req.language {
            form = form.text("language", l.clone());
        }
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
        let mut segs: OpenAiSegments = resp.json().await.map_err(RuntimeError::Http)?;
        segs.validate_words();
        Ok(segs)
    }
}

/// axum mock app：GET / → health；POST /v1/audio/transcriptions → canned OpenAI 形状。
fn build_mock_app(canned: CannedResponse) -> axum::Router {
    use axum::extract::Multipart;
    use axum::routing::{get, post};
    use axum::Json;
    axum::Router::new()
        .route(
            "/",
            get(|| async { Json(serde_json::json!({"status":"ok"})) }),
        )
        .route(
            "/v1/audio/transcriptions",
            post(move |_mp: Multipart| async move {
                let c = canned.clone();
                Json(serde_json::json!({
                    "text": c.text,
                    "object": "transcription",
                    "segments": c.segments,
                    "words": c.words,
                }))
            }),
        )
}
