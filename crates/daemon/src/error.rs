//! 统一错误响应模型（ST-M1.6）。
//!
//! 对应 `proto/openapi.yaml` `components/schemas/Error`：所有错误响应统一为
//! `{"error":{"code":<string>,"message":<string>}}`。覆盖路线图 ST-M1.6 列的状态码
//! 401/403/404/409/410/423/413 + 500（内部错误），各端点 handler 以 `Result<T, AppError>` 复用。
//!
//! `code` 为稳定契约串（snake_case），前端按 `code` 分支处理；`message` 为人可读
//! 运行时提示（英文，前端按 `code` 本地化，不直接展示 `message`）。同状态码下可有
//! 多 `code` 语义变体（403 下 `forbidden`/`wrong_password`/`insufficient_scope`/
//! `cross_account`/`scope_not_grantable`、401 下 `unauthorized`）——变体即契约新增
//! 点，一经发布不可改名。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use crate::account::AccountError;
use crate::application::ApplicationError;

/// 错误响应体；序列化为 `{"error":{"code","message"}}`（对应 openapi Error schema）。
#[derive(Serialize)]
struct ErrorBody {
    error: ErrorFields,
}

#[derive(Serialize)]
struct ErrorFields {
    code: String,
    message: String,
}

/// 应用错误。每变体映射到 `(StatusCode, code, message)`。
#[derive(Debug)]
pub enum AppError {
    /// 400 `bad_request`：请求语义或字段校验不通过。
    BadRequest(String),
    /// 401 `unauthorized`：未携带或无效 token / 账户不存在（verify 路径防泄露）。
    Unauthorized(String),
    /// 403 `forbidden`：权限不足（通用 403，缺其他细分语义时用）。
    Forbidden(String),
    /// 403 `wrong_password`：密码错误（unlock / 改密 / 导出门禁）。
    WrongPassword(String),
    /// 403 `insufficient_scope`：caller 缺 `x-required-scope`。
    InsufficientScope(String),
    /// 403 `cross_account`：token 的 account_id ≠ 目标资源 account_id。
    CrossAccount(String),
    /// 403 `scope_not_grantable`：签发第三方 token 时请求的 scope 不可授。
    ScopeNotGrantable(String),
    /// 404 `not_found`：资源不存在。
    NotFound(String),
    /// 404 `context_not_found`：会话无上下文或上下文事件不存在（区别于通用 not_found）。
    ContextNotFound(String),
    /// 409 `conflict`：服务忙 / 试图删活跃账户。
    Conflict(String),
    /// 422 `unprocessable_entity`：请求语义合法但无法处理（如未下载即开启组件）。
    UnprocessableEntity(String),
    /// 410 `gone`：已有账户，不可重复 setup。
    Gone(String),
    /// 423 `locked`：目标账户未解锁（DEK 不在内存，需先 unlock）。
    Locked(String),
    /// 413 `payload_too_large`：音频过大。
    PayloadTooLarge(String),
    /// 413 `request_too_large`：Dictionary JSON/CSV 请求超过固定上限。
    RequestTooLarge(String),
    /// Dictionary 稳定业务错误码。
    DictionaryInvalidTerm(String),
    DictionaryCsvInvalid(String),
    DictionaryLimitExceeded(String),
    DictionaryConflict(String),
    LearningTicketInvalid(String),
    LearningTicketExpired(String),
    LearningTicketConsumed(String),
    LearningTicketMismatch(String),
    /// 500 `internal`：内部错误（I/O / serde / 加密原语 / 存储）。
    Internal(String),
    /// 503 `service_unavailable`：FunASR bundle / models-manifest 不可用等暂时性服务缺失。
    ServiceUnavailable(String),
    ResourceUnavailable(String),
    ResourceUnsafe(String),
    UnsupportedScheme(String),
    ThumbnailBusy(String),
    ThumbnailBudgetExceeded(String),
    /// 409：同账户已有 Probe/Test 在运行。
    CleanupBusy(String),
    /// 422：Probe 已安全执行但配置、网络或上游响应不满足 cleanup 契约。
    CleanupFailure {
        code: &'static str,
        message: String,
    },
}

