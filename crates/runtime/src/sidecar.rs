//! sidecar 子进程封装（ST-M3.1）：spawn + stderr drain + health 轮询 + stop。
//!
//! 复用 daemon `harness.rs` 的 spawn + 轮询就绪模式，补齐 stderr 管道 drain
//! （防背压）与 kill+wait 退出码。仅真二进制 driver 用；mock 走进程内 axum
//! task（见 [`crate::MockRuntime`]）。

use regex::Regex;
use std::io;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

pub struct SidecarProcess {
    child: Option<Child>,
    drain: Vec<JoinHandle<()>>,
    tag: &'static str,
    record: Option<std::path::PathBuf>,
}

impl SidecarProcess {
    /// spawn：`cmd` 已配好二进制 + flags + 模型路径；设 stdout/stderr piped 起 drain。
    pub async fn spawn(mut cmd: Command, tag: &'static str) -> io::Result<Self> {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let pending = crate::process_identity::SpawnRecord::before_spawn(tag)?;
        let pending_path = pending.as_ref().map(|record| record.path().to_path_buf());
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(error) => {
                if let Some(record) = pending {
                    record.no_child()?;
                }
                return Err(error);
            }
        };
        let record = match pending
            .map(|record| {
                record.register(
                    child
                        .id()
                        .ok_or_else(|| io::Error::other("missing child PID"))?,
                )
            })
            .transpose()
        {
            Ok(record) => record,
            Err(error) => {
                let _ = child.start_kill();
                if matches!(
                    tokio::time::timeout(Duration::from_secs(3), child.wait()).await,
                    Ok(Ok(_))
                ) {
                    if let Some(path) = pending_path {
                        let _ = std::fs::remove_file(path);
                    }
                }
                return Err(error);
            }
        };
        let mut drain = Vec::new();
        if let Some(o) = child.stdout.take() {
            drain.push(tokio::spawn(drain_pipe(o, tag, false)));
        }
        if let Some(e) = child.stderr.take() {
            drain.push(tokio::spawn(drain_pipe(e, tag, true)));
        }
        Ok(Self {
            child: Some(child),
            drain,
            tag,
            record,
        })
    }

    /// 轮询 GET / 至 200 或 `timeout`。raw TCP（健康探测轻量，不依赖 reqwest）。
    pub async fn wait_health(port: u16, timeout: Duration) -> bool {
        Self::wait_health_at(port, "/", timeout).await
    }

    /// 轮询指定 HTTP health path 至 200。FunASR 的 FastAPI sidecar 使用
    /// `/health`；whisper.cpp 与既有 mock 则保留根路径 `/`。
    pub async fn wait_health_at(port: u16, path: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            // `TcpStream::read` can wait forever if a process accepts but never responds.
            // Bound every connect/write/read probe by the remaining overall health deadline.
            if matches!(
                tokio::time::timeout(remaining, probe_health(port, path)).await,
                Ok(true)
            ) {
                return true;
            }
            let pause =
                Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now()));
            if pause.is_zero() {
                break;
            }
            tokio::time::sleep(pause).await;
        }
        false
    }

    /// kill + wait 退出码（3s 超时后强 kill）+ abort drain。幂等。
    pub async fn stop(&mut self) -> io::Result<Option<i32>> {
        if let Some(child) = self.child.as_mut() {
            child.start_kill()?;
            let status = tokio::time::timeout(Duration::from_secs(3), child.wait())
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::TimedOut, "sidecar exit not confirmed")
                })??;
            self.child.take();
            for h in &self.drain {
                h.abort();
            }
            if let Some(path) = self.record.take() {
                crate::process_identity::clear_if_exited(&path)?;
            }
            return Ok(status.code());
        }
        Ok(None)
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(|c| c.id())
    }

    pub fn tag(&self) -> &'static str {
        self.tag
    }

    /// Verify that this child, rather than another local process, owns exactly
    /// one IPv4 loopback listener on `port`. Callers must refuse audio upload
    /// and stop the child when this check fails.
    pub async fn owns_loopback_listener(&self, port: u16) -> bool {
        let Some(pid) = self.pid() else {
            return false;
        };
        let output = match tokio::time::timeout(
            Duration::from_secs(1),
            Command::new("lsof")
                .args([
                    "-nP",
                    "-a",
                    "-p",
                    &pid.to_string(),
                    &format!("-iTCP:{port}"),
                    "-sTCP:LISTEN",
                ])
                .output(),
        )
        .await
        {
            Ok(Ok(output)) if output.status.success() => output,
            _ => return false,
        };
        let text = String::from_utf8_lossy(&output.stdout);
        let listeners: Vec<_> = text
            .lines()
            .skip(1)
            .filter(|line| !line.trim().is_empty())
            .collect();
        listeners.len() == 1 && listeners[0].ends_with(&format!("TCP 127.0.0.1:{port} (LISTEN)"))
    }
}

/// raw TCP GET / 探测 200。
async fn probe_health(port: u16, path: &str) -> bool {
    use tokio::net::TcpStream;
    let mut s = match TcpStream::connect(("127.0.0.1", port)).await {
        Ok(s) => s,
        Err(_) => return false,
    };
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    let req = request.as_bytes();
    if s.write_all(req).await.is_err() {
        return false;
    }
    let mut buf = [0u8; 128];
    let n = match s.read(&mut buf).await {
        Ok(n) if n > 0 => n,
        _ => return false,
    };
    let line = String::from_utf8_lossy(&buf[..n]);
    line.starts_with("HTTP/") && line.contains(" 200 ")
}

/// 行级 drain stdout/stderr；只记录安全分类与长度，绝不记录可能含路径或用户数据的原文。
async fn drain_pipe<R>(pipe: R, tag: &'static str, is_stderr: bool)
where
    R: AsyncReadExt + Unpin,
{
    let mut lines = BufReader::new(pipe).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let filtered = strip_control_chars(&line);
        if filtered.trim().is_empty() {
            continue;
        }
        let message_len = filtered.len();
        if is_stderr && is_error_line(&filtered) {
            tracing::warn!(
                runtime = tag,
                stream = "stderr",
                class = "error",
                message_len,
                "sidecar output classified"
            );
        } else {
            tracing::debug!(
                runtime = tag,
                stream = if is_stderr { "stderr" } else { "stdout" },
                class = "non_error",
                message_len,
                "sidecar output classified"
            );
        }
    }
}

fn is_error_line(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    l.contains("error") || l.contains("fail") || l.contains("fatal")
}

/// 过滤 whisper.cpp 进度条控制字符（\r 覆写 + ANSI SGR）。
pub fn strip_control_chars(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\x1b\[[0-9;]*[mK]").expect("control-char regex"));
    let no_ansi = re.replace_all(s, "");
    no_ansi.split('\r').next_back().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::strip_control_chars;

    #[test]
    fn strip_ansi_and_cr() {
        let s = "\rwhisper \x1b[32mprogress 50%\x1b[0m";
        let out = strip_control_chars(s);
        assert!(!out.contains('\r'));
        assert!(!out.contains("\x1b["));
        assert!(out.contains("progress 50%"));
    }

    #[test]
    fn passthrough_plain() {
        assert_eq!(strip_control_chars("hello world"), "hello world");
    }
}
