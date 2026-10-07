//! OpenAPI wire DTO 与 epoch↔date-time 换算（ST-M2.6）。
//!
//! 编排层用 epoch 秒（`i64`）记时间（DB 存 INTEGER、利于游标分页）；OpenAPI wire
//! 用 RFC3339 `date-time` 字符串。本模块集中换算，供 handler 组装响应 DTO。
//! proto `*_ms` int64 毫秒 vs OpenAPI wire 秒的换算属转译链路（M3），本模块仅涉
//! 鉴权端点的 epoch 秒 → date-time。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::application::{
    AccountSummaryResult, DictionaryEntry, DictionaryImportPreview, DictionaryImportResult,
    DictionaryMutationResult, IssuedTokenResult, ResolvedFinalText, SessionResult,
    TokenSummaryResult, TranscriptResult, TranscriptSpeakerResult,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddDictionaryEntriesRequest {
    pub terms: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditDictionaryEntryRequest {
    pub term: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportDictionaryCsvRequest {
    pub csv: String,
}

#[derive(Debug, Serialize)]
pub struct DictionaryEntryDto {
    pub id: String,
    pub term: String,
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
}

impl From<DictionaryEntry> for DictionaryEntryDto {
    fn from(entry: DictionaryEntry) -> Self {
        Self {
            id: entry.id,
            term: entry.term,
            source: entry.source,
            created_at: epoch_to_rfc3339(entry.created_at),
            updated_at: epoch_to_rfc3339(entry.updated_at),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DictionaryPageDto {
    pub items: Vec<DictionaryEntryDto>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DictionaryMutationResultDto {
    pub added: Vec<DictionaryEntryDto>,
    pub promoted: Vec<DictionaryEntryDto>,
    pub skipped_count: usize,
}

impl From<DictionaryMutationResult> for DictionaryMutationResultDto {
    fn from(result: DictionaryMutationResult) -> Self {
        Self {
            added: result.added.into_iter().map(Into::into).collect(),
            promoted: result.promoted.into_iter().map(Into::into).collect(),
            skipped_count: result.skipped_count,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DictionaryImportPreviewDto {
    pub parsed_count: usize,
    pub valid_count: usize,
    pub added_count: usize,
    pub promoted_count: usize,
    pub skipped_count: usize,
}

impl From<DictionaryImportPreview> for DictionaryImportPreviewDto {
    fn from(preview: DictionaryImportPreview) -> Self {
        Self {
            parsed_count: preview.parsed_count,
            valid_count: preview.valid_count,
            added_count: preview.added_count,
            promoted_count: preview.promoted_count,
            skipped_count: preview.skipped_count,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DictionaryImportResultDto {
    pub parsed_count: usize,
    pub added: Vec<DictionaryEntryDto>,
    pub promoted: Vec<DictionaryEntryDto>,
    pub skipped_count: usize,
}

impl From<DictionaryImportResult> for DictionaryImportResultDto {
    fn from(result: DictionaryImportResult) -> Self {
        Self {
            parsed_count: result.parsed_count,
            added: result.mutation.added.into_iter().map(Into::into).collect(),
            promoted: result
                .mutation
                .promoted
                .into_iter()
                .map(Into::into)
                .collect(),
            skipped_count: result.mutation.skipped_count,
        }
    }
}

/// epoch 秒 → RFC3339 `date-time` 字符串（`2026-08-11T12:34:56+00:00`）。
/// `secs` 为负或越界时回退 `DateTime::default()`（unix epoch），不致序列化失败。
pub fn epoch_to_rfc3339(secs: i64) -> String {
    DateTime::<Utc>::from_timestamp(secs, 0)
        .unwrap_or_default()
        .to_rfc3339()
}

/// `Token` schema（`GET /tokens` 返回）。`token_hash` 不进 wire（敏感）。
/// `last_used_at` 序列化为 `null`（保持响应 shape 稳定，利前端类型生成）。
#[derive(Debug, Serialize)]
pub struct TokenDto {
    pub id: String,
    pub account_id: String,
    pub name: String,
    pub prefix: String,
    pub is_root: bool,
    pub scopes: Vec<String>,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

impl TokenDto {
    /// 从 DB 行转换。`token_hash` 丢弃。
    pub fn from_row(r: TokenSummaryResult) -> Self {
        Self {
            id: r.id,
            account_id: r.account_id,
            name: r.name,
            prefix: r.prefix,
            is_root: r.is_root,
            scopes: r.scopes,
            created_at: epoch_to_rfc3339(r.created_at),
            last_used_at: r.last_used_at.map(epoch_to_rfc3339),
        }
    }
}

/// `TokenCreated` schema（setup/unlock/create_token 返回）= `Token` + `secret`。
/// `secret` = 完整 `ss_live_xxx` bearer，仅创建时返回一次。
#[derive(Debug, Serialize)]
pub struct TokenCreatedDto {
    pub id: String,
    pub account_id: String,
    pub name: String,
    pub prefix: String,
    pub is_root: bool,
    pub scopes: Vec<String>,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub secret: String,
}

impl TokenCreatedDto {
    /// 从已签发 token 转换。`last_used_at = None`（新建 token 未使用过）。
    pub fn from_issued(t: IssuedTokenResult) -> Self {
        Self {
            id: t.id,
            account_id: t.account_id,
            name: t.name,
            prefix: t.prefix,
            is_root: t.is_root,
            scopes: t.scopes,
            created_at: epoch_to_rfc3339(t.created_at),
            last_used_at: None,
            secret: t.secret,
        }
    }
}

/// `AccountSummary` schema（`GET /accounts` 返回）。
#[derive(Debug, Serialize)]
pub struct AccountSummaryDto {
    pub id: String,
    pub username: String,
    pub created_at: String,
    pub is_active: bool,
}

impl AccountSummaryDto {
    pub fn from_summary(a: AccountSummaryResult) -> Self {
        Self {
            id: a.id,
            username: a.username,
            created_at: epoch_to_rfc3339(a.created_at),
            is_active: a.is_active,
        }
    }
}

/// `Speaker` schema（`Session.speakers[]` 项）。
#[derive(Debug, Serialize)]
pub struct SpeakerDto {
    pub id: String,
    pub label: String,
}

impl From<&TranscriptSpeakerResult> for SpeakerDto {
    fn from(s: &TranscriptSpeakerResult) -> Self {
        Self {
            id: s.id.clone(),
            label: s.label.clone(),
        }
    }
}

/// 转写时间单元 schema（OpenAPI wire 时间单位为秒）。
/// 由 proto 毫秒 int64 /1000 换算（daemon 在 proto 边界换算）。
#[derive(Debug, Serialize)]
pub struct TranscriptUnitDto {
    pub speaker: String,
    pub start: Option<f64>,
    pub end: Option<f64>,
    pub text: String,
}

/// `Transcript` schema（`Session.transcript`，status=completed 时返回）。
#[derive(Debug, Serialize)]
pub struct TranscriptDto {
    pub full_text: String,
    pub units: Vec<TranscriptUnitDto>,
}

impl TranscriptDto {
    /// 由 proto `TranscriptFile` 转 wire DTO：units 时间 ms→秒、丢弃 confidence。
    pub(crate) fn from_result(t: &TranscriptResult) -> Self {
        let units = t
            .units
            .iter()
            .map(|s| TranscriptUnitDto {
                speaker: s.speaker.clone(),
                start: s.start_ms.map(|v| v as f64 / 1000.0),
                end: s.end_ms.map(|v| v as f64 / 1000.0),
                text: s.text.clone(),
            })
            .collect();
        Self {
            full_text: t.full_text.clone(),
            units,
        }
    }
}

/// 内部注入计划（`GET /sessions/{id}/injection-plan`，仅 root、进程内一次性）。
/// `plain` = 合成后的普通全文（含 `（剪贴板…）` 标记）；`html` = 同位置的安全 HTML
///（ASR 文本与系统标记 HTML 转义 + RichText 事件经 ammonia 过滤的富文本片段；空转写
/// 时为空串）。原生层 `html` 非空时同时写 text/html+text/plain，否则降级为纯文本注入。
/// 不进 `SessionView`，普通 GET 不暴露。
/// 自定义 Debug 省略粘贴文本与 learning_ticket，避免调试输出泄漏用户内容或票据。
#[derive(Serialize)]
pub struct InjectionPlanDto {
    pub plain: String,
    pub html: String,
    pub learning_ticket: String,
}

impl std::fmt::Debug for InjectionPlanDto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InjectionPlanDto")
            .field("plain", &format_args!("<{} bytes>", self.plain.len()))
            .field("html", &format_args!("<{} bytes>", self.html.len()))
            .field("learning_ticket", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LearningEventRequest {
    pub mode: String,
    pub ticket: Option<String>,
    pub candidates: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct LearningEventResponse {
    pub learning_event_id: Option<String>,
    pub added_terms: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct UndoLearningEventResponse {
    pub undone_count: usize,
}

/// 仅供 root-only Tauri 内部链路使用；绝不进入普通会话 DTO。
#[derive(Debug, Serialize)]
pub struct ResolvedResourceDto {
    pub path: String,
    pub kind: String,
    pub size: u64,
    pub modified_unix_ms: Option<u128>,
    pub device: Option<u64>,
    pub inode: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct ResolvedLinkDto {
    pub url: String,
}

/// `Session` schema（`GET /sessions/{id}` 返回，ST-M3.7）。`transcript` 仅
/// status=completed 时拼装（`read_transcript` 解密）；`failure_reason` 仅 failed 时有值。
#[derive(Debug, Serialize)]
pub struct SessionView {
    pub id: String,
    pub created_at: String,
    pub source: String,
    pub language: String,
    pub duration_sec: f64,
    pub status: String,
    pub model: String,
    pub input_device: Option<String>,
    pub file_name: Option<String>,
    /// 创建时是否提交了剪贴板上下文；true 时 retry 复用已持久化的 context.pb.enc。
    pub context_present: bool,
    pub speakers: Vec<SpeakerDto>,
    pub transcript: Option<TranscriptDto>,
    pub failure_reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SessionSummaryDto {
    pub id: String,
    pub created_at: String,
    pub source: String,
    pub language: String,
    pub duration_sec: f64,
    pub status: String,
    pub model: String,
    pub preview: String,
    pub cleanup_status: String,
    pub text_source: Option<&'static str>,
    pub cleanup_error_code: Option<String>,
}
impl SessionSummaryDto {
    pub(crate) fn from_row(
        row: &SessionResult,
        preview: String,
        resolved: Option<&ResolvedFinalText>,
        include_cleanup_diagnostics: bool,
    ) -> Self {
        Self {
            id: row.id.clone(),
            created_at: epoch_to_rfc3339(row.created_at),
            source: row.source.clone(),
            language: row.language.clone(),
            duration_sec: row.duration_sec,
            status: row.status.clone(),
            model: row.model.clone(),
            preview,
            cleanup_status: row.cleanup_status.clone(),
            text_source: resolved.map(|value| value.source.as_str()),
            cleanup_error_code: include_cleanup_diagnostics
                .then(|| resolved.and_then(|value| value.cleanup_error_code.clone()))
                .flatten(),
        }
    }
}
#[derive(Debug, Serialize)]
pub struct SessionListDto {
    pub items: Vec<SessionSummaryDto>,
    pub next_cursor: Option<String>,
}

impl SessionView {
    /// 由 DB 行 + 可选 transcript/speakers（completed 时由 `read_transcript` 解出）拼装。
    pub(crate) fn from_row(
        row: &SessionResult,
        transcript: Option<TranscriptDto>,
        speakers: Vec<SpeakerDto>,
    ) -> Self {
        Self {
            id: row.id.clone(),
            created_at: epoch_to_rfc3339(row.created_at),
            source: row.source.clone(),
            language: row.language.clone(),
            duration_sec: row.duration_sec,
            status: row.status.clone(),
            model: row.model.clone(),
            input_device: row.input_device.clone(),
            file_name: row.file_name.clone(),
            context_present: row.context_present,
            speakers,
            transcript,
            failure_reason: row.failure_reason.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_to_rfc3339_basic() {
        let s = epoch_to_rfc3339(1_700_000_000);
        assert!(s.starts_with("2023-"), "1700000000 ≈ 2023-11-14, 实际 {s}");
        assert!(s.ends_with("+00:00") || s.ends_with("Z"));
    }

    #[test]
    fn token_dto_from_row_drops_hash() {
        let r = TokenSummaryResult {
            id: "id".into(),
            account_id: "acct".into(),
            name: "n".into(),
            prefix: "ss_live_ab".into(),
            is_root: true,
            scopes: vec!["sessions:read".into()],
            created_at: 1_700_000_000,
            last_used_at: Some(1_700_000_100),
        };
        let dto = TokenDto::from_row(r);
        assert_eq!(dto.id, "id");
        assert!(dto.is_root);
        assert!(dto.created_at.starts_with("2023-"));
        assert!(dto.last_used_at.as_ref().unwrap().starts_with("2023-"));
        // token_hash 不进 wire（serde 结构无此字段，编译期保证）。
    }

    #[test]
    fn token_created_dto_carries_secret() {
        let t = IssuedTokenResult {
            id: "id".into(),
            account_id: "acct".into(),
            name: "root".into(),
            prefix: "ss_live_ab".into(),
            secret: "ss_live_xxx".into(),
            is_root: true,
            scopes: vec!["sessions:read".into()],
            created_at: 1_700_000_000,
        };
        let dto = TokenCreatedDto::from_issued(t);
        assert_eq!(dto.secret, "ss_live_xxx");
        assert!(dto.is_root);
        assert!(dto.last_used_at.is_none());
    }
}
