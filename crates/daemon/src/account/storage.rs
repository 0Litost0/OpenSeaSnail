//! `Storage` 会话数据面编排（ST-M2.7）。
//!
//! 账户隔离的会话数据面（见 `doc/architecture.md#accounts-and-storage`）：
//! per-account 文件树读写（audio.enc / transcript.pb.enc，K_files AEAD 加解密 +
//! temp+rename 原子写）、写会话原子性（文件树先 → DB INSERT 为提交点）、启动期
//! reconcile 双向（孤儿 DB 行删、孤儿文件删）、search 内存解密子串匹配。
//!
//! 兼容视图委托 [`Crypto`] 对活跃账户操作；认证后的请求使用冻结账户目录与密钥的
//! bound 视图，后续 unlock/switch 不会改变其数据库或文件目标。
//! 文件树路径 `data/{account_id}/{YYYY-MM-DD}/{session_id}/{audio.enc, transcript.pb.enc}`，
//! 相对路径存 `sessions.audio_path`/`transcript_path`（相对 `data/{account_id}/`）。
//!
//! **不做 HTTP 端点**（M5）、**不做导出**（M5，预留 `read_audio`/`read_transcript` 复用）。

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Datelike, Utc};
use prost::Message;
use seasnail_crypto::{decrypt_file, decrypt_file_with_aad, encrypt_file, encrypt_file_with_aad};
use seasnail_proto::{
    seasnail::v1::{
        CleanupFile, CleanupOutcome, ClipboardContextFile, ContextEventKind, TranscriptFile,
    },
    validate_cleanup_context_placements, validate_cleanup_file, validate_transcript_file,
};
use seasnail_storage::{
    begin_retry as db_begin_retry, checkpoint_raw as db_checkpoint_raw, clear_context_present,
    complete_cleanup as db_complete_cleanup, delete_session, get_session, insert_session,
    list_sessions, list_sessions_after, recover_interrupted_cleanup as db_recover_cleanup,
    update_session_outcome, CleanupCompletion, CleanupFailureCode, RawCheckpointDecision,
    RecoveredCleanupOutcome, RecoveryExpectedState, SessionRow,
};
use walkdir::WalkDir;

use super::crypto::{AccountDataKeys, Crypto};
use super::error::AccountError;

/// 文件名常量。
const AUDIO_ENC: &str = "audio.enc";
const TRANSCRIPT_ENC: &str = "transcript.pb.enc";
const CONTEXT_ENC: &str = "context.pb.enc";
const CLEANUP_ENC: &str = "cleanup.pb.enc";
const MAX_CONTEXT_EVENTS: usize = 100;
const MAX_CONTEXT_TEXT_BYTES: usize = 512 * 1024;
const MAX_CONTEXT_HTML_BYTES: usize = 2 * 1024 * 1024;
const MAX_CONTEXT_PATHS: usize = 32;

/// search 候选上限（避免全扫；通配/正则后置）。
const SEARCH_CANDIDATE_LIMIT: i64 = 500;

/// 会话数据面编排器。未绑定实例用于启动期兼容任务，绑定实例用于认证请求/job。
pub struct Storage {
    pub(crate) crypto: Arc<Crypto>,
    bound: Option<AccountDataKeys>,
}

/// reconcile 结果报告。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// 孤儿 DB 行（audio/transcript 文件缺失）已删数。
    pub orphan_rows_removed: u64,
    /// 孤儿文件目录（文件无 DB 行）已删数。
    pub orphan_dirs_removed: u64,
    /// 未被任何仍存在 context 引用的明文 capture 目录已删数。
    pub orphan_capture_dirs_removed: u64,
    /// context.pb.enc 确认缺失但 audio+transcript 完好 → 降级（清 context_present）的行数。
    pub context_downgraded: u64,
    /// cleanup 中间态依据已有 artifact 恢复为 completed 的行数。
    pub cleanup_recovered: u64,
    /// cleanup 中间态无效/缺失 artifact，收口为 cleanup_interrupted 的行数。
    pub cleanup_interrupted: u64,
    /// 已完成但 succeeded artifact 损坏/缺失，降级为 cleanup_artifact_invalid 的行数。
    pub cleanup_invalidated: u64,
}

/// search 命中。
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub session_id: String,
    pub created_at: i64,
    pub snippet: String,
}

/// 会话目录内允许被枚举/导出的固定 artifact 类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionArtifactKind {
    Audio,
    Transcript,
    Context,
    Cleanup,
}

