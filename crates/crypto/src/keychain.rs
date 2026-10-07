//! macOS keychain 集成（ST-M2.3）。
//!
//! 活跃账户的 `master_DEK`、`root token secret` 与账户级 provider credential vault
//! 以 generic password 项存入 app 私有 keychain（macOS Data Protection keychain）。
//! 条目属性统一：`service = com.seasnail`、`account = {prefix}-{account_id}`
//!（`master-dek` / `root-token` / `provider-credentials`），可访问性
//! `WhenUnlocked` + `ThisDeviceOnly`（解锁后可读、禁 iCloud 同步），不挂生物
//! 特征/密码约束（访问不弹 Touch ID/密码框）。
//!
//! 因真实 keychain 行为依赖签名 `.app` 上下文与当前用户登录态，且 `cargo test`
//! 跑的是未签名裸二进制、反复写入会污染开发者本人登录 keychain，故抽象出
//! [`KeychainStore`] trait + 两实现：[`MemoryKeychain`] 供自动化单测（内存语义），
//! [`MacKeychain`]（`cfg(target_os = "macos")`）为真实实现，仅留 `#[ignore]` 的
//! 手动往返测试对应路线图"上机确认"验收。ST-M2.4 `Crypto` 编排经 trait 注入，
//! 生产用 `MacKeychain`、测录用 `MemoryKeychain`。
//!
//! 见 `doc/architecture.md#accounts-and-storage`「加解密模块」与「测试策略」。

use std::collections::HashMap;
use std::sync::Mutex;

use crate::Error;

/// keychain service 名（所有条目共用，集中命名一处）。
pub const SERVICE: &str = "com.seasnail";

/// keychain 条目种类：决定 `account` 属性前缀与语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeychainLabel {
    /// 活跃账户 master_DEK（32B 明文）。account = `master-dek-{account_id}`。
    MasterDek,
    /// 活跃账户 root token secret。account = `root-token-{account_id}`。
    RootTokenSecret,
    /// 账户级 provider credential vault。account = `provider-credentials-{account_id}`。
    ProviderCredentials,
}

impl KeychainLabel {
    /// `account` 属性前缀（不含 account_id）。
    const fn prefix(self) -> &'static str {
        match self {
            KeychainLabel::MasterDek => "master-dek",
            KeychainLabel::RootTokenSecret => "root-token",
            KeychainLabel::ProviderCredentials => "provider-credentials",
        }
    }

    /// 拼出 keychain `account` 属性：`{prefix}-{account_id}`。
    fn account(self, account_id: &str) -> String {
        format!("{}-{}", self.prefix(), account_id)
    }
}

/// keychain 存取抽象。ST-M2.4 `Crypto` 编排经此 trait 注入实现，便于测试。
///
/// 语义约定：
/// - [`get_secret`]：不存在返回 `Ok(None)`，不报错。
/// - [`delete_secret`]：幂等，不存在返回 `Ok(())`，不报错。
/// - [`set_secret`]：覆盖既有项（同 service+account）。
pub trait KeychainStore: Send + Sync {
    /// 写入/覆盖 secret。
    fn set_secret(&self, label: KeychainLabel, account_id: &str, value: &[u8])
        -> Result<(), Error>;
    /// 读取 secret；不存在返回 `Ok(None)`。
    fn get_secret(&self, label: KeychainLabel, account_id: &str) -> Result<Option<Vec<u8>>, Error>;
    /// 删除 secret；幂等（不存在也 `Ok(())`）。
    fn delete_secret(&self, label: KeychainLabel, account_id: &str) -> Result<(), Error>;
}

// ── 内存实现（自动化单测默认用）──────────────────────────────────────────────

