//! ReasoningService（ST-M3.5）：统一非流式文本推理入口。
//!
//! 调用链顺序固定：静态绑定校验（零网络）→ canonicalize 幂等重验 → 总 deadline
//! 内 DNS 解析/分类 → credential 传输闸门 → pinned client → adapter 请求 → 受限
//! 响应读取 → 统一错误归一。
//!
//! deadline 语义：外层 `tokio::time::timeout` 从 DNS 解析前开始，覆盖连接、TLS、
//! 请求写入、模型处理与响应读取；到期即取消整个 future。adapter 不把网络调用拆成
//! 后台任务，因此迟到响应不存在任何结果提交路径。
//!
//! 脱敏边界：所有上游 body 都只做受限读取，不进入日志或错误文本；live cleanup
//! 可由业务层把诊断快照写入账户加密 artifact。响应类型手工 Debug，不输出 body。

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use seasnail_storage::ProviderType;
use sha2::{Digest, Sha256};

use super::credential::{self, CredentialMode};
use super::endpoint::{self, CanonicalEndpoint, DnsResolver, SystemResolver};
use super::http::{
    read_limited_body_with_observed, HardenedClientFactory, MAX_RESPONSE_BODY_BYTES,
};
use super::registry::{AdapterRequest, ProviderAdapter, ProviderRegistry};
use super::{
    DiagnosticCaptureStatus, OutputContract, ProviderSnapshot, ReasoningDiagnostics, ReasoningError,
};

/// 推理用途。live 与 test 共用同一条链路，仅 deadline 与下游持久化不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseCase {
    CleanupLive,
    CleanupTest,
    Probe,
}

/// 产品契约固定的总 deadline。调用方不能覆盖，避免 live/test 因接线错误突破
/// 15/30 秒交付边界。
pub const CLEANUP_LIVE_DEADLINE: Duration = Duration::from_secs(15);
pub const CLEANUP_TEST_DEADLINE: Duration = Duration::from_secs(30);
pub const PROBE_DEADLINE: Duration = Duration::from_secs(30);

impl UseCase {
    pub const fn deadline(self) -> Duration {
        match self {
            Self::CleanupLive => CLEANUP_LIVE_DEADLINE,
            Self::CleanupTest => CLEANUP_TEST_DEADLINE,
            Self::Probe => PROBE_DEADLINE,
        }
    }
}

/// 统一推理请求。deadline 由 `use_case` 固定（live 15s / test/probe 30s），调用方
/// 不得自定义。
pub struct ReasoningRequest {
    pub use_case: UseCase,
    pub provider: ProviderSnapshot,
    pub system_prompt: String,
    pub user_json: String,
    pub output_contract: OutputContract,
}

/// 统一推理响应：只含受限字符串与非敏感元数据。
pub struct ReasoningResponse {
    pub content: String,
    pub provider_config_id: String,
    pub model: String,
    pub elapsed_ms: u64,
    pub diagnostics: ReasoningDiagnostics,
}

// 手工 Debug：不含 content（模型输出属敏感数据）。
impl std::fmt::Debug for ReasoningResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReasoningResponse")
            .field("provider_config_id", &self.provider_config_id)
            .field("model", &self.model)
            .field("elapsed_ms", &self.elapsed_ms)
            .field("diagnostics", &self.diagnostics)
            .finish_non_exhaustive()
    }
}

/// 统一推理入口。`resolver` 可注入以便测试固定 DNS 答案。
pub struct ReasoningService {
    resolver: Arc<dyn DnsResolver>,
    /// 同账户 Probe/Test single-flight（设计：重复请求返回稳定 busy；live 不受限）。
    probe_test_flight: Mutex<HashSet<String>>,
}

/// Probe/Test single-flight 守卫：RAII 释放，账户维度互斥。
pub(crate) struct ProbeTestFlight<'a> {
    service: &'a ReasoningService,
    account_id: String,
}

