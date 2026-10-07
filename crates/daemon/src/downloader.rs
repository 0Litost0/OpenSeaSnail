//! ST-M4.2：模型组件按需下载器。
//!
//! 从 ModelScope 直拉（URL 由 manifest 的 `model_id/revision/path` 派生，实测支持 HTTP
//! Range 206），断点续传 + 逐文件 SHA-256 校验 + 拒 `*.incomplete` 半成品 + 原子 rename +
//! 进度回调。机制 URL 无关，单测用本地 axum mock server（HTTP，无需 TLS）；真实 ModelScope
//! HTTPS 所需的 reqwest TLS feature 在接线真 URL 时再加。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

const INCOMPLETE: &str = ".incomplete";

/// 连接超时（防 SYN/握手 hang）。**不设** `.timeout`（total 会误杀 1G 慢下载）；
/// 改用 per-chunk idle timeout（`IDLE_TIMEOUT`）抓「连上后长时间无数据」的 hang。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// 单 chunk 间最长无数据间隔：超过即判 hang → `Timeout`（瞬态）→ 重试续传。
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// 单文件下载最大尝试次数（含首次）。瞬态错误（Http/Io/Timeout）退避重试；
/// 内容错误（SHA/Size）不重试。
const MAX_ATTEMPTS: u32 = 4;
/// 退避基数（毫秒）：`500ms * 2^(attempt-1)`，封顶 30s。重试不锤击故障服务器。
const BACKOFF_BASE_MS: u64 = 500;
const BACKOFF_CAP_MS: u64 = 30_000;

/// manifest 根（`models-manifest.json`：`{format, models[]}`）。
#[derive(Debug, Deserialize)]
pub struct ManifestRoot {
    #[allow(dead_code)]
    pub format: u32,
    pub models: Vec<ManifestComponent>,
}

/// manifest 一个组件条目（role/model_id/revision + 文件列表）。
#[derive(Debug, Clone, Deserialize)]
pub struct ManifestComponent {
    pub role: String,
    pub model_id: String,
    pub revision: String,
    pub files: Vec<ManifestFile>,
}

/// manifest 一个文件条目。
#[derive(Debug, Clone, Deserialize)]
pub struct ManifestFile {
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
}

/// 派生 ModelScope 下载 URL（`…/repo?Revision=…&FilePath=…`，实测支持 Range 206）。
/// `Revision`/`FilePath` 经百分号编码（query 值中的 `&`/`=`/`#`/空格/非 ASCII 等不安全字符）；
/// `/` 保留不编码（ModelScope 的 FilePath 用 `/` 分层路径，编码为 `%2F` 会破坏解析）。
pub fn modelscope_url(model_id: &str, revision: &str, file_path: &str) -> String {
    format!(
        "https://www.modelscope.cn/api/v1/models/{model_id}/repo?Revision={}&FilePath={}",
        encode_query(revision),
        encode_query(file_path),
    )
}

/// query 值百分号编码：保留字母数字与 `-._~/`，其余按字节 `%XX`。
fn encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// 下载错误。
#[derive(Debug)]
pub enum DownloaderError {
    Http(String),
    Io(String),
    Sha256Mismatch {
        expected: String,
        actual: String,
    },
    SizeMismatch {
        expected: u64,
        actual: u64,
    },
    /// 连上后单 chunk 间无数据超 `IDLE_TIMEOUT`，或连接超时。瞬态 → 可重试续传。
    Timeout(String),
    /// 取消（预留：将来取消端点接线；本次仅建 variant）。
    Cancelled,
}

/// 是否瞬态错误（值得退避重试）。Http/Io/Timeout 视为瞬态；SHA/Size 为内容问题
/// 不重试；Cancelled 不可恢复。
fn is_transient(e: &DownloaderError) -> bool {
    matches!(
        e,
        DownloaderError::Http(_) | DownloaderError::Io(_) | DownloaderError::Timeout(_)
    )
}

/// 重试退避：`BACKOFF_BASE_MS * 2^(attempt-1)`，封顶 `BACKOFF_CAP_MS`。
fn backoff(attempt: u32) -> Duration {
    let ms = std::cmp::min(
        BACKOFF_CAP_MS,
        BACKOFF_BASE_MS.saturating_mul(1u64 << (attempt - 1)),
    );
    Duration::from_millis(ms)
}

