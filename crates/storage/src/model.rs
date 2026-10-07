//! sessions / tokens 行模型 + CRUD（ST-M2.2 上机）。
//!
//! 行类型为 DB 层结构（非 API DTO）；API DTO 与 wire 换算（epoch→date-time、
//! scopes→JSON 数组）属 ST-M2.6 / ST-M5.1。created_at 存 epoch 秒（INTEGER）利
//! 游标分页；scopes 存空格分隔 TEXT（scope 名不含空格），ST-M2.5 verify 路径用。

use rusqlite::{params, Connection, OptionalExtension, Row};
use sha2::{Digest, Sha256};

const SESSION_COLS: &str = "id, account_id, created_at, source, language, duration_sec, \
     status, model, input_device, file_name, audio_path, transcript_path, failure_reason, context_present, \
     cleanup_status, cleanup_path, cleanup_error_code";
const TOKEN_COLS: &str = "id, account_id, name, prefix, token_hash, is_root, scopes, \
     created_at, last_used_at";
const PROVIDER_CONFIG_COLS: &str = "id, name, provider_type, endpoint, endpoint_fingerprint, \
     model, created_at, updated_at";
const DICTIONARY_COLS: &str =
    "id, term, normalized_term, source, learning_event_id, created_at, updated_at";

/// sessions 表行。transcript 内容（speakers/segments/full_text）不入库，在文件树。
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
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
    /// failed 行的失败原因（ST-M3.7 状态机）；completed/transcribing 为 None。
    pub failure_reason: Option<String>,
    /// 是否在创建时提交了 clipboard context。旧行默认为 false；true 时 retry 必须读取 context.pb.enc。
    pub context_present: bool,
    /// Cleanup 独立状态；历史行由 migration v4 得到 `not_requested`。
    pub cleanup_status: String,
    /// `cleanup.pb.enc` 的会话目录相对路径，仅 succeeded 时存在。
    pub cleanup_path: Option<String>,
    /// Cleanup 失败的稳定脱敏错误码。
    pub cleanup_error_code: Option<String>,
}

impl SessionRow {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            account_id: r.get("account_id")?,
            created_at: r.get("created_at")?,
            source: r.get("source")?,
            language: r.get("language")?,
            duration_sec: r.get("duration_sec")?,
            status: r.get("status")?,
            model: r.get("model")?,
            input_device: r.get("input_device")?,
            file_name: r.get("file_name")?,
            audio_path: r.get("audio_path")?,
            transcript_path: r.get("transcript_path")?,
            failure_reason: r.get("failure_reason")?,
            context_present: r.get::<_, i64>("context_present")? != 0,
            cleanup_status: r.get("cleanup_status")?,
            cleanup_path: r.get("cleanup_path")?,
            cleanup_error_code: r.get("cleanup_error_code")?,
        })
    }
}

/// 账户库内的 reasoning provider 配置。Credential 不属于此行，只存 Keychain。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningProviderConfigRow {
    pub id: String,
    pub name: String,
    pub provider_type: String,
    pub endpoint: String,
    pub endpoint_fingerprint: String,
    pub model: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl ReasoningProviderConfigRow {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            name: r.get("name")?,
            provider_type: r.get("provider_type")?,
            endpoint: r.get("endpoint")?,
            endpoint_fingerprint: r.get("endpoint_fingerprint")?,
            model: r.get("model")?,
            created_at: r.get("created_at")?,
            updated_at: r.get("updated_at")?,
        })
    }
}

/// singleton=1 的 cleanup 设置行。不存在由上层解释为默认关闭。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupSettingsRow {
    pub enabled: bool,
    pub selected_provider_config_id: Option<String>,
    pub custom_prompt: Option<String>,
    pub updated_at: i64,
}

/// 账户级 Dictionary 词条。`normalized_term` 仅用于 repository 查询和唯一约束，
/// transport DTO 不应暴露该字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictionaryEntryRow {
    pub id: String,
    pub term: String,
    pub normalized_term: String,
    pub source: String,
    pub learning_event_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl DictionaryEntryRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            term: row.get("term")?,
            normalized_term: row.get("normalized_term")?,
            source: row.get("source")?,
            learning_event_id: row.get("learning_event_id")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

/// Dictionary keyset 分页位置。source_rank 固定为 manual=0、learned=1。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictionaryCursor {
    pub source_rank: i64,
    pub updated_at: i64,
    pub id: String,
}

/// MVP 支持的 provider 配置类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderType {
    OpenAi,
    OpenAiCompatibleCloud,
    SelfHostedPublic,
    SelfHostedPrivate,
}

impl ProviderType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::OpenAiCompatibleCloud => "openai_compatible_cloud",
            Self::SelfHostedPublic => "openai_compatible_self_hosted_public",
            Self::SelfHostedPrivate => "openai_compatible_self_hosted_private",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "openai" => Some(Self::OpenAi),
            "openai_compatible_cloud" => Some(Self::OpenAiCompatibleCloud),
            "openai_compatible_self_hosted_public" => Some(Self::SelfHostedPublic),
            "openai_compatible_self_hosted_private" => Some(Self::SelfHostedPrivate),
            _ => None,
        }
    }

    pub const fn requires_credential(self) -> bool {
        matches!(self, Self::OpenAi | Self::OpenAiCompatibleCloud)
    }
}

/// 创建/更新 provider config 的非 secret 输入。
#[derive(Debug, Clone, Copy)]
pub struct ProviderConfigInput<'a> {
    pub name: &'a str,
    pub provider_type: ProviderType,
    pub endpoint: &'a str,
    pub model: &'a str,
}

impl CleanupSettingsRow {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            enabled: r.get::<_, i64>("enabled")? != 0,
            selected_provider_config_id: r.get("selected_provider_config_id")?,
            custom_prompt: r.get("custom_prompt")?,
            updated_at: r.get("updated_at")?,
        })
    }
}

