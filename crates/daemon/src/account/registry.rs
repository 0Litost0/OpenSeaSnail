//! 全局账户 registry `data/accounts.json`（ST-M2.4）。
//!
//! 设计见 `doc/architecture.md#accounts-and-storage`「存储结构设计」。registry 只存
//! 解锁前就要回答的元数据（账户 id / username / created_at / wrapped-dek 相对
//! 路径），**不存密钥/密码/DEK/KDF 参数**——KDF 参数随 `wrapped-dek` 独立文件。
//! 非加密、mode 0o600、temp+rename 原子写（与 `bootstrap.json` 同模式）。
//!
//! `initialized` = `accounts` 非空；`GET /accounts` 列账户；守护进程重启免密取
//! `last_active_account_id`——三者均发生在任何账户解锁之前、per-account
//! SQLCipher 库此时打不开，故须独立于 per-account 库的全局清单。
//!
//! 落盘为事实源；进程内持内存副本（verify 等热路径不每次读盘），账户变更后同步刷新。
//! 路径：`{data_dir}/data/accounts.json`（per-account 目录同根于 `data/`）。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::error::AccountError;

/// registry 文件名（落在 `data/` 子目录下）。
const REGISTRY_FILE: &str = "accounts.json";
/// registry 格式版本。一经发布不可变。
const REGISTRY_VERSION: u32 = 1;

/// 全局账户 registry。内存副本 + 落盘事实源。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    /// 格式版本。
    pub version: u32,
    /// 当前活跃账户 id（非敏感 prefs；解锁前可读）。
    #[serde(default)]
    pub last_active_account_id: Option<String>,
    /// 全部账户记录。空 = 未初始化。
    #[serde(default)]
    pub accounts: Vec<AccountRecord>,
}

/// 单个账户的 registry 记录（仅元数据，不含密钥）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountRecord {
    /// UUID v4，贯通全链路（registry / 文件树目录名 / per-account 库行 / token 内嵌）。
    pub id: String,
    /// 用户名（展示用，改名不影响密钥）。
    pub username: String,
    /// 创建时间（epoch 秒 UTC）。
    pub created_at: i64,
    /// wrapped-DEK 文件相对路径，例 `"{account_id}/wrapped-dek"`（相对 `data/`）。
    pub wrapped_dek_relpath: String,
}

impl Registry {
    /// 新建空 registry。
    pub fn new() -> Self {
        Self {
            version: REGISTRY_VERSION,
            last_active_account_id: None,
            accounts: Vec::new(),
        }
    }

    /// 是否已初始化（至少一个账户）。
    pub fn is_initialized(&self) -> bool {
        !self.accounts.is_empty()
    }

    /// 按 id 查记录。
    pub fn find(&self, account_id: &str) -> Option<&AccountRecord> {
        self.accounts.iter().find(|a| a.id == account_id)
    }

    /// registry 文件路径：`{data_root}/data/accounts.json`。
    pub fn path(data_root: &Path) -> PathBuf {
        data_root.join("data").join(REGISTRY_FILE)
    }

    /// 从 `data_root` 读 registry；缺失返回空 registry，损坏返错。
    pub fn load(data_root: &Path) -> Result<Self, AccountError> {
        let path = Self::path(data_root);
        match fs::read(&path) {
            Ok(bytes) => {
                let r: Registry = serde_json::from_slice(&bytes)?;
                Ok(r)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::new()),
            Err(e) => Err(AccountError::Io(e)),
        }
    }

    /// 原子写落盘（temp+rename，mode 0o600），与 `bootstrap.json` 同模式。
    /// 写中途崩溃：要么旧文件不变、要么新文件就位，不留半截。
    pub fn save(data_root: &Path, registry: &Registry) -> Result<(), AccountError> {
        let dir = data_root.join("data");
        // 确保目录存在；新建则收紧 0o700。
        let created = !dir.exists();
        if created {
            fs::create_dir_all(&dir)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&dir, PermissionsExt::from_mode(0o700));
        }

