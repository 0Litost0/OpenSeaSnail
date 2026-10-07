//! `Crypto` 编排器：账户生命周期全编排（ST-M2.4）。
//!
//! 串联加密原语（ST-M2.1）/ SQLCipher 存储（ST-M2.2）/ keychain（ST-M2.3），
//! 落实设计文档「加解密模块」数据流：
//!
//! - `setup_first_account`：密码→KEK→随机 master_DEK→wrap 落盘 + DEK 存 keychain（活跃）
//!   + 签发该账户 root token → 记 `last_active_account_id`。
//! - `create_account`：独立密码/DEK，切换活跃至新账户（旧活跃 keychain 移出）。
//! - `unlock_account`：密码→KEK→unwrap 得 DEK→入 keychain 作活跃 + 签发 root token。
//! - `verify_password`：unwrap wrapped-DEK（密码恢复路径）。
//! - `change_password`：校验当前 → 新 KEK 重 wrap（DEK/历史不变）。
//!
//! 活跃账户**唯一**：keychain 同一时刻仅缓存一个活跃账户的 DEK + root token bearer；
//! 切账户 = 移出旧活跃项 + 入新活跃项。非活跃账户 DEK 不在 keychain，访问须先 unlock。
//!
//! 持久化顺序（crash-safety）：wrapped-dek 文件 → keychain 项 → registry（最后）。
//! registry 为账户存在性的事实源——其前置依赖先落盘，崩溃至多留孤儿 wrapped-dek
//! （ST-M2.7 reconcile 清），不会出现 registry 有账户但密钥材料缺失的"半账户"。
//!
//! 编排经 [`KeychainStore`] trait 注入实现：生产用 `MacKeychain`、测录用 `MemoryKeychain`，
//! 不碰真实系统 keychain。`Mutex<State>` 串行 registry 读改写（进程内 CAS），配合 flock
//! 单例 + registry 原子写三层防竞态。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use seasnail_crypto::{
    derive_k_files, derive_k_sqlite, derive_kek, generate_salt, random_master_dek, unwrap_dek,
    wrap_dek, Argon2Params, KeychainLabel, KeychainProviderCredentialStore, KeychainStore,
    ProviderCredentialStore,
};
use seasnail_storage::{insert_token, open_db, TokenRow};
use uuid::Uuid;

use super::error::AccountError;
use super::registry::{AccountRecord, Registry};
use super::token::{generate_token, IssuedToken, ROOT_TOKEN_NAME};
use super::wrapped_dek::WrappedDek;

/// 账户摘要（`GET /accounts`）。date-time↔epoch 换算属 ST-M2.6。
#[derive(Debug, Clone)]
pub struct AccountSummary {
    pub id: String,
    pub username: String,
    pub created_at: i64,
    /// 是否当前活跃账户（DEK 在 keychain / 内存）。
    pub is_active: bool,
}

/// 已解锁的活跃账户（DEK 在内存）。keychain 作持久缓存，此为运行态副本。
struct ActiveAccount {
    account_id: String,
    master_dek: [u8; 32],
    k_sqlite: [u8; 32],
    k_files: [u8; 32],
    /// 活跃账户 root token bearer（从 keychain 恢复，供 GUI 日常免密）。
    root_token_bearer: Option<String>,
}

/// 固定到账户的数据访问材料。只在 daemon crate 内传给 account-scoped repository；
/// 不向 HTTP/application 公共接口暴露原始密钥。
#[derive(Clone)]
pub(crate) struct AccountDataKeys {
    pub(crate) account_id: String,
    pub(crate) account_dir: PathBuf,
    pub(crate) k_sqlite: [u8; 32],
    pub(crate) k_files: [u8; 32],
}

/// 编排器运行态（registry 内存缓存 + 活跃账户）。
struct State {
    registry: Registry,
    active: Option<ActiveAccount>,
}

/// `Crypto` 编排器。账户/密钥/存储编排的唯一入口。
pub struct Crypto {
    data_dir: PathBuf,
    keychain: Arc<dyn KeychainStore>,
    provider_credentials: Arc<KeychainProviderCredentialStore>,
    reasoning_lock: Mutex<()>,
    params: Argon2Params,
    state: Mutex<State>,
    coordination: Arc<super::coordination::IdentityCoordination>,
}

