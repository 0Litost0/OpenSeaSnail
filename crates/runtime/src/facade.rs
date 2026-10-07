//! Stable runtime ports (ST-M4.5).
//!
//! These ports are intentionally independent of backend response DTOs.  The
//! compatibility drivers may continue to speak OpenAI/Sherpa protocols
//! internally, but callers of this module receive only canonical results and
//! typed lifecycle requests.

use crate::contract::TranscribeReq;
use crate::{
    merge_speakers, reserve_retry_transcription, reserve_transcription, resolve_diarize_driver,
    CanonicalTranscript, Diarization, DiarizationSegment, ReservedTranscription, RuntimeError,
    RuntimeKind, RuntimeOperationGate, RuntimeReservationError, SidecarRegistry,
};
use async_trait::async_trait;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeFailure {
    #[error("runtime operation is busy: {0}")]
    Busy(#[from] crate::RuntimeOperationOccupied),
    #[error("no active runtime")]
    Unavailable,
    #[error("runtime failed: {0}")]
    Runtime(#[from] RuntimeError),
    #[error("runtime preparation failed: {0}")]
    Preparation(String),
    #[error("runtime administration failed: {0}")]
    Administration(String),
}

impl From<RuntimeReservationError> for RuntimeFailure {
    fn from(error: RuntimeReservationError) -> Self {
        match error {
            RuntimeReservationError::Busy(error) => Self::Busy(error),
            RuntimeReservationError::Unavailable => Self::Unavailable,
            RuntimeReservationError::Restart(error) => {
                Self::Administration(format!("sidecar restart failed: {error}"))
            }
        }
    }
}

/// Backend-neutral request handed to the runtime facade.
#[derive(Debug, Clone)]
pub struct TranscriptionRequest {
    pub wav: PathBuf,
    pub language: Option<String>,
    pub prompt: Option<String>,
    /// Application 在本次后台执行开始时读取一次设置并显式写入请求。
    pub punc: Option<bool>,
    pub spk: Option<bool>,
    pub duration_ms: i64,
}

impl TranscriptionRequest {
    fn backend_request(&self) -> TranscribeReq {
        TranscribeReq {
            wav: self.wav.clone(),
            language: self.language.clone(),
            prompt: self.prompt.clone(),
            punc: self.punc,
            spk: self.spk,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeStatus {
    pub active_model_id: Option<String>,
    pub active_kind: Option<RuntimeKind>,
}

#[derive(Debug, Clone)]
pub struct ActivateRuntime {
    pub model_id: String,
}

/// Backend-neutral result of a successful transcription request.
///
/// Drivers remain responsible for rejecting malformed or contradictory
/// responses. `NoSpeech` is reserved for a valid response without any visible
/// text evidence.
#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptionOutcome {
    Transcript(CanonicalTranscript),
    NoSpeech,
}

#[async_trait]
pub trait TranscriptionEngine: Send + Sync {
    async fn reserve(&self, session_id: &str) -> Result<ReservedTranscription, RuntimeFailure>;
    async fn reserve_retry(
        &self,
        session_id: &str,
    ) -> Result<ReservedTranscription, RuntimeFailure>;

    async fn transcribe(
        &self,
        reservation: ReservedTranscription,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionOutcome, RuntimeFailure>;
}

#[async_trait]
pub trait RuntimeAdmin: Send + Sync {
    async fn status(&self) -> RuntimeStatus;
    async fn activate(&self, request: ActivateRuntime) -> Result<(), RuntimeFailure>;
}

type BuildFuture =
    Pin<Box<dyn Future<Output = Result<Arc<dyn crate::ModelRuntime>, RuntimeFailure>> + Send>>;
type RuntimeBuilder = dyn Fn(ActivateRuntime) -> BuildFuture + Send + Sync;

/// Registry owner used by the daemon composition root.  Model construction is
/// injected so this facade never knows catalog entries or backend DTOs.
pub struct RegistryRuntimeAdmin {
    gate: Arc<RuntimeOperationGate>,
    registry: Arc<SidecarRegistry>,
    builder: Arc<RuntimeBuilder>,
}

impl RegistryRuntimeAdmin {
    pub fn new(
        gate: Arc<RuntimeOperationGate>,
        registry: Arc<SidecarRegistry>,
        builder: Arc<RuntimeBuilder>,
    ) -> Self {
        Self {
            gate,
            registry,
            builder,
        }
    }
}

#[async_trait]
impl RuntimeAdmin for RegistryRuntimeAdmin {
    async fn status(&self) -> RuntimeStatus {
        let active = self.registry.active().await;
        RuntimeStatus {
            active_model_id: active.as_ref().map(|runtime| runtime.id().to_owned()),
            active_kind: active.as_ref().map(|runtime| runtime.runtime_kind()),
        }
    }

    async fn activate(&self, request: ActivateRuntime) -> Result<(), RuntimeFailure> {
        if self.registry.active_id().await.as_deref() == Some(request.model_id.as_str()) {
            return Ok(());
        }
        // Artifact resolve/verify/runtime construction happens before taking the
        // primary ASR slot. A construction failure must not block or stop the
        // currently active runtime.
        let runtime = (self.builder)(request.clone()).await?;
        let _lease = self.gate.acquire(crate::RuntimeOperation::ModelSwitch {
            model_id: request.model_id.clone(),
        })?;
        // The outer check is only a fast path. Another activation may have won
        // while construction was in flight, so consistency is decided again
        // while the shared gate is held.
        if self.registry.active_id().await.as_deref() == Some(request.model_id.as_str()) {
            return Ok(());
        }
        if let Some(old) = self.registry.active().await {
            old.stop()
                .await
                .map_err(|error| RuntimeFailure::Administration(error.to_string()))?;
        }
        self.registry.clear().await;
        runtime
            .start(0)
            .await
            .map_err(|error| RuntimeFailure::Administration(error.to_string()))?;
        self.registry.register(runtime).await;
        Ok(())
    }
}

/// Small default adapter useful during migration and in deterministic tests.
/// It owns the same registry and gate rather than creating a second slot.
pub struct RegistryTranscriptionEngine {
    gate: Arc<RuntimeOperationGate>,
    registry: Arc<SidecarRegistry>,
    diarizer: Arc<dyn ExternalDiarizer>,
}

impl RegistryTranscriptionEngine {
    pub fn new(gate: Arc<RuntimeOperationGate>, registry: Arc<SidecarRegistry>) -> Self {
        Self {
            gate,
            registry,
            diarizer: Arc::new(EnvironmentExternalDiarizer),
        }
    }

    #[cfg(test)]
    fn with_diarizer(
        gate: Arc<RuntimeOperationGate>,
        registry: Arc<SidecarRegistry>,
        diarizer: Arc<dyn ExternalDiarizer>,
    ) -> Self {
        Self {
            gate,
            registry,
            diarizer,
        }
    }
}

#[async_trait]
trait ExternalDiarizer: Send + Sync {
    /// `Ok(None)` means the optional local diarizer is not configured.
    async fn run(&self, wav: &Path) -> Result<Option<Vec<DiarizationSegment>>, RuntimeError>;
}

struct EnvironmentExternalDiarizer;

#[async_trait]
impl ExternalDiarizer for EnvironmentExternalDiarizer {
    async fn run(&self, wav: &Path) -> Result<Option<Vec<DiarizationSegment>>, RuntimeError> {
        match resolve_diarize_driver() {
            Some(driver) => driver.run(wav).await.map(Some),
            None => Ok(None),
        }
    }
}

#[async_trait]
impl TranscriptionEngine for RegistryTranscriptionEngine {
    async fn reserve(&self, session_id: &str) -> Result<ReservedTranscription, RuntimeFailure> {
        reserve_transcription(&self.gate, &self.registry, session_id)
            .await
            .map_err(Into::into)
    }

    async fn reserve_retry(
        &self,
        session_id: &str,
    ) -> Result<ReservedTranscription, RuntimeFailure> {
        reserve_retry_transcription(&self.gate, &self.registry, session_id)
            .await
            .map_err(Into::into)
    }

    async fn transcribe(
        &self,
        reservation: ReservedTranscription,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionOutcome, RuntimeFailure> {
        let (runtime, _model_id, lease) = reservation.into_parts();
        let capabilities = runtime.capabilities();
        let mut response = runtime.transcribe(request.backend_request()).await?;
        // Release the primary runtime slot before canonical projection and any
        // enrichment work. The response is already detached from the
        // backend at this point.
        drop(lease);
        if !has_visible_content(&response) {
            return Ok(TranscriptionOutcome::NoSpeech);
        }
        if matches!(capabilities.diarization, Diarization::External) {
            match self.diarizer.run(&request.wav).await {
                Ok(Some(segments)) => merge_speakers(&mut response, &segments),
                Ok(None) => tracing::warn!(
                    error_code = "diarization_unavailable",
                    "external diarization is not configured; transcript remains successful"
                ),
                Err(_) => tracing::warn!(
                    error_code = "diarization_failed",
                    "external diarization failed; transcript remains successful"
                ),
            }
        }
        Ok(TranscriptionOutcome::Transcript(
            CanonicalTranscript::from_openai_with_duration(&response, request.duration_ms),
        ))
    }
}

fn has_visible_content(response: &crate::contract::OpenAiSegments) -> bool {
    crate::contract::has_visible_text(&response.text)
        || response
            .segments
            .iter()
            .any(|segment| crate::contract::has_visible_text(&segment.text))
        || response
            .words
            .iter()
            .any(|word| crate::contract::has_visible_text(&word.text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{OpenAiSegment, OpenAiSegments};
    use crate::{Capabilities, ModelRuntime};
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Mutex;

    struct DirectRuntime {
        id: String,
        kind: RuntimeKind,
        diarization: Diarization,
        last_request: Mutex<Option<TranscribeReq>>,
        response: Result<OpenAiSegments, String>,
    }

    #[async_trait]
    impl ModelRuntime for DirectRuntime {
        fn id(&self) -> &str {
            &self.id
        }
        fn runtime_kind(&self) -> RuntimeKind {
            self.kind
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                diarization: self.diarization,
                streaming: false,
                languages: vec!["mixed".into()],
                max_audio_seconds: 60,
                word_timestamps: false,
            }
        }
        async fn start(&self, _: u16) -> io::Result<()> {
            Ok(())
        }
        async fn stop(&self) -> io::Result<()> {
            Ok(())
        }
        async fn health(&self) -> bool {
            true
        }
        async fn transcribe(&self, request: TranscribeReq) -> Result<OpenAiSegments, RuntimeError> {
            *self.last_request.lock().await = Some(request);
            self.response.clone().map_err(RuntimeError::Protocol)
        }
    }

    struct InspectingDiarizer {
        gate: Arc<RuntimeOperationGate>,
        fail: bool,
        called: AtomicBool,
    }

    #[async_trait]
    impl ExternalDiarizer for InspectingDiarizer {
        async fn run(&self, _: &Path) -> Result<Option<Vec<DiarizationSegment>>, RuntimeError> {
            assert!(
                self.gate.active().is_none(),
                "ASR lease must be released first"
            );
            self.called.store(true, Ordering::SeqCst);
            if self.fail {
                Err(RuntimeError::Decode("bad diarization output".into()))
            } else {
                Ok(Some(vec![DiarizationSegment {
                    start: 0.0,
                    end: 1.0,
                    speaker: "speaker_0".into(),
                }]))
            }
        }
    }

    fn request() -> TranscriptionRequest {
        TranscriptionRequest {
            wav: PathBuf::from("audio.wav"),
            language: Some("mixed".into()),
            prompt: None,
            punc: Some(true),
            spk: Some(false),
            duration_ms: 1_000,
        }
    }

    fn visible_response() -> OpenAiSegments {
        OpenAiSegments {
            text: "hello".into(),
            segments: vec![OpenAiSegment {
                start: 0.0,
                end: 1.0,
                text: "hello".into(),
                speaker: None,
            }],
            words: vec![],
        }
    }

    #[tokio::test]
    async fn facade_releases_primary_slot_before_best_effort_enrichment() {
        let gate = Arc::new(RuntimeOperationGate::new_for_test());
        let registry = Arc::new(SidecarRegistry::new());
        let runtime = Arc::new(DirectRuntime {
            id: "direct".into(),
            kind: RuntimeKind::Whisper,
            diarization: Diarization::External,
            last_request: Mutex::new(None),
            response: Ok(visible_response()),
        });
        registry.register(runtime.clone()).await;
        let diarizer = Arc::new(InspectingDiarizer {
            gate: Arc::clone(&gate),
            fail: false,
            called: AtomicBool::new(false),
        });
        let engine = RegistryTranscriptionEngine::with_diarizer(
            Arc::clone(&gate),
            registry,
            diarizer.clone(),
        );
        let reserved = engine.reserve("s1").await.unwrap();
        let TranscriptionOutcome::Transcript(transcript) =
            engine.transcribe(reserved, request()).await.unwrap()
        else {
            panic!("visible response must produce a transcript");
        };
        assert_eq!(transcript.speaker_roster, vec!["A"]);
        assert!(diarizer.called.load(Ordering::SeqCst));
        let captured = runtime.last_request.lock().await;
        let captured = captured.as_ref().unwrap();
        assert_eq!(captured.punc, Some(true));
        assert_eq!(captured.spk, Some(false));
    }

    #[tokio::test]
    async fn enrichment_failure_keeps_successful_canonical_text() {
        let gate = Arc::new(RuntimeOperationGate::new_for_test());
        let registry = Arc::new(SidecarRegistry::new());
        registry
            .register(Arc::new(DirectRuntime {
                id: "direct".into(),
                kind: RuntimeKind::Whisper,
                diarization: Diarization::External,
                last_request: Mutex::new(None),
                response: Ok(visible_response()),
            }))
            .await;
        let engine = RegistryTranscriptionEngine::with_diarizer(
            Arc::clone(&gate),
            registry,
            Arc::new(InspectingDiarizer {
                gate: Arc::clone(&gate),
                fail: true,
                called: AtomicBool::new(false),
            }),
        );
        let reserved = engine.reserve("s1").await.unwrap();
        let TranscriptionOutcome::Transcript(transcript) =
            engine.transcribe(reserved, request()).await.unwrap()
        else {
            panic!("visible response must produce a transcript");
        };
        assert_eq!(transcript.full_text, "hello");
        assert!(transcript.speaker_roster.is_empty());
        assert!(gate.active().is_none());
    }

    #[tokio::test]
    async fn runtime_admin_rechecks_idempotency_under_gate_after_build() {
        let gate = Arc::new(RuntimeOperationGate::new_for_test());
        let registry = Arc::new(SidecarRegistry::new());
        let registered = Arc::new(DirectRuntime {
            id: "model-a".into(),
            kind: RuntimeKind::Whisper,
            diarization: Diarization::None,
            last_request: Mutex::new(None),
            response: Ok(visible_response()),
        }) as Arc<dyn ModelRuntime>;
        let built = Arc::new(DirectRuntime {
            id: "model-a".into(),
            kind: RuntimeKind::Whisper,
            diarization: Diarization::None,
            last_request: Mutex::new(None),
            response: Ok(visible_response()),
        }) as Arc<dyn ModelRuntime>;
        let builder_registry = Arc::clone(&registry);
        let builder_registered = Arc::clone(&registered);
        let builder: Arc<RuntimeBuilder> = Arc::new(move |_| {
            let registry = Arc::clone(&builder_registry);
            let registered = Arc::clone(&builder_registered);
            let built = Arc::clone(&built);
            Box::pin(async move {
                registry.register(registered).await;
                Ok(built)
            })
        });
        let admin = RegistryRuntimeAdmin::new(Arc::clone(&gate), Arc::clone(&registry), builder);
        admin
            .activate(ActivateRuntime {
                model_id: "model-a".into(),
            })
            .await
            .unwrap();
        assert!(gate.active().is_none());
        assert!(Arc::ptr_eq(&registry.active().await.unwrap(), &registered));
    }

    #[test]
    fn visible_content_accepts_text_from_any_normalized_source() {
        use crate::contract::OpenAiWord;

        let mut response = OpenAiSegments {
            text: "content".into(),
            segments: vec![],
            words: vec![],
        };
        assert!(has_visible_content(&response));

        response.text.clear();
        response.segments.push(OpenAiSegment {
            start: 0.0,
            end: 1.0,
            text: "segment".into(),
            speaker: None,
        });
        assert!(has_visible_content(&response));

        response.segments.clear();
        response.words.push(OpenAiWord {
            start: 0.0,
            end: 1.0,
            text: "word".into(),
        });
        assert!(has_visible_content(&response));
    }

    #[test]
    fn visible_content_rejects_ascii_and_unicode_whitespace() {
        use crate::contract::OpenAiWord;

        let response = OpenAiSegments {
            text: " \t\r\n".into(),
            segments: vec![OpenAiSegment {
                start: 0.0,
                end: 0.0,
                text: "\u{00a0}\u{2003}".into(),
                speaker: None,
            }],
            words: vec![OpenAiWord {
                start: 0.0,
                end: 0.0,
                text: "\u{3000}".into(),
            }],
        };
        assert!(!has_visible_content(&response));
    }

    #[tokio::test]
    async fn no_speech_skips_external_diarization_and_releases_slot() {
        let gate = Arc::new(RuntimeOperationGate::new_for_test());
        let registry = Arc::new(SidecarRegistry::new());
        registry
            .register(Arc::new(DirectRuntime {
                id: "direct".into(),
                kind: RuntimeKind::Whisper,
                diarization: Diarization::External,
                last_request: Mutex::new(None),
                response: Ok(OpenAiSegments {
                    text: " \n".into(),
                    segments: vec![],
                    words: vec![],
                }),
            }))
            .await;
        let diarizer = Arc::new(InspectingDiarizer {
            gate: Arc::clone(&gate),
            fail: false,
            called: AtomicBool::new(false),
        });
        let engine = RegistryTranscriptionEngine::with_diarizer(
            Arc::clone(&gate),
            registry,
            diarizer.clone(),
        );

        let reserved = engine.reserve("s1").await.unwrap();
        let outcome = engine.transcribe(reserved, request()).await.unwrap();

        assert_eq!(outcome, TranscriptionOutcome::NoSpeech);
        assert!(!diarizer.called.load(Ordering::SeqCst));
        assert!(gate.active().is_none());
    }

    #[tokio::test]
    async fn every_backend_kind_maps_a_valid_empty_response_to_no_speech() {
        for kind in [
            RuntimeKind::SherpaOnnx,
            RuntimeKind::FunAsr,
            RuntimeKind::Whisper,
            RuntimeKind::Gguf,
        ] {
            let gate = Arc::new(RuntimeOperationGate::new_for_test());
            let registry = Arc::new(SidecarRegistry::new());
            registry
                .register(Arc::new(DirectRuntime {
                    id: format!("{kind:?}"),
                    kind,
                    diarization: Diarization::None,
                    last_request: Mutex::new(None),
                    response: Ok(OpenAiSegments {
                        text: String::new(),
                        segments: vec![],
                        words: vec![],
                    }),
                }))
                .await;
            let engine = RegistryTranscriptionEngine::new(Arc::clone(&gate), registry);
            let reserved = engine.reserve("s1").await.unwrap();

            assert_eq!(
                engine.transcribe(reserved, request()).await.unwrap(),
                TranscriptionOutcome::NoSpeech,
                "backend kind {kind:?}"
            );
        }
    }

    #[tokio::test]
    async fn every_backend_kind_maps_visible_content_to_transcript() {
        for kind in [
            RuntimeKind::SherpaOnnx,
            RuntimeKind::FunAsr,
            RuntimeKind::Whisper,
            RuntimeKind::Gguf,
        ] {
            let gate = Arc::new(RuntimeOperationGate::new_for_test());
            let registry = Arc::new(SidecarRegistry::new());
            registry
                .register(Arc::new(DirectRuntime {
                    id: format!("{kind:?}"),
                    kind,
                    diarization: Diarization::None,
                    last_request: Mutex::new(None),
                    response: Ok(visible_response()),
                }))
                .await;
            let engine = RegistryTranscriptionEngine::new(Arc::clone(&gate), registry);
            let reserved = engine.reserve("s1").await.unwrap();

            assert!(matches!(
                engine.transcribe(reserved, request()).await.unwrap(),
                TranscriptionOutcome::Transcript(_)
            ));
        }
    }

    #[tokio::test]
    async fn every_backend_kind_preserves_protocol_failures() {
        for kind in [
            RuntimeKind::SherpaOnnx,
            RuntimeKind::FunAsr,
            RuntimeKind::Whisper,
            RuntimeKind::Gguf,
        ] {
            let gate = Arc::new(RuntimeOperationGate::new_for_test());
            let registry = Arc::new(SidecarRegistry::new());
            registry
                .register(Arc::new(DirectRuntime {
                    id: format!("{kind:?}"),
                    kind,
                    diarization: Diarization::None,
                    last_request: Mutex::new(None),
                    response: Err("contradictory response".into()),
                }))
                .await;
            let engine = RegistryTranscriptionEngine::new(Arc::clone(&gate), registry);
            let reserved = engine.reserve("s1").await.unwrap();

            assert!(matches!(
                engine.transcribe(reserved, request()).await,
                Err(RuntimeFailure::Runtime(RuntimeError::Protocol(message)))
                    if message == "contradictory response"
            ));
        }
    }
}