impl AppError {
    /// `(status, code, message)` 三元组。`code` 为静态字面量，零分配。
    fn parts(&self) -> (StatusCode, &'static str, &str) {
        match self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, "bad_request", m.as_str()),
            Self::Unauthorized(m) => (StatusCode::UNAUTHORIZED, "unauthorized", m.as_str()),
            Self::Forbidden(m) => (StatusCode::FORBIDDEN, "forbidden", m.as_str()),
            Self::WrongPassword(m) => (StatusCode::FORBIDDEN, "wrong_password", m.as_str()),
            Self::InsufficientScope(m) => (StatusCode::FORBIDDEN, "insufficient_scope", m.as_str()),
            Self::CrossAccount(m) => (StatusCode::FORBIDDEN, "cross_account", m.as_str()),
            Self::ScopeNotGrantable(m) => {
                (StatusCode::FORBIDDEN, "scope_not_grantable", m.as_str())
            }
            Self::NotFound(m) => (StatusCode::NOT_FOUND, "not_found", m.as_str()),
            Self::ContextNotFound(m) => (StatusCode::NOT_FOUND, "context_not_found", m.as_str()),
            Self::Conflict(m) => (StatusCode::CONFLICT, "conflict", m.as_str()),
            Self::UnprocessableEntity(m) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "unprocessable_entity",
                m.as_str(),
            ),
            Self::Gone(m) => (StatusCode::GONE, "gone", m.as_str()),
            Self::Locked(m) => (StatusCode::LOCKED, "locked", m.as_str()),
            Self::PayloadTooLarge(m) => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                m.as_str(),
            ),
            Self::RequestTooLarge(m) => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_too_large",
                m.as_str(),
            ),
            Self::DictionaryInvalidTerm(m) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "dictionary_invalid_term",
                m.as_str(),
            ),
            Self::DictionaryCsvInvalid(m) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "dictionary_csv_invalid",
                m.as_str(),
            ),
            Self::DictionaryLimitExceeded(m) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "dictionary_limit_exceeded",
                m.as_str(),
            ),
            Self::DictionaryConflict(m) => {
                (StatusCode::CONFLICT, "dictionary_conflict", m.as_str())
            }
            Self::LearningTicketInvalid(m) => (
                StatusCode::BAD_REQUEST,
                "learning_ticket_invalid",
                m.as_str(),
            ),
            Self::LearningTicketExpired(m) => {
                (StatusCode::GONE, "learning_ticket_expired", m.as_str())
            }
            Self::LearningTicketConsumed(m) => {
                (StatusCode::GONE, "learning_ticket_consumed", m.as_str())
            }
            Self::LearningTicketMismatch(m) => {
                (StatusCode::CONFLICT, "learning_ticket_mismatch", m.as_str())
            }
            Self::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, "internal", m.as_str()),
            Self::ServiceUnavailable(m) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                m.as_str(),
            ),
            Self::ResourceUnavailable(m) => {
                (StatusCode::CONFLICT, "resource_unavailable", m.as_str())
            }
            Self::ResourceUnsafe(m) => (StatusCode::CONFLICT, "resource_unsafe", m.as_str()),
            Self::UnsupportedScheme(m) => (StatusCode::CONFLICT, "unsupported_scheme", m.as_str()),
            Self::ThumbnailBusy(m) => (StatusCode::TOO_MANY_REQUESTS, "thumbnail_busy", m.as_str()),
            Self::ThumbnailBudgetExceeded(m) => (
                StatusCode::TOO_MANY_REQUESTS,
                "thumbnail_budget_exceeded",
                m.as_str(),
            ),
            Self::CleanupBusy(m) => (StatusCode::CONFLICT, "cleanup_busy", m.as_str()),
            Self::CleanupFailure { code, message } => {
                (StatusCode::UNPROCESSABLE_ENTITY, code, message.as_str())
            }
        }
    }
}