/// tokens 表行。token secret 不入库；prefix 展示 + token_hash 校验（ST-M2.5）。
#[derive(Debug, Clone, PartialEq)]
pub struct TokenRow {
    pub id: String,
    pub account_id: String,
    pub name: String,
    pub prefix: String,
    pub token_hash: String,
    pub is_root: bool,
    pub scopes: Vec<String>,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

impl TokenRow {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        let scopes_str: String = r.get("scopes")?;
        let scopes: Vec<String> = if scopes_str.is_empty() {
            Vec::new()
        } else {
            scopes_str.split_whitespace().map(String::from).collect()
        };
        Ok(Self {
            id: r.get("id")?,
            account_id: r.get("account_id")?,
            name: r.get("name")?,
            prefix: r.get("prefix")?,
            token_hash: r.get("token_hash")?,
            is_root: r.get::<_, i64>("is_root")? != 0,
            scopes,
            created_at: r.get("created_at")?,
            last_used_at: r.get("last_used_at")?,
        })
    }
}

// ── sessions CRUD ──────────────────────────────────────────────────────────────

/// 插入 session 行（id / created_at 由调用方设定，便于游标分页确定性）。
pub fn insert_session(conn: &Connection, s: &SessionRow) -> Result<(), crate::Error> {
    conn.execute(
        "INSERT INTO sessions
           (id, account_id, created_at, source, language, duration_sec, status, model,
            input_device, file_name, audio_path, transcript_path, context_present,
            cleanup_status, cleanup_path, cleanup_error_code)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            &s.id,
            &s.account_id,
            s.created_at,
            &s.source,
            &s.language,
            s.duration_sec,
            &s.status,
            &s.model,
            &s.input_device,
            &s.file_name,
            &s.audio_path,
            &s.transcript_path,
            s.context_present as i64,
            &s.cleanup_status,
            &s.cleanup_path,
            &s.cleanup_error_code,
        ],
    )?;
    Ok(())
}

/// 取单个 session。无则 None。
pub fn get_session(conn: &Connection, id: &str) -> Result<Option<SessionRow>, crate::Error> {
    let sql = format!("SELECT {SESSION_COLS} FROM sessions WHERE id = ?1");
    let row = conn
        .query_row(&sql, params![id], SessionRow::from_row)
        .optional()?;
    Ok(row)
}

