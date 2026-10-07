//! daemon Application Service 容器与身份垂直切片。

use async_trait::async_trait;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use prost::Message;
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Write};
use std::sync::{Arc, Mutex, Weak};
use std::time::{SystemTime, UNIX_EPOCH};
use zip::write::FileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::account::token::generate_token;
use crate::account::{Auth, CleanupSettingsView, IssuedToken, ProviderConfigView};
use seasnail_storage::ProviderConfigInput;

use super::transcription::{PipelineTaskController, TranscriptionService};
use super::{
    resolve_final_text, AccountSummaryResult, ApplicationError, CallerContext, IdentityContext,
    IssuedTokenResult, ResolvedFinalText, SessionResult, TokenSummaryResult,
};

fn issued_result(token: IssuedToken) -> IssuedTokenResult {
    IssuedTokenResult {
        id: token.id,
        account_id: token.account_id,
        name: token.name,
        prefix: token.prefix,
        secret: token.secret_bearer,
        is_root: token.is_root,
        scopes: token.scopes,
        created_at: token.created_at,
    }
}

pub(super) fn require_scope(caller: &CallerContext, scope: &str) -> Result<(), ApplicationError> {
    if caller.has_scope(scope) {
        Ok(())
    } else {
        Err(ApplicationError::InsufficientScope(format!(
            "requires scope: {scope}"
        )))
    }
}

fn require_root(caller: &CallerContext) -> Result<(), ApplicationError> {
    if caller.is_root() {
        Ok(())
    } else {
        Err(ApplicationError::InsufficientScope(
            "requires root token".into(),
        ))
    }
}

#[derive(Clone)]
pub struct AuthService {
    identity: Arc<IdentityContext>,
}

impl AuthService {
    fn new(identity: Arc<IdentityContext>) -> Self {
        Self { identity }
    }

    pub fn is_initialized(&self) -> bool {
        self.identity.auth.is_initialized()
    }

    /// bearer 校验、账户密钥快照与 reader lease 获取在同一个 identity operation
    /// 临界区内完成，不能被 unlock/create/delete 插入。
    pub fn authenticate_and_bind(&self, bearer: &str) -> Result<CallerContext, ApplicationError> {
        let _operation = self.identity.coordination.operation();
        let caller = self
            .identity
            .auth
            .verify(bearer)
            .map_err(|error| match error {
                crate::account::AccountError::AccountNotFound(_)
                | crate::account::AccountError::TokenNotFound
                | crate::account::AccountError::InvalidToken(_) => ApplicationError::Unauthorized,
                other => ApplicationError::from(other),
            })?;
        self.identity.bind_caller(caller)
    }

    pub fn setup_first_account(
        &self,
        username: &str,
        password: &str,
    ) -> Result<IssuedTokenResult, ApplicationError> {
        let _operation = self.identity.coordination.operation();
        let token = self
            .identity
            .auth
            .setup_first_account(username, password)
            .map_err(ApplicationError::from)?;
        self.identity.reconcile_active()?;
        Ok(issued_result(token))
    }

    pub fn change_password(
        &self,
        caller: &CallerContext,
        current: &str,
        new_password: &str,
    ) -> Result<(), ApplicationError> {
        require_scope(caller, "tokens:manage")?;
        let _operation = self.identity.coordination.operation();
        self.identity
            .auth
            .change_password(caller.account_id(), current, new_password)
            .map_err(ApplicationError::from)
    }

    pub fn verify_password(
        &self,
        caller: &CallerContext,
        password: &str,
    ) -> Result<(), ApplicationError> {
        self.identity
            .auth
            .verify_password(caller.account_id(), password)
            .map(|_| ())
            .map_err(ApplicationError::from)
    }
}

#[derive(Clone)]
pub struct AccountService {
    identity: Arc<IdentityContext>,
    cleanup: Arc<crate::cleanup::CleanupService>,
}

impl AccountService {
    fn new(identity: Arc<IdentityContext>, cleanup: Arc<crate::cleanup::CleanupService>) -> Self {
        Self { identity, cleanup }
    }

