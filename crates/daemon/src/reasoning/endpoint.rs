//! Endpoint 地址分类与 DNS 校验（ST-M3.1）。
//!
//! 结构规范化（scheme/userinfo/query/fragment 拒绝、默认端口移除、资源尾缀剥离、
//! bare origin 补 `/v1`）复用 storage 层 `normalize_provider_endpoint`，本模块叠加：
//!
//! - known-native host guard（精确或 dot-suffix，见 registry 模块）；
//! - IPv4/IPv6 地址分类：cloud/public 要求全部解析地址 global-unicast；private 只
//!   允许 loopback / RFC1918 / IPv6 ULA / CGNAT；所有类型拒绝 unspecified、
//!   multicast、documentation、link-local（含 169.254.169.254 等 metadata 地址）、
//!   broadcast 与保留段；
//! - 可注入的异步 DNS 解析：任一解析地址不合规即整体拒绝（fail closed）；
//! - HTTP credential 传输策略：仅全 loopback 的 HTTP 允许携带 credential，其余
//!   私网 HTTP 必须显式无认证。
//!
//! 本模块不设置 deadline；调用方（ReasoningService）把解析与后续请求放进同一个
//! 总 deadline 内。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use async_trait::async_trait;
use seasnail_storage::ProviderType;
use url::Url;

use super::registry::is_known_native_host;
use super::ReasoningError;

/// 单地址分类结果（仅允许进入策略判断的类别）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressClass {
    /// 127.0.0.0/8 或 ::1。
    Loopback,
    /// RFC1918：10/8、172.16/12、192.168/16。
    PrivateLan,
    /// RFC6598 CGNAT 100.64.0.0/10（Tailscale 等覆盖网络）。
    Cgnat,
    /// IPv6 ULA fc00::/7。
    Ula,
    /// 公网 global-unicast。
    GlobalUnicast,
}

/// 一律拒绝的地址类别（错误描述为静态脱敏文本）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressReject {
    /// 0.0.0.0/8、::。
    Unspecified,
    /// 224/4、ff00::/8。
    Multicast,
    /// 192.0.2/24、198.51.100/24、203.0.113/24、2001:db8::/32。
    Documentation,
    /// 169.254/16、fe80::/10（含云 metadata 169.254.169.254）。
    LinkLocal,
    /// 255.255.255.255。
    Broadcast,
    /// 240/4、198.18/15（benchmarking）、192.88.99/24（已废弃 6to4 anycast）、
    /// IPv4-compatible ::/96 残余段。
    Reserved,
}

impl AddressReject {
    fn message(self) -> &'static str {
        match self {
            Self::Unspecified => "endpoint address is unspecified",
            Self::Multicast => "endpoint address is multicast",
            Self::Documentation => "endpoint address is a documentation range",
            Self::LinkLocal => "endpoint address is link-local or metadata",
            Self::Broadcast => "endpoint address is broadcast",
            Self::Reserved => "endpoint address is reserved",
        }
    }
}

/// 对单个 IP 做分类。允许类别与拒绝类别互斥，无默认放行。
pub fn classify_ip(ip: IpAddr) -> Result<AddressClass, AddressReject> {
    match ip {
        IpAddr::V4(v4) => classify_ipv4(v4),
        IpAddr::V6(v6) => classify_ipv6(v6),
    }
}

