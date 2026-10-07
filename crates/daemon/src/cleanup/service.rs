//! CleanupService 与短期 presentation cache（ST-M4.6）。
//!
//! 本模块只负责一次 cleanup execution 的构造、推理调用和本地校验，返回可供
//! M5 pipeline 提交的 artifact 候选；不会在这里改变 session 状态。execution
//! snapshot 取得后不再读取外部配置，避免请求期间配置变更影响在飞请求。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use seasnail_proto::seasnail::v1::{
    CleanupContextPlacement, CleanupDiagnostics, CleanupFile, CleanupOutcome, ClipboardContextFile,
    Correction, CorrectionKind as ProtoKind, DiagnosticCaptureStatus as ProtoCaptureStatus,
    PlaceholderValidationStatus, TranscriptFile, TranscriptUnit, UnitGranularity,
};

use super::correction::{validate_cleanup_json, CleanupJsonError, CorrectionKind};
use super::marker::{build_marker_input, build_marker_input_with_dictionary_and_rng, MarkerInput};
use super::output::{validate_and_split, ValidatedMarkerOutput};
use super::prompt::{default_prompt, EffectivePrompt};
use crate::reasoning::probe::{ProbeError, ProbeOutcome};
use crate::reasoning::service::{ReasoningRequest, ReasoningResponse, ReasoningService, UseCase};
use crate::reasoning::{
    DiagnosticCaptureStatus, OutputContract, ProviderSnapshot, ReasoningDiagnostics,
    ReasoningErrorKind,
};
use seasnail_storage::ProviderType;

pub const PRESENTATION_CACHE_MAX_ENTRIES: usize = 16;
pub const PRESENTATION_CACHE_MAX_BYTES: usize = 2 * 1024 * 1024;
pub const PRESENTATION_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
pub const MAX_CLEANUP_INPUT_BYTES: usize = 64 * 1024;

/// 请求开始前冻结的 cleanup 配置。`provider` 为 None 表示配置失效；当 `enabled`
/// 为 false 时不会构造 marker、不会调用 provider，也不会创建 artifact/cache。
pub struct CleanupExecutionSnapshot {
    pub account_id: String,
    pub session_id: String,
    pub enabled: bool,
    pub provider: Option<ProviderSnapshot>,
    pub prompt: EffectivePrompt,
    /// realtime pipeline 开始到 raw transcript 原子提交完成的单调时钟耗时。
    pub local_transcription_elapsed_ms: u64,
}

