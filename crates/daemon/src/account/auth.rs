//! `Auth` 鉴权编排层（ST-M2.5）。
//!
//! 账户鉴权编排层（见 `doc/architecture.md#accounts-and-storage`）：
//! token verify 三态分流（401/423/403）、scope 硬约束、`require_unlocked`（423）、
//! 跨账户判断（403）、token CRUD（issue/revoke/list）、账户生命周期
//! （setup/create/unlock/change_password/list/delete/is_initialized）——委托 [`Crypto`]，
//! Auth 为鉴权与会话唯一编排入口，handler 调 Auth 委托方法、不经 `crypto_arc` 穿透。
//!
//! **不引入 axum**——纯 Rust 编排，单测注入 `MemoryKeychain`+`tempfile` 跑通；HTTP
//! 端点落地、`AppState` 注入、`From<AccountError> for AppError` 映射、axum 提取器属 ST-M2.6。
//!
//! ### verify 五步流程（设计 `:706-712`）
//!
//! 1. `parse_bearer` → `account_id` + `secret`；失败 → `InvalidToken`（401）。
//! 2. `account_id` 不在 registry → `AccountNotFound`（401，不泄露存在性）。
//! 3. `account_id` 非活跃（DEK 不在内存）→ `Locked`（423）。
//! 4. 活跃账户开库查 `tokens.token_hash = SHA-256(secret)`；命中 → `Caller` + 刷 last_used；
//!    不命中 → `TokenNotFound`（401）。
//! 5. 跨账户（`Caller.account_id ≠ 目标`）→ `CrossAccount`（403），由 [`check_target_account]` 封装。
//!
//! 步 1-3 查内存零 DB I/O；步 4 仅对活跃账户开库（非活跃在步 3 直接 423）。

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use seasnail_storage::{
    delete_token, get_token_by_hash, insert_token, list_tokens, touch_token_last_used, TokenRow,
};

use super::crypto::{AccountSummary, Crypto};
use super::error::AccountError;
use super::token::{generate_token, hash_secret, parse_bearer, IssuedToken};

/// 鉴权后的调用方。由 `verify` 产出，handler（ST-M2.6）消费。
#[derive(Debug, Clone)]
pub struct Caller {
    /// token 内嵌的 account_id（亦即 token 所属账户）。
    pub account_id: String,
    /// 是否 root token（持全 scope）。
    pub is_root: bool,
    /// token 持有的 scope 集合。
    pub scopes: HashSet<String>,
    /// token id（UUID 字符串）。
    pub token_id: String,
}

/// 鉴权编排层。委托 [`Crypto`]，持 `Arc<Crypto>` 共享编排态。
///
/// 生产由 `AppState = Arc<Auth>` 注入 axum（ST-M2.6）；测录用 `MemoryKeychain`+`tempfile`
/// 构造 `Crypto` 后包成 `Auth`。
pub struct Auth {
    crypto: Arc<Crypto>,
}

impl Auth {
    /// 由 `Crypto` 构造鉴权层。`Auth` 共享同一份编排态（registry 内存缓存 + 活跃账户）。
    pub fn new(crypto: Arc<Crypto>) -> Self {
        Self { crypto }
    }

    /// 底层 `Crypto` 的 `Arc` 克隆（供 `AppState` 构造 `Storage` 数据面：
    /// `Storage::new(crypto_arc)`，与 `Auth` 共享同一份编排态）。handler 不经此穿透
    /// 调账户生命周期——改用下方 `Auth` 暴露的委托方法。
    pub(crate) fn crypto_arc(&self) -> Arc<Crypto> {
        self.crypto.clone()
    }

    pub(crate) fn coordination(&self) -> Arc<super::coordination::IdentityCoordination> {
        self.crypto.coordination()
    }

    // ── 账户生命周期（委托 `Crypto`，Auth 为唯一编排入口，handler 不穿透）──────────

    /// 是否已初始化（registry 有任何账户）。
    pub fn is_initialized(&self) -> bool {
        self.crypto.is_initialized()
    }

    /// 当前活跃账户 id（已解锁）。
    pub fn active_account_id(&self) -> Option<String> {
        self.crypto.active_account_id()
    }

    pub(crate) fn logout(&self) -> Result<(), AccountError> {
        self.crypto.logout()
    }

    /// 首次建账户（仅未初始化）。返回该账户 root token（明文 secret 仅一次）。
    pub fn setup_first_account(
        &self,
        username: &str,
        password: &str,
    ) -> Result<IssuedToken, AccountError> {
        self.crypto.setup_first_account(username, password)
    }

