//! 日志基座（ST-M1.5）。
//!
//! 对应设计文档「日志系统」与「日志与可观测模块」。职责：
//! - 小时滚动文件 `backend.log.YYYY-MM-DD-HH` + stderr 双写（tracing 多 layer）；
//! - 非阻塞写（`tracing-appender::non_blocking`，返回 `WorkerGuard` 保后台 flush）；
//! - 保留回收：删超 N 天旧文件 + 目录总量超上限删最旧；
//! - 脱敏：`redact_line` 覆盖 `ss_live_` 前缀 / 32B(64 hex) 密钥 / 敏感字段名（password·token·转译内容…）→ `***`；
//! - 请求级 trace_id：前端 `X-Trace-Id` 头 → 中间件注入 span + 事件字段 → 全链路日志带 trace_id。
//!
//! sidecar stdout/stderr 捕获进 backend.log 属 M3（`pipe_stdout`），本步不落地。
//!
//! 脱敏编码约束：敏感值一律以**字段**形式记日志（`info!(token=%x)`），勿插值进 message
//! 文本——`redact_line` 仅对渲染行的 `name=value` 与已知值模式生效，message 内裸露的敏感串
//! 除 `ss_live_`/64-hex 外不会被字段名规则覆盖。

use std::io::{self, Write};
use std::path::Path;
use std::sync::OnceLock;
use std::time::SystemTime;

use axum::extract::Request;
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::Response;
use regex::Regex;
use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_appender::rolling;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::{fmt, layer::SubscriberExt, EnvFilter, Registry};
use uuid::Uuid;
// `Future::instrument` 扩展 trait，供中间件把请求 future 绑入 trace_id span。
use tracing::Instrument;

/// 日志文件名前缀；小时切后形如 `backend.log.2099-01-01-00`。
const LOG_FILE_PREFIX: &str = "backend.log";
/// 旧日志保留天数；超此即删（设计「删超过 N 天的旧文件」）。
const RETAIN_DAYS: u64 = 7;
/// 日志目录总量上限；超此删最旧（按 mtime）直至达标。
const MAX_TOTAL_BYTES: u64 = 50 * 1024 * 1024;
/// 前端↔后端共享 trace_id 的 HTTP 头名。
const TRACE_ID_HEADER: &str = "X-Trace-Id";

// ===== 脱敏 =====

struct Patterns {
    /// token secret 前缀（openapi.yaml `bearerFormat: ss_live_`）。
    ss_live: Regex,
    /// 32 字节密钥 = 64 hex（DEK / K_sqlite / K_files / SHA-256 token hash 均此长度）。
    hex32: Regex,
    /// 敏感字段名 `name=value` → 掩码；`pre` 捕获行首/空白/逗号分隔。
    sensitive: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        ss_live: Regex::new(r"ss_live_[A-Za-z0-9_]+").expect("ss_live 正则"),
        hex32: Regex::new(r"\b[0-9a-fA-F]{64}\b").expect("hex32 正则"),
        sensitive: Regex::new(
            r#"(?P<pre>^|[\s,])(?P<name>password|current_password|cur_password|new_password|cur|new|secret|dek|master_dek|wrapped_dek|k_sqlite|k_files|key|token|transcript|full_text|text|segments)=(?P<val>"[^"]*"|[^,\s\]]*)"#,
        )
        .expect("sensitive 正则"),
    })
}

/// 对单行渲染文本脱敏。返回脱敏后的字符串。
///
/// 三类触发：`ss_live_<...>` → `ss_live_REDACTED`；64 hex（32B）→ `<redacted:32B-hex>`；
/// 敏感字段名 `name=value` → `name=***`。以行为单位，行内任何位置匹配即掩码（含 message）。
pub fn redact_line(line: &str) -> String {
    let p = patterns();
    let mut out = p.ss_live.replace_all(line, "ss_live_REDACTED").into_owned();
    out = p.hex32.replace_all(&out, "<redacted:32B-hex>").into_owned();
    out = p
        .sensitive
        .replace_all(&out, "${pre}${name}=***")
        .into_owned();
    out
}

// ===== 脱敏 writer：缓冲每事件、于 flush 对每行脱敏 =====

/// 双写：同一字节流写两路 sink（文件 NonBlocking + stderr）。
struct TeeWriter<A, B> {
    a: A,
    b: B,
}

