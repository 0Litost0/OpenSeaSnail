//! 账户级 Provider credential vault。
//!
//! 每个账户只占用一个固定 Keychain item；所有 config credential 通过版本化二进制
//! codec 保存。读改写由进程内互斥锁串行化，避免同进程并发覆盖。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use zeroize::{Zeroize, Zeroizing};

use crate::{Error, KeychainLabel, KeychainStore};

const MAGIC: &[u8; 4] = b"SSPV";
const VERSION: u16 = 1;
const MAX_CONFIGS: usize = 32;
const MAX_PROVIDER_TYPE_BYTES: usize = 64;
const MAX_SECRET_BYTES: usize = 8 * 1024;
const MAX_ENCODED_BYTES: usize = 128 * 1024;

/// Vault 输入或持久化内容不合法。错误信息只描述结构，不包含 secret 数据。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CredentialVaultError {
    #[error("account_id must be a canonical UUID")]
    InvalidAccountId,
    #[error("config_id must be a canonical UUID")]
    InvalidConfigId,
    #[error("provider_type is invalid")]
    InvalidProviderType,
    #[error("credential secret length must be within 1..=8192 bytes")]
    InvalidSecretLength,
    #[error("provider credential vault exceeds 32 configurations")]
    TooManyConfigs,
    #[error("provider credential vault exceeds 128 KiB")]
    VaultTooLarge,
    #[error("provider credential vault magic is invalid")]
    InvalidMagic,
    #[error("provider credential vault version is unsupported")]
    UnsupportedVersion,
    #[error("provider credential vault is malformed")]
    Malformed,
    #[error("provider credential vault contains a duplicate config id")]
    DuplicateConfig,
}

/// 进程内 credential secret。离开作用域时尽力清零；刻意不实现 Debug/Display/Clone。
pub struct CredentialSecret(Vec<u8>);

impl CredentialSecret {
    pub fn new(value: Vec<u8>) -> Result<Self, CredentialVaultError> {
        if value.is_empty()
            || value.len() > MAX_SECRET_BYTES
            || http::HeaderValue::from_bytes(&value).is_err()
        {
            let mut value = value;
            value.zeroize();
            return Err(CredentialVaultError::InvalidSecretLength);
        }
        Ok(Self(value))
    }

