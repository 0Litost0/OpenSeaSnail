//! Credential 绑定校验与传输策略（ST-M3.2）。
//!
//! 两段式校验，保证任何失败都发生在网络之前：
//!
//! 1. `verify_binding` 静态校验：不触碰 DNS/连接。envelope 的 provider_type 与
//!    endpoint fingerprint 必须与 config snapshot 逐字段相等；修改 endpoint 或
//!    provider type 后旧 credential 进入 stale，发请求时按缺失快速失败；
//! 2. `enforce_transport` 在 endpoint 解析分类之后、请求发送之前执行：非
//!    loopback 私网 HTTP 拒绝携带 credential（设计 step 7 与「横切关注点」）。
//!
//! Authorization header 只在全部校验通过后构造；`CredentialSecret` 构造期已保证
//! header-safe。构造时只借用源 secret，临时拼接 buffer 自动清零，header 标记为
//! sensitive，避免通用 Debug/trace 意外展开其值。

use reqwest::header::HeaderValue;
use seasnail_crypto::CredentialSecret;
use zeroize::Zeroizing;

use super::endpoint::{CredentialTransport, ValidatedEndpoint};
use super::{ProviderSnapshot, ReasoningError};

/// 绑定校验通过后本次请求实际使用的认证形态（零拷贝借用 snapshot 内 secret）。
pub enum CredentialMode<'a> {
    Bearer(&'a CredentialSecret),
    NoAuth,
}

// 手工 Debug：只暴露认证形态，绝不接触 secret 字节。
impl std::fmt::Debug for CredentialMode<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bearer(_) => f.write_str("CredentialMode::Bearer(..)"),
            Self::NoAuth => f.write_str("CredentialMode::NoAuth"),
        }
    }
}

/// 静态绑定校验：零网络。envelope 缺失、provider_type/fingerprint 失配或 cloud
/// 被标 no-auth，统一按 credential_missing 快速失败。
pub fn verify_binding(snapshot: &ProviderSnapshot) -> Result<(), ReasoningError> {
    match &snapshot.credential {
        None => Err(ReasoningError::credential_missing(
            "provider credential is not configured",
        )),
        Some(envelope) => {
            if envelope.provider_type() != snapshot.provider_type.as_str()
                || envelope.endpoint_fingerprint() != &snapshot.endpoint_fingerprint
            {
                return Err(ReasoningError::credential_missing(
                    "provider credential binding is stale",
                ));
            }
            if envelope.is_no_auth() && snapshot.provider_type.requires_credential() {
                return Err(ReasoningError::credential_missing(
                    "cloud provider requires a credential",
                ));
            }
            Ok(())
        }
    }
}

/// 传输策略：endpoint 分类后的最后一道 credential 闸门。
pub fn enforce_transport<'a>(
    snapshot: &'a ProviderSnapshot,
    endpoint: &ValidatedEndpoint,
) -> Result<CredentialMode<'a>, ReasoningError> {
    let envelope = snapshot.credential.as_ref().ok_or_else(|| {
        ReasoningError::credential_missing("provider credential is not configured")
    })?;
    match envelope.secret() {
        Some(_) if endpoint.credential_transport == CredentialTransport::NoAuthOnly => Err(
            ReasoningError::endpoint_rejected("credential is forbidden over non-loopback HTTP"),
        ),
        Some(secret) => Ok(CredentialMode::Bearer(secret)),
        None => Ok(CredentialMode::NoAuth),
    }
}

