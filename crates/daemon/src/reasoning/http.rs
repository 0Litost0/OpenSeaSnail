//! Hardened HTTP client 工厂（ST-M3.3）：把 M0.2 spike 验证过的 reqwest 机制产品化。
//!
//! - 每个请求构建独立 client：domain 经 `resolve_to_addrs` pin 到本次校验后的地址
//!   集合，URL authority 不变，HTTP Host / TLS SNI 保持规范化 hostname；字面量 IP
//!   无需 pin（reqwest 直连 authority）；
//! - `no_proxy` 清除显式与环境/系统 proxy（system-proxy feature 已随
//!   default-features=false 关闭，双保险）；`redirect(Policy::none())` 不跟随跳转；
//! - rustls + webpki 公开根证书集（`rustls-tls` feature），证书校验不可关闭，
//!   不支持自签名/用户导入 CA；
//! - connect timeout 固定 3 秒；总 deadline 不在此设置，由调用方外层
//!   `tokio::time::timeout` 统一覆盖 DNS、connect、TLS、header、body 全阶段；
//! - 响应 body 受限读取，超限立即中止，不消耗剩余字节。

use std::time::Duration;

use reqwest::Client;

use super::endpoint::ValidatedEndpoint;
use super::ReasoningError;

/// 连接超时（设计：3 秒）。只覆盖 connect 阶段，整体时限由调用方 deadline 控制。
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// provider HTTP response body 上限（设计防御性容量表：256 KiB）。
pub const MAX_RESPONSE_BODY_BYTES: usize = 256 * 1024;

pub struct HardenedClientFactory;

impl HardenedClientFactory {
    /// 为单个请求构建 pinned client。无法建立 pin（空地址集合）时 fail closed。
    pub fn build(endpoint: &ValidatedEndpoint) -> Result<Client, ReasoningError> {
        Self::build_with_connect_timeout(endpoint, CONNECT_TIMEOUT)
    }

    /// 测试可缩短 connect timeout；生产路径固定使用 [`CONNECT_TIMEOUT`]。
    pub(crate) fn build_with_connect_timeout(
        endpoint: &ValidatedEndpoint,
        connect_timeout: Duration,
    ) -> Result<Client, ReasoningError> {
        if endpoint.addresses.is_empty() {
            return Err(ReasoningError::endpoint_rejected(
                "endpoint has no pinned addresses",
            ));
        }
        let mut builder = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(connect_timeout)
            .use_rustls_tls();
        if let Some(domain) = endpoint.domain() {
            builder = builder.resolve_to_addrs(domain, &endpoint.addresses);
        }
        builder
            .build()
            .map_err(|_| ReasoningError::configuration("failed to build hardened HTTP client"))
    }
}

/// 受限响应读取：Content-Length 超限快速失败；流式累计超过 `max` 立即中止。
pub async fn read_limited_body(
    mut response: reqwest::Response,
    max: usize,
) -> Result<Vec<u8>, ReasoningError> {
    read_limited_body_with_observed(&mut response, max, |_| {})
        .await
        .map_err(|(error, _)| error)
}