/// 游标分页列 sessions：按 created_at DESC（同秒按 id DESC）取 limit 条；`before_created_at`
/// 为上一页末尾行 created_at，作不透明游标（编码属 ST-M5.1）。限本账户。
pub fn list_sessions(
    conn: &Connection,
    account_id: &str,
    limit: i64,
    before_created_at: Option<i64>,
) -> Result<Vec<SessionRow>, crate::Error> {
    let rows = match before_created_at {
        Some(before) => {
            let sql = format!(
                "SELECT {SESSION_COLS} FROM sessions
                 WHERE account_id = ?1 AND created_at < ?2
                 ORDER BY created_at DESC, id DESC
                 LIMIT ?3"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(params![account_id, before, limit], SessionRow::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        }
        None => {
            let sql = format!(
                "SELECT {SESSION_COLS} FROM sessions
                 WHERE account_id = ?1
                 ORDER BY created_at DESC, id DESC
                 LIMIT ?2"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(params![account_id, limit], SessionRow::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        }
    };
    Ok(rows)
}

/// 稳定游标分页：同秒会话按 id DESC 继续，避免仅按时间戳翻页漏项。
pub fn list_sessions_after(
    conn: &Connection,
    account_id: &str,
    limit: i64,
    before: Option<(i64, &str)>,
    source: Option<&str>,
) -> Result<Vec<SessionRow>, crate::Error> {
    let mut sql = format!("SELECT {SESSION_COLS} FROM sessions WHERE account_id = ?1");
    if source.is_some() {
        sql.push_str(" AND source = ?2");
    }
    match (source.is_some(), before) {
        (false, Some(_)) => sql.push_str(" AND (created_at < ?2 OR (created_at = ?2 AND id < ?3))"),
        (true, Some(_)) => sql.push_str(" AND (created_at < ?3 OR (created_at = ?3 AND id < ?4))"),
        _ => {}
    }
    sql.push_str(" ORDER BY created_at DESC, id DESC");
    sql.push_str(if source.is_some() && before.is_some() {
        " LIMIT ?5"
    } else if source.is_some() {
        " LIMIT ?3"
    } else if before.is_some() {
        " LIMIT ?4"
    } else {
        " LIMIT ?2"
    });
    let mut stmt = conn.prepare(&sql)?;
    let rows = match (source, before) {
        (Some(s), Some((t, id))) => {
            stmt.query_map(params![account_id, s, t, id, limit], SessionRow::from_row)?
        }
        (Some(s), None) => stmt.query_map(params![account_id, s, limit], SessionRow::from_row)?,
        (None, Some((t, id))) => {
            stmt.query_map(params![account_id, t, id, limit], SessionRow::from_row)?
        }
        (None, None) => stmt.query_map(params![account_id, limit], SessionRow::from_row)?,
    }
    .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// 删 session。返回是否删到行（二次删返 false）。
pub fn delete_session(conn: &Connection, id: &str) -> Result<bool, crate::Error> {
    let n = conn.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
    Ok(n > 0)
}

/// 把会话降级为「无上下文」：清 `context_present`，**保留** audio/transcript 行。
/// 用于启动期 reconcile：`context.pb.enc` 缺失/损坏但 audio+transcript 完好时，
/// 只降级不删行——避免静默摧毁一个完好的 transcript（修复 D-1）。返回是否改到行。
pub fn clear_context_present(conn: &Connection, id: &str) -> Result<bool, crate::Error> {
    let n = conn.execute(
        "UPDATE sessions SET context_present = 0 WHERE id = ?1 AND context_present = 1",
        params![id],
    )?;
    Ok(n > 0)
}

/// 更新转译结果（ST-M3.5 / ST-M3.7）：写完 transcript.pb.enc 后把行从 transcribing
/// 推进到 completed/failed，回填 transcript_path（失败 None）与 duration_sec；失败时
/// 同时写 `failure_reason`（ST-M3.7 状态机），成功/retry 重置时传 None 清空。
///
/// `model`（ST-M3.7 retry）：`Some(m)` 时同步行 model 为当前 runtime 标识（使行 model
/// 反映产当前 transcript 的真实模型，GET /sessions/{id}.model 与 transcript.model 一致）；
/// `None` 时**不动**（成功/失败路径 model 在 INSERT 时已定且不变）。用 `COALESCE(?6, model)`
/// 在**单条 UPDATE** 内条件更新——原子提交，不致「reset 成功但 model 更新失败」留半截行。
/// 返回是否更新到行。
pub fn update_session_outcome(
    conn: &Connection,
    id: &str,
    status: &str,
    transcript_path: Option<&str>,
    duration_sec: f64,
    failure_reason: Option<&str>,
    model: Option<&str>,
) -> Result<bool, crate::Error> {
    let n = conn.execute(
        "UPDATE sessions SET status = ?2, transcript_path = ?3, duration_sec = ?4, \
         failure_reason = ?5, model = COALESCE(?6, model), \
         cleanup_status = CASE WHEN ?2 = 'transcribing' THEN 'not_requested' ELSE cleanup_status END, \
         cleanup_path = CASE WHEN ?2 = 'transcribing' THEN NULL ELSE cleanup_path END, \
         cleanup_error_code = CASE WHEN ?2 = 'transcribing' THEN NULL ELSE cleanup_error_code END \
         WHERE id = ?1",
        params![
            id,
            status,
            transcript_path,
            duration_sec,
            failure_reason,
            model
        ],
    )?;
    Ok(n > 0)
}

/// failed → transcribing 的 retry 起点。旧状态与旧 ASR model 必须同时匹配，避免
/// 迟到 retry 请求清空另一代任务的结果。
pub fn begin_retry(
    conn: &Connection,
    id: &str,
    expected_model: &str,
    new_model: &str,
) -> Result<bool, crate::Error> {
    require_nonempty(expected_model, "expected_model")?;
    require_nonempty(new_model, "new_model")?;
    let changed = conn.execute(
        "UPDATE sessions SET status='transcribing', transcript_path=NULL, duration_sec=0.0,
         failure_reason=NULL, model=?, cleanup_status='not_requested', cleanup_path=NULL,
         cleanup_error_code=NULL WHERE id=? AND status='failed' AND model=?",
        params![new_model, id, expected_model],
    )?;
    Ok(changed > 0)
}

/// 将仍属于指定 ASR worker 的 transcribing 行收口为 failed。
/// 旧 worker 必须同时匹配 model，避免 retry 后迟到错误覆盖新一代状态。
pub fn fail_transcription(
    conn: &Connection,
    id: &str,
    expected_model: &str,
    failure_reason: &str,
) -> Result<bool, crate::Error> {
    require_nonempty(expected_model, "expected_model")?;
    require_nonempty(failure_reason, "failure_reason")?;
    let changed = conn.execute(
        "UPDATE sessions SET status='failed', transcript_path=NULL, duration_sec=0.0,
         failure_reason=?, cleanup_status='not_requested', cleanup_path=NULL,
         cleanup_error_code=NULL WHERE id=? AND status='transcribing' AND model=?",
        params![failure_reason, id, expected_model],
    )?;
    Ok(changed > 0)
}

/// Raw transcript 落盘后的单次决策。枚举把 session/cleanup 合法组合固定在存储边界。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawCheckpointDecision {
    NotRequested,
    Disabled,
    Processing,
}

/// `cleaning_up/processing` 的终态候选。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupCompletion<'a> {
    Succeeded {
        cleanup_path: &'a str,
    },
    Failed {
        cleanup_path: Option<&'a str>,
        error_code: CleanupFailureCode,
    },
}

/// 可持久化到 session 行的稳定、脱敏 cleanup 错误码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupFailureCode {
    NotConfigured,
    EndpointRejected,
    CredentialMissing,
    InputTooLarge,
    Timeout,
    TransportError,
    HttpAuth,
    HttpRateLimit,
    HttpServer,
    ResponseTooLarge,
    ResponseInvalidJson,
    CleanedTextInvalid,
    PlaceholderInvalid,
    ArtifactWriteFailed,
    ArtifactInvalid,
    Interrupted,
    Busy,
}

impl CleanupFailureCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotConfigured => "cleanup_not_configured",
            Self::EndpointRejected => "cleanup_endpoint_rejected",
            Self::CredentialMissing => "cleanup_credential_missing",
            Self::InputTooLarge => "cleanup_input_too_large",
            Self::Timeout => "cleanup_timeout",
            Self::TransportError => "cleanup_transport_error",
            Self::HttpAuth => "cleanup_http_auth",
            Self::HttpRateLimit => "cleanup_http_rate_limit",
            Self::HttpServer => "cleanup_http_server",
            Self::ResponseTooLarge => "cleanup_response_too_large",
            Self::ResponseInvalidJson => "cleanup_response_invalid_json",
            Self::CleanedTextInvalid => "cleanup_cleaned_text_invalid",
            Self::PlaceholderInvalid => "cleanup_placeholder_invalid",
            Self::ArtifactWriteFailed => "cleanup_artifact_write_failed",
            Self::ArtifactInvalid => "cleanup_artifact_invalid",
            Self::Interrupted => "cleanup_interrupted",
            Self::Busy => "cleanup_busy",
        }
    }
}

/// Reconcile 只允许从这些明确前态执行 CAS。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryExpectedState {
    TranscribingNotRequested,
    CleaningUpProcessing,
    CompletedSucceeded,
}

impl RecoveryExpectedState {
    fn columns(self) -> (&'static str, &'static str) {
        match self {
            Self::TranscribingNotRequested => ("transcribing", "not_requested"),
            Self::CleaningUpProcessing => ("cleaning_up", "processing"),
            Self::CompletedSucceeded => ("completed", "succeeded"),
        }
    }
}

