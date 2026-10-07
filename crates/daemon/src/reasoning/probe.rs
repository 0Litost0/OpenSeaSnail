//! 单次 Probe（ST-M3.6）：用配置中手填的 model 验证实际可达性与模型兼容性。
//!
//! - 发送最小 chat completion（FreeformText 契约，不请求结构化输出），不调用
//!   `/models` 做自动发现；
//! - 同账户 Probe/Test single-flight：重复并发返回 [`ProbeError::Busy`]；
//! - 结果脱敏：只回传 model、耗时与能力摘要，不回传 secret 或上游 body；
//! - 无 token cap 方言（self-hosted 最小方言）明确标记 [`ProbeTokenCap::ClientOnly`]，
//!   供 UI 提示「只能依赖客户端 body/deadline，无法保证服务端生成上限」；
//! - Probe 只做用户显式触发的单次请求，不重试（设计「一致性、崩溃恢复与并发」）。

use super::registry::ProviderRegistry;
use super::service::{ReasoningRequest, ReasoningService, UseCase};
use super::{OutputContract, ProviderSnapshot, ReasoningError};

/// 服务端生成上限是否有协议保证。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeTokenCap {
    /// adapter 声明了 output-token 字段，请求已携带上限。
    ServerBounded,
    /// 最小方言无 token cap：只能依赖客户端 body/deadline。
    ClientOnly,
}