    pub(crate) fn desktop_status(&self) -> serde_json::Value {
        let accounts = self
            .identity
            .auth
            .list_accounts()
            .into_iter()
            .map(|account| {
                serde_json::json!({
                    "id": account.id, "username": account.username, "is_active": account.is_active
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({ "initialized": self.identity.auth.is_initialized(),
            "authenticated": self.identity.auth.active_account_id().is_some(), "accounts": accounts })
    }

    pub(crate) fn desktop_create(
        &self,
        username: &str,
        password: &str,
    ) -> Result<(), ApplicationError> {
        let _operation = self.identity.coordination.operation();
        if self.identity.auth.active_account_id().is_some() {
            return Err(ApplicationError::Conflict(
                "sign out before creating an account".into(),
            ));
        }
        self.identity
            .auth
            .create_account(username, password)
            .map_err(ApplicationError::from)?;
        self.recover_desktop_account()?;
        Ok(())
    }

    pub(crate) fn desktop_login(&self, id: &str, password: &str) -> Result<(), ApplicationError> {
        let _operation = self.identity.coordination.operation();
        if self.identity.auth.active_account_id().is_some() {
            return Err(ApplicationError::Conflict(
                "sign out before logging in".into(),
            ));
        }
        self.identity
            .auth
            .unlock_account(id, password)
            .map_err(ApplicationError::from)?;
        self.recover_desktop_account()?;
        Ok(())
    }

    fn recover_desktop_account(&self) -> Result<(), ApplicationError> {
        if let Err(error) = self.identity.reconcile_active() {
            // Unlock/create already committed credentials. Re-lock before reporting
            // recovery failure; preserve account data so password login can retry.
            if self.identity.auth.logout().is_err() {
                tracing::warn!("failed to persist account relock after storage recovery error");
            }
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn desktop_logout(&self) -> Result<(), ApplicationError> {
        let _operation = self.identity.coordination.operation();
        let id = self.identity.auth.active_account_id();
        let _lease_guard = id
            .as_deref()
            .map(|id| self.identity.coordination.begin_delete(id))
            .transpose()
            .map_err(ApplicationError::from)?;
        self.identity.auth.logout().map_err(ApplicationError::from)
    }

    pub fn list(
        &self,
        caller: &CallerContext,
    ) -> Result<Vec<AccountSummaryResult>, ApplicationError> {
        require_scope(caller, "tokens:manage")?;
        Ok(self
            .identity
            .auth
            .list_accounts()
            .into_iter()
            .map(|account| AccountSummaryResult {
                id: account.id,
                username: account.username,
                created_at: account.created_at,
                is_active: account.is_active,
            })
            .collect())
    }

    pub fn create(
        &self,
        caller: &CallerContext,
        username: &str,
        password: &str,
    ) -> Result<IssuedTokenResult, ApplicationError> {
        require_scope(caller, "tokens:manage")?;
        let _operation = self.identity.coordination.operation();
        let token = self
            .identity
            .auth
            .create_account(username, password)
            .map_err(ApplicationError::from)?;
        self.identity.reconcile_active()?;
        Ok(issued_result(token))
    }

    pub fn unlock(&self, id: &str, password: &str) -> Result<IssuedTokenResult, ApplicationError> {
        let _operation = self.identity.coordination.operation();
        let token = self
            .identity
            .auth
            .unlock_account(id, password)
            .map_err(ApplicationError::from)?;
        self.identity.reconcile_active()?;
        Ok(issued_result(token))
    }

    pub fn delete(&self, caller: &CallerContext, id: &str) -> Result<(), ApplicationError> {
        require_scope(caller, "tokens:manage")?;
        let _operation = self.identity.coordination.operation();
        if self.identity.auth.active_account_id().as_deref() == Some(id) {
            return Err(ApplicationError::Conflict(
                "cannot delete active account; switch away first".into(),
            ));
        }
        let _deletion = self
            .identity
            .coordination
            .begin_delete(id)
            .map_err(ApplicationError::from)?;
        self.identity
            .auth
            .delete_account(id)
            .map_err(ApplicationError::from)?;
        self.cleanup.presentation_cache().remove_account(id);
        Ok(())
    }

    pub fn list_provider_configs(
        &self,
        caller: &CallerContext,
    ) -> Result<Vec<ProviderConfigView>, ApplicationError> {
        require_root(caller)?;
        caller
            .repository()
            .storage()
            .list_provider_configs()
            .map_err(ApplicationError::from)
    }

    pub async fn create_provider_config(
        &self,
        caller: &CallerContext,
        input: ProviderConfigInput<'_>,
    ) -> Result<ProviderConfigView, ApplicationError> {
        require_root(caller)?;
        seasnail_storage::normalize_provider_endpoint(input.provider_type, input.endpoint)
            .map_err(ApplicationError::from)?;
        self.cleanup
            .validate_provider_endpoint(input.provider_type, input.endpoint, false)
            .await
            .map_err(endpoint_validation_error)?;
        caller
            .repository()
            .storage()
            .create_provider_config(input, now_secs())
            .map_err(ApplicationError::from)
    }

    pub async fn replace_provider_config(
        &self,
        caller: &CallerContext,
        id: &str,
        input: ProviderConfigInput<'_>,
    ) -> Result<Option<ProviderConfigView>, ApplicationError> {
        require_root(caller)?;
        seasnail_storage::normalize_provider_endpoint(input.provider_type, input.endpoint)
            .map_err(ApplicationError::from)?;
        self.cleanup
            .validate_provider_endpoint(input.provider_type, input.endpoint, false)
            .await
            .map_err(endpoint_validation_error)?;
        caller
            .repository()
            .storage()
            .replace_provider_config(id, input, now_secs())
            .map_err(ApplicationError::from)
    }

    pub fn delete_provider_config(
        &self,
        caller: &CallerContext,
        id: &str,
    ) -> Result<bool, ApplicationError> {
        require_root(caller)?;
        caller
            .repository()
            .storage()
            .delete_provider_config(id, now_secs())
            .map_err(ApplicationError::from)
    }

    pub fn cleanup_settings(
        &self,
        caller: &CallerContext,
    ) -> Result<CleanupSettingsView, ApplicationError> {
        require_root(caller)?;
        caller
            .repository()
            .storage()
            .get_cleanup_settings()
            .map_err(ApplicationError::from)
    }

    pub fn save_cleanup_settings(
        &self,
        caller: &CallerContext,
        enabled: bool,
        selected_provider_config_id: Option<&str>,
        custom_prompt: Option<&str>,
    ) -> Result<CleanupSettingsView, ApplicationError> {
        require_root(caller)?;
        caller
            .repository()
            .storage()
            .save_cleanup_settings(
                enabled,
                selected_provider_config_id,
                custom_prompt,
                now_secs(),
            )
            .map_err(ApplicationError::from)
    }

    pub fn provider_snapshot(
        &self,
        caller: &CallerContext,
        id: &str,
    ) -> Result<crate::reasoning::ProviderSnapshot, ApplicationError> {
        require_root(caller)?;
        caller
            .repository()
            .storage()
            .provider_snapshot(id)
            .map_err(ApplicationError::from)
    }

    pub async fn probe_provider(
        &self,
        caller: &CallerContext,
        id: &str,
    ) -> Result<crate::reasoning::probe::ProbeOutcome, ApplicationError> {
        require_root(caller)?;
        let snapshot = self.provider_snapshot(caller, id)?;
        self.cleanup
            .probe(caller.account_id(), snapshot)
            .await
            .map_err(|error| match error {
                crate::reasoning::probe::ProbeError::Busy => ApplicationError::CleanupBusy,
                crate::reasoning::probe::ProbeError::Reasoning(error) => {
                    ApplicationError::CleanupFailure(error.kind().cleanup_error_code())
                }
            })
    }

    pub async fn cleanup_test(
        &self,
        caller: &CallerContext,
        provider_config_id: &str,
        text: String,
        prompt_draft: Option<&str>,
    ) -> Result<crate::cleanup::CleanupTestOutcome, ApplicationError> {
        require_root(caller)?;
        if text.trim().is_empty() {
            return Err(ApplicationError::InvalidInput(
                "cleanup test text must not be blank".into(),
            ));
        }
        let (account_id, provider, prompt) = caller
            .repository()
            .storage()
            .cleanup_test_snapshot(provider_config_id, prompt_draft)
            .map_err(ApplicationError::from)?;
        self.cleanup
            .test(&account_id, provider, prompt, text)
            .await
            .map_err(|error| match error {
                crate::cleanup::CleanupTestError::Busy => ApplicationError::CleanupBusy,
                crate::cleanup::CleanupTestError::Failed(code) => {
                    ApplicationError::CleanupFailure(code)
                }
            })
    }

    pub async fn set_provider_credential(
        &self,
        caller: &CallerContext,
        id: &str,
        secret: Vec<u8>,
    ) -> Result<crate::account::CredentialState, ApplicationError> {
        require_root(caller)?;
        let snapshot = caller
            .repository()
            .storage()
            .provider_snapshot(id)
            .map_err(ApplicationError::from)?;
        self.cleanup
            .validate_provider_endpoint(snapshot.provider_type, &snapshot.endpoint, true)
            .await
            .map_err(endpoint_validation_error)?;
        caller
            .repository()
            .storage()
            .set_provider_credential(id, secret)
            .map_err(ApplicationError::from)
    }

    pub async fn set_provider_no_auth(
        &self,
        caller: &CallerContext,
        id: &str,
    ) -> Result<crate::account::CredentialState, ApplicationError> {
        require_root(caller)?;
        let snapshot = caller
            .repository()
            .storage()
            .provider_snapshot(id)
            .map_err(ApplicationError::from)?;
        self.cleanup
            .validate_provider_endpoint(snapshot.provider_type, &snapshot.endpoint, false)
            .await
            .map_err(endpoint_validation_error)?;
        caller
            .repository()
            .storage()
            .set_provider_no_auth(id)
            .map_err(ApplicationError::from)
    }

    pub fn delete_provider_credential(
        &self,
        caller: &CallerContext,
        id: &str,
    ) -> Result<crate::account::CredentialState, ApplicationError> {
        require_root(caller)?;
        caller
            .repository()
            .storage()
            .delete_provider_credential(id, now_secs())
            .map_err(ApplicationError::from)
    }
}

fn endpoint_validation_error(error: crate::reasoning::ReasoningError) -> ApplicationError {
    ApplicationError::CleanupFailure(error.kind().cleanup_error_code())
}

#[derive(Clone)]
pub struct TokenService;

impl TokenService {
    fn new() -> Self {
        Self
    }

    pub fn issue(
        &self,
        caller: &CallerContext,
        name: &str,
        scopes: Vec<String>,
    ) -> Result<IssuedTokenResult, ApplicationError> {
        require_scope(caller, "tokens:manage")?;
        enforce_grantable(caller, &scopes)?;
        let token = generate_token(caller.account_id(), name, false, scopes, now_secs())
            .map_err(ApplicationError::from)?;
        caller.repository().insert_token(&token)?;
        Ok(issued_result(token))
    }

    pub fn list(
        &self,
        caller: &CallerContext,
    ) -> Result<Vec<TokenSummaryResult>, ApplicationError> {
        require_scope(caller, "tokens:manage")?;
        Ok(caller
            .repository()
            .list_tokens()?
            .into_iter()
            .map(|token| TokenSummaryResult {
                id: token.id,
                account_id: token.account_id,
                name: token.name,
                prefix: token.prefix,
                is_root: token.is_root,
                scopes: token.scopes,
                created_at: token.created_at,
                last_used_at: token.last_used_at,
            })
            .collect())
    }

    pub fn revoke(&self, caller: &CallerContext, id: &str) -> Result<(), ApplicationError> {
        require_scope(caller, "tokens:manage")?;
        if caller.repository().delete_token(id)? {
            Ok(())
        } else {
            Err(ApplicationError::NotFound("token not found".into()))
        }
    }
}

fn enforce_grantable(caller: &CallerContext, requested: &[String]) -> Result<(), ApplicationError> {
    for scope in requested {
        if scope == "is_root" || scope == "sessions:write" || scope == "tokens:manage" {
            let reason = if !caller.is_root() {
                format!("non-root cannot grant {scope}")
            } else {
                format!("{scope} not grantable to third-party tokens")
            };
            return Err(ApplicationError::ScopeNotGrantable(reason));
        }
    }
    Ok(())
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

pub struct SessionService {
    mutation_locks: Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
    injection_consumed: Arc<Mutex<HashSet<String>>>,
    task_controller: PipelineTaskController,
    cleanup: Arc<crate::cleanup::CleanupService>,
    learning_tickets: Arc<super::LearningTicketRegistry>,
}

pub(crate) struct WorkspaceDetailResult {
    pub full_text: String,
    pub final_text: String,
    pub text_source: &'static str,
    pub cleanup_status: String,
    pub cleanup_error_code: Option<String>,
    pub context_layout: &'static str,
    pub display_items: Vec<crate::composer::TypedTimelineItem>,
    pub separate_contexts: Vec<crate::composer::TypedTimelineItem>,
    pub context_degraded: bool,
}

pub(crate) struct CleanupDetailResult {
    pub original_text: String,
    pub cleaned_text: Option<String>,
    pub corrections: Vec<CleanupCorrectionResult>,
    pub cleanup_elapsed_ms: Option<u64>,
    pub diagnostics: Option<CleanupDiagnosticsResult>,
}

pub(crate) struct CleanupCorrectionResult {
    pub original_text: String,
    pub corrected_text: String,
    pub kind: &'static str,
}

pub(crate) struct CleanupDiagnosticsResult {
    pub local_transcription_elapsed_ms: Option<u64>,
    pub trace_id: String,
    pub request_started_at_ms: i64,
    pub response_started_at_ms: Option<i64>,
    pub response_completed_at_ms: Option<i64>,
    pub http_status: Option<u32>,
    pub response_content_type: Option<String>,
    pub provider_request_id: Option<String>,
    pub raw_response: Option<String>,
    pub raw_response_base64: String,
    pub response_sha256: String,
    pub response_body_bytes: u64,
    pub capture_status: &'static str,
}

pub(crate) struct InjectionPlanResult {
    pub plain: String,
    pub html: String,
    pub learning_ticket: String,
}

/// 自定义 Debug 省略粘贴文本与 learning_ticket，避免调试输出泄漏用户内容或票据。
impl std::fmt::Debug for InjectionPlanResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InjectionPlanResult")
            .field("plain", &format_args!("<{} bytes>", self.plain.len()))
            .field("html", &format_args!("<{} bytes>", self.html.len()))
            .field("learning_ticket", &"<redacted>")
            .finish()
    }
}

#[derive(Debug)]
pub(crate) struct ContextResourceResult {
    pub path: std::path::PathBuf,
    pub kind: &'static str,
    pub size: u64,
    pub modified_unix_ms: Option<u128>,
    pub device: Option<u64>,
    pub inode: Option<u64>,
}

impl std::fmt::Debug for SessionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionService").finish_non_exhaustive()
    }
}

impl SessionService {
    pub(crate) fn consume_learning_ticket(
        &self,
        caller: &CallerContext,
        ticket: &str,
    ) -> Result<super::LearningTicketData, super::LearningTicketError> {
        let data = self.learning_tickets.consume(ticket, caller.account_id())?;
        let current = self
            .get(caller, &data.claims.session_id)
            .map_err(|_| super::LearningTicketError::Mismatch)?;
        if current.status != "completed" {
            return Err(super::LearningTicketError::Mismatch);
        }
        Ok(data)
    }
    pub(crate) fn list(
        &self,
        caller: &CallerContext,
        limit: i64,
        before: Option<(i64, String)>,
        source: Option<&str>,
    ) -> Result<Vec<super::SessionResult>, ApplicationError> {
        require_scope(caller, "sessions:read")?;
        Ok(caller
            .repository()
            .storage()
            .list_page(
                limit,
                before.as_ref().map(|(t, id)| (*t, id.as_str())),
                source,
            )?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub(crate) fn search(
        &self,
        caller: &CallerContext,
        query: &str,
        limit: i64,
        before: Option<(i64, String)>,
        source: Option<&str>,
    ) -> Result<Vec<crate::account::SearchHit>, ApplicationError> {
        require_scope(caller, "sessions:read")?;
        if query.is_empty() {
            return Ok(Vec::new());
        }
        const CANDIDATE_BATCH: i64 = 500;
        let storage = caller.repository().storage();
        let mut cursor = before;
        let mut hits = Vec::new();
        loop {
            let rows = storage.list_page(
                CANDIDATE_BATCH,
                cursor.as_ref().map(|(time, id)| (*time, id.as_str())),
                source,
            )?;
            if rows.is_empty() {
                break;
            }
            for row in &rows {
                let session = SessionResult::from(row.clone());
                let Some(resolved) = resolve_final_text(storage, &session).ok().flatten() else {
                    continue;
                };
                if let Some(snippet) = super::final_text_snippet(&resolved.text, query) {
                    hits.push(crate::account::SearchHit {
                        session_id: row.id.clone(),
                        created_at: row.created_at,
                        snippet,
                    });
                    if hits.len() as i64 >= limit {
                        return Ok(hits);
                    }
                }
            }
            let last = rows.last().expect("nonempty candidate batch");
            cursor = Some((last.created_at, last.id.clone()));
            if rows.len() < CANDIDATE_BATCH as usize {
                break;
            }
        }
        Ok(hits)
    }

    pub(crate) fn get(
        &self,
        caller: &CallerContext,
        id: &str,
    ) -> Result<super::SessionResult, ApplicationError> {
        require_scope(caller, "sessions:read")?;
        caller
            .repository()
            .storage()
            .get(id)?
            .map(Into::into)
            .ok_or_else(|| ApplicationError::NotFound(format!("session not found: {id}")))
    }

    pub(crate) fn get_optional(
        &self,
        caller: &CallerContext,
        id: &str,
    ) -> Result<Option<super::SessionResult>, ApplicationError> {
        require_scope(caller, "sessions:read")?;
        caller
            .repository()
            .storage()
            .get(id)
            .map(|row| row.map(Into::into))
            .map_err(ApplicationError::from)
    }

    pub(crate) fn read_transcript(
        &self,
        caller: &CallerContext,
        path: &str,
    ) -> Result<super::TranscriptResult, ApplicationError> {
        self.read_transcript_file(caller, path).map(Into::into)
    }

    fn read_transcript_file(
        &self,
        caller: &CallerContext,
        path: &str,
    ) -> Result<seasnail_proto::seasnail::v1::TranscriptFile, ApplicationError> {
        caller
            .repository()
            .storage()
            .read_transcript(path)
            .map_err(ApplicationError::from)
    }

    fn read_context(
        &self,
        caller: &CallerContext,
        id: &str,
        created_at: i64,
    ) -> Result<seasnail_proto::seasnail::v1::ClipboardContextFile, ApplicationError> {
        caller
            .repository()
            .storage()
            .read_context(id, created_at)
            .map_err(ApplicationError::from)
    }

    pub(crate) fn read_audio(
        &self,
        caller: &CallerContext,
        path: &str,
    ) -> Result<Vec<u8>, ApplicationError> {
        caller
            .repository()
            .storage()
            .read_audio(path)
            .map_err(ApplicationError::from)
    }

    pub(crate) fn delete(
        &self,
        caller: &CallerContext,
        id: &str,
    ) -> Result<bool, ApplicationError> {
        require_scope(caller, "sessions:delete")?;
        let lock = {
            let mut locks = self
                .mutation_locks
                .lock()
                .map_err(|_| ApplicationError::Internal("session mutation lock poisoned".into()))?;
            if let Some(lock) = locks.get(id).and_then(Weak::upgrade) {
                lock
            } else {
                let lock = Arc::new(Mutex::new(()));
                locks.insert(id.to_owned(), Arc::downgrade(&lock));
                lock
            }
        };
        let _guard = lock
            .lock()
            .map_err(|_| ApplicationError::Internal("session mutation lock poisoned".into()))?;
        let deleted = caller
            .repository()
            .storage()
            .delete(id)
            .map_err(ApplicationError::from)?;
        if deleted {
            // 先以 mutation lock 完成 DB/目录删除，再取消 worker；删除失败时保留
            // 原 worker，避免请求错误导致 session 停在中间态。
            self.task_controller.cancel(id);
            self.cleanup
                .presentation_cache()
                .remove_session(caller.account_id(), id);
        }
        Ok(deleted)
    }

    pub(crate) fn mark_injection_consumed(&self, id: &str) -> bool {
        self.injection_consumed
            .lock()
            .map(|mut set| set.insert(id.to_owned()))
            .unwrap_or(false)
    }

    pub(crate) fn workspace_detail(
        &self,
        caller: &CallerContext,
        id: &str,
    ) -> Result<WorkspaceDetailResult, ApplicationError> {
        if !caller.is_root() {
            return Err(ApplicationError::InsufficientScope(
                "root token required".into(),
            ));
        }
        let row = self.get(caller, id)?;
        if row.status != "completed" {
            return Err(ApplicationError::Conflict("session not completed".into()));
        }
        let resolved = resolve_final_text(caller.repository().storage(), &row)?
            .ok_or_else(|| ApplicationError::Conflict("session not completed".into()))?;
        let (display_items, separate_contexts, context_layout, degraded) =
            self.compose_final_display(caller, &row, &resolved);
        Ok(WorkspaceDetailResult {
            full_text: resolved.text.clone(),
            final_text: resolved.text,
            text_source: resolved.source.as_str(),
            cleanup_status: resolved.cleanup_status,
            cleanup_error_code: resolved.cleanup_error_code,
            context_layout,
            display_items,
            separate_contexts,
            context_degraded: degraded,
        })
    }

    pub(crate) fn cleanup_detail(
        &self,
        caller: &CallerContext,
        id: &str,
    ) -> Result<CleanupDetailResult, ApplicationError> {
        if !caller.is_root() {
            return Err(ApplicationError::InsufficientScope(
                "root token required".into(),
            ));
        }
        let lock = super::transcription::mutation_lock(&self.mutation_locks, id)?;
        let _mutation = lock
            .lock()
            .map_err(|_| ApplicationError::Internal("session mutation lock poisoned".into()))?;
        let row = self.get(caller, id)?;
        if row.status != "completed" {
            return Err(ApplicationError::Conflict("session not completed".into()));
        }
        let resolved = resolve_final_text(caller.repository().storage(), &row)?
            .ok_or_else(|| ApplicationError::Conflict("session not completed".into()))?;
        let expected_cleanup_path =
            crate::account::Storage::canonical_cleanup_path(&row.id, row.created_at);
        let cleanup = resolved.cleanup_artifact.clone().or_else(|| {
            row.cleanup_path
                .as_deref()
                .filter(|path| *path == expected_cleanup_path)
                .and_then(|path| caller.repository().storage().read_cleanup(path).ok())
        });
        let cleaned_text = cleanup.as_ref().and_then(|artifact| {
            (!artifact.cleaned_text.is_empty()).then(|| artifact.cleaned_text.clone())
        });
        let corrections = cleanup
            .as_ref()
            .map(|artifact| {
                artifact
                    .corrections
                    .iter()
                    .map(|correction| CleanupCorrectionResult {
                        original_text: correction.original_text.clone(),
                        corrected_text: correction.corrected_text.clone(),
                        kind: match seasnail_proto::seasnail::v1::CorrectionKind::try_from(
                            correction.kind,
                        ) {
                            Ok(seasnail_proto::seasnail::v1::CorrectionKind::Phonetic) => {
                                "phonetic"
                            }
                            Ok(seasnail_proto::seasnail::v1::CorrectionKind::ProperNoun) => {
                                "proper_noun"
                            }
                            Ok(seasnail_proto::seasnail::v1::CorrectionKind::OtherAsr) => {
                                "other_asr"
                            }
                            _ => "unknown",
                        },
                    })
                    .collect()
            })
            .unwrap_or_default();
        let diagnostics = cleanup
            .as_ref()
            .and_then(|artifact| artifact.diagnostics.as_ref())
            .filter(|diagnostics| seasnail_proto::validate_cleanup_diagnostics(diagnostics).is_ok())
            .map(cleanup_diagnostics_result);
        let cleanup_elapsed_ms = cleanup.as_ref().map(|artifact| artifact.elapsed_ms);
        Ok(CleanupDetailResult {
            original_text: resolved.raw_transcript.full_text,
            cleaned_text,
            corrections,
            cleanup_elapsed_ms,
            diagnostics,
        })
    }

    fn compose_final_display(
        &self,
        caller: &CallerContext,
        row: &SessionResult,
        resolved: &ResolvedFinalText,
    ) -> (
        Vec<crate::composer::TypedTimelineItem>,
        Vec<crate::composer::TypedTimelineItem>,
        &'static str,
        bool,
    ) {
        let empty = seasnail_proto::seasnail::v1::ClipboardContextFile {
            schema_version: 1,
            session_id: row.id.clone(),
            capture_id: String::new(),
            events: vec![],
        };
        let (context, degraded) = if row.context_present {
            match self.read_context(caller, &row.id, row.created_at) {
                Ok(context) => (context, false),
                Err(_) => (empty, true),
            }
        } else {
            (empty, false)
        };
        if resolved.source.as_str() == "cleanup" {
            let allow_legacy_cache = resolved
                .cleanup_artifact
                .as_ref()
                .map_or(true, |artifact| artifact.context_placements.is_empty());
            if let Some(artifact) = resolved.cleanup_artifact.as_ref() {
                if let Some(items) = crate::composer::compose_persisted_cleanup_timeline(
                    &artifact.cleaned_text,
                    &artifact.context_placements,
                    &context,
                ) {
                    let context_layout = if context.events.is_empty() {
                        "none"
                    } else {
                        "inline"
                    };
                    return (items, Vec::new(), context_layout, degraded);
                }
            }
            // 兼容当前进程内生成但未携带持久化位置的旧结果。
            if let Some(entry) = allow_legacy_cache
                .then(|| {
                    self.cleanup
                        .presentation_cache()
                        .get(caller.account_id(), &row.id)
                })
                .flatten()
            {
                let context_layout = if context.events.is_empty() {
                    "none"
                } else {
                    "inline"
                };
                return (
                    crate::composer::compose_cleanup_timeline(
                        &entry.parts,
                        &entry.event_sequences,
                        &context,
                    ),
                    Vec::new(),
                    context_layout,
                    degraded,
                );
            }
            let separate = crate::composer::compose_separate_context(&context);
            return (
                vec![crate::composer::TypedTimelineItem::FinalText {
                    text: resolved.text.clone(),
                }],
                separate,
                if context.events.is_empty() {
                    "none"
                } else {
                    "separate"
                },
                degraded,
            );
        }
        (
            crate::composer::compose_typed_timeline(&resolved.raw_transcript, &context),
            Vec::new(),
            if context.events.is_empty() {
                "none"
            } else {
                "inline"
            },
            degraded,
        )
    }

    pub(crate) fn injection_plan(
        &self,
        caller: &CallerContext,
        id: &str,
    ) -> Result<InjectionPlanResult, ApplicationError> {
        if !caller.is_root() {
            return Err(ApplicationError::InsufficientScope(
                "root token required".into(),
            ));
        }
        let row = self.get(caller, id)?;
        if row.status != "completed" {
            return Err(ApplicationError::Conflict(format!(
                "session not completed: {id}"
            )));
        }
        let resolved = resolve_final_text(caller.repository().storage(), &row)?
            .ok_or_else(|| ApplicationError::Conflict(format!("session not completed: {id}")))?;
        let (inline, separate, _, _) = self.compose_final_display(caller, &row, &resolved);
        let (mut plain, mut html) = crate::composer::render_typed_timeline(&inline);
        if !separate.is_empty() {
            let (separate_plain, separate_html) =
                crate::composer::render_separate_context(&separate);
            plain.push_str(&separate_plain);
            html.push_str(&separate_html);
        }
        if !self.mark_injection_consumed(id) {
            return Err(ApplicationError::Conflict(format!(
                "injection plan already consumed: {id}"
            )));
        }
        let cleanup_candidates = resolved
            .cleanup_artifact
            .as_ref()
            .into_iter()
            .flat_map(|artifact| artifact.corrections.iter())
            .filter(|correction| {
                matches!(
                    seasnail_proto::seasnail::v1::CorrectionKind::try_from(correction.kind),
                    Ok(seasnail_proto::seasnail::v1::CorrectionKind::Phonetic
                        | seasnail_proto::seasnail::v1::CorrectionKind::ProperNoun)
                )
            })
            .filter(|correction| {
                super::dictionary::is_safe_cleanup_learning_candidate(
                    &correction.original_text,
                    &correction.corrected_text,
                )
            })
            .map(|correction| correction.corrected_text.clone())
            .take(32)
            .collect();
        let learning_ticket =
            self.learning_tickets
                .issue(caller.account_id(), id, cleanup_candidates);
        Ok(InjectionPlanResult {
            plain,
            html,
            learning_ticket,
        })
    }

    pub(crate) fn resolve_context_link(
        &self,
        caller: &CallerContext,
        id: &str,
        sequence: u32,
    ) -> Result<String, ApplicationError> {
        if !caller.is_root() {
            return Err(ApplicationError::InsufficientScope(
                "root token required".into(),
            ));
        }
        let row = self.get(caller, id)?;
        let context = self
            .read_context(caller, id, row.created_at)
            .map_err(|_| ApplicationError::Conflict("context unavailable".into()))?;
        let event = context
            .events
            .iter()
            .find(|e| e.sequence == sequence)
            .ok_or_else(|| ApplicationError::ContextNotFound("context event not found".into()))?;
        crate::composer::normalized_http_url(&event.plain_text)
            .ok_or_else(|| ApplicationError::UnsupportedScheme("unsupported scheme".into()))
    }

    pub(crate) fn resolve_context_resource(
        &self,
        caller: &CallerContext,
        id: &str,
        sequence: u32,
        index: usize,
        image: bool,
    ) -> Result<ContextResourceResult, ApplicationError> {
        if !caller.is_root() {
            return Err(ApplicationError::InsufficientScope(
                "root token required".into(),
            ));
        }
        let row = self.get(caller, id)?;
        let context = self
            .read_context(caller, id, row.created_at)
            .map_err(|_| ApplicationError::Conflict("context unavailable".into()))?;
        let event = context
            .events
            .iter()
            .find(|e| e.sequence == sequence)
            .ok_or_else(|| ApplicationError::ContextNotFound("context event not found".into()))?;
        let kind = seasnail_proto::seasnail::v1::ContextEventKind::try_from(event.kind).ok();
        if !matches!(
            kind,
            Some(
                seasnail_proto::seasnail::v1::ContextEventKind::ContextEventFiles
                    | seasnail_proto::seasnail::v1::ContextEventKind::ContextEventImage
            )
        ) {
            return Err(ApplicationError::ResourceUnavailable(
                "resource unavailable".into(),
            ));
        }
        if image
            && !matches!(
                kind,
                Some(seasnail_proto::seasnail::v1::ContextEventKind::ContextEventImage)
            )
        {
            return Err(ApplicationError::ResourceUnavailable(
                "resource unavailable".into(),
            ));
        }
        let raw = event
            .absolute_paths
            .get(index)
            .ok_or_else(|| ApplicationError::ResourceUnavailable("resource unavailable".into()))?;
        let account_dir = caller
            .repository()
            .storage()
            .bound_account_dir()
            .map_err(ApplicationError::from)?;
        let root = account_dir
            .parent()
            .and_then(|p| p.parent())
            .ok_or_else(|| ApplicationError::Internal("invalid account path".into()))?;
        let private_root = root
            .join("cache")
            .join("clipboard-context")
            .join(caller.account_id())
            .join(&context.capture_id);
        let is_image = image
            || event.kind
                == seasnail_proto::seasnail::v1::ContextEventKind::ContextEventImage as i32;
        let (path, metadata) = if is_image {
            crate::resource::validate_private_cache_file(raw, &private_root)
        } else {
            crate::resource::validate_openable_file(raw)
        }
        .map_err(|error| match error {
            crate::resource::ResourceError::Unavailable => {
                ApplicationError::ResourceUnavailable("resource unavailable".into())
            }
            crate::resource::ResourceError::Unsafe
            | crate::resource::ResourceError::BudgetExceeded => {
                ApplicationError::ResourceUnsafe("resource unsafe".into())
            }
        })?;
        #[cfg(unix)]
        let (device, inode) = {
            use std::os::unix::fs::MetadataExt;
            (Some(metadata.dev()), Some(metadata.ino()))
        };
        #[cfg(not(unix))]
        let (device, inode) = (None, None);
        Ok(ContextResourceResult {
            path,
            kind: if is_image { "image" } else { "file" },
            size: metadata.len(),
            modified_unix_ms: metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis()),
            device,
            inode,
        })
    }
}

pub struct ModelService {
    catalog: Arc<super::ModelCatalog>,
    registry: Arc<seasnail_runtime::SidecarRegistry>,
    settings: Arc<crate::model_settings::ModelSettings>,
    download_progress: Arc<Mutex<HashMap<String, f32>>>,
    installed: Arc<dyn Fn(&super::ManifestEntry) -> bool + Send + Sync>,
    gate: Arc<seasnail_runtime::RuntimeOperationGate>,
    builder: Arc<ModelRuntimeBuilder>,
    backend_switching: bool,
}

pub(crate) type ModelRuntimeBuilder = dyn Fn(
        &super::ManifestEntry,
        &std::path::Path,
    ) -> Result<Arc<dyn seasnail_runtime::ModelRuntime>, super::ModelRuntimeBuildError>
    + Send
    + Sync;

impl std::fmt::Debug for ModelService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelService").finish_non_exhaustive()
    }
}

impl ModelService {
    fn new(
        catalog: Arc<super::ModelCatalog>,
        registry: Arc<seasnail_runtime::SidecarRegistry>,
        settings: Arc<crate::model_settings::ModelSettings>,
        download_progress: Arc<Mutex<HashMap<String, f32>>>,
        installed: Arc<dyn Fn(&super::ManifestEntry) -> bool + Send + Sync>,
        gate: Arc<seasnail_runtime::RuntimeOperationGate>,
        builder: Arc<ModelRuntimeBuilder>,
        backend_switching: bool,
    ) -> Self {
        Self {
            catalog,
            registry,
            settings,
            download_progress,
            installed,
            gate,
            builder,
            backend_switching,
        }
    }

    pub(crate) async fn list(
        &self,
        caller: &CallerContext,
    ) -> Result<Vec<super::ModelSummaryResult>, ApplicationError> {
        require_scope(caller, "sessions:read")?;
        let active_id = self.registry.active_id().await;
        Ok(self
            .catalog
            .list()
            .iter()
            .map(|entry| {
                let status = if active_id.as_deref() == Some(entry.id.as_str()) {
                    super::ModelStatusResult::Active
                } else if (self.installed)(entry) {
                    super::ModelStatusResult::Installed
                } else {
                    super::ModelStatusResult::NotInstalled
                };
                let components = if entry.runtime == super::ModelRuntimeKind::FunAsr {
                    super::model_components::states(&self.settings, &self.download_progress)
                } else {
                    Vec::new()
                };
                super::ModelSummaryResult {
                    id: entry.id.clone(),
                    runtime: entry.runtime,
                    size_bytes: entry.size_bytes,
                    languages: entry.languages.clone(),
                    diarization: entry.diarization,
                    status,
                    bundled: entry.bundled,
                    default: entry.default,
                    components,
                }
            })
            .collect())
    }

    pub(crate) async fn set_component_enabled(
        &self,
        caller: &CallerContext,
        component: String,
        enabled: bool,
    ) -> Result<(), ApplicationError> {
        require_scope(caller, "sessions:write")?;
        let settings = Arc::clone(&self.settings);
        tokio::task::spawn_blocking(move || {
            super::model_components::set_enabled(&settings, &component, enabled)
        })
        .await
        .map_err(|error| ApplicationError::Internal(format!("set_enabled join: {error}")))?
    }

    pub(crate) fn download_component(
        &self,
        caller: &CallerContext,
        component: &str,
    ) -> Result<(), ApplicationError> {
        require_scope(caller, "sessions:write")?;
        super::model_components::start_download(&self.settings, &self.download_progress, component)
    }

    async fn describe(&self, entry: &super::ManifestEntry) -> super::ModelSummaryResult {
        let active_id = self.registry.active_id().await;
        super::ModelSummaryResult {
            id: entry.id.clone(),
            runtime: entry.runtime,
            size_bytes: entry.size_bytes,
            languages: entry.languages.clone(),
            diarization: entry.diarization,
            status: if active_id.as_deref() == Some(entry.id.as_str()) {
                super::ModelStatusResult::Active
            } else if (self.installed)(entry) {
                super::ModelStatusResult::Installed
            } else {
                super::ModelStatusResult::NotInstalled
            },
            bundled: entry.bundled,
            default: entry.default,
            components: if entry.runtime == super::ModelRuntimeKind::FunAsr {
                super::model_components::states(&self.settings, &self.download_progress)
            } else {
                Vec::new()
            },
        }
    }

    pub(crate) async fn activate(
        &self,
        caller: &CallerContext,
        model_id: &str,
    ) -> Result<super::ModelSummaryResult, ApplicationError> {
        require_scope(caller, "sessions:write")?;
        let entry = self
            .catalog
            .get(model_id)
            .ok_or_else(|| ApplicationError::NotFound(format!("model not found: {model_id}")))?;
        if !self.backend_switching && !entry.default {
            return Err(ApplicationError::Conflict(
                "runtime selection is only available in debug builds".into(),
            ));
        }
        seasnail_runtime::RuntimeAdmin::activate(
            self,
            seasnail_runtime::ActivateRuntime {
                model_id: model_id.into(),
            },
        )
        .await
        .map_err(|error| match error {
            seasnail_runtime::RuntimeFailure::Busy(_) => ApplicationError::Conflict(
                "model switch slot occupied; another switch or transcription in progress".into(),
            ),
            seasnail_runtime::RuntimeFailure::Preparation(message) => {
                ApplicationError::Conflict(message)
            }
            seasnail_runtime::RuntimeFailure::Administration(message) => {
                ApplicationError::Internal(message)
            }
            seasnail_runtime::RuntimeFailure::Unavailable => {
                ApplicationError::Conflict("no active runtime".into())
            }
            seasnail_runtime::RuntimeFailure::Runtime(error) => {
                ApplicationError::Internal(error.to_string())
            }
        })?;
        Ok(self.describe(entry).await)
    }

    /// 启动预热不经过 HTTP caller，但与显式激活共享完全相同的切换与补偿链。
    pub(crate) async fn activate_default(&self) -> Result<(), ApplicationError> {
        let saved = self.settings.backend();
        let selected = if self.backend_switching {
            saved.as_deref().and_then(|id| self.catalog.get(id))
        } else {
            None
        };
        if let Some(saved_id) = saved.as_deref() {
            if self.catalog.get(saved_id).is_none() {
                tracing::warn!(
                    saved_catalog_id = saved_id,
                    class = "saved_model_missing",
                    "saved model is absent from the current catalog; restoring bundled default"
                );
            } else if !self.backend_switching
                && self
                    .catalog
                    .get(saved_id)
                    .is_some_and(|entry| !entry.default)
            {
                tracing::warn!(
                    saved_catalog_id = saved_id,
                    class = "saved_model_not_allowed",
                    "saved non-default model is disabled in this build; restoring bundled default"
                );
            }
        }
        let model_id = selected
            .or_else(|| {
                self.catalog
                    .list()
                    .iter()
                    .find(|entry| entry.default && entry.bundled)
            })
            .ok_or_else(|| {
                ApplicationError::Internal("models manifest has no default model".into())
            })?
            .id
            .clone();
        seasnail_runtime::RuntimeAdmin::activate(
            self,
            seasnail_runtime::ActivateRuntime { model_id },
        )
        .await
        .map_err(|error| ApplicationError::Internal(error.to_string()))
    }

    async fn restore_old_runtime(
        &self,
        old: Option<Arc<dyn seasnail_runtime::ModelRuntime>>,
    ) -> Result<(), String> {
        let Some(old) = old else {
            return Ok(());
        };
        old.start(0)
            .await
            .map_err(|error| format!("rollback start failed: {error}"))?;
        self.registry.register(old).await;
        Ok(())
    }
}

#[async_trait]
impl seasnail_runtime::RuntimeAdmin for ModelService {
    async fn status(&self) -> seasnail_runtime::RuntimeStatus {
        let active = self.registry.active().await;
        seasnail_runtime::RuntimeStatus {
            active_model_id: active.as_ref().map(|runtime| runtime.id().to_owned()),
            active_kind: active.as_ref().map(|runtime| runtime.runtime_kind()),
        }
    }

    async fn activate(
        &self,
        request: seasnail_runtime::ActivateRuntime,
    ) -> Result<(), seasnail_runtime::RuntimeFailure> {
        if self.registry.active_id().await.as_deref() == Some(request.model_id.as_str()) {
            return Ok(());
        }
        let entry = self.catalog.get(&request.model_id).ok_or_else(|| {
            seasnail_runtime::RuntimeFailure::Preparation(format!(
                "model not found: {}",
                request.model_id
            ))
        })?;
        let new_runtime = (self.builder)(entry, &self.settings.dir().join("models")).map_err(
            |error| match error {
                super::ModelRuntimeBuildError::Unavailable(message)
                | super::ModelRuntimeBuildError::InvalidArtifact(message) => {
                    seasnail_runtime::RuntimeFailure::Preparation(message)
                }
                super::ModelRuntimeBuildError::Internal(message) => {
                    seasnail_runtime::RuntimeFailure::Administration(message)
                }
            },
        )?;
        let _lease = self
            .gate
            .acquire(seasnail_runtime::RuntimeOperation::ModelSwitch {
                model_id: request.model_id.clone(),
            })?;
        if self.registry.active_id().await.as_deref() == Some(request.model_id.as_str()) {
            return Ok(());
        }

        let old_runtime = self.registry.active().await;
        let old_selection = self.settings.backend();
        if let Some(old) = &old_runtime {
            if let Err(error) = old.stop().await {
                // Driver stop 可能已 take 进程句柄后才返回错误；此时 registry 不能继续
                // 宣称旧 runtime active，且绝不启动新 runtime。
                self.registry.clear().await;
                return Err(seasnail_runtime::RuntimeFailure::Administration(format!(
                    "failed to stop active model before switch: {error}; registry cleared"
                )));
            }
        }
        self.registry.clear().await;
        if let Err(error) = new_runtime.start(0).await {
            let rollback = self.restore_old_runtime(old_runtime).await;
            return Err(seasnail_runtime::RuntimeFailure::Administration(
                match rollback {
                    Ok(()) => format!("model start failed: {error}; old runtime restored"),
                    Err(rollback) => format!("model start failed: {error}; {rollback}"),
                },
            ));
        }
        self.registry.register(Arc::clone(&new_runtime)).await;

        if self.backend_switching {
            if let Err(error) = self.settings.set_backend(&request.model_id) {
                let stop_new = new_runtime.stop().await;
                self.registry.clear().await;
                // stop-new 失败时进程状态未知，绝不能再 start old 形成双 runtime。
                // registry 保持空，磁盘/内存设置仍由 set_backend 回滚为旧值。
                let rollback = match &stop_new {
                    Ok(()) => self.restore_old_runtime(old_runtime).await,
                    Err(_) => {
                        Err("old runtime not restored because new runtime stop failed".into())
                    }
                };
                debug_assert_eq!(self.settings.backend(), old_selection);
                let mut message = format!("persist selected backend failed: {error}");
                if let Err(error) = stop_new {
                    message.push_str(&format!("; failed to stop new runtime: {error}"));
                }
                if let Err(error) = rollback {
                    message.push_str(&format!("; {error}"));
                }
                return Err(seasnail_runtime::RuntimeFailure::Administration(message));
            }
        }
        Ok(())
    }
}

pub struct ExportService {
    auth: Arc<AuthService>,
}

impl std::fmt::Debug for ExportService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportService").finish_non_exhaustive()
    }
}

