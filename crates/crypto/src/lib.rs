//! SeaSnail 加密原语封装（ST-M2.1）。
//!
//! 对应设计文档「加密与密钥设计」与「加解密模块」。per-account 密钥层级：
//! master_DEK[acct]（32B 随机，存 keychain）→ HKDF-SHA256 域隔离派生 K_sqlite / K_files；
//! 密码 → Argon2id 派生 KEK → AEAD 包裹 master_DEK 得 wrapped-DEK 落盘。
//!
//! 本 crate 仅**原语层**（HKDF / AEAD / Argon2id）；账户/存储编排（setup / unlock /
//! derive_subkeys 编排等）属 ST-M2.4 `Crypto` 全实现，不在本步。
//!
//! **不可变常量**：HKDF info 串 `seasnail/sqlite/v1` / `seasnail/files/v1` 一经发布
//! 即不可改（改=解不出历史库与文件）；账户隔离靠各账户独立 DEK，不靠 info 嵌入
//! account_id，故 info 全局恒定。升级靠 info 版本号 `v1→v2` + 迁移脚本（ST-M2.4+）。

pub mod aead;
pub mod hkdf;
pub mod kdf;
pub mod keychain;
pub mod provider_credentials;

/// 加密原语统一错误。AEAD 解密失败 / Argon2 失败映射为此枚举，调用方据此判分支。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 密文短于 `nonce(12) ‖ tag(16)` 最小长度。
    #[error("ciphertext too short")]
    CiphertextTooShort,
    /// AEAD 认证失败：密钥错或密文被篡改。
    #[error("authentication failed (wrong key or tampered ciphertext)")]
    AuthenticationFailed,
    /// 解包后明文长度非 32B（master_DEK 应为 32B）。
    #[error("decrypted DEK length is not 32 bytes")]
    InvalidDekLength,
    /// HKDF expand 失败（OKM 长度超限；32B 下不可达，防御性保留）。
    #[error("hkdf expansion failed")]
    HkdfFailed,
    /// Argon2id 派生失败（参数非法 / 内存不足）。
    #[error("argon2 key derivation failed")]
    KdfFailed,
    /// keychain 存取失败（非"项不存在"的 OS 错误）。
    #[error("keychain error: {0}")]
    Keychain(String),
    /// Provider credential vault 的版本、结构或容量不合法。
    #[error(transparent)]
    CredentialVault(#[from] provider_credentials::CredentialVaultError),
}

pub use aead::{
    decrypt_file, decrypt_file_with_aad, encrypt_file, encrypt_file_with_aad, unwrap_dek, wrap_dek,
};
pub use hkdf::{derive_k_files, derive_k_sqlite};
pub use kdf::{derive_kek, generate_salt, random_master_dek, Argon2Params};
#[cfg(target_os = "macos")]
pub use keychain::MacKeychain;
pub use keychain::{KeychainLabel, KeychainStore, MemoryKeychain, SERVICE};
pub use provider_credentials::{
    CredentialEnvelope, CredentialSecret, CredentialVaultError, KeychainProviderCredentialStore,
    ProviderCredentialStore,
};