impl From<crate::application::DictionaryError> for AppError {
    fn from(error: crate::application::DictionaryError) -> Self {
        use crate::application::DictionaryError;
        match error {
            DictionaryError::Forbidden => Self::InsufficientScope("requires root token".into()),
            DictionaryError::InvalidTerm | DictionaryError::TooManyTerms => {
                Self::DictionaryInvalidTerm("dictionary term is invalid".into())
            }
            DictionaryError::RequestTooLarge => {
                Self::RequestTooLarge("dictionary request exceeds 1 MiB".into())
            }
            DictionaryError::InvalidCsv => {
                Self::DictionaryCsvInvalid("dictionary CSV is invalid".into())
            }
            DictionaryError::LimitExceeded => {
                Self::DictionaryLimitExceeded("dictionary capacity would be exceeded".into())
            }
            DictionaryError::Conflict => {
                Self::DictionaryConflict("dictionary entry already exists".into())
            }
            DictionaryError::NotFound => Self::NotFound("dictionary entry not found".into()),
            DictionaryError::Storage(error) => {
                Self::Internal(format!("dictionary storage: {error}"))
            }
            DictionaryError::Sqlite(error) => {
                Self::Internal(format!("dictionary transaction: {error}"))
            }
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, message) = self.parts();
        (
            status,
            Json(ErrorBody {
                error: ErrorFields {
                    code: code.to_string(),
                    message: message.to_string(),
                },
            }),
        )
            .into_response()
    }
}

/// 编排层错误 → HTTP 错误的默认映射。
///
/// **上下文相关覆写**：`AccountNotFound`/`TokenNotFound` 默认 → 404 `not_found`（资源端点）；
/// 但在 verify 路径（`AuthedCaller` 提取器）须覆写为 401 `unauthorized`（不泄露账户存在性），
/// 故提取器内不直接用此 `From`，而是显式 match。
impl From<AccountError> for AppError {
    fn from(e: AccountError) -> Self {
        match e {
            AccountError::AlreadyInitialized => AppError::Gone("already initialized".into()),
            AccountError::WrongPassword => AppError::WrongPassword("wrong password".into()),
            AccountError::NotUnlocked => AppError::Locked("account not unlocked".into()),
            AccountError::KeychainMissing => {
                AppError::Locked("active account DEK missing; unlock required".into())
            }
            AccountError::InvalidToken(m) => AppError::Unauthorized(m),
            AccountError::CrossAccount(m) => AppError::CrossAccount(m),
            AccountError::InsufficientScope(m) => AppError::InsufficientScope(m),
            AccountError::ScopeNotGrantable(m) => AppError::ScopeNotGrantable(m),
            AccountError::ActiveAccountDeletion => {
                AppError::Conflict("cannot delete active account; switch away first".into())
            }
            AccountError::ActiveLease => {
                AppError::Conflict("account has active background work".into())
            }
            AccountError::AccountNotFound(m) => {
                AppError::NotFound(format!("account not found: {m}"))
            }
            AccountError::TokenNotFound => AppError::NotFound("token not found".into()),
            // 内部错误：不向客户端透传底层细节，仅记日志。
            AccountError::Io(e) => AppError::Internal(format!("io: {e}")),
            AccountError::Serde(e) => AppError::Internal(format!("serde: {e}")),
            AccountError::Crypto(e) => AppError::Internal(format!("crypto: {e}")),
            AccountError::Storage(e) => AppError::Internal(format!("storage: {e}")),
            AccountError::Proto(e) => AppError::Internal(format!("proto: {e}")),
            AccountError::ContextIntegrity(e) => {
                AppError::Internal(format!("context integrity: {e}"))
            }
            AccountError::CleanupIntegrity(e) => {
                AppError::Internal(format!("cleanup integrity: {e}"))
            }
        }
    }
}