/// 已存在且为普通文件的会话 artifact；路径始终相对账户数据目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionArtifact {
    pub kind: SessionArtifactKind,
    pub relative_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilePresence {
    Present,
    Missing,
    TemporarilyUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupArtifactErrorKind {
    Missing,
    Invalid,
    Transient,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupArtifactState {
    Valid,
    Missing,
    Invalid,
}

fn file_presence(path: &std::path::Path) -> FilePresence {
    match std::fs::symlink_metadata(path) {
        Ok(_) => FilePresence::Present,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => FilePresence::Missing,
        Err(_) => FilePresence::TemporarilyUnavailable,
    }
}

fn context_is_confirmed_missing(error: &AccountError) -> bool {
    matches!(error, AccountError::Io(source) if source.kind() == std::io::ErrorKind::NotFound)
}

fn cleanup_artifact_error_kind(error: &AccountError) -> CleanupArtifactErrorKind {
    match error {
        AccountError::Io(source) if source.kind() == std::io::ErrorKind::NotFound => {
            CleanupArtifactErrorKind::Missing
        }
        AccountError::Io(_) => CleanupArtifactErrorKind::Transient,
        AccountError::Crypto(_) | AccountError::Proto(_) | AccountError::CleanupIntegrity(_) => {
            CleanupArtifactErrorKind::Invalid
        }
        _ => CleanupArtifactErrorKind::Transient,
    }
}

fn cleanup_failure_code_from_artifact(code: &str) -> CleanupFailureCode {
    match code {
        "cleanup_not_configured" => CleanupFailureCode::NotConfigured,
        "cleanup_endpoint_rejected" => CleanupFailureCode::EndpointRejected,
        "cleanup_credential_missing" => CleanupFailureCode::CredentialMissing,
        "cleanup_input_too_large" => CleanupFailureCode::InputTooLarge,
        "cleanup_timeout" => CleanupFailureCode::Timeout,
        "cleanup_http_auth" => CleanupFailureCode::HttpAuth,
        "cleanup_http_rate_limit" => CleanupFailureCode::HttpRateLimit,
        "cleanup_http_server" => CleanupFailureCode::HttpServer,
        "cleanup_response_too_large" => CleanupFailureCode::ResponseTooLarge,
        "cleanup_response_invalid_json" => CleanupFailureCode::ResponseInvalidJson,
        "cleanup_cleaned_text_invalid" => CleanupFailureCode::CleanedTextInvalid,
        "cleanup_placeholder_invalid" => CleanupFailureCode::PlaceholderInvalid,
        "cleanup_transport_error" => CleanupFailureCode::TransportError,
        "cleanup_artifact_write_failed" => CleanupFailureCode::ArtifactWriteFailed,
        "cleanup_artifact_invalid" => CleanupFailureCode::ArtifactInvalid,
        "cleanup_interrupted" => CleanupFailureCode::Interrupted,
        "cleanup_busy" => CleanupFailureCode::Busy,
        _ => CleanupFailureCode::Interrupted,
    }
}

impl Storage {
    /// 由 `Crypto` 构造数据面编排器。共享活跃账户运行态。
    pub fn new(crypto: Arc<Crypto>) -> Self {
        Self {
            crypto,
            bound: None,
        }
    }

    /// 返回由 session 身份派生的固定 transcript artifact 相对路径。
    pub fn canonical_transcript_path(session_id: &str, created_at: i64) -> String {
        Self::rel_path(created_at, session_id, TRANSCRIPT_ENC)
    }

    /// 返回由 session 身份派生的固定 cleanup artifact 相对路径。
    pub fn canonical_cleanup_path(session_id: &str, created_at: i64) -> String {
        Self::rel_path(created_at, session_id, CLEANUP_ENC)
    }

    /// 构造冻结到账户目录、数据库密钥和文件密钥的数据视图。
    pub(crate) fn bind(crypto: Arc<Crypto>, keys: AccountDataKeys) -> Self {
        Self {
            crypto,
            bound: Some(keys),
        }
    }

    /// 校验不可信 multipart context 的资源与时间轴边界。调用方在服务端补写 session
    /// id 后调用；Storage 写入与读取也会重复校验，避免绕过 HTTP 边界。
    pub fn validate_context_manifest(context: &ClipboardContextFile) -> Result<(), AccountError> {
        if context.schema_version != 1
            || uuid::Uuid::parse_str(&context.capture_id).is_err()
            || context.events.len() > MAX_CONTEXT_EVENTS
        {
            return Err(AccountError::ContextIntegrity(
                "invalid context metadata".into(),
            ));
        }
        for (index, event) in context.events.iter().enumerate() {
            let kind = ContextEventKind::try_from(event.kind).ok();
            if event.sequence != (index + 1) as u32
                || event.source_sample_rate < 8_000
                || event.source_sample_rate > 192_000
                || event.plain_text.len() > MAX_CONTEXT_TEXT_BYTES
                || event.html_fragment.len() > MAX_CONTEXT_HTML_BYTES
                || event.absolute_paths.len() > MAX_CONTEXT_PATHS
                || !event
                    .absolute_paths
                    .iter()
                    .all(|path| std::path::Path::new(path).is_absolute())
            {
                return Err(AccountError::ContextIntegrity(
                    "invalid context event bounds".into(),
                ));
            }
            match kind {
                Some(ContextEventKind::ContextEventPlainText) => {
                    if event.plain_text.is_empty()
                        || !event.html_fragment.is_empty()
                        || !event.absolute_paths.is_empty()
                    {
                        return Err(AccountError::ContextIntegrity(
                            "invalid plain text event".into(),
                        ));
                    }
                }
                Some(ContextEventKind::ContextEventRichText) => {
                    if event.plain_text.is_empty() || !event.absolute_paths.is_empty() {
                        return Err(AccountError::ContextIntegrity(
                            "invalid rich text event".into(),
                        ));
                    }
                }
                Some(ContextEventKind::ContextEventFiles)
                | Some(ContextEventKind::ContextEventImage) => {
                    if !event.plain_text.is_empty()
                        || !event.html_fragment.is_empty()
                        || event.absolute_paths.is_empty()
                    {
                        return Err(AccountError::ContextIntegrity("invalid path event".into()));
                    }
                }
                _ => {
                    return Err(AccountError::ContextIntegrity(
                        "unknown context event kind".into(),
                    ))
                }
            }
        }
        Ok(())
    }

    // ── 路径辅助 ──────────────────────────────────────────────────────────────

    /// 活跃账户 id（须已解锁）。
    pub(crate) fn active_account(&self) -> Result<String, AccountError> {
        if let Some(bound) = &self.bound {
            return Ok(bound.account_id.clone());
        }
        self.crypto
            .active_account_id()
            .ok_or(AccountError::NotUnlocked)
    }

    /// 文件树绝对路径：`{account_dir}/{rel}`。`rel` 为相对 `data/{account_id}/` 的路径。
    fn abs_path(&self, account_id: &str, rel: &str) -> PathBuf {
        self.account_dir(account_id).join(rel)
    }

    fn account_dir(&self, account_id: &str) -> PathBuf {
        match &self.bound {
            Some(bound) => {
                debug_assert_eq!(bound.account_id, account_id);
                bound.account_dir.clone()
            }
            None => self.crypto.account_dir(account_id),
        }
    }

    pub(crate) fn bound_account_dir(&self) -> Result<&std::path::Path, AccountError> {
        self.bound
            .as_ref()
            .map(|bound| bound.account_dir.as_path())
            .ok_or(AccountError::NotUnlocked)
    }

    fn file_key(&self) -> Result<[u8; 32], AccountError> {
        match &self.bound {
            Some(bound) => Ok(bound.k_files),
            None => self.crypto.active_k_files(),
        }
    }

    pub(crate) fn open_db(&self) -> Result<seasnail_storage::rusqlite::Connection, AccountError> {
        match &self.bound {
            Some(bound) => Ok(seasnail_storage::open_db(
                &bound.account_dir.join("meta.db"),
                &bound.k_sqlite,
            )?),
            None => self.crypto.open_active_db(),
        }
    }

    /// 由 created_at（epoch 秒）+ session_id 拼相对路径 `{YYYY-MM-DD}/{session_id}/{name}`。
    fn rel_path(created_at: i64, session_id: &str, name: &str) -> String {
        format!("{}/{}/{}", date_bucket(created_at), session_id, name)
    }

    // ── 音频 ──────────────────────────────────────────────────────────────────

    /// 加密写音频文件（temp+rename 原子）。返回相对路径 `YYYY-MM-DD/{sid}/audio.enc`。
    pub fn write_audio(
        &self,
        session_id: &str,
        created_at: i64,
        plaintext: &[u8],
    ) -> Result<String, AccountError> {
        let account_id = self.active_account()?;
        let k_files = self.file_key()?;
        let rel = Self::rel_path(created_at, session_id, AUDIO_ENC);
        let ct = encrypt_file(&k_files, plaintext);
        self.atomic_write(&account_id, &rel, &ct)?;
        Ok(rel)
    }

    /// 读音频（按相对路径）→ 解密 → 明文。
    pub fn read_audio(&self, audio_rel: &str) -> Result<Vec<u8>, AccountError> {
        let account_id = self.active_account()?;
        let k_files = self.file_key()?;
        let ct = std::fs::read(self.abs_path(&account_id, audio_rel))?;
        Ok(decrypt_file(&k_files, &ct)?)
    }

    // ── 转译 ──────────────────────────────────────────────────────────────────

    /// 加密写转译文件（proto encode → AEAD → 原子写）。返回相对路径。
    pub fn write_transcript(
        &self,
        session_id: &str,
        created_at: i64,
        t: &TranscriptFile,
    ) -> Result<String, AccountError> {
        let account_id = self.active_account()?;
        let k_files = self.file_key()?;
        if t.schema_version != 2 || t.session_id != session_id || t.account_id != account_id {
            return Err(AccountError::ContextIntegrity(
                "transcript identity mismatch".into(),
            ));
        }
        validate_transcript_file(t).map_err(AccountError::ContextIntegrity)?;
        let pt = t.encode_to_vec();
        let aad = transcript_aad(&account_id, session_id);
        let ct = encrypt_file_with_aad(&k_files, &aad, &pt);
        let rel = Self::rel_path(created_at, session_id, TRANSCRIPT_ENC);
        self.atomic_write(&account_id, &rel, &ct)?;
        Ok(rel)
    }

    /// 读转译（按相对路径）→ 解密 → proto decode。
    pub fn read_transcript(&self, transcript_rel: &str) -> Result<TranscriptFile, AccountError> {
        let account_id = self.active_account()?;
        let k_files = self.file_key()?;
        let path = std::path::Path::new(transcript_rel);
        let components: Vec<_> = path.components().collect();
        let session_id = match components.as_slice() {
            [std::path::Component::Normal(_date), std::path::Component::Normal(session), std::path::Component::Normal(file)]
                if file.to_str() == Some(TRANSCRIPT_ENC) =>
            {
                session.to_str().ok_or_else(|| {
                    AccountError::ContextIntegrity("invalid transcript path".into())
                })?
            }
            _ => {
                return Err(AccountError::ContextIntegrity(
                    "invalid transcript path".into(),
                ))
            }
        };
        let ct = std::fs::read(self.abs_path(&account_id, transcript_rel))?;
        let aad = transcript_aad(&account_id, session_id);
        let pt = decrypt_file_with_aad(&k_files, &aad, &ct)?;
        let t = TranscriptFile::decode(&*pt).map_err(|e| AccountError::Proto(e.to_string()))?;
        if t.session_id != session_id || t.account_id != account_id {
            return Err(AccountError::ContextIntegrity(
                "transcript identity mismatch".into(),
            ));
        }
        validate_transcript_file(&t).map_err(AccountError::ContextIntegrity)?;
        Ok(t)
    }

    // ── 剪贴板上下文 ────────────────────────────────────────────────────────────

    /// 加密写录音期上下文。路径固定派生自 session，不另增 DB 路径列，确保 retry 与
    /// 会话删除可按既有会话目录语义处理。
    pub fn write_context(
        &self,
        session_id: &str,
        created_at: i64,
        context: &ClipboardContextFile,
    ) -> Result<String, AccountError> {
        let account_id = self.active_account()?;
        let k_files = self.file_key()?;
        let rel = Self::rel_path(created_at, session_id, CONTEXT_ENC);
        if context.session_id != session_id {
            return Err(AccountError::ContextIntegrity(
                "invalid schema or session id".into(),
            ));
        }
        Self::validate_context_manifest(context)?;
        let aad = context_aad(&account_id, session_id);
        self.atomic_write(
            &account_id,
            &rel,
            &encrypt_file_with_aad(&k_files, &aad, &context.encode_to_vec()),
        )?;
        Ok(rel)
    }

    /// 读取固定会话目录内的上下文；缺失/篡改为错误，调用 retry 的上层必须拒绝静默
    /// 丢失上下文的重试。
    pub fn read_context(
        &self,
        session_id: &str,
        created_at: i64,
    ) -> Result<ClipboardContextFile, AccountError> {
        let account_id = self.active_account()?;
        let k_files = self.file_key()?;
        let rel = Self::rel_path(created_at, session_id, CONTEXT_ENC);
        let ciphertext = std::fs::read(self.abs_path(&account_id, &rel))?;
        let aad = context_aad(&account_id, session_id);
        let plaintext = decrypt_file_with_aad(&k_files, &aad, &ciphertext)?;
        let context = ClipboardContextFile::decode(&*plaintext)
            .map_err(|e| AccountError::Proto(e.to_string()))?;
        if context.session_id != session_id {
            return Err(AccountError::ContextIntegrity(
                "schema or session id mismatch".into(),
            ));
        }
        Self::validate_context_manifest(&context)?;
        Ok(context)
    }

    // ── Cleanup artifact ───────────────────────────────────────────────────────

    /// 用 cleanup 专用 AAD 加密并原子写入固定 `cleanup.pb.enc` 路径。
    pub fn write_cleanup(
        &self,
        session_id: &str,
        created_at: i64,
        cleanup: &CleanupFile,
    ) -> Result<String, AccountError> {
        let account_id = self.active_account()?;
        validate_cleanup_identity(cleanup, session_id, &account_id)?;
        validate_cleanup_file(cleanup).map_err(AccountError::CleanupIntegrity)?;
        validate_cleanup_context_placements(cleanup).map_err(AccountError::CleanupIntegrity)?;
        let rel = Self::rel_path(created_at, session_id, CLEANUP_ENC);
        let aad = cleanup_aad(&account_id, session_id);
        let ciphertext = encrypt_file_with_aad(&self.file_key()?, &aad, &cleanup.encode_to_vec());
        self.atomic_write(&account_id, &rel, &ciphertext)?;
        Ok(rel)
    }

    /// 删除本次 cleanup 尝试写出的固定 artifact。仅由持有 session mutation lock 的
    /// 上层在 DB CAS 未认领该 artifact 时调用，避免把旧 worker 的孤儿文件留在目录中。
    pub fn remove_cleanup(&self, session_id: &str, created_at: i64) -> Result<bool, AccountError> {
        let account_id = self.active_account()?;
        let rel = Self::rel_path(created_at, session_id, CLEANUP_ENC);
        let path = self.abs_path(&account_id, &rel);
        match std::fs::remove_file(&path) {
            Ok(()) => {
                #[cfg(unix)]
                if let Some(parent) = path.parent() {
                    std::fs::File::open(parent)?.sync_all()?;
                }
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// 删除 retry 将替换的旧 raw transcript。路径由 session 身份派生，避免消费 DB 中
    /// 可能被篡改的任意路径；调用方应在把行重新置为 transcribing 前执行。
    pub fn remove_transcript(
        &self,
        session_id: &str,
        created_at: i64,
    ) -> Result<bool, AccountError> {
        let account_id = self.active_account()?;
        let path = self.abs_path(
            &account_id,
            &Self::rel_path(created_at, session_id, TRANSCRIPT_ENC),
        );
        match std::fs::remove_file(&path) {
            Ok(()) => {
                #[cfg(unix)]
                if let Some(parent) = path.parent() {
                    std::fs::File::open(parent)?.sync_all()?;
                }
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// 严格读取 cleanup artifact：固定三段相对路径、AAD、proto、双重归属与字段不变量。
    pub fn read_cleanup(&self, cleanup_rel: &str) -> Result<CleanupFile, AccountError> {
        let account_id = self.active_account()?;
        let path = std::path::Path::new(cleanup_rel);
        let components: Vec<_> = path.components().collect();
        let session_id = match components.as_slice() {
            [std::path::Component::Normal(_date), std::path::Component::Normal(session), std::path::Component::Normal(file)]
                if file.to_str() == Some(CLEANUP_ENC) =>
            {
                session
                    .to_str()
                    .ok_or_else(|| AccountError::CleanupIntegrity("invalid cleanup path".into()))?
            }
            _ => {
                return Err(AccountError::CleanupIntegrity(
                    "invalid cleanup path".into(),
                ))
            }
        };
        let ciphertext = std::fs::read(self.abs_path(&account_id, cleanup_rel))?;
        let plaintext = decrypt_file_with_aad(
            &self.file_key()?,
            &cleanup_aad(&account_id, session_id),
            &ciphertext,
        )?;
        let cleanup = CleanupFile::decode(plaintext.as_slice())
            .map_err(|error| AccountError::Proto(error.to_string()))?;
        validate_cleanup_identity(&cleanup, session_id, &account_id)?;
        validate_cleanup_file(&cleanup).map_err(AccountError::CleanupIntegrity)?;
        Ok(cleanup)
    }

    // ── 会话元数据（DB） ───────────────────────────────────────────────────────

    /// 插入会话行（id/created_at/audio_path/transcript_path 由调用方填）。
    /// DB INSERT 为写会话的提交点（见设计 `:405`）。
    pub fn insert_session(&self, row: &SessionRow) -> Result<(), AccountError> {
        let account_id = self.active_account()?;
        if row.account_id != account_id {
            return Err(AccountError::CrossAccount(row.account_id.clone()));
        }
        let conn = self.open_db()?;
        Ok(insert_session(&conn, row)?)
    }

    /// 更新转译结果（ST-M3.5 / ST-M3.7）：transcribing → completed/failed，回填
    /// transcript_path（失败 None）与 duration_sec；失败时写 `failure_reason`，
    /// 成功/retry 重置时传 None 清空。转译任务结束时调。
    ///
    /// `model`：`Some` 时同步行 model（retry 重切 runtime 后调用方传 `rt.id()`）；
    /// `None` 不动（成功/失败路径）。单条 UPDATE 原子提交（见 model 层文档）。
    pub fn update_outcome(
        &self,
        session_id: &str,
        status: &str,
        transcript_path: Option<&str>,
        duration_sec: f64,
        failure_reason: Option<&str>,
        model: Option<&str>,
    ) -> Result<bool, AccountError> {
        let conn = self.open_db()?;
        Ok(update_session_outcome(
            &conn,
            session_id,
            status,
            transcript_path,
            duration_sec,
            failure_reason,
            model,
        )?)
    }

    pub fn begin_retry(
        &self,
        session_id: &str,
        expected_model: &str,
        new_model: &str,
    ) -> Result<bool, AccountError> {
        let conn = self.open_db()?;
        Ok(db_begin_retry(
            &conn,
            session_id,
            expected_model,
            new_model,
        )?)
    }

    pub fn fail_transcription(
        &self,
        session_id: &str,
        expected_model: &str,
        failure_reason: &str,
    ) -> Result<bool, AccountError> {
        let conn = self.open_db()?;
        Ok(seasnail_storage::fail_transcription(
            &conn,
            session_id,
            expected_model,
            failure_reason,
        )?)
    }

    /// raw transcript checkpoint：文件写入成功后原子决定 completed 或 cleaning_up。
    pub fn checkpoint_raw(
        &self,
        session_id: &str,
        expected_model: &str,
        transcript_path: &str,
        duration_sec: f64,
        decision: RawCheckpointDecision,
    ) -> Result<bool, AccountError> {
        let conn = self.open_db()?;
        Ok(db_checkpoint_raw(
            &conn,
            session_id,
            expected_model,
            transcript_path,
            duration_sec,
            decision,
        )?)
    }

    pub fn complete_cleanup(
        &self,
        session_id: &str,
        expected_model: &str,
        completion: CleanupCompletion<'_>,
    ) -> Result<bool, AccountError> {
        let conn = self.open_db()?;
        Ok(db_complete_cleanup(
            &conn,
            session_id,
            expected_model,
            completion,
        )?)
    }

    pub fn recover_interrupted_cleanup(
        &self,
        session_id: &str,
        expected_model: &str,
        expected: RecoveryExpectedState,
        recovered: RecoveredCleanupOutcome<'_>,
    ) -> Result<bool, AccountError> {
        let conn = self.open_db()?;
        Ok(db_recover_cleanup(
            &conn,
            session_id,
            expected_model,
            expected,
            recovered,
        )?)
    }

    /// 取单会话。
    pub fn get(&self, session_id: &str) -> Result<Option<SessionRow>, AccountError> {
        let conn = self.open_db()?;
        Ok(get_session(&conn, session_id)?)
    }

    /// 游标分页列会话（活跃账户，created_at DESC）。
    pub fn list(
        &self,
        limit: i64,
        before_created_at: Option<i64>,
    ) -> Result<Vec<SessionRow>, AccountError> {
        let account_id = self.active_account()?;
        let conn = self.open_db()?;
        Ok(list_sessions(&conn, &account_id, limit, before_created_at)?)
    }

    pub fn list_page(
        &self,
        limit: i64,
        before: Option<(i64, &str)>,
        source: Option<&str>,
    ) -> Result<Vec<SessionRow>, AccountError> {
        let account_id = self.active_account()?;
        let conn = self.open_db()?;
        Ok(list_sessions_after(
            &conn,
            &account_id,
            limit,
            before,
            source,
        )?)
    }

    /// 枚举固定会话目录中的普通 artifact，不接受符号链接或 DB 提供的任意路径。
    pub fn enumerate_session_artifacts(
        &self,
        session_id: &str,
        created_at: i64,
    ) -> Result<Vec<SessionArtifact>, AccountError> {
        let account_id = self.active_account()?;
        if !is_canonical_uuid(session_id) {
            return Err(AccountError::CleanupIntegrity(
                "invalid session artifact identity".into(),
            ));
        }
        let date = date_bucket(created_at);
        let candidates = [
            (SessionArtifactKind::Audio, AUDIO_ENC),
            (SessionArtifactKind::Transcript, TRANSCRIPT_ENC),
            (SessionArtifactKind::Context, CONTEXT_ENC),
            (SessionArtifactKind::Cleanup, CLEANUP_ENC),
        ];
        let account_dir = self.account_dir(&account_id);
        let root = account_dir
            .parent()
            .and_then(|data| data.parent())
            .ok_or_else(|| AccountError::CleanupIntegrity("invalid account data root".into()))?;
        let names: Vec<_> = candidates.iter().map(|(_, name)| *name).collect();
        let present = crate::media_cache::enumerate_session_files(
            root,
            &account_id,
            &date,
            session_id,
            &names,
        )
        .map_err(|error| {
            AccountError::CleanupIntegrity(format!("unsafe session artifact tree: {error}"))
        })?;
        Ok(candidates
            .into_iter()
            .filter(|(_, name)| present.iter().any(|present| present == name))
            .map(|(kind, name)| SessionArtifact {
                kind,
                relative_path: Self::rel_path(created_at, session_id, name),
            })
            .collect())
    }

    /// 删会话：删 DB 行（提交点）+ 删文件树目录（best-effort）。
    /// 返回是否删到行。文件树目录只由 canonical session id 与 created_at 派生。
    pub fn delete(&self, session_id: &str) -> Result<bool, AccountError> {
        let account_id = self.active_account()?;
        let conn = self.open_db()?;
        let row = get_session(&conn, session_id)?;
        let capture_id = row
            .as_ref()
            .filter(|row| row.context_present)
            .and_then(|row| {
                self.read_context(session_id, row.created_at)
                    .ok()
                    .map(|context| context.capture_id)
            });
        if let Some(capture_id) = capture_id.as_deref() {
            // capture id 由客户端生成，不能假设它在不同会话间天然唯一。仅在没有其他
            // 有效 context 引用时清理明文目录，避免删掉仍可被其他会话使用的截图。
            if !self.capture_is_referenced_elsewhere(session_id, capture_id)? {
                let account_dir = self.account_dir(&account_id);
                let root = account_dir.parent().and_then(|data| data.parent());
                if let Some(root) = root {
                    crate::media_cache::remove_capture(root, &account_id, capture_id)?;
                }
            }
        }
        let deleted = delete_session(&conn, session_id)?;
        if deleted {
            if let Some(r) = row {
                // 只按 DB 的 created_at + canonical session id 派生固定目录；不消费
                // audio/transcript/cleanup_path 中可能损坏的任意路径。
                if is_canonical_uuid(&r.id) {
                    if let Some(root) = self
                        .account_dir(&account_id)
                        .parent()
                        .and_then(|data| data.parent())
                    {
                        let _ = crate::media_cache::remove_session_data(
                            root,
                            &account_id,
                            &date_bucket(r.created_at),
                            &r.id,
                        );
                    }
                }
            }
        }
        Ok(deleted)
    }

    fn capture_is_referenced_elsewhere(
        &self,
        excluded_session_id: &str,
        capture_id: &str,
    ) -> Result<bool, AccountError> {
        for candidate in self.list(i64::MAX, None)? {
            if candidate.id == excluded_session_id || !candidate.context_present {
                continue;
            }
            if self
                .read_context(&candidate.id, candidate.created_at)
                .ok()
                .is_some_and(|context| context.capture_id == capture_id)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    // ── search ─────────────────────────────────────────────────────────────────

    /// 内存解密子串匹配（设计 `:403`）：SQL 取候选 → 各解密 transcript →
    /// `full_text` 子串匹配。大小写敏感；候选上限 500。
    pub fn search(&self, query: &str, limit: i64) -> Result<Vec<SearchHit>, AccountError> {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let candidates = self.list(SEARCH_CANDIDATE_LIMIT, None)?;
        let mut hits = Vec::new();
        for row in candidates {
            if hits.len() as i64 >= limit {
                break;
            }
            let Some(tp) = row.transcript_path.as_ref() else {
                continue;
            };
            // 解密失败（文件缺失/损坏）→ 跳过该候选（reconcile 兜底）。
            let transcript = match self.read_transcript(tp) {
                Ok(t) => t,
                Err(_) => continue,
            };
            if let Some(snippet) = match_transcript(&transcript, query) {
                hits.push(SearchHit {
                    session_id: row.id.clone(),
                    created_at: row.created_at,
                    snippet,
                });
            }
        }
        Ok(hits)
    }

    /// 可遍历搜索分页：从调用方游标之后分批扫描，直到取得 limit+1 命中或耗尽，
    /// 因而不会把“最新 500 条”误当成全部搜索空间。
    pub fn search_page(
        &self,
        query: &str,
        limit: i64,
        before: Option<(i64, &str)>,
        source: Option<&str>,
    ) -> Result<Vec<SearchHit>, AccountError> {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let mut cursor = before.map(|(time, id)| (time, id.to_string()));
        let mut hits = Vec::new();
        loop {
            let rows = self.list_page(
                SEARCH_CANDIDATE_LIMIT,
                cursor.as_ref().map(|(time, id)| (*time, id.as_str())),
                source,
            )?;
            if rows.is_empty() {
                break;
            }
            for row in &rows {
                if let Some(path) = &row.transcript_path {
                    if let Ok(transcript) = self.read_transcript(path) {
                        if let Some(snippet) = match_transcript(&transcript, query) {
                            hits.push(SearchHit {
                                session_id: row.id.clone(),
                                created_at: row.created_at,
                                snippet,
                            });
                            if hits.len() as i64 >= limit {
                                return Ok(hits);
                            }
                        }
                    }
                }
            }
            let last = rows.last().expect("nonempty");
            cursor = Some((last.created_at, last.id.clone()));
            if rows.len() < SEARCH_CANDIDATE_LIMIT as usize {
                break;
            }
        }
        Ok(hits)
    }

    // ── reconcile ──────────────────────────────────────────────────────────────

    /// 启动期双向 reconcile（设计 `:405`）：
    /// - 孤儿 DB 行（**audio/transcript** 文件缺失）→ 删行。
    /// - `context.pb.enc` 确认缺失但 audio+transcript 完好 → **降级**（清
    ///   `context_present`、保留行）；AEAD/schema/临时 I/O 错误保持引用和媒体缓存，
    ///   等待恢复。原实现把 context 缺失也当孤儿删整行，会静默摧毁完好的 transcript。
    /// - 孤儿文件目录（无 DB 行）→ 删目录。
    ///
    /// 返回清理计数。
    pub fn reconcile(&self) -> Result<ReconcileReport, AccountError> {
        let account_id = self.active_account()?;
        let conn = self.open_db()?;
        let mut report = ReconcileReport::default();

        // 1. 遍历全部会话：只有确认 NotFound 才执行不可逆清理。权限、临时 I/O、
        //    AEAD 和 schema 错误均保留原状态，等待下一次启动或用户修复后恢复。
        let rows = list_sessions(&conn, &account_id, i64::MAX, None)?;
        for row in rows {
            let audio_presence = row
                .audio_path
                .as_ref()
                .map(|p| file_presence(&self.abs_path(&account_id, p)))
                .unwrap_or(FilePresence::Present); // 无 audio_path 不判（可选字段）
            let transcript_presence = row
                .transcript_path
                .as_ref()
                .map(|p| file_presence(&self.abs_path(&account_id, p)))
                .unwrap_or(FilePresence::Present);
            if matches!(audio_presence, FilePresence::Missing)
                || matches!(transcript_presence, FilePresence::Missing)
            {
                // 音频或转写文件缺失 → 真孤儿，删行。
                let _ = delete_session(&conn, &row.id);
                report.orphan_rows_removed += 1;
                continue;
            }
            if matches!(audio_presence, FilePresence::TemporarilyUnavailable)
                || matches!(transcript_presence, FilePresence::TemporarilyUnavailable)
            {
                continue;
            }
            if row.context_present {
                if let Err(error) = self.read_context(&row.id, row.created_at) {
                    if context_is_confirmed_missing(&error)
                        && clear_context_present(&conn, &row.id).unwrap_or(false)
                    {
                        report.context_downgraded += 1;
                    }
                }
            }
            // Cleanup 的恢复独立于 audio/transcript 存在性检查：只消费已经落盘的
            // raw/cleanup artifact，不重新调用 LLM；暂时性 I/O 错误必须向上返回，
            // 避免把未完成的 processing 状态误报成已恢复。
            self.reconcile_cleanup_state(&row, &mut report)?;
        }

        // 2. 孤儿文件目录：walk account_dir 下所有 session 叶目录，查 DB 行。
        let acct_dir = self.account_dir(&account_id);
        for entry in WalkDir::new(&acct_dir)
            .min_depth(2) // 跳过 acct_dir 与 date 桶目录
            .max_depth(3) // date/{session_id} 叶目录
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_dir())
        {
            // 叶目录名 = session_id（路径 .../{date}/{session_id}）。
            let session_id = match entry.file_name().to_str() {
                Some(s) => s.to_string(),
                None => continue,
            };
            // 仅处理看起来是 UUID 的目录名（跳过非会话目录如 meta.db 父级已 min_depth 避开）。
            if uuid::Uuid::parse_str(&session_id).is_err() {
                continue;
            }
            if get_session(&conn, &session_id)?.is_none() {
                let _ = std::fs::remove_dir_all(entry.path());
                report.orphan_dirs_removed += 1;
            }
        }

        // 3. 明文图片缓存：只处理本账户 root 下 UUID capture 目录。引用集来自仍在 DB
        // 中且明确标记 context_present 的会话；无法解密的 context 不作引用，留给用户
        // 可见的完整性错误/下一次删除处理，不能凭路径字符串向外删除。
        let mut referenced_captures = std::collections::HashSet::new();
        let mut has_unresolved_context = false;
        for row in list_sessions(&conn, &account_id, i64::MAX, None)?
            .into_iter()
            .filter(|row| row.context_present)
        {
            match self.read_context(&row.id, row.created_at) {
                Ok(context) => {
                    referenced_captures.insert(context.capture_id);
                }
                Err(_) => has_unresolved_context = true,
            }
        }
        let cache_root = acct_dir
            .parent()
            .and_then(|data| data.parent())
            .map(|root| {
                root.join("cache")
                    .join("clipboard-context")
                    .join(&account_id)
            });
        if let Some(cache_root) = cache_root.filter(|_| !has_unresolved_context) {
            if let Ok(entries) = std::fs::read_dir(&cache_root) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let Some(name) = name.to_str() else { continue };
                    if uuid::Uuid::parse_str(name).is_err() || referenced_captures.contains(name) {
                        continue;
                    }
                    if let Some(root) = acct_dir.parent().and_then(|data| data.parent()) {
                        if crate::media_cache::remove_capture(root, &account_id, name)
                            .unwrap_or(false)
                        {
                            report.orphan_capture_dirs_removed += 1;
                        }
                    }
                }
            }
        }

        Ok(report)
    }

    fn reconcile_cleanup_state(
        &self,
        row: &SessionRow,
        report: &mut ReconcileReport,
    ) -> Result<(), AccountError> {
        match (row.status.as_str(), row.cleanup_status.as_str()) {
            ("transcribing", "not_requested") if row.transcript_path.is_none() => {
                // transcript rename 已完成但 DB checkpoint 尚未提交：固定路径上的有效
                // raw transcript 足以证明这次 cleanup decision 尚未发生，恢复为 raw-only。
                let transcript_path = Self::rel_path(row.created_at, &row.id, TRANSCRIPT_ENC);
                if !self.valid_raw_checkpoint(row, &transcript_path, false)? {
                    return Ok(());
                }
                let transcript = self.read_transcript(&transcript_path)?;
                let _ = self.recover_interrupted_cleanup(
                    &row.id,
                    &row.model,
                    RecoveryExpectedState::TranscribingNotRequested,
                    RecoveredCleanupOutcome::RawCheckpoint {
                        transcript_path: &transcript_path,
                        duration_sec: transcript.duration_ms as f64 / 1000.0,
                    },
                )?;
            }
            ("cleaning_up", "processing") => {
                let cleanup_path = Self::rel_path(row.created_at, &row.id, CLEANUP_ENC);
                let transcript_path = Self::rel_path(row.created_at, &row.id, TRANSCRIPT_ENC);
                if !self.valid_raw_checkpoint(row, &transcript_path, true)? {
                    return Ok(());
                }
                match self.read_cleanup(&cleanup_path) {
                    // cleanup.model 是 provider model，不是 ASR row.model；两者允许不同。
                    Ok(cleanup) => match CleanupOutcome::try_from(cleanup.outcome) {
                        Ok(CleanupOutcome::Succeeded) => {
                            if self.recover_interrupted_cleanup(
                                &row.id,
                                &row.model,
                                RecoveryExpectedState::CleaningUpProcessing,
                                RecoveredCleanupOutcome::Succeeded {
                                    cleanup_path: &cleanup_path,
                                },
                            )? {
                                report.cleanup_recovered += 1;
                            }
                        }
                        Ok(CleanupOutcome::Failed) => {
                            if self.recover_interrupted_cleanup(
                                &row.id,
                                &row.model,
                                RecoveryExpectedState::CleaningUpProcessing,
                                RecoveredCleanupOutcome::Failed {
                                    cleanup_path: Some(&cleanup_path),
                                    error_code: cleanup_failure_code_from_artifact(
                                        &cleanup.error_code,
                                    ),
                                },
                            )? {
                                report.cleanup_recovered += 1;
                            }
                        }
                        Ok(CleanupOutcome::Unspecified) => {
                            self.recover_interrupted_cleanup(
                                &row.id,
                                &row.model,
                                RecoveryExpectedState::CleaningUpProcessing,
                                RecoveredCleanupOutcome::Failed {
                                    cleanup_path: None,
                                    error_code: CleanupFailureCode::Interrupted,
                                },
                            )?;
                            self.remove_cleanup(&row.id, row.created_at)?;
                            report.cleanup_interrupted += 1;
                        }
                        Err(_) => {
                            self.recover_interrupted_cleanup(
                                &row.id,
                                &row.model,
                                RecoveryExpectedState::CleaningUpProcessing,
                                RecoveredCleanupOutcome::Failed {
                                    cleanup_path: None,
                                    error_code: CleanupFailureCode::Interrupted,
                                },
                            )?;
                            self.remove_cleanup(&row.id, row.created_at)?;
                            report.cleanup_interrupted += 1;
                        }
                    },
                    Err(error) => match cleanup_artifact_error_kind(&error) {
                        CleanupArtifactErrorKind::Transient => return Err(error),
                        CleanupArtifactErrorKind::Missing | CleanupArtifactErrorKind::Invalid => {
                            self.recover_interrupted_cleanup(
                                &row.id,
                                &row.model,
                                RecoveryExpectedState::CleaningUpProcessing,
                                RecoveredCleanupOutcome::Failed {
                                    cleanup_path: None,
                                    error_code: CleanupFailureCode::Interrupted,
                                },
                            )?;
                            self.remove_cleanup(&row.id, row.created_at)?;
                            report.cleanup_interrupted += 1;
                        }
                    },
                }
            }
            ("completed", "succeeded") => {
                let expected_path = Self::rel_path(row.created_at, &row.id, CLEANUP_ENC);
                let artifact_state = if row.cleanup_path.as_deref() != Some(expected_path.as_str())
                {
                    CleanupArtifactState::Invalid
                } else {
                    match self.read_cleanup(&expected_path) {
                        Ok(cleanup)
                            if CleanupOutcome::try_from(cleanup.outcome)
                                .is_ok_and(|outcome| outcome == CleanupOutcome::Succeeded) =>
                        {
                            CleanupArtifactState::Valid
                        }
                        Ok(_) => CleanupArtifactState::Invalid,
                        Err(error) => match cleanup_artifact_error_kind(&error) {
                            CleanupArtifactErrorKind::Transient => return Err(error),
                            CleanupArtifactErrorKind::Missing => CleanupArtifactState::Missing,
                            CleanupArtifactErrorKind::Invalid => CleanupArtifactState::Invalid,
                        },
                    }
                };
                if artifact_state != CleanupArtifactState::Valid
                    && self.recover_interrupted_cleanup(
                        &row.id,
                        &row.model,
                        RecoveryExpectedState::CompletedSucceeded,
                        RecoveredCleanupOutcome::Failed {
                            cleanup_path: None,
                            error_code: CleanupFailureCode::ArtifactInvalid,
                        },
                    )?
                {
                    // 仅按固定派生路径清理，绝不消费损坏 DB 行中的任意路径。
                    self.remove_cleanup(&row.id, row.created_at)?;
                    report.cleanup_invalidated += 1;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn valid_raw_checkpoint(
        &self,
        row: &SessionRow,
        expected_path: &str,
        require_db_path: bool,
    ) -> Result<bool, AccountError> {
        if require_db_path && row.transcript_path.as_deref() != Some(expected_path) {
            return Ok(false);
        }
        match self.read_transcript(expected_path) {
            Ok(transcript) => Ok(transcript.session_id == row.id && transcript.model == row.model),
            Err(error)
                if cleanup_artifact_error_kind(&error) == CleanupArtifactErrorKind::Transient =>
            {
                Err(error)
            }
            Err(_) => Ok(false),
        }
    }

    // ── 原子写（temp+rename，0o600）───────────────────────────────────────────

    /// 原子写文件：建叶目录（0o700）→ 写 temp → sync → 0o600 → rename。
    /// 与 `registry.rs` / `wrapped_dek.rs` 同模式。
    fn atomic_write(&self, account_id: &str, rel: &str, data: &[u8]) -> Result<(), AccountError> {
        let final_path = self.abs_path(account_id, rel);
        let dir = final_path.parent().expect("rel 必含父目录");
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, PermissionsExt::from_mode(0o700));
        }
        let tmp_path = final_path.with_file_name(format!(
            ".{}.tmp",
            final_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("file")
        ));
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp_path)?;
            f.write_all(data)?;
            f.sync_all()?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp_path, PermissionsExt::from_mode(0o600))?;
        }
        std::fs::rename(&tmp_path, &final_path)?;
        // rename 只保证目录项原子替换；同步父目录后，掉电恢复才不会丢失新文件名。
        #[cfg(unix)]
        std::fs::File::open(dir)?.sync_all()?;
        Ok(())
    }
}

fn context_aad(account_id: &str, session_id: &str) -> Vec<u8> {
    format!("seasnail/context/v1\0{account_id}\0{session_id}").into_bytes()
}

fn transcript_aad(account_id: &str, session_id: &str) -> Vec<u8> {
    format!("seasnail/transcript/v2\0{account_id}\0{session_id}").into_bytes()
}

fn cleanup_aad(account_id: &str, session_id: &str) -> Vec<u8> {
    format!("seasnail/cleanup/v1\0{account_id}\0{session_id}").into_bytes()
}

fn is_canonical_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|parsed| parsed.hyphenated().to_string() == value)
}

fn validate_cleanup_identity(
    cleanup: &CleanupFile,
    session_id: &str,
    account_id: &str,
) -> Result<(), AccountError> {
    if cleanup.session_id != session_id
        || cleanup.account_id != account_id
        || !is_canonical_uuid(session_id)
        || !is_canonical_uuid(account_id)
        || (!cleanup.provider_config_id.is_empty()
            && !is_canonical_uuid(&cleanup.provider_config_id))
    {
        return Err(AccountError::CleanupIntegrity(
            "cleanup artifact identity mismatch".into(),
        ));
    }
    Ok(())
}

/// 由 created_at（epoch 秒）算日期桶 `YYYY-MM-DD`（UTC）。
fn date_bucket(created_at: i64) -> String {
    let dt = DateTime::<Utc>::from_timestamp(created_at, 0).unwrap_or_default();
    format!("{:04}-{:02}-{:02}", dt.year(), dt.month(), dt.day())
}

/// 在 transcript 的 full_text 上做子串匹配；命中返回 snippet。
fn match_transcript(t: &TranscriptFile, query: &str) -> Option<String> {
    if t.full_text.contains(query) {
        return Some(snippet(&t.full_text, query));
    }
    None
}

/// 取命中处前后若干字符的片段。
fn snippet(text: &str, query: &str) -> String {
    let radius = 30;
    let start = text.find(query).unwrap_or(0);
    let lo = start.saturating_sub(radius);
    let hi = (start + query.len() + radius).min(text.len());
    // 按 char 边界对齐。
    let lo = text.floor_char_boundary(lo);
    let hi = text.ceil_char_boundary(hi);
    let mut s = String::new();
    if lo > 0 {
        s.push('…');
    }
    s.push_str(&text[lo..hi]);
    if hi < text.len() {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use seasnail_crypto::{Argon2Params, KeychainStore, MemoryKeychain};
    use seasnail_proto::seasnail::v1::{
        CleanupFile, CleanupOutcome, ClipboardContextFile, ContextEvent, ContextEventKind,
        Correction, CorrectionKind, PlaceholderValidationStatus, Source, Speaker, TranscriptFile,
        TranscriptUnit, UnitGranularity,
    };
    use uuid::Uuid;

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

    /// 构造 Storage：建首账户活跃。
    fn storage_with_account(dir: &tempfile::TempDir) -> (Storage, String) {
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let crypto = Arc::new(Crypto::new(dir.path().to_path_buf(), kc, fast_params()).unwrap());
        let t = crypto.setup_first_account("alice", "p").unwrap();
        let storage = Storage::new(crypto);
        (storage, t.account_id)
    }

    fn sample_transcript(session_id: &str, account_id: &str, full: &str) -> TranscriptFile {
        TranscriptFile {
            schema_version: 2,
            session_id: session_id.to_string(),
            account_id: account_id.to_string(),
            created_at_ms: 1_700_000_000_000,
            model: "whisper".into(),
            language: "zh".into(),
            source: Source::Realtime as i32,
            duration_ms: 5000,
            speakers: vec![Speaker {
                id: "A".into(),
                label: "说话人 A".into(),
            }],
            units: vec![TranscriptUnit {
                sequence: 0,
                speaker: "A".into(),
                start_ms: Some(0),
                end_ms: Some(1000),
                text: full.to_string(),
                confidence: Some(0.9),
                granularity: UnitGranularity::Segment as i32,
            }],
            full_text: full.to_string(),
        }
    }

    fn sample_cleanup(session_id: &str, account_id: &str) -> CleanupFile {
        CleanupFile {
            schema_version: 1,
            session_id: session_id.into(),
            account_id: account_id.into(),
            outcome: CleanupOutcome::Succeeded as i32,
            cleaned_text: "SeaSnail 已完成。".into(),
            corrections: vec![Correction {
                original_text: "Sea Snail".into(),
                corrected_text: "SeaSnail".into(),
                kind: CorrectionKind::ProperNoun as i32,
            }],
            provider_config_id: Uuid::new_v4().to_string(),
            model: "example-model".into(),
            prompt_sha256: vec![0xa5; 32],
            error_code: String::new(),
            elapsed_ms: 120,
            placeholder_validation: PlaceholderValidationStatus::PlaceholderValidationPassed as i32,
            context_placements: Vec::new(),
            diagnostics: None,
        }
    }

    fn new_session_row(
        id: &str,
        account_id: &str,
        created_at: i64,
        audio: &str,
        trans: &str,
    ) -> SessionRow {
        SessionRow {
            id: id.to_string(),
            account_id: account_id.to_string(),
            created_at,
            source: "realtime".into(),
            language: "zh".into(),
            duration_sec: 5.0,
            status: "completed".into(),
            model: "whisper".into(),
            input_device: Some("mic".into()),
            file_name: None,
            audio_path: Some(audio.to_string()),
            transcript_path: Some(trans.to_string()),
            failure_reason: None,
            context_present: false,
            cleanup_status: "not_requested".into(),
            cleanup_path: None,
            cleanup_error_code: None,
        }
    }

    // ── audio 往返 ──────────────────────────────────────────────────────────────

    #[test]
    fn write_read_audio_roundtrip() {
        let dir = tmpdir();
        let (storage, acct) = storage_with_account(&dir);
        let sid = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let rel = storage
            .write_audio(&sid, created_at, b"RIFF...pcm")
            .unwrap();
        assert!(rel.contains(&date_bucket(created_at)));
        assert!(rel.ends_with(AUDIO_ENC));
        let pt = storage.read_audio(&rel).unwrap();
        assert_eq!(pt, b"RIFF...pcm");
        let _ = acct;
    }

    // ── transcript 往返 ──────────────────────────────────────────────────────────

    #[test]
    fn write_read_transcript_roundtrip() {
        let dir = tmpdir();
        let (storage, acct) = storage_with_account(&dir);
        let sid = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let t = sample_transcript(&sid, &acct, "你好世界");
        let rel = storage.write_transcript(&sid, created_at, &t).unwrap();
        assert!(rel.ends_with(TRANSCRIPT_ENC));
        let dec = storage.read_transcript(&rel).unwrap();
        assert_eq!(dec.full_text, "你好世界");
        assert_eq!(dec.units.len(), 1);
    }

    #[test]
    fn transcript_ciphertext_cannot_be_replayed_to_another_session() {
        let dir = tmpdir();
        let (storage, acct) = storage_with_account(&dir);
        let created_at = 1_700_000_000;
        let first = Uuid::new_v4().to_string();
        let second = Uuid::new_v4().to_string();
        let first_rel = storage
            .write_transcript(
                &first,
                created_at,
                &sample_transcript(&first, &acct, "first"),
            )
            .unwrap();
        let second_rel = storage
            .write_transcript(
                &second,
                created_at,
                &sample_transcript(&second, &acct, "second"),
            )
            .unwrap();
        let root = storage.crypto.account_dir(&acct);
        let first_bytes = std::fs::read(root.join(&first_rel)).unwrap();
        std::fs::write(root.join(&second_rel), first_bytes).unwrap();
        assert!(storage.read_transcript(&second_rel).is_err());
    }

    // ── cleanup artifact 往返与归属 ────────────────────────────────────────────

    #[test]
    fn write_read_cleanup_roundtrip_and_rejects_tampering() {
        let dir = tmpdir();
        let (storage, account) = storage_with_account(&dir);
        let session_id = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let cleanup = sample_cleanup(&session_id, &account);
        let rel = storage
            .write_cleanup(&session_id, created_at, &cleanup)
            .unwrap();
        assert!(rel.ends_with(CLEANUP_ENC));
        assert_eq!(storage.read_cleanup(&rel).unwrap(), cleanup);

        let path = storage.crypto.account_dir(&account).join(&rel);
        let mut ciphertext = std::fs::read(&path).unwrap();
        *ciphertext.last_mut().unwrap() ^= 1;
        std::fs::write(path, ciphertext).unwrap();
        assert!(matches!(
            storage.read_cleanup(&rel),
            Err(AccountError::Crypto(_))
        ));
    }

    #[test]
    fn remove_cleanup_removes_only_the_cleanup_artifact() {
        let dir = tmpdir();
        let (storage, account) = storage_with_account(&dir);
        let session_id = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let audio_rel = storage
            .write_audio(&session_id, created_at, b"audio")
            .unwrap();
        let cleanup = sample_cleanup(&session_id, &account);
        let cleanup_rel = storage
            .write_cleanup(&session_id, created_at, &cleanup)
            .unwrap();

        assert!(storage.remove_cleanup(&session_id, created_at).unwrap());
        assert!(!storage
            .crypto
            .account_dir(&account)
            .join(&cleanup_rel)
            .exists());
        assert_eq!(storage.read_audio(&audio_rel).unwrap(), b"audio");
        assert!(!storage.remove_cleanup(&session_id, created_at).unwrap());
    }

    #[test]
    fn cleanup_ciphertext_cannot_be_replayed_to_another_session() {
        let dir = tmpdir();
        let (storage, account) = storage_with_account(&dir);
        let created_at = 1_700_000_000;
        let first = Uuid::new_v4().to_string();
        let second = Uuid::new_v4().to_string();
        let first_rel = storage
            .write_cleanup(&first, created_at, &sample_cleanup(&first, &account))
            .unwrap();
        let second_rel = Storage::rel_path(created_at, &second, CLEANUP_ENC);
        let second_path = storage.crypto.account_dir(&account).join(&second_rel);
        std::fs::create_dir_all(second_path.parent().unwrap()).unwrap();
        std::fs::copy(
            storage.crypto.account_dir(&account).join(first_rel),
            second_path,
        )
        .unwrap();
        assert!(matches!(
            storage.read_cleanup(&second_rel),
            Err(AccountError::Crypto(_))
        ));
    }

    #[test]
    fn cleanup_ciphertext_cannot_be_replayed_to_another_account() {
        let dir = tmpdir();
        let keychain = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let crypto =
            Arc::new(Crypto::new(dir.path().to_path_buf(), keychain, fast_params()).unwrap());
        let alice = crypto.setup_first_account("alice", "p1").unwrap();
        let alice_keys = crypto.snapshot_active_account(&alice.account_id).unwrap();
        let alice_storage = Storage::bind(Arc::clone(&crypto), alice_keys);
        let bob = crypto.create_account("bob", "p2").unwrap();
        let bob_keys = crypto.snapshot_active_account(&bob.account_id).unwrap();
        let bob_storage = Storage::bind(Arc::clone(&crypto), bob_keys);

        let session_id = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let rel = alice_storage
            .write_cleanup(
                &session_id,
                created_at,
                &sample_cleanup(&session_id, &alice.account_id),
            )
            .unwrap();
        let bob_path = crypto.account_dir(&bob.account_id).join(&rel);
        std::fs::create_dir_all(bob_path.parent().unwrap()).unwrap();
        std::fs::copy(crypto.account_dir(&alice.account_id).join(&rel), bob_path).unwrap();

        assert!(matches!(
            bob_storage.read_cleanup(&rel),
            Err(AccountError::Crypto(_))
        ));
    }

    #[test]
    fn cleanup_read_rejects_invalid_outcome_and_field_bounds() {
        let dir = tmpdir();
        let (storage, account) = storage_with_account(&dir);
        let session_id = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let rel = Storage::rel_path(created_at, &session_id, CLEANUP_ENC);
        let mut cleanup = sample_cleanup(&session_id, &account);
        cleanup.outcome = CleanupOutcome::Unspecified as i32;
        let ciphertext = encrypt_file_with_aad(
            &storage.file_key().unwrap(),
            &cleanup_aad(&account, &session_id),
            &cleanup.encode_to_vec(),
        );
        storage.atomic_write(&account, &rel, &ciphertext).unwrap();
        assert!(matches!(
            storage.read_cleanup(&rel),
            Err(AccountError::CleanupIntegrity(_))
        ));

        cleanup = sample_cleanup(&session_id, &account);
        cleanup.model = "m".repeat(257);
        assert!(matches!(
            storage.write_cleanup(&session_id, created_at, &cleanup),
            Err(AccountError::CleanupIntegrity(_))
        ));
        cleanup = sample_cleanup(&session_id, &account);
        cleanup.cleaned_text = "x".repeat(128 * 1024 + 1);
        assert!(matches!(
            storage.write_cleanup(&session_id, created_at, &cleanup),
            Err(AccountError::CleanupIntegrity(_))
        ));
    }

    #[test]
    fn artifact_enumeration_and_session_delete_cover_all_fixed_files() {
        let dir = tmpdir();
        let (storage, account) = storage_with_account(&dir);
        let session_id = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let audio = storage
            .write_audio(&session_id, created_at, b"audio")
            .unwrap();
        let transcript = storage
            .write_transcript(
                &session_id,
                created_at,
                &sample_transcript(&session_id, &account, "raw"),
            )
            .unwrap();
        storage
            .write_context(
                &session_id,
                created_at,
                &ClipboardContextFile {
                    schema_version: 1,
                    session_id: session_id.clone(),
                    capture_id: Uuid::new_v4().to_string(),
                    events: Vec::new(),
                },
            )
            .unwrap();
        let cleanup = storage
            .write_cleanup(
                &session_id,
                created_at,
                &sample_cleanup(&session_id, &account),
            )
            .unwrap();
        let mut row = new_session_row(&session_id, &account, created_at, &audio, &transcript);
        row.context_present = true;
        row.cleanup_status = "succeeded".into();
        row.cleanup_path = Some(cleanup);
        storage.insert_session(&row).unwrap();

        let artifacts = storage
            .enumerate_session_artifacts(&session_id, created_at)
            .unwrap();
        assert_eq!(
            artifacts
                .iter()
                .map(|artifact| artifact.kind)
                .collect::<Vec<_>>(),
            vec![
                SessionArtifactKind::Audio,
                SessionArtifactKind::Transcript,
                SessionArtifactKind::Context,
                SessionArtifactKind::Cleanup,
            ]
        );
        let session_dir = storage
            .crypto
            .account_dir(&account)
            .join(date_bucket(created_at))
            .join(&session_id);
        assert!(storage.delete(&session_id).unwrap());
        assert!(!session_dir.exists());
    }

    #[cfg(unix)]
    #[test]
    fn session_delete_does_not_follow_symlinks_or_db_paths_outside_account() {
        use std::os::unix::fs::symlink;

        let dir = tmpdir();
        let (storage, account) = storage_with_account(&dir);
        let session_id = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let audio = storage
            .write_audio(&session_id, created_at, b"audio")
            .unwrap();
        let transcript = storage
            .write_transcript(
                &session_id,
                created_at,
                &sample_transcript(&session_id, &account, "raw"),
            )
            .unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("must-survive"), b"safe").unwrap();

        let session_dir = storage
            .crypto
            .account_dir(&account)
            .join(date_bucket(created_at))
            .join(&session_id);
        symlink(&outside, session_dir.join("escape-link")).unwrap();
        let mut row = new_session_row(&session_id, &account, created_at, &audio, &transcript);
        row.audio_path = Some(outside.join("must-survive").to_string_lossy().into_owned());
        storage.insert_session(&row).unwrap();

        assert!(storage.delete(&session_id).unwrap());
        assert!(!session_dir.exists());
        assert_eq!(
            std::fs::read(outside.join("must-survive")).unwrap(),
            b"safe"
        );
    }

    #[cfg(unix)]
    #[test]
    fn artifact_enumeration_rejects_symlinked_fixed_artifact() {
        use std::os::unix::fs::symlink;

        let dir = tmpdir();
        let (storage, account) = storage_with_account(&dir);
        let session_id = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let session_dir = storage
            .crypto
            .account_dir(&account)
            .join(date_bucket(created_at))
            .join(&session_id);
        std::fs::create_dir_all(&session_dir).unwrap();
        let outside = dir.path().join("outside-cleanup");
        std::fs::write(&outside, b"not-an-artifact").unwrap();
        symlink(&outside, session_dir.join(CLEANUP_ENC)).unwrap();
        assert!(matches!(
            storage.enumerate_session_artifacts(&session_id, created_at),
            Err(AccountError::CleanupIntegrity(_))
        ));
        assert!(outside.exists());
    }

    #[test]
    fn artifact_enumeration_rejects_symlinked_ancestor() {
        use std::os::unix::fs::symlink;

        let dir = tmpdir();
        let (storage, account) = storage_with_account(&dir);
        let session_id = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let outside = dir.path().join("outside-date");
        std::fs::create_dir_all(outside.join(&session_id)).unwrap();
        std::fs::write(outside.join(&session_id).join(CLEANUP_ENC), b"outside").unwrap();
        let account_dir = storage.crypto.account_dir(&account);
        std::fs::create_dir_all(&account_dir).unwrap();
        symlink(&outside, account_dir.join(date_bucket(created_at))).unwrap();

        assert!(matches!(
            storage.enumerate_session_artifacts(&session_id, created_at),
            Err(AccountError::CleanupIntegrity(_))
        ));
        assert!(outside.join(&session_id).join(CLEANUP_ENC).exists());
    }

    #[test]
    fn deleting_account_removes_cleanup_artifact_directory() {
        let dir = tmpdir();
        let keychain = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let crypto =
            Arc::new(Crypto::new(dir.path().to_path_buf(), keychain, fast_params()).unwrap());
        let alice = crypto.setup_first_account("alice", "p1").unwrap();
        let alice_keys = crypto.snapshot_active_account(&alice.account_id).unwrap();
        let alice_storage = Storage::bind(Arc::clone(&crypto), alice_keys);
        let session_id = Uuid::new_v4().to_string();
        let rel = alice_storage
            .write_cleanup(
                &session_id,
                1_700_000_000,
                &sample_cleanup(&session_id, &alice.account_id),
            )
            .unwrap();
        assert!(crypto.account_dir(&alice.account_id).join(&rel).exists());

        crypto.create_account("bob", "p2").unwrap();
        crypto.delete_account(&alice.account_id).unwrap();
        assert!(!crypto.account_dir(&alice.account_id).exists());
    }

    #[test]
    fn write_read_context_roundtrip_and_rejects_tampering() {
        let dir = tmpdir();
        let (storage, _acct) = storage_with_account(&dir);
        let sid = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let context = ClipboardContextFile {
            schema_version: 1,
            session_id: sid.clone(),
            capture_id: Uuid::new_v4().to_string(),
            events: vec![ContextEvent {
                sequence: 1,
                source_sample_rate: 48_000,
                sample_offset: 50_400,
                kind: ContextEventKind::ContextEventPlainText as i32,
                plain_text: "上下文".into(),
                html_fragment: String::new(),
                absolute_paths: Vec::new(),
            }],
        };
        let rel = storage.write_context(&sid, created_at, &context).unwrap();
        assert!(rel.ends_with(CONTEXT_ENC));
        assert_eq!(
            storage.read_context(&sid, created_at).unwrap().events.len(),
            1
        );

        let path = storage
            .crypto
            .account_dir(&storage.active_account().unwrap())
            .join(rel);
        let mut bytes = std::fs::read(&path).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(path, bytes).unwrap();
        assert!(storage.read_context(&sid, created_at).is_err());
    }

    #[test]
    fn context_ciphertext_cannot_be_replayed_to_another_session() {
        let dir = tmpdir();
        let (storage, _acct) = storage_with_account(&dir);
        let created_at = 1_700_000_000;
        let first = Uuid::new_v4().to_string();
        let second = Uuid::new_v4().to_string();
        let context = ClipboardContextFile {
            schema_version: 1,
            session_id: first.clone(),
            capture_id: Uuid::new_v4().to_string(),
            events: Vec::new(),
        };
        let first_rel = storage.write_context(&first, created_at, &context).unwrap();
        let second_rel = Storage::rel_path(created_at, &second, CONTEXT_ENC);
        let account = storage.active_account().unwrap();
        let second_path = storage.crypto.account_dir(&account).join(second_rel);
        std::fs::create_dir_all(second_path.parent().unwrap()).unwrap();
        std::fs::copy(
            storage.crypto.account_dir(&account).join(first_rel),
            second_path,
        )
        .unwrap();

        assert!(matches!(
            storage.read_context(&second, created_at),
            Err(AccountError::Crypto(_))
        ));
    }

    #[test]
    fn rejects_context_with_path_traversal_capture_id() {
        let dir = tmpdir();
        let (storage, _account) = storage_with_account(&dir);
        let session_id = Uuid::new_v4().to_string();
        let context = ClipboardContextFile {
            schema_version: 1,
            session_id: session_id.clone(),
            capture_id: "../../outside".into(),
            events: Vec::new(),
        };
        assert!(matches!(
            storage.write_context(&session_id, 1_700_000_000, &context),
            Err(AccountError::ContextIntegrity(_))
        ));
    }

    // ── 会话元数据 CRUD ────────────────────────────────────────────────────────

    #[test]
    fn session_crud_roundtrip() {
        let dir = tmpdir();
        let (storage, acct) = storage_with_account(&dir);
        let sid = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let audio_rel = storage.write_audio(&sid, created_at, b"audio").unwrap();
        let trans_rel = storage
            .write_transcript(&sid, created_at, &sample_transcript(&sid, &acct, "hi"))
            .unwrap();
        let row = new_session_row(&sid, &acct, created_at, &audio_rel, &trans_rel);
        storage.insert_session(&row).unwrap();

        // get。
        let got = storage.get(&sid).unwrap().unwrap();
        assert_eq!(got.id, sid);
        assert_eq!(got.audio_path.as_deref(), Some(audio_rel.as_str()));

        // list。
        let list = storage.list(10, None).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, sid);

        // delete。
        assert!(storage.delete(&sid).unwrap());
        assert!(storage.get(&sid).unwrap().is_none());
        // 文件树目录已删。
        assert!(!storage.crypto.account_dir(&acct).join(&audio_rel).exists());
        let _ = trans_rel;
    }

    // ── 写会话原子性 ────────────────────────────────────────────────────────────

    /// 写 audio 成功 → 未 INSERT → DB 无行（未提交）。文件残留由 reconcile 清。
    #[test]
    fn write_files_without_insert_leaves_no_db_row() {
        let dir = tmpdir();
        let (storage, acct) = storage_with_account(&dir);
        let sid = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let _ = storage.write_audio(&sid, created_at, b"audio").unwrap();
        // 不调 insert_session → DB 无行。
        assert!(storage.get(&sid).unwrap().is_none(), "未 INSERT 应无 DB 行");
        let _ = acct;
    }

    // ── reconcile ────────────────────────────────────────────────────────────────

    /// 孤儿 DB 行（文件缺失）→ 删行。
    #[test]
    fn reconcile_removes_orphan_rows() {
        let dir = tmpdir();
        let (storage, acct) = storage_with_account(&dir);
        let sid = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let audio_rel = storage.write_audio(&sid, created_at, b"audio").unwrap();
        let trans_rel = storage
            .write_transcript(&sid, created_at, &sample_transcript(&sid, &acct, "hi"))
            .unwrap();
        storage
            .insert_session(&new_session_row(
                &sid, &acct, created_at, &audio_rel, &trans_rel,
            ))
            .unwrap();
        // 手动删文件 → 孤儿行。
        let _ = std::fs::remove_file(storage.crypto.account_dir(&acct).join(&audio_rel));

        let report = storage.reconcile().unwrap();
        assert_eq!(report.orphan_rows_removed, 1, "应删 1 孤儿行");
        assert!(storage.get(&sid).unwrap().is_none());
    }

    #[test]
    fn reconcile_recovers_raw_checkpoint_without_reinvoking_cleanup() {
        let dir = tmpdir();
        let (storage, account) = storage_with_account(&dir);
        let sid = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let audio_rel = storage.write_audio(&sid, created_at, b"audio").unwrap();
        let mut transcript = sample_transcript(&sid, &account, "raw");
        transcript.model = "model-a".into();
        let transcript_rel = storage
            .write_transcript(&sid, created_at, &transcript)
            .unwrap();
        let mut row = new_session_row(&sid, &account, created_at, &audio_rel, "");
        row.status = "transcribing".into();
        row.model = "model-a".into();
        row.transcript_path = None;
        storage.insert_session(&row).unwrap();

        let report = storage.reconcile().unwrap();
        assert_eq!(report.cleanup_recovered, 0);
        let recovered = storage.get(&sid).unwrap().unwrap();
        assert_eq!(recovered.status, "completed");
        assert_eq!(recovered.cleanup_status, "not_requested");
        assert_eq!(
            recovered.transcript_path.as_deref(),
            Some(transcript_rel.as_str())
        );
    }

    #[test]
    fn reconcile_recovers_processing_success_artifact() {
        let dir = tmpdir();
        let (storage, account) = storage_with_account(&dir);
        let sid = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let audio_rel = storage.write_audio(&sid, created_at, b"audio").unwrap();
        let mut transcript = sample_transcript(&sid, &account, "raw");
        transcript.model = "model-a".into();
        let transcript_rel = storage
            .write_transcript(&sid, created_at, &transcript)
            .unwrap();
        let cleanup_rel = storage
            .write_cleanup(&sid, created_at, &sample_cleanup(&sid, &account))
            .unwrap();
        let mut row = new_session_row(&sid, &account, created_at, &audio_rel, &transcript_rel);
        row.status = "cleaning_up".into();
        row.model = "model-a".into();
        row.cleanup_status = "processing".into();
        storage.insert_session(&row).unwrap();

        let report = storage.reconcile().unwrap();
        assert_eq!(report.cleanup_recovered, 1);
        let recovered = storage.get(&sid).unwrap().unwrap();
        assert_eq!(recovered.status, "completed");
        assert_eq!(recovered.cleanup_status, "succeeded");
        assert_eq!(
            recovered.cleanup_path.as_deref(),
            Some(cleanup_rel.as_str())
        );
    }

    #[test]
    fn reconcile_terminalizes_processing_without_artifact_as_interrupted() {
        let dir = tmpdir();
        let (storage, account) = storage_with_account(&dir);
        let sid = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let audio_rel = storage.write_audio(&sid, created_at, b"audio").unwrap();
        let mut transcript = sample_transcript(&sid, &account, "raw");
        transcript.model = "model-a".into();
        let transcript_rel = storage
            .write_transcript(&sid, created_at, &transcript)
            .unwrap();
        let mut row = new_session_row(&sid, &account, created_at, &audio_rel, &transcript_rel);
        row.status = "cleaning_up".into();
        row.model = "model-a".into();
        row.cleanup_status = "processing".into();
        storage.insert_session(&row).unwrap();

        let report = storage.reconcile().unwrap();
        assert_eq!(report.cleanup_interrupted, 1);
        let recovered = storage.get(&sid).unwrap().unwrap();
        assert_eq!(recovered.status, "completed");
        assert_eq!(recovered.cleanup_status, "failed");
        assert_eq!(
            recovered.cleanup_error_code.as_deref(),
            Some(CleanupFailureCode::Interrupted.as_str())
        );
        assert!(recovered.cleanup_path.is_none());
    }

    #[test]
    fn reconcile_invalidates_completed_success_with_corrupt_artifact() {
        let dir = tmpdir();
        let (storage, account) = storage_with_account(&dir);
        let sid = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let audio_rel = storage.write_audio(&sid, created_at, b"audio").unwrap();
        let transcript_rel = storage
            .write_transcript(&sid, created_at, &sample_transcript(&sid, &account, "raw"))
            .unwrap();
        let cleanup_rel = storage
            .write_cleanup(&sid, created_at, &sample_cleanup(&sid, &account))
            .unwrap();
        let mut row = new_session_row(&sid, &account, created_at, &audio_rel, &transcript_rel);
        row.cleanup_status = "succeeded".into();
        row.cleanup_path = Some(cleanup_rel.clone());
        storage.insert_session(&row).unwrap();

        let cleanup_path = storage.crypto.account_dir(&account).join(&cleanup_rel);
        let mut bytes = std::fs::read(&cleanup_path).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(&cleanup_path, bytes).unwrap();

        let report = storage.reconcile().unwrap();
        assert_eq!(report.cleanup_invalidated, 1);
        let recovered = storage.get(&sid).unwrap().unwrap();
        assert_eq!(recovered.cleanup_status, "failed");
        assert_eq!(
            recovered.cleanup_error_code.as_deref(),
            Some(CleanupFailureCode::ArtifactInvalid.as_str())
        );
        assert!(recovered.cleanup_path.is_none());
        assert!(!cleanup_path.exists());
    }

    #[test]
    fn reconcile_downgrades_row_when_context_is_missing() {
        let dir = tmpdir();
        let (storage, account) = storage_with_account(&dir);
        let session_id = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let audio = storage
            .write_audio(&session_id, created_at, b"audio")
            .unwrap();
        let mut row = new_session_row(&session_id, &account, created_at, &audio, "");
        row.transcript_path = None;
        row.context_present = true;
        storage.insert_session(&row).unwrap();

        let report = storage.reconcile().unwrap();
        // audio 完好（失败会话无 transcript）→ 不删行，只降级 context_present。
        assert_eq!(report.orphan_rows_removed, 0, "audio 完好 → 不删行");
        assert_eq!(report.context_downgraded, 1, "context 缺失 → 降级");
        let row = storage.get(&session_id).unwrap().expect("行保留");
        assert!(!row.context_present, "降级后 context_present=false");
    }

    /// 已成功转写的会话，其 context.pb.enc 损坏（AEAD tag 失配）时，reconcile
    /// 不得清除引用或回收媒体；用户修复/恢复文件后仍应有机会重新读取。
    #[test]
    fn reconcile_downgrades_corrupt_context_and_preserves_transcript() {
        let dir = tmpdir();
        let (storage, account) = storage_with_account(&dir);
        let session_id = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let audio_rel = storage
            .write_audio(&session_id, created_at, b"RIFF...pcm")
            .unwrap();
        let trans_rel = storage
            .write_transcript(
                &session_id,
                created_at,
                &sample_transcript(&session_id, &account, "你好世界"),
            )
            .unwrap();
        let context = ClipboardContextFile {
            schema_version: 1,
            session_id: session_id.clone(),
            capture_id: Uuid::new_v4().to_string(),
            events: vec![ContextEvent {
                sequence: 1,
                source_sample_rate: 48_000,
                sample_offset: 50_400,
                kind: ContextEventKind::ContextEventPlainText as i32,
                plain_text: "上下文".into(),
                html_fragment: String::new(),
                absolute_paths: Vec::new(),
            }],
        };
        storage
            .write_context(&session_id, created_at, &context)
            .unwrap();
        let mut row = new_session_row(&session_id, &account, created_at, &audio_rel, &trans_rel);
        row.context_present = true;
        storage.insert_session(&row).unwrap();

        // 篡改 context.pb.enc 使 read_context 失败（AEAD tag 失配）。
        let ctx_path = storage
            .crypto
            .account_dir(&storage.active_account().unwrap())
            .join(Storage::rel_path(created_at, &session_id, CONTEXT_ENC));
        let mut bytes = std::fs::read(&ctx_path).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(&ctx_path, bytes).unwrap();

        let report = storage.reconcile().unwrap();
        assert_eq!(report.orphan_rows_removed, 0, "transcript 完好 → 不删行");
        assert_eq!(report.context_downgraded, 0, "损坏 context 不是确认缺失");
        let row = storage.get(&session_id).unwrap().expect("行保留");
        assert!(row.context_present, "损坏 context 必须保留恢复标记");
        // 完好的 audio/transcript 仍可读——未被摧毁。
        assert_eq!(storage.read_audio(&audio_rel).unwrap(), b"RIFF...pcm");
        assert_eq!(
            storage.read_transcript(&trans_rel).unwrap().full_text,
            "你好世界"
        );
    }

    #[test]
    fn reconcile_preserves_media_when_any_context_is_unresolved() {
        let dir = tmpdir();
        let (storage, account) = storage_with_account(&dir);
        let session_id = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let audio_rel = storage
            .write_audio(&session_id, created_at, b"audio")
            .unwrap();
        let trans_rel = storage
            .write_transcript(
                &session_id,
                created_at,
                &sample_transcript(&session_id, &account, "body"),
            )
            .unwrap();
        let capture_id = Uuid::new_v4().to_string();
        let context = ClipboardContextFile {
            schema_version: 1,
            session_id: session_id.clone(),
            capture_id: capture_id.clone(),
            events: vec![ContextEvent {
                sequence: 1,
                source_sample_rate: 48_000,
                sample_offset: 1,
                kind: ContextEventKind::ContextEventPlainText as i32,
                plain_text: "context".into(),
                html_fragment: String::new(),
                absolute_paths: Vec::new(),
            }],
        };
        storage
            .write_context(&session_id, created_at, &context)
            .unwrap();
        let mut row = new_session_row(&session_id, &account, created_at, &audio_rel, &trans_rel);
        row.context_present = true;
        storage.insert_session(&row).unwrap();

        let data_root = storage.crypto.account_dir(&account);
        let app_root = data_root.parent().and_then(|data| data.parent()).unwrap();
        let media_dir = app_root
            .join("cache")
            .join("clipboard-context")
            .join(&account)
            .join(&capture_id);
        std::fs::create_dir_all(&media_dir).unwrap();
        std::fs::write(media_dir.join("image.png"), b"png").unwrap();

        let context_path = data_root.join(Storage::rel_path(created_at, &session_id, CONTEXT_ENC));
        let mut bytes = std::fs::read(&context_path).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(context_path, bytes).unwrap();

        let report = storage.reconcile().unwrap();
        assert_eq!(report.context_downgraded, 0);
        assert_eq!(report.orphan_capture_dirs_removed, 0);
        assert!(
            media_dir.exists(),
            "无法解析 context 时必须保守保留媒体缓存"
        );
    }

    #[test]
    fn reconcile_error_classification_only_treats_not_found_as_missing() {
        let missing = AccountError::Io(std::io::Error::from(std::io::ErrorKind::NotFound));
        let permission =
            AccountError::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        let corrupt = AccountError::Crypto(seasnail_crypto::Error::AuthenticationFailed);
        let schema = AccountError::ContextIntegrity("unsupported schema".into());
        assert!(context_is_confirmed_missing(&missing));
        assert!(!context_is_confirmed_missing(&permission));
        assert!(!context_is_confirmed_missing(&corrupt));
        assert!(!context_is_confirmed_missing(&schema));
    }

    /// 孤儿文件目录（无 DB 行）→ 删目录。
    #[test]
    fn reconcile_removes_orphan_files() {
        let dir = tmpdir();
        let (storage, _acct) = storage_with_account(&dir);
        let sid = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let _rel = storage.write_audio(&sid, created_at, b"audio").unwrap();
        // 不 INSERT → 孤儿文件。
        let report = storage.reconcile().unwrap();
        assert_eq!(report.orphan_dirs_removed, 1, "应删 1 孤儿文件目录");
        assert!(storage.get(&sid).unwrap().is_none());
    }

    /// 无孤儿 → reconcile 零清理。
    #[test]
    fn reconcile_no_op_when_consistent() {
        let dir = tmpdir();
        let (storage, acct) = storage_with_account(&dir);
        let sid = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let audio_rel = storage.write_audio(&sid, created_at, b"audio").unwrap();
        let trans_rel = storage
            .write_transcript(&sid, created_at, &sample_transcript(&sid, &acct, "hi"))
            .unwrap();
        storage
            .insert_session(&new_session_row(
                &sid, &acct, created_at, &audio_rel, &trans_rel,
            ))
            .unwrap();
        let report = storage.reconcile().unwrap();
        assert_eq!(report, ReconcileReport::default());
    }

    #[test]
    fn reconcile_removes_unreferenced_media_capture_directory() {
        let dir = tmpdir();
        let (storage, account) = storage_with_account(&dir);
        let capture = Uuid::new_v4().to_string();
        let capture_dir = dir
            .path()
            .join("cache")
            .join("clipboard-context")
            .join(&account)
            .join(&capture);
        std::fs::create_dir_all(capture_dir.join("2026-08-16/image")).unwrap();
        std::fs::write(capture_dir.join("2026-08-16/image/event.png"), b"png").unwrap();

        let report = storage.reconcile().unwrap();
        assert_eq!(report.orphan_capture_dirs_removed, 1);
        assert!(!capture_dir.exists());
    }

    #[test]
    fn deleting_a_context_session_removes_its_media_capture_directory() {
        let dir = tmpdir();
        let (storage, account) = storage_with_account(&dir);
        let session_id = Uuid::new_v4().to_string();
        let capture_id = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;
        let audio = storage
            .write_audio(&session_id, created_at, b"audio")
            .unwrap();
        storage
            .write_context(
                &session_id,
                created_at,
                &ClipboardContextFile {
                    schema_version: 1,
                    session_id: session_id.clone(),
                    capture_id: capture_id.clone(),
                    events: Vec::new(),
                },
            )
            .unwrap();
        let mut row = new_session_row(&session_id, &account, created_at, &audio, "");
        row.transcript_path = None;
        row.context_present = true;
        storage.insert_session(&row).unwrap();
        let capture_dir = dir
            .path()
            .join("cache/clipboard-context")
            .join(&account)
            .join(&capture_id);
        std::fs::create_dir_all(&capture_dir).unwrap();

        assert!(storage.delete(&session_id).unwrap());
        assert!(!capture_dir.exists());
    }

    #[test]
    fn deleting_one_of_two_sessions_sharing_a_capture_keeps_media_until_last_reference() {
        let dir = tmpdir();
        let (storage, account) = storage_with_account(&dir);
        let capture_id = Uuid::new_v4().to_string();
        let first = Uuid::new_v4().to_string();
        let second = Uuid::new_v4().to_string();
        let created_at = 1_700_000_000;

        for session_id in [&first, &second] {
            let audio = storage
                .write_audio(session_id, created_at, b"audio")
                .unwrap();
            storage
                .write_context(
                    session_id,
                    created_at,
                    &ClipboardContextFile {
                        schema_version: 1,
                        session_id: session_id.to_string(),
                        capture_id: capture_id.clone(),
                        events: Vec::new(),
                    },
                )
                .unwrap();
            let mut row = new_session_row(session_id, &account, created_at, &audio, "");
            row.transcript_path = None;
            row.context_present = true;
            storage.insert_session(&row).unwrap();
        }
        let capture_dir = dir
            .path()
            .join("cache/clipboard-context")
            .join(&account)
            .join(&capture_id);
        std::fs::create_dir_all(&capture_dir).unwrap();

        assert!(storage.delete(&first).unwrap());
        assert!(capture_dir.exists(), "另一个会话仍引用时不得删除截图缓存");
        assert!(storage.delete(&second).unwrap());
        assert!(!capture_dir.exists(), "最后一个引用删除后应回收缓存");
    }

    // ── search ───────────────────────────────────────────────────────────────────

    #[test]
    fn search_matches_substring() {
        let dir = tmpdir();
        let (storage, acct) = storage_with_account(&dir);
        // 两个会话：一个含「你好」，一个含「再见」。
        for (i, full) in ["你好世界", "再见朋友"].iter().enumerate() {
            let sid = Uuid::new_v4().to_string();
            let created_at = 1_700_000_000 + i as i64;
            let audio_rel = storage.write_audio(&sid, created_at, b"a").unwrap();
            let trans_rel = storage
                .write_transcript(&sid, created_at, &sample_transcript(&sid, &acct, full))
                .unwrap();
            storage
                .insert_session(&new_session_row(
                    &sid, &acct, created_at, &audio_rel, &trans_rel,
                ))
                .unwrap();
        }
        let hits = storage.search("你好", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains("你好"));
        let no_hits = storage.search("不存在", 10).unwrap();
        assert!(no_hits.is_empty());
    }

    #[test]
    fn search_empty_query_returns_empty() {
        let dir = tmpdir();
        let (storage, _acct) = storage_with_account(&dir);
        assert!(storage.search("", 10).unwrap().is_empty());
    }

    // ── 未解锁 ───────────────────────────────────────────────────────────────────

    #[test]
    fn operations_require_unlocked() {
        // 无活跃账户的 Storage → NotUnlocked。
        let dir_empty = tmpdir();
        let kc2 = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let empty_crypto =
            Arc::new(Crypto::new(dir_empty.path().to_path_buf(), kc2, fast_params()).unwrap());
        let storage_empty = Storage::new(empty_crypto);
        match storage_empty.write_audio("sid", 0, b"x") {
            Err(AccountError::NotUnlocked) => {}
            other => panic!("期望 NotUnlocked，实际 {other:?}"),
        }
        // 有活跃账户的 Storage 仍可用。
        let dir2 = tmpdir();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let crypto = Arc::new(Crypto::new(dir2.path().to_path_buf(), kc, fast_params()).unwrap());
        crypto.setup_first_account("alice", "p").unwrap();
        let storage = Storage::new(crypto);
        assert!(storage.write_audio("sid2", 1_700_000_000, b"x").is_ok());
    }

    // ── date_bucket ───────────────────────────────────────────────────────────────

    #[test]
    fn date_bucket_format() {
        // 1_700_000_000 = 2023-11-14T22:13:20Z。
        assert_eq!(date_bucket(1_700_000_000), "2023-11-14");
    }
}