impl<A: Write, B: Write> Write for TeeWriter<A, B> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.a.write_all(buf)?;
        self.b.write_all(buf)?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.a.flush()?;
        self.b.flush()
    }
}

/// 缓冲写入、于 flush/drop 对每行跑 `redact_line` 后写底层。
/// tracing fmt layer 每事件调一次 `make_writer` 得到新 writer，写完整事件后 drop
/// → Drop 触发脱敏落盘，无需 fmt 层显式 flush。
struct RedactWriter<W: Write> {
    inner: W,
    buf: Vec<u8>,
}

impl<W: Write> Write for RedactWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            let data = std::mem::take(&mut self.buf);
            let text = String::from_utf8_lossy(&data);
            let mut out = String::with_capacity(data.len());
            for line in text.split_inclusive('\n') {
                out.push_str(&redact_line(line));
            }
            self.inner.write_all(out.as_bytes())?;
        }
        self.inner.flush()
    }
}

impl<W: Write> Drop for RedactWriter<W> {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

/// 守护进程 `MakeWriter`：每事件返回一个 `RedactWriter<TeeWriter<NonBlocking, Stderr>>`。
struct DaemonMakeWriter {
    file: NonBlocking,
}

impl<'a> MakeWriter<'a> for DaemonMakeWriter {
    type Writer = RedactWriter<TeeWriter<NonBlocking, io::Stderr>>;
    fn make_writer(&'a self) -> Self::Writer {
        RedactWriter {
            inner: TeeWriter {
                a: self.file.clone(),
                b: io::stderr(),
            },
            buf: Vec::new(),
        }
    }
}

// ===== 保留回收 =====

/// 删超过 `max_age_days` 的旧日志 + 目录总量超 `max_total_bytes` 时删最旧直至达标。
/// 仅作用于 `backend.log*` 文件。失败项 best-effort 跳过，不阻断启动。
fn retain_logs(dir: &Path, max_age_days: u64, max_total_bytes: u64) -> io::Result<()> {
    let now = SystemTime::now();
    let age_cutoff = max_age_days.saturating_mul(86400);

    let mut kept: Vec<(std::path::PathBuf, SystemTime, u64)> = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let e = match e {
            Ok(e) => e,
            Err(_) => continue,
        };
        let p = e.path();
        let name = match p.file_name().and_then(|n| n.to_str()) {
            Some(n) if n.starts_with(LOG_FILE_PREFIX) => n,
            _ => continue,
        };
        let _ = name; // 仅过滤
        let md = match e.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = md.modified().unwrap_or(now);
        let size = md.len();
        let age = now.duration_since(mtime).map(|d| d.as_secs()).unwrap_or(0);
        if age >= age_cutoff {
            let _ = std::fs::remove_file(&p);
            continue;
        }
        kept.push((p, mtime, size));
    }

    let total: u64 = kept.iter().map(|(_, _, s)| *s).sum();
    if total > max_total_bytes {
        kept.sort_by_key(|(_, mtime, _)| *mtime); // 最旧在前
        let mut cur = total;
        for (p, _, s) in &kept {
            if cur <= max_total_bytes {
                break;
            }
            let _ = std::fs::remove_file(p);
            cur = cur.saturating_sub(*s);
        }
    }
    Ok(())
}

// ===== 初始化 =====

/// 初始化日志：建目录（0o700）→ 保留回收 → 小时滚动 + 非阻塞 + 双写 + 脱敏 → 全局 subscriber。
/// 返回 `WorkerGuard`，调用方须保其存活至进程退出（drop 时 flush 残留缓冲）。
pub fn init_logging(log_dir: &Path) -> WorkerGuard {
    let _ = std::fs::create_dir_all(log_dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(log_dir, PermissionsExt::from_mode(0o700));
    }
    let _ = retain_logs(log_dir, RETAIN_DAYS, MAX_TOTAL_BYTES);

    let file_appender = rolling::hourly(log_dir, LOG_FILE_PREFIX);
    let (nb, guard) = tracing_appender::non_blocking(file_appender);

    let make_writer = DaemonMakeWriter { file: nb };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let subscriber = Registry::default()
        .with(filter)
        .with(fmt::layer().with_writer(make_writer).with_ansi(false));

    // 守护进程为独立进程，首次设置必成功；best-effort：失败仅告警不阻断。
    if let Err(e) = tracing::subscriber::set_global_default(subscriber) {
        eprintln!("warning: set_global_default 失败: {e}");
    }
    guard
}