/// 与 [`read_limited_body`] 相同，但错误时同时返回已经观察到的响应字节数，供诊断使用。
pub(crate) async fn read_limited_body_with_observed(
    response: &mut reqwest::Response,
    max: usize,
    mut on_observed: impl FnMut(u64),
) -> Result<Vec<u8>, (ReasoningError, u64)> {
    if let Some(length) = response.content_length() {
        if length > max as u64 {
            return Err((
                ReasoningError::response_too_large("provider response exceeds the body limit"),
                0,
            ));
        }
    }
    let mut body = Vec::new();
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(_) => {
                return Err((
                    ReasoningError::connect("failed to read provider response"),
                    body.len() as u64,
                ))
            }
        };
        let observed = (body.len() + chunk.len()) as u64;
        on_observed(observed);
        if body.len() + chunk.len() > max {
            return Err((
                ReasoningError::response_too_large("provider response exceeds the body limit"),
                observed,
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reasoning::endpoint::ValidatedEndpoint;
    use seasnail_storage::ProviderType;
    use std::net::{Ipv4Addr, SocketAddr};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    const PROXY_CHILD: &str = "SEASNAIL_M33_PROXY_CHILD";
    const PROXY_DESTINATION: &str = "SEASNAIL_M33_PROXY_DESTINATION";

    /// 直接拼一个指向 loopback mock 的 validated endpoint（绕过分类，专测 client 行为）。
    fn loopback_endpoint(
        domain: Option<&str>,
        address: SocketAddr,
        https: bool,
    ) -> ValidatedEndpoint {
        ValidatedEndpoint::for_test(domain, address, https, ProviderType::SelfHostedPrivate)
    }

    async fn http_server_once(response: &'static [u8]) -> (SocketAddr, oneshot::Receiver<Vec<u8>>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sent, received) = oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 8192];
            let read = stream.read(&mut request).await.unwrap();
            request.truncate(read);
            let _ = sent.send(request);
            stream.write_all(response).await.unwrap();
        });
        (address, received)
    }

    #[tokio::test]
    async fn pinned_domain_connects_to_approved_address_and_preserves_host() {
        let (address, request) = http_server_once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
        )
        .await;
        let endpoint = loopback_endpoint(Some("llm.invalid"), address, false);
        let client = HardenedClientFactory::build(&endpoint).unwrap();
        let response = client
            .get(format!("http://llm.invalid:{}/probe", address.port()))
            .send()
            .await
            .unwrap();
        let body = read_limited_body(response, MAX_RESPONSE_BODY_BYTES)
            .await
            .unwrap();
        assert_eq!(body, b"ok");
        let request = String::from_utf8(request.await.unwrap()).unwrap();
        assert!(request
            .to_ascii_lowercase()
            .contains(&format!("host: llm.invalid:{}\r\n", address.port())));
    }

    #[tokio::test]
    async fn tls_client_hello_keeps_original_hostname_in_sni() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let capture = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut hello = vec![0_u8; 8192];
            let read = stream.read(&mut hello).await.unwrap();
            hello.truncate(read);
            hello
        });
        let endpoint = loopback_endpoint(Some("llm.invalid"), address, true);
        let client = HardenedClientFactory::build(&endpoint).unwrap();
        let result = client
            .get(format!("https://llm.invalid:{}/probe", address.port()))
            .send()
            .await;
        assert!(result.is_err(), "the capture listener is not a TLS server");
        let hello = capture.await.unwrap();
        assert!(hello
            .windows(b"llm.invalid".len())
            .any(|window| window == b"llm.invalid"));
    }

    #[tokio::test]
    async fn environment_proxy_variables_are_ignored_in_isolated_child() {
        if std::env::var_os(PROXY_CHILD).is_some() {
            let destination: SocketAddr =
                std::env::var(PROXY_DESTINATION).unwrap().parse().unwrap();
            let endpoint = loopback_endpoint(Some("llm.invalid"), destination, false);
            let client = HardenedClientFactory::build(&endpoint).unwrap();
            let response = client
                .get(format!("http://llm.invalid:{}/probe", destination.port()))
                .send()
                .await
                .unwrap();
            let body = read_limited_body(response, MAX_RESPONSE_BODY_BYTES)
                .await
                .unwrap();
            assert_eq!(body, b"env-proxy-ok");
            return;
        }

        let (destination, destination_request) = http_server_once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\nenv-proxy-ok",
        )
        .await;
        let poison_proxy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let poison_address = poison_proxy.local_addr().unwrap();
        let proxy_contacted = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(2), poison_proxy.accept())
                .await
                .is_ok()
        });

        let proxy_url = format!("http://{poison_address}");
        let output = tokio::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(
                "reasoning::http::tests::environment_proxy_variables_are_ignored_in_isolated_child",
            )
            .arg("--nocapture")
            .env(PROXY_CHILD, "1")
            .env(PROXY_DESTINATION, destination.to_string())
            .env("HTTP_PROXY", &proxy_url)
            .env("HTTPS_PROXY", &proxy_url)
            .env("ALL_PROXY", &proxy_url)
            .env_remove("NO_PROXY")
            .env_remove("http_proxy")
            .env_remove("https_proxy")
            .env_remove("all_proxy")
            .env_remove("no_proxy")
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "isolated child failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        tokio::time::timeout(Duration::from_secs(1), destination_request)
            .await
            .expect("pinned destination was not contacted")
            .unwrap();
        assert!(
            !proxy_contacted.await.unwrap(),
            "environment proxy was contacted"
        );
    }

    #[tokio::test]
    async fn redirect_is_returned_without_contacting_the_target() {
        let target = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let target_address = target.local_addr().unwrap();
        let target_probe = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_millis(300), target.accept())
                .await
                .is_ok()
        });
        let response = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{}/secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            target_address.port()
        );
        let leaked: &'static [u8] = Box::leak(response.into_bytes().into_boxed_slice());
        let (source_address, _) = http_server_once(leaked).await;

        let endpoint = loopback_endpoint(Some("llm.invalid"), source_address, false);
        let client = HardenedClientFactory::build(&endpoint).unwrap();
        let response = client
            .get(format!(
                "http://llm.invalid:{}/probe",
                source_address.port()
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        assert!(
            !target_probe.await.unwrap(),
            "redirect target was contacted"
        );
    }

    #[tokio::test]
    async fn oversized_response_body_is_aborted_with_and_without_content_length() {
        // 声明超长 Content-Length：直接拒绝，不读取 body。
        let declared = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            MAX_RESPONSE_BODY_BYTES + 1
        );
        let leaked: &'static [u8] = Box::leak(declared.into_bytes().into_boxed_slice());
        let (address, _) = http_server_once(leaked).await;
        let endpoint = loopback_endpoint(Some("llm.invalid"), address, false);
        let client = HardenedClientFactory::build(&endpoint).unwrap();
        let response = client
            .get(format!("http://llm.invalid:{}/probe", address.port()))
            .send()
            .await
            .unwrap();
        assert_eq!(
            read_limited_body(response, MAX_RESPONSE_BODY_BYTES)
                .await
                .unwrap_err()
                .kind(),
            crate::reasoning::ReasoningErrorKind::ResponseTooLarge
        );

        // 无 Content-Length 的 chunked 流：累计超限即中止。
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 8192];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            loop {
                // 每段 64 KiB，写满 5 段必超限；对端中止后 write 失败即退出。
                let chunk = format!("{:x}\r\n", 64 * 1024);
                if stream.write_all(chunk.as_bytes()).await.is_err() {
                    return;
                }
                if stream.write_all(&vec![b'x'; 64 * 1024]).await.is_err() {
                    return;
                }
                if stream.write_all(b"\r\n").await.is_err() {
                    return;
                }
            }
        });
        let endpoint = loopback_endpoint(Some("llm.invalid"), address, false);
        let client = HardenedClientFactory::build(&endpoint).unwrap();
        let response = client
            .get(format!("http://llm.invalid:{}/probe", address.port()))
            .send()
            .await
            .unwrap();
        assert_eq!(
            read_limited_body(response, MAX_RESPONSE_BODY_BYTES)
                .await
                .unwrap_err()
                .kind(),
            crate::reasoning::ReasoningErrorKind::ResponseTooLarge
        );
    }

    #[tokio::test]
    async fn empty_or_unreachable_pinned_addresses_fail_closed() {
        // 空地址集合：构建前拒绝。
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut endpoint = loopback_endpoint(Some("llm.invalid"), address, false);
        endpoint.addresses.clear();
        assert_eq!(
            HardenedClientFactory::build(&endpoint).unwrap_err().kind(),
            crate::reasoning::ReasoningErrorKind::EndpointRejected
        );

        // pin 到无人监听的 loopback 端口：连接被拒绝，不回退到重新解析。
        let silent = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let silent_address = silent.local_addr().unwrap();
        drop(silent);
        let endpoint = loopback_endpoint(Some("unreachable.invalid"), silent_address, false);
        let client = HardenedClientFactory::build(&endpoint).unwrap();
        let error = client
            .get(format!(
                "http://unreachable.invalid:{}/probe",
                silent_address.port()
            ))
            .send()
            .await
            .unwrap_err();
        assert!(error.is_connect(), "expected connect failure, got {error}");
    }

    #[tokio::test]
    async fn header_stage_stall_is_bounded_by_caller_deadline() {
        // server 接受连接但不回 header：外层 deadline 是唯一的总闸门。
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 8192];
            let _ = stream.read(&mut request).await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let endpoint = loopback_endpoint(Some("llm.invalid"), address, false);
        let client = HardenedClientFactory::build(&endpoint).unwrap();
        let outcome = tokio::time::timeout(
            Duration::from_millis(300),
            client
                .get(format!("http://llm.invalid:{}/probe", address.port()))
                .send(),
        )
        .await;
        assert!(
            outcome.is_err(),
            "stalled header stage must hit the caller deadline"
        );
    }
}