/// Probe 脱敏结果。刻意不含 secret、endpoint、请求/响应 body。
#[derive(Debug)]
pub struct ProbeOutcome {
    pub model: String,
    pub elapsed_ms: u64,
    pub token_cap: ProbeTokenCap,
    pub structured_output: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// 同账户已有 Probe/Test 在运行。
    #[error("another probe or test is already running for this account")]
    Busy,
    #[error(transparent)]
    Reasoning(#[from] ReasoningError),
}

impl ReasoningService {
    /// 执行单次 Probe。flight 守卫保证同账户互斥；请求链路与正式 cleanup 一致。
    pub async fn probe(
        &self,
        account_id: &str,
        snapshot: ProviderSnapshot,
    ) -> Result<ProbeOutcome, ProbeError> {
        let _flight = self
            .acquire_probe_test_flight(account_id)
            .map_err(|()| ProbeError::Busy)?;
        let capabilities = ProviderRegistry::adapter(snapshot.provider_type).capabilities();
        let response = self
            .complete(ReasoningRequest {
                use_case: UseCase::Probe,
                provider: snapshot,
                system_prompt: "You are a connectivity probe. Reply with a short acknowledgement."
                    .into(),
                user_json: "ping".into(),
                output_contract: OutputContract::FreeformText,
            })
            .await?;
        Ok(ProbeOutcome {
            model: response.model,
            elapsed_ms: response.elapsed_ms,
            token_cap: if capabilities.token_dialect.is_some() {
                ProbeTokenCap::ServerBounded
            } else {
                ProbeTokenCap::ClientOnly
            },
            structured_output: capabilities.structured_output.is_some(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reasoning::endpoint::DnsResolver;
    use async_trait::async_trait;
    use seasnail_crypto::{CredentialEnvelope, CredentialSecret};
    use seasnail_storage::ProviderType;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const FINGERPRINT: [u8; 32] = [7; 32];
    const ACCOUNT: &str = "00000000-0000-4000-8000-0000000000aa";
    const OTHER_ACCOUNT: &str = "00000000-0000-4000-8000-0000000000bb";

    struct LoopbackResolver;

    #[async_trait]
    impl DnsResolver for LoopbackResolver {
        async fn lookup(&self, _host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
            Ok(vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)])
        }
    }

    fn service() -> ReasoningService {
        ReasoningService::with_resolver(Arc::new(LoopbackResolver))
    }

    fn snapshot(port: u16) -> ProviderSnapshot {
        ProviderSnapshot {
            config_id: "00000000-0000-4000-8000-000000000001".into(),
            provider_type: ProviderType::SelfHostedPrivate,
            endpoint: format!("http://llm.test:{port}/v1"),
            endpoint_fingerprint: FINGERPRINT,
            model: "manual-model".into(),
            credential: Some(
                CredentialEnvelope::new(
                    "openai_compatible_self_hosted_private".into(),
                    FINGERPRINT,
                    CredentialSecret::new(b"sk-probe".to_vec()).unwrap(),
                )
                .unwrap(),
            ),
        }
    }

    /// 自定义响应延迟的 mock server；返回捕获的请求字节。
    async fn mock_server(
        response: &'static str,
        delay: Duration,
    ) -> (u16, tokio::sync::oneshot::Receiver<String>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sent, received) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            let mut chunk = [0_u8; 8192];
            loop {
                let read = stream.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
                let Some(header_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n")
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
            let body = response;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
        (port, received)
    }

    const OK_COMPLETION: &str =
        "{\"choices\":[{\"message\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}";

    #[tokio::test]
    async fn probe_success_returns_redacted_capability_summary() {
        let (port, received) = mock_server(OK_COMPLETION, Duration::ZERO).await;
        let outcome = service().probe(ACCOUNT, snapshot(port)).await.unwrap();
        assert_eq!(outcome.model, "manual-model");
        // self-hosted 最小方言：无服务端 token cap，必须显式标记 ClientOnly。
        assert_eq!(outcome.token_cap, ProbeTokenCap::ClientOnly);
        assert!(!outcome.structured_output);

        let request = received.await.unwrap();
        assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1"));
        assert!(!request.contains("/models"), "probe must not call /models");
        assert!(request.contains("\"stream\":false"));
        assert!(request.contains("\"model\":\"manual-model\""));
        assert!(!request.contains("response_format"));
        // loopback HTTP 允许携带 credential。
        assert!(request
            .to_lowercase()
            .contains("authorization: bearer sk-probe"));
    }

    #[tokio::test]
    async fn concurrent_probe_returns_busy_and_does_not_block_other_accounts() {
        let (port, received) = mock_server(OK_COMPLETION, Duration::from_millis(400)).await;
        let service = service();
        // 直接持有 flight 模拟在途 probe。
        let held = service.acquire_probe_test_flight(ACCOUNT).unwrap();
        let busy = service.probe(ACCOUNT, snapshot(port)).await.unwrap_err();
        assert!(matches!(busy, ProbeError::Busy));
        // 其他账户不受影响。
        let other = service.probe(OTHER_ACCOUNT, snapshot(port)).await;
        drop(held);
        other.unwrap();
        let _ = received.await;
    }

    #[tokio::test]
    async fn probe_failure_returns_stable_kind_without_upstream_body() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 8192];
            let _ = stream.read(&mut request).await.unwrap();
            let body = "{\"error\":{\"message\":\"invalid key sk-probe leaked\"}}";
            let response = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let error = service().probe(ACCOUNT, snapshot(port)).await.unwrap_err();
        let ProbeError::Reasoning(inner) = error else {
            panic!("expected reasoning error");
        };
        assert_eq!(inner.kind(), crate::reasoning::ReasoningErrorKind::HttpAuth);
        assert!(
            !inner.to_string().contains("sk-probe"),
            "upstream body must never leak into errors: {inner}"
        );
    }

    #[tokio::test]
    async fn probe_with_missing_credential_fails_before_network() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let contacted = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_millis(300), listener.accept())
                .await
                .is_ok()
        });
        let mut provider = snapshot(port);
        provider.credential = None;
        let error = service().probe(ACCOUNT, provider).await.unwrap_err();
        let ProbeError::Reasoning(inner) = error else {
            panic!("expected reasoning error");
        };
        assert_eq!(
            inner.kind(),
            crate::reasoning::ReasoningErrorKind::CredentialMissing
        );
        assert!(
            !contacted.await.unwrap(),
            "missing credential must not open a connection"
        );
    }
}