    /// 追加账户（授权由 handler 经 `require_scope` 校验）。切换活跃至新账户。
    pub fn create_account(
        &self,
        username: &str,
        password: &str,
    ) -> Result<IssuedToken, AccountError> {
        self.crypto.create_account(username, password)
    }

    /// 切换/解锁账户：密码 → unwrap 得 DEK → 入 keychain 作活跃 + 签发 root token。
    pub fn unlock_account(
        &self,
        account_id: &str,
        password: &str,
    ) -> Result<IssuedToken, AccountError> {
        self.crypto.unlock_account(account_id, password)
    }

    /// 改密：校验当前 → 新 KEK 重 wrap（master_DEK / 子密钥 / 历史 / token 均不变）。
    pub fn change_password(
        &self,
        account_id: &str,
        current: &str,
        new: &str,
    ) -> Result<(), AccountError> {
        self.crypto.change_password(account_id, current, new)
    }

    /// 列全部账户摘要。
    pub fn list_accounts(&self) -> Vec<AccountSummary> {
        self.crypto.list_accounts()
    }

    /// 删除账户（活跃账户不可直接删）。授权由 handler 经 `require_scope` 校验。
    pub(crate) fn delete_account(&self, account_id: &str) -> Result<(), AccountError> {
        self.crypto.delete_account(account_id)
    }

    /// 校验密码（unwrap wrapped-DEK）。导出门禁 / 改密 / unlock 复用，不触碰 keychain。
    pub fn verify_password(
        &self,
        account_id: &str,
        password: &str,
    ) -> Result<[u8; 32], AccountError> {
        self.crypto.verify_password(account_id, password)
    }

    /// 活跃账户 root token bearer（从 keychain 恢复，供 GUI 日常免密）。须已解锁。
    pub fn active_root_token_bearer(&self) -> Result<Option<String>, AccountError> {
        self.crypto.active_root_token_bearer()
    }

    // ── verify 五步流程 ─────────────────────────────────────────────────────────

    /// 校验 bearer → `Caller`。五步流程见模块文档。
    pub fn verify(&self, bearer: &str) -> Result<Caller, AccountError> {
        // 1. 解析 bearer。
        let parsed = parse_bearer(bearer)?;
        // 2. account_id 须在 registry。
        if !self.crypto.is_known_account(&parsed.account_id) {
            return Err(AccountError::AccountNotFound(parsed.account_id));
        }
        // 3. 须为活跃已解锁账户。
        if !self.crypto.is_active_unlocked(&parsed.account_id) {
            return Err(AccountError::NotUnlocked);
        }
        // 4. 活跃账户开库查 token_hash。
        let token_hash = hash_secret(&parsed.secret);
        let conn = self.crypto.open_active_db()?;
        let row = get_token_by_hash(&conn, &token_hash)?.ok_or(AccountError::TokenNotFound)?;
        // 命中：刷 last_used。
        touch_token_last_used(&conn, &row.id, now_secs())?;

        Ok(Caller {
            account_id: parsed.account_id,
            is_root: row.is_root,
            scopes: row.scopes.iter().cloned().collect(),
            token_id: row.id,
        })
    }

    // ── LockGuard / 跨账户 ───────────────────────────────────────────────────────

    /// 数据访问前置：目标账户须已解锁（DEK 在内存），否则 `NotUnlocked`（423）。
    pub fn require_unlocked(&self, account_id: &str) -> Result<(), AccountError> {
        if !self.crypto.is_active_unlocked(account_id) {
            return Err(AccountError::NotUnlocked);
        }
        Ok(())
    }

    // 跨账户判断见自由函数 [`check_target_account`]。

    // ── token CRUD ──────────────────────────────────────────────────────────────