impl std::fmt::Display for DownloaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(m) => write!(f, "download http: {m}"),
            Self::Io(m) => write!(f, "download io: {m}"),
            Self::Sha256Mismatch { expected, actual } => {
                write!(f, "sha256 mismatch: expected {expected}, got {actual}")
            }
            Self::SizeMismatch { expected, actual } => {
                write!(f, "size mismatch: expected {expected}, got {actual}")
            }
            Self::Timeout(m) => write!(f, "download timeout: {m}"),
            Self::Cancelled => write!(f, "download cancelled"),
        }
    }
}
impl std::error::Error for DownloaderError {}

/// 进度回调（已下载字节 / 总字节）。`Arc<dyn Fn>` 便于多文件累计跨闭包共享。
pub type ProgressFn = std::sync::Arc<dyn Fn(u64, u64) + Send + Sync>;

/// 共享 HTTP 客户端（连接池复用，`connect_timeout` 防 SYN hang；无 total timeout——
/// 大文件慢下载靠 `IDLE_TIMEOUT` 抓无数据 hang，而非整请求超时）。
fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .expect("reqwest client with connect_timeout builds")
}

/// 下载一个组件的全部文件到 `dest_dir`（逐文件 Range 续传 + SHA + incomplete + rename）。
/// `url_for` 派生每文件 URL（prod 传 `modelscope_url(model_id, revision, path)`；
/// 测试注入 mock URL）；`progress` 报累计 (done, total) 且**逐 chunk 更新**（单大文件
/// 期间不冻结）；任一文件失败立即返回（已落盘的完整文件保留）。
pub async fn download_component(
    component: &ManifestComponent,
    dest_dir: &Path,
    progress: ProgressFn,
    url_for: impl Fn(&ManifestFile) -> String,
) -> Result<(), DownloaderError> {
    let client = build_client();
    let total: u64 = component.files.iter().map(|f| f.size_bytes).sum();
    let mut done = 0u64;
    for f in &component.files {
        let url = url_for(f);
        download_file_inner(
            &client,
            &url,
            &f.sha256,
            f.size_bytes,
            &dest_dir.join(&f.path),
            Some(&progress),
            done,
            total,
        )
        .await?;
        done += f.size_bytes;
        progress(done, total);
    }
    Ok(())
}

/// 下载单文件到 `dest`（测试薄壳：默认 client、无进度回调）。完整逻辑见 `download_file_inner`。
pub async fn download_file(
    url: &str,
    expected_sha: &str,
    expected_size: u64,
    dest: &Path,
) -> Result<(), DownloaderError> {
    let client = build_client();
    download_file_inner(
        &client,
        url,
        expected_sha,
        expected_size,
        dest,
        None,
        0,
        expected_size,
    )
    .await
}