fn classify_ipv4(ip: Ipv4Addr) -> Result<AddressClass, AddressReject> {
    let [a, b, c, d] = ip.octets();
    // 0.0.0.0/8「本网络」整段不放行（含 0.0.0.0 自身）。
    if a == 0 {
        return Err(AddressReject::Unspecified);
    }
    if ip.is_loopback() {
        return Ok(AddressClass::Loopback);
    }
    if ip.is_private() {
        return Ok(AddressClass::PrivateLan);
    }
    // Alibaba Cloud 等环境会把 metadata 放在 CGNAT 范围内；必须先于 CGNAT
    // 放行规则精确拒绝，不能仅依赖 link-local 169.254/16。
    if [a, b, c, d] == [100, 100, 100, 200] || [a, b, c, d] == [100, 100, 100, 201] {
        return Err(AddressReject::LinkLocal);
    }
    // CGNAT 100.64.0.0/10：std 无判定，手工前缀匹配。
    if a == 100 && (64..=127).contains(&b) {
        return Ok(AddressClass::Cgnat);
    }
    // 169.254.0.0/16：明确不作为可用私网地址（含 169.254.169.254 metadata）。
    if ip.is_link_local() {
        return Err(AddressReject::LinkLocal);
    }
    if ip.is_documentation() {
        return Err(AddressReject::Documentation);
    }
    if ip.is_multicast() {
        return Err(AddressReject::Multicast);
    }
    if ip.is_broadcast() {
        return Err(AddressReject::Broadcast);
    }
    // 剩余 IANA special-purpose 范围一律 fail closed。192.0.0/24 包含协议分配和
    // anycast 例外；这里保守拒绝整段，因为 provider endpoint 不应依赖这些地址。
    if (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 31 && c == 196)
        || (a == 192 && b == 52 && c == 193)
        || (a == 192 && b == 88 && c == 99)
        || (a == 192 && b == 175 && c == 48)
        || (a == 198 && (18..=19).contains(&b))
        || a >= 240
    {
        return Err(AddressReject::Reserved);
    }
    Ok(AddressClass::GlobalUnicast)
}

fn classify_ipv6(ip: Ipv6Addr) -> Result<AddressClass, AddressReject> {
    if ip.is_unspecified() {
        return Err(AddressReject::Unspecified);
    }
    if ip.is_loopback() {
        return Ok(AddressClass::Loopback);
    }
    // IPv4-mapped ::ffff:a.b.c.d 必须按映射后的 IPv4 语义分类，防止绕过。
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return classify_ipv4(mapped);
    }
    if ip.is_unique_local() {
        return Ok(AddressClass::Ula);
    }
    if ip.is_unicast_link_local() {
        return Err(AddressReject::LinkLocal);
    }
    if ip.is_multicast() {
        return Err(AddressReject::Multicast);
    }
    let segments = ip.segments();
    // 2001:db8::/32 documentation。
    if segments[0] == 0x2001 && segments[1] == 0x0db8 {
        return Err(AddressReject::Documentation);
    }
    // ::/96 残余（deprecated IPv4-compatible）：前 96 bit 全 0 且非 ::/::1 的一律拒绝。
    if segments[..6].iter().all(|segment| *segment == 0) {
        return Err(AddressReject::Reserved);
    }
    // 当前可公开路由的 IPv6 global-unicast 属于 2000::/3；其余未明确允许的
    // 单播范围（例如 deprecated site-local fec0::/10、discard-only 100::/64）
    // 不得因“不是 link-local/ULA”而默认放行。
    if segments[0] & 0xe000 != 0x2000 {
        return Err(AddressReject::Reserved);
    }
    // 2001::/23 IETF protocol assignments、2002::/16 deprecated 6to4，以及
    // 3fff::/20 documentation 均不作为 provider 的公网连接目标。
    if (segments[0] == 0x2001 && segments[1] <= 0x01ff)
        || segments[0] == 0x2002
        || segments[0] & 0xfff0 == 0x3ff0
    {
        return Err(AddressReject::Reserved);
    }
    Ok(AddressClass::GlobalUnicast)
}

/// provider 类型对应的网络策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkPolicy {
    /// cloud / public self-hosted：全部地址必须 global-unicast。
    GlobalUnicastOnly,
    /// private self-hosted：全部地址必须是 loopback / RFC1918 / ULA / CGNAT。
    PrivateNetworkOnly,
}

pub fn network_policy(provider_type: ProviderType) -> NetworkPolicy {
    match provider_type {
        ProviderType::OpenAi
        | ProviderType::OpenAiCompatibleCloud
        | ProviderType::SelfHostedPublic => NetworkPolicy::GlobalUnicastOnly,
        ProviderType::SelfHostedPrivate => NetworkPolicy::PrivateNetworkOnly,
    }
}

/// HTTP credential 传输策略（设计 step 7）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialTransport {
    /// HTTPS，或全 loopback 的 HTTP：允许携带 credential。
    Allowed,
    /// 非 loopback 私网 HTTP：必须显式无认证，携带 credential 一律拒绝。
    NoAuthOnly,
}