impl Crypto {
    /// 构造：载入 registry 内存副本，并尝试从 keychain 恢复活跃账户（免密重开）。
    pub fn new(
        data_dir: PathBuf,
        keychain: Arc<dyn KeychainStore>,
        params: Argon2Params,
    ) -> Result<Self, AccountError> {
        let registry = Registry::load(&data_dir)?;
        let active = restore_active(&data_dir, &*keychain, &registry)?;
        let state = State { registry, active };
        let provider_credentials =
            Arc::new(KeychainProviderCredentialStore::new(Arc::clone(&keychain)));
        Ok(Self {
            data_dir,
            keychain,
            provider_credentials,
            reasoning_lock: Mutex::new(()),
            params,
            state: Mutex::new(state),
            coordination: Arc::new(super::coordination::IdentityCoordination::default()),
        })
    }

    pub(crate) fn coordination(&self) -> Arc<super::coordination::IdentityCoordination> {
        Arc::clone(&self.coordination)
    }

    pub(crate) fn provider_credentials(&self) -> Arc<KeychainProviderCredentialStore> {
        Arc::clone(&self.provider_credentials)
    }

    pub(crate) fn reasoning_guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.reasoning_lock
            .lock()
            .expect("reasoning configuration mutex poisoned")
    }

    /// 是否已初始化（registry 有任何账户）。
    pub fn is_initialized(&self) -> bool {
        self.state
            .lock()
            .expect("state mutex")
            .registry
            .is_initialized()
    }

    /// 当前活跃账户 id（已解锁）。
    pub fn active_account_id(&self) -> Option<String> {
        self.state
            .lock()
            .expect("state mutex")
            .active
            .as_ref()
            .map(|a| a.account_id.clone())
    }

    pub(crate) fn logout(&self) -> Result<(), AccountError> {
        let mut state = self.state.lock().expect("state mutex");
        let Some(mut active) = state.active.take() else {
            return Ok(());
        };
        // Fail closed in memory even if persistence/keychain cleanup fails. Attempt
        // all cleanup steps so either the registry or removed keys stop auto-login.
        state.registry.last_active_account_id = None;
        let registry_result = Registry::save(&self.data_dir, &state.registry);
        let dek_result = self
            .keychain
            .delete_secret(KeychainLabel::MasterDek, &active.account_id);
        let token_result = self
            .keychain
            .delete_secret(KeychainLabel::RootTokenSecret, &active.account_id);
        use zeroize::Zeroize;
        active.master_dek.zeroize();
        active.k_sqlite.zeroize();
        active.k_files.zeroize();
        active.root_token_bearer.zeroize();
        registry_result?;
        dek_result?;
        token_result?;
        Ok(())
    }

    /// registry 内存查 account_id 是否存在（verify 步 2，不读盘）。
    pub(crate) fn is_known_account(&self, id: &str) -> bool {
        self.state
            .lock()
            .expect("state mutex")
            .registry
            .find(id)
            .is_some()
    }

    /// per-account 数据目录：`{data_dir}/data/{account_id}/`（含 meta.db、wrapped-dek、
    /// 文件树）。供 `Storage` 编排器拼文件树路径，不暴露 `data_dir` 根。
    pub(crate) fn account_dir(&self, account_id: &str) -> std::path::PathBuf {
        self.data_dir.join("data").join(account_id)
    }

    /// account_id 是否为当前已解锁的活跃账户（verify 步 3，不读盘）。
    pub(crate) fn is_active_unlocked(&self, id: &str) -> bool {
        self.state
            .lock()
            .expect("state mutex")
            .active
            .as_ref()
            .map(|a| a.account_id == id)
            .unwrap_or(false)
    }

    /// 在同一次状态锁持有期间确认 active account 并复制其固定数据能力。
    /// 调用方必须再通过 identity operation gate 与账户切换/删除串行化。
    pub(crate) fn snapshot_active_account(
        &self,
        account_id: &str,
    ) -> Result<AccountDataKeys, AccountError> {
        let state = self.state.lock().expect("state mutex");
        let active = state.active.as_ref().ok_or(AccountError::NotUnlocked)?;
        if active.account_id != account_id {
            return Err(AccountError::NotUnlocked);
        }
        Ok(AccountDataKeys {
            account_id: account_id.to_owned(),
            account_dir: self.account_dir(account_id),
            k_sqlite: active.k_sqlite,
            k_files: active.k_files,
        })
    }

    /// 列全部账户摘要。
    pub fn list_accounts(&self) -> Vec<AccountSummary> {
        let state = self.state.lock().expect("state mutex");
        let active_id = state.active.as_ref().map(|a| a.account_id.as_str());
        state
            .registry
            .accounts
            .iter()
            .map(|r| AccountSummary {
                id: r.id.clone(),
                username: r.username.clone(),
                created_at: r.created_at,
                is_active: active_id == Some(r.id.as_str()),
            })
            .collect()
    }

    /// 首次建账户（仅未初始化）。返回该账户 root token（明文 secret 仅一次）。
    pub fn setup_first_account(
        &self,
        username: &str,
        password: &str,
    ) -> Result<IssuedToken, AccountError> {
        let mut state = self.state.lock().expect("state mutex");
        if state.registry.is_initialized() {
            return Err(AccountError::AlreadyInitialized);
        }
        let account_id = Uuid::new_v4().to_string();
        let created_at = now_secs();

        let token =
            self.create_account_inner(&mut state, &account_id, username, password, created_at)?;
        Ok(token)
    }

    /// 追加账户（需任一已解锁账户 root token——授权由 ST-M2.5/Auth 层校验）。
    /// 切换活跃至新账户。
    pub fn create_account(
        &self,
        username: &str,
        password: &str,
    ) -> Result<IssuedToken, AccountError> {
        let mut state = self.state.lock().expect("state mutex");
        let account_id = Uuid::new_v4().to_string();
        let created_at = now_secs();
        self.create_account_inner(&mut state, &account_id, username, password, created_at)
    }

    /// setup / create 共用编排。持有 state 锁。
    fn create_account_inner(
        &self,
        state: &mut State,
        account_id: &str,
        username: &str,
        password: &str,
        created_at: i64,
    ) -> Result<IssuedToken, AccountError> {
        // 1. 密码 → KEK → 随机 master_DEK → wrap。
        let salt = generate_salt();
        let master_dek = random_master_dek();
        let kek = derive_kek(password, &salt, &self.params)?;
        let wrapped = wrap_dek(&kek, &master_dek);
        let wdek = WrappedDek::new(&self.params, &salt, &wrapped);

        // 2. wrapped-dek 落盘（持久化第 1 步）。
        WrappedDek::write(&self.data_dir, account_id, &wdek)?;

        // 3. 派生子密钥 → 开库 + migrate → 签发 root token 并入库。
        let k_sqlite = derive_k_sqlite(&master_dek);
        let k_files = derive_k_files(&master_dek);
        let token = self.issue_root_token(account_id, &k_sqlite, created_at)?;

        // 4. 旧活跃账户 keychain 移出（活跃唯一）。
        if let Some(old) = state.active.as_ref() {
            if old.account_id != account_id {
                self.keychain
                    .delete_secret(KeychainLabel::MasterDek, &old.account_id)?;
                self.keychain
                    .delete_secret(KeychainLabel::RootTokenSecret, &old.account_id)?;
            }
        }

        // 5. 新账户 DEK + root token bearer 入 keychain（持久化第 2 步）。
        self.keychain
            .set_secret(KeychainLabel::MasterDek, account_id, &master_dek)?;
        self.keychain.set_secret(
            KeychainLabel::RootTokenSecret,
            account_id,
            token.secret_bearer.as_bytes(),
        )?;

        // 6. registry 追加 + 记活跃 + 原子写（持久化第 3 步，最后）。
        state.registry.accounts.push(AccountRecord {
            id: account_id.to_string(),
            username: username.to_string(),
            created_at,
            wrapped_dek_relpath: format!("{account_id}/wrapped-dek"),
        });
        state.registry.last_active_account_id = Some(account_id.to_string());
        state.registry.version = 1;
        Registry::save(&self.data_dir, &state.registry)?;

        // 7. 内存活跃态。
        state.active = Some(ActiveAccount {
            account_id: account_id.to_string(),
            master_dek,
            k_sqlite,
            k_files,
            root_token_bearer: Some(token.secret_bearer.clone()),
        });

        Ok(token)
    }

    /// 切换/解锁账户：密码 → unwrap 得 DEK → 入 keychain 作活跃 + 签发 root token。
    pub fn unlock_account(
        &self,
        account_id: &str,
        password: &str,
    ) -> Result<IssuedToken, AccountError> {
        let mut state = self.state.lock().expect("state mutex");
        // 账户须在 registry。
        if state.registry.find(account_id).is_none() {
            return Err(AccountError::AccountNotFound(account_id.to_string()));
        }
        let created_at = now_secs();

        // 1. 密码 → KEK → unwrap 得 master_DEK（密码错 → WrongPassword）。
        let master_dek = self.verify_password_inner(account_id, password)?;
        let k_sqlite = derive_k_sqlite(&master_dek);
        let k_files = derive_k_files(&master_dek);

        // 2. 签发新 root token 并入库（新 secret；旧 root token 仍有效）。
        let token = self.issue_root_token(account_id, &k_sqlite, created_at)?;

        // 3. 旧活跃 keychain 移出（活跃唯一；若解锁的是当前活跃账户则跳过移出）。
        if let Some(old) = state.active.as_ref() {
            if old.account_id != account_id {
                self.keychain
                    .delete_secret(KeychainLabel::MasterDek, &old.account_id)?;
                self.keychain
                    .delete_secret(KeychainLabel::RootTokenSecret, &old.account_id)?;
            }
        }

        // 4. 新账户 DEK + root token bearer 入 keychain。
        self.keychain
            .set_secret(KeychainLabel::MasterDek, account_id, &master_dek)?;
        self.keychain.set_secret(
            KeychainLabel::RootTokenSecret,
            account_id,
            token.secret_bearer.as_bytes(),
        )?;

        // 5. 记活跃 + 原子写。
        state.registry.last_active_account_id = Some(account_id.to_string());
        Registry::save(&self.data_dir, &state.registry)?;

        // 6. 内存活跃态。
        state.active = Some(ActiveAccount {
            account_id: account_id.to_string(),
            master_dek,
            k_sqlite,
            k_files,
            root_token_bearer: Some(token.secret_bearer.clone()),
        });

        Ok(token)
    }

    /// 校验密码（unwrap wrapped-DEK）。密码错 → `WrongPassword`。
    /// 不触碰 keychain / 活跃态；导出门禁 / 改密 / unlock 复用。
    pub fn verify_password(
        &self,
        account_id: &str,
        password: &str,
    ) -> Result<[u8; 32], AccountError> {
        self.verify_password_inner(account_id, password)
    }

    fn verify_password_inner(
        &self,
        account_id: &str,
        password: &str,
    ) -> Result<[u8; 32], AccountError> {
        let wdek = WrappedDek::read(&self.data_dir, account_id)?
            .ok_or_else(|| AccountError::AccountNotFound(account_id.to_string()))?;
        let salt = wdek.salt_bytes()?;
        let wrapped = wdek.wrapped_bytes()?;
        let kek = derive_kek(password, &salt, &wdek.params())?;
        unwrap_dek(&kek, &wrapped).map_err(|_| AccountError::WrongPassword)
    }

    /// 改密：校验当前 → 新 KEK 重 wrap（master_DEK / 子密钥 / 历史 / token 均不变）。
    pub fn change_password(
        &self,
        account_id: &str,
        current: &str,
        new: &str,
    ) -> Result<(), AccountError> {
        // 校验当前密码 + 取 master_DEK（不变）。
        let master_dek = self.verify_password_inner(account_id, current)?;

        // 新盐 + 新 KEK → 重 wrap 同一 master_DEK。
        let new_salt = generate_salt();
        let new_kek = derive_kek(new, &new_salt, &self.params)?;
        let new_wrapped = wrap_dek(&new_kek, &master_dek);
        let new_wdek = WrappedDek::new(&self.params, &new_salt, &new_wrapped);

        // 覆写 wrapped-dek 文件（temp+rename 原子）。DEK 不变 ⇒ keychain / DB / 历史不动。
        WrappedDek::write(&self.data_dir, account_id, &new_wdek)?;
        Ok(())
    }

    /// 删除账户：删该账户全部数据（per-account 目录 + keychain 项 + registry 行）。
    ///
    /// 活跃账户不可直接删（须先切走），否则 `ActiveAccountDeletion`（→409）。
    /// 非活跃账户删除：rm `data/{account_id}/` 整目录 → 删 keychain `MasterDek`+`RootTokenSecret`
    /// → registry `retain` + 若 `last_active_account_id` 指向目标则清空 → 原子写。
    pub(crate) fn delete_account(&self, account_id: &str) -> Result<(), AccountError> {
        let mut state = self.state.lock().expect("state mutex");

        // 账户须存在。
        if state.registry.find(account_id).is_none() {
            return Err(AccountError::AccountNotFound(account_id.to_string()));
        }
        // 活跃账户不可直接删。
        if state
            .active
            .as_ref()
            .map(|a| a.account_id == account_id)
            .unwrap_or(false)
        {
            return Err(AccountError::ActiveAccountDeletion);
        }

        // 1. provider vault 有固定 Keychain 地址，不依赖 DB 枚举；必须最先删除。
        self.provider_credentials
            .delete_account_credentials(account_id)?;

        // 2. 以 fd-relative/no-follow 方式清理独立明文缓存；安全校验失败时不触碰
        // 加密账户目录，避免产生半删除账户。
        crate::media_cache::remove_account(&self.data_dir, account_id)?;

        // 3. 删 per-account 整目录（meta.db + 文件树 + wrapped-dek）。
        crate::media_cache::remove_account_data(&self.data_dir, account_id)?;
        // 4. 删 keychain 项（幂等）。
        self.keychain
            .delete_secret(KeychainLabel::MasterDek, account_id)?;
        self.keychain
            .delete_secret(KeychainLabel::RootTokenSecret, account_id)?;

        // 5. 先保存候选 registry，成功后才替换内存事实；磁盘失败时可原地重试。
        let mut next_registry = state.registry.clone();
        next_registry.accounts.retain(|r| r.id != account_id);
        if next_registry.last_active_account_id.as_deref() == Some(account_id) {
            next_registry.last_active_account_id = None;
        }
        Registry::save(&self.data_dir, &next_registry)?;
        state.registry = next_registry;

        Ok(())
    }

    /// 从 master_DEK 派生子密钥（K_sqlite / K_files），仅内存。
    pub fn derive_subkeys(master_dek: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
        (derive_k_sqlite(master_dek), derive_k_files(master_dek))
    }

    /// 打开活跃账户的 per-account SQLCipher 库（K_sqlite raw key）。须已解锁。
    /// 按需开（不长期持有 Connection；ST-M2.6 可加连接缓存）。
    pub fn open_active_db(&self) -> Result<seasnail_storage::rusqlite::Connection, AccountError> {
        let state = self.state.lock().expect("state mutex");
        let active = state.active.as_ref().ok_or(AccountError::NotUnlocked)?;
        let path = meta_db_path(&self.data_dir, &active.account_id);
        let conn = open_db(&path, &active.k_sqlite)?;
        Ok(conn)
    }

    /// 活跃账户的 K_files（内容文件 AEAD 钥）。须已解锁。
    pub fn active_k_files(&self) -> Result<[u8; 32], AccountError> {
        let state = self.state.lock().expect("state mutex");
        state
            .active
            .as_ref()
            .map(|a| a.k_files)
            .ok_or(AccountError::NotUnlocked)
    }

    /// 活跃账户的 master_DEK 明文（内存副本）。须已解锁。
    /// 供未来重 wrap / 导出等需直接持 DEK 的路径，无需经密码重解。
    pub fn active_master_dek(&self) -> Result<[u8; 32], AccountError> {
        let state = self.state.lock().expect("state mutex");
        state
            .active
            .as_ref()
            .map(|a| a.master_dek)
            .ok_or(AccountError::NotUnlocked)
    }

    /// 活跃账户 root token bearer（从 keychain 恢复的，供 GUI 日常免密）。须已解锁。
    pub fn active_root_token_bearer(&self) -> Result<Option<String>, AccountError> {
        let state = self.state.lock().expect("state mutex");
        Ok(state
            .active
            .as_ref()
            .and_then(|a| a.root_token_bearer.clone()))
    }

    /// 生成 root token 并插入该账户 per-account DB 的 tokens 行。开库含 migrate（首开建表）。
    fn issue_root_token(
        &self,
        account_id: &str,
        k_sqlite: &[u8; 32],
        created_at: i64,
    ) -> Result<IssuedToken, AccountError> {
        let token = generate_token(account_id, ROOT_TOKEN_NAME, true, vec![], created_at)?;
        let conn = open_db(&meta_db_path(&self.data_dir, account_id), k_sqlite)?;
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
}