impl ExportService {
    pub(crate) fn new(auth: Arc<AuthService>) -> Self {
        Self { auth }
    }

    pub(crate) fn verify_password(
        &self,
        caller: &CallerContext,
        password: &str,
    ) -> Result<(), ApplicationError> {
        self.auth.verify_password(caller, password)
    }

    /// 生成内存 ZIP；service 不写明文临时文件，HTTP 层只负责 response 适配。
    pub(crate) fn build_zip(
        &self,
        caller: &CallerContext,
        ids: &[String],
    ) -> Result<Vec<u8>, ApplicationError> {
        require_scope(caller, "sessions:read")?;
        const MAX_SESSIONS: usize = 100;
        const MAX_BYTES: usize = 500 * 1024 * 1024;
        let storage = caller.repository().storage();
        let rows = if ids.is_empty() {
            storage.list(i64::MAX, None)?
        } else {
            ids.iter()
                .map(|id| {
                    storage.get(id)?.ok_or_else(|| {
                        ApplicationError::NotFound(format!("session not found: {id}"))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        if rows.len() > MAX_SESSIONS {
            return Err(ApplicationError::Conflict(
                "export exceeds session limit".into(),
            ));
        }
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let options = FileOptions::default().compression_method(CompressionMethod::Deflated);
        let mut total = 0usize;
        for row in rows {
            let day = DateTime::<Utc>::from_timestamp(row.created_at, 0)
                .unwrap_or_default()
                .format("%Y-%m-%d");
            let base = format!("{}/{}/{}/", caller.account_id(), day, row.id);
            if let Some(path) = row.audio_path.as_deref() {
                let name = row
                    .file_name
                    .as_deref()
                    .and_then(|n| std::path::Path::new(n).file_name())
                    .and_then(|n| n.to_str())
                    .filter(|n| !n.is_empty())
                    .unwrap_or("audio.bin");
                let bytes = storage.read_audio(path)?;
                total = total.saturating_add(bytes.len());
                if total > MAX_BYTES {
                    return Err(ApplicationError::Conflict(
                        "export exceeds size limit".into(),
                    ));
                }
                zip.start_file(format!("{base}{name}"), options)
                    .map_err(|e| ApplicationError::Internal(e.to_string()))?;
                zip.write_all(&bytes)
                    .map_err(|e| ApplicationError::Internal(e.to_string()))?;
            }
            if let Some(path) = row.transcript_path.as_deref() {
                let transcript = storage
                    .read_transcript(path)
                    .map_err(|_| ApplicationError::Conflict("transcript unavailable".into()))?;
                let speakers: Vec<_> = transcript
                    .speakers
                    .iter()
                    .map(|speaker| serde_json::json!({"id": speaker.id, "label": speaker.label}))
                    .collect();
                let units: Vec<_> = transcript.units.iter().map(|unit| serde_json::json!({"speaker": unit.speaker, "start": unit.start_ms.map(|v| v as f64 / 1000.0), "end": unit.end_ms.map(|v| v as f64 / 1000.0), "text": unit.text})).collect();
                let value = serde_json::json!({"session":{"id":row.id,"created_at":DateTime::<Utc>::from_timestamp(row.created_at,0).unwrap_or_default().to_rfc3339(),"source":row.source,"language":row.language,"duration_sec":row.duration_sec,"status":row.status,"model":row.model},"speakers":speakers,"transcript":{"full_text":transcript.full_text,"units":units}});
                let bytes = serde_json::to_vec_pretty(&value)
                    .map_err(|e| ApplicationError::Internal(e.to_string()))?;
                total = total.saturating_add(bytes.len());
                if total > MAX_BYTES {
                    return Err(ApplicationError::Conflict(
                        "export exceeds size limit".into(),
                    ));
                }
                zip.start_file(format!("{base}transcript.json"), options)
                    .map_err(|e| ApplicationError::Internal(e.to_string()))?;
                zip.write_all(&bytes)
                    .map_err(|e| ApplicationError::Internal(e.to_string()))?;
            }
            if let Some(resolved) = resolve_final_text(storage, &SessionResult::from(row.clone()))?
            {
                if let Some(cleanup) = resolved.cleanup_artifact.as_ref() {
                    let bytes = serde_json::to_vec_pretty(&cleanup_export_json(cleanup))
                        .map_err(|e| ApplicationError::Internal(e.to_string()))?;
                    total = total.saturating_add(bytes.len());
                    if total > MAX_BYTES {
                        return Err(ApplicationError::Conflict(
                            "export exceeds size limit".into(),
                        ));
                    }
                    zip.start_file(format!("{base}cleanup.json"), options)
                        .map_err(|e| ApplicationError::Internal(e.to_string()))?;
                    zip.write_all(&bytes)
                        .map_err(|e| ApplicationError::Internal(e.to_string()))?;
                }
            }
            if row.context_present {
                let context = storage
                    .read_context(&row.id, row.created_at)
                    .map_err(|_| ApplicationError::Conflict("context unavailable".into()))?;
                let bytes = context.encode_to_vec();
                total = total.saturating_add(bytes.len());
                if total > MAX_BYTES {
                    return Err(ApplicationError::Conflict(
                        "export exceeds size limit".into(),
                    ));
                }
                zip.start_file(format!("{base}context.pb"), options)
                    .map_err(|e| ApplicationError::Internal(e.to_string()))?;
                zip.write_all(&bytes)
                    .map_err(|e| ApplicationError::Internal(e.to_string()))?;
            }
        }
        Ok(zip
            .finish()
            .map_err(|e| ApplicationError::Internal(e.to_string()))?
            .into_inner())
    }
}

fn cleanup_export_json(cleanup: &seasnail_proto::seasnail::v1::CleanupFile) -> serde_json::Value {
    serde_json::json!({
        "schema_version": cleanup.schema_version,
        "session_id": cleanup.session_id,
        "account_id": cleanup.account_id,
        "outcome": "succeeded",
        "cleaned_text": cleanup.cleaned_text,
        "corrections": cleanup.corrections.iter().map(|correction| serde_json::json!({
            "original_text": correction.original_text,
            "corrected_text": correction.corrected_text,
            "kind": correction.kind,
        })).collect::<Vec<_>>(),
        "provider_config_id": cleanup.provider_config_id,
        "model": cleanup.model,
        "prompt_sha256": cleanup
            .prompt_sha256
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "elapsed_ms": cleanup.elapsed_ms,
        "placeholder_validation": cleanup.placeholder_validation,
    })
}

fn cleanup_diagnostics_result(
    diagnostics: &seasnail_proto::seasnail::v1::CleanupDiagnostics,
) -> CleanupDiagnosticsResult {
    use seasnail_proto::seasnail::v1::DiagnosticCaptureStatus;

    let raw_response = String::from_utf8(diagnostics.raw_response_body.clone()).ok();
    let raw_response_base64 = if raw_response.is_none() {
        base64::engine::general_purpose::STANDARD.encode(&diagnostics.raw_response_body)
    } else {
        String::new()
    };
    CleanupDiagnosticsResult {
        local_transcription_elapsed_ms: diagnostics.local_transcription_elapsed_ms,
        trace_id: diagnostics.trace_id.clone(),
        request_started_at_ms: diagnostics.request_started_at_ms,
        response_started_at_ms: (diagnostics.response_started_at_ms > 0)
            .then_some(diagnostics.response_started_at_ms),
        response_completed_at_ms: (diagnostics.response_completed_at_ms > 0)
            .then_some(diagnostics.response_completed_at_ms),
        http_status: diagnostics.http_status,
        response_content_type: (!diagnostics.response_content_type.is_empty())
            .then(|| diagnostics.response_content_type.clone()),
        provider_request_id: (!diagnostics.provider_request_id.is_empty())
            .then(|| diagnostics.provider_request_id.clone()),
        raw_response,
        raw_response_base64,
        response_sha256: diagnostics
            .response_sha256
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        response_body_bytes: diagnostics.response_body_bytes,
        capture_status: match DiagnosticCaptureStatus::try_from(diagnostics.capture_status) {
            Ok(DiagnosticCaptureStatus::NotReceived) => "not_received",
            Ok(DiagnosticCaptureStatus::Complete) => "complete",
            Ok(DiagnosticCaptureStatus::TooLarge) => "too_large",
            Ok(DiagnosticCaptureStatus::ReadFailed) => "read_failed",
            Ok(DiagnosticCaptureStatus::Redacted) => "redacted",
            _ => "unknown",
        },
    }
}

/// 由组合根构造一次、在 HTTP state 中只读共享的服务容器。
#[derive(Clone)]
pub struct ApplicationServices {
    pub auth: Arc<AuthService>,
    pub accounts: Arc<AccountService>,
    pub tokens: Arc<TokenService>,
    pub sessions: Arc<SessionService>,
    pub transcription: Arc<TranscriptionService>,
    pub models: Arc<ModelService>,
    pub export: Arc<ExportService>,
    pub dictionary: Arc<super::DictionaryService>,
}

impl ApplicationServices {
    #[cfg(test)]
    pub(crate) fn new_for_test(auth: Arc<Auth>) -> Self {
        let catalog = Arc::new(
            super::ModelCatalog::from_manifest_str(include_str!(concat!(
                env!("OUT_DIR"),
                "/models.json"
            )))
            .expect("bundled models.json must be valid"),
        );
        let registry = Arc::new(seasnail_runtime::SidecarRegistry::new());
        let settings = Arc::new(crate::model_settings::ModelSettings::new(
            std::env::temp_dir().join("seasnail-application-service-test"),
        ));
        Self::new(
            auth,
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashSet::new())),
            catalog,
            registry,
            settings,
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(|_| false),
            Arc::new(seasnail_runtime::RuntimeOperationGate::new_for_composition_root()),
            Arc::new(|entry, _| {
                Err(super::ModelRuntimeBuildError::Internal(format!(
                    "runtime builder unavailable in application service test: {}",
                    entry.id
                )))
            }),
            false,
            Arc::new(seasnail_runtime::AudioNormalizer::new("ffmpeg".into())),
        )
    }

    #[cfg(test)]
    pub(crate) fn new(
        auth: Arc<Auth>,
        mutation_locks: Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
        injection_consumed: Arc<Mutex<HashSet<String>>>,
        catalog: Arc<super::ModelCatalog>,
        registry: Arc<seasnail_runtime::SidecarRegistry>,
        settings: Arc<crate::model_settings::ModelSettings>,
        download_progress: Arc<Mutex<HashMap<String, f32>>>,
        installed: Arc<dyn Fn(&super::ManifestEntry) -> bool + Send + Sync>,
        gate: Arc<seasnail_runtime::RuntimeOperationGate>,
        builder: Arc<ModelRuntimeBuilder>,
        backend_switching: bool,
        normalizer: Arc<seasnail_runtime::AudioNormalizer>,
    ) -> Self {
        Self::new_with_cleanup(
            auth,
            mutation_locks,
            injection_consumed,
            catalog,
            registry,
            settings,
            download_progress,
            installed,
            gate,
            builder,
            backend_switching,
            normalizer,
            Arc::new(crate::cleanup::CleanupService::default()),
        )
    }

    pub(crate) fn new_with_cleanup(
        auth: Arc<Auth>,
        mutation_locks: Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
        injection_consumed: Arc<Mutex<HashSet<String>>>,
        catalog: Arc<super::ModelCatalog>,
        registry: Arc<seasnail_runtime::SidecarRegistry>,
        settings: Arc<crate::model_settings::ModelSettings>,
        download_progress: Arc<Mutex<HashMap<String, f32>>>,
        installed: Arc<dyn Fn(&super::ManifestEntry) -> bool + Send + Sync>,
        gate: Arc<seasnail_runtime::RuntimeOperationGate>,
        builder: Arc<ModelRuntimeBuilder>,
        backend_switching: bool,
        normalizer: Arc<seasnail_runtime::AudioNormalizer>,
        cleanup: Arc<crate::cleanup::CleanupService>,
    ) -> Self {
        let identity = IdentityContext::new(auth);
        let dictionary = Arc::new(super::DictionaryService);
        let learning_tickets = Arc::new(super::LearningTicketRegistry::default());
        let auth_service = Arc::new(AuthService::new(Arc::clone(&identity)));
        let transcription_engine = Arc::new(seasnail_runtime::RegistryTranscriptionEngine::new(
            Arc::clone(&gate),
            Arc::clone(&registry),
        ));
        let task_controller = PipelineTaskController::new();
        Self {
            auth: Arc::clone(&auth_service),
            accounts: Arc::new(AccountService::new(
                Arc::clone(&identity),
                Arc::clone(&cleanup),
            )),
            tokens: Arc::new(TokenService::new()),
            sessions: Arc::new(SessionService {
                mutation_locks: Arc::clone(&mutation_locks),
                injection_consumed,
                task_controller: task_controller.clone(),
                cleanup: Arc::clone(&cleanup),
                learning_tickets,
            }),
            transcription: Arc::new(TranscriptionService::new(
                transcription_engine,
                normalizer,
                Arc::clone(&settings),
                Arc::clone(&catalog),
                Arc::clone(&mutation_locks),
                task_controller,
                cleanup,
                Arc::clone(&dictionary),
            )),
            models: Arc::new(ModelService::new(
                catalog,
                registry,
                settings,
                download_progress,
                installed,
                gate,
                builder,
                backend_switching,
            )),
            export: Arc::new(ExportService::new(auth_service)),
            dictionary,
        }
    }
}

#[cfg(test)]
mod model_activation_tests {
    use super::*;
    use seasnail_runtime::contract::{OpenAiSegments, TranscribeReq};
    use seasnail_runtime::{
        Capabilities, Diarization, ModelRuntime, RuntimeAdmin, RuntimeError, RuntimeKind,
    };
    use std::io;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct FaultRuntime {
        id: String,
        starts: AtomicUsize,
        stops: AtomicUsize,
        fail_start_on: Option<usize>,
        fail_stop_on: Option<usize>,
        healthy: AtomicBool,
    }

    impl FaultRuntime {
        fn new(id: &str) -> Self {
            Self {
                id: id.into(),
                starts: AtomicUsize::new(0),
                stops: AtomicUsize::new(0),
                fail_start_on: None,
                fail_stop_on: None,
                healthy: AtomicBool::new(false),
            }
        }

        fn fail_start_on(mut self, call: usize) -> Self {
            self.fail_start_on = Some(call);
            self
        }

        fn fail_stop_on(mut self, call: usize) -> Self {
            self.fail_stop_on = Some(call);
            self
        }
    }

    #[async_trait]
    impl ModelRuntime for FaultRuntime {
        fn id(&self) -> &str {
            &self.id
        }

        fn runtime_kind(&self) -> RuntimeKind {
            RuntimeKind::Whisper
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities {
                diarization: Diarization::None,
                streaming: false,
                languages: vec!["en".into()],
                max_audio_seconds: 60,
                word_timestamps: false,
            }
        }

        async fn start(&self, _: u16) -> io::Result<()> {
            let call = self.starts.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_start_on == Some(call) {
                return Err(io::Error::other(format!("start failure {call}")));
            }
            self.healthy.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn stop(&self) -> io::Result<()> {
            let call = self.stops.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_stop_on == Some(call) {
                return Err(io::Error::other(format!("stop failure {call}")));
            }
            self.healthy.store(false, Ordering::SeqCst);
            Ok(())
        }

        async fn health(&self) -> bool {
            self.healthy.load(Ordering::SeqCst)
        }

        async fn transcribe(&self, _: TranscribeReq) -> Result<OpenAiSegments, RuntimeError> {
            Err(RuntimeError::NotStarted)
        }
    }

    fn catalog() -> Arc<super::super::ModelCatalog> {
        Arc::new(
            super::super::ModelCatalog::from_manifest_str(
                r#"{"version":1,"models":[
                    {"id":"old","runtime":"whisper","size_bytes":1,"languages":["en"],"diarization":"none","bundled":true,"default":true},
                    {"id":"new","runtime":"whisper","size_bytes":1,"languages":["en"],"diarization":"none","bundled":false}
                ]}"#,
            )
            .unwrap(),
        )
    }

    fn service(
        dir: PathBuf,
        registry: Arc<seasnail_runtime::SidecarRegistry>,
        gate: Arc<seasnail_runtime::RuntimeOperationGate>,
        runtimes: HashMap<String, Arc<FaultRuntime>>,
        backend_switching: bool,
    ) -> (ModelService, Arc<crate::model_settings::ModelSettings>) {
        let settings = Arc::new(crate::model_settings::ModelSettings::new(dir));
        let builder = Arc::new(
            move |entry: &super::super::ManifestEntry, _: &std::path::Path| {
                runtimes
                    .get(&entry.id)
                    .cloned()
                    .map(|runtime| runtime as Arc<dyn ModelRuntime>)
                    .ok_or_else(|| {
                        super::super::ModelRuntimeBuildError::Unavailable(format!(
                            "cannot build {}",
                            entry.id
                        ))
                    })
            },
        );
        (
            ModelService::new(
                catalog(),
                registry,
                Arc::clone(&settings),
                Arc::new(Mutex::new(HashMap::new())),
                Arc::new(|_| true),
                gate,
                builder,
                backend_switching,
            ),
            settings,
        )
    }

    async fn register_old(registry: &seasnail_runtime::SidecarRegistry, old: &Arc<FaultRuntime>) {
        old.start(0).await.unwrap();
        registry
            .register(Arc::clone(old) as Arc<dyn ModelRuntime>)
            .await;
    }

    #[tokio::test]
    async fn activate_stops_old_starts_and_registers_new() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(seasnail_runtime::SidecarRegistry::new());
        let gate = Arc::new(seasnail_runtime::RuntimeOperationGate::new_for_composition_root());
        let old = Arc::new(FaultRuntime::new("old"));
        let new = Arc::new(FaultRuntime::new("new"));
        register_old(&registry, &old).await;
        let (service, _) = service(
            dir.path().into(),
            Arc::clone(&registry),
            gate,
            HashMap::from([("new".into(), Arc::clone(&new))]),
            false,
        );

        RuntimeAdmin::activate(
            &service,
            seasnail_runtime::ActivateRuntime {
                model_id: "new".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(old.stops.load(Ordering::SeqCst), 1);
        assert_eq!(new.starts.load(Ordering::SeqCst), 1);
        assert_eq!(registry.active_id().await.as_deref(), Some("new"));
    }

    #[tokio::test]
    async fn construction_failure_does_not_stop_old_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(seasnail_runtime::SidecarRegistry::new());
        let old = Arc::new(FaultRuntime::new("old"));
        register_old(&registry, &old).await;
        let (service, _) = service(
            dir.path().into(),
            Arc::clone(&registry),
            Arc::new(seasnail_runtime::RuntimeOperationGate::new_for_composition_root()),
            HashMap::new(),
            false,
        );

        assert!(RuntimeAdmin::activate(
            &service,
            seasnail_runtime::ActivateRuntime {
                model_id: "new".into()
            }
        )
        .await
        .is_err());
        assert_eq!(old.stops.load(Ordering::SeqCst), 0);
        assert_eq!(registry.active_id().await.as_deref(), Some("old"));
    }

    #[tokio::test]
    async fn internal_builder_failure_preserves_internal_error_class() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(seasnail_runtime::SidecarRegistry::new());
        let old = Arc::new(FaultRuntime::new("old"));
        register_old(&registry, &old).await;
        let settings = Arc::new(crate::model_settings::ModelSettings::new(
            dir.path().to_path_buf(),
        ));
        let service = ModelService::new(
            catalog(),
            Arc::clone(&registry),
            settings,
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(|_| true),
            Arc::new(seasnail_runtime::RuntimeOperationGate::new_for_composition_root()),
            Arc::new(|_, _| {
                Err(super::super::ModelRuntimeBuildError::Internal(
                    "descriptor invariant failed".into(),
                ))
            }),
            false,
        );

        let error = RuntimeAdmin::activate(
            &service,
            seasnail_runtime::ActivateRuntime {
                model_id: "new".into(),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            seasnail_runtime::RuntimeFailure::Administration(message)
                if message == "descriptor invariant failed"
        ));
        assert_eq!(old.stops.load(Ordering::SeqCst), 0);
        assert_eq!(registry.active_id().await.as_deref(), Some("old"));
    }

    #[tokio::test]
    async fn transcription_lease_rejects_activation_before_process_changes() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(seasnail_runtime::SidecarRegistry::new());
        let gate = Arc::new(seasnail_runtime::RuntimeOperationGate::new_for_composition_root());
        let old = Arc::new(FaultRuntime::new("old"));
        let new = Arc::new(FaultRuntime::new("new"));
        register_old(&registry, &old).await;
        let (service, _) = service(
            dir.path().into(),
            Arc::clone(&registry),
            Arc::clone(&gate),
            HashMap::from([("new".into(), Arc::clone(&new))]),
            false,
        );
        let _lease = gate
            .acquire(seasnail_runtime::RuntimeOperation::Transcription {
                session_id: "busy".into(),
            })
            .unwrap();

        assert!(matches!(
            RuntimeAdmin::activate(
                &service,
                seasnail_runtime::ActivateRuntime {
                    model_id: "new".into()
                }
            )
            .await,
            Err(seasnail_runtime::RuntimeFailure::Busy(_))
        ));
        assert_eq!(old.stops.load(Ordering::SeqCst), 0);
        assert_eq!(new.starts.load(Ordering::SeqCst), 0);
        assert_eq!(registry.active_id().await.as_deref(), Some("old"));
    }