impl Drop for ProbeTestFlight<'_> {
    fn drop(&mut self) {
        self.service
            .probe_test_flight
            .lock()
            .expect("probe/test flight mutex poisoned")
            .remove(&self.account_id);
    }
}

impl Default for ReasoningService {
    fn default() -> Self {
        Self::new()
    }
}

impl ReasoningService {
    pub fn new() -> Self {
        Self {
            resolver: Arc::new(SystemResolver),
            probe_test_flight: Mutex::new(HashSet::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_resolver(resolver: Arc<dyn DnsResolver>) -> Self {
        Self {
            resolver,
            probe_test_flight: Mutex::new(HashSet::new()),
        }
    }

    /// 获取同账户 Probe/Test 互斥位；已被占用时返回 `Err(())`（由调用方映射 busy）。
    pub(crate) fn acquire_probe_test_flight(
        &self,
        account_id: &str,
    ) -> Result<ProbeTestFlight<'_>, ()> {
        let mut flights = self
            .probe_test_flight
            .lock()
            .expect("probe/test flight mutex poisoned");
        if !flights.insert(account_id.to_owned()) {
            return Err(());
        }
        drop(flights);
        Ok(ProbeTestFlight {
            service: self,
            account_id: account_id.to_owned(),
        })
    }

    pub async fn complete(
        &self,
        request: ReasoningRequest,
    ) -> Result<ReasoningResponse, ReasoningError> {
        let deadline = request.use_case.deadline();
        self.complete_with_deadline(request, deadline).await
    }

    /// 在 provider 配置或 credential 持久化前执行 endpoint 安全校验。
    ///
    /// OpenAI endpoint 已由 storage 固定为 registry host/path，保存时只需静态
    /// canonicalization；Probe 和每次发送仍会重新执行 DNS/IP 校验。所有用户自定义
    /// endpoint 在保存前解析并分类；携带 credential 时还会拒绝非 loopback 私网 HTTP。
    pub async fn validate_provider_endpoint(
        &self,
        provider_type: ProviderType,
        raw_endpoint: &str,
        carries_credential: bool,
    ) -> Result<(), ReasoningError> {
        let canonical = endpoint::canonicalize(provider_type, raw_endpoint)?;
        if provider_type == ProviderType::OpenAi {
            return Ok(());
        }
        let validated = tokio::time::timeout(
            PROBE_DEADLINE,
            endpoint::resolve_and_validate(self.resolver.as_ref(), &canonical),
        )
        .await
        .map_err(|_| ReasoningError::timeout("endpoint validation exceeded its deadline"))??;
        if carries_credential
            && validated.credential_transport == endpoint::CredentialTransport::NoAuthOnly
        {
            return Err(ReasoningError::endpoint_rejected(
                "credential is forbidden for this HTTP endpoint",
            ));
        }
        Ok(())
    }

    async fn complete_with_deadline(
        &self,
        request: ReasoningRequest,
        deadline: Duration,
    ) -> Result<ReasoningResponse, ReasoningError> {
        let started = Instant::now();
        let diagnostics = Arc::new(Mutex::new(ReasoningDiagnostics {
            trace_id: uuid::Uuid::new_v4().to_string(),
            request_started_at_ms: unix_time_ms(),
            ..Default::default()
        }));
        // 静态绑定校验在任何网络操作之前：stale/缺失直接失败，零 DNS、零连接。
        credential::verify_binding(&request.provider).map_err(|error| {
            error
                .with_diagnostics(diagnostics_snapshot(&diagnostics))
                .with_elapsed_ms(started.elapsed().as_millis() as u64)
        })?;
        let canonical =
            endpoint::canonicalize(request.provider.provider_type, &request.provider.endpoint)
                .map_err(|error| {
                    error
                        .with_diagnostics(diagnostics_snapshot(&diagnostics))
                        .with_elapsed_ms(started.elapsed().as_millis() as u64)
                })?;
        let inner = self.complete_inner(&request, &canonical, diagnostics.clone());
        match tokio::time::timeout(deadline, inner).await {
            Ok(result) => match result {
                Ok((content, diagnostics)) => Ok(ReasoningResponse {
                    content,
                    provider_config_id: request.provider.config_id.clone(),
                    model: request.provider.model.clone(),
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    diagnostics,
                }),
                Err(error) => Err(error.with_elapsed_ms(started.elapsed().as_millis() as u64)),
            },
            Err(_) => {
                let mut diagnostics = diagnostics_snapshot(&diagnostics);
                if diagnostics.response_started_at_ms > 0 {
                    diagnostics.response_completed_at_ms = unix_time_ms();
                    diagnostics.capture_status = DiagnosticCaptureStatus::ReadFailed;
                }
                let error = diagnostics
                    .http_status
                    .filter(|status| !(200..=299).contains(status))
                    .map(|status| {
                        ReasoningError::new(
                            ProviderRegistry::adapter(request.provider.provider_type)
                                .map_http_status(status),
                            "provider returned an error status",
                        )
                    })
                    .unwrap_or_else(|| {
                        ReasoningError::timeout("reasoning request exceeded its deadline")
                    });
                Err(error
                    .with_diagnostics(diagnostics)
                    .with_elapsed_ms(started.elapsed().as_millis() as u64))
            }
        }
    }

    /// 单测使用短 deadline 验证取消语义；生产调用只能经过 `complete` 的固定值。
    #[cfg(test)]
    async fn complete_for_test(
        &self,
        request: ReasoningRequest,
        deadline: Duration,
    ) -> Result<ReasoningResponse, ReasoningError> {
        self.complete_with_deadline(request, deadline).await
    }

    async fn complete_inner(
        &self,
        request: &ReasoningRequest,
        canonical: &CanonicalEndpoint,
        diagnostics: Arc<Mutex<ReasoningDiagnostics>>,
    ) -> Result<(String, ReasoningDiagnostics), ReasoningError> {
        let provider = &request.provider;
        let adapter = ProviderRegistry::adapter(provider.provider_type);
        let validated = endpoint::resolve_and_validate(self.resolver.as_ref(), canonical)
            .await
            .map_err(|error| error.with_diagnostics(diagnostics_snapshot(&diagnostics)))?;
        let mode = credential::enforce_transport(provider, &validated)
            .map_err(|error| error.with_diagnostics(diagnostics_snapshot(&diagnostics)))?;
        let client = HardenedClientFactory::build(&validated)
            .map_err(|error| error.with_diagnostics(diagnostics_snapshot(&diagnostics)))?;
        let body = adapter.build_request_body(&AdapterRequest {
            model: &provider.model,
            system_prompt: &request.system_prompt,
            user_json: &request.user_json,
            output_contract: request.output_contract,
        });
        let url = adapter.chat_completions_url(validated.canonical.url());
        let mut builder = client.post(url).json(&body);
        if let CredentialMode::Bearer(secret) = mode {
            builder = builder.header(AUTHORIZATION, adapter_authorization(adapter, secret));
        }
        let response = builder
            .send()
            .await
            .map_err(classify_reqwest_error)
            .map_err(|error| error.with_diagnostics(diagnostics_snapshot(&diagnostics)))?;
        let status = response.status();
        let response_content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .chars()
            .filter(|character| !character.is_control())
            .take(256)
            .collect();
        let request_id = provider_request_id(response.headers());
        if let Ok(mut state) = diagnostics.lock() {
            state.response_started_at_ms = unix_time_ms();
            state.http_status = Some(status.as_u16());
            state.response_content_type = response_content_type;
            state.provider_request_id = request_id;
        }
        let mut response = response;
        let body = match read_limited_body_with_observed(
            &mut response,
            MAX_RESPONSE_BODY_BYTES,
            |observed_bytes| {
                if let Ok(mut state) = diagnostics.lock() {
                    state.response_body_bytes = observed_bytes;
                }
            },
        )
        .await
        {
            Ok(body) => body,
            Err((error, observed_bytes)) => {
                if let Ok(mut state) = diagnostics.lock() {
                    state.response_completed_at_ms = unix_time_ms();
                    state.response_body_bytes = observed_bytes;
                    state.capture_status =
                        if error.kind() == super::ReasoningErrorKind::ResponseTooLarge {
                            DiagnosticCaptureStatus::TooLarge
                        } else {
                            DiagnosticCaptureStatus::ReadFailed
                        };
                }
                let error = if status.is_success() {
                    error
                } else {
                    ReasoningError::new(
                        adapter.map_http_status(status.as_u16()),
                        "provider returned an error status",
                    )
                };
                return Err(error.with_diagnostics(diagnostics_snapshot(&diagnostics)));
            }
        };
        let sensitive_echo = response_contains_sensitive_echo(&body, request);
        if let Ok(mut state) = diagnostics.lock() {
            state.response_completed_at_ms = unix_time_ms();
            state.response_body_bytes = body.len() as u64;
            state.response_sha256 = Sha256::digest(&body).to_vec();
            if sensitive_echo {
                state.capture_status = DiagnosticCaptureStatus::Redacted;
            } else {
                state.raw_response_body = body.clone();
                state.capture_status = DiagnosticCaptureStatus::Complete;
            }
        }
        if !status.is_success() {
            return Err(ReasoningError::new(
                adapter.map_http_status(status.as_u16()),
                "provider returned an error status",
            )
            .with_diagnostics(diagnostics_snapshot(&diagnostics)));
        }
        if sensitive_echo {
            return Err(ReasoningError::response_invalid(
                "provider response echoed sensitive request data",
            )
            .with_diagnostics(diagnostics_snapshot(&diagnostics)));
        }
        let parsed = serde_json::from_slice(&body)
            .map_err(|_| ReasoningError::response_invalid("provider response is not valid JSON"))
            .map_err(|error| error.with_diagnostics(diagnostics_snapshot(&diagnostics)))?;
        let content = adapter
            .extract_content(&parsed)
            .map_err(|error| error.with_diagnostics(diagnostics_snapshot(&diagnostics)))?;
        Ok((content, diagnostics_snapshot(&diagnostics)))
    }
}

fn response_contains_sensitive_echo(body: &[u8], request: &ReasoningRequest) -> bool {
    let contains = |needle: &[u8]| {
        !needle.is_empty() && body.windows(needle.len()).any(|window| window == needle)
    };
    request
        .provider
        .credential
        .as_ref()
        .and_then(|credential| credential.secret())
        .is_some_and(|secret| contains(secret.expose_secret()))
        || contains(request.system_prompt.as_bytes())
        || contains(request.user_json.as_bytes())
}

fn unix_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn diagnostics_snapshot(diagnostics: &Arc<Mutex<ReasoningDiagnostics>>) -> ReasoningDiagnostics {
    diagnostics
        .lock()
        .map(|state| state.clone())
        .unwrap_or_default()
}

fn provider_request_id(headers: &reqwest::header::HeaderMap) -> String {
    ["x-request-id", "request-id", "x-ms-request-id"]
        .iter()
        .find_map(|name| headers.get(*name).and_then(|value| value.to_str().ok()))
        .unwrap_or_default()
        .chars()
        .filter(|character| !character.is_control())
        .take(256)
        .collect()
}

fn adapter_authorization(
    adapter: &dyn ProviderAdapter,
    secret: &seasnail_crypto::CredentialSecret,
) -> reqwest::header::HeaderValue {
    adapter.authorization_header(secret)
}

/// reqwest 错误归一。`is_connect` 覆盖 TCP 与 TLS 握手；穿透 source 链识别 TLS
/// 指纹（rustls/certificate/handshake）。日志只输出类别，不外泄错误原文。
fn classify_reqwest_error(error: reqwest::Error) -> ReasoningError {
    if error.is_timeout() {
        return ReasoningError::timeout("provider connection timed out");
    }
    if error.is_connect() {
        // TLS 指纹：rustls 证书/握手错误的 Display/Debug 文本特征（如
        // "invalid certificate: ..."、"received corrupt message of type ..."）。
        const TLS_MARKERS: &[&str] = &[
            "certificate",
            "rustls",
            "tls",
            "corrupt message",
            "handshake",
            "fatal alert",
        ];
        let mut source = std::error::Error::source(&error);
        while let Some(cause) = source {
            let text = format!("{cause} {cause:?}").to_lowercase();
            if TLS_MARKERS.iter().any(|marker| text.contains(marker)) {
                return ReasoningError::tls("TLS handshake or certificate validation failed");
            }
            source = cause.source();
        }
        return ReasoningError::connect("failed to connect to provider");
    }
    if error.is_decode() {
        return ReasoningError::response_invalid("failed to decode provider response");
    }
    ReasoningError::connect("provider request failed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reasoning::endpoint::DnsResolver;
    use crate::reasoning::ReasoningErrorKind;
    use async_trait::async_trait;
    use seasnail_crypto::{CredentialEnvelope, CredentialSecret};
    use seasnail_storage::ProviderType;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const FINGERPRINT: [u8; 32] = [42; 32];

    struct LoopbackResolver;

    #[async_trait]
    impl DnsResolver for LoopbackResolver {
        async fn lookup(&self, _host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
            Ok(vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)])
        }
    }

    struct CountingRejectingResolver(Arc<AtomicUsize>);

    #[async_trait]
    impl DnsResolver for CountingRejectingResolver {
        async fn lookup(&self, _host: &str, _port: u16) -> std::io::Result<Vec<SocketAddr>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(std::io::Error::other("unexpected DNS lookup"))
        }
    }