/// 从 keychain 恢复活跃账户（免密重开）。无 last_active / keychain 缺失 / 不可读 → None（锁定）。
///
/// keychain 读取错误（如未签名裸二进制读 Data Protection keychain 得 `errSecMissingEntitlement`）
/// 容忍为「锁定」而非致命——守护进程仍可启动，用户经 `unlock_account`（密码）恢复活跃。
fn restore_active(
    _data_dir: &Path,
    keychain: &dyn KeychainStore,
    registry: &Registry,
) -> Result<Option<ActiveAccount>, AccountError> {
    let Some(active_id) = registry.last_active_account_id.as_ref() else {
        return Ok(None);
    };
    // keychain 读失败 → 锁定（不致命）；仅 registry 读取与派生失败才上抛。
    let dek_bytes = match keychain.get_secret(KeychainLabel::MasterDek, active_id) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let Some(dek_bytes) = dek_bytes else {
        // keychain 无此项 → 锁定，需 unlock 经密码恢复。
        return Ok(None);
    };
    if dek_bytes.len() != 32 {
        return Ok(None);
    }
    let mut master_dek = [0u8; 32];
    master_dek.copy_from_slice(&dek_bytes);
    let k_sqlite = derive_k_sqlite(&master_dek);
    let k_files = derive_k_files(&master_dek);
    let root_token_bearer = match keychain.get_secret(KeychainLabel::RootTokenSecret, active_id) {
        Ok(Some(b)) => String::from_utf8(b).ok(),
        _ => None,
    };
    Ok(Some(ActiveAccount {
        account_id: active_id.clone(),
        master_dek,
        k_sqlite,
        k_files,
        root_token_bearer,
    }))
}