    #[tokio::test]
    async fn stop_old_failure_clears_registry_without_starting_new() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(seasnail_runtime::SidecarRegistry::new());
        let old = Arc::new(FaultRuntime::new("old").fail_stop_on(1));
        let new = Arc::new(FaultRuntime::new("new"));
        register_old(&registry, &old).await;
        let (service, _) = service(
            dir.path().into(),
            Arc::clone(&registry),
            Arc::new(seasnail_runtime::RuntimeOperationGate::new_for_composition_root()),
            HashMap::from([("new".into(), Arc::clone(&new))]),
            false,
        );

        assert!(RuntimeAdmin::activate(
            &service,
            seasnail_runtime::ActivateRuntime {
                model_id: "new".into()
            }
        )
        .await
        .is_err());
        assert_eq!(registry.active_id().await, None);
        assert_eq!(new.starts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn start_new_failure_restores_old_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(seasnail_runtime::SidecarRegistry::new());
        let old = Arc::new(FaultRuntime::new("old"));
        let new = Arc::new(FaultRuntime::new("new").fail_start_on(1));
        register_old(&registry, &old).await;
        let (service, _) = service(
            dir.path().into(),
            Arc::clone(&registry),
            Arc::new(seasnail_runtime::RuntimeOperationGate::new_for_composition_root()),
            HashMap::from([("new".into(), new)]),
            false,
        );

        let error = RuntimeAdmin::activate(
            &service,
            seasnail_runtime::ActivateRuntime {
                model_id: "new".into(),
            },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("old runtime restored"));
        assert_eq!(old.starts.load(Ordering::SeqCst), 2);
        assert_eq!(registry.active_id().await.as_deref(), Some("old"));
    }

