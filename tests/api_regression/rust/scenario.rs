use async_trait::async_trait;
use seasnail_runtime::{
    contract::{Capabilities, OpenAiSegment, OpenAiSegments, RuntimeKind, TranscribeReq},
    error::RuntimeError,
    ModelRuntime,
};
use std::{
    io,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};

pub struct ScenarioRuntime {
    id: String,
    responses: Vec<serde_json::Value>,
    index: AtomicUsize,
    started: AtomicBool,
}
impl ScenarioRuntime {
    pub fn load(id: &str, path: &std::path::Path, scenario: &str) -> anyhow::Result<Self> {
        let data: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
        anyhow::ensure!(data["version"] == "v1", "unsupported ASR scenario version");
        let value = &data["scenarios"][scenario];
        anyhow::ensure!(
            value["after_sequence"] == "repeat-last",
            "unsupported sequence behavior"
        );
        let responses = value["responses"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("unknown ASR scenario"))?
            .clone();
        anyhow::ensure!(!responses.is_empty(), "empty response sequence");
        for response in &responses {
            anyhow::ensure!(
                ["transcript", "error", "no-speech"]
                    .contains(&response["kind"].as_str().unwrap_or("")),
                "unsupported ASR response"
            );
            anyhow::ensure!(response["delay_ms"].as_u64().is_some(), "invalid ASR delay");
        }
        Ok(Self {
            id: id.into(),
            responses,
            index: AtomicUsize::new(0),
            started: AtomicBool::new(false),
        })
    }
}
#[async_trait]
impl ModelRuntime for ScenarioRuntime {
    fn id(&self) -> &str {
        &self.id
    }
    fn runtime_kind(&self) -> RuntimeKind {
        RuntimeKind::SherpaOnnx
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::sensevoice_gguf()
    }
    async fn start(&self, _port: u16) -> io::Result<()> {
        self.started.store(true, Ordering::Release);
        Ok(())
    }
    async fn stop(&self) -> io::Result<()> {
        self.started.store(false, Ordering::Release);
        Ok(())
    }
    async fn health(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }
    async fn transcribe(&self, req: TranscribeReq) -> Result<OpenAiSegments, RuntimeError> {
        if !self.health().await {
            return Err(RuntimeError::NotStarted);
        }
        // Read the normalized file, so the production normalizer cannot be bypassed.
        let wav = tokio::fs::read(req.wav).await?;
        if !wav.starts_with(b"RIFF") {
            return Err(RuntimeError::Protocol("expected normalized WAV".into()));
        }
        let i = self
            .index
            .fetch_add(1, Ordering::AcqRel)
            .min(self.responses.len() - 1);
        let response = &self.responses[i];
        tokio::time::sleep(Duration::from_millis(
            response["delay_ms"].as_u64().unwrap(),
        ))
        .await;
        if response["kind"] == "error" {
            return Err(RuntimeError::Protocol("controlled_asr_failure".into()));
        }
        let text = response["text"].as_str().unwrap_or("").to_owned();
        let segments = if text.is_empty() {
            vec![]
        } else {
            vec![OpenAiSegment {
                start: 0.0,
                end: 1.0,
                text: text.clone(),
                speaker: None,
            }]
        };
        Ok(OpenAiSegments {
            text,
            segments,
            words: vec![],
        })
    }
}