/// 结构化规范化后的 endpoint（storage 结构校验 + known-native guard 均已通过）。
#[derive(Debug, Clone)]
pub struct CanonicalEndpoint {
    url: Url,
    host: url::Host<String>,
    port: u16,
    https: bool,
    provider_type: ProviderType,
}

impl CanonicalEndpoint {
    pub fn url(&self) -> &Url {
        &self.url
    }

    pub fn host(&self) -> &url::Host<String> {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn is_https(&self) -> bool {
        self.https
    }

    pub fn provider_type(&self) -> ProviderType {
        self.provider_type
    }
}

/// 结构规范化 + known-native guard。DNS/IP 分类在 `resolve_and_validate` 完成。
///
/// 幂等：对已规范化的存储值再次调用结果一致（send 路径可安全重跑）。
pub fn canonicalize(
    provider_type: ProviderType,
    raw_endpoint: &str,
) -> Result<CanonicalEndpoint, ReasoningError> {
    let normalized = seasnail_storage::normalize_provider_endpoint(provider_type, raw_endpoint)
        .map_err(|_| ReasoningError::endpoint_rejected("endpoint rejected by structural policy"))?;
    let url = Url::parse(&normalized)
        .map_err(|_| ReasoningError::endpoint_rejected("endpoint is not a valid URL"))?;
    let host = url
        .host()
        .ok_or_else(|| ReasoningError::endpoint_rejected("endpoint has no host"))?
        .to_owned();
    if let url::Host::Domain(name) = &host {
        if is_known_native_host(name) {
            return Err(ReasoningError::endpoint_rejected(
                "endpoint is a known native-only provider",
            ));
        }
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| ReasoningError::endpoint_rejected("endpoint has no usable port"))?;
    let https = url.scheme() == "https";
    Ok(CanonicalEndpoint {
        url,
        host,
        port,
        https,
        provider_type,
    })
}

/// 可注入的 DNS 解析器。正式实现走系统 getaddrinfo；测试用 mock 固定答案。
#[async_trait]
pub trait DnsResolver: Send + Sync {
    async fn lookup(&self, host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>>;
}

/// 系统 DNS（tokio `lookup_host`）。调用方负责把它放进总 deadline。
pub struct SystemResolver;

#[async_trait]
impl DnsResolver for SystemResolver {
    async fn lookup(&self, host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
        let answers = tokio::net::lookup_host((host, port)).await?;
        Ok(answers.collect())
    }
}

/// 通过全部校验、可直接 pin 连接的 endpoint。
#[derive(Debug, Clone)]
pub struct ValidatedEndpoint {
    pub canonical: CanonicalEndpoint,
    /// 本次解析/字面量得到的全部可连接地址（含端口）；client 只能连接这些地址。
    pub addresses: Vec<SocketAddr>,
    pub credential_transport: CredentialTransport,
}

impl ValidatedEndpoint {
    /// domain 形式的 host 字符串（用于 client 的 resolve pin）；字面量 IP 返回 None。
    pub fn domain(&self) -> Option<&str> {
        match &self.canonical.host {
            url::Host::Domain(name) => Some(name.as_str()),
            _ => None,
        }
    }

