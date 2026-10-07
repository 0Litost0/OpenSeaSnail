//! wrapped-DEK 文件格式（ST-M2.4）。
//!
//! 设计见 `doc/architecture.md#accounts-and-storage`「加解密模块」边界 case：wrapped-DEK
//! 落盘内容 `{kdf:"argon2id", m_cost, t_cost, p_cost, salt, wrapped_dek}`——KDF 参数
//! 随件存储、不可硬编码（改参=解不出历史）。per-account 一份，独立文件，文件名
//! `wrapped-dek`（无扩展名），路径 `{data_root}/data/{account_id}/wrapped-dek`。
//!
//! 序列化选 **JSON + base64 字段**（与 `bootstrap.json` / `accounts.json` 同工具链，
//! AI 友好、可读、serde 复用）；二进制字段（salt、wrapped_dek）以 base64(STANDARD)
//! 编码。文件 mode 0o600、temp+rename 原子写。
//!
//! wrapped-DEK 是密码恢复的唯一材料：master_DEK 明文不落盘（仅活跃账户存 keychain），
//! keychain 丢失时靠密码经此文件 unwrap 恢复 master_DEK。

use std::fs;
use std::io;
use std::path::Path;

use base64::Engine;
use serde::{Deserialize, Serialize};

use seasnail_crypto::Argon2Params;

use super::error::AccountError;

/// wrapped-DEK 文件名（无扩展名）。
const WRAPPED_DEK_FILE: &str = "wrapped-dek";

/// KDF 算法标识。当前仅 `argon2id`。
const KDF_ARGON2ID: &str = "argon2id";

/// wrapped-DEK 落盘结构。二进制字段 base64(STANDARD) 编码。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WrappedDek {
    /// KDF 算法（`argon2id`）。
    pub kdf: String,
    /// 内存成本（KiB）。
    pub m_cost: u32,
    /// 时间成本（迭代轮数）。
    pub t_cost: u32,
    /// 并行成本（lanes）。
    pub p_cost: u32,
    /// salt（16B），base64 编码。
    pub salt: String,
    /// KEK 包裹的 master_DEK（`nonce(12)‖ct‖tag(16)`），base64 编码。
    pub wrapped_dek: String,
}

impl WrappedDek {
    /// 由 Argon2 参数 + salt + 包裹密文构造。
    pub fn new(params: &Argon2Params, salt: &[u8], wrapped: &[u8]) -> Self {
        Self {
            kdf: KDF_ARGON2ID.to_string(),
            m_cost: params.m_kib,
            t_cost: params.t_cost,
            p_cost: params.p_cost,
            salt: base64::engine::general_purpose::STANDARD.encode(salt),
            wrapped_dek: base64::engine::general_purpose::STANDARD.encode(wrapped),
        }
    }

    /// 还原 Argon2 参数。
    pub fn params(&self) -> Argon2Params {
        Argon2Params {
            m_kib: self.m_cost,
            t_cost: self.t_cost,
            p_cost: self.p_cost,
        }
    }