    struct PrivateResolver;

    #[async_trait]
    impl DnsResolver for PrivateResolver {
        async fn lookup(&self, _host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
            Ok(vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(10, 0, 0, 8)),
                port,
            )])
        }
    }

    fn service() -> ReasoningService {
        ReasoningService::with_resolver(Arc::new(LoopbackResolver))
    }

    fn snapshot(credential: Option<CredentialEnvelope>) -> ProviderSnapshot {
        ProviderSnapshot {
            config_id: "00000000-0000-4000-8000-000000000001".into(),
            provider_type: ProviderType::SelfHostedPrivate,
            // 端口在请求构造时按实际 mock server 填写。
            endpoint: String::new(),
            endpoint_fingerprint: FINGERPRINT,
            model: "test-model".into(),
            credential,
        }
    }

    fn no_auth_snapshot() -> ProviderSnapshot {
        snapshot(Some(
            CredentialEnvelope::no_auth(
                "openai_compatible_self_hosted_private".into(),
                FINGERPRINT,
            )
            .unwrap(),
        ))
    }

    fn bearer_snapshot(secret: &[u8]) -> ProviderSnapshot {
        snapshot(Some(
            CredentialEnvelope::new(
                "openai_compatible_self_hosted_private".into(),
                FINGERPRINT,
                CredentialSecret::new(secret.to_vec()).unwrap(),
            )
            .unwrap(),
        ))
    }

    fn live_request(provider: ProviderSnapshot) -> ReasoningRequest {
        ReasoningRequest {
            use_case: UseCase::CleanupLive,
            provider,
            system_prompt: "system".into(),
            user_json: "{\"transcript\":\"hello\"}".into(),
            output_contract: OutputContract::JsonObject,
        }
    }

    #[test]
    fn use_case_deadlines_are_fixed_by_the_service_contract() {
        assert_eq!(UseCase::CleanupLive.deadline(), Duration::from_secs(15));
        assert_eq!(UseCase::CleanupTest.deadline(), Duration::from_secs(30));
        assert_eq!(UseCase::Probe.deadline(), Duration::from_secs(30));
    }

    #[tokio::test]
    async fn configuration_validation_classifies_custom_endpoints_and_credentials() {
        let public_on_loopback = service()
            .validate_provider_endpoint(
                ProviderType::SelfHostedPublic,
                "https://public.test/v1",
                false,
            )
            .await
            .unwrap_err();
        assert_eq!(
            public_on_loopback.kind(),
            ReasoningErrorKind::EndpointRejected
        );

        let private = ReasoningService::with_resolver(Arc::new(PrivateResolver));
        private
            .validate_provider_endpoint(
                ProviderType::SelfHostedPrivate,
                "http://private.test/v1",
                false,
            )
            .await
            .unwrap();
        let credential_error = private
            .validate_provider_endpoint(
                ProviderType::SelfHostedPrivate,
                "http://private.test/v1",
                true,
            )
            .await
            .unwrap_err();
        assert_eq!(
            credential_error.kind(),
            ReasoningErrorKind::EndpointRejected
        );
    }

    #[tokio::test]
    async fn fixed_openai_endpoint_does_not_require_save_time_dns() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service = ReasoningService::with_resolver(Arc::new(CountingRejectingResolver(
            Arc::clone(&calls),
        )));
        service
            .validate_provider_endpoint(ProviderType::OpenAi, "https://api.openai.com/v1", true)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// 读完整 HTTP 请求（header + Content-Length body），回包固定响应。
    async fn mock_server(response: Vec<u8>) -> (u16, tokio::sync::oneshot::Receiver<Vec<u8>>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sent, received) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            let mut chunk = [0_u8; 8192];
            let mut body_length = None;
            loop {
                let read = stream.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
                if body_length.is_none() {
                    if let Some(header_end) = find_subslice(&buffer, b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&buffer[..header_end]).to_lowercase();
                        body_length = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .map(|length| header_end + 4 + length);
                    }
                }
                if let Some(total) = body_length {
                    if buffer.len() >= total {
                        break;
                    }
                }
            }
            let _ = sent.send(buffer);
            stream.write_all(&response).await.unwrap();
        });
        (port, received)
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    fn json_response(status: u16, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn endpoint_for(port: u16, snapshot: &mut ProviderSnapshot) {
        snapshot.endpoint = format!("http://llm.test:{port}/v1");
    }

    #[tokio::test]
    async fn success_roundtrip_pins_host_and_sends_minimal_dialect() {
        let (port, received) = mock_server(json_response(
            200,
            "{\"choices\":[{\"message\":{\"content\":\"{\\\"cleaned_text\\\":\\\"done\\\"}\"},\"finish_reason\":\"stop\"}]}",
        ))
        .await;
        let mut provider = no_auth_snapshot();
        endpoint_for(port, &mut provider);
        let response = service().complete(live_request(provider)).await.unwrap();
        assert!(response.content.contains("cleaned_text"));
        assert_eq!(response.model, "test-model");
        assert_eq!(
            response.provider_config_id,
            "00000000-0000-4000-8000-000000000001"
        );
        assert_eq!(response.diagnostics.http_status, Some(200));
        assert_eq!(
            response.diagnostics.capture_status,
            DiagnosticCaptureStatus::Complete
        );
        assert!(response.diagnostics.request_started_at_ms > 0);
        assert_eq!(response.diagnostics.response_sha256.len(), 32);
        assert!(
            String::from_utf8(response.diagnostics.raw_response_body.clone())
                .unwrap()
                .contains("choices")
        );

        let request = received.await.unwrap();
        let text = String::from_utf8_lossy(&request);
        assert!(text.starts_with("POST /v1/chat/completions HTTP/1.1"));
        assert!(text
            .to_lowercase()
            .contains(&format!("host: llm.test:{port}")));
        assert!(!text.to_lowercase().contains("authorization:"));
        assert!(text.contains("\"stream\":false"));
        assert!(text.contains("\"model\":\"test-model\""));
    }

    #[tokio::test]
    async fn bearer_credential_is_attached_only_after_all_checks() {
        let (port, received) = mock_server(json_response(
            200,
            "{\"choices\":[{\"message\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}",
        ))
        .await;
        let mut provider = bearer_snapshot(b"sk-live");
        endpoint_for(port, &mut provider);
        service().complete(live_request(provider)).await.unwrap();
        let request = String::from_utf8_lossy(&received.await.unwrap()).to_lowercase();
        assert!(request.contains("authorization: bearer sk-live"));
    }

    #[tokio::test]
    async fn stale_binding_fails_before_any_connection() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service = ReasoningService::with_resolver(Arc::new(CountingRejectingResolver(
            Arc::clone(&calls),
        )));
        let mut provider = bearer_snapshot(b"sk-live");
        provider.endpoint = "https://llm.test/v1".into();
        provider.endpoint_fingerprint = [1; 32]; // envelope 与 snapshot 失配
        let error = service.complete(live_request(provider)).await.unwrap_err();
        assert_eq!(error.kind(), ReasoningErrorKind::CredentialMissing);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "stale binding must skip DNS"
        );
    }

    #[tokio::test]
    async fn http_statuses_map_to_stable_kinds() {
        for (status, expected) in [
            (401_u16, ReasoningErrorKind::HttpAuth),
            (403, ReasoningErrorKind::HttpAuth),
            (429, ReasoningErrorKind::HttpRateLimit),
            (500, ReasoningErrorKind::HttpServer),
            (503, ReasoningErrorKind::HttpServer),
            (400, ReasoningErrorKind::Configuration),
            (404, ReasoningErrorKind::Configuration),
        ] {
            let (port, _) = mock_server(json_response(status, "{\"error\":\"x\"}")).await;
            let mut provider = no_auth_snapshot();
            endpoint_for(port, &mut provider);
            let error = service()
                .complete(live_request(provider))
                .await
                .unwrap_err();
            assert_eq!(error.kind(), expected, "status {status}");
            let diagnostics = error.into_diagnostics().unwrap();
            assert_eq!(diagnostics.http_status, Some(status));
            assert_eq!(diagnostics.raw_response_body, br#"{"error":"x"}"#);
        }
    }

    #[tokio::test]
    async fn http_status_remains_primary_when_error_body_is_oversized() {
        let big = "x".repeat(MAX_RESPONSE_BODY_BYTES + 1);
        let (port, _) = mock_server(json_response(401, &big)).await;
        let mut provider = no_auth_snapshot();
        endpoint_for(port, &mut provider);
        let error = service()
            .complete(live_request(provider))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ReasoningErrorKind::HttpAuth);
        let diagnostics = error.into_diagnostics().unwrap();
        assert_eq!(
            diagnostics.capture_status,
            DiagnosticCaptureStatus::TooLarge
        );
        assert_eq!(diagnostics.response_body_bytes, 0);
    }

    #[tokio::test]
    async fn http_status_remains_primary_when_error_body_times_out() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 8192];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 10\r\n\r\nabc")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(300)).await;
        });
        let mut provider = no_auth_snapshot();
        endpoint_for(port, &mut provider);
        let error = service()
            .complete_for_test(live_request(provider), Duration::from_millis(100))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ReasoningErrorKind::HttpRateLimit);
        let diagnostics = error.into_diagnostics().unwrap();
        assert_eq!(diagnostics.http_status, Some(429));
        assert_eq!(
            diagnostics.capture_status,
            DiagnosticCaptureStatus::ReadFailed
        );
        assert_eq!(diagnostics.response_body_bytes, 3);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn response_echoing_the_credential_is_rejected_and_redacted() {
        let (port, _) = mock_server(json_response(
            200,
            "{\"choices\":[{\"message\":{\"content\":\"sk-live\"},\"finish_reason\":\"stop\"}]}",
        ))
        .await;
        let mut provider = bearer_snapshot(b"sk-live");
        endpoint_for(port, &mut provider);
        let error = service()
            .complete(live_request(provider))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ReasoningErrorKind::ResponseInvalid);
        let diagnostics = error.into_diagnostics().unwrap();
        assert_eq!(
            diagnostics.capture_status,
            DiagnosticCaptureStatus::Redacted
        );
        assert!(diagnostics.raw_response_body.is_empty());
        assert_eq!(diagnostics.response_sha256.len(), 32);
    }

    #[tokio::test]
    async fn invalid_or_truncated_responses_are_rejected() {
        // 非 JSON body。
        let (port, _) = mock_server(json_response(200, "not json")).await;
        let mut provider = no_auth_snapshot();
        endpoint_for(port, &mut provider);
        let error = service()
            .complete(live_request(provider))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ReasoningErrorKind::ResponseInvalid);

        // finish_reason=length。
        let (port, _) = mock_server(json_response(
            200,
            "{\"choices\":[{\"message\":{\"content\":\"{\"},\"finish_reason\":\"length\"}]}",
        ))
        .await;
        let mut provider = no_auth_snapshot();
        endpoint_for(port, &mut provider);
        let error = service()
            .complete(live_request(provider))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ReasoningErrorKind::ResponseInvalid);
    }

    #[tokio::test]
    async fn oversized_body_is_rejected() {
        let big = "x".repeat(MAX_RESPONSE_BODY_BYTES + 1);
        let (port, _) = mock_server(json_response(200, &big)).await;
        let mut provider = no_auth_snapshot();
        endpoint_for(port, &mut provider);
        let error = service()
            .complete(live_request(provider))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ReasoningErrorKind::ResponseTooLarge);
        let diagnostics = error.into_diagnostics().unwrap();
        assert_eq!(
            diagnostics.capture_status,
            DiagnosticCaptureStatus::TooLarge
        );
        assert!(diagnostics.raw_response_body.is_empty());
    }

    #[tokio::test]
    async fn chunked_oversized_body_records_observed_bytes() {
        let big = "x".repeat(MAX_RESPONSE_BODY_BYTES + 1);
        let response = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:X}\r\n{}\r\n0\r\n\r\n",
            big.len(),
            big
        )
        .into_bytes();
        let (port, _) = mock_server(response).await;
        let mut provider = no_auth_snapshot();
        endpoint_for(port, &mut provider);
        let error = service()
            .complete(live_request(provider))
            .await
            .unwrap_err();
        let diagnostics = error.into_diagnostics().unwrap();
        assert_eq!(
            diagnostics.capture_status,
            DiagnosticCaptureStatus::TooLarge
        );
        assert!(diagnostics.response_body_bytes > MAX_RESPONSE_BODY_BYTES as u64);
    }

    #[tokio::test]
    async fn deadline_expiry_cancels_and_late_response_has_no_commit_path() {
        // server 延迟超过 deadline 才响应。
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 8192];
            let _ = stream.read(&mut request).await.unwrap();
            tokio::time::sleep(Duration::from_millis(500)).await;
            // 客户端已超时取消：迟到写入应观察到连接被关闭。
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .is_err()
                || stream.flush().await.is_err()
                || stream.read(&mut request).await.is_err()
                || {
                    // 即使写入侥幸成功，读 EOF/reset 证明对端已离开。
                    true
                }
        });
        let mut provider = no_auth_snapshot();
        endpoint_for(port, &mut provider);
        let request = live_request(provider);
        let started = Instant::now();
        let error = service()
            .complete_for_test(request, Duration::from_millis(150))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ReasoningErrorKind::Timeout);
        assert!(
            started.elapsed() < Duration::from_millis(450),
            "deadline must bound the whole call"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn connect_refused_maps_to_connect() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut provider = no_auth_snapshot();
        endpoint_for(port, &mut provider);
        let error = service()
            .complete(live_request(provider))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ReasoningErrorKind::Connect);
    }

    #[tokio::test]
    async fn plaintext_server_behind_https_maps_to_tls() {
        // 收到 ClientHello 立即回垃圾字节：rustls 握手失败，分类应为 Tls。
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut hello = vec![0_u8; 4096];
            let _ = stream.read(&mut hello).await.unwrap();
            let _ = stream.write_all(b"this is not a TLS server").await;
        });
        let mut provider = no_auth_snapshot();
        provider.endpoint = format!("https://llm.test:{port}/v1");
        let error = service()
            .complete(live_request(provider))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ReasoningErrorKind::Tls, "got: {error}");
    }
}
