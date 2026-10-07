//! 统一推理基础设施（M3）：endpoint 安全、provider registry、hardened HTTP 与
//! 统一非流式推理入口。业务层（cleanup 等）只面对本模块的统一契约，不出现厂商分支。

pub mod credential;
pub mod endpoint;
pub mod http;
pub mod probe;
pub mod registry;
pub mod service;

use seasnail_crypto::CredentialEnvelope;
use seasnail_storage::ProviderType;

/// 一次性读取的 provider 配置 + credential 快照。发请求期间配置被修改/删除不影响
/// 本次请求；credential 只活到本次 future 结束。刻意不实现 Debug/Clone：内含
/// 不可调试、不可复制的 CredentialSecret。
pub struct ProviderSnapshot {
    pub config_id: String,
    pub provider_type: ProviderType,
    /// 存储层规范化后的 canonical endpoint（`endpoint::canonicalize` 幂等重验）。
    pub endpoint: String,
    pub endpoint_fingerprint: [u8; 32],
    /// 用户手填的 model 标识（MVP 不做 `/models` 自动发现）。
    pub model: String,
    /// Keychain vault 读取结果；None 表示该 config 从未设置 credential/无认证。
    pub credential: Option<CredentialEnvelope>,
}

/// 输出契约：ReasoningService 只按契约决定结构化输出请求形态，不解析业务 schema。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputContract {
    /// cleanup 业务：要求 JSON 对象输出；具体 `response_format` 由 adapter 能力决定，
    /// 无结构化能力的 provider 回退为纯 prompt 约束。
    JsonObject,
    /// probe 等自由文本请求：不发送 `response_format`。
    FreeformText,
}

/// 统一推理错误类别（设计「统一契约」）。对外只暴露类别与脱敏静态描述，
/// 绝不包含 upstream body、prompt、transcript、credential 或完整 endpoint query。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningErrorKind {
    /// 配置不完整或与 provider 不兼容（含 provider 4xx 拒绝请求形状）。
    Configuration,
    /// endpoint 未通过结构/地址分类/denylist/credential 传输策略。
    EndpointRejected,
    /// 必需 credential 缺失，或 envelope 与 config snapshot 绑定失配（stale）。
    CredentialMissing,
    /// DNS 解析失败。
    Dns,
    /// TCP 连接或其他传输层失败。
    Connect,
    /// TLS 握手或证书校验失败。
    Tls,
    /// 超过调用方给定的总 deadline（含 connect timeout）。
    Timeout,
    /// 401/403。
    HttpAuth,
    /// 429。
    HttpRateLimit,
    /// 5xx。
    HttpServer,
    /// 响应 body 超过受限大小。
    ResponseTooLarge,
    /// 响应不是合法/完整的 Chat Completions JSON。
    ResponseInvalid,
    /// 调用方取消（MVP 主要由 deadline 覆盖，保留语义位）。
    Cancelled,
}

impl ReasoningErrorKind {
    /// 面向 cleanup/Probe API 的稳定脱敏错误码。DNS、连接、TLS 与取消都属于
    /// 客户端可重试的传输失败，不向调用方暴露底层网络细节。
    pub const fn cleanup_error_code(self) -> &'static str {
        match self {
            Self::Configuration => "cleanup_not_configured",
            Self::EndpointRejected => "cleanup_endpoint_rejected",
            Self::CredentialMissing => "cleanup_credential_missing",
            Self::Dns | Self::Connect | Self::Tls | Self::Cancelled => "cleanup_transport_error",
            Self::Timeout => "cleanup_timeout",
            Self::HttpAuth => "cleanup_http_auth",
            Self::HttpRateLimit => "cleanup_http_rate_limit",
            Self::HttpServer => "cleanup_http_server",
            Self::ResponseTooLarge => "cleanup_response_too_large",
            Self::ResponseInvalid => "cleanup_response_invalid_json",
        }
    }
}

/// 统一推理错误。`message` 刻意限定为 `&'static str`，从类型上阻断敏感内容插值。
#[derive(thiserror::Error)]
#[error("{message}")]
pub struct ReasoningError {
    kind: ReasoningErrorKind,
    message: &'static str,
    diagnostics: Option<ReasoningDiagnostics>,
    elapsed_ms: u64,
}

impl std::fmt::Debug for ReasoningError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReasoningError")
            .field("kind", &self.kind)
            .field("message", &self.message)
            .field("has_diagnostics", &self.diagnostics.is_some())
            .field("elapsed_ms", &self.elapsed_ms)
            .finish()
    }
}

/// live cleanup 可持久化的单次调用诊断。手工 Debug 永不输出 provider body。
#[derive(Clone, Default)]
pub struct ReasoningDiagnostics {
    pub trace_id: String,
    pub request_started_at_ms: i64,
    pub response_started_at_ms: i64,
    pub response_completed_at_ms: i64,
    pub http_status: Option<u16>,
    pub response_content_type: String,
    pub provider_request_id: String,
    pub raw_response_body: Vec<u8>,
    pub response_sha256: Vec<u8>,
    pub response_body_bytes: u64,
    pub capture_status: DiagnosticCaptureStatus,
}

impl std::fmt::Debug for ReasoningDiagnostics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReasoningDiagnostics")
            .field("trace_id", &self.trace_id)
            .field("http_status", &self.http_status)
            .field("response_body_bytes", &self.response_body_bytes)
            .field("capture_status", &self.capture_status)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DiagnosticCaptureStatus {
    #[default]
    NotReceived,
    Complete,
    TooLarge,
    ReadFailed,
    Redacted,
}

impl ReasoningError {
    pub(crate) fn new(kind: ReasoningErrorKind, message: &'static str) -> Self {
        Self {
            kind,
            message,
            diagnostics: None,
            elapsed_ms: 0,
        }
    }

    pub fn kind(&self) -> ReasoningErrorKind {
        self.kind
    }

    pub fn into_diagnostics(self) -> Option<ReasoningDiagnostics> {
        self.diagnostics
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.elapsed_ms
    }

    pub(crate) fn with_diagnostics(mut self, diagnostics: ReasoningDiagnostics) -> Self {
        self.diagnostics = Some(diagnostics);
        self
    }

    pub(crate) fn with_elapsed_ms(mut self, elapsed_ms: u64) -> Self {
        self.elapsed_ms = elapsed_ms;
        self
    }

    pub(crate) fn configuration(message: &'static str) -> Self {
        Self::new(ReasoningErrorKind::Configuration, message)
    }

    pub(crate) fn endpoint_rejected(message: &'static str) -> Self {
        Self::new(ReasoningErrorKind::EndpointRejected, message)
    }

    pub(crate) fn credential_missing(message: &'static str) -> Self {
        Self::new(ReasoningErrorKind::CredentialMissing, message)
    }

    pub(crate) fn dns(message: &'static str) -> Self {
        Self::new(ReasoningErrorKind::Dns, message)
    }

    pub(crate) fn connect(message: &'static str) -> Self {
        Self::new(ReasoningErrorKind::Connect, message)
    }

    pub(crate) fn tls(message: &'static str) -> Self {
        Self::new(ReasoningErrorKind::Tls, message)
    }

    pub(crate) fn timeout(message: &'static str) -> Self {
        Self::new(ReasoningErrorKind::Timeout, message)
    }

    pub(crate) fn response_too_large(message: &'static str) -> Self {
        Self::new(ReasoningErrorKind::ResponseTooLarge, message)
    }

    pub(crate) fn response_invalid(message: &'static str) -> Self {
        Self::new(ReasoningErrorKind::ResponseInvalid, message)
    }
}