    #[tokio::test]
    async fn rollback_start_failure_leaves_registry_empty() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(seasnail_runtime::SidecarRegistry::new());
        let old = Arc::new(FaultRuntime::new("old").fail_start_on(2));
        let new = Arc::new(FaultRuntime::new("new").fail_start_on(1));
        register_old(&registry, &old).await;
        let (service, _) = service(
            dir.path().into(),
            Arc::clone(&registry),
            Arc::new(seasnail_runtime::RuntimeOperationGate::new_for_composition_root()),
            HashMap::from([("new".into(), new)]),
            false,
        );

        let error = RuntimeAdmin::activate(
            &service,
            seasnail_runtime::ActivateRuntime {
                model_id: "new".into(),
            },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("rollback start failed"));
        assert_eq!(registry.active_id().await, None);
    }

    #[tokio::test]
    async fn persist_failure_restores_runtime_and_preserves_old_setting() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(seasnail_runtime::SidecarRegistry::new());
        let old = Arc::new(FaultRuntime::new("old"));
        let new = Arc::new(FaultRuntime::new("new"));
        register_old(&registry, &old).await;
        let (service, settings) = service(
            dir.path().into(),
            Arc::clone(&registry),
            Arc::new(seasnail_runtime::RuntimeOperationGate::new_for_composition_root()),
            HashMap::from([("new".into(), Arc::clone(&new))]),
            true,
        );
        settings.set_backend("old").unwrap();
        std::fs::create_dir(dir.path().join(".models-settings.json.tmp")).unwrap();

        let error = RuntimeAdmin::activate(
            &service,
            seasnail_runtime::ActivateRuntime {
                model_id: "new".into(),
            },
        )
        .await
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("persist selected backend failed"));
        assert_eq!(registry.active_id().await.as_deref(), Some("old"));
        assert_eq!(new.stops.load(Ordering::SeqCst), 1);
        assert_eq!(settings.backend().as_deref(), Some("old"));
        assert_eq!(
            crate::model_settings::ModelSettings::new(dir.path().into())
                .backend()
                .as_deref(),
            Some("old")
        );
    }

    #[tokio::test]
    async fn persist_compensation_stop_failure_keeps_registry_empty_and_does_not_restart_old() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(seasnail_runtime::SidecarRegistry::new());
        let old = Arc::new(FaultRuntime::new("old"));
        let new = Arc::new(FaultRuntime::new("new").fail_stop_on(1));
        register_old(&registry, &old).await;
        let (service, settings) = service(
            dir.path().into(),
            Arc::clone(&registry),
            Arc::new(seasnail_runtime::RuntimeOperationGate::new_for_composition_root()),
            HashMap::from([("new".into(), Arc::clone(&new))]),
            true,
        );
        settings.set_backend("old").unwrap();
        std::fs::create_dir(dir.path().join(".models-settings.json.tmp")).unwrap();

        let error = RuntimeAdmin::activate(
            &service,
            seasnail_runtime::ActivateRuntime {
                model_id: "new".into(),
            },
        )
        .await
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("old runtime not restored because new runtime stop failed"));
        assert_eq!(registry.active_id().await, None);
        assert_eq!(old.starts.load(Ordering::SeqCst), 1);
        assert_eq!(settings.backend().as_deref(), Some("old"));
        assert_eq!(
            crate::model_settings::ModelSettings::new(dir.path().into())
                .backend()
                .as_deref(),
            Some("old")
        );
    }
}