impl CleanupExecutionSnapshot {
    pub fn disabled(
        account_id: impl Into<String>,
        session_id: impl Into<String>,
        prompt: EffectivePrompt,
    ) -> Self {
        Self {
            account_id: account_id.into(),
            session_id: session_id.into(),
            enabled: false,
            provider: None,
            prompt,
            local_transcription_elapsed_ms: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentationEntry {
    pub parts: Vec<String>,
    pub event_sequences: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CleanupExecution {
    Disabled,
    Succeeded {
        artifact: CleanupFile,
        presentation: PresentationEntry,
    },
    Failed {
        artifact: CleanupFile,
    },
}

/// Prompt Studio 的一次性已校验结果。只包含 UI 所需的纯文本、合法 correction
/// 和耗时，不包含 provider 响应正文、endpoint、credential 或持久化标识。
#[derive(Clone, PartialEq, Eq)]
pub struct CleanupTestOutcome {
    pub cleaned_text: String,
    pub corrections: Vec<super::correction::ValidatedCorrection>,
    pub elapsed_ms: u64,
}

impl std::fmt::Debug for CleanupTestOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CleanupTestOutcome")
            .field("correction_count", &self.corrections.len())
            .field("elapsed_ms", &self.elapsed_ms)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupTestError {
    Busy,
    Failed(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupFailure {
    NotConfigured,
    InputTooLarge,
    PlaceholderInvalid,
    ResponseInvalidJson,
    CleanedTextInvalid,
    EndpointRejected,
    CredentialMissing,
    Timeout,
    HttpAuth,
    HttpRateLimit,
    HttpServer,
    ResponseTooLarge,
    Transport,
}

impl CleanupFailure {
    const fn code(self) -> &'static str {
        match self {
            Self::NotConfigured => "cleanup_not_configured",
            Self::InputTooLarge => "cleanup_input_too_large",
            Self::PlaceholderInvalid => "cleanup_placeholder_invalid",
            Self::ResponseInvalidJson => "cleanup_response_invalid_json",
            Self::CleanedTextInvalid => "cleanup_cleaned_text_invalid",
            Self::EndpointRejected => "cleanup_endpoint_rejected",
            Self::CredentialMissing => "cleanup_credential_missing",
            Self::Timeout => "cleanup_timeout",
            Self::HttpAuth => "cleanup_http_auth",
            Self::HttpRateLimit => "cleanup_http_rate_limit",
            Self::HttpServer => "cleanup_http_server",
            Self::ResponseTooLarge => "cleanup_response_too_large",
            Self::Transport => "cleanup_transport_error",
        }
    }
}

pub struct CleanupService {
    reasoning: Arc<ReasoningService>,
    presentation: Arc<CleanupPresentationCache>,
}

impl Default for CleanupService {
    fn default() -> Self {
        Self::new(Arc::new(ReasoningService::new()))
    }
}

impl CleanupService {
    pub fn new(reasoning: Arc<ReasoningService>) -> Self {
        Self {
            reasoning,
            presentation: Arc::new(CleanupPresentationCache::new()),
        }
    }

    pub fn with_cache(
        reasoning: Arc<ReasoningService>,
        presentation: Arc<CleanupPresentationCache>,
    ) -> Self {
        Self {
            reasoning,
            presentation,
        }
    }

    pub fn presentation_cache(&self) -> Arc<CleanupPresentationCache> {
        Arc::clone(&self.presentation)
    }

    /// 配置写入前复用 ReasoningService 的 endpoint 安全边界，不发起模型请求。
    pub async fn validate_provider_endpoint(
        &self,
        provider_type: ProviderType,
        endpoint: &str,
        carries_credential: bool,
    ) -> Result<(), crate::reasoning::ReasoningError> {
        self.reasoning
            .validate_provider_endpoint(provider_type, endpoint, carries_credential)
            .await
    }

    /// 复用正式 reasoning transport 的一次性 Probe，不写入 cleanup/session/artifact。
    pub async fn probe(
        &self,
        account_id: &str,
        snapshot: ProviderSnapshot,
    ) -> Result<ProbeOutcome, ProbeError> {
        self.reasoning.probe(account_id, snapshot).await
    }

    /// 执行一次 Prompt Studio 测试。该路径只使用内存中的 transcript/provider/prompt
    /// 快照，不创建 session、artifact 或 presentation cache，也不修改正式设置。
    pub async fn test(
        &self,
        account_id: &str,
        provider: ProviderSnapshot,
        prompt: EffectivePrompt,
        text: String,
    ) -> Result<CleanupTestOutcome, CleanupTestError> {
        let _flight = self
            .reasoning
            .acquire_probe_test_flight(account_id)
            .map_err(|()| CleanupTestError::Busy)?;
        let transcript = TranscriptFile {
            full_text: text.clone(),
            units: if text.is_empty() {
                Vec::new()
            } else {
                vec![TranscriptUnit {
                    sequence: 0,
                    text,
                    granularity: UnitGranularity::Untimed as i32,
                    ..Default::default()
                }]
            },
            ..Default::default()
        };
        let input = build_marker_input(&transcript, &ClipboardContextFile::default());
        if input.transcript_with_markers.len() > MAX_CLEANUP_INPUT_BYTES
            || input.user_json.len() > MAX_CLEANUP_INPUT_BYTES
        {
            return Err(CleanupTestError::Failed(
                CleanupFailure::InputTooLarge.code(),
            ));
        }
        let response = self
            .reasoning
            .complete(ReasoningRequest {
                use_case: UseCase::CleanupTest,
                provider,
                system_prompt: prompt.text,
                user_json: input.user_json.clone(),
                output_contract: OutputContract::JsonObject,
            })
            .await
            .map_err(|error| CleanupTestError::Failed(map_reasoning_error(error.kind()).code()))?;
        let marker_output = validate_marker_output(&response.content, &input)
            .map_err(|failure| CleanupTestError::Failed(failure.code()))?;
        let output =
            validate_cleanup_json(&response.content, &transcript.full_text, &marker_output)
                .map_err(|error| CleanupTestError::Failed(map_json_error(error).code()))?;
        Ok(CleanupTestOutcome {
            cleaned_text: output.cleaned_text,
            corrections: output.corrections,
            elapsed_ms: response.elapsed_ms,
        })
    }

    /// 为 snapshot/transcript/context 预检阶段构造 failure artifact。此时可能没有
    /// 可用 provider，因此允许 provider_config_id/model 为空，但仍保存有效 prompt hash。
    pub(crate) fn preflight_failure_artifact(
        account_id: &str,
        session_id: &str,
        error_code: &str,
    ) -> CleanupFile {
        CleanupFile {
            schema_version: 1,
            session_id: session_id.to_owned(),
            account_id: account_id.to_owned(),
            outcome: CleanupOutcome::Failed as i32,
            prompt_sha256: default_prompt().sha256.to_vec(),
            error_code: error_code.to_owned(),
            placeholder_validation: PlaceholderValidationStatus::PlaceholderValidationNotRun as i32,
            ..Default::default()
        }
    }

    pub(crate) fn failure_artifact_for_snapshot(
        snapshot: &CleanupExecutionSnapshot,
        error_code: &str,
    ) -> CleanupFile {
        let (provider_config_id, model) = snapshot
            .provider
            .as_ref()
            .map(|provider| (provider.config_id.as_str(), provider.model.as_str()))
            .unwrap_or(("", ""));
        CleanupFile {
            schema_version: 1,
            session_id: snapshot.session_id.clone(),
            account_id: snapshot.account_id.clone(),
            outcome: CleanupOutcome::Failed as i32,
            provider_config_id: provider_config_id.to_owned(),
            model: model.to_owned(),
            prompt_sha256: snapshot.prompt.sha256.to_vec(),
            error_code: error_code.to_owned(),
            placeholder_validation: PlaceholderValidationStatus::PlaceholderValidationNotRun as i32,
            ..Default::default()
        }
    }

    /// 执行一次 live cleanup。返回值中的 artifact 尚未写盘/提交 DB，由 M5 负责在
    /// session mutation lock 内按 CAS 顺序持久化；失败也返回 best-effort artifact 候选。
    pub async fn execute(
        &self,
        snapshot: CleanupExecutionSnapshot,
        transcript: &TranscriptFile,
        context: &ClipboardContextFile,
    ) -> CleanupExecution {
        self.execute_with_dictionary(snapshot, transcript, context, &[])
            .await
    }

    /// 执行 live cleanup，并将任务创建时冻结的词条作为不可信 JSON 数据注入。
    pub async fn execute_with_dictionary(
        &self,
        snapshot: CleanupExecutionSnapshot,
        transcript: &TranscriptFile,
        context: &ClipboardContextFile,
        dictionary_terms: &[String],
    ) -> CleanupExecution {
        if !snapshot.enabled {
            return CleanupExecution::Disabled;
        }
        // ReasoningService owns the live deadline and returns its shared diagnostic snapshot on
        // timeout. Marker construction and validation are synchronously bounded by the input/body
        // limits, so a second equal outer timeout would only race and discard that snapshot.
        self.execute_inner(snapshot, transcript, context, dictionary_terms)
            .await
    }

    async fn execute_inner(
        &self,
        mut snapshot: CleanupExecutionSnapshot,
        transcript: &TranscriptFile,
        context: &ClipboardContextFile,
        dictionary_terms: &[String],
    ) -> CleanupExecution {
        if !snapshot.enabled {
            return CleanupExecution::Disabled;
        }
        let Some(provider) = snapshot.provider.take() else {
            return CleanupExecution::Failed {
                artifact: failure_artifact(&snapshot, CleanupFailure::NotConfigured, 0, "", ""),
            };
        };
        let provider_config_id = provider.config_id.clone();
        let provider_model = provider.model.clone();
        let (input, injected_terms) =
            build_bounded_marker_input(transcript, context, dictionary_terms);
        let has_context_markers = !input.bindings.is_empty();
        // 只记录数量与是否截断，词条内容不进入日志（设计「可观测性」允许字段）。
        if injected_terms < dictionary_terms.len() {
            tracing::warn!(
                offered = dictionary_terms.len(),
                injected = injected_terms,
                truncated = true,
                "cleanup dictionary terms trimmed to fit input budget"
            );
        }
        if input.transcript_with_markers.len() > MAX_CLEANUP_INPUT_BYTES
            || input.user_json.len() > MAX_CLEANUP_INPUT_BYTES
        {
            return CleanupExecution::Failed {
                artifact: failure_artifact_with_provider(
                    &snapshot,
                    &provider_config_id,
                    &provider_model,
                    CleanupFailure::InputTooLarge,
                    0,
                ),
            };
        }

        let response = self
            .reasoning
            .complete(ReasoningRequest {
                use_case: UseCase::CleanupLive,
                provider,
                system_prompt: snapshot.prompt.text.clone(),
                user_json: input.user_json.clone(),
                output_contract: OutputContract::JsonObject,
            })
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                let failure = map_reasoning_error(error.kind());
                let elapsed_ms = error.elapsed_ms();
                let diagnostics = error.into_diagnostics();
                let mut artifact = failure_artifact(
                    &snapshot,
                    failure,
                    elapsed_ms,
                    &provider_config_id,
                    &provider_model,
                );
                artifact.diagnostics = diagnostics.as_ref().map(|diagnostics| {
                    cleanup_diagnostics(
                        diagnostics,
                        snapshot.local_transcription_elapsed_ms,
                        has_context_markers,
                    )
                });
                return CleanupExecution::Failed { artifact };
            }
        };

        let validated = match validate_marker_output(&response.content, &input) {
            Ok(validated) => validated,
            Err(failure) => {
                let mut artifact = failure_artifact_with_provider(
                    &snapshot,
                    &response.provider_config_id,
                    &response.model,
                    failure,
                    response.elapsed_ms,
                );
                artifact.diagnostics = Some(cleanup_diagnostics(
                    &response.diagnostics,
                    snapshot.local_transcription_elapsed_ms,
                    has_context_markers,
                ));
                return CleanupExecution::Failed { artifact };
            }
        };
        let output =
            match validate_cleanup_json(&response.content, &transcript.full_text, &validated) {
                Ok(output) => output,
                Err(error) => {
                    let failure = map_json_error(error);
                    let mut artifact = failure_artifact_with_provider(
                        &snapshot,
                        &response.provider_config_id,
                        &response.model,
                        failure,
                        response.elapsed_ms,
                    );
                    artifact.diagnostics = Some(cleanup_diagnostics(
                        &response.diagnostics,
                        snapshot.local_transcription_elapsed_ms,
                        has_context_markers,
                    ));
                    return CleanupExecution::Failed { artifact };
                }
            };

        let presentation = PresentationEntry {
            parts: validated.parts,
            event_sequences: input
                .bindings
                .iter()
                .map(|binding| binding.event_sequence)
                .collect(),
        };
        let artifact = success_artifact(&snapshot, &response, &output, &presentation);
        CleanupExecution::Succeeded {
            artifact,
            presentation,
        }
    }

    /// 在 DB commit 成功后发布展示数据，避免旧/未提交结果进入 cache。
    pub fn publish_presentation(
        &self,
        account_id: &str,
        session_id: &str,
        presentation: PresentationEntry,
    ) -> bool {
        self.presentation
            .publish(account_id, session_id, presentation)
    }
}

fn build_bounded_marker_input(
    transcript: &TranscriptFile,
    context: &ClipboardContextFile,
    dictionary_terms: &[String],
) -> (MarkerInput, usize) {
    let mut terms = dictionary_terms.to_vec();
    let mut input = build_marker_input_with_dictionary_and_rng(
        transcript,
        context,
        &terms,
        &mut rand::rngs::OsRng,
    );
    // 词条按 snapshot 的稳定顺序从尾部缩减；transcript 和 marker 永不截断。
    while (input.transcript_with_markers.len() > MAX_CLEANUP_INPUT_BYTES
        || input.user_json.len() > MAX_CLEANUP_INPUT_BYTES)
        && !terms.is_empty()
    {
        terms.pop();
        input = build_marker_input_with_dictionary_and_rng(
            transcript,
            context,
            &terms,
            &mut rand::rngs::OsRng,
        );
    }
    let count = terms.len();
    (input, count)
}

fn validate_marker_output(
    response: &str,
    input: &MarkerInput,
) -> Result<ValidatedMarkerOutput, CleanupFailure> {
    // The model response is a JSON object; extracting cleaned_text before marker validation
    // prevents markers in corrections or unrelated JSON fields from being treated as正文。
    let value: serde_json::Value =
        serde_json::from_str(response).map_err(|_| CleanupFailure::ResponseInvalidJson)?;
    let cleaned_text = value
        .get("cleaned_text")
        .and_then(serde_json::Value::as_str)
        .ok_or(CleanupFailure::ResponseInvalidJson)?;
    validate_and_split(cleaned_text, &input.bindings)
        .map_err(|_| CleanupFailure::PlaceholderInvalid)
}

fn map_json_error(error: CleanupJsonError) -> CleanupFailure {
    match error {
        CleanupJsonError::ResponseTooLarge => CleanupFailure::ResponseTooLarge,
        CleanupJsonError::TopLevelInvalid => CleanupFailure::ResponseInvalidJson,
        CleanupJsonError::CleanedTextInvalid => CleanupFailure::CleanedTextInvalid,
    }
}

fn map_reasoning_error(kind: ReasoningErrorKind) -> CleanupFailure {
    match kind {
        ReasoningErrorKind::Configuration => CleanupFailure::NotConfigured,
        ReasoningErrorKind::EndpointRejected => CleanupFailure::EndpointRejected,
        ReasoningErrorKind::CredentialMissing => CleanupFailure::CredentialMissing,
        ReasoningErrorKind::Timeout => CleanupFailure::Timeout,
        ReasoningErrorKind::HttpAuth => CleanupFailure::HttpAuth,
        ReasoningErrorKind::HttpRateLimit => CleanupFailure::HttpRateLimit,
        ReasoningErrorKind::HttpServer => CleanupFailure::HttpServer,
        ReasoningErrorKind::ResponseTooLarge => CleanupFailure::ResponseTooLarge,
        ReasoningErrorKind::ResponseInvalid => CleanupFailure::ResponseInvalidJson,
        ReasoningErrorKind::Dns
        | ReasoningErrorKind::Connect
        | ReasoningErrorKind::Tls
        | ReasoningErrorKind::Cancelled => CleanupFailure::Transport,
    }
}

fn failure_artifact(
    snapshot: &CleanupExecutionSnapshot,
    failure: CleanupFailure,
    elapsed_ms: u64,
    provider_config_id: &str,
    model: &str,
) -> CleanupFile {
    CleanupFile {
        schema_version: 1,
        session_id: snapshot.session_id.clone(),
        account_id: snapshot.account_id.clone(),
        outcome: CleanupOutcome::Failed as i32,
        provider_config_id: provider_config_id.to_owned(),
        model: model.to_owned(),
        prompt_sha256: snapshot.prompt.sha256.to_vec(),
        error_code: failure.code().into(),
        elapsed_ms,
        placeholder_validation: PlaceholderValidationStatus::PlaceholderValidationNotRun as i32,
        ..Default::default()
    }
}

fn failure_artifact_with_provider(
    snapshot: &CleanupExecutionSnapshot,
    provider_config_id: &str,
    model: &str,
    failure: CleanupFailure,
    elapsed_ms: u64,
) -> CleanupFile {
    CleanupFile {
        schema_version: 1,
        session_id: snapshot.session_id.clone(),
        account_id: snapshot.account_id.clone(),
        outcome: CleanupOutcome::Failed as i32,
        provider_config_id: provider_config_id.to_owned(),
        model: model.to_owned(),
        prompt_sha256: snapshot.prompt.sha256.to_vec(),
        error_code: failure.code().into(),
        elapsed_ms,
        placeholder_validation: if matches!(failure, CleanupFailure::PlaceholderInvalid) {
            PlaceholderValidationStatus::PlaceholderValidationFailed as i32
        } else {
            PlaceholderValidationStatus::PlaceholderValidationNotRun as i32
        },
        ..Default::default()
    }
}

fn success_artifact(
    snapshot: &CleanupExecutionSnapshot,
    response: &ReasoningResponse,
    output: &super::correction::ValidatedCleanupOutput,
    presentation: &PresentationEntry,
) -> CleanupFile {
    let mut byte_offset = 0_u64;
    let context_placements = presentation
        .event_sequences
        .iter()
        .zip(&presentation.parts)
        .map(|(event_sequence, part)| {
            byte_offset = byte_offset.saturating_add(part.len() as u64);
            CleanupContextPlacement {
                event_sequence: *event_sequence,
                byte_offset,
            }
        })
        .collect();
    CleanupFile {
        schema_version: 1,
        session_id: snapshot.session_id.clone(),
        account_id: snapshot.account_id.clone(),
        outcome: CleanupOutcome::Succeeded as i32,
        cleaned_text: output.cleaned_text.clone(),
        corrections: output
            .corrections
            .iter()
            .map(|correction| Correction {
                original_text: correction.original_text.clone(),
                corrected_text: correction.corrected_text.clone(),
                kind: match correction.kind {
                    CorrectionKind::Phonetic => ProtoKind::Phonetic as i32,
                    CorrectionKind::ProperNoun => ProtoKind::ProperNoun as i32,
                    CorrectionKind::OtherAsr => ProtoKind::OtherAsr as i32,
                },
            })
            .collect(),
        provider_config_id: response.provider_config_id.clone(),
        model: response.model.clone(),
        prompt_sha256: snapshot.prompt.sha256.to_vec(),
        elapsed_ms: response.elapsed_ms,
        placeholder_validation: PlaceholderValidationStatus::PlaceholderValidationPassed as i32,
        context_placements,
        diagnostics: Some(cleanup_diagnostics(
            &response.diagnostics,
            snapshot.local_transcription_elapsed_ms,
            !presentation.event_sequences.is_empty(),
        )),
        ..Default::default()
    }
}

fn cleanup_diagnostics(
    diagnostics: &ReasoningDiagnostics,
    local_transcription_elapsed_ms: u64,
    has_context_markers: bool,
) -> CleanupDiagnostics {
    // 非零 context 请求的 provider 响应可能合法回显随机 marker。即使 marker 不含
    // context payload，也不能把请求级随机标识写入历史 artifact，因此整段正文诊断
    // 按敏感响应处理；长度和摘要仍可用于排障。
    let raw_response_body = if has_context_markers {
        Vec::new()
    } else {
        diagnostics.raw_response_body.clone()
    };
    CleanupDiagnostics {
        schema_version: 1,
        trace_id: diagnostics.trace_id.clone(),
        request_started_at_ms: diagnostics.request_started_at_ms,
        response_started_at_ms: diagnostics.response_started_at_ms,
        response_completed_at_ms: diagnostics.response_completed_at_ms,
        http_status: diagnostics.http_status.map(u32::from),
        response_content_type: diagnostics.response_content_type.clone(),
        provider_request_id: diagnostics.provider_request_id.clone(),
        raw_response_body,
        response_sha256: diagnostics.response_sha256.clone(),
        response_body_bytes: diagnostics.response_body_bytes,
        capture_status: if has_context_markers {
            ProtoCaptureStatus::Redacted as i32
        } else {
            match diagnostics.capture_status {
                DiagnosticCaptureStatus::NotReceived => ProtoCaptureStatus::NotReceived as i32,
                DiagnosticCaptureStatus::Complete => ProtoCaptureStatus::Complete as i32,
                DiagnosticCaptureStatus::TooLarge => ProtoCaptureStatus::TooLarge as i32,
                DiagnosticCaptureStatus::ReadFailed => ProtoCaptureStatus::ReadFailed as i32,
                DiagnosticCaptureStatus::Redacted => ProtoCaptureStatus::Redacted as i32,
            }
        },
        local_transcription_elapsed_ms: Some(local_transcription_elapsed_ms),
    }
}

struct CacheEntry {
    value: PresentationEntry,
    expires_at: Instant,
    bytes: usize,
}

/// 只保存正文分片和 event sequence；marker、context payload、credential 均不进入 cache。
pub struct CleanupPresentationCache {
    entries: Mutex<HashMap<(String, String), CacheEntry>>,
}

impl Default for CleanupPresentationCache {
    fn default() -> Self {
        Self::new()
    }
}

impl CleanupPresentationCache {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub fn publish(&self, account_id: &str, session_id: &str, value: PresentationEntry) -> bool {
        let bytes = value.parts.iter().map(String::len).sum::<usize>();
        if bytes > PRESENTATION_CACHE_MAX_BYTES
            || value.parts.len() != value.event_sequences.len() + 1
        {
            return false;
        }
        let now = Instant::now();
        let mut entries = self
            .entries
            .lock()
            .expect("cleanup presentation cache mutex poisoned");
        entries.retain(|_, entry| entry.expires_at > now);
        entries.insert(
            (account_id.to_owned(), session_id.to_owned()),
            CacheEntry {
                value,
                expires_at: now + PRESENTATION_CACHE_TTL,
                bytes,
            },
        );
        while entries.len() > PRESENTATION_CACHE_MAX_ENTRIES
            || total_bytes(&entries) > PRESENTATION_CACHE_MAX_BYTES
        {
            let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, entry)| entry.expires_at)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            entries.remove(&oldest);
        }
        true
    }

    pub fn get(&self, account_id: &str, session_id: &str) -> Option<PresentationEntry> {
        let now = Instant::now();
        let mut entries = self
            .entries
            .lock()
            .expect("cleanup presentation cache mutex poisoned");
        entries.retain(|_, entry| entry.expires_at > now);
        entries
            .get(&(account_id.to_owned(), session_id.to_owned()))
            .map(|entry| entry.value.clone())
    }

    pub fn remove_session(&self, account_id: &str, session_id: &str) {
        self.entries
            .lock()
            .expect("cleanup presentation cache mutex poisoned")
            .remove(&(account_id.to_owned(), session_id.to_owned()));
    }

    pub fn remove_account(&self, account_id: &str) {
        self.entries
            .lock()
            .expect("cleanup presentation cache mutex poisoned")
            .retain(|(cached_account, _), _| cached_account != account_id);
    }

    pub fn len(&self) -> usize {
        let now = Instant::now();
        let mut entries = self
            .entries
            .lock()
            .expect("cleanup presentation cache mutex poisoned");
        entries.retain(|_, entry| entry.expires_at > now);
        entries.len()
    }
}

fn total_bytes(entries: &HashMap<(String, String), CacheEntry>) -> usize {
    entries.values().map(|entry| entry.bytes).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cleanup::prompt::{compose_prompt, default_prompt};
    use seasnail_crypto::CredentialEnvelope;
    use seasnail_storage::ProviderType;
    use std::net::{IpAddr, Ipv4Addr};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn dictionary_terms_are_reduced_before_cleanup_input_limit() {
        let transcript = TranscriptFile {
            full_text: "short".into(),
            ..Default::default()
        };
        let context = ClipboardContextFile::default();
        let terms = (0..1_000)
            .map(|index| format!("dictionary-term-{index:04}-{}", "x".repeat(80)))
            .collect::<Vec<_>>();
        let (input, retained) = build_bounded_marker_input(&transcript, &context, &terms);
        assert!(input.user_json.len() <= MAX_CLEANUP_INPUT_BYTES);
        assert!(retained < terms.len());
        let value: serde_json::Value = serde_json::from_str(&input.user_json).unwrap();
        assert_eq!(
            value["dictionary_terms"].as_array().unwrap().len(),
            retained
        );
    }
    use tokio::net::TcpListener;

    fn test_provider(port: u16) -> ProviderSnapshot {
        let fingerprint = [7; 32];
        ProviderSnapshot {
            config_id: "00000000-0000-4000-8000-000000000001".into(),
            provider_type: ProviderType::SelfHostedPrivate,
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            endpoint_fingerprint: fingerprint,
            model: "manual-model".into(),
            credential: Some(
                CredentialEnvelope::no_auth(
                    "openai_compatible_self_hosted_private".into(),
                    fingerprint,
                )
                .unwrap(),
            ),
        }
    }

    async fn cleanup_mock(
        cleaned_json: &'static str,
        delay: Duration,
    ) -> (u16, tokio::sync::oneshot::Receiver<String>) {
        let listener = TcpListener::bind((IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sent, received) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                let read = stream.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
                let Some(header_end) = buffer.windows(4).position(|part| part == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&buffer[..header_end]).to_lowercase();
                let length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buffer.len() >= header_end + 4 + length {
                    break;
                }
            }
            let _ = sent.send(String::from_utf8_lossy(&buffer).into_owned());
            tokio::time::sleep(delay).await;
            let body = serde_json::json!({
                "choices": [{
                    "message": { "content": cleaned_json },
                    "finish_reason": "stop"
                }]
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
        (port, received)
    }

    #[test]
    fn cache_keeps_only_reversible_parts_and_enforces_limits() {
        let cache = CleanupPresentationCache::new();
        assert!(cache.publish(
            "a",
            "s",
            PresentationEntry {
                parts: vec!["left".into(), "right".into()],
                event_sequences: vec![7]
            },
        ));
        let hit = cache.get("a", "s").unwrap();
        assert_eq!(hit.parts, vec!["left", "right"]);
        assert_eq!(hit.event_sequences, vec![7]);
        assert!(!cache.publish(
            "a",
            "bad",
            PresentationEntry {
                parts: vec!["x".repeat(PRESENTATION_CACHE_MAX_BYTES + 1)],
                event_sequences: vec![]
            },
        ));
    }

    #[tokio::test]
    async fn disabled_snapshot_does_not_create_artifact_or_cache() {
        let service = CleanupService::default();
        let snapshot = CleanupExecutionSnapshot::disabled("a", "s", default_prompt());
        let result = service
            .execute(
                snapshot,
                &TranscriptFile::default(),
                &ClipboardContextFile::default(),
            )
            .await;
        assert_eq!(result, CleanupExecution::Disabled);
        assert_eq!(service.presentation_cache().len(), 0);
    }

    #[tokio::test]
    async fn invalid_configuration_and_missing_key_fail_closed_before_network() {
        let service = CleanupService::default();
        let transcript = TranscriptFile {
            full_text: "raw".into(),
            units: vec![seasnail_proto::seasnail::v1::TranscriptUnit {
                text: "raw".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let disabled_provider = CleanupExecutionSnapshot::disabled("a", "s", default_prompt());
        assert_eq!(
            service
                .execute(
                    disabled_provider,
                    &transcript,
                    &ClipboardContextFile::default()
                )
                .await,
            CleanupExecution::Disabled
        );
        let unconfigured = CleanupExecutionSnapshot {
            account_id: "account".into(),
            session_id: "session".into(),
            enabled: true,
            provider: None,
            prompt: default_prompt(),
            local_transcription_elapsed_ms: 0,
        };
        let CleanupExecution::Failed { artifact } = service
            .execute(unconfigured, &transcript, &ClipboardContextFile::default())
            .await
        else {
            panic!("missing configuration must fail");
        };
        assert_eq!(artifact.error_code, "cleanup_not_configured");

        let snapshot = CleanupExecutionSnapshot {
            account_id: "account".into(),
            session_id: "session".into(),
            enabled: true,
            provider: Some(ProviderSnapshot {
                config_id: "config".into(),
                provider_type: ProviderType::SelfHostedPrivate,
                endpoint: "http://127.0.0.1:1/v1".into(),
                endpoint_fingerprint: [7; 32],
                model: "model".into(),
                credential: None,
            }),
            prompt: default_prompt(),
            local_transcription_elapsed_ms: 0,
        };
        let CleanupExecution::Failed { artifact } = service
            .execute(snapshot, &transcript, &ClipboardContextFile::default())
            .await
        else {
            panic!("missing credential must fail");
        };
        assert_eq!(artifact.error_code, "cleanup_credential_missing");
        assert_eq!(artifact.provider_config_id, "config");
    }

    #[tokio::test]
    async fn prompt_studio_test_returns_validated_output_without_publishing_cache() {
        const OUTPUT: &str = r#"{"cleaned_text":"正确词","corrections":[{"original_text":"错误词","corrected_text":"正确词","kind":"phonetic"}]}"#;
        let (port, received) = cleanup_mock(OUTPUT, Duration::ZERO).await;
        let service = CleanupService::default();
        let outcome = service
            .test(
                "account-a",
                test_provider(port),
                compose_prompt("DRAFT_PROMPT_SENTINEL").unwrap(),
                "错误词".into(),
            )
            .await
            .unwrap();
        assert_eq!(outcome.cleaned_text, "正确词");
        assert_eq!(outcome.corrections.len(), 1);
        assert_eq!(outcome.corrections[0].kind, CorrectionKind::Phonetic);
        assert_eq!(service.presentation_cache().len(), 0);
        let request = received.await.unwrap();
        assert!(request.contains("DRAFT_PROMPT_SENTINEL"));
    }

    #[test]
    fn cleanup_test_debug_output_does_not_expose_text() {
        let outcome = CleanupTestOutcome {
            cleaned_text: "CLEANED_TEXT_SECRET_SENTINEL".into(),
            corrections: vec![crate::cleanup::correction::ValidatedCorrection {
                original_text: "ORIGINAL_SECRET_SENTINEL".into(),
                corrected_text: "CORRECTED_SECRET_SENTINEL".into(),
                kind: CorrectionKind::ProperNoun,
            }],
            elapsed_ms: 42,
        };
        let debug = format!("{outcome:?}");
        assert!(!debug.contains("SECRET_SENTINEL"));
        assert!(debug.contains("correction_count: 1"));
        assert!(debug.contains("elapsed_ms: 42"));
    }

    #[tokio::test]
    async fn prompt_studio_test_shares_singleflight_with_other_tests() {
        const OUTPUT: &str = r#"{"cleaned_text":"clean","corrections":[]}"#;
        let (port, received) = cleanup_mock(OUTPUT, Duration::from_millis(50)).await;
        let service = Arc::new(CleanupService::default());
        let running = {
            let service = Arc::clone(&service);
            tokio::spawn(async move {
                service
                    .test(
                        "account-a",
                        test_provider(port),
                        default_prompt(),
                        "raw".into(),
                    )
                    .await
            })
        };
        let _request = received.await.unwrap();
        assert_eq!(
            service
                .test(
                    "account-a",
                    test_provider(port),
                    default_prompt(),
                    "raw".into(),
                )
                .await,
            Err(CleanupTestError::Busy)
        );
        assert!(running.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn prompt_studio_test_rejects_oversized_input_before_network() {
        let service = CleanupService::default();
        assert_eq!(
            service
                .test(
                    "account-a",
                    test_provider(1),
                    default_prompt(),
                    "x".repeat(MAX_CLEANUP_INPUT_BYTES + 1),
                )
                .await,
            Err(CleanupTestError::Failed("cleanup_input_too_large"))
        );
        assert_eq!(service.presentation_cache().len(), 0);
    }

    #[test]
    fn validated_success_is_projected_to_artifact_and_errors_are_stable() {
        let snapshot = CleanupExecutionSnapshot::disabled("account", "session", default_prompt());
        let response = ReasoningResponse {
            content: String::new(),
            provider_config_id: "config".into(),
            model: "model".into(),
            elapsed_ms: 12,
            diagnostics: ReasoningDiagnostics {
                raw_response_body: br#"{"cleaned_text":"[[SEASNAIL_CTX_V1:7:secret]]"}"#.to_vec(),
                capture_status: DiagnosticCaptureStatus::Complete,
                ..Default::default()
            },
        };
        let output = super::super::correction::ValidatedCleanupOutput {
            cleaned_text: "clean".into(),
            corrections: Vec::new(),
        };
        let presentation = PresentationEntry {
            parts: vec!["cl".into(), "ean".into()],
            event_sequences: vec![7],
        };
        let artifact = success_artifact(&snapshot, &response, &output, &presentation);
        assert_eq!(artifact.outcome, CleanupOutcome::Succeeded as i32);
        assert_eq!(artifact.cleaned_text, "clean");
        assert_eq!(artifact.context_placements[0].event_sequence, 7);
        assert_eq!(artifact.context_placements[0].byte_offset, 2);
        let diagnostics = artifact.diagnostics.as_ref().unwrap();
        assert!(diagnostics.raw_response_body.is_empty());
        assert_eq!(
            diagnostics.capture_status,
            ProtoCaptureStatus::Redacted as i32
        );
        assert_eq!(artifact.prompt_sha256.len(), 32);
        assert_eq!(
            map_reasoning_error(ReasoningErrorKind::EndpointRejected).code(),
            "cleanup_endpoint_rejected"
        );
        assert_eq!(
            map_reasoning_error(ReasoningErrorKind::Timeout).code(),
            "cleanup_timeout"
        );
    }
}