/// per-account meta.db 路径：`{data_root}/data/{account_id}/meta.db`。
fn meta_db_path(data_root: &Path, account_id: &str) -> PathBuf {
    data_root.join("data").join(account_id).join("meta.db")
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
    use seasnail_crypto::MemoryKeychain;

    /// 测试用小 Argon2 参数（快）；非生产 DEFAULT。
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

    fn new_crypto(dir: &tempfile::TempDir) -> Crypto {
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        Crypto::new(dir.path().to_path_buf(), kc, fast_params()).expect("Crypto::new")
    }

    #[test]
    fn logout_clears_memory_and_keychain_even_when_registry_save_fails() {
        let dir = tmpdir();
        let kc = Arc::new(MemoryKeychain::new());
        let crypto = Crypto::new(dir.path().into(), kc.clone(), fast_params()).unwrap();
        crypto.setup_first_account("alice", "password").unwrap();
        let id = crypto.active_account_id().unwrap();
        let registry = Registry::path(dir.path());
        let backup = registry.with_extension("backup");
        std::fs::rename(&registry, &backup).unwrap();
        std::fs::create_dir(&registry).unwrap();
        assert!(crypto.logout().is_err());
        assert!(crypto.active_account_id().is_none());
        assert!(crypto.active_master_dek().is_err());
        assert!(kc
            .get_secret(KeychainLabel::MasterDek, &id)
            .unwrap()
            .is_none());
        assert!(kc
            .get_secret(KeychainLabel::RootTokenSecret, &id)
            .unwrap()
            .is_none());
        std::fs::remove_dir(&registry).unwrap();
        std::fs::rename(backup, &registry).unwrap();
        assert!(Crypto::new(dir.path().into(), kc, fast_params())
            .unwrap()
            .active_account_id()
            .is_none());
    }

    // ── setup_first_account ────────────────────────────────────────────────────

    /// 建首账户：未初始化 → 签发 root token，registry 非空，活跃为此账户。
    #[test]
    fn setup_first_account_succeeds() {
        let dir = tmpdir();
        let crypto = new_crypto(&dir);
        assert!(!crypto.is_initialized());

        let token = crypto.setup_first_account("alice", "p@ss").unwrap();
        assert!(token.is_root);
        assert!(token.secret_bearer.starts_with("ss_live_"));
        assert!(crypto.is_initialized());
        assert_eq!(crypto.active_account_id(), Some(token.account_id));
    }

    /// 已初始化再 setup → AlreadyInitialized。
    #[test]
    fn setup_when_initialized_errors() {
        let dir = tmpdir();
        let crypto = new_crypto(&dir);
        crypto.setup_first_account("alice", "p@ss").unwrap();
        match crypto.setup_first_account("bob", "p@ss2") {
            Err(AccountError::AlreadyInitialized) => {}
            other => panic!("期望 AlreadyInitialized，实际 {other:?}"),
        }
    }

    /// setup 后 wrapped-dek 文件落盘 + 内容可解开（密码正确）。
    #[test]
    fn setup_writes_wrapped_dek_file() {
        let dir = tmpdir();
        let crypto = new_crypto(&dir);
        let token = crypto.setup_first_account("alice", "p@ss").unwrap();

        let wdek = WrappedDek::read(dir.path(), &token.account_id)
            .unwrap()
            .expect("wrapped-dek 应已落盘");
        assert_eq!(wdek.params(), fast_params());
        // verify_password 能解开 ⇒ wrapped-dek 与密码一致。
        let dek = crypto.verify_password(&token.account_id, "p@ss").unwrap();
        assert_eq!(dek.len(), 32);
    }

    /// setup 后 root token 行入库（可按 hash 查到）。
    #[test]
    fn setup_persists_root_token_row() {
        let dir = tmpdir();
        let crypto = new_crypto(&dir);
        let token = crypto.setup_first_account("alice", "p@ss").unwrap();

        let conn = crypto.open_active_db().unwrap();
        let row = seasnail_storage::get_token_by_hash(&conn, &token.token_hash).unwrap();
        assert!(row.is_some(), "root token 行应已入库");
        let row = row.unwrap();
        assert!(row.is_root);
        assert_eq!(row.account_id, token.account_id);
    }

    // ── 免密重开 ───────────────────────────────────────────────────────────────

    /// 免密重开：新建 Crypto 实例 → 从 registry + keychain 恢复活跃账户，无需密码。
    #[test]
    fn restore_active_without_password() {
        let dir = tmpdir();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        {
            let crypto = Crypto::new(dir.path().to_path_buf(), kc.clone(), fast_params()).unwrap();
            crypto.setup_first_account("alice", "p@ss").unwrap();
            // 原实例 drop（模拟重启）。
        }
        // 新实例：从盘 + keychain 恢复。
        let crypto2 = Crypto::new(dir.path().to_path_buf(), kc, fast_params()).unwrap();
        assert!(crypto2.is_initialized());
        assert!(crypto2.active_account_id().is_some(), "免密应恢复活跃账户");
        // 能开库（K_sqlite 已从 keychain DEK 派生）。
        let conn = crypto2.open_active_db().unwrap();
        // root token bearer 已从 keychain 恢复。
        assert!(crypto2.active_root_token_bearer().unwrap().is_some());
        let _ = conn;
    }

    /// keychain 丢失 → 活跃不可恢复（锁定），须 unlock 经密码恢复。
    #[test]
    fn keychain_lost_means_locked() {
        let dir = tmpdir();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        {
            let crypto = Crypto::new(dir.path().to_path_buf(), kc.clone(), fast_params()).unwrap();
            let t = crypto.setup_first_account("alice", "p@ss").unwrap();
            // 模拟 keychain 丢失：清空。
            kc.delete_secret(KeychainLabel::MasterDek, &t.account_id)
                .unwrap();
            kc.delete_secret(KeychainLabel::RootTokenSecret, &t.account_id)
                .unwrap();
        }
        let kc2 = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>; // 全新空 keychain
        let crypto2 = Crypto::new(dir.path().to_path_buf(), kc2, fast_params()).unwrap();
        assert!(crypto2.active_account_id().is_none(), "keychain 丢 → 锁定");
        assert!(crypto2.active_root_token_bearer().unwrap().is_none());
    }

    // ── create_account ─────────────────────────────────────────────────────────

    /// 追加账户：切换活跃至新账户，旧账户 keychain 移出。
    #[test]
    fn create_account_switches_active() {
        let dir = tmpdir();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let crypto = Crypto::new(dir.path().to_path_buf(), kc.clone(), fast_params()).unwrap();

        let t1 = crypto.setup_first_account("alice", "p1").unwrap();
        let t2 = crypto.create_account("bob", "p2").unwrap();
        assert_ne!(t1.account_id, t2.account_id);
        assert_eq!(crypto.active_account_id(), Some(t2.account_id.clone()));

        // 旧账户 DEK 移出 keychain。
        assert_eq!(
            kc.get_secret(KeychainLabel::MasterDek, &t1.account_id)
                .unwrap(),
            None,
            "旧活跃 DEK 应移出 keychain"
        );
        // 新账户 DEK 在 keychain。
        assert!(kc
            .get_secret(KeychainLabel::MasterDek, &t2.account_id)
            .unwrap()
            .is_some());
    }

    #[test]
    fn deleting_non_active_account_removes_media_cache() {
        let dir = tmpdir();
        let crypto = new_crypto(&dir);
        let alice = crypto.setup_first_account("alice", "p1").unwrap();
        let _bob = crypto.create_account("bob", "p2").unwrap(); // bob active, alice removable
        let cache_dir = dir
            .path()
            .join("cache/clipboard-context")
            .join(&alice.account_id)
            .join("capture");
        std::fs::create_dir_all(&cache_dir).unwrap();

        crypto.delete_account(&alice.account_id).unwrap();
        assert!(!cache_dir.parent().unwrap().exists());
        assert!(!dir.path().join("data").join(&alice.account_id).exists());
    }

    // ── unlock_account ─────────────────────────────────────────────────────────

    /// unlock 已存在账户：密码正确 → 切活跃 + 签发新 root token。
    #[test]
    fn unlock_with_correct_password() {
        let dir = tmpdir();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let crypto = Crypto::new(dir.path().to_path_buf(), kc, fast_params()).unwrap();

        let t1 = crypto.setup_first_account("alice", "p1").unwrap();
        let _t2 = crypto.create_account("bob", "p2").unwrap(); // bob 活跃
                                                               // 切回 alice：需 alice 密码。
        let t1b = crypto.unlock_account(&t1.account_id, "p1").unwrap();
        assert_eq!(crypto.active_account_id(), Some(t1.account_id.clone()));
        // 新签发的 root token（新 secret）。
        assert_ne!(t1.secret_bearer, t1b.secret_bearer);
        assert!(t1b.is_root);
    }

    /// unlock 密码错 → WrongPassword，活跃不变。
    #[test]
    fn unlock_wrong_password_errors() {
        let dir = tmpdir();
        let crypto = new_crypto(&dir);
        let t1 = crypto.setup_first_account("alice", "p1").unwrap();
        crypto.create_account("bob", "p2").unwrap(); // bob 活跃
        match crypto.unlock_account(&t1.account_id, "wrong") {
            Err(AccountError::WrongPassword) => {}
            other => panic!("期望 WrongPassword，实际 {other:?}"),
        }
        // 活跃仍为 bob。
        assert_ne!(crypto.active_account_id(), Some(t1.account_id));
    }

    /// unlock 未知账户 → AccountNotFound。
    #[test]
    fn unlock_unknown_account_errors() {
        let dir = tmpdir();
        let crypto = new_crypto(&dir);
        let ghost = Uuid::new_v4().to_string();
        match crypto.unlock_account(&ghost, "p") {
            Err(AccountError::AccountNotFound(_)) => {}
            other => panic!("期望 AccountNotFound，实际 {other:?}"),
        }
    }

    // ── verify_password ─────────────────────────────────────────────────────────

    /// 正确密码解开 DEK；错误密码失败。
    #[test]
    fn verify_password_correct_and_wrong() {
        let dir = tmpdir();
        let crypto = new_crypto(&dir);
        let t = crypto.setup_first_account("alice", "p@ss").unwrap();
        assert!(crypto.verify_password(&t.account_id, "p@ss").is_ok());
        match crypto.verify_password(&t.account_id, "nope") {
            Err(AccountError::WrongPassword) => {}
            other => panic!("期望 WrongPassword，实际 {other:?}"),
        }
    }

    // ── change_password ────────────────────────────────────────────────────────

    /// 改密：DEK 不变（旧 wrapped-dek 解出的 DEK 与新 wrapped-dek 解出的一致），
    /// 旧密码失效、新密码可用。
    #[test]
    fn change_password_preserves_dek() {
        let dir = tmpdir();
        let crypto = new_crypto(&dir);
        let t = crypto.setup_first_account("alice", "old").unwrap();
        let dek_before = crypto.verify_password(&t.account_id, "old").unwrap();

        crypto.change_password(&t.account_id, "old", "new").unwrap();

        // 旧密码失效。
        match crypto.verify_password(&t.account_id, "old") {
            Err(AccountError::WrongPassword) => {}
            other => panic!("期望旧密码失效，实际 {other:?}"),
        }
        // 新密码可用，DEK 不变。
        let dek_after = crypto.verify_password(&t.account_id, "new").unwrap();
        assert_eq!(dek_before, dek_after, "改密后 master_DEK 不变");
    }

    /// 改密时当前密码错 → WrongPassword，不动 wrapped-dek。
    #[test]
    fn change_password_wrong_current_errors() {
        let dir = tmpdir();
        let crypto = new_crypto(&dir);
        let t = crypto.setup_first_account("alice", "old").unwrap();
        match crypto.change_password(&t.account_id, "wrong", "new") {
            Err(AccountError::WrongPassword) => {}
            other => panic!("期望 WrongPassword，实际 {other:?}"),
        }
        // 旧密码仍可用（未改）。
        assert!(crypto.verify_password(&t.account_id, "old").is_ok());
    }

    /// 改密后 token 仍有效（token secret 独立于密码）。
    #[test]
    fn change_password_keeps_token_valid() {
        let dir = tmpdir();
        let crypto = new_crypto(&dir);
        let t = crypto.setup_first_account("alice", "old").unwrap();
        crypto.change_password(&t.account_id, "old", "new").unwrap();

        let conn = crypto.open_active_db().unwrap();
        let row = seasnail_storage::get_token_by_hash(&conn, &t.token_hash)
            .unwrap()
            .expect("token 行应仍在");
        assert_eq!(
            row.token_hash, t.token_hash,
            "token hash 不变 ⇒ token 仍有效"
        );
    }

    // ── list_accounts ───────────────────────────────────────────────────────────

    /// list_accounts：含 is_active 标记。
    #[test]
    fn list_accounts_marks_active() {
        let dir = tmpdir();
        let crypto = new_crypto(&dir);
        let t1 = crypto.setup_first_account("alice", "p1").unwrap();
        let t2 = crypto.create_account("bob", "p2").unwrap();

        let list = crypto.list_accounts();
        assert_eq!(list.len(), 2);
        let bob = list.iter().find(|a| a.id == t2.account_id).unwrap();
        let alice = list.iter().find(|a| a.id == t1.account_id).unwrap();
        assert!(bob.is_active, "bob 当前活跃");
        assert!(!alice.is_active, "alice 非活跃");
    }

    // ── derive_subkeys ──────────────────────────────────────────────────────────

    /// derive_subkeys 与原语一致。
    #[test]
    fn derive_subkeys_matches_primitives() {
        let dek = [0x42u8; 32];
        let (k_sqlite, k_files) = Crypto::derive_subkeys(&dek);
        assert_eq!(k_sqlite, derive_k_sqlite(&dek));
        assert_eq!(k_files, derive_k_files(&dek));
        assert_ne!(k_sqlite, k_files);
    }
}