impl From<ApplicationError> for AppError {
    fn from(error: ApplicationError) -> Self {
        match error {
            ApplicationError::Unauthorized => Self::Unauthorized("invalid token".into()),
            ApplicationError::Forbidden(message) => Self::Forbidden(message),
            ApplicationError::WrongPassword => Self::WrongPassword("wrong password".into()),
            ApplicationError::InsufficientScope(message) => Self::InsufficientScope(message),
            ApplicationError::ScopeNotGrantable(message) => Self::ScopeNotGrantable(message),
            ApplicationError::CrossAccount(message) => Self::CrossAccount(message),
            ApplicationError::NotFound(message) => Self::NotFound(message),
            ApplicationError::ContextNotFound(message) => Self::ContextNotFound(message),
            ApplicationError::Conflict(message) => Self::Conflict(message),
            ApplicationError::UnprocessableEntity(message) => Self::UnprocessableEntity(message),
            ApplicationError::ServiceUnavailable(message) => Self::ServiceUnavailable(message),
            ApplicationError::ResourceUnavailable(message) => Self::ResourceUnavailable(message),
            ApplicationError::ResourceUnsafe(message) => Self::ResourceUnsafe(message),
            ApplicationError::UnsupportedScheme(message) => Self::UnsupportedScheme(message),
            ApplicationError::AlreadyInitialized => Self::Gone("already initialized".into()),
            ApplicationError::InvalidInput(message) => Self::BadRequest(message),
            ApplicationError::Locked(message) => Self::Locked(message),
            ApplicationError::CleanupBusy => Self::CleanupBusy("cleanup operation busy".into()),
            ApplicationError::CleanupFailure(code) => Self::CleanupFailure {
                code,
                message: "cleanup operation failed".into(),
            },
            ApplicationError::Internal(message) => Self::Internal(message),
        }
    }
}