// ===== trace_id 中间件 =====

/// `axum` 中间件：取 `X-Trace-Id`（合法则用，否则 uuid v4 生成）→ 注入 `request` span
/// + 事件字段 → 落 backend.log。前端带同一 id，两端日志可串联同一请求。
pub async fn trace_id_middleware(req: Request, next: Next) -> Response {
    let trace_id = extract_trace_id(req.headers());
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let span = tracing::info_span!("request", trace_id = %trace_id);
    async move {
        tracing::info!(trace_id = %trace_id, %method, %path, "request received");
        next.run(req).await
    }
    .instrument(span)
    .await
}

fn extract_trace_id(headers: &HeaderMap) -> String {
    if let Some(v) = headers.get(TRACE_ID_HEADER).and_then(|v| v.to_str().ok()) {
        if is_valid_trace_id(v) {
            return v.to_owned();
        }
    }
    Uuid::new_v4().to_string()
}

/// 合法 trace_id：1..=64 字符，仅 ASCII 字母数字 / `-` / `_`（防注入与控制字符）。
fn is_valid_trace_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Duration;

    // --- redact_line ---

    #[test]
    fn redact_ss_live() {
        // 消息文本中的 ss_live_ 前缀 → 掩码（非字段名上下文，纯值模式命中）
        assert_eq!(
            redact_line("bearer ss_live_abc123 ok"),
            "bearer ss_live_REDACTED ok"
        );
        // 敏感字段 token=ss_live_... → 字段名规则更宽，掩整个值
        assert_eq!(redact_line("token=ss_live_abc123"), "token=***");
    }

    #[test]
    fn redact_64_hex() {
        let hex = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        // 非敏感字段名 + 64 hex → hex32 规则命中
        assert_eq!(
            redact_line(&format!("hash={hex}")),
            "hash=<redacted:32B-hex>"
        );
        // 敏感字段名 dek + 64 hex → 字段名规则更宽，掩整个值
        assert_eq!(redact_line(&format!("dek={hex}")), "dek=***");
        assert!(!redact_line(&format!("dek={hex}")).contains(hex));
        // 32-hex（salt / nonce 量级）不脱敏，避免误伤
        let salt = "00112233445566778899aabbccddeeff";
        assert_eq!(redact_line(&format!("salt={salt}")), format!("salt={salt}"));
    }

    #[test]
    fn redact_sensitive_fields() {
        assert_eq!(redact_line("password=hunter2"), "password=***");
        assert_eq!(redact_line("cur=oldpw new=newpw"), "cur=*** new=***");
        assert_eq!(redact_line(r#"transcript="hello world""#), "transcript=***");
        assert_eq!(
            redact_line("segments=abc segments=def"),
            "segments=*** segments=***"
        );
        assert_eq!(redact_line("secret=topsecret"), "secret=***");
        // current_password 须脱敏（openapi /auth/password 字段名，前缀 `_` 不阻 pre 匹配）。
        assert_eq!(
            redact_line("current_password=hunter2"),
            "current_password=***"
        );
        assert_eq!(
            redact_line("current_password=old new_password=new"),
            "current_password=*** new_password=***"
        );
    }

    #[test]
    fn redact_preserves_non_sensitive() {
        let line = "session_id=abc-123 port=60943 status=ok";
        assert_eq!(redact_line(line), line);
        // `tokens`（复数，元数据）非裸 `token=`，不脱敏
        assert_eq!(redact_line("tokens=[id1,id2]"), "tokens=[id1,id2]");
        // `new_state` 非 `new=`，不误伤
        assert_eq!(redact_line("new_state=loaded"), "new_state=loaded");
    }

    #[test]
    fn redact_mixed_line() {
        let hex = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let line = format!("method=GET password=hunter2 token=ss_live_x dek={hex}");
        let out = redact_line(&line);
        assert!(out.contains("password=***"));
        assert!(out.contains("token=***"));
        assert!(out.contains("dek=***")); // 字段名 + hex 双命中
        assert!(!out.contains("hunter2"));
        assert!(!out.contains("ss_live_x"));
        assert!(!out.contains(hex));
        assert!(out.contains("method=GET"));
    }

    // --- RedactWriter ---

    #[test]
    fn redact_writer_buffers_then_redacts_on_flush() {
        let mut w = RedactWriter {
            inner: Vec::<u8>::new(),
            buf: Vec::new(),
        };
        w.write_all(b"INFO password=hunter2 token=ss_live_abc123\n")
            .unwrap();
        w.flush().unwrap();
        let out = String::from_utf8(w.inner.clone()).unwrap();
        assert!(out.contains("password=***"));
        assert!(out.contains("token=***"));
        assert!(!out.contains("hunter2"));
        assert!(!out.contains("abc123"));
    }

    #[test]
    fn redact_writer_redacts_across_split_writes() {
        // 同一事件的写被分两次，flush 时 buffer 已拼合 → 仍脱敏。
        let mut w = RedactWriter {
            inner: Vec::<u8>::new(),
            buf: Vec::new(),
        };
        w.write_all(b"INFO password=hu").unwrap();
        w.write_all(b"nter2\n").unwrap();
        w.flush().unwrap();
        let out = String::from_utf8(w.inner.clone()).unwrap();
        assert!(out.contains("password=***"));
        assert!(!out.contains("hunter2"));
    }

    // --- trace_id ---

    #[test]
    fn valid_trace_id_accepted() {
        assert!(is_valid_trace_id("abc-123_XYZ"));
        assert!(is_valid_trace_id("a"));
        assert!(is_valid_trace_id(&"a".repeat(64)));
    }

    #[test]
    fn invalid_trace_id_rejected() {
        assert!(!is_valid_trace_id(""));
        assert!(!is_valid_trace_id(&"a".repeat(65)));
        assert!(!is_valid_trace_id("bad id")); // 空格
        assert!(!is_valid_trace_id("evil\r\ninject"));
    }

    #[test]
    fn extract_uses_header_when_valid() {
        let mut h = HeaderMap::new();
        h.insert(TRACE_ID_HEADER, "abc-123".parse().unwrap());
        assert_eq!(extract_trace_id(&h), "abc-123");
    }

    #[test]
    fn extract_generates_when_absent() {
        let h = HeaderMap::new();
        let id = extract_trace_id(&h);
        assert!(is_valid_trace_id(&id));
        assert_ne!(id, "");
    }

    #[test]
    fn extract_generates_when_invalid() {
        let mut h = HeaderMap::new();
        h.insert(TRACE_ID_HEADER, "bad id".parse().unwrap()); // 含空格，非法
        let id = extract_trace_id(&h);
        assert!(is_valid_trace_id(&id));
        assert_ne!(id, "bad id");
    }

    // --- retain_logs ---

    fn set_mtime(p: &Path, t: SystemTime) {
        use std::fs::FileTimes;
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(p)
            .expect("open");
        f.set_times(FileTimes::new().set_modified(t))
            .expect("set_times");
    }

    #[test]
    fn retain_deletes_old_logs_keeps_recent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let old = dir.path().join("backend.log.2099-01-01-00");
        let new = dir.path().join("backend.log.2099-06-01-00");
        std::fs::write(&old, b"x").unwrap();
        std::fs::write(&new, b"x").unwrap();
        let now = SystemTime::now();
        set_mtime(
            &old,
            now.checked_sub(Duration::from_secs(10 * 86400)).unwrap(),
        );
        set_mtime(&new, now);
        retain_logs(dir.path(), RETAIN_DAYS, MAX_TOTAL_BYTES).unwrap();
        assert!(!old.exists(), "超 7 天旧日志应删");
        assert!(new.exists(), "近期日志应留");
    }

    #[test]
    fn retain_caps_total_size_deletes_oldest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("backend.log.aa");
        let b = dir.path().join("backend.log.bb");
        std::fs::write(&a, b"aaaa").unwrap(); // 4B，较旧
        std::fs::write(&b, b"bbbb").unwrap(); // 4B，较新
        let now = SystemTime::now();
        set_mtime(&a, now.checked_sub(Duration::from_secs(1000)).unwrap());
        set_mtime(&b, now);
        // 不按年龄删（u64::MAX），仅按总量：8B > 5B → 删最旧 a
        retain_logs(dir.path(), u64::MAX, 5).unwrap();
        assert!(!a.exists(), "超总量应删最旧");
        assert!(b.exists(), "较新应留");
    }
}