/// 内存 keychain：`Mutex<HashMap<(prefix, account_id), Vec<u8>>>`，语义与
/// [`MacKeychain`] 对齐。不碰任何系统状态。
pub struct MemoryKeychain {
    store: Mutex<HashMap<(&'static str, String), Vec<u8>>>,
}

impl MemoryKeychain {
    /// 新建空内存 keychain。
    pub fn new() -> Self {
        Self {
            store: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for MemoryKeychain {
    fn default() -> Self {
        Self::new()
    }
}

impl KeychainStore for MemoryKeychain {
    fn set_secret(
        &self,
        label: KeychainLabel,
        account_id: &str,
        value: &[u8],
    ) -> Result<(), Error> {
        let mut store = self.store.lock().expect("MemoryKeychain mutex poisoned");
        store.insert((label.prefix(), account_id.to_string()), value.to_vec());
        Ok(())
    }

    fn get_secret(&self, label: KeychainLabel, account_id: &str) -> Result<Option<Vec<u8>>, Error> {
        let store = self.store.lock().expect("MemoryKeychain mutex poisoned");
        Ok(store
            .get(&(label.prefix(), account_id.to_string()))
            .cloned())
    }

    fn delete_secret(&self, label: KeychainLabel, account_id: &str) -> Result<(), Error> {
        let mut store = self.store.lock().expect("MemoryKeychain mutex poisoned");
        store.remove(&(label.prefix(), account_id.to_string()));
        Ok(())
    }
}

// ── macOS 真实实现 ────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod mac {
    use super::{Error, KeychainLabel, KeychainStore, SERVICE};

    use security_framework::access_control::{ProtectionMode, SecAccessControl};
    use security_framework::passwords::{
        delete_generic_password_options, generic_password, set_generic_password_options,
        AccessControlOptions, PasswordOptions,
    };
    use security_framework_sys::base::errSecItemNotFound;

    /// macOS keychain 真实实现（`security-framework` crate）。
    ///
    /// 两种形态由 [`MacKeychain::with_protected`] 切换：
    /// - `protected = true`（生产默认，[`MacKeychain::new`]）：走 **Data Protection
    ///   keychain** + 可访问性 `WhenUnlocked`/`ThisDeviceOnly`。**要求签名 `.app`
    ///   的 keychain 访问组 entitlement**——未签名裸二进制（如 `cargo test`）写入
    ///   会得 `errSecMissingEntitlement`。这是设计文档「仅签名 App 可读」的预期
    ///   行为；该路径的归属与跨启动稳定属 ST-M9.3 签名包上机确认项。
    /// - `protected = false`：走默认 **文件 keychain**、不设访问控制，未签名二进
    ///   制可写——供 `#[ignore]` 手动测试验证 security-framework 接线（set/get/
    ///   delete 往返与 not-found 语义），不验证 Data Protection 归属。
    ///
    /// 无状态：所有条目经全局 keychain 按 `service+account` 寻址。可安全 `Clone`
    /// /多处共享。ST-M2.4 生产编排用 [`MacKeychain::new`]。
    #[derive(Debug, Clone, Copy)]
    pub struct MacKeychain {
        protected: bool,
    }

    impl MacKeychain {
        /// 生产实现：Data Protection keychain + `WhenUnlocked`/`ThisDeviceOnly`，
        /// 需签名 `.app` entitlement。
        pub fn new() -> Self {
            Self { protected: true }
        }

        /// 切换 keychain 形态。`false` 用文件 keychain（未签名可写，测试用）。
        pub fn with_protected(protected: bool) -> Self {
            Self { protected }
        }

        /// 构建本条目的 `PasswordOptions`。`protected` 决定是否挂访问控制 + 切
        /// Data Protection keychain。
        fn options(self, label: KeychainLabel, account_id: &str) -> Result<PasswordOptions, Error> {
            let mut opts =
                PasswordOptions::new_generic_password(SERVICE, &label.account(account_id));
            if self.protected {
                let access_control = SecAccessControl::create_with_protection(
                    Some(ProtectionMode::AccessibleWhenUnlockedThisDeviceOnly),
                    AccessControlOptions::empty().bits(),
                )
                .map_err(|e| Error::Keychain(format!("create access control: {e}")))?;
                opts.set_access_control(access_control);
                opts.use_protected_keychain();
            }
            Ok(opts)
        }
    }

    impl Default for MacKeychain {
        fn default() -> Self {
            Self::new()
        }
    }

    impl KeychainStore for MacKeychain {
        fn set_secret(
            &self,
            label: KeychainLabel,
            account_id: &str,
            value: &[u8],
        ) -> Result<(), Error> {
            let opts = self.options(label, account_id)?;
            set_generic_password_options(value, opts)
                .map_err(|e| Error::Keychain(format!("set_generic_password: {e}")))
        }

        fn get_secret(
            &self,
            label: KeychainLabel,
            account_id: &str,
        ) -> Result<Option<Vec<u8>>, Error> {
            let opts = self.options(label, account_id)?;
            match generic_password(opts) {
                Ok(v) => Ok(Some(v)),
                Err(e) if e.code() == errSecItemNotFound => Ok(None),
                Err(e) => Err(Error::Keychain(format!("generic_password: {e}"))),
            }
        }

        fn delete_secret(&self, label: KeychainLabel, account_id: &str) -> Result<(), Error> {
            let opts = self.options(label, account_id)?;
            match delete_generic_password_options(opts) {
                Ok(()) => Ok(()),
                Err(e) if e.code() == errSecItemNotFound => Ok(()),
                Err(e) => Err(Error::Keychain(format!("delete_generic_password: {e}"))),
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub use mac::MacKeychain;

#[cfg(test)]
mod tests {
    use super::*;

    /// set→get 命中。
    #[test]
    fn memory_set_get_roundtrip() {
        let kc = MemoryKeychain::new();
        let dek = [0xab; 32];
        kc.set_secret(KeychainLabel::MasterDek, "acct-A", &dek)
            .unwrap();
        assert_eq!(
            kc.get_secret(KeychainLabel::MasterDek, "acct-A").unwrap(),
            Some(dek.to_vec())
        );
    }

    /// 不存在 → Ok(None)。
    #[test]
    fn memory_get_missing_is_none() {
        let kc = MemoryKeychain::new();
        assert_eq!(
            kc.get_secret(KeychainLabel::RootTokenSecret, "nobody")
                .unwrap(),
            None
        );
    }

    /// set 覆盖既有项。
    #[test]
    fn memory_set_overwrites() {
        let kc = MemoryKeychain::new();
        kc.set_secret(KeychainLabel::MasterDek, "acct", b"first")
            .unwrap();
        kc.set_secret(KeychainLabel::MasterDek, "acct", b"second")
            .unwrap();
        assert_eq!(
            kc.get_secret(KeychainLabel::MasterDek, "acct").unwrap(),
            Some(b"second".to_vec())
        );
    }

    /// delete 后 get → None。
    #[test]
    fn memory_delete_clears() {
        let kc = MemoryKeychain::new();
        kc.set_secret(KeychainLabel::MasterDek, "acct", b"v")
            .unwrap();
        kc.delete_secret(KeychainLabel::MasterDek, "acct").unwrap();
        assert_eq!(
            kc.get_secret(KeychainLabel::MasterDek, "acct").unwrap(),
            None
        );
    }

    /// delete 不存在 → 幂等 Ok(())。
    #[test]
    fn memory_delete_missing_is_ok() {
        let kc = MemoryKeychain::new();
        assert!(kc.delete_secret(KeychainLabel::MasterDek, "ghost").is_ok());
    }

    /// label 隔离：MasterDek 与 RootTokenSecret 同账户互不干扰。
    #[test]
    fn memory_label_isolation() {
        let kc = MemoryKeychain::new();
        kc.set_secret(KeychainLabel::MasterDek, "acct", b"dek")
            .unwrap();
        kc.set_secret(KeychainLabel::RootTokenSecret, "acct", b"tok")
            .unwrap();
        assert_eq!(
            kc.get_secret(KeychainLabel::MasterDek, "acct").unwrap(),
            Some(b"dek".to_vec())
        );
        assert_eq!(
            kc.get_secret(KeychainLabel::RootTokenSecret, "acct")
                .unwrap(),
            Some(b"tok".to_vec())
        );
    }

    /// account_id 隔离：两账户同 label 互不干扰。
    #[test]
    fn memory_account_isolation() {
        let kc = MemoryKeychain::new();
        kc.set_secret(KeychainLabel::MasterDek, "A", b"a").unwrap();
        kc.set_secret(KeychainLabel::MasterDek, "B", b"b").unwrap();
        assert_eq!(
            kc.get_secret(KeychainLabel::MasterDek, "A").unwrap(),
            Some(b"a".to_vec())
        );
        assert_eq!(
            kc.get_secret(KeychainLabel::MasterDek, "B").unwrap(),
            Some(b"b".to_vec())
        );
        // 删 A 不影响 B。
        kc.delete_secret(KeychainLabel::MasterDek, "A").unwrap();
        assert_eq!(kc.get_secret(KeychainLabel::MasterDek, "A").unwrap(), None);
        assert_eq!(
            kc.get_secret(KeychainLabel::MasterDek, "B").unwrap(),
            Some(b"b".to_vec())
        );
    }

    /// account 属性命名 = `{prefix}-{account_id}`。
    #[test]
    fn account_attribute_naming() {
        assert_eq!(
            KeychainLabel::MasterDek.account("uuid-123"),
            "master-dek-uuid-123"
        );
        assert_eq!(
            KeychainLabel::RootTokenSecret.account("uuid-123"),
            "root-token-uuid-123"
        );
        assert_eq!(
            KeychainLabel::ProviderCredentials.account("uuid-123"),
            "provider-credentials-uuid-123"
        );
    }

    /// MacKeychain 真实往返（文件 keychain，未签名可写）：set→get 命中→delete→get None。
    /// 验证 security-framework 接线（含 not-found 语义），不验证 Data Protection 归属。
    ///
    /// `#[ignore]`：触碰开发者真实登录 keychain，须手动跑（`cargo test -- --ignored`）。
    /// 测试用固定可辨识 account_id，跑完即删不留痕。
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "触碰真实系统 keychain，手动：cargo test -p seasnail-crypto -- --ignored mac_keychain_file_roundtrip"]
    fn mac_keychain_file_roundtrip() {
        let kc = MacKeychain::with_protected(false);
        let acct = "seasnail-test-m23-file-roundtrip";
        // 前置清理，避免上次中断残留。
        let _ = kc.delete_secret(KeychainLabel::MasterDek, acct);
        let dek = [0xcd; 32];
        kc.set_secret(KeychainLabel::MasterDek, acct, &dek).unwrap();
        assert_eq!(
            kc.get_secret(KeychainLabel::MasterDek, acct).unwrap(),
            Some(dek.to_vec())
        );
        kc.delete_secret(KeychainLabel::MasterDek, acct).unwrap();
        assert_eq!(kc.get_secret(KeychainLabel::MasterDek, acct).unwrap(), None);
    }

    /// 用可在未签名测试中运行的文件 Keychain 验证 provider vault 的 128 KiB
    /// set/get/overwrite/delete 接线。Data Protection 上的同一边界留给 ST-M9.3。
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "触碰真实系统 keychain，手动：cargo test -p seasnail-crypto -- --ignored mac_provider_vault_file_roundtrip_at_128_kib"]
    fn mac_provider_vault_file_roundtrip_at_128_kib() {
        let kc = MacKeychain::with_protected(false);
        let acct = "seasnail-test-m03-provider-vault-128k";
        let _ = kc.delete_secret(KeychainLabel::ProviderCredentials, acct);

        let maximum = vec![0xa5; 128 * 1024];
        kc.set_secret(KeychainLabel::ProviderCredentials, acct, &maximum)
            .unwrap();
        assert_eq!(
            kc.get_secret(KeychainLabel::ProviderCredentials, acct)
                .unwrap(),
            Some(maximum)
        );

        let replacement = b"replacement-vault".to_vec();
        kc.set_secret(KeychainLabel::ProviderCredentials, acct, &replacement)
            .unwrap();
        assert_eq!(
            kc.get_secret(KeychainLabel::ProviderCredentials, acct)
                .unwrap(),
            Some(replacement)
        );

        kc.delete_secret(KeychainLabel::ProviderCredentials, acct)
            .unwrap();
        assert_eq!(
            kc.get_secret(KeychainLabel::ProviderCredentials, acct)
                .unwrap(),
            None
        );
        kc.delete_secret(KeychainLabel::ProviderCredentials, acct)
            .unwrap();
    }

    /// MacKeychain 生产路径（Data Protection keychain）须签名 `.app` entitlement。
    /// 未签名裸二进制写入得 `errSecMissingEntitlement`——这是「仅签名 App 可读」的
    /// 预期行为，确认之即验证生产路径的 entitlement 依赖。真正归属与跨启动稳定
    /// 验证在 ST-M9.3 签名包内做。
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "触碰真实系统 keychain，手动：cargo test -p seasnail-crypto -- --ignored mac_keychain_protected_needs_entitlement"]
    fn mac_keychain_protected_needs_entitlement() {
        let kc = MacKeychain::new(); // protected = true
        let acct = "seasnail-test-m23-entitlement-check";
        let _ = kc.delete_secret(KeychainLabel::MasterDek, acct);
        let res = kc.set_secret(KeychainLabel::MasterDek, acct, &[0xcd; 32]);
        match res {
            Ok(()) => {
                // 已签名/已授权上下文：清理并标记需在 ST-M9.3 验证跨启动稳定。
                let _ = kc.delete_secret(KeychainLabel::MasterDek, acct);
                eprintln!(
                    "protected keychain 写入成功（上下文已有 entitlement）；\
                     跨启动稳定验证留待 ST-M9.3 签名包上机"
                );
            }
            Err(Error::Keychain(msg)) => {
                assert!(
                    msg.contains("entitlement") || msg.contains("Entitlement"),
                    "预期 entitlement 相关错误，实际: {msg}"
                );
                eprintln!("protected keychain 如预期拒绝未签名写入：{msg}");
            }
            Err(other) => panic!("预期 Keychain(entitlement) 错误，实际: {other:?}"),
        }
    }
}