/// 构造 `Bearer <secret>` header 值。`CredentialSecret::new` 已校验 header-safe。
pub fn authorization_header(secret: &CredentialSecret) -> HeaderValue {
    let mut value = Zeroizing::new(b"Bearer ".to_vec());
    value.extend_from_slice(secret.expose_secret());
    let mut header =
        HeaderValue::from_bytes(&value).expect("credential secret is header-safe by construction");
    header.set_sensitive(true);
    header
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reasoning::endpoint::{self, DnsResolver};
    use async_trait::async_trait;
    use seasnail_crypto::CredentialEnvelope;
    use seasnail_storage::ProviderType;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    fn snapshot(
        provider_type: ProviderType,
        endpoint: &str,
        fingerprint: [u8; 32],
        credential: Option<CredentialEnvelope>,
    ) -> ProviderSnapshot {
        ProviderSnapshot {
            config_id: "00000000-0000-4000-8000-000000000001".into(),
            provider_type,
            endpoint: endpoint.into(),
            endpoint_fingerprint: fingerprint,
            model: "model-a".into(),
            credential,
        }
    }

    fn envelope(
        provider_type: &str,
        fingerprint: [u8; 32],
        secret: Option<&[u8]>,
    ) -> CredentialEnvelope {
        match secret {
            Some(secret) => CredentialEnvelope::new(
                provider_type.into(),
                fingerprint,
                CredentialSecret::new(secret.to_vec()).unwrap(),
            )
            .unwrap(),
            None => CredentialEnvelope::no_auth(provider_type.into(), fingerprint).unwrap(),
        }
    }

    #[test]
    fn binding_requires_matching_envelope_before_any_network() {
        let credential = envelope("openai", [7; 32], Some(b"sk-test"));
        // 缺失：cloud 与 self-hosted 一致按 missing 快速失败（self-hosted 无认证须显式选择）。
        for provider_type in [ProviderType::OpenAi, ProviderType::SelfHostedPrivate] {
            let snapshot = snapshot(provider_type, "https://example.invalid/v1", [7; 32], None);
            assert_eq!(
                verify_binding(&snapshot).unwrap_err().kind(),
                super::super::ReasoningErrorKind::CredentialMissing
            );
        }
        // provider_type 失配。
        let wrong_type = envelope("openai_compatible_cloud", [7; 32], Some(b"sk-test"));
        let snap = snapshot(
            ProviderType::OpenAi,
            "https://api.openai.com/v1",
            [7; 32],
            Some(wrong_type),
        );
        assert_eq!(
            verify_binding(&snap).unwrap_err().kind(),
            super::super::ReasoningErrorKind::CredentialMissing
        );
        // fingerprint 失配（endpoint 被修改后的 stale）。
        let snap = snapshot(
            ProviderType::OpenAi,
            "https://api.openai.com/v1",
            [9; 32],
            Some(credential),
        );
        assert_eq!(
            verify_binding(&snap).unwrap_err().kind(),
            super::super::ReasoningErrorKind::CredentialMissing
        );
        // cloud 被标 no-auth。
        let no_auth = envelope("openai", [7; 32], None);
        let snap = snapshot(
            ProviderType::OpenAi,
            "https://api.openai.com/v1",
            [7; 32],
            Some(no_auth),
        );
        assert_eq!(
            verify_binding(&snap).unwrap_err().kind(),
            super::super::ReasoningErrorKind::CredentialMissing
        );
        // 合法绑定通过。
        let credential = envelope("openai", [7; 32], Some(b"sk-test"));
        let snap = snapshot(
            ProviderType::OpenAi,
            "https://api.openai.com/v1",
            [7; 32],
            Some(credential),
        );
        verify_binding(&snap).unwrap();
        // self-hosted 显式无认证通过。
        let no_auth = envelope("openai_compatible_self_hosted_private", [3; 32], None);
        let snap = snapshot(
            ProviderType::SelfHostedPrivate,
            "http://127.0.0.1:8080/v1",
            [3; 32],
            Some(no_auth),
        );
        verify_binding(&snap).unwrap();
    }

    struct FixedResolver(Vec<SocketAddr>);

    #[async_trait]
    impl DnsResolver for FixedResolver {
        async fn lookup(&self, _host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
            Ok(self
                .0
                .iter()
                .map(|address| SocketAddr::new(address.ip(), port))
                .collect())
        }
    }

    async fn validated(
        provider_type: ProviderType,
        endpoint: &str,
        answers: &[u8; 4],
    ) -> ValidatedEndpoint {
        let canonical = endpoint::canonicalize(provider_type, endpoint).unwrap();
        let resolver = FixedResolver(vec![SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(
                answers[0], answers[1], answers[2], answers[3],
            )),
            0,
        )]);
        endpoint::resolve_and_validate(&resolver, &canonical)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn transport_policy_gates_credential_over_http() {
        // HTTPS 公网：携带 credential 允许。
        let endpoint = validated(
            ProviderType::OpenAiCompatibleCloud,
            "https://api.llm.example/v1",
            &[8, 8, 8, 8],
        )
        .await;
        let snap = snapshot(
            ProviderType::OpenAiCompatibleCloud,
            "https://api.llm.example/v1",
            [5; 32],
            Some(envelope("openai_compatible_cloud", [5; 32], Some(b"key"))),
        );
        verify_binding(&snap).unwrap();
        assert!(matches!(
            enforce_transport(&snap, &endpoint).unwrap(),
            CredentialMode::Bearer(_)
        ));

        // loopback HTTP：允许携带 credential。
        let endpoint = validated(
            ProviderType::SelfHostedPrivate,
            "http://127.0.0.1:11434/v1",
            &[127, 0, 0, 1],
        )
        .await;
        let snap = snapshot(
            ProviderType::SelfHostedPrivate,
            "http://127.0.0.1:11434/v1",
            [6; 32],
            Some(envelope(
                "openai_compatible_self_hosted_private",
                [6; 32],
                Some(b"key"),
            )),
        );
        verify_binding(&snap).unwrap();
        assert!(matches!(
            enforce_transport(&snap, &endpoint).unwrap(),
            CredentialMode::Bearer(_)
        ));

        // 非 loopback 私网 HTTP（RFC1918）：携带 credential 一律拒绝；无认证放行。
        let endpoint = validated(
            ProviderType::SelfHostedPrivate,
            "http://10.0.0.2:8080/v1",
            &[10, 0, 0, 2],
        )
        .await;
        let with_key = snapshot(
            ProviderType::SelfHostedPrivate,
            "http://10.0.0.2:8080/v1",
            [8; 32],
            Some(envelope(
                "openai_compatible_self_hosted_private",
                [8; 32],
                Some(b"key"),
            )),
        );
        verify_binding(&with_key).unwrap();
        assert_eq!(
            enforce_transport(&with_key, &endpoint).unwrap_err().kind(),
            super::super::ReasoningErrorKind::EndpointRejected
        );
        let no_auth = snapshot(
            ProviderType::SelfHostedPrivate,
            "http://10.0.0.2:8080/v1",
            [8; 32],
            Some(envelope(
                "openai_compatible_self_hosted_private",
                [8; 32],
                None,
            )),
        );
        verify_binding(&no_auth).unwrap();
        assert!(matches!(
            enforce_transport(&no_auth, &endpoint).unwrap(),
            CredentialMode::NoAuth
        ));
    }

    #[test]
    fn authorization_header_is_bearer_prefixed_secret() {
        let secret = CredentialSecret::new(b"sk-live-123".to_vec()).unwrap();
        let header = authorization_header(&secret);
        assert_eq!(header.to_str().unwrap(), "Bearer sk-live-123");
        assert!(header.is_sensitive());
    }
}