    pub fn expose_secret(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for CredentialSecret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// 与 config 当前 endpoint/provider type 绑定的 credential。
///
/// 字段私有，避免无意结构化日志；secret 仅能显式借用。
pub struct CredentialEnvelope {
    provider_type: String,
    endpoint_fingerprint: [u8; 32],
    secret: Option<CredentialSecret>,
}

impl CredentialEnvelope {
    pub fn new(
        provider_type: String,
        endpoint_fingerprint: [u8; 32],
        secret: CredentialSecret,
    ) -> Result<Self, CredentialVaultError> {
        validate_provider_type(&provider_type)?;
        Ok(Self {
            provider_type,
            endpoint_fingerprint,
            secret: Some(secret),
        })
    }

    pub fn no_auth(
        provider_type: String,
        endpoint_fingerprint: [u8; 32],
    ) -> Result<Self, CredentialVaultError> {
        validate_provider_type(&provider_type)?;
        Ok(Self {
            provider_type,
            endpoint_fingerprint,
            secret: None,
        })
    }

    pub fn provider_type(&self) -> &str {
        &self.provider_type
    }

    pub fn endpoint_fingerprint(&self) -> &[u8; 32] {
        &self.endpoint_fingerprint
    }

    pub fn secret(&self) -> Option<&CredentialSecret> {
        self.secret.as_ref()
    }

    pub fn is_no_auth(&self) -> bool {
        self.secret.is_none()
    }
}

/// Provider credential 的账户/config 维度存取接口。
pub trait ProviderCredentialStore: Send + Sync {
    fn get_config_credential(
        &self,
        account_id: &str,
        config_id: &str,
    ) -> Result<Option<CredentialEnvelope>, Error>;

    fn set_config_credential(
        &self,
        account_id: &str,
        config_id: &str,
        credential: CredentialEnvelope,
    ) -> Result<(), Error>;

    fn set_config_no_auth(
        &self,
        account_id: &str,
        config_id: &str,
        provider_type: String,
        endpoint_fingerprint: [u8; 32],
    ) -> Result<(), Error>;

    fn delete_config_credential(&self, account_id: &str, config_id: &str) -> Result<bool, Error>;

    fn delete_account_credentials(&self, account_id: &str) -> Result<(), Error>;
}

/// 基于现有 [`KeychainStore`] 的正式 vault 实现，可注入 MemoryKeychain 或 MacKeychain。
pub struct KeychainProviderCredentialStore {
    keychain: Arc<dyn KeychainStore>,
}

impl KeychainProviderCredentialStore {
    pub fn new(keychain: Arc<dyn KeychainStore>) -> Self {
        Self { keychain }
    }

    fn read_vault(
        &self,
        account_id: &str,
    ) -> Result<BTreeMap<[u8; 16], CredentialEnvelope>, Error> {
        let Some(bytes) = self
            .keychain
            .get_secret(KeychainLabel::ProviderCredentials, account_id)?
        else {
            return Ok(BTreeMap::new());
        };
        let bytes = Zeroizing::new(bytes);
        decode_vault(&bytes).map_err(Into::into)
    }

    fn write_vault(
        &self,
        account_id: &str,
        vault: &BTreeMap<[u8; 16], CredentialEnvelope>,
    ) -> Result<(), Error> {
        if vault.is_empty() {
            return self
                .keychain
                .delete_secret(KeychainLabel::ProviderCredentials, account_id);
        }
        let encoded = Zeroizing::new(encode_vault(vault)?);
        self.keychain
            .set_secret(KeychainLabel::ProviderCredentials, account_id, &encoded)
    }
}

impl ProviderCredentialStore for KeychainProviderCredentialStore {
    fn get_config_credential(
        &self,
        account_id: &str,
        config_id: &str,
    ) -> Result<Option<CredentialEnvelope>, Error> {
        validate_account_id(account_id)?;
        let config_id = parse_config_id(config_id)?;
        let _guard = vault_lock()
            .lock()
            .expect("provider credential mutex poisoned");
        Ok(self.read_vault(account_id)?.remove(&config_id))
    }

    fn set_config_credential(
        &self,
        account_id: &str,
        config_id: &str,
        credential: CredentialEnvelope,
    ) -> Result<(), Error> {
        validate_account_id(account_id)?;
        let config_id = parse_config_id(config_id)?;
        let _guard = vault_lock()
            .lock()
            .expect("provider credential mutex poisoned");
        let mut vault = self.read_vault(account_id)?;
        if !vault.contains_key(&config_id) && vault.len() == MAX_CONFIGS {
            return Err(CredentialVaultError::TooManyConfigs.into());
        }
        vault.insert(config_id, credential);
        self.write_vault(account_id, &vault)
    }

    fn set_config_no_auth(
        &self,
        account_id: &str,
        config_id: &str,
        provider_type: String,
        endpoint_fingerprint: [u8; 32],
    ) -> Result<(), Error> {
        self.set_config_credential(
            account_id,
            config_id,
            CredentialEnvelope::no_auth(provider_type, endpoint_fingerprint)?,
        )
    }

    fn delete_config_credential(&self, account_id: &str, config_id: &str) -> Result<bool, Error> {
        validate_account_id(account_id)?;
        let config_id = parse_config_id(config_id)?;
        let _guard = vault_lock()
            .lock()
            .expect("provider credential mutex poisoned");
        let mut vault = self.read_vault(account_id)?;
        let removed = vault.remove(&config_id).is_some();
        if removed {
            self.write_vault(account_id, &vault)?;
        }
        Ok(removed)
    }

    fn delete_account_credentials(&self, account_id: &str) -> Result<(), Error> {
        validate_account_id(account_id)?;
        let _guard = vault_lock()
            .lock()
            .expect("provider credential mutex poisoned");
        self.keychain
            .delete_secret(KeychainLabel::ProviderCredentials, account_id)
    }
}

fn vault_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn validate_account_id(value: &str) -> Result<(), CredentialVaultError> {
    if canonical_uuid(value).is_some() {
        Ok(())
    } else {
        Err(CredentialVaultError::InvalidAccountId)
    }
}

fn parse_config_id(value: &str) -> Result<[u8; 16], CredentialVaultError> {
    canonical_uuid(value)
        .map(|uuid| *uuid.as_bytes())
        .ok_or(CredentialVaultError::InvalidConfigId)
}

fn canonical_uuid(value: &str) -> Option<uuid::Uuid> {
    uuid::Uuid::parse_str(value)
        .ok()
        .filter(|parsed| parsed.hyphenated().to_string() == value)
}

fn validate_provider_type(value: &str) -> Result<(), CredentialVaultError> {
    if value.is_empty()
        || value.len() > MAX_PROVIDER_TYPE_BYTES
        || value.chars().any(char::is_control)
    {
        Err(CredentialVaultError::InvalidProviderType)
    } else {
        Ok(())
    }
}

fn encode_vault(
    vault: &BTreeMap<[u8; 16], CredentialEnvelope>,
) -> Result<Vec<u8>, CredentialVaultError> {
    if vault.len() > MAX_CONFIGS {
        return Err(CredentialVaultError::TooManyConfigs);
    }
    let mut bytes = Vec::new();
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&VERSION.to_be_bytes());
    bytes.extend_from_slice(&(vault.len() as u16).to_be_bytes());
    for (config_id, credential) in vault {
        validate_provider_type(&credential.provider_type)?;
        let secret_len = credential
            .secret
            .as_ref()
            .map_or(0, |secret| secret.0.len());
        if secret_len > MAX_SECRET_BYTES {
            return Err(CredentialVaultError::InvalidSecretLength);
        }
        bytes.extend_from_slice(config_id);
        bytes.extend_from_slice(&(credential.provider_type.len() as u16).to_be_bytes());
        bytes.extend_from_slice(credential.provider_type.as_bytes());
        bytes.extend_from_slice(&credential.endpoint_fingerprint);
        bytes.extend_from_slice(&(secret_len as u16).to_be_bytes());
        if let Some(secret) = &credential.secret {
            bytes.extend_from_slice(&secret.0);
        }
        if bytes.len() > MAX_ENCODED_BYTES {
            return Err(CredentialVaultError::VaultTooLarge);
        }
    }
    Ok(bytes)
}

fn decode_vault(
    bytes: &[u8],
) -> Result<BTreeMap<[u8; 16], CredentialEnvelope>, CredentialVaultError> {
    if bytes.len() > MAX_ENCODED_BYTES {
        return Err(CredentialVaultError::VaultTooLarge);
    }
    if bytes.len() < 8 || &bytes[..4] != MAGIC {
        return Err(CredentialVaultError::InvalidMagic);
    }
    if u16::from_be_bytes([bytes[4], bytes[5]]) != VERSION {
        return Err(CredentialVaultError::UnsupportedVersion);
    }
    let count = u16::from_be_bytes([bytes[6], bytes[7]]) as usize;
    if count > MAX_CONFIGS {
        return Err(CredentialVaultError::TooManyConfigs);
    }
    let mut cursor = 8;
    let mut vault = BTreeMap::new();
    for _ in 0..count {
        let config_id = take_array::<16>(bytes, &mut cursor)?;
        let provider_len = take_u16(bytes, &mut cursor)? as usize;
        if provider_len == 0 || provider_len > MAX_PROVIDER_TYPE_BYTES {
            return Err(CredentialVaultError::InvalidProviderType);
        }
        let provider_type = take(bytes, &mut cursor, provider_len)?;
        let provider_type = std::str::from_utf8(provider_type)
            .map_err(|_| CredentialVaultError::InvalidProviderType)?
            .to_owned();
        validate_provider_type(&provider_type)?;
        let endpoint_fingerprint = take_array::<32>(bytes, &mut cursor)?;
        let secret_len = take_u16(bytes, &mut cursor)? as usize;
        let secret = take(bytes, &mut cursor, secret_len)?;
        let credential = if secret_len == 0 {
            CredentialEnvelope::no_auth(provider_type, endpoint_fingerprint)?
        } else {
            CredentialEnvelope::new(
                provider_type,
                endpoint_fingerprint,
                CredentialSecret::new(secret.to_vec())?,
            )?
        };
        if vault.insert(config_id, credential).is_some() {
            return Err(CredentialVaultError::DuplicateConfig);
        }
    }
    if cursor != bytes.len() {
        return Err(CredentialVaultError::Malformed);
    }
    Ok(vault)
}

fn take<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
    length: usize,
) -> Result<&'a [u8], CredentialVaultError> {
    let end = cursor
        .checked_add(length)
        .ok_or(CredentialVaultError::Malformed)?;
    let value = bytes
        .get(*cursor..end)
        .ok_or(CredentialVaultError::Malformed)?;
    *cursor = end;
    Ok(value)
}