/// 下载单文件到 `dest`：Range 续传 + per-chunk 进度 + idle 超时 + 重试退避 +
/// 逐文件 SHA-256 校验 + 拒 `*.incomplete` + 原子 rename。
///
/// - 幂等（re-SHA）：`dest` 已存在且 size 对 → **流式 SHA 复校**；通过→Ok，不符→删 dest 重下
///   （防「右大小错内容漏检」，review 延后项）。
/// - 续传：`dest.incomplete` 存在 → `Range: bytes=<其大小>-` 续传（206 append）；
///   服务器忽略 Range 返 200 → 截断重来（truncate，不拼接残留）。
/// - 超时：单 chunk 间无数据超 `IDLE_TIMEOUT` → `Timeout`（瞬态）→ 重试续传。
/// - 重试：瞬态错误（Http/Io/Timeout）退避重试 `MAX_ATTEMPTS` 次；SHA/Size 内容错误不重试。
/// - 完成后整文件 SHA-256 + size 校验；不符删 incomplete 报错（拒半成品）；通过原子 rename。
///
/// `progress`：每写一块 chunk 后调 `progress(done_before + base + written, total)`，
/// `done_before` = 此文件前已完成的累计字节，`base` = 本文件已落盘的续传起点。
async fn download_file_inner(
    client: &reqwest::Client,
    url: &str,
    expected_sha: &str,
    expected_size: u64,
    dest: &Path,
    progress: Option<&ProgressFn>,
    done_before: u64,
    total: u64,
) -> Result<(), DownloaderError> {
    // 幂等 re-SHA：dest 已存在且 size 对 → 流式 SHA 复校；通过 Ok，不符删 dest 走重下。
    if let Ok(meta) = tokio::fs::metadata(dest).await {
        if meta.len() == expected_size {
            match sha256_file(dest).await {
                Ok(actual) if actual == expected_sha => return Ok(()),
                _ => {
                    let _ = tokio::fs::remove_file(dest).await;
                }
            }
        }
    }
    let incomplete: PathBuf = dest.with_file_name(format!(
        "{}{INCOMPLETE}",
        dest.file_name().and_then(|s| s.to_str()).unwrap_or("file")
    ));

    for attempt in 1..=MAX_ATTEMPTS {
        // 每轮重读 incomplete 长度作续传起点（retry 自然续传）。
        let mut resume_from = match tokio::fs::metadata(&incomplete).await {
            Ok(m) => m.len(),
            Err(_) => 0,
        };
        // incomplete 超 expected（损坏/旧版）→ 删了重来；避免 Range: bytes={>size}- 触发 416。
        if resume_from > expected_size {
            let _ = tokio::fs::remove_file(&incomplete).await;
            resume_from = 0;
        }

        // incomplete 未满 → 下载补齐；已满（== expected）则跳过网络直接校验+rename
        // ——上次跑到 flush 后、rename 前被中断的情形。
        if resume_from < expected_size {
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| DownloaderError::Io(e.to_string()))?;
            }
            match fetch_with_retry(
                client,
                url,
                &incomplete,
                resume_from,
                progress,
                done_before,
                total,
            )
            .await
            {
                Ok(()) => {}
                Err(e) if is_transient(&e) && attempt < MAX_ATTEMPTS => {
                    tokio::time::sleep(backoff(attempt)).await;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }

        // SHA-256 校验整文件（含 resume 的旧前缀；已满则直接校验上次留的 incomplete）。
        let actual = sha256_file(&incomplete).await?;
        if actual != expected_sha {
            let _ = tokio::fs::remove_file(&incomplete).await;
            return Err(DownloaderError::Sha256Mismatch {
                expected: expected_sha.into(),
                actual,
            });
        }
        let actual_size = tokio::fs::metadata(&incomplete)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        if actual_size != expected_size {
            let _ = tokio::fs::remove_file(&incomplete).await;
            return Err(DownloaderError::SizeMismatch {
                expected: expected_size,
                actual: actual_size,
            });
        }
        tokio::fs::rename(&incomplete, dest)
            .await
            .map_err(|e| DownloaderError::Io(e.to_string()))?;
        return Ok(());
    }
    unreachable!("download retry loop returns on every path")
}

