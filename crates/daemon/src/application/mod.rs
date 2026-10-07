//! HTTP 无关的 daemon 应用层边界。

mod dictionary;
mod error;
mod final_text;
mod learning;
mod model_catalog;
mod model_components;
mod services;
mod transcription;

pub use dictionary::{
    normalize_dictionary_query, normalize_dictionary_term, DictionaryEntry, DictionaryError,
    DictionaryImportPreview, DictionaryImportResult, DictionaryMutationResult, DictionaryPage,
    DictionaryService, DictionarySnapshot, LearningMutationResult, MAX_CSV_BYTES,
};
pub use error::ApplicationError;
pub(crate) use final_text::{
    preview as final_text_preview, resolve as resolve_final_text, snippet as final_text_snippet,
    ResolvedFinalText,
};
pub use learning::{LearningTicketData, LearningTicketError, LearningTicketRegistry};
pub(crate) use model_catalog::{safe_artifact_key, ManifestEntry};
pub use model_catalog::{
    ModelCatalog, ModelComponentResult, ModelDiarization, ModelRuntimeKind, ModelStatusResult,
    ModelSummaryResult, TimestampCapability,
};
pub(crate) use services::ModelRuntimeBuilder;
pub use services::{
    AccountService, ApplicationServices, AuthService, ExportService, ModelService, SessionService,
    TokenService,
};
pub use transcription::TranscriptionService;

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::account::coordination::{AccountLeaseGuard, IdentityCoordination};
use crate::account::{Auth, Crypto};

/// HTTP 无关的 runtime 构造失败分类。adapter 可以从具体资源/descriptor 错误构造
/// 该类型，应用服务再决定冲突（用户可修复）或内部错误语义，避免经 `String` 丢失类别。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ModelRuntimeBuildError {
    Unavailable(String),
    InvalidArtifact(String),
    Internal(String),
}

/// 已认证调用方及其固定账户数据能力。repository 在认证时一次性绑定；后续账户
/// 切换不会把已有请求重定向到新账户。
#[derive(Clone)]
pub struct CallerContext {
    account_id: String,
    token_id: String,
    is_root: bool,
    scopes: BTreeSet<String>,
    repository: AccountScopedRepository,
}

impl fmt::Debug for CallerContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallerContext")
            .field("account_id", &self.account_id)
            .field("token_id", &self.token_id)
            .field("is_root", &self.is_root)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

impl CallerContext {
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    pub fn token_id(&self) -> &str {
        &self.token_id
    }

    pub fn is_root(&self) -> bool {
        self.is_root
    }

    pub fn scopes(&self) -> &BTreeSet<String> {
        &self.scopes
    }

    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.contains(scope)
    }

    pub(crate) fn repository(&self) -> &AccountScopedRepository {
        &self.repository
    }

    pub(crate) fn repository_clone(&self) -> AccountScopedRepository {
        self.repository.clone()
    }
}