    /// 测试构造：绕过分类，直接指向给定地址（仅本 crate 单测使用）。
    #[cfg(test)]
    pub(crate) fn for_test(
        domain: Option<&str>,
        address: SocketAddr,
        https: bool,
        provider_type: ProviderType,
    ) -> Self {
        let (url, host) = match domain {
            Some(domain) => {
                let scheme = if https { "https" } else { "http" };
                let url =
                    Url::parse(&format!("{scheme}://{domain}:{}/v1", address.port())).unwrap();
                (url, url::Host::Domain(domain.to_owned()))
            }
            None => {
                let scheme = if https { "https" } else { "http" };
                let url = Url::parse(&format!("{scheme}://{address}/v1")).unwrap();
                (url.clone(), url.host().unwrap().to_owned())
            }
        };
        Self {
            canonical: CanonicalEndpoint {
                url,
                host,
                port: address.port(),
                https,
                provider_type,
            },
            addresses: vec![address],
            credential_transport: CredentialTransport::Allowed,
        }
    }
}

/// 解析并分类全部地址；任一不合规即整体拒绝。字面量 IP 不经 DNS。
pub async fn resolve_and_validate(
    resolver: &dyn DnsResolver,
    canonical: &CanonicalEndpoint,
) -> Result<ValidatedEndpoint, ReasoningError> {
    let ips: Vec<IpAddr> = match &canonical.host {
        url::Host::Ipv4(ip) => vec![IpAddr::V4(*ip)],
        url::Host::Ipv6(ip) => vec![IpAddr::V6(*ip)],
        url::Host::Domain(name) => {
            let answers = resolver
                .lookup(name, canonical.port)
                .await
                .map_err(|_| ReasoningError::dns("DNS resolution failed"))?;
            answers.into_iter().map(|address| address.ip()).collect()
        }
    };
    let mut ips = ips;
    ips.sort_unstable();
    ips.dedup();
    let classes = validate_ips(network_policy(canonical.provider_type), &ips)?;
    let credential_transport = if canonical.https {
        CredentialTransport::Allowed
    } else if classes.iter().all(|class| *class == AddressClass::Loopback) {
        CredentialTransport::Allowed
    } else {
        // 私网 HTTP 且含非 loopback 地址：只允许显式无认证。
        CredentialTransport::NoAuthOnly
    };
    let addresses = ips
        .into_iter()
        .map(|ip| SocketAddr::new(ip, canonical.port))
        .collect();
    Ok(ValidatedEndpoint {
        canonical: canonical.clone(),
        addresses,
        credential_transport,
    })
}

/// 地址集合策略校验：空集合拒绝；任一地址不合规即整体拒绝。
fn validate_ips(
    policy: NetworkPolicy,
    ips: &[IpAddr],
) -> Result<Vec<AddressClass>, ReasoningError> {
    if ips.is_empty() {
        return Err(ReasoningError::endpoint_rejected(
            "endpoint resolved to no addresses",
        ));
    }
    let mut classes = Vec::with_capacity(ips.len());
    for ip in ips {
        let class = classify_ip(*ip)
            .map_err(|reject| ReasoningError::endpoint_rejected(reject.message()))?;
        let allowed = match policy {
            NetworkPolicy::GlobalUnicastOnly => class == AddressClass::GlobalUnicast,
            NetworkPolicy::PrivateNetworkOnly => matches!(
                class,
                AddressClass::Loopback
                    | AddressClass::PrivateLan
                    | AddressClass::Cgnat
                    | AddressClass::Ula
            ),
        };
        if !allowed {
            return Err(ReasoningError::endpoint_rejected(
                "endpoint address failed the provider network policy",
            ));
        }
        classes.push(class);
    }
    Ok(classes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    #[test]
    fn ipv4_classification_matrix() {
        // 私网允许类别。
        assert_eq!(classify_ip(v4(127, 0, 0, 1)), Ok(AddressClass::Loopback));
        assert_eq!(classify_ip(v4(127, 255, 0, 9)), Ok(AddressClass::Loopback));
        assert_eq!(classify_ip(v4(10, 0, 0, 8)), Ok(AddressClass::PrivateLan));
        assert_eq!(classify_ip(v4(172, 16, 0, 1)), Ok(AddressClass::PrivateLan));
        assert_eq!(
            classify_ip(v4(172, 31, 255, 255)),
            Ok(AddressClass::PrivateLan)
        );
        assert_eq!(
            classify_ip(v4(192, 168, 1, 10)),
            Ok(AddressClass::PrivateLan)
        );
        assert_eq!(classify_ip(v4(100, 64, 0, 1)), Ok(AddressClass::Cgnat));
        assert_eq!(classify_ip(v4(100, 127, 255, 254)), Ok(AddressClass::Cgnat));
        // 公网。
        assert_eq!(classify_ip(v4(8, 8, 8, 8)), Ok(AddressClass::GlobalUnicast));
        assert_eq!(classify_ip(v4(1, 1, 1, 1)), Ok(AddressClass::GlobalUnicast));
        // 拒绝类别。
        assert_eq!(classify_ip(v4(0, 0, 0, 0)), Err(AddressReject::Unspecified));
        assert_eq!(classify_ip(v4(0, 1, 2, 3)), Err(AddressReject::Unspecified));
        assert_eq!(
            classify_ip(v4(169, 254, 0, 1)),
            Err(AddressReject::LinkLocal)
        );
        assert_eq!(
            classify_ip(v4(169, 254, 169, 254)),
            Err(AddressReject::LinkLocal),
            "cloud metadata address must be rejected"
        );
        assert_eq!(
            classify_ip(v4(192, 0, 2, 1)),
            Err(AddressReject::Documentation)
        );
        assert_eq!(
            classify_ip(v4(198, 51, 100, 7)),
            Err(AddressReject::Documentation)
        );
        assert_eq!(
            classify_ip(v4(203, 0, 113, 9)),
            Err(AddressReject::Documentation)
        );
        assert_eq!(classify_ip(v4(224, 0, 0, 1)), Err(AddressReject::Multicast));
        assert_eq!(
            classify_ip(v4(255, 255, 255, 255)),
            Err(AddressReject::Broadcast)
        );
        assert_eq!(classify_ip(v4(240, 0, 0, 1)), Err(AddressReject::Reserved));
        assert_eq!(classify_ip(v4(198, 18, 0, 1)), Err(AddressReject::Reserved));
        assert_eq!(
            classify_ip(v4(198, 19, 255, 9)),
            Err(AddressReject::Reserved)
        );
        assert_eq!(
            classify_ip(v4(192, 88, 99, 1)),
            Err(AddressReject::Reserved)
        );
        assert_eq!(classify_ip(v4(192, 0, 0, 8)), Err(AddressReject::Reserved));
        assert_eq!(
            classify_ip(v4(100, 100, 100, 200)),
            Err(AddressReject::LinkLocal),
            "metadata addresses inside CGNAT must not inherit the private allow rule"
        );
        // CGNAT 上界之外回到 global。
        assert_eq!(
            classify_ip(v4(100, 128, 0, 1)),
            Ok(AddressClass::GlobalUnicast)
        );
        // 172.15 / 172.32 不在 RFC1918。
        assert_eq!(
            classify_ip(v4(172, 15, 0, 1)),
            Ok(AddressClass::GlobalUnicast)
        );
        assert_eq!(
            classify_ip(v4(172, 32, 0, 1)),
            Ok(AddressClass::GlobalUnicast)
        );
    }

    #[test]
    fn ipv6_classification_matrix() {
        let classify = |text: &str| classify_ip(text.parse::<IpAddr>().unwrap());
        assert_eq!(classify("::1"), Ok(AddressClass::Loopback));
        assert_eq!(classify("fc00::1"), Ok(AddressClass::Ula));
        assert_eq!(classify("fdff:1234::1"), Ok(AddressClass::Ula));
        assert_eq!(
            classify("2606:4700:4700::1111"),
            Ok(AddressClass::GlobalUnicast)
        );
        assert_eq!(classify("::"), Err(AddressReject::Unspecified));
        assert_eq!(classify("fe80::1"), Err(AddressReject::LinkLocal));
        assert_eq!(classify("febf::ffff"), Err(AddressReject::LinkLocal));
        assert_eq!(classify("ff02::1"), Err(AddressReject::Multicast));
        assert_eq!(classify("2001:db8::1"), Err(AddressReject::Documentation));
        assert_eq!(classify("fec0::1"), Err(AddressReject::Reserved));
        assert_eq!(classify("100::1"), Err(AddressReject::Reserved));
        assert_eq!(classify("2001::1"), Err(AddressReject::Reserved));
        assert_eq!(classify("2002:c000:0204::1"), Err(AddressReject::Reserved));
        assert_eq!(classify("3fff::1"), Err(AddressReject::Reserved));
        // IPv4-mapped 按映射后分类。
        assert_eq!(classify("::ffff:127.0.0.1"), Ok(AddressClass::Loopback));
        assert_eq!(classify("::ffff:10.0.0.1"), Ok(AddressClass::PrivateLan));
        assert_eq!(classify("::ffff:8.8.8.8"), Ok(AddressClass::GlobalUnicast));
        assert_eq!(
            classify("::ffff:169.254.169.254"),
            Err(AddressReject::LinkLocal)
        );
        // deprecated IPv4-compatible ::/96 残余拒绝。
        assert_eq!(classify("::127.0.0.1"), Err(AddressReject::Reserved));
        assert_eq!(classify("::8.8.8.8"), Err(AddressReject::Reserved));
    }

    #[test]
    fn policy_validation_rejects_empty_mixed_and_out_of_scope() {
        let private = NetworkPolicy::PrivateNetworkOnly;
        let public = NetworkPolicy::GlobalUnicastOnly;

        assert!(validate_ips(private, &[]).is_err());
        assert!(validate_ips(public, &[]).is_err());

        // 私网策略允许 loopback/RFC1918/CGNAT/ULA，拒绝公网与 link-local。
        assert!(validate_ips(private, &[v4(127, 0, 0, 1), v4(10, 0, 0, 1)]).is_ok());
        assert!(validate_ips(private, &[v4(100, 64, 0, 1)]).is_ok());
        assert!(validate_ips(private, &[v4(8, 8, 8, 8)]).is_err());
        assert!(validate_ips(private, &[v4(169, 254, 1, 1)]).is_err());
        // 混合地址：任一不合规整体拒绝。
        assert!(validate_ips(private, &[v4(127, 0, 0, 1), v4(8, 8, 8, 8)]).is_err());

        // 公网策略只允许 global-unicast；CGNAT/RFC1918 都不算。
        assert!(validate_ips(public, &[v4(8, 8, 8, 8), v4(1, 1, 1, 1)]).is_ok());
        assert!(validate_ips(public, &[v4(10, 0, 0, 1)]).is_err());
        assert!(validate_ips(public, &[v4(100, 64, 0, 1)]).is_err());
        assert!(validate_ips(public, &[v4(8, 8, 8, 8), v4(192, 168, 0, 1)]).is_err());
    }

    #[test]
    fn canonicalize_applies_structural_policy_and_native_guard() {
        // 结构策略复用 storage：cloud/public 必须 HTTPS。
        assert!(canonicalize(
            ProviderType::OpenAiCompatibleCloud,
            "http://llm.example.com"
        )
        .is_err());
        assert!(canonicalize(ProviderType::SelfHostedPublic, "http://10.0.0.2:8443").is_err());
        // userinfo/query/fragment 拒绝。
        assert!(canonicalize(ProviderType::SelfHostedPrivate, "https://user@10.0.0.2").is_err());
        assert!(canonicalize(ProviderType::SelfHostedPrivate, "http://10.0.0.2?a=b").is_err());
        assert!(canonicalize(ProviderType::SelfHostedPrivate, "http://10.0.0.2#frag").is_err());
        // 默认端口移除与 bare origin 补 /v1。
        let canonical =
            canonicalize(ProviderType::SelfHostedPrivate, "http://10.0.0.2:80/").unwrap();
        assert_eq!(canonical.url().as_str(), "http://10.0.0.2/v1");
        assert_eq!(canonical.port(), 80);
        assert!(!canonical.is_https());
        // 资源尾缀剥离。
        let canonical = canonicalize(
            ProviderType::SelfHostedPrivate,
            "https://10.0.0.2:8443/v1/chat/completions",
        )
        .unwrap();
        assert_eq!(canonical.url().as_str(), "https://10.0.0.2:8443/v1");
        assert_eq!(canonical.port(), 8443);
        assert!(canonical.is_https());
        // known-native：精确与 dot-suffix 拒绝，相似域名放行。
        assert!(canonicalize(
            ProviderType::OpenAiCompatibleCloud,
            "https://api.anthropic.com/v1"
        )
        .is_err());
        assert!(canonicalize(
            ProviderType::SelfHostedPrivate,
            "https://sub.generativelanguage.googleapis.com/v1"
        )
        .is_err());
        assert!(canonicalize(
            ProviderType::OpenAiCompatibleCloud,
            "https://api.anthropic.com.evil.example/v1"
        )
        .is_ok());
        // OpenAI 固定 registry host。
        assert!(canonicalize(ProviderType::OpenAi, "https://api.openai.com/v1").is_ok());
        assert!(canonicalize(ProviderType::OpenAi, "https://not-openai.example/v1").is_err());
        // 字面量 IP。
        let canonical = canonicalize(ProviderType::SelfHostedPrivate, "http://[::1]:9000").unwrap();
        assert_eq!(canonical.port(), 9000);
    }

    struct MockResolver {
        answers: Vec<SocketAddr>,
        calls: Arc<AtomicUsize>,
    }

    impl MockResolver {
        fn new(
            answers: impl IntoIterator<Item = IpAddr>,
            port: u16,
            calls: Arc<AtomicUsize>,
        ) -> Self {
            Self {
                answers: answers
                    .into_iter()
                    .map(|ip| SocketAddr::new(ip, port))
                    .collect(),
                calls,
            }
        }
    }

    #[async_trait]
    impl DnsResolver for MockResolver {
        async fn lookup(&self, _host: &str, _port: u16) -> std::io::Result<Vec<SocketAddr>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.answers.clone())
        }
    }