/// Reconcile 可提交的恢复结果；每个变体都形成合法的组合终态。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RecoveredCleanupOutcome<'a> {
    RawCheckpoint {
        transcript_path: &'a str,
        duration_sec: f64,
    },
    Succeeded {
        cleanup_path: &'a str,
    },
    Failed {
        cleanup_path: Option<&'a str>,
        error_code: CleanupFailureCode,
    },
}

fn require_nonempty(value: &str, name: &str) -> Result<(), crate::Error> {
    if value.is_empty() {
        return Err(crate::Error::InvalidMutation(format!(
            "{name} must not be empty"
        )));
    }
    Ok(())
}

fn validate_failure_fields(cleanup_path: Option<&str>) -> Result<(), crate::Error> {
    if cleanup_path.is_some_and(str::is_empty) {
        return Err(crate::Error::InvalidMutation(
            "cleanup_path must not be empty when present".into(),
        ));
    }
    Ok(())
}

/// `transcribing/not_requested` + expected model → raw checkpoint 的唯一合法组合。
pub fn checkpoint_raw(
    conn: &Connection,
    id: &str,
    expected_model: &str,
    transcript_path: &str,
    duration_sec: f64,
    decision: RawCheckpointDecision,
) -> Result<bool, crate::Error> {
    require_nonempty(expected_model, "expected_model")?;
    require_nonempty(transcript_path, "transcript_path")?;
    if !duration_sec.is_finite() || duration_sec < 0.0 {
        return Err(crate::Error::InvalidMutation(
            "duration_sec must be finite and non-negative".into(),
        ));
    }
    let (status, cleanup_status) = match decision {
        RawCheckpointDecision::NotRequested => ("completed", "not_requested"),
        RawCheckpointDecision::Disabled => ("completed", "disabled"),
        RawCheckpointDecision::Processing => ("cleaning_up", "processing"),
    };
    let changed = conn.execute(
        "UPDATE sessions SET status=?4, transcript_path=?5, duration_sec=?6,
         failure_reason=NULL, cleanup_status=?7, cleanup_path=NULL, cleanup_error_code=NULL
         WHERE id=?1 AND status='transcribing' AND cleanup_status=?3 AND model=?2",
        params![
            id,
            expected_model,
            "not_requested",
            status,
            transcript_path,
            duration_sec,
            cleanup_status,
        ],
    )?;
    Ok(changed > 0)
}

/// `cleaning_up/processing` + expected model → `completed/succeeded|failed`。
pub fn complete_cleanup(
    conn: &Connection,
    id: &str,
    expected_model: &str,
    completion: CleanupCompletion<'_>,
) -> Result<bool, crate::Error> {
    require_nonempty(expected_model, "expected_model")?;
    let (cleanup_status, cleanup_path, error_code) = match completion {
        CleanupCompletion::Succeeded { cleanup_path } => {
            require_nonempty(cleanup_path, "cleanup_path")?;
            ("succeeded", Some(cleanup_path), None)
        }
        CleanupCompletion::Failed {
            cleanup_path,
            error_code,
        } => {
            validate_failure_fields(cleanup_path)?;
            ("failed", cleanup_path, Some(error_code.as_str()))
        }
    };
    let changed = conn.execute(
        "UPDATE sessions SET status='completed', cleanup_status=?4, cleanup_path=?5,
         cleanup_error_code=?6, failure_reason=NULL
         WHERE id=?1 AND status='cleaning_up' AND cleanup_status=?3 AND model=?2
           AND transcript_path IS NOT NULL",
        params![
            id,
            expected_model,
            "processing",
            cleanup_status,
            cleanup_path,
            error_code,
        ],
    )?;
    Ok(changed > 0)
}

/// 启动/解锁 reconcile 的状态恢复 CAS。错误前态、错误 model 或已删除行返回 false。
pub fn recover_interrupted_cleanup(
    conn: &Connection,
    id: &str,
    expected_model: &str,
    expected: RecoveryExpectedState,
    recovered: RecoveredCleanupOutcome<'_>,
) -> Result<bool, crate::Error> {
    require_nonempty(expected_model, "expected_model")?;
    let (expected_status, expected_cleanup) = expected.columns();
    if !matches!(
        (expected, recovered),
        (
            RecoveryExpectedState::TranscribingNotRequested,
            RecoveredCleanupOutcome::RawCheckpoint { .. }
        ) | (
            RecoveryExpectedState::CleaningUpProcessing,
            RecoveredCleanupOutcome::Succeeded { .. }
        ) | (
            RecoveryExpectedState::CleaningUpProcessing,
            RecoveredCleanupOutcome::Failed { .. }
        ) | (
            RecoveryExpectedState::CompletedSucceeded,
            RecoveredCleanupOutcome::Failed { .. }
        )
    ) {
        return Err(crate::Error::InvalidMutation(
            "recovery outcome is incompatible with expected state".into(),
        ));
    }
    let (status, cleanup_status, transcript_path, duration_sec, cleanup_path, error_code) =
        match recovered {
            RecoveredCleanupOutcome::RawCheckpoint {
                transcript_path,
                duration_sec,
            } => {
                require_nonempty(transcript_path, "transcript_path")?;
                if !duration_sec.is_finite() || duration_sec < 0.0 {
                    return Err(crate::Error::InvalidMutation(
                        "duration_sec must be finite and non-negative".into(),
                    ));
                }
                (
                    "completed",
                    "not_requested",
                    Some(transcript_path),
                    Some(duration_sec),
                    None,
                    None,
                )
            }
            RecoveredCleanupOutcome::Succeeded { cleanup_path } => {
                require_nonempty(cleanup_path, "cleanup_path")?;
                (
                    "completed",
                    "succeeded",
                    None,
                    None,
                    Some(cleanup_path),
                    None,
                )
            }
            RecoveredCleanupOutcome::Failed {
                cleanup_path,
                error_code,
            } => {
                validate_failure_fields(cleanup_path)?;
                (
                    "completed",
                    "failed",
                    None,
                    None,
                    cleanup_path,
                    Some(error_code.as_str()),
                )
            }
        };
    let requires_raw_checkpoint =
        !matches!(recovered, RecoveredCleanupOutcome::RawCheckpoint { .. });
    let changed = conn.execute(
        "UPDATE sessions SET status=?5, cleanup_status=?6,
         transcript_path=COALESCE(?7, transcript_path),
         duration_sec=COALESCE(?8, duration_sec), cleanup_path=?9, cleanup_error_code=?10,
         failure_reason=NULL
         WHERE id=?1 AND status=?2 AND cleanup_status=?3 AND model=?4
           AND (?11=0 OR transcript_path IS NOT NULL)",
        params![
            id,
            expected_status,
            expected_cleanup,
            expected_model,
            status,
            cleanup_status,
            transcript_path,
            duration_sec,
            cleanup_path,
            error_code,
            requires_raw_checkpoint as i64,
        ],
    )?;
    Ok(changed > 0)
}

