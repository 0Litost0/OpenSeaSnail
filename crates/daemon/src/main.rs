//! SeaSnail 守护进程入口（纯数据面）。
//!
//! M1 阶段：init 日志基座（ST-M1.5）→ acquire 单例锁（ST-M1.3）→ bind loopback
//! → 写 bootstrap（ST-M1.2）→ 启动 HTTP 应用（ST-M1.4：内部 liveness `GET /`
//! + OpenAPI 面 `/api/v1/*`，trace_id 中间件注入）→ stdin EOF 父进程死亡检测
//! + SIGINT/SIGTERM 优雅关闭 + 退出清 bootstrap。

use seasnail_crypto::KeychainStore;
use seasnail_daemon::{
    bootstrap::Bootstrap,
    exit_code,
    launch::{self, LaunchConfig},
    singleton::AcquireError,
};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = LaunchConfig::production(Bootstrap::default_dir()?, build_keychain());
    if let Err(error) = launch::run(config, |_| Ok(())).await {
        if matches!(
            error.downcast_ref::<AcquireError>(),
            Some(AcquireError::AlreadyRunning)
        ) {
            std::process::exit(exit_code::ALREADY_RUNNING);
        }
        return Err(error);
    }
    Ok(())
}

const DEV_FILE_KEYCHAIN_ENV: &str = "SEASNAIL_DEV_FILE_KEYCHAIN";

/// 构造 keychain：macOS 默认走 Data Protection keychain（`MacKeychain::new`，需签名
/// `.app` entitlement）。仅未签名开发 App 的显式环境标记可选择普通文件 keychain；
/// 该模式不能用于分发或验证发行态的 Keychain 安全边界。
fn build_keychain() -> Arc<dyn KeychainStore> {
    #[cfg(target_os = "macos")]
    {
        if std::env::var(DEV_FILE_KEYCHAIN_ENV).as_deref() == Ok("1") {
            tracing::warn!(
                "开发模式：使用普通文件 Keychain；该模式不具备受保护 Keychain 的发行安全语义"
            );
            return Arc::new(seasnail_crypto::MacKeychain::with_protected(false));
        }
        Arc::new(seasnail_crypto::MacKeychain::new())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Arc::new(seasnail_crypto::MemoryKeychain::new())
    }
}