/// 单次网络抓取（send + 流式写 incomplete），含 per-chunk idle 超时与逐 chunk 进度回调。
/// `resume_from` = 已落盘字节数（206 append 起点）；返回的 `is_resume` 据响应状态决定
/// append 还是截断。瞬态错误（Http/Io/Timeout）上抛由调用方决定重试。
async fn fetch_with_retry(
    client: &reqwest::Client,
    url: &str,
    incomplete: &Path,
    resume_from: u64,
    progress: Option<&ProgressFn>,
    done_before: u64,
    total: u64,
) -> Result<(), DownloaderError> {
    let mut req = client.get(url);
    if resume_from > 0 {
        req = req.header("Range", format!("bytes={resume_from}-"));
    }
    let mut resp = req
        .send()
        .await
        .map_err(|e| DownloaderError::Http(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(DownloaderError::Http(format!("HTTP {}", resp.status())));
    }
    // 206=续传（append 到 incomplete 尾）；200=服务器忽略 Range 或全量（截断重来）。
    let is_resume = resume_from > 0 && resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    use tokio::io::AsyncWriteExt;
    let mut file = if is_resume {
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(incomplete)
            .await
            .map_err(|e| DownloaderError::Io(e.to_string()))?
    } else {
        tokio::fs::File::create(incomplete)
            .await
            .map_err(|e| DownloaderError::Io(e.to_string()))?
    };
    // 进度基准：续传用 resume_from（已有前缀），200 截断则从 0 起（incomplete 已清空）。
    let base = if is_resume { resume_from } else { 0 };
    let mut written = 0u64;
    loop {
        let chunk_future = resp.chunk();
        match tokio::time::timeout(IDLE_TIMEOUT, chunk_future).await {
            Ok(Ok(Some(chunk))) => {
                file.write_all(&chunk)
                    .await
                    .map_err(|e| DownloaderError::Io(e.to_string()))?;
                written += chunk.len() as u64;
                if let Some(p) = progress {
                    p(done_before + base + written, total);
                }
            }
            Ok(Ok(None)) => break,
            Ok(Err(e)) => return Err(DownloaderError::Http(e.to_string())),
            Err(_) => {
                return Err(DownloaderError::Timeout(format!(
                    "no data for {IDLE_TIMEOUT:?} during {url}"
                )));
            }
        }
    }
    file.flush()
        .await
        .map_err(|e| DownloaderError::Io(e.to_string()))?;
    Ok(())
}

/// 流式 SHA-256 一整个文件（64KB 块，避免对 1G 文件全量入内存）。
async fn sha256_file(path: &Path) -> Result<String, DownloaderError> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| DownloaderError::Io(e.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 65536];
    loop {
        let n = file
            .read(&mut buf)
            .await
            .map_err(|e| DownloaderError::Io(e.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    //! M4.2 机制单测：本地 axum mock server（支持 Range 206），验证全量下载、
    //! 断点续传、SHA 不符拒绝 + 删 incomplete。URL 无关，无需 TLS。

    use super::*;
    use axum::body::Bytes;
    use axum::extract::Request;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;
    use std::sync::Arc;

    /// mock server：serve 一份已知 bytes，支持 Range（206 + 切片）+ 无 Range（200 全量）。
    fn mock_app(content: Arc<Vec<u8>>) -> Router {
        Router::new().route(
            "/file",
            get(move |req: Request| {
                let content = content.clone();
                async move {
                    let total = content.len() as u64;
                    if let Some(range) = req.headers().get("range") {
                        let s = range.to_str().unwrap_or("");
                        let nums: Vec<&str> =
                            s.strip_prefix("bytes=").unwrap_or("").split('-').collect();
                        let start = nums
                            .first()
                            .and_then(|n| n.parse::<u64>().ok())
                            .unwrap_or(0);
                        // `bytes=start-`（开区间，到末尾）→ end=total-1；`start-end`（闭）→ end
                        let end = nums
                            .get(1)
                            .and_then(|n| n.parse::<u64>().ok())
                            .unwrap_or(total.saturating_sub(1))
                            .min(total.saturating_sub(1));
                        let lo = (start as usize).min(content.len());
                        let hi = ((end + 1) as usize).min(content.len());
                        let slice = Bytes::copy_from_slice(&content[lo..hi]);
                        return (
                            StatusCode::PARTIAL_CONTENT,
                            [
                                ("content-range", format!("bytes {start}-{end}/{total}")),
                                ("content-length", format!("{}", hi - lo)),
                            ],
                            slice,
                        )
                            .into_response();
                    }
                    (StatusCode::OK, Bytes::from(content.as_ref().clone())).into_response()
                }
            }),
        )
    }

    async fn serve(content: Arc<Vec<u8>>) -> (u16, tokio::task::JoinHandle<()>) {
        let app = mock_app(content);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let h = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (port, h)
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(bytes);
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    #[tokio::test]
    async fn download_full_file_succeeds() {
        let content = b"hello seasnail downloader".to_vec();
        let sha = sha256_hex(&content);
        let (port, h) = serve(Arc::new(content.clone())).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.bin");
        download_file(
            &format!("http://127.0.0.1:{port}/file"),
            &sha,
            content.len() as u64,
            &dest,
        )
        .await
        .expect("download");
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            content,
            "dest 内容应 == mock"
        );
        assert!(
            !dir.path().join("out.bin.incomplete").exists(),
            "incomplete 应已 rename 掉"
        );
        h.abort();
    }

    #[tokio::test]
    async fn download_resumes_from_partial_incomplete() {
        let content = (0..100_000).map(|i| (i % 251) as u8).collect::<Vec<u8>>();
        let sha = sha256_hex(&content);
        let (port, h) = serve(Arc::new(content.clone())).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("res.bin");
        let incomplete = dir.path().join("res.bin.incomplete");
        // 预写 incomplete 前 40_000 字节（模拟中断）
        std::fs::write(&incomplete, &content[..40_000]).unwrap();
        download_file(
            &format!("http://127.0.0.1:{port}/file"),
            &sha,
            content.len() as u64,
            &dest,
        )
        .await
        .expect("resume download");
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            content,
            "续传后 dest 应 == 完整内容"
        );
        h.abort();
    }

    #[tokio::test]
    async fn download_rejects_sha_mismatch_and_deletes_incomplete() {
        let content = b"correct content".to_vec();
        let wrong_sha = "deadbeef".to_string(); // 故意错误
        let (port, h) = serve(Arc::new(content.clone())).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("bad.bin");
        let incomplete = dir.path().join("bad.bin.incomplete");
        let err = download_file(
            &format!("http://127.0.0.1:{port}/file"),
            &wrong_sha,
            content.len() as u64,
            &dest,
        )
        .await
        .expect_err("应 SHA 不符报错");
        assert!(
            matches!(err, DownloaderError::Sha256Mismatch { .. }),
            "应 Sha256Mismatch"
        );
        assert!(!incomplete.exists(), "不符应删 incomplete");
        assert!(!dest.exists(), "不符不应 rename 出 dest");
        h.abort();
    }

    #[tokio::test]
    async fn download_skips_when_dest_already_complete() {
        let content = b"already there".to_vec();
        let sha = sha256_hex(&content);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("skip.bin");
        std::fs::write(&dest, &content).unwrap();
        // 用一个不可达 URL（不应被请求——幂等跳过）
        download_file(
            "http://127.0.0.1:1/unreachable",
            &sha,
            content.len() as u64,
            &dest,
        )
        .await
        .expect("幂等跳过，不发请求");
        assert_eq!(std::fs::read(&dest).unwrap(), content);
    }

    #[tokio::test]
    async fn download_nested_path_creates_parent_dirs() {
        // manifest 有 example/、fig/ 等子路径——须建父目录否则 File::create 失败（review HIGH#1）
        let content = b"nested file content here".to_vec();
        let sha = sha256_hex(&content);
        let (port, h) = serve(Arc::new(content.clone())).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("sub/dir/file.bin");
        download_file(
            &format!("http://127.0.0.1:{port}/file"),
            &sha,
            content.len() as u64,
            &dest,
        )
        .await
        .expect("嵌套路径下载应建父目录");
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            content,
            "dest 内容应 == mock"
        );
        assert!(dest.exists(), "嵌套 dest 应存在");
        h.abort();
    }

    #[tokio::test]
    async fn download_skips_network_when_incomplete_already_full() {
        // 上次跑到 flush 后、rename 前被中断：incomplete 已满（size==expected，内容正确）→
        // 不发请求（否则 Range: bytes={size}- 触发 416），直接 SHA+rename（review HIGH#2）
        let content = b"full content already written".to_vec();
        let sha = sha256_hex(&content);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("full.bin");
        let incomplete = dir.path().join("full.bin.incomplete");
        std::fs::write(&incomplete, &content).unwrap();
        // 不可达 URL：不应被请求
        download_file(
            "http://127.0.0.1:1/unreachable",
            &sha,
            content.len() as u64,
            &dest,
        )
        .await
        .expect("已满 incomplete 应跳过网络直接校验+rename");
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            content,
            "dest 应 == 预写内容"
        );
        assert!(!incomplete.exists(), "incomplete 应已 rename 掉");
    }

    #[tokio::test]
    async fn download_component_iterates_files_with_progress() {
        let c1 = b"file1".to_vec();
        let c2 = b"file-two-content".to_vec();
        let total = (c1.len() + c2.len()) as u64;
        let content = Arc::new(c1.clone());
        let (port, h) = serve(content).await; // 同一 mock 路径返同样 bytes；用 sha 区分
        let comp = ManifestComponent {
            role: "punc".into(),
            model_id: "iic/test".into(),
            revision: "master".into(),
            files: vec![
                ManifestFile {
                    path: "a".into(),
                    size_bytes: c1.len() as u64,
                    sha256: sha256_hex(&c1),
                },
                ManifestFile {
                    path: "b".into(),
                    size_bytes: c2.len() as u64,
                    sha256: sha256_hex(&c2),
                },
            ],
        };
        let dir = tempfile::tempdir().unwrap();
        let progress = std::sync::Arc::new(move |done: u64, t: u64| {
            assert!(done <= t && t == total);
        }) as ProgressFn;
        // url_for 注入 mock URL（prod 传 modelscope_url 派生）；mock 只返 c1，b 的 sha=c2 → 不符
        let err = download_component(&comp, dir.path(), progress, move |_| {
            format!("http://127.0.0.1:{port}/file")
        })
        .await
        .expect_err("b 的 SHA 不符应报错");
        assert!(matches!(err, DownloaderError::Sha256Mismatch { .. }));
        assert!(dir.path().join("a").exists(), "a 已完成应保留");
        h.abort();
    }

    /// 启一个忽略 Range 的 mock（永远返 200 全量）。
    async fn serve_ignore_range(content: Arc<Vec<u8>>) -> (u16, tokio::task::JoinHandle<()>) {
        let app = Router::new().route(
            "/file",
            get(move |_: Request| {
                let content = content.clone();
                async move { (StatusCode::OK, Bytes::from(content.as_ref().clone())) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let h = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (port, h)
    }

    /// 启一个首请求 503、之后 200 的 mock（验瞬态重试）。
    async fn serve_fail_then_ok(
        content: Arc<Vec<u8>>,
        fail_first: Arc<std::sync::atomic::AtomicU32>,
    ) -> (u16, tokio::task::JoinHandle<()>) {
        let app = Router::new().route(
            "/file",
            get(move |_: Request| {
                let content = content.clone();
                let fail_first = fail_first.clone();
                async move {
                    if fail_first
                        .compare_exchange(
                            0,
                            1,
                            std::sync::atomic::Ordering::SeqCst,
                            std::sync::atomic::Ordering::SeqCst,
                        )
                        .is_ok()
                    {
                        return StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                    (StatusCode::OK, Bytes::from(content.as_ref().clone())).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let h = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (port, h)
    }

    /// 启一个记录请求数的 mock（验幂等 re-SHA 是否真发请求重下）。
    async fn serve_with_counter(
        content: Arc<Vec<u8>>,
        counter: Arc<std::sync::atomic::AtomicU32>,
    ) -> (u16, tokio::task::JoinHandle<()>) {
        let app = Router::new().route(
            "/file",
            get(move |_: Request| {
                let content = content.clone();
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    (StatusCode::OK, Bytes::from(content.as_ref().clone())).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let h = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (port, h)
    }

    /// per-chunk 进度：256KB 单文件经 download_component，进度回调应多次递增、
    /// 含中途（< total）与末尾（== total），非仅 per-file 一次（防大文件进度冻结）。
    #[tokio::test]
    async fn download_per_chunk_progress_reports_mid_stream() {
        let content: Vec<u8> = (0..262_144).map(|i| (i % 251) as u8).collect();
        let total = content.len() as u64;
        let sha = sha256_hex(&content);
        let (port, h) = serve(Arc::new(content.clone())).await;
        let comp = ManifestComponent {
            role: "punc".into(),
            model_id: "iic/test".into(),
            revision: "master".into(),
            files: vec![ManifestFile {
                path: "big.bin".into(),
                size_bytes: total,
                sha256: sha,
            }],
        };
        let samples = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(u64, u64)>::new()));
        let s = samples.clone();
        let progress = std::sync::Arc::new(move |done: u64, t: u64| {
            s.lock().unwrap().push((done, t));
        }) as ProgressFn;
        let dir = tempfile::tempdir().unwrap();
        download_component(&comp, dir.path(), progress, move |_| {
            format!("http://127.0.0.1:{port}/file")
        })
        .await
        .expect("download");
        let snap = samples.lock().unwrap().clone();
        assert!(
            snap.len() > 1,
            "per-chunk 应多次回调（实际 {}）",
            snap.len()
        );
        assert_eq!(snap.last().unwrap(), &(total, total), "末尾 == total");
        assert!(
            snap.iter().any(|(d, _)| *d < total),
            "应含中途 < total 的回调（非仅末尾一次）"
        );
        // 单调不减。
        for w in snap.windows(2) {
            assert!(w[0].0 <= w[1].0, "进度回调应单调不减");
        }
        assert_eq!(std::fs::read(dir.path().join("big.bin")).unwrap(), content);
        h.abort();
    }

    /// 200-truncate：预写 stale incomplete，mock 忽略 Range 返 200 全量 → dest == 全量，
    /// 不残留 stale 前缀（非 append 拼接）。
    #[tokio::test]
    async fn download_200_truncate_ignores_stale_incomplete() {
        let content = b"the full correct body".to_vec();
        let sha = sha256_hex(&content);
        let (port, h) = serve_ignore_range(Arc::new(content.clone())).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("trunc.bin");
        let incomplete = dir.path().join("trunc.bin.incomplete");
        std::fs::write(&incomplete, b"STALE").unwrap(); // 非完整、且 resume_from>0
        download_file(
            &format!("http://127.0.0.1:{port}/file"),
            &sha,
            content.len() as u64,
            &dest,
        )
        .await
        .expect("200 截断后应得正确内容");
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            content,
            "dest 应 == 全量，无 stale 残留"
        );
        assert!(!incomplete.exists(), "incomplete 应已 rename");
        h.abort();
    }

    /// 重试：首请求 503（Http，瞬态）→ 退避重试 → 第二请求 200 成功。验"+重试"链路。
    #[tokio::test]
    async fn download_retries_on_transient_then_succeeds() {
        let content = b"retry me to success".to_vec();
        let sha = sha256_hex(&content);
        let fail_first = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let (port, h) = serve_fail_then_ok(Arc::new(content.clone()), fail_first).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("retry.bin");
        download_file(
            &format!("http://127.0.0.1:{port}/file"),
            &sha,
            content.len() as u64,
            &dest,
        )
        .await
        .expect("首 503 后重试应成功");
        assert_eq!(std::fs::read(&dest).unwrap(), content);
        h.abort();
    }

    /// 幂等 re-SHA：dest 预写「size 对但内容错」→ 应流式 SHA 复校发现不符 → 删 dest 重下
    /// （而非仅 size-skip 误判成功）。
    #[tokio::test]
    async fn download_idempotent_rechecks_sha_redownloads_on_mismatch() {
        let correct = b"the genuine content".to_vec();
        let sha = sha256_hex(&correct);
        let counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let (port, h) = serve_with_counter(Arc::new(correct.clone()), counter.clone()).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("stale.bin");
        // 预写 size 对、内容错的 dest（防 size-skip 漏检）：同长度但首字节改 → SHA 不符。
        let mut stale = correct.clone();
        stale[0] = if correct[0] == b'X' { b'Y' } else { b'X' };
        std::fs::write(&dest, &stale).unwrap();
        assert_eq!(
            std::fs::metadata(&dest).unwrap().len(),
            correct.len() as u64
        );
        download_file(
            &format!("http://127.0.0.1:{port}/file"),
            &sha,
            correct.len() as u64,
            &dest,
        )
        .await
        .expect("re-SHA 不符应重下成功");
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            correct,
            "dest 应被正确内容覆盖"
        );
        assert!(
            counter.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "应发请求重下（非 size-skip）"
        );
        h.abort();
    }

    /// modelscope_url：query 值百分号编码非安全字符，保留 `/`（FilePath 分层路径）。
    #[test]
    fn modelscope_url_encodes_unsafe_chars() {
        let url = modelscope_url("iic/SenseVoice", "master", "example/fig a+b.bin");
        assert!(
            url.contains("Revision=master"),
            "安全 revision 不编码: {url}"
        );
        assert!(
            url.contains("FilePath=example/fig%20a%2Bb.bin"),
            "空格→%20、+→%2B，/保留: {url}"
        );
        // model_id 中的 / 不在 query 值，照常出现在路径段。
        assert!(
            url.contains("/models/iic/SenseVoice/repo"),
            "model_id 路径不编码: {url}"
        );
    }

    /// is_transient 分类：Http/Io/Timeout 瞬态可重试；SHA/Size/Cancelled 不可重试。
    #[test]
    fn is_transient_classifies_errors() {
        assert!(is_transient(&DownloaderError::Http("x".into())));
        assert!(is_transient(&DownloaderError::Io("x".into())));
        assert!(is_transient(&DownloaderError::Timeout("x".into())));
        assert!(!is_transient(&DownloaderError::Sha256Mismatch {
            expected: "a".into(),
            actual: "b".into(),
        }));
        assert!(!is_transient(&DownloaderError::SizeMismatch {
            expected: 1,
            actual: 2
        }));
        assert!(!is_transient(&DownloaderError::Cancelled));
    }
}