#[derive(Clone)]
pub struct CreateTranscriptionCommand {
    pub caller: CallerContext,
    pub audio: Vec<u8>,
    pub file_name: String,
    pub source: String,
    pub language: Option<String>,
    pub input_device: Option<String>,
    pub clipboard_context: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedTranscription {
    pub session_id: String,
    pub status: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountSummaryResult {
    pub id: String,
    pub username: String,
    pub created_at: i64,
    pub is_active: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IssuedTokenResult {
    pub id: String,
    pub account_id: String,
    pub name: String,
    pub prefix: String,
    pub secret: String,
    pub is_root: bool,
    pub scopes: Vec<String>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenSummaryResult {
    pub id: String,
    pub account_id: String,
    pub name: String,
    pub prefix: String,
    pub is_root: bool,
    pub scopes: Vec<String>,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

/// Application-owned session metadata returned to transport adapters. Storage
/// rows are converted at the repository boundary and never escape the service.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SessionResult {
    pub id: String,
    pub account_id: String,
    pub created_at: i64,
    pub source: String,
    pub language: String,
    pub duration_sec: f64,
    pub status: String,
    pub model: String,
    pub input_device: Option<String>,
    pub file_name: Option<String>,
    pub audio_path: Option<String>,
    pub transcript_path: Option<String>,
    pub failure_reason: Option<String>,
    pub context_present: bool,
    pub cleanup_status: String,
    pub cleanup_path: Option<String>,
    pub cleanup_error_code: Option<String>,
}

impl From<seasnail_storage::SessionRow> for SessionResult {
    fn from(row: seasnail_storage::SessionRow) -> Self {
        Self {
            id: row.id,
            account_id: row.account_id,
            created_at: row.created_at,
            source: row.source,
            language: row.language,
            duration_sec: row.duration_sec,
            status: row.status,
            model: row.model,
            input_device: row.input_device,
            file_name: row.file_name,
            audio_path: row.audio_path,
            transcript_path: row.transcript_path,
            failure_reason: row.failure_reason,
            context_present: row.context_present,
            cleanup_status: row.cleanup_status,
            cleanup_path: row.cleanup_path,
            cleanup_error_code: row.cleanup_error_code,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TranscriptResult {
    pub full_text: String,
    pub speakers: Vec<TranscriptSpeakerResult>,
    pub units: Vec<TranscriptUnitResult>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TranscriptSpeakerResult {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TranscriptUnitResult {
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub text: String,
    pub speaker: String,
}

impl From<seasnail_proto::seasnail::v1::TranscriptFile> for TranscriptResult {
    fn from(transcript: seasnail_proto::seasnail::v1::TranscriptFile) -> Self {
        Self {
            full_text: transcript.full_text,
            speakers: transcript
                .speakers
                .into_iter()
                .map(|speaker| TranscriptSpeakerResult {
                    id: speaker.id,
                    label: speaker.label,
                })
                .collect(),
            units: transcript
                .units
                .into_iter()
                .map(|unit| TranscriptUnitResult {
                    start_ms: unit.start_ms,
                    end_ms: unit.end_ms,
                    text: unit.text,
                    speaker: unit.speaker,
                })
                .collect(),
        }
    }
}

/// 固定到账户的数据库和文件加密能力。
pub struct AccountDataLease {
    account_id: String,
    account_dir: PathBuf,
    k_sqlite: [u8; 32],
    k_files: [u8; 32],
    _guard: AccountLeaseGuard,
}

impl Clone for AccountDataLease {
    fn clone(&self) -> Self {
        Self {
            account_id: self.account_id.clone(),
            account_dir: self.account_dir.clone(),
            k_sqlite: self.k_sqlite,
            k_files: self.k_files,
            _guard: self._guard.clone(),
        }
    }
}

impl AccountDataLease {
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    pub fn account_dir(&self) -> &Path {
        &self.account_dir
    }
}

#[derive(Clone)]
pub struct AccountScopedRepository {
    lease: AccountDataLease,
    storage: Arc<crate::account::Storage>,
}

impl AccountScopedRepository {
    pub fn account_id(&self) -> &str {
        self.lease.account_id()
    }

    pub(crate) fn open_db(
        &self,
    ) -> Result<seasnail_storage::rusqlite::Connection, ApplicationError> {
        let path = self.lease.account_dir.join("meta.db");
        seasnail_storage::open_db(&path, &self.lease.k_sqlite).map_err(ApplicationError::from)
    }

    pub(crate) fn storage(&self) -> &crate::account::Storage {
        &self.storage
    }

    fn insert_token(&self, token: &crate::account::IssuedToken) -> Result<(), ApplicationError> {
        let conn = self.open_db()?;
        seasnail_storage::insert_token(
            &conn,
            &seasnail_storage::TokenRow {
                id: token.id.clone(),
                account_id: token.account_id.clone(),
                name: token.name.clone(),
                prefix: token.prefix.clone(),
                token_hash: token.token_hash.clone(),
                is_root: token.is_root,
                scopes: token.scopes.clone(),
                created_at: token.created_at,
                last_used_at: None,
            },
        )
        .map_err(ApplicationError::from)
    }

    fn list_tokens(&self) -> Result<Vec<seasnail_storage::TokenRow>, ApplicationError> {
        let conn = self.open_db()?;
        seasnail_storage::list_tokens(&conn, self.account_id()).map_err(ApplicationError::from)
    }

    fn delete_token(&self, id: &str) -> Result<bool, ApplicationError> {
        let conn = self.open_db()?;
        seasnail_storage::delete_token(&conn, id).map_err(ApplicationError::from)
    }

    #[allow(dead_code)]
    pub(crate) fn file_key(&self) -> &[u8; 32] {
        &self.lease.k_files
    }
}

#[derive(Clone)]
pub(crate) struct RepositoryFactory {
    crypto: Arc<Crypto>,
    coordination: Arc<IdentityCoordination>,
}

impl RepositoryFactory {
    fn new(crypto: Arc<Crypto>, coordination: Arc<IdentityCoordination>) -> Self {
        Self {
            crypto,
            coordination,
        }
    }

    fn bind_active(&self, account_id: &str) -> Result<AccountScopedRepository, ApplicationError> {
        let keys = self
            .crypto
            .snapshot_active_account(account_id)
            .map_err(ApplicationError::from)?;
        let guard = self
            .coordination
            .acquire_reader(account_id)
            .map_err(ApplicationError::from)?;
        let storage = Arc::new(crate::account::Storage::bind(
            Arc::clone(&self.crypto),
            keys.clone(),
        ));
        Ok(AccountScopedRepository {
            lease: AccountDataLease {
                account_id: account_id.to_owned(),
                account_dir: keys.account_dir,
                k_sqlite: keys.k_sqlite,
                k_files: keys.k_files,
                _guard: guard,
            },
            storage,
        })
    }
}

/// 身份用例共享的唯一协调状态。operation gate 使认证绑定与切换/删除线性化。
pub(crate) struct IdentityContext {
    pub(crate) auth: Arc<Auth>,
    repositories: RepositoryFactory,
    coordination: Arc<IdentityCoordination>,
}

impl IdentityContext {
    fn new(auth: Arc<Auth>) -> Arc<Self> {
        let coordination = auth.coordination();
        Arc::new(Self {
            repositories: RepositoryFactory::new(auth.crypto_arc(), Arc::clone(&coordination)),
            auth,
            coordination,
        })
    }

    fn bind_caller(
        &self,
        caller: crate::account::Caller,
    ) -> Result<CallerContext, ApplicationError> {
        let repository = self.repositories.bind_active(&caller.account_id)?;
        Ok(CallerContext {
            account_id: caller.account_id,
            token_id: caller.token_id,
            is_root: caller.is_root,
            scopes: caller.scopes.into_iter().collect(),
            repository,
        })
    }

    /// 账户 setup/unlock/switch 成功后，在向业务层开放该账户前恢复其文件/DB
    /// 一致性；cleanup 状态恢复也属于此生命周期边界。
    fn reconcile_active(&self) -> Result<(), ApplicationError> {
        crate::account::Storage::new(self.auth.crypto_arc())
            .reconcile()
            .map(|_| ())
            .map_err(ApplicationError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seasnail_crypto::{Argon2Params, KeychainStore, MemoryKeychain};
    use std::sync::Barrier;

    fn auth() -> Arc<Auth> {
        let dir = tempfile::tempdir().unwrap().keep();
        let keychain = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let crypto = Arc::new(
            Crypto::new(
                dir,
                keychain,
                Argon2Params {
                    m_kib: 8192,
                    t_cost: 1,
                    p_cost: 1,
                },
            )
            .unwrap(),
        );
        Arc::new(Auth::new(crypto))
    }

    fn services() -> ApplicationServices {
        ApplicationServices::new_for_test(auth())
    }

    #[test]
    fn desktop_login_relocks_after_storage_recovery_failure_and_can_retry() {
        let auth = auth();
        let crypto = auth.crypto_arc();
        let services = ApplicationServices::new_for_test(auth);
        let issued = services
            .auth
            .setup_first_account("alice", "password")
            .unwrap();
        let conn = crypto.open_active_db().unwrap();
        conn.execute_batch("ALTER TABLE sessions RENAME TO sessions_saved")
            .unwrap();
        services.accounts.desktop_logout().unwrap();
        assert!(services
            .accounts
            .desktop_login(&issued.account_id, "password")
            .is_err());
        assert!(crypto.active_account_id().is_none());
        assert_eq!(services.accounts.desktop_status()["authenticated"], false);
        conn.execute_batch("ALTER TABLE sessions_saved RENAME TO sessions")
            .unwrap();
        services
            .accounts
            .desktop_login(&issued.account_id, "password")
            .unwrap();
        assert_eq!(services.accounts.desktop_status()["authenticated"], true);
    }

    #[test]
    fn desktop_logout_rejects_active_account_leases() {
        let services = services();
        let issued = services
            .auth
            .setup_first_account("alice", "password")
            .unwrap();
        let caller = services.auth.authenticate_and_bind(&issued.secret).unwrap();
        assert!(matches!(
            services.accounts.desktop_logout(),
            Err(ApplicationError::Conflict(_))
        ));
        assert_eq!(services.accounts.desktop_status()["authenticated"], true);
        drop(caller);
        services.accounts.desktop_logout().unwrap();
        assert_eq!(services.accounts.desktop_status()["authenticated"], false);
    }

    #[test]
    fn bound_repository_survives_account_switch_and_blocks_delete() {
        let services = services();
        let alice = services.auth.setup_first_account("alice", "p1").unwrap();
        let caller = services.auth.authenticate_and_bind(&alice.secret).unwrap();
        assert!(caller.repository().open_db().is_ok());

        let bob = services.accounts.create(&caller, "bob", "p2").unwrap();
        assert_ne!(bob.account_id, caller.account_id());
        assert!(caller.repository().open_db().is_ok());
        assert!(matches!(
            services.accounts.delete(&caller, caller.account_id()),
            Err(ApplicationError::Conflict(_))
        ));

        drop(caller);
        let bob_caller = services.auth.authenticate_and_bind(&bob.secret).unwrap();
        services
            .accounts
            .delete(&bob_caller, &alice.account_id)
            .unwrap();
    }

    #[test]
    fn exclusive_delete_rejects_new_readers_until_guard_drops() {
        let tracker = Arc::new(IdentityCoordination::default());
        let deleting = tracker.begin_delete("acct").unwrap();
        assert!(matches!(
            tracker.acquire_reader("acct"),
            Err(crate::account::AccountError::ActiveLease)
        ));
        drop(deleting);
        let reader = tracker.acquire_reader("acct").unwrap();
        assert_eq!(tracker.active("acct"), 1);
        assert!(matches!(
            tracker.begin_delete("acct"),
            Err(crate::account::AccountError::ActiveLease)
        ));
        drop(reader);
        assert_eq!(tracker.active("acct"), 0);
    }

    #[test]
    fn background_job_lease_blocks_delete_across_switch_and_unlock_barriers() {
        let services = services();
        let alice = services.auth.setup_first_account("alice", "p1").unwrap();
        let alice_request = services.auth.authenticate_and_bind(&alice.secret).unwrap();
        let alice_job = alice_request.clone();
        let bob = services
            .accounts
            .create(&alice_request, "bob", "p2")
            .unwrap();
        drop(alice_request);
        let bob_request = services.auth.authenticate_and_bind(&bob.secret).unwrap();

        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let worker = {
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            std::thread::spawn(move || {
                assert!(alice_job.repository().open_db().is_ok());
                entered.wait();
                release.wait();
                drop(alice_job);
            })
        };
        entered.wait();
        assert!(matches!(
            services.accounts.delete(&bob_request, &alice.account_id),
            Err(ApplicationError::Conflict(_))
        ));

        services.accounts.unlock(&alice.account_id, "p1").unwrap();
        assert!(bob_request.repository().open_db().is_ok());
        let bob_tokens = services.tokens.list(&bob_request).unwrap();
        assert!(bob_tokens
            .iter()
            .any(|token| token.account_id == bob.account_id));
        assert!(bob_tokens
            .iter()
            .all(|token| token.account_id != alice.account_id));
        release.wait();
        worker.join().unwrap();

        let alice_request = services.auth.authenticate_and_bind(&alice.secret).unwrap();
        drop(bob_request);
        services
            .accounts
            .delete(&alice_request, &bob.account_id)
            .unwrap();
    }

    #[test]
    fn bound_storage_reads_and_writes_original_account_after_switch() {
        let services = services();
        let alice = services.auth.setup_first_account("alice", "p1").unwrap();
        let alice_caller = services.auth.authenticate_and_bind(&alice.secret).unwrap();
        let bob = services
            .accounts
            .create(&alice_caller, "bob", "p2")
            .unwrap();
        let bob_caller = services.auth.authenticate_and_bind(&bob.secret).unwrap();

        let session_id = "00000000-0000-0000-0000-000000000001";
        let created_at = 1_700_000_000;
        let alice_storage = alice_caller.repository().storage();
        let audio_path = alice_storage
            .write_audio(session_id, created_at, b"alice-audio")
            .unwrap();
        alice_storage
            .insert_session(&seasnail_storage::SessionRow {
                id: session_id.into(),
                account_id: alice.account_id.clone(),
                created_at,
                source: "imported".into(),
                language: "mixed".into(),
                duration_sec: 0.0,
                status: "transcribing".into(),
                model: "mock".into(),
                input_device: None,
                file_name: Some("audio.wav".into()),
                audio_path: Some(audio_path.clone()),
                transcript_path: None,
                failure_reason: None,
                context_present: false,
                cleanup_status: "not_requested".into(),
                cleanup_path: None,
                cleanup_error_code: None,
            })
            .unwrap();
        alice_storage
            .update_outcome(session_id, "failed", None, 0.0, Some("expected"), None)
            .unwrap();

        let alice_row = alice_storage.get(session_id).unwrap().unwrap();
        assert_eq!(alice_row.account_id, alice.account_id);
        assert_eq!(alice_row.status, "failed");
        assert_eq!(
            alice_storage.read_audio(&audio_path).unwrap(),
            b"alice-audio"
        );
        assert!(bob_caller
            .repository()
            .storage()
            .get(session_id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn independent_service_graphs_share_crypto_lease_coordination() {
        let auth = auth();
        let first = ApplicationServices::new_for_test(Arc::clone(&auth));
        let second = ApplicationServices::new_for_test(auth);
        let alice = first.auth.setup_first_account("alice", "p1").unwrap();
        let alice_reader = first.auth.authenticate_and_bind(&alice.secret).unwrap();
        let bob = first.accounts.create(&alice_reader, "bob", "p2").unwrap();
        let bob_caller = second.auth.authenticate_and_bind(&bob.secret).unwrap();

        assert!(matches!(
            second.accounts.delete(&bob_caller, &alice.account_id),
            Err(ApplicationError::Conflict(message))
                if message == "account has active background work"
        ));
    }
}