fn take_array<const N: usize>(
    bytes: &[u8],
    cursor: &mut usize,
) -> Result<[u8; N], CredentialVaultError> {
    take(bytes, cursor, N)?
        .try_into()
        .map_err(|_| CredentialVaultError::Malformed)
}

fn take_u16(bytes: &[u8], cursor: &mut usize) -> Result<u16, CredentialVaultError> {
    Ok(u16::from_be_bytes(take_array::<2>(bytes, cursor)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryKeychain;
    use static_assertions::assert_not_impl_any;

    assert_not_impl_any!(CredentialSecret: std::fmt::Debug, std::fmt::Display, Clone);
    assert_not_impl_any!(CredentialEnvelope: std::fmt::Debug, std::fmt::Display, Clone);

    fn ids(index: u128) -> (String, String) {
        (
            "00000000-0000-4000-8000-000000000001".into(),
            uuid::Uuid::from_u128(0x00000000_0000_4000_8000_000000000100 + index).to_string(),
        )
    }

    fn envelope(secret_len: usize, marker: u8) -> CredentialEnvelope {
        let secret_byte = b'a' + marker % 26;
        CredentialEnvelope::new(
            "openai".into(),
            [marker; 32],
            CredentialSecret::new(vec![secret_byte; secret_len]).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn account_and_config_are_isolated_and_set_overwrites() {
        let keychain = Arc::new(MemoryKeychain::new());
        let store = KeychainProviderCredentialStore::new(keychain);
        let (account_a, config_a) = ids(1);
        let account_b = "00000000-0000-4000-8000-000000000002";

        store
            .set_config_credential(&account_a, &config_a, envelope(4, 1))
            .unwrap();
        assert!(store
            .get_config_credential(account_b, &config_a)
            .unwrap()
            .is_none());
        store
            .set_config_credential(&account_a, &config_a, envelope(5, 2))
            .unwrap();
        let value = store
            .get_config_credential(&account_a, &config_a)
            .unwrap()
            .unwrap();
        assert_eq!(value.provider_type(), "openai");
        assert_eq!(value.endpoint_fingerprint(), &[2; 32]);
        assert_eq!(value.secret().unwrap().expose_secret(), &[b'c'; 5]);
    }

    #[test]
    fn config_and_account_deletion_are_idempotent_and_isolated() {
        let keychain = Arc::new(MemoryKeychain::new());
        let store = KeychainProviderCredentialStore::new(keychain);
        let (account_a, config_a) = ids(1);
        let account_b = "00000000-0000-4000-8000-000000000002";
        let (_, config_b) = ids(2);
        store
            .set_config_credential(&account_a, &config_a, envelope(1, 1))
            .unwrap();
        store
            .set_config_credential(account_b, &config_b, envelope(1, 2))
            .unwrap();
        assert!(store
            .delete_config_credential(&account_a, &config_a)
            .unwrap());
        assert!(!store
            .delete_config_credential(&account_a, &config_a)
            .unwrap());
        store.delete_account_credentials(&account_a).unwrap();
        assert!(store
            .get_config_credential(account_b, &config_b)
            .unwrap()
            .is_some());
        store.delete_account_credentials(account_b).unwrap();
        store.delete_account_credentials(account_b).unwrap();
    }

    #[test]
    fn validates_ids_secret_and_provider_type() {
        let store = KeychainProviderCredentialStore::new(Arc::new(MemoryKeychain::new()));
        let (account, config) = ids(1);
        assert!(matches!(
            store.get_config_credential("ACCOUNT", &config),
            Err(Error::CredentialVault(
                CredentialVaultError::InvalidAccountId
            ))
        ));
        assert!(matches!(
            store.get_config_credential(&account, "not-a-uuid"),
            Err(Error::CredentialVault(
                CredentialVaultError::InvalidConfigId
            ))
        ));
        assert!(matches!(
            CredentialSecret::new(Vec::new()),
            Err(CredentialVaultError::InvalidSecretLength)
        ));
        assert!(matches!(
            CredentialSecret::new(vec![0; MAX_SECRET_BYTES + 1]),
            Err(CredentialVaultError::InvalidSecretLength)
        ));
        assert!(CredentialSecret::new(vec![b'k'; MAX_SECRET_BYTES]).is_ok());
        assert!(matches!(
            CredentialSecret::new(b"secret\nheader-injection".to_vec()),
            Err(CredentialVaultError::InvalidSecretLength)
        ));
        assert!(matches!(
            CredentialEnvelope::new(
                "bad\nprovider".into(),
                [0; 32],
                CredentialSecret::new(vec![b'x']).unwrap(),
            ),
            Err(CredentialVaultError::InvalidProviderType)
        ));
    }

    #[test]
    fn rejects_corrupt_version_and_malformed_vault() {
        let keychain = Arc::new(MemoryKeychain::new());
        let store = KeychainProviderCredentialStore::new(keychain.clone());
        let (account, config) = ids(1);
        keychain
            .set_secret(
                KeychainLabel::ProviderCredentials,
                &account,
                b"SSPV\0\x02\0\0",
            )
            .unwrap();
        assert!(matches!(
            store.get_config_credential(&account, &config),
            Err(Error::CredentialVault(
                CredentialVaultError::UnsupportedVersion
            ))
        ));
        keychain
            .set_secret(
                KeychainLabel::ProviderCredentials,
                &account,
                b"SSPV\0\x01\0\x01",
            )
            .unwrap();
        assert!(matches!(
            store.get_config_credential(&account, &config),
            Err(Error::CredentialVault(CredentialVaultError::Malformed))
        ));
    }

    #[test]
    fn enforces_config_count_without_overwriting_existing_vault() {
        let store = KeychainProviderCredentialStore::new(Arc::new(MemoryKeychain::new()));
        let (account, _) = ids(0);
        for index in 0..MAX_CONFIGS as u128 {
            let (_, config) = ids(index);
            store
                .set_config_credential(&account, &config, envelope(1, index as u8))
                .unwrap();
        }
        let (_, extra) = ids(MAX_CONFIGS as u128);
        assert!(matches!(
            store.set_config_credential(&account, &extra, envelope(1, 99)),
            Err(Error::CredentialVault(CredentialVaultError::TooManyConfigs))
        ));
        let (_, first) = ids(0);
        assert!(store
            .get_config_credential(&account, &first)
            .unwrap()
            .is_some());
    }

    #[test]
    fn accepts_exact_128_kib_and_rejects_one_byte_more_without_data_loss() {
        let store = KeychainProviderCredentialStore::new(Arc::new(MemoryKeychain::new()));
        let (account, _) = ids(0);
        for index in 0..15_u128 {
            let (_, config) = ids(index);
            store
                .set_config_credential(&account, &config, envelope(MAX_SECRET_BYTES, index as u8))
                .unwrap();
        }
        let (_, boundary) = ids(15);
        store
            .set_config_credential(&account, &boundary, envelope(7_256, 15))
            .unwrap();
        assert_eq!(
            store
                .read_vault(&account)
                .and_then(|vault| Ok(encode_vault(&vault)?))
                .unwrap()
                .len(),
            MAX_ENCODED_BYTES
        );
        assert!(matches!(
            store.set_config_credential(&account, &boundary, envelope(7_257, 16)),
            Err(Error::CredentialVault(CredentialVaultError::VaultTooLarge))
        ));
        assert_eq!(
            store
                .get_config_credential(&account, &boundary)
                .unwrap()
                .unwrap()
                .secret()
                .unwrap()
                .expose_secret()
                .len(),
            7_256
        );
    }
}