    /// 解码 salt 原始字节。
    pub fn salt_bytes(&self) -> Result<Vec<u8>, AccountError> {
        base64::engine::general_purpose::STANDARD
            .decode(&self.salt)
            .map_err(|e| {
                AccountError::Serde(serde_json::Error::io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    e,
                )))
            })
    }

    /// 解码 wrapped 密文原始字节。
    pub fn wrapped_bytes(&self) -> Result<Vec<u8>, AccountError> {
        base64::engine::general_purpose::STANDARD
            .decode(&self.wrapped_dek)
            .map_err(|e| {
                AccountError::Serde(serde_json::Error::io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    e,
                )))
            })
    }

    /// wrapped-DEK 文件路径：`{data_root}/data/{account_id}/wrapped-dek`。
    pub fn path(data_root: &Path, account_id: &str) -> std::path::PathBuf {
        data_root
            .join("data")
            .join(account_id)
            .join(WRAPPED_DEK_FILE)
    }

    /// 原子写落盘（temp+rename，mode 0o600）。先建账户目录。
    pub fn write(data_root: &Path, account_id: &str, dek: &WrappedDek) -> Result<(), AccountError> {
        let dir = data_root.join("data").join(account_id);
        fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&dir, PermissionsExt::from_mode(0o700));
        }

        let bytes = serde_json::to_vec_pretty(dek)?;
        let final_path = Self::path(data_root, account_id);
        let tmp_path = final_path.with_extension("dek.tmp");

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

    /// 从盘读 wrapped-DEK。缺失返 `None`（registry↔wrapped-dek 一致性由 reconcile 兜底，
    /// ST-M2.7）；损坏返错。
    pub fn read(data_root: &Path, account_id: &str) -> Result<Option<WrappedDek>, AccountError> {
        let path = Self::path(data_root, account_id);
        match fs::read(&path) {
            Ok(bytes) => {
                let dek: WrappedDek = serde_json::from_slice(&bytes)?;
                Ok(Some(dek))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(AccountError::Io(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn fast_params() -> Argon2Params {
        Argon2Params {
            m_kib: 8192,
            t_cost: 1,
            p_cost: 1,
        }
    }

    /// 构造 → params/salt/wrapped 往返一致。
    #[test]
    fn new_then_decode_roundtrip() {
        let params = fast_params();
        let salt = [0xabu8; 16];
        let wrapped = vec![0xcdu8; 60];
        let dek = WrappedDek::new(&params, &salt, &wrapped);

        assert_eq!(dek.kdf, "argon2id");
        assert_eq!(dek.params(), params);
        assert_eq!(dek.salt_bytes().unwrap(), salt);
        assert_eq!(dek.wrapped_bytes().unwrap(), wrapped);
    }

    /// 写后读回一致（JSON 序列化往返）。
    #[test]
    fn write_then_read_roundtrip() {
        let dir = tmpdir();
        let params = fast_params();
        let dek = WrappedDek::new(&params, &[0x11; 16], &[0x22; 60]);
        WrappedDek::write(dir.path(), "acct-A", &dek).unwrap();

        let loaded = WrappedDek::read(dir.path(), "acct-A")
            .unwrap()
            .expect("应能读回");
        assert_eq!(loaded, dek);
    }

    /// 文件缺失 → None（不报错；reconcile 兜底语义）。
    #[test]
    fn read_missing_returns_none() {
        let dir = tmpdir();
        assert!(WrappedDek::read(dir.path(), "ghost").unwrap().is_none());
    }

    /// 损坏文件 → 报错。
    #[test]
    fn read_corrupt_errors() {
        let dir = tmpdir();
        fs::create_dir_all(WrappedDek::path(dir.path(), "acct-X").parent().unwrap()).unwrap();
        fs::write(WrappedDek::path(dir.path(), "acct-X"), b"not json").unwrap();
        assert!(WrappedDek::read(dir.path(), "acct-X").is_err());
    }

    /// 路径形如 `data/{account_id}/wrapped-dek`。
    #[test]
    fn path_shape() {
        let p = WrappedDek::path(Path::new("/tmp/seasnail"), "acct-4f3a");
        assert!(p.ends_with("data/acct-4f3a/wrapped-dek"));
    }

    #[cfg(unix)]
    #[test]
    fn file_mode_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmpdir();
        let dek = WrappedDek::new(&fast_params(), &[0u8; 16], &[0u8; 60]);
        WrappedDek::write(dir.path(), "acct", &dek).unwrap();
        let mode = fs::metadata(WrappedDek::path(dir.path(), "acct"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "wrapped-dek 文件权限应为 0o600");
    }

    #[cfg(unix)]
    #[test]
    fn account_dir_mode_is_0700() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmpdir();
        let dek = WrappedDek::new(&fast_params(), &[0u8; 16], &[0u8; 60]);
        WrappedDek::write(dir.path(), "acct", &dek).unwrap();
        let mode = fs::metadata(dir.path().join("data").join("acct"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "账户目录权限应为 0o700");
    }

    /// 原子写无 .tmp 残留。
    #[test]
    fn no_tmp_lingering() {
        let dir = tmpdir();
        let dek = WrappedDek::new(&fast_params(), &[0u8; 16], &[0u8; 60]);
        WrappedDek::write(dir.path(), "acct", &dek).unwrap();
        assert!(!WrappedDek::path(dir.path(), "acct")
            .with_extension("dek.tmp")
            .exists());
    }

    /// KDF 算法标识恒为 argon2id。
    #[test]
    fn kdf_is_argon2id() {
        let dek = WrappedDek::new(&fast_params(), &[0u8; 16], &[0u8; 60]);
        assert_eq!(dek.kdf, "argon2id");
    }
}