    /// 签发第三方 token（绑 caller.account_id）。
    ///
    /// caller 须持 `tokens:manage`（仅 root 持有，等价「须 root」）；`requested` 须经
    /// [`enforce_grantable`]（非 root 不可授 write/manage、任何人不可授 is_root）。
    /// 明文 secret 仅本次返回。
    pub fn issue_token(
        &self,
        caller: &Caller,
        name: &str,
        scopes: Vec<String>,
    ) -> Result<IssuedToken, AccountError> {
        // 授权：须 tokens:manage（仅 root 持）。
        if !caller.scopes.contains("tokens:manage") {
            return Err(AccountError::InsufficientScope("tokens:manage".into()));
        }
        // 硬约束：请求的 scope 须可授。
        enforce_grantable(caller, &scopes)?;
        // 第三方 token 非 root（is_root=false）；绑 caller.account_id。
        let created_at = now_secs();
        let token = generate_token(&caller.account_id, name, false, scopes, created_at)?;
        let conn = self.crypto.open_active_db()?;
        let row = TokenRow {
            id: token.id.clone(),
            account_id: token.account_id.clone(),
            name: token.name.clone(),
            prefix: token.prefix.clone(),
            token_hash: token.token_hash.clone(),
            is_root: token.is_root,
            scopes: token.scopes.clone(),
            created_at: token.created_at,
            last_used_at: None,
        };
        insert_token(&conn, &row)?;
        Ok(token)
    }

    /// 吊销 token（硬删 DB 行）。caller 须持 `tokens:manage`。撤销即失效。
    /// 返回是否删到行（false → `TokenNotFound`）。
    pub fn revoke_token(&self, caller: &Caller, token_id: &str) -> Result<(), AccountError> {
        if !caller.scopes.contains("tokens:manage") {
            return Err(AccountError::InsufficientScope("tokens:manage".into()));
        }
        let conn = self.crypto.open_active_db()?;
        let deleted = delete_token(&conn, token_id)?;
        if !deleted {
            return Err(AccountError::TokenNotFound);
        }
        Ok(())
    }

    /// 列 caller 账户的全部 token（按 created_at DESC）。caller 须持 `tokens:manage`（与
    /// openapi `GET /tokens` `x-required-scope: tokens:manage` 一致；列 token 属管理操作）。
    pub fn list_tokens(&self, caller: &Caller) -> Result<Vec<TokenRow>, AccountError> {
        if !caller.scopes.contains("tokens:manage") {
            return Err(AccountError::InsufficientScope("tokens:manage".into()));
        }
        let conn = self.crypto.open_active_db()?;
        Ok(list_tokens(&conn, &caller.account_id)?)
    }
}

// ── ScopeGuard ──────────────────────────────────────────────────────────────────

/// 跨账户判断：token 的 `account_id` 须等于目标资源 `account_id`，否则 `CrossAccount`（403）。
/// token 内嵌 account_id 与 row.account_id 一致（签发时写入），故直接比 caller.account_id。
pub fn check_target_account(caller: &Caller, target_account_id: &str) -> Result<(), AccountError> {
    if caller.account_id != target_account_id {
        return Err(AccountError::CrossAccount(caller.account_id.clone()));
    }
    Ok(())
}

/// 签发第三方 token 时的 scope 硬约束（设计 `:681-683`、`openapi.yaml:21`）。
///
/// - 任何人**不可授** `is_root`（is_root 不是 scope 但请求里出现该字串须拒）。
/// - **非 root 不可授** `sessions:write` / `tokens:manage`。
/// - root 调用方签发第三方 token **也拒授** write/manage（第三方仅可得
///   `sessions:read` / `sessions:delete`）。
pub fn enforce_grantable(caller: &Caller, requested: &[String]) -> Result<(), AccountError> {
    for s in requested {
        if s == "is_root" {
            return Err(AccountError::ScopeNotGrantable(
                "is_root not grantable".into(),
            ));
        }
        if !caller.is_root && (s == "sessions:write" || s == "tokens:manage") {
            return Err(AccountError::ScopeNotGrantable(format!(
                "non-root cannot grant {s}"
            )));
        }
        // root 调用方亦不可把 write/manage 授予第三方 token。
        if caller.is_root && (s == "sessions:write" || s == "tokens:manage") {
            return Err(AccountError::ScopeNotGrantable(format!(
                "{s} not grantable to third-party tokens"
            )));
        }
    }
    Ok(())
}

