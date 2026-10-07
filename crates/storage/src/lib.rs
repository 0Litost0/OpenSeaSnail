//! SeaSnail per-account 加密存储（ST-M2.2 上机）。
//!
//! 对应设计文档「存储结构设计」。per-account 一个 SQLCipher 库（`meta.db`），
//! 守护进程以 HKDF 派生的 K_sqlite[acct] 为 raw key 开库（整库透明加解密）。
//! 内容（音频 / 转译）存文件树 + K_files AEAD，不在本 crate。
//!
//! 本步为「上机」去风险：验证 `rusqlite` + 经典 SQLCipher raw key PRAGMA
//! `PRAGMA key = "x'<64 hex>'"` 可开库 / schema migrate / CRUD 跑通。完整 Storage
//! （文件树编排、reconcile、导出）属 ST-M2.7；账户/密钥编排（DEK→K_sqlite）属 ST-M2.4。
//!
//! 选型：`rusqlite` feature `bundled-sqlcipher-vendored-openssl`——构建期静态编译
//! SQLCipher amalgamation + OpenSSL libcrypto 进二进制，自包含、零运行时外部依赖，
//! 利于分发给其他 macOS 用户（无需用户安装 openssl / libcrypto / sqlcipher）。

pub mod db;
pub mod model;

pub use db::{migrate, open_db, Error};
pub use model::{
    begin_retry, checkpoint_raw, clear_context_present, complete_cleanup, count_dictionary_entries,
    create_provider_config, delete_dictionary_entries, delete_dictionary_entry,
    delete_dictionary_learning_event, delete_session, delete_token, disable_cleanup_if_selected,
    fail_transcription, get_cleanup_settings, get_dictionary_entry,
    get_dictionary_entry_by_normalized, get_provider_config, get_session, get_token,
    get_token_by_hash, insert_dictionary_entry, insert_session, insert_token,
    list_dictionary_entries, list_dictionary_entries_for_export,
    list_dictionary_entries_for_snapshot, list_provider_configs, list_sessions,
    list_sessions_after, list_tokens, normalize_provider_endpoint, promote_dictionary_entry,
    provider_endpoint_fingerprint, recover_interrupted_cleanup, remove_provider_config_and_disable,
    replace_provider_config, save_cleanup_settings, touch_token_last_used, update_dictionary_entry,
    update_session_outcome, CleanupCompletion, CleanupFailureCode, CleanupSettingsRow,
    DictionaryCursor, DictionaryEntryRow, ProviderConfigInput, ProviderType, RawCheckpointDecision,
    ReasoningProviderConfigRow, RecoveredCleanupOutcome, RecoveryExpectedState, SessionRow,
    TokenRow,
};
/// 重导出 rusqlite，供编排层（daemon）声明返回 `Connection` 的签名而无需 daemon
/// 直接依赖 rusqlite（依赖随 storage 的 SQLCipher feature 集）。
pub use rusqlite;