    #[tokio::test]
    async fn resolve_classifies_domain_answers_and_sets_credential_transport() {
        let calls = Arc::new(AtomicUsize::new(0));
        // 私网 HTTPS：loopback + RFC1918 混合答案全部合规。
        let canonical =
            canonicalize(ProviderType::SelfHostedPrivate, "https://llm.lan:8443").unwrap();
        let resolver = MockResolver::new(
            [v4(127, 0, 0, 1), v4(10, 0, 0, 2)],
            8443,
            Arc::clone(&calls),
        );
        let validated = resolve_and_validate(&resolver, &canonical).await.unwrap();
        assert_eq!(validated.addresses.len(), 2);
        assert_eq!(validated.credential_transport, CredentialTransport::Allowed);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // 私网 HTTP 且含非 loopback：只允许无认证。
        let canonical =
            canonicalize(ProviderType::SelfHostedPrivate, "http://llm.lan:8080").unwrap();
        let resolver = MockResolver::new([v4(10, 0, 0, 2)], 8080, Arc::clone(&calls));
        let validated = resolve_and_validate(&resolver, &canonical).await.unwrap();
        assert_eq!(
            validated.credential_transport,
            CredentialTransport::NoAuthOnly
        );

        // 私网 HTTP 全 loopback：允许携带 credential。
        let resolver = MockResolver::new([v4(127, 0, 0, 1)], 8080, Arc::clone(&calls));
        let validated = resolve_and_validate(&resolver, &canonical).await.unwrap();
        assert_eq!(validated.credential_transport, CredentialTransport::Allowed);

        // 公网 cloud：解析到私网地址即拒绝（rebinding 防护在分类层）。
        let canonical = canonicalize(
            ProviderType::OpenAiCompatibleCloud,
            "https://api.llm.example",
        )
        .unwrap();
        let resolver = MockResolver::new([v4(10, 0, 0, 2)], 443, Arc::clone(&calls));
        let error = resolve_and_validate(&resolver, &canonical)
            .await
            .unwrap_err();
        assert_eq!(
            error.kind(),
            super::super::ReasoningErrorKind::EndpointRejected
        );
    }