/// 当前 epoch 秒。
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::crypto::Crypto;
    use seasnail_crypto::{Argon2Params, KeychainStore, MemoryKeychain};
    use uuid::Uuid;

    /// 测试用小 Argon2 参数（快）。
    fn fast_params() -> Argon2Params {
        Argon2Params {
            m_kib: 8192,
            t_cost: 1,
            p_cost: 1,
        }
    }

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    /// 构造 Auth：建一个账户并返回 (Auth, 活跃账户 root token bearer, account_id)。
    fn auth_with_first_account(
        dir: &tempfile::TempDir,
        kc: Arc<dyn KeychainStore>,
    ) -> (Auth, String, String) {
        let crypto = Arc::new(Crypto::new(dir.path().to_path_buf(), kc, fast_params()).unwrap());
        let auth = Auth::new(crypto);
        let t = auth.setup_first_account("alice", "p@ss").unwrap();
        (auth, t.secret_bearer, t.account_id)
    }

    fn caller_root(account_id: &str) -> Caller {
        Caller {
            account_id: account_id.to_string(),
            is_root: true,
            scopes: [
                "sessions:read",
                "sessions:write",
                "sessions:delete",
                "tokens:manage",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            token_id: Uuid::new_v4().to_string(),
        }
    }

    // ── verify 三态 ─────────────────────────────────────────────────────────────

    /// 活跃账户 root token verify 命中。
    #[test]
    fn verify_active_root_token_succeeds() {
        let dir = tmpdir();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let (auth, bearer, account_id) = auth_with_first_account(&dir, kc);
        let caller = auth.verify(&bearer).unwrap();
        assert!(caller.is_root);
        assert_eq!(caller.account_id, account_id);
        assert!(caller.scopes.contains("sessions:write"));
    }

    /// 无前缀 / 非法 base64 / 长度不对 → InvalidToken（401）。
    #[test]
    fn verify_invalid_token_errors() {
        let dir = tmpdir();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let (auth, _, _) = auth_with_first_account(&dir, kc);
        for bad in &["live_abcd", "ss_live_!!!bad!!!", "ss_live_short"] {
            match auth.verify(bad) {
                Err(AccountError::InvalidToken(_)) => {}
                other => panic!("verify({bad:?}) 期望 InvalidToken，实际 {other:?}"),
            }
        }
    }

    /// account_id 不在 registry → AccountNotFound（401，不泄露存在性）。
    #[test]
    fn verify_unknown_account_errors() {
        let dir = tmpdir();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let (auth, _, _) = auth_with_first_account(&dir, kc);
        // 构造一个合法 bearer，但 account_id 指向不存在的 UUID。
        let ghost = Uuid::new_v4().to_string();
        let t = super::generate_token(&ghost, "n", true, vec![], 0).unwrap();
        match auth.verify(&t.secret_bearer) {
            Err(AccountError::AccountNotFound(_)) => {}
            other => panic!("期望 AccountNotFound，实际 {other:?}"),
        }
    }

    /// account_id 在 registry 但非活跃（切到另一账户）→ Locked（423）。
    #[test]
    fn verify_inactive_account_locked() {
        let dir = tmpdir();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let (auth, bearer_a, id_a) = auth_with_first_account(&dir, kc.clone());
        // 追加 bob，bob 活跃，alice 非活跃。
        auth.create_account("bob", "p2").unwrap();
        match auth.verify(&bearer_a) {
            Err(AccountError::NotUnlocked) => {}
            other => panic!("期望 NotUnlocked（alice 非活跃），实际 {other:?}"),
        }
        // bob 的 root token 应能 verify（活跃）。
        let _ = id_a;
        let bob_bearer = auth.active_root_token_bearer().unwrap().unwrap();
        assert!(auth.verify(&bob_bearer).is_ok());
    }

    /// token 已撤销 → TokenNotFound（401）。
    #[test]
    fn verify_revoked_token_errors() {
        let dir = tmpdir();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let (auth, bearer, account_id) = auth_with_first_account(&dir, kc);
        let caller = auth.verify(&bearer).unwrap();
        // 撤销该 root token。
        auth.revoke_token(&caller, &caller.token_id).unwrap();
        match auth.verify(&bearer) {
            Err(AccountError::TokenNotFound) => {}
            other => panic!("期望 TokenNotFound（已撤销），实际 {other:?}"),
        }
        let _ = account_id;
    }

    // ── 跨账户 / require_unlocked ────────────────────────────────────────────────

    /// check_target_account：同账户 Ok，跨账户 CrossAccount（403）。
    #[test]
    fn check_target_account_cross_account() {
        let dir = tmpdir();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let (auth, _, account_id) = auth_with_first_account(&dir, kc);
        let caller = auth
            .verify(&auth.active_root_token_bearer().unwrap().unwrap())
            .unwrap();
        assert!(check_target_account(&caller, &account_id).is_ok());
        let other = Uuid::new_v4().to_string();
        match check_target_account(&caller, &other) {
            Err(AccountError::CrossAccount(_)) => {}
            other => panic!("期望 CrossAccount，实际 {other:?}"),
        }
    }

    /// require_unlocked：活跃账户 Ok，非活跃 Locked。
    #[test]
    fn require_unlocked_active_vs_inactive() {
        let dir = tmpdir();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let (auth, _, id_a) = auth_with_first_account(&dir, kc);
        let t_bob = auth.create_account("bob", "p2").unwrap();
        // bob 活跃。
        assert!(auth.require_unlocked(&t_bob.account_id).is_ok());
        // alice 非活跃。
        match auth.require_unlocked(&id_a) {
            Err(AccountError::NotUnlocked) => {}
            other => panic!("期望 NotUnlocked，实际 {other:?}"),
        }
    }

    // ── scope 硬约束 ─────────────────────────────────────────────────────────────

    /// enforce_grantable：root 与非 root 的可授集合。
    #[test]
    fn enforce_grantable_rules() {
        let root = caller_root("acct");
        let nonroot = Caller {
            account_id: "acct".into(),
            is_root: false,
            scopes: ["sessions:read", "sessions:delete"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            token_id: "t".into(),
        };

        // 任何人不可授 is_root。
        assert!(matches!(
            enforce_grantable(&root, &["is_root".into()]),
            Err(AccountError::ScopeNotGrantable(_))
        ));
        // 非 root 不可授 sessions:write / tokens:manage。
        assert!(matches!(
            enforce_grantable(&nonroot, &["sessions:write".into()]),
            Err(AccountError::ScopeNotGrantable(_))
        ));
        assert!(matches!(
            enforce_grantable(&nonroot, &["tokens:manage".into()]),
            Err(AccountError::ScopeNotGrantable(_))
        ));
        // root 也不可把 write/manage 授给第三方 token。
        assert!(matches!(
            enforce_grantable(&root, &["sessions:write".into()]),
            Err(AccountError::ScopeNotGrantable(_))
        ));
        assert!(matches!(
            enforce_grantable(&root, &["tokens:manage".into()]),
            Err(AccountError::ScopeNotGrantable(_))
        ));
        // 可授：sessions:read / sessions:delete（任何 caller）。
        assert!(enforce_grantable(&root, &["sessions:read".into()]).is_ok());
        assert!(enforce_grantable(
            &nonroot,
            &["sessions:read".into(), "sessions:delete".into()]
        )
        .is_ok());
    }

    /// issue_token：root 可签发第三方 token（read scope），第三方非 root。
    #[test]
    fn issue_third_party_token() {
        let dir = tmpdir();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let (auth, _, account_id) = auth_with_first_account(&dir, kc);
        let caller = auth
            .verify(&auth.active_root_token_bearer().unwrap().unwrap())
            .unwrap();

        let t = auth
            .issue_token(&caller, "ci", vec!["sessions:read".into()])
            .unwrap();
        assert!(!t.is_root, "第三方 token 非 root");
        assert_eq!(t.account_id, account_id);
        assert!(t.secret_bearer.starts_with("ss_live_"));

        // 第三方 token 可 verify（活跃账户、scope=read）。
        let c2 = auth.verify(&t.secret_bearer).unwrap();
        assert!(!c2.is_root);
        assert!(c2.scopes.contains("sessions:read"));
        assert!(!c2.scopes.contains("sessions:write"));
    }

    /// issue_token：非 root caller（无 tokens:manage）→ InsufficientScope。
    #[test]
    fn issue_token_non_root_rejected() {
        let dir = tmpdir();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let (auth, _, _) = auth_with_first_account(&dir, kc);
        // 先签一个第三方 token，用它（非 root、无 tokens:manage）去再签发。
        let root_caller = auth
            .verify(&auth.active_root_token_bearer().unwrap().unwrap())
            .unwrap();
        let third = auth
            .issue_token(&root_caller, "ci", vec!["sessions:read".into()])
            .unwrap();
        let third_caller = auth.verify(&third.secret_bearer).unwrap();
        match auth.issue_token(&third_caller, "x", vec!["sessions:read".into()]) {
            Err(AccountError::InsufficientScope(_)) => {}
            other => panic!("期望 InsufficientScope，实际 {other:?}"),
        }
    }

    /// issue_token：root 请求 write scope 给第三方 → ScopeNotGrantable。
    #[test]
    fn issue_token_write_to_third_party_rejected() {
        let dir = tmpdir();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let (auth, _, _) = auth_with_first_account(&dir, kc);
        let caller = auth
            .verify(&auth.active_root_token_bearer().unwrap().unwrap())
            .unwrap();
        match auth.issue_token(&caller, "ci", vec!["sessions:write".into()]) {
            Err(AccountError::ScopeNotGrantable(_)) => {}
            other => panic!("期望 ScopeNotGrantable，实际 {other:?}"),
        }
    }

    /// revoke_token：删到→Ok；删不存在的 token→TokenNotFound。
    #[test]
    fn revoke_token_behaviour() {
        let dir = tmpdir();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let (auth, _, _) = auth_with_first_account(&dir, kc);
        let caller = auth
            .verify(&auth.active_root_token_bearer().unwrap().unwrap())
            .unwrap();
        let t = auth
            .issue_token(&caller, "ci", vec!["sessions:read".into()])
            .unwrap();

        // 删存在 → Ok。
        auth.revoke_token(&caller, &t.id).unwrap();
        // 再删 → TokenNotFound。
        match auth.revoke_token(&caller, &t.id) {
            Err(AccountError::TokenNotFound) => {}
            other => panic!("期望 TokenNotFound，实际 {other:?}"),
        }
    }

    /// list_tokens：只列本账户 token。
    #[test]
    fn list_tokens_scoped() {
        let dir = tmpdir();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let (auth, _, _) = auth_with_first_account(&dir, kc);
        let caller = auth
            .verify(&auth.active_root_token_bearer().unwrap().unwrap())
            .unwrap();
        // root token + 2 个第三方。
        auth.issue_token(&caller, "a", vec!["sessions:read".into()])
            .unwrap();
        auth.issue_token(&caller, "b", vec!["sessions:delete".into()])
            .unwrap();

        let list = auth.list_tokens(&caller).unwrap();
        // root + 2 = 3。
        assert_eq!(list.len(), 3);
        assert!(list.iter().any(|t| t.is_root));
    }

    // ── delete_account ─────────────────────────────────────────────────────────

    /// 删除活跃账户 → ActiveAccountDeletion（409）。
    #[test]
    fn delete_active_account_rejected() {
        let dir = tmpdir();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let (auth, _, account_id) = auth_with_first_account(&dir, kc);
        match auth.delete_account(&account_id) {
            Err(AccountError::ActiveAccountDeletion) => {}
            other => panic!("期望 ActiveAccountDeletion，实际 {other:?}"),
        }
    }

    /// 删除非活跃账户：目录/keychain/registry 清理。
    #[test]
    fn delete_inactive_account_cleans_up() {
        let dir = tmpdir();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let (auth, _, _id_a) = auth_with_first_account(&dir, kc.clone());
        let t_bob = auth.create_account("bob", "p2").unwrap();
        // bob 活跃，alice 非活跃；删 alice。
        let alice_id = auth
            .list_accounts()
            .iter()
            .find(|a| a.username == "alice")
            .map(|a| a.id.clone())
            .unwrap();
        auth.delete_account(&alice_id).unwrap();

        // alice 目录已删。
        assert!(!dir.path().join("data").join(&alice_id).exists());
        // registry 无 alice。
        assert!(auth.list_accounts().iter().all(|a| a.id != alice_id));
        // bob 仍活跃、registry 仍在。
        assert_eq!(auth.active_account_id(), Some(t_bob.account_id));
        assert_eq!(auth.list_accounts().len(), 1);
    }

    /// 删除未知账户 → AccountNotFound。
    #[test]
    fn delete_unknown_account_errors() {
        let dir = tmpdir();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let (auth, _, _) = auth_with_first_account(&dir, kc);
        let ghost = Uuid::new_v4().to_string();
        match auth.delete_account(&ghost) {
            Err(AccountError::AccountNotFound(_)) => {}
            other => panic!("期望 AccountNotFound，实际 {other:?}"),
        }
    }

    // ── ROOT_SCOPES 完整性 ───────────────────────────────────────────────────────

    /// root token 持 sessions:delete（验证补缺修复）。
    #[test]
    fn root_token_has_sessions_delete() {
        let dir = tmpdir();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let (auth, _, _) = auth_with_first_account(&dir, kc);
        let caller = auth
            .verify(&auth.active_root_token_bearer().unwrap().unwrap())
            .unwrap();
        assert!(
            caller.scopes.contains("sessions:delete"),
            "root 应持 sessions:delete"
        );
        assert!(caller.scopes.contains("sessions:write"));
        assert!(caller.scopes.contains("tokens:manage"));
    }
}
