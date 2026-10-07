//! 账户编排层错误（ST-M2.4）。
//!
//! 编排（registry / wrapped-DEK / token / Crypto 编排器）涉及 I/O、serde、crypto
//! 原语与 storage SQLCipher 多源失败，统一为本枚举。HTTP 边界（ST-M2.6）由
//! handler 将各 variant 映射到 [`crate::error::AppError`] 的状态码（注释标出预期映射）。

use std::io;

#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    /// 文件 I/O 失败。
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    /// `accounts.json` / `wrapped-dek` 序列化失败。
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
    /// 加密原语失败（AEAD 认证失败 / KDF 失败 / keychain 失败等）。
    #[error("crypto error: {0}")]
    Crypto(#[from] seasnail_crypto::Error),
    /// per-account SQLCipher 库失败（开库 / 迁移 / CRUD）。
    #[error("storage error: {0}")]
    Storage(#[from] seasnail_storage::Error),
    /// 已初始化（registry 已有账户），不可再 setup → 410 gone。
    #[error("already initialized: at least one account exists")]
    AlreadyInitialized,
    /// `account_id` 在 registry 中不存在 → 404 not_found。
    #[error("account not found: {0}")]
    AccountNotFound(String),
    /// 密码错误（unwrap wrapped-DEK AEAD 认证失败）→ 403 forbidden。
    #[error("wrong password")]
    WrongPassword,
    /// 当前无活跃已解锁账户，操作需先 unlock → 423 locked。
    #[error("no active unlocked account; unlock required")]
    NotUnlocked,
    /// 活跃账户 DEK 不在 keychain（keychain 丢失，需 unlock 经密码恢复）→ 423 locked。
    #[error("active account DEK missing from keychain; unlock required")]
    KeychainMissing,
    /// bearer 解析失败（无 `ss_live_` 前缀 / base64url 解码失败 / 长度不对）→ 401。
    #[error("invalid token: {0}")]
    InvalidToken(String),
    /// token 的 `account_id` 与目标资源 `account_id` 不符（跨账户）→ 403。
    #[error("cross-account access denied: token account {0}")]
    CrossAccount(String),
    /// caller 缺 `x-required-scope`（如非 root 调 `tokens:manage` 端点）→ 403。
    #[error("insufficient scope: {0}")]
    InsufficientScope(String),
    /// 签发第三方 token 时请求的 scope 不可授（非 root 授 write/manage、任何人授 is_root）→ 403。
    #[error("scope not grantable: {0}")]
    ScopeNotGrantable(String),
    /// 活跃账户 DB 内未找到 `SHA-256(secret)` 命中行（token 已撤销或不存在）→ 401。
    #[error("token not found")]
    TokenNotFound,
    /// 试图删除当前活跃账户（须先切走）→ 409。
    #[error("cannot delete active account: switch away first")]
    ActiveAccountDeletion,
    /// 仍有后台请求持有账户 lease，删除必须等待其释放。
    #[error("account has active leases")]
    ActiveLease,
    /// proto 编解码失败（transcript.pb.enc 反序列化错）→ 500。
    #[error("proto decode error: {0}")]
    Proto(String),
    #[error("clipboard context integrity error: {0}")]
    ContextIntegrity(String),
    /// cleanup.pb.enc 身份、schema、字段边界或 outcome 组合非法。
    #[error("cleanup artifact integrity error: {0}")]
    CleanupIntegrity(String),
}
