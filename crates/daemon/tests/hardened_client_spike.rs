//! ST-M0.2 feasibility spike for request-scoped DNS pinning.
//!
//! Production endpoint classification and the reusable client factory belong to M3. This test
//! intentionally proves only the reqwest mechanics that would otherwise block that design:
//! pinning an unresolvable URL hostname to approved socket addresses preserves HTTP authority and
//! TLS SNI, while proxy and redirect behavior can be disabled explicitly.

use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

const PROXY_CHILD: &str = "SEASNAIL_M02_PROXY_CHILD";
const PROXY_DESTINATION: &str = "SEASNAIL_M02_PROXY_DESTINATION";

struct RebindingResolver {
    answers: Mutex<VecDeque<Vec<SocketAddr>>>,
}

impl RebindingResolver {
    fn new(answers: impl IntoIterator<Item = Vec<SocketAddr>>) -> Self {
        Self {
            answers: Mutex::new(answers.into_iter().collect()),
        }
    }

    fn resolve(&self) -> Vec<SocketAddr> {
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .expect("mock DNS answer exhausted")
    }

    fn remaining_answers(&self) -> usize {
        self.answers.lock().unwrap().len()
    }
}

fn pinned_client(host: &str, addresses: &[SocketAddr]) -> Result<reqwest::Client, &'static str> {
    if addresses.is_empty() || addresses.iter().any(|address| !address.ip().is_loopback()) {
        return Err("spike policy rejected an unapproved address");
    }
    reqwest::Client::builder()
        // Calling `no_proxy` after adding an explicit poison proxy proves that it clears both
        // explicit and environment/system proxy configuration.
        .proxy(
            reqwest::Proxy::all("http://127.0.0.1:9")
                .map_err(|_| "failed to construct poison proxy")?,
        )
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(2))
        .resolve_to_addrs(host, addresses)
        .build()
        .map_err(|_| "failed to build pinned client")
}

fn pinned_client_from_dns(
    host: &str,
    resolver: &RebindingResolver,
) -> Result<reqwest::Client, &'static str> {
    let snapshot = resolver.resolve();
    pinned_client(host, &snapshot)
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
async fn pin_preserves_url_authority_and_bypasses_proxy_configuration() {
    let (address, request) =
        http_server_once(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await;
    let client = pinned_client("llm.invalid", &[address]).unwrap();
    let response = client
        .get(format!("http://llm.invalid:{}/probe", address.port()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "ok");

    let request = String::from_utf8(request.await.unwrap()).unwrap();
    assert!(request.starts_with("GET /probe HTTP/1.1\r\n"));
    assert!(request
        .to_ascii_lowercase()
        .contains(&format!("host: llm.invalid:{}\r\n", address.port())));
}

#[tokio::test]
async fn environment_proxy_variables_are_ignored_in_isolated_child() {
    if std::env::var_os(PROXY_CHILD).is_some() {
        let destination: SocketAddr = std::env::var(PROXY_DESTINATION).unwrap().parse().unwrap();
        let client = pinned_client("llm.invalid", &[destination]).unwrap();
        let response = client
            .get(format!("http://llm.invalid:{}/probe", destination.port()))
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "env-proxy-ok");
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
        .arg("environment_proxy_variables_are_ignored_in_isolated_child")
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
async fn pin_preserves_original_hostname_in_tls_client_hello_sni() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let capture = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut hello = vec![0_u8; 8192];
        let read = stream.read(&mut hello).await.unwrap();
        hello.truncate(read);
        hello
    });

    let client = pinned_client("llm.invalid", &[address]).unwrap();
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
async fn redirect_is_returned_without_contacting_the_redirect_target() {
    let target = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let target_address = target.local_addr().unwrap();
    let target_probe = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_millis(200), target.accept())
            .await
            .is_ok()
    });
    let response = format!(
        "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{}/secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        target_address.port()
    );
    let leaked: &'static [u8] = Box::leak(response.into_bytes().into_boxed_slice());
    let (source_address, _) = http_server_once(leaked).await;

    let client = pinned_client("llm.invalid", &[source_address]).unwrap();
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

#[test]
fn mixed_or_unapproved_addresses_fail_before_client_construction() {
    let approved = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 11434);
    let unapproved = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)), 11434);
    assert!(pinned_client("llm.invalid", &[approved, unapproved]).is_err());
    assert!(pinned_client("llm.invalid", &[]).is_err());
}

#[tokio::test]
async fn dns_snapshot_is_validated_once_and_rebinding_answer_is_not_used() {
    let (approved, request) =
        http_server_once(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await;
    let rebound = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)), approved.port());
    let resolver = RebindingResolver::new([vec![approved], vec![rebound]]);

    let client = pinned_client_from_dns("rebind.invalid", &resolver).unwrap();
    let response = client
        .get(format!("http://rebind.invalid:{}/probe", approved.port()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "ok");
    request.await.unwrap();
    assert_eq!(
        resolver.remaining_answers(),
        1,
        "request unexpectedly performed a second DNS resolution"
    );
}

#[test]
fn mixed_mock_dns_answer_fails_before_client_construction() {
    let approved = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 11434);
    let unapproved = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)), 11434);
    let resolver = RebindingResolver::new([vec![approved, unapproved]]);
    assert!(pinned_client_from_dns("mixed.invalid", &resolver).is_err());
}