// ── reasoning provider / cleanup settings CRUD ───────────────────────────────

fn validate_text_field(value: &str, name: &str, max_chars: usize) -> Result<(), crate::Error> {
    if value.trim() != value
        || value.is_empty()
        || value.chars().count() > max_chars
        || value.chars().any(char::is_control)
    {
        return Err(crate::Error::InvalidMutation(format!("invalid {name}")));
    }
    Ok(())
}

/// 仅做持久化前的 URL 结构规范化；DNS/IP 分类和连接 pin 属于 hardened client 层。
pub fn normalize_provider_endpoint(
    provider_type: ProviderType,
    endpoint: &str,
) -> Result<String, crate::Error> {
    if endpoint.len() > 2048 {
        return Err(crate::Error::InvalidMutation(
            "endpoint exceeds 2048 bytes".into(),
        ));
    }
    let mut url = url::Url::parse(endpoint)
        .map_err(|_| crate::Error::InvalidMutation("invalid endpoint".into()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(crate::Error::InvalidMutation("invalid endpoint".into()));
    }
    match provider_type {
        ProviderType::OpenAi => {
            if url.scheme() != "https"
                || url.host_str() != Some("api.openai.com")
                || url.port().is_some()
            {
                return Err(crate::Error::InvalidMutation(
                    "openai endpoint must use the registry host".into(),
                ));
            }
            let path = url.path().trim_end_matches('/');
            if !matches!(path, "" | "/v1" | "/v1/chat/completions" | "/v1/models") {
                return Err(crate::Error::InvalidMutation(
                    "openai endpoint must use the registry base path".into(),
                ));
            }
            url.set_path("/v1");
        }
        ProviderType::OpenAiCompatibleCloud | ProviderType::SelfHostedPublic
            if url.scheme() != "https" =>
        {
            return Err(crate::Error::InvalidMutation(
                "cloud and public self-hosted endpoints must use HTTPS".into(),
            ));
        }
        _ => {}
    }
    let mut parts: Vec<_> = url
        .path_segments()
        .ok_or_else(|| crate::Error::InvalidMutation("invalid endpoint path".into()))?
        .filter(|part| !part.is_empty())
        .collect();
    if parts.ends_with(&["chat", "completions"]) {
        parts.truncate(parts.len() - 2);
    } else if parts.last() == Some(&"models") {
        parts.pop();
    }
    if parts.is_empty() {
        parts.push("v1");
    }
    url.set_path(&format!("/{}", parts.join("/")));
    let canonical = url.to_string().trim_end_matches('/').to_owned();
    if canonical.len() > 2048 {
        return Err(crate::Error::InvalidMutation(
            "endpoint exceeds 2048 bytes".into(),
        ));
    }
    Ok(canonical)
}

pub fn provider_endpoint_fingerprint(provider_type: ProviderType, endpoint: &str) -> String {
    let digest = Sha256::digest(format!("{}\0{endpoint}", provider_type.as_str()).as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn validated_provider_row(
    id: String,
    created_at: i64,
    updated_at: i64,
    input: ProviderConfigInput<'_>,
) -> Result<ReasoningProviderConfigRow, crate::Error> {
    validate_text_field(input.name, "provider name", 128)?;
    validate_text_field(input.model, "provider model", 256)?;
    let endpoint = normalize_provider_endpoint(input.provider_type, input.endpoint)?;
    Ok(ReasoningProviderConfigRow {
        id,
        name: input.name.to_owned(),
        provider_type: input.provider_type.as_str().to_owned(),
        endpoint_fingerprint: provider_endpoint_fingerprint(input.provider_type, &endpoint),
        endpoint,
        model: input.model.to_owned(),
        created_at,
        updated_at,
    })
}

/// 生成 canonical UUID 并创建非 secret provider config。
pub fn create_provider_config(
    conn: &Connection,
    input: ProviderConfigInput<'_>,
    now: i64,
) -> Result<ReasoningProviderConfigRow, crate::Error> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM reasoning_provider_configs",
        [],
        |row| row.get(0),
    )?;
    if count >= 32 {
        return Err(crate::Error::InvalidMutation(
            "provider config limit reached".into(),
        ));
    }
    let row = validated_provider_row(uuid::Uuid::new_v4().to_string(), now, now, input)?;
    insert_provider_config(conn, &row)?;
    Ok(row)
}

/// 更新已有 config；缺失返回 None，created_at 保持不变。
pub fn replace_provider_config(
    conn: &Connection,
    id: &str,
    input: ProviderConfigInput<'_>,
    now: i64,
) -> Result<Option<ReasoningProviderConfigRow>, crate::Error> {
    let parsed = uuid::Uuid::parse_str(id)
        .ok()
        .filter(|value| value.hyphenated().to_string() == id)
        .ok_or_else(|| crate::Error::InvalidMutation("invalid provider config id".into()))?;
    let tx = conn.unchecked_transaction()?;
    let Some(existing) = get_provider_config(&tx, id)? else {
        return Ok(None);
    };
    let row = validated_provider_row(parsed.to_string(), existing.created_at, now, input)?;
    update_provider_config(&tx, &row)?;
    if existing.provider_type != row.provider_type
        || existing.endpoint_fingerprint != row.endpoint_fingerprint
    {
        disable_cleanup_if_selected_in_transaction(&tx, id, now)?;
    }
    tx.commit()?;
    Ok(Some(row))
}

/// Keychain credential 已先删除后，原子删除 config；若它被选中则同步关闭 cleanup。
pub fn remove_provider_config_and_disable(
    conn: &Connection,
    id: &str,
    now: i64,
) -> Result<bool, crate::Error> {
    if uuid::Uuid::parse_str(id)
        .ok()
        .is_none_or(|value| value.hyphenated().to_string() != id)
    {
        return Err(crate::Error::InvalidMutation(
            "invalid provider config id".into(),
        ));
    }
    let tx = conn.unchecked_transaction()?;
    let removed = delete_provider_config(&tx, id)?;
    if removed {
        disable_cleanup_if_selected_in_transaction(&tx, id, now)?;
        tx.execute(
            "UPDATE cleanup_settings SET selected_provider_config_id=NULL
             WHERE singleton=1 AND selected_provider_config_id=?1",
            params![id],
        )?;
    }
    tx.commit()?;
    Ok(removed)
}

fn disable_cleanup_if_selected_in_transaction(
    conn: &Connection,
    id: &str,
    now: i64,
) -> Result<(), crate::Error> {
    conn.execute(
        "UPDATE cleanup_settings SET enabled=0, updated_at=?2
         WHERE singleton=1 AND selected_provider_config_id=?1",
        params![id, now],
    )?;
    Ok(())
}

pub fn disable_cleanup_if_selected(
    conn: &Connection,
    id: &str,
    now: i64,
) -> Result<(), crate::Error> {
    let tx = conn.unchecked_transaction()?;
    disable_cleanup_if_selected_in_transaction(&tx, id, now)?;
    tx.commit()?;
    Ok(())
}

/// 保存 cleanup settings；credential 可用性由 daemon 从 DB+Keychain 合成后传入，
/// transaction 内再次确认 selected config 仍存在。
pub fn save_cleanup_settings(
    conn: &Connection,
    enabled: bool,
    selected_provider_config_id: Option<&str>,
    custom_prompt: Option<&str>,
    credential_usable: bool,
    now: i64,
) -> Result<CleanupSettingsRow, crate::Error> {
    if custom_prompt.is_some_and(|prompt| {
        prompt.is_empty()
            || prompt.len() > 32 * 1024
            || prompt.chars().any(|character| character == '\0')
    }) {
        return Err(crate::Error::InvalidMutation(
            "invalid custom prompt".into(),
        ));
    }
    if let Some(id) = selected_provider_config_id {
        if uuid::Uuid::parse_str(id)
            .ok()
            .is_none_or(|value| value.hyphenated().to_string() != id)
        {
            return Err(crate::Error::InvalidMutation(
                "invalid selected provider config id".into(),
            ));
        }
    }
    if enabled && (selected_provider_config_id.is_none() || !credential_usable) {
        return Err(crate::Error::InvalidMutation(
            "cleanup requires a selected provider with usable credential state".into(),
        ));
    }
    let tx = conn.unchecked_transaction()?;
    if let Some(id) = selected_provider_config_id {
        if get_provider_config(&tx, id)?.is_none() {
            return Err(crate::Error::InvalidMutation(
                "selected provider config does not exist".into(),
            ));
        }
    }
    let settings = CleanupSettingsRow {
        enabled,
        selected_provider_config_id: selected_provider_config_id.map(str::to_owned),
        custom_prompt: custom_prompt.map(str::to_owned),
        updated_at: now,
    };
    upsert_cleanup_settings(&tx, &settings)?;
    tx.commit()?;
    Ok(settings)
}

// 仅供本 crate 后续事务性 provider/settings facade 调用；daemon 不可直接绕过门禁。
#[allow(dead_code)]
pub(crate) fn insert_provider_config(
    conn: &Connection,
    config: &ReasoningProviderConfigRow,
) -> Result<(), crate::Error> {
    conn.execute(
        "INSERT INTO reasoning_provider_configs
         (id, name, provider_type, endpoint, endpoint_fingerprint, model, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            &config.id,
            &config.name,
            &config.provider_type,
            &config.endpoint,
            &config.endpoint_fingerprint,
            &config.model,
            config.created_at,
            config.updated_at,
        ],
    )?;
    Ok(())
}

pub fn get_provider_config(
    conn: &Connection,
    id: &str,
) -> Result<Option<ReasoningProviderConfigRow>, crate::Error> {
    let sql = format!("SELECT {PROVIDER_CONFIG_COLS} FROM reasoning_provider_configs WHERE id=?1");
    Ok(conn
        .query_row(&sql, params![id], ReasoningProviderConfigRow::from_row)
        .optional()?)
}

pub fn list_provider_configs(
    conn: &Connection,
) -> Result<Vec<ReasoningProviderConfigRow>, crate::Error> {
    let sql = format!(
        "SELECT {PROVIDER_CONFIG_COLS} FROM reasoning_provider_configs
         ORDER BY created_at ASC, id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], ReasoningProviderConfigRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[allow(dead_code)]
pub(crate) fn update_provider_config(
    conn: &Connection,
    config: &ReasoningProviderConfigRow,
) -> Result<bool, crate::Error> {
    let changed = conn.execute(
        "UPDATE reasoning_provider_configs SET name=?2, provider_type=?3, endpoint=?4,
         endpoint_fingerprint=?5, model=?6, updated_at=?7 WHERE id=?1",
        params![
            &config.id,
            &config.name,
            &config.provider_type,
            &config.endpoint,
            &config.endpoint_fingerprint,
            &config.model,
            config.updated_at,
        ],
    )?;
    Ok(changed > 0)
}

#[allow(dead_code)]
pub(crate) fn delete_provider_config(conn: &Connection, id: &str) -> Result<bool, crate::Error> {
    Ok(conn.execute(
        "DELETE FROM reasoning_provider_configs WHERE id=?1",
        params![id],
    )? > 0)
}

pub fn get_cleanup_settings(conn: &Connection) -> Result<Option<CleanupSettingsRow>, crate::Error> {
    Ok(conn
        .query_row(
            "SELECT enabled, selected_provider_config_id, custom_prompt, updated_at
             FROM cleanup_settings WHERE singleton=1",
            [],
            CleanupSettingsRow::from_row,
        )
        .optional()?)
}

#[allow(dead_code)]
pub(crate) fn upsert_cleanup_settings(
    conn: &Connection,
    settings: &CleanupSettingsRow,
) -> Result<(), crate::Error> {
    conn.execute(
        "INSERT INTO cleanup_settings
         (singleton, enabled, selected_provider_config_id, custom_prompt, updated_at)
         VALUES (1, ?1, ?2, ?3, ?4)
         ON CONFLICT(singleton) DO UPDATE SET
           enabled=excluded.enabled,
           selected_provider_config_id=excluded.selected_provider_config_id,
           custom_prompt=excluded.custom_prompt,
           updated_at=excluded.updated_at",
        params![
            settings.enabled as i64,
            &settings.selected_provider_config_id,
            &settings.custom_prompt,
            settings.updated_at,
        ],
    )?;
    Ok(())
}

// ── Dictionary CRUD ──────────────────────────────────────────────────────────

/// 返回当前账户库中的 Dictionary 行数。账户隔离由 per-account 数据库保证。
pub fn count_dictionary_entries(conn: &Connection) -> Result<i64, crate::Error> {
    Ok(
        conn.query_row("SELECT count(*) FROM dictionary_entries", [], |row| {
            row.get(0)
        })?,
    )
}

pub fn get_dictionary_entry(
    conn: &Connection,
    id: &str,
) -> Result<Option<DictionaryEntryRow>, crate::Error> {
    let sql = format!("SELECT {DICTIONARY_COLS} FROM dictionary_entries WHERE id = ?1");
    Ok(conn
        .query_row(&sql, [id], DictionaryEntryRow::from_row)
        .optional()?)
}

pub fn get_dictionary_entry_by_normalized(
    conn: &Connection,
    normalized_term: &str,
) -> Result<Option<DictionaryEntryRow>, crate::Error> {
    let sql =
        format!("SELECT {DICTIONARY_COLS} FROM dictionary_entries WHERE normalized_term = ?1");
    Ok(conn
        .query_row(&sql, [normalized_term], DictionaryEntryRow::from_row)
        .optional()?)
}

fn escape_like(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// 固定排序的 keyset 分页查询。`normalized_query` 已由上层统一规范化；LIKE
/// metacharacter 在此按普通字符转义。调用方通常请求 `limit + 1` 行判断下一页。
pub fn list_dictionary_entries(
    conn: &Connection,
    normalized_query: Option<&str>,
    after: Option<&DictionaryCursor>,
    limit: i64,
) -> Result<Vec<DictionaryEntryRow>, crate::Error> {
    let pattern = normalized_query.map(|query| format!("%{}%", escape_like(query)));
    let (after_rank, after_updated, after_id) = after
        .map(|cursor| {
            (
                Some(cursor.source_rank),
                Some(cursor.updated_at),
                Some(cursor.id.as_str()),
            )
        })
        .unwrap_or((None, None, None));
    let sql = format!(
        "SELECT {DICTIONARY_COLS}
         FROM dictionary_entries
         WHERE (?1 IS NULL OR normalized_term LIKE ?1 ESCAPE '\\')
           AND (
             ?2 IS NULL
             OR CASE source WHEN 'manual' THEN 0 ELSE 1 END > ?2
             OR (CASE source WHEN 'manual' THEN 0 ELSE 1 END = ?2 AND updated_at < ?3)
             OR (CASE source WHEN 'manual' THEN 0 ELSE 1 END = ?2 AND updated_at = ?3 AND id > ?4)
           )
         ORDER BY CASE source WHEN 'manual' THEN 0 ELSE 1 END ASC,
                  updated_at DESC,
                  id ASC
         LIMIT ?5"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map(
            params![pattern, after_rank, after_updated, after_id, limit],
            DictionaryEntryRow::from_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn list_dictionary_entries_for_export(
    conn: &Connection,
) -> Result<Vec<DictionaryEntryRow>, crate::Error> {
    let sql = format!(
        "SELECT {DICTIONARY_COLS} FROM dictionary_entries ORDER BY normalized_term ASC, id ASC"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map([], DictionaryEntryRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// 返回任务级 Dictionary 快照所需的稳定顺序（manual 优先，再按更新时间倒序、ID 正序）。
pub fn list_dictionary_entries_for_snapshot(
    conn: &Connection,
) -> Result<Vec<DictionaryEntryRow>, crate::Error> {
    let sql = format!(
        "SELECT {DICTIONARY_COLS} FROM dictionary_entries
         ORDER BY CASE source WHEN 'manual' THEN 0 ELSE 1 END ASC,
                  updated_at DESC, id ASC"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map([], DictionaryEntryRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn insert_dictionary_entry(
    conn: &Connection,
    row: &DictionaryEntryRow,
) -> Result<(), crate::Error> {
    conn.execute(
        "INSERT INTO dictionary_entries
         (id, term, normalized_term, source, learning_event_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            &row.id,
            &row.term,
            &row.normalized_term,
            &row.source,
            &row.learning_event_id,
            row.created_at,
            row.updated_at,
        ],
    )?;
    Ok(())
}

/// learned 词条由用户确认后提升为 manual，并解除学习事件关联。
pub fn promote_dictionary_entry(
    conn: &Connection,
    id: &str,
    term: &str,
    updated_at: i64,
) -> Result<bool, crate::Error> {
    Ok(conn.execute(
        "UPDATE dictionary_entries
         SET term = ?2, source = 'manual', learning_event_id = NULL, updated_at = ?3
         WHERE id = ?1 AND source = 'learned'",
        params![id, term, updated_at],
    )? > 0)
}

/// 编辑规范写法；调用方先完成规范化冲突检查。任何编辑都确认成 manual。
pub fn update_dictionary_entry(
    conn: &Connection,
    id: &str,
    term: &str,
    normalized_term: &str,
    updated_at: i64,
) -> Result<bool, crate::Error> {
    Ok(conn.execute(
        "UPDATE dictionary_entries
         SET term = ?2, normalized_term = ?3, source = 'manual',
             learning_event_id = NULL, updated_at = ?4
         WHERE id = ?1",
        params![id, term, normalized_term, updated_at],
    )? > 0)
}

pub fn delete_dictionary_entry(conn: &Connection, id: &str) -> Result<bool, crate::Error> {
    Ok(conn.execute("DELETE FROM dictionary_entries WHERE id = ?1", [id])? > 0)
}

pub fn delete_dictionary_entries(conn: &Connection) -> Result<usize, crate::Error> {
    Ok(conn.execute("DELETE FROM dictionary_entries", [])?)
}

/// 精确撤销本批仍为 learned 的新增词条；重复调用返回 0。
pub fn delete_dictionary_learning_event(
    conn: &Connection,
    learning_event_id: &str,
) -> Result<usize, crate::Error> {
    Ok(conn.execute(
        "DELETE FROM dictionary_entries WHERE learning_event_id = ?1 AND source = 'learned'",
        [learning_event_id],
    )?)
}

// ── tokens CRUD ───────────────────────────────────────────────────────────────

/// 插入 token 行。token_hash 须唯一（idx_tokens_hash）。
pub fn insert_token(conn: &Connection, t: &TokenRow) -> Result<(), crate::Error> {
    let scopes = t.scopes.join(" ");
    conn.execute(
        "INSERT INTO tokens
           (id, account_id, name, prefix, token_hash, is_root, scopes, created_at, last_used_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            &t.id,
            &t.account_id,
            &t.name,
            &t.prefix,
            &t.token_hash,
            t.is_root as i64,
            &scopes,
            t.created_at,
            &t.last_used_at,
        ],
    )?;
    Ok(())
}

/// 按 id 取 token。
pub fn get_token(conn: &Connection, id: &str) -> Result<Option<TokenRow>, crate::Error> {
    let sql = format!("SELECT {TOKEN_COLS} FROM tokens WHERE id = ?1");
    let row = conn
        .query_row(&sql, params![id], TokenRow::from_row)
        .optional()?;
    Ok(row)
}

/// 按 token_hash 查 token（鉴权 verify 路径，ST-M2.5 用）。
pub fn get_token_by_hash(
    conn: &Connection,
    token_hash: &str,
) -> Result<Option<TokenRow>, crate::Error> {
    let sql = format!("SELECT {TOKEN_COLS} FROM tokens WHERE token_hash = ?1");
    let row = conn
        .query_row(&sql, params![token_hash], TokenRow::from_row)
        .optional()?;
    Ok(row)
}

/// 列某账户全部 token（按 created_at DESC）。
pub fn list_tokens(conn: &Connection, account_id: &str) -> Result<Vec<TokenRow>, crate::Error> {
    let sql = format!(
        "SELECT {TOKEN_COLS} FROM tokens
         WHERE account_id = ?1
         ORDER BY created_at DESC, id DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![account_id], TokenRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// 删 token。返回是否删到行。
pub fn delete_token(conn: &Connection, id: &str) -> Result<bool, crate::Error> {
    let n = conn.execute("DELETE FROM tokens WHERE id = ?1", params![id])?;
    Ok(n > 0)
}

/// 更新 token 最后使用时间（鉴权命中时调）。
pub fn touch_token_last_used(
    conn: &Connection,
    id: &str,
    last_used_at: i64,
) -> Result<(), crate::Error> {
    conn.execute(
        "UPDATE tokens SET last_used_at = ?1 WHERE id = ?2",
        params![last_used_at, id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_provider_and_settings_crud_stays_inside_storage_crate() {
        let conn = Connection::open_in_memory().unwrap();
        crate::migrate(&conn).unwrap();
        assert_eq!(get_cleanup_settings(&conn).unwrap(), None);

        let mut config = ReasoningProviderConfigRow {
            id: "00000000-0000-4000-8000-000000000001".into(),
            name: "Primary".into(),
            provider_type: "openai".into(),
            endpoint: "https://api.example.invalid/v1".into(),
            endpoint_fingerprint: "fingerprint-a".into(),
            model: "model-a".into(),
            created_at: 100,
            updated_at: 100,
        };
        insert_provider_config(&conn, &config).unwrap();
        assert_eq!(
            get_provider_config(&conn, &config.id).unwrap(),
            Some(config.clone())
        );
        assert_eq!(list_provider_configs(&conn).unwrap(), vec![config.clone()]);

        config.name = "Updated".into();
        config.updated_at = 200;
        assert!(update_provider_config(&conn, &config).unwrap());
        assert_eq!(
            get_provider_config(&conn, &config.id).unwrap(),
            Some(config.clone())
        );

        let settings = CleanupSettingsRow {
            enabled: false,
            selected_provider_config_id: Some(config.id.clone()),
            custom_prompt: Some("custom".into()),
            updated_at: 300,
        };
        upsert_cleanup_settings(&conn, &settings).unwrap();
        assert_eq!(get_cleanup_settings(&conn).unwrap(), Some(settings));

        assert!(delete_provider_config(&conn, &config.id).unwrap());
        assert!(!delete_provider_config(&conn, &config.id).unwrap());
    }
}
