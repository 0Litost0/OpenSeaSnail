//! Provider config、credential state 与 cleanup settings 的账户级编排。

use seasnail_crypto::{CredentialEnvelope, CredentialSecret, ProviderCredentialStore};
use seasnail_storage::{
    self, CleanupSettingsRow, ProviderConfigInput, ProviderType, ReasoningProviderConfigRow,
};

use super::{AccountError, Storage};
use crate::cleanup::prompt::{compose_prompt, default_prompt, EffectivePrompt};
use crate::cleanup::CleanupExecutionSnapshot;
use crate::reasoning::ProviderSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialState {
    NotRequired,
    Missing,
    Bound,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfigView {
    pub id: String,
    pub name: String,
    pub provider_type: String,
    pub endpoint: String,
    pub endpoint_fingerprint: String,
    pub model: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub credential_state: CredentialState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupSettingsView {
    pub enabled: bool,
    pub selected_provider_config_id: Option<String>,
    pub custom_prompt: Option<String>,
    pub updated_at: i64,
    pub selected_credential_state: Option<CredentialState>,
}

impl Storage {
    /// 冻结当前账户 cleanup 配置、prompt 与 credential，供一次 pipeline execution 使用。
    pub fn cleanup_execution_snapshot(
        &self,
        session_id: &str,
    ) -> Result<CleanupExecutionSnapshot, AccountError> {
        let account_id = self.active_account()?;
        let settings = self.get_cleanup_settings()?;
        let prompt = match settings.custom_prompt.as_deref() {
            Some(custom) => {
                compose_prompt(custom).map_err(|_| invalid_mutation("invalid custom prompt"))?
            }
            None => default_prompt(),
        };
        if !settings.enabled {
            return Ok(CleanupExecutionSnapshot::disabled(
                account_id, session_id, prompt,
            ));
        }
        let provider = if let Some(id) = settings.selected_provider_config_id.as_deref() {
            let conn = self.open_db()?;
            match seasnail_storage::get_provider_config(&conn, id)? {
                Some(row) => {
                    let Some(provider_type) = ProviderType::parse(&row.provider_type) else {
                        return Err(invalid_mutation("invalid persisted provider type"));
                    };
                    let endpoint_fingerprint = decode_fingerprint(&row.endpoint_fingerprint)?;
                    Some(ProviderSnapshot {
                        config_id: row.id.clone(),
                        provider_type,
                        endpoint: row.endpoint,
                        endpoint_fingerprint,
                        model: row.model,
                        credential: self
                            .crypto
                            .provider_credentials()
                            .get_config_credential(&account_id, &row.id)?,
                    })
                }
                None => None,
            }
        } else {
            None
        };
        Ok(CleanupExecutionSnapshot {
            account_id,
            session_id: session_id.to_owned(),
            enabled: true,
            provider,
            prompt,
            local_transcription_elapsed_ms: 0,
        })
    }

    /// 读取指定配置及其当前 credential，供 Probe 和正式 cleanup 共享同一快照语义。
    pub fn provider_snapshot(&self, config_id: &str) -> Result<ProviderSnapshot, AccountError> {
        let account_id = self.active_account()?;
        let _guard = self.crypto.reasoning_guard();
        let conn = self.open_db()?;
        let row = seasnail_storage::get_provider_config(&conn, config_id)?
            .ok_or_else(|| invalid_mutation("provider config does not exist"))?;
        let provider_type = ProviderType::parse(&row.provider_type)
            .ok_or_else(|| invalid_mutation("invalid persisted provider type"))?;
        Ok(ProviderSnapshot {
            config_id: row.id.clone(),
            provider_type,
            endpoint: row.endpoint,
            endpoint_fingerprint: decode_fingerprint(&row.endpoint_fingerprint)?,
            model: row.model,
            credential: self
                .crypto
                .provider_credentials()
                .get_config_credential(&account_id, &row.id)?,
        })
    }

    /// 冻结 Prompt Studio 测试所需的 provider/credential/prompt。草稿只参与本次
    /// 快照；未提供草稿时读取正式设置，但两种情况都不执行任何持久化写入。
    pub fn cleanup_test_snapshot(
        &self,
        config_id: &str,
        prompt_draft: Option<&str>,
    ) -> Result<(String, ProviderSnapshot, EffectivePrompt), AccountError> {
        let account_id = self.active_account()?;
        let _guard = self.crypto.reasoning_guard();
        let conn = self.open_db()?;
        let row = seasnail_storage::get_provider_config(&conn, config_id)?
            .ok_or_else(|| invalid_mutation("provider config does not exist"))?;
        let provider_type = ProviderType::parse(&row.provider_type)
            .ok_or_else(|| invalid_mutation("invalid persisted provider type"))?;
        let prompt = match prompt_draft {
            Some(draft) => compose_prompt(draft)
                .map_err(|_| invalid_mutation("invalid custom prompt draft"))?,
            None => match seasnail_storage::get_cleanup_settings(&conn)?
                .and_then(|settings| settings.custom_prompt)
            {
                Some(custom) => compose_prompt(&custom)
                    .map_err(|_| invalid_mutation("invalid persisted custom prompt"))?,
                None => default_prompt(),
            },
        };
        let provider = ProviderSnapshot {
            config_id: row.id.clone(),
            provider_type,
            endpoint: row.endpoint,
            endpoint_fingerprint: decode_fingerprint(&row.endpoint_fingerprint)?,
            model: row.model,
            credential: self
                .crypto
                .provider_credentials()
                .get_config_credential(&account_id, &row.id)?,
        };
        Ok((account_id, provider, prompt))
    }

    pub fn create_provider_config(
        &self,
        input: ProviderConfigInput<'_>,
        now: i64,
    ) -> Result<ProviderConfigView, AccountError> {
        self.active_account()?;
        let _guard = self.crypto.reasoning_guard();
        let conn = self.open_db()?;
        let row = seasnail_storage::create_provider_config(&conn, input, now)?;
        self.provider_view(row)
    }

    pub fn replace_provider_config(
        &self,
        id: &str,
        input: ProviderConfigInput<'_>,
        now: i64,
    ) -> Result<Option<ProviderConfigView>, AccountError> {
        self.active_account()?;
        let _guard = self.crypto.reasoning_guard();
        let conn = self.open_db()?;
        seasnail_storage::replace_provider_config(&conn, id, input, now)?
            .map(|row| self.provider_view(row))
            .transpose()
    }

    pub fn list_provider_configs(&self) -> Result<Vec<ProviderConfigView>, AccountError> {
        self.active_account()?;
        let _guard = self.crypto.reasoning_guard();
        let conn = self.open_db()?;
        seasnail_storage::list_provider_configs(&conn)?
            .into_iter()
            .map(|row| self.provider_view(row))
            .collect()
    }

    pub fn set_provider_credential(
        &self,
        config_id: &str,
        secret: Vec<u8>,
    ) -> Result<CredentialState, AccountError> {
        let account_id = self.active_account()?;
        let _guard = self.crypto.reasoning_guard();
        let conn = self.open_db()?;
        let row = seasnail_storage::get_provider_config(&conn, config_id)?
            .ok_or_else(|| invalid_mutation("provider config does not exist"))?;
        let fingerprint = decode_fingerprint(&row.endpoint_fingerprint)?;
        let secret = CredentialSecret::new(secret).map_err(seasnail_crypto::Error::from)?;
        let credential = CredentialEnvelope::new(row.provider_type.clone(), fingerprint, secret)
            .map_err(seasnail_crypto::Error::from)?;
        self.crypto.provider_credentials().set_config_credential(
            &account_id,
            config_id,
            credential,
        )?;
        Ok(CredentialState::Bound)
    }

    /// 显式选择 self-hosted 无认证，或移除现有 credential。
    pub fn delete_provider_credential(
        &self,
        config_id: &str,
        now: i64,
    ) -> Result<CredentialState, AccountError> {
        let account_id = self.active_account()?;
        let _guard = self.crypto.reasoning_guard();
        let conn = self.open_db()?;
        seasnail_storage::get_provider_config(&conn, config_id)?
            .ok_or_else(|| invalid_mutation("provider config does not exist"))?;
        self.crypto
            .provider_credentials()
            .delete_config_credential(&account_id, config_id)?;
        let state = CredentialState::Missing;
        seasnail_storage::disable_cleanup_if_selected(&conn, config_id, now)?;
        Ok(state)
    }

    pub fn set_provider_no_auth(&self, config_id: &str) -> Result<CredentialState, AccountError> {
        let account_id = self.active_account()?;
        let _guard = self.crypto.reasoning_guard();
        let conn = self.open_db()?;
        let row = seasnail_storage::get_provider_config(&conn, config_id)?
            .ok_or_else(|| invalid_mutation("provider config does not exist"))?;
        let provider_type = ProviderType::parse(&row.provider_type)
            .ok_or_else(|| invalid_mutation("invalid persisted provider type"))?;
        if provider_type.requires_credential() {
            return Err(invalid_mutation("cloud provider cannot use no-auth mode"));
        }
        let fingerprint = decode_fingerprint(&row.endpoint_fingerprint)?;
        self.crypto.provider_credentials().set_config_no_auth(
            &account_id,
            config_id,
            row.provider_type,
            fingerprint,
        )?;
        Ok(CredentialState::NotRequired)
    }

    /// 固定跨存储顺序：先删 vault secret，再事务性删 DB config/清选择/关开关。
    pub fn delete_provider_config(&self, config_id: &str, now: i64) -> Result<bool, AccountError> {
        let account_id = self.active_account()?;
        let _guard = self.crypto.reasoning_guard();
        self.crypto
            .provider_credentials()
            .delete_config_credential(&account_id, config_id)?;
        let conn = self.open_db()?;
        Ok(seasnail_storage::remove_provider_config_and_disable(
            &conn, config_id, now,
        )?)
    }

    pub fn get_cleanup_settings(&self) -> Result<CleanupSettingsView, AccountError> {
        self.active_account()?;
        let _guard = self.crypto.reasoning_guard();
        let conn = self.open_db()?;
        let settings =
            seasnail_storage::get_cleanup_settings(&conn)?.unwrap_or(CleanupSettingsRow {
                enabled: false,
                selected_provider_config_id: None,
                custom_prompt: None,
                updated_at: 0,
            });
        let selected_credential_state = settings
            .selected_provider_config_id
            .as_deref()
            .map(|id| {
                seasnail_storage::get_provider_config(&conn, id)?
                    .map(|row| self.credential_state(&row))
                    .transpose()
            })
            .transpose()?
            .flatten();
        let effective_enabled = settings.enabled
            && matches!(
                selected_credential_state,
                Some(CredentialState::Bound | CredentialState::NotRequired)
            );
        Ok(CleanupSettingsView {
            enabled: effective_enabled,
            selected_provider_config_id: settings.selected_provider_config_id,
            custom_prompt: settings.custom_prompt,
            updated_at: settings.updated_at,
            selected_credential_state,
        })
    }

    pub fn save_cleanup_settings(
        &self,
        enabled: bool,
        selected_provider_config_id: Option<&str>,
        custom_prompt: Option<&str>,
        now: i64,
    ) -> Result<CleanupSettingsView, AccountError> {
        self.active_account()?;
        if let Some(custom_prompt) = custom_prompt {
            compose_prompt(custom_prompt).map_err(|_| invalid_mutation("invalid custom prompt"))?;
        }
        let _guard = self.crypto.reasoning_guard();
        let conn = self.open_db()?;
        let selected_state = selected_provider_config_id
            .map(|id| {
                let row = seasnail_storage::get_provider_config(&conn, id)?
                    .ok_or_else(|| invalid_mutation("selected provider config does not exist"))?;
                self.credential_state(&row)
            })
            .transpose()?;
        let usable = matches!(
            selected_state,
            Some(CredentialState::Bound | CredentialState::NotRequired)
        );
        let settings = seasnail_storage::save_cleanup_settings(
            &conn,
            enabled,
            selected_provider_config_id,
            custom_prompt,
            usable,
            now,
        )?;
        Ok(CleanupSettingsView {
            enabled: settings.enabled,
            selected_provider_config_id: settings.selected_provider_config_id,
            custom_prompt: settings.custom_prompt,
            updated_at: settings.updated_at,
            selected_credential_state: selected_state,
        })
    }

    fn provider_view(
        &self,
        row: ReasoningProviderConfigRow,
    ) -> Result<ProviderConfigView, AccountError> {
        let credential_state = self.credential_state(&row)?;
        Ok(ProviderConfigView {
            id: row.id,
            name: row.name,
            provider_type: row.provider_type,
            endpoint: row.endpoint,
            endpoint_fingerprint: row.endpoint_fingerprint,
            model: row.model,
            created_at: row.created_at,
            updated_at: row.updated_at,
            credential_state,
        })
    }

    fn credential_state(
        &self,
        row: &ReasoningProviderConfigRow,
    ) -> Result<CredentialState, AccountError> {
        let account_id = self.active_account()?;
        let provider_type = ProviderType::parse(&row.provider_type)
            .ok_or_else(|| invalid_mutation("invalid persisted provider type"))?;
        let expected_fingerprint = decode_fingerprint(&row.endpoint_fingerprint)?;
        let credential = self
            .crypto
            .provider_credentials()
            .get_config_credential(&account_id, &row.id)?;
        Ok(match credential {
            None => CredentialState::Missing,
            Some(credential)
                if credential.provider_type() == row.provider_type
                    && credential.endpoint_fingerprint() == &expected_fingerprint =>
            {
                if credential.is_no_auth() {
                    if provider_type.requires_credential() {
                        CredentialState::Missing
                    } else {
                        CredentialState::NotRequired
                    }
                } else {
                    CredentialState::Bound
                }
            }
            Some(_) => CredentialState::Stale,
        })
    }
}

fn decode_fingerprint(value: &str) -> Result<[u8; 32], AccountError> {
    if value.len() != 64 {
        return Err(invalid_mutation("invalid endpoint fingerprint"));
    }
    let mut bytes = [0_u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| invalid_mutation("invalid endpoint fingerprint"))?;
    }
    Ok(bytes)
}

fn invalid_mutation(message: &str) -> AccountError {
    AccountError::Storage(seasnail_storage::Error::InvalidMutation(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::Crypto;
    use seasnail_crypto::{
        Argon2Params, Error as CryptoError, KeychainLabel, KeychainStore, MemoryKeychain,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    struct FailingProviderDeleteKeychain {
        inner: MemoryKeychain,
        fail_provider_delete: AtomicBool,
    }

    impl FailingProviderDeleteKeychain {
        fn new() -> Self {
            Self {
                inner: MemoryKeychain::new(),
                fail_provider_delete: AtomicBool::new(false),
            }
        }
    }

    impl KeychainStore for FailingProviderDeleteKeychain {
        fn set_secret(
            &self,
            label: KeychainLabel,
            account_id: &str,
            value: &[u8],
        ) -> Result<(), CryptoError> {
            self.inner.set_secret(label, account_id, value)
        }

        fn get_secret(
            &self,
            label: KeychainLabel,
            account_id: &str,
        ) -> Result<Option<Vec<u8>>, CryptoError> {
            self.inner.get_secret(label, account_id)
        }

        fn delete_secret(&self, label: KeychainLabel, account_id: &str) -> Result<(), CryptoError> {
            if label == KeychainLabel::ProviderCredentials
                && self.fail_provider_delete.load(Ordering::SeqCst)
            {
                return Err(CryptoError::Keychain(
                    "injected provider delete failure".into(),
                ));
            }
            self.inner.delete_secret(label, account_id)
        }
    }

    fn crypto_with_keychain(
        dir: &tempfile::TempDir,
        keychain: Arc<dyn KeychainStore>,
    ) -> Arc<Crypto> {
        Arc::new(
            Crypto::new(
                dir.path().to_path_buf(),
                keychain,
                Argon2Params {
                    m_kib: 8192,
                    t_cost: 1,
                    p_cost: 1,
                },
            )
            .unwrap(),
        )
    }

    fn openai_input<'a>(endpoint: &'a str, model: &'a str) -> ProviderConfigInput<'a> {
        ProviderConfigInput {
            name: "Primary",
            provider_type: ProviderType::OpenAi,
            endpoint,
            model,
        }
    }

    #[test]
    fn settings_default_off_and_require_usable_selected_provider() {
        let dir = tempfile::tempdir().unwrap();
        let crypto = crypto_with_keychain(&dir, Arc::new(MemoryKeychain::new()));
        crypto.setup_first_account("alice", "pw").unwrap();
        let storage = Storage::new(crypto);
        assert_eq!(
            storage.get_cleanup_settings().unwrap(),
            CleanupSettingsView {
                enabled: false,
                selected_provider_config_id: None,
                custom_prompt: None,
                updated_at: 0,
                selected_credential_state: None,
            }
        );

        let config = storage
            .create_provider_config(openai_input("https://api.openai.com/v1", "manual-model"), 1)
            .unwrap();
        assert_eq!(config.credential_state, CredentialState::Missing);
        assert!(storage
            .save_cleanup_settings(true, Some(&config.id), None, 2)
            .is_err());
        storage
            .set_provider_credential(&config.id, b"secret".to_vec())
            .unwrap();
        let enabled = storage
            .save_cleanup_settings(true, Some(&config.id), Some("custom"), 3)
            .unwrap();
        assert!(enabled.enabled);
        assert_eq!(
            enabled.selected_credential_state,
            Some(CredentialState::Bound)
        );

        let model_only = storage
            .replace_provider_config(
                &config.id,
                openai_input("https://api.openai.com/v1", "new-model"),
                4,
            )
            .unwrap()
            .unwrap();
        assert_eq!(model_only.credential_state, CredentialState::Bound);
        assert_eq!(
            storage.delete_provider_credential(&config.id, 5).unwrap(),
            CredentialState::Missing
        );
        assert!(!storage.get_cleanup_settings().unwrap().enabled);
    }

    #[test]
    fn settings_reject_prompt_values_that_execution_would_not_use() {
        let dir = tempfile::tempdir().unwrap();
        let crypto = crypto_with_keychain(&dir, Arc::new(MemoryKeychain::new()));
        crypto.setup_first_account("alice", "pw").unwrap();
        let storage = Storage::new(crypto);

        for invalid in [" \n\t", "bad\u{0001}prompt"] {
            assert!(storage
                .save_cleanup_settings(false, None, Some(invalid), 1)
                .is_err());
        }
        let settings = storage
            .save_cleanup_settings(false, None, Some("整理中文听写。\n保留原意。"), 2)
            .unwrap();
        assert_eq!(
            settings.custom_prompt.as_deref(),
            Some("整理中文听写。\n保留原意。")
        );
        let snapshot = storage
            .cleanup_execution_snapshot(&uuid::Uuid::new_v4().to_string())
            .unwrap();
        assert!(snapshot.prompt.text.starts_with("整理中文听写。"));
    }

    #[test]
    fn endpoint_change_becomes_stale_and_self_hosted_can_explicitly_use_no_auth() {
        let dir = tempfile::tempdir().unwrap();
        let crypto = crypto_with_keychain(&dir, Arc::new(MemoryKeychain::new()));
        crypto.setup_first_account("alice", "pw").unwrap();
        let storage = Storage::new(crypto);
        let config = storage
            .create_provider_config(
                ProviderConfigInput {
                    name: "Hosted",
                    provider_type: ProviderType::SelfHostedPrivate,
                    endpoint: "http://127.0.0.1:8080/v1",
                    model: "model",
                },
                1,
            )
            .unwrap();
        assert_eq!(config.credential_state, CredentialState::Missing);
        assert_eq!(
            storage.set_provider_no_auth(&config.id).unwrap(),
            CredentialState::NotRequired
        );
        assert!(
            storage
                .save_cleanup_settings(true, Some(&config.id), None, 2)
                .unwrap()
                .enabled
        );
        storage
            .set_provider_credential(&config.id, b"optional-secret".to_vec())
            .unwrap();
        let changed = storage
            .replace_provider_config(
                &config.id,
                ProviderConfigInput {
                    name: "Hosted",
                    provider_type: ProviderType::SelfHostedPrivate,
                    endpoint: "https://private.example/v1",
                    model: "model",
                },
                3,
            )
            .unwrap()
            .unwrap();
        assert_eq!(changed.credential_state, CredentialState::Stale);
        assert!(!storage.get_cleanup_settings().unwrap().enabled);
        assert!(storage
            .save_cleanup_settings(true, Some(&config.id), None, 4)
            .is_err());
        assert_eq!(
            storage.delete_provider_credential(&config.id, 5).unwrap(),
            CredentialState::Missing
        );
    }

    #[test]
    fn provider_delete_failure_order_keeps_config_manageable_and_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let keychain = Arc::new(FailingProviderDeleteKeychain::new());
        let crypto = crypto_with_keychain(&dir, keychain.clone());
        crypto.setup_first_account("alice", "pw").unwrap();
        let storage = Storage::new(crypto);
        let config = storage
            .create_provider_config(openai_input("https://api.openai.com/v1", "model"), 1)
            .unwrap();
        storage
            .set_provider_credential(&config.id, b"secret".to_vec())
            .unwrap();
        storage
            .save_cleanup_settings(true, Some(&config.id), None, 2)
            .unwrap();

        keychain.fail_provider_delete.store(true, Ordering::SeqCst);
        assert!(storage.delete_provider_config(&config.id, 3).is_err());
        assert_eq!(storage.list_provider_configs().unwrap().len(), 1);
        assert!(storage.get_cleanup_settings().unwrap().enabled);

        keychain.fail_provider_delete.store(false, Ordering::SeqCst);
        let conn = storage.open_db().unwrap();
        conn.execute_batch(
            "CREATE TRIGGER injected_provider_delete_failure
             BEFORE DELETE ON reasoning_provider_configs
             BEGIN SELECT RAISE(ABORT, 'injected'); END;",
        )
        .unwrap();
        assert!(storage.delete_provider_config(&config.id, 4).is_err());
        assert_eq!(
            storage.list_provider_configs().unwrap()[0].credential_state,
            CredentialState::Missing
        );
        assert!(!storage.get_cleanup_settings().unwrap().enabled);
        conn.execute_batch("DROP TRIGGER injected_provider_delete_failure")
            .unwrap();
        assert!(storage.delete_provider_config(&config.id, 5).unwrap());
        assert!(storage.list_provider_configs().unwrap().is_empty());
        let settings = storage.get_cleanup_settings().unwrap();
        assert!(!settings.enabled);
        assert_eq!(settings.selected_provider_config_id, None);
    }

    #[test]
    fn account_delete_removes_provider_vault_first_and_retries_after_failure() {
        let dir = tempfile::tempdir().unwrap();
        let keychain = Arc::new(FailingProviderDeleteKeychain::new());
        let crypto = crypto_with_keychain(&dir, keychain.clone());
        let alice = crypto.setup_first_account("alice", "p1").unwrap();
        let alice_keys = crypto.snapshot_active_account(&alice.account_id).unwrap();
        let alice_storage = Storage::bind(Arc::clone(&crypto), alice_keys);
        let config = alice_storage
            .create_provider_config(openai_input("https://api.openai.com/v1", "model"), 1)
            .unwrap();
        alice_storage
            .set_provider_credential(&config.id, b"secret".to_vec())
            .unwrap();
        crypto.create_account("bob", "p2").unwrap();

        keychain.fail_provider_delete.store(true, Ordering::SeqCst);
        assert!(crypto.delete_account(&alice.account_id).is_err());
        assert!(crypto.account_dir(&alice.account_id).exists());
        assert!(crypto
            .list_accounts()
            .iter()
            .any(|account| account.id == alice.account_id));

        keychain.fail_provider_delete.store(false, Ordering::SeqCst);
        let registry_path = dir.path().join("data/accounts.json");
        let registry_backup = dir.path().join("data/accounts.json.backup");
        std::fs::rename(&registry_path, &registry_backup).unwrap();
        std::fs::create_dir_all(&registry_path).unwrap();
        assert!(crypto.delete_account(&alice.account_id).is_err());
        assert!(crypto
            .list_accounts()
            .iter()
            .any(|account| account.id == alice.account_id));
        std::fs::remove_dir_all(&registry_path).unwrap();
        std::fs::rename(&registry_backup, &registry_path).unwrap();
        crypto.delete_account(&alice.account_id).unwrap();
        assert_eq!(
            keychain
                .get_secret(KeychainLabel::ProviderCredentials, &alice.account_id)
                .unwrap(),
            None
        );
        assert!(!crypto.account_dir(&alice.account_id).exists());
        assert!(!crypto
            .list_accounts()
            .iter()
            .any(|account| account.id == alice.account_id));
    }
}