        let bytes = serde_json::to_vec_pretty(registry)?;
        let final_path = Self::path(data_root);
        let tmp_path = final_path.with_extension("json.tmp");

        {
            use std::io::Write;
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp_path)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp_path, PermissionsExt::from_mode(0o600))?;
        }
        fs::rename(&tmp_path, &final_path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn sample_record(id: &str) -> AccountRecord {
        AccountRecord {
            id: id.to_string(),
            username: format!("user-{id}"),
            created_at: 1_700_000_000,
            wrapped_dek_relpath: format!("{id}/wrapped-dek"),
        }
    }

    /// 写后读回一致。
    #[test]
    fn save_then_load_roundtrip() {
        let dir = tmpdir();
        let mut r = Registry::new();
        r.accounts.push(sample_record("acct-A"));
        r.last_active_account_id = Some("acct-A".into());
        Registry::save(dir.path(), &r).unwrap();

        let loaded = Registry::load(dir.path()).unwrap();
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.last_active_account_id.as_deref(), Some("acct-A"));
        assert_eq!(loaded.accounts, vec![sample_record("acct-A")]);
    }

    /// 缺失文件 → 空 registry（未初始化）。
    #[test]
    fn load_missing_returns_empty() {
        let dir = tmpdir();
        let r = Registry::load(dir.path()).unwrap();
        assert!(!r.is_initialized());
        assert!(r.accounts.is_empty());
        assert_eq!(r.last_active_account_id, None);
    }

    /// is_initialized = accounts 非空。
    #[test]
    fn initialized_means_accounts_nonempty() {
        let mut r = Registry::new();
        assert!(!r.is_initialized());
        r.accounts.push(sample_record("x"));
        assert!(r.is_initialized());
    }

    /// 损坏文件 → 报错（不静默吞）。
    #[test]
    fn load_corrupt_errors() {
        let dir = tmpdir();
        fs::create_dir_all(dir.path().join("data")).unwrap();
        fs::write(Registry::path(dir.path()), b"not json").unwrap();
        assert!(Registry::load(dir.path()).is_err());
    }

    /// find 按 id 命中 / 未命中。
    #[test]
    fn find_by_id() {
        let mut r = Registry::new();
        r.accounts.push(sample_record("A"));
        r.accounts.push(sample_record("B"));
        assert!(r.find("A").is_some());
        assert!(r.find("B").is_some());
        assert!(r.find("C").is_none());
    }

    /// 路径落在 data/ 子目录下。
    #[test]
    fn path_under_data_subdir() {
        let p = Registry::path(Path::new("/tmp/seasnail"));
        assert!(p.ends_with("data/accounts.json"));
    }

    /// wrapped_dek_relpath 形如 `{account_id}/wrapped-dek`（相对 data/）。
    #[test]
    fn wrapped_dek_relpath_shape() {
        let rec = sample_record("acct-4f3a");
        assert_eq!(rec.wrapped_dek_relpath, "acct-4f3a/wrapped-dek");
    }

    #[cfg(unix)]
    #[test]
    fn file_mode_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmpdir();
        Registry::save(dir.path(), &Registry::new()).unwrap();
        let mode = fs::metadata(Registry::path(dir.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "registry 文件权限应为 0o600");
    }

    #[cfg(unix)]
    #[test]
    fn data_dir_mode_is_0700_when_created() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmpdir();
        Registry::save(dir.path(), &Registry::new()).unwrap();
        let mode = fs::metadata(dir.path().join("data"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "data/ 目录权限应为 0o700");
    }

    /// 原子写：写后无 .tmp 残留。
    #[test]
    fn no_tmp_lingering_after_save() {
        let dir = tmpdir();
        Registry::save(dir.path(), &Registry::new()).unwrap();
        let tmp = Registry::path(dir.path()).with_extension("json.tmp");
        assert!(!tmp.exists(), "temp 残留应被 rename 清掉");
    }
}
