//! 账户编排模块（ST-M2.4）：`Crypto` 全实现 + 全局 registry + wrapped-DEK + token。
//!
//! 串联 [`seasnail_crypto`]（原语）/ [`seasnail_storage`]（SQLCipher）/ keychain
//! （`KeychainStore`）落实设计文档「加解密模块」「存储结构设计」「鉴权与会话模块」
//! 的账户生命周期编排：setup_first_account / create_account / unlock_account /
//! verify_password / change_password / derive_subkeys / open_db。
//!
//! 子模块：
//! - [`registry`]：`data/accounts.json` 全局账户清单（非加密 0o600、原子写 + 内存缓存）。
//! - [`wrapped_dek`]：wrapped-DEK 文件格式（JSON + base64，KDF 参数随件）。
//! - [`token`]：root token 生成/解析（`ss_live_` + base64url，内嵌 account_id）。
//! - [`crypto`]：`Crypto` 编排器，账户生命周期唯一入口。
//!
//! HTTP 端点（`POST /auth/setup` 等）落地属 ST-M2.6；鉴权 verify 三态分流属 ST-M2.5。

pub mod auth;
pub(crate) mod coordination;
pub mod crypto;
pub mod error;
pub mod reasoning;
pub mod registry;
pub mod storage;
pub mod token;
pub mod wrapped_dek;

pub use auth::{check_target_account, enforce_grantable, Auth, Caller};
pub use crypto::{AccountSummary, Crypto};
pub use error::AccountError;
pub use reasoning::{CleanupSettingsView, CredentialState, ProviderConfigView};
pub use registry::{AccountRecord, Registry};
pub use storage::{ReconcileReport, SearchHit, Storage};
pub use token::{IssuedToken, ROOT_SCOPES, ROOT_TOKEN_NAME, TOKEN_PREFIX};
pub use wrapped_dek::WrappedDek;