    #[tokio::test]
    async fn literal_ip_skips_dns() {
        let calls = Arc::new(AtomicUsize::new(0));
        let canonical =
            canonicalize(ProviderType::SelfHostedPrivate, "http://127.0.0.1:8080").unwrap();
        let resolver = MockResolver::new([], 8080, Arc::clone(&calls));
        let validated = resolve_and_validate(&resolver, &canonical).await.unwrap();
        assert_eq!(validated.domain(), None);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "literal IP must not hit DNS"
        );
        assert_eq!(validated.credential_transport, CredentialTransport::Allowed);
    }

    #[tokio::test]
    async fn localhost_resolves_via_system_dns() {
        // getaddrinfo("localhost") 不依赖外部网络；通常同时给出 127.0.0.1 与 ::1。
        let canonical =
            canonicalize(ProviderType::SelfHostedPrivate, "http://localhost:8080").unwrap();
        let validated = resolve_and_validate(&SystemResolver, &canonical)
            .await
            .unwrap();
        assert!(!validated.addresses.is_empty());
        assert_eq!(validated.credential_transport, CredentialTransport::Allowed);
        // 公网类型下 localhost 必须被拒绝。
        let canonical =
            canonicalize(ProviderType::OpenAiCompatibleCloud, "https://localhost").unwrap();
        assert!(resolve_and_validate(&SystemResolver, &canonical)
            .await
            .is_err());
    }
}
