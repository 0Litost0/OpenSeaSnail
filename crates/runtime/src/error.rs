//! M3 运行时错误（ST-M3.1）。平行于 AccountError，仅 runtime crate 内部用。

use std::io;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    /// runtime 未 start 即调 transcribe。
    #[error("runtime not started")]
    NotStarted,
    /// sidecar health 轮询超时。
    #[error("sidecar health timeout")]
    HealthTimeout,
    /// sidecar 返回非 2xx。
    #[error("sidecar bad status: {0}")]
    BadStatus(reqwest::StatusCode),
    /// 响应解析失败。
    #[error("response decode error: {0}")]
    Decode(String),
    /// server 违反已锁定的 HTTP/verbose_json 契约。
    #[error("sidecar protocol error: {0}")]
    Protocol(String),
    #[error("sidecar listener identity could not be verified: {0}")]
    SidecarLost(String),
}

impl RuntimeError {
    /// GGUF sidecar 位于本机独立进程；请求/协议失败均可在保留会话数据后重试。
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Http(_)
                | Self::HealthTimeout
                | Self::BadStatus(_)
                | Self::Decode(_)
                | Self::Protocol(_)
                | Self::SidecarLost(_)
        )
    }
}