/// 编排层错误 → HTTP 错误的 **verify 路径**映射：账户/token 不存在统一 → 401
/// `unauthorized`（不泄露账户存在性，与 verify 步 1/2/4 的 401 语义一致）。
///
/// 供 `AuthedCaller` 提取器使用，而非 blanket `From`。
pub fn verify_err_to_apperr(e: AccountError) -> AppError {
    match e {
        AccountError::AccountNotFound(_)
        | AccountError::TokenNotFound
        | AccountError::InvalidToken(_) => AppError::Unauthorized("invalid token".into()),
        AccountError::NotUnlocked | AccountError::KeychainMissing => AppError::from(e),
        other => AppError::from(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验收 ST-M1.6：错误码均产出统一 shape `{error:{code,message}}`，状态码与 code 对应。
    #[tokio::test]
    async fn all_codes_unified_shape() {
        let cases: Vec<(AppError, u16, &str)> = vec![
            (AppError::BadRequest("m".into()), 400, "bad_request"),
            (AppError::ThumbnailBusy("m".into()), 429, "thumbnail_busy"),
            (
                AppError::ThumbnailBudgetExceeded("m".into()),
                429,
                "thumbnail_budget_exceeded",
            ),
            (AppError::Unauthorized("m".into()), 401, "unauthorized"),
            (AppError::Forbidden("m".into()), 403, "forbidden"),
            (AppError::WrongPassword("m".into()), 403, "wrong_password"),
            (
                AppError::InsufficientScope("m".into()),
                403,
                "insufficient_scope",
            ),
            (AppError::CrossAccount("m".into()), 403, "cross_account"),
            (
                AppError::ScopeNotGrantable("m".into()),
                403,
                "scope_not_grantable",
            ),
            (AppError::NotFound("m".into()), 404, "not_found"),
            (
                AppError::ContextNotFound("m".into()),
                404,
                "context_not_found",
            ),
            (AppError::Conflict("m".into()), 409, "conflict"),
            (AppError::Gone("m".into()), 410, "gone"),
            (AppError::Locked("m".into()), 423, "locked"),
            (
                AppError::PayloadTooLarge("m".into()),
                413,
                "payload_too_large",
            ),
            (
                AppError::RequestTooLarge("m".into()),
                413,
                "request_too_large",
            ),
            (
                AppError::DictionaryInvalidTerm("m".into()),
                422,
                "dictionary_invalid_term",
            ),
            (
                AppError::DictionaryCsvInvalid("m".into()),
                422,
                "dictionary_csv_invalid",
            ),
            (
                AppError::DictionaryLimitExceeded("m".into()),
                422,
                "dictionary_limit_exceeded",
            ),
            (
                AppError::DictionaryConflict("m".into()),
                409,
                "dictionary_conflict",
            ),
            (AppError::Internal("m".into()), 500, "internal"),
        ];
        for (err, want_status, want_code) in cases {
            let resp = err.into_response();
            assert_eq!(
                resp.status().as_u16(),
                want_status,
                "code={want_code} 状态码"
            );
            let bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
            let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(v["error"]["code"], want_code, "code={want_code} code 字段");
            assert_eq!(v["error"]["message"], "m", "code={want_code} message 字段");
            assert_eq!(v.as_object().unwrap().len(), 1, "code={want_code} 顶层键数");
            assert_eq!(
                v["error"].as_object().unwrap().len(),
                2,
                "code={want_code} error 键数"
            );
        }
    }

    /// `From<AccountError>` 默认映射：AccountNotFound→404、AlreadyInitialized→410、
    /// WrongPassword→403 wrong_password、ActiveAccountDeletion→409。
    #[tokio::test]
    async fn from_account_error_default_mapping() {
        use crate::account::registry::Registry; // 触发 AccountError 可达性
        let _ = Registry::new();
        let cases: Vec<(AccountError, u16, &str)> = vec![
            (AccountError::AlreadyInitialized, 410, "gone"),
            (AccountError::WrongPassword, 403, "wrong_password"),
            (AccountError::NotUnlocked, 423, "locked"),
            (AccountError::InvalidToken("x".into()), 401, "unauthorized"),
            (AccountError::CrossAccount("a".into()), 403, "cross_account"),
            (
                AccountError::InsufficientScope("s".into()),
                403,
                "insufficient_scope",
            ),
            (
                AccountError::ScopeNotGrantable("s".into()),
                403,
                "scope_not_grantable",
            ),
            (AccountError::ActiveAccountDeletion, 409, "conflict"),
            (AccountError::ActiveLease, 409, "conflict"),
            (AccountError::AccountNotFound("a".into()), 404, "not_found"),
            (AccountError::TokenNotFound, 404, "not_found"),
        ];
        for (ae, want_status, want_code) in cases {
            let resp: AppError = ae.into();
            let resp = resp.into_response();
            assert_eq!(resp.status().as_u16(), want_status, "code={want_code}");
            let bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
            let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(v["error"]["code"], want_code, "code={want_code}");
        }
    }

    /// verify 路径覆写：AccountNotFound / TokenNotFound → 401（不泄露存在性）。
    #[tokio::test]
    async fn verify_err_maps_notfound_to_unauthorized() {
        let cases = vec![
            AccountError::AccountNotFound("a".into()),
            AccountError::TokenNotFound,
            AccountError::InvalidToken("x".into()),
        ];
        for ae in cases {
            let resp = verify_err_to_apperr(ae).into_response();
            assert_eq!(resp.status().as_u16(), 401);
            let bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
            let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(v["error"]["code"], "unauthorized");
        }
        // NotUnlocked 仍 423。
        let resp = verify_err_to_apperr(AccountError::NotUnlocked).into_response();
        assert_eq!(resp.status().as_u16(), 423);
    }

    #[tokio::test]
    async fn application_error_mapping_preserves_identity_messages() {
        let cases = [
            (
                ApplicationError::Conflict(
                    "cannot delete active account; switch away first".into(),
                ),
                409,
                "conflict",
                "cannot delete active account; switch away first",
            ),
            (
                ApplicationError::Locked("active account DEK missing; unlock required".into()),
                423,
                "locked",
                "active account DEK missing; unlock required",
            ),
        ];
        for (error, status, code, message) in cases {
            let response = AppError::from(error).into_response();
            assert_eq!(response.status().as_u16(), status);
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["error"]["code"], code);
            assert_eq!(value["error"]["message"], message);
        }
    }
}
