//! GUI 壳 / 测试桩 复用：spawn 守护进程 + bootstrap 发现 + 健康等待 + 简易 HTTP 客户端。
//!
//! 对应 daemon 生命周期的 GUI 侧职责（spawn + wait_health）；
//! M6（Tauri GUI 壳）将正式落地该 trait，此模块为过渡实现，便于 ST-M1.x 集成验收。

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};

use crate::bootstrap::{health_probe, Bootstrap};

/// spawn 守护进程二进制，stdin 设为管道（本进程持写端，daemon 继承读端），
/// `SEASNAIL_DATA_DIR` 指向 `home`（bootstrap 与后续 lock 落点，便于测试隔离）。
/// 调用方持返回的 Child；`drop(child.stdin.take())` 即模拟 GUI 退出 / 崩溃
/// （关闭写端 → daemon stdin EOF → 自退）。
pub async fn spawn_daemon(bin: &str, home: &Path) -> std::io::Result<Child> {
    Command::new(bin)
        .stdin(Stdio::piped())
        .stderr(Stdio::inherit())
        .env("SEASNAIL_DATA_DIR", home)
        .kill_on_drop(false)
        .spawn()
}

/// 简易 HTTP GET（raw TCP，测试用）：返回 `(状态码, body)`。
/// 发 `Connection: close` 使服务在响应后关连接 → `read_to_end` 读到 EOF 取全量。
/// 供集成验收对 OpenAPI 端点做断言（M1 `/api/v1/auth/status` 起，M2+ 复用）。
///
/// **仅适用于小 JSON 文本端点**：body 用 `split("\r\n\r\n")` 切分，不支持
/// chunked transfer-encoding；`from_utf8_lossy` 会损坏二进制。`/sessions/{id}/audio`
/// 等二进制 / 大响应 / 分块场景需另写 helper，勿复用此函数。
pub async fn http_get(port: u16, path: &str) -> std::io::Result<(u16, String)> {
    http_request("GET", port, path, None, &[]).await
}

/// 同 `http_get`，但携带自定义请求头（如 `X-Trace-Id`）。
pub async fn http_get_with_headers(
    port: u16,
    path: &str,
    headers: &[(&str, &str)],
) -> std::io::Result<(u16, String)> {
    http_request("GET", port, path, None, headers).await
}

/// 简易 HTTP POST（raw TCP，测试用）：`application/json` body + 自定义头（如
/// `Authorization: Bearer ...`）。返回 `(状态码, body)`。同 `http_get` 的限制（小 JSON）。
pub async fn http_post(
    port: u16,
    path: &str,
    body: &str,
    headers: &[(&str, &str)],
) -> std::io::Result<(u16, String)> {
    http_request("POST", port, path, Some(body), headers).await
}

async fn http_request(
    method: &str,
    port: u16,
    path: &str,
    body: Option<&str>,
    headers: &[(&str, &str)],
) -> std::io::Result<(u16, String)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n");
    let body_bytes = body.unwrap_or("");
    if body.is_some() {
        req.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body_bytes.len()
        ));
    }
    for (name, value) in headers {
        req.push_str(&format!("{name}: {value}\r\n"));
    }
    req.push_str("Connection: close\r\n\r\n");
    req.push_str(body_bytes);
    s.write_all(req.as_bytes()).await?;
    let mut buf = Vec::with_capacity(8192);
    s.read_to_end(&mut buf).await?;
    let text = String::from_utf8_lossy(&buf);
    let status_line = text.split("\r\n").next().unwrap_or("");
    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("无法解析 HTTP 状态行: {status_line:?}"),
            )
        })?;
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    Ok((code, body))
}

/// 轮询 `GET /` 至 200，超时返回 false。
pub async fn wait_health(port: u16, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        if health_probe(port).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 经 bootstrap 文件发现守护进程并确认健康：轮询 bootstrap 出现 + GET / 200，返回 port。
/// 对应 GUI 启动流程：读 bootstrap → 校验存活 → 接入；连不上由调用方当 stale 重新 spawn。
pub async fn wait_ready(home: &Path, timeout: Duration) -> std::io::Result<u16> {
    let bs = Bootstrap::new(home.to_path_buf());
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "bootstrap 未就绪超时",
            ));
        }
        if let Some(info) = bs.read() {
            if health_probe(info.port).await {
                return Ok(info.port);
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
