//! 应用层错误；不包含 HTTP status、Axum response 或操作系统错误对象。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApplicationError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("wrong password")]
    WrongPassword,
    #[error("insufficient scope: {0}")]
    InsufficientScope(String),
    #[error("scope not grantable: {0}")]
    ScopeNotGrantable(String),
    #[error("cross-account access denied: {0}")]
    CrossAccount(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("context not found: {0}")]
    ContextNotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("unprocessable entity: {0}")]
    UnprocessableEntity(String),
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),
    #[error("resource unavailable: {0}")]
    ResourceUnavailable(String),
    #[error("resource unsafe: {0}")]
    ResourceUnsafe(String),
    #[error("unsupported scheme: {0}")]
    UnsupportedScheme(String),
    #[error("already initialized")]
    AlreadyInitialized,
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("locked: {0}")]
    Locked(String),
    #[error("cleanup operation busy")]
    CleanupBusy,
    #[error("cleanup operation failed: {0}")]
    CleanupFailure(&'static str),
    #[error("internal application failure: {0}")]
    Internal(String),
}

impl From<crate::account::AccountError> for ApplicationError {
    fn from(error: crate::account::AccountError) -> Self {
        use crate::account::AccountError;
        match error {
            AccountError::InvalidToken(_) => Self::Unauthorized,
            AccountError::AccountNotFound(id) => Self::NotFound(format!("account not found: {id}")),
            AccountError::TokenNotFound => Self::NotFound("token not found".into()),
            AccountError::WrongPassword => Self::WrongPassword,
            AccountError::NotUnlocked => Self::Locked("account not unlocked".into()),
            AccountError::KeychainMissing => {
                Self::Locked("active account DEK missing; unlock required".into())
            }
            AccountError::CrossAccount(message) => Self::CrossAccount(message),
            AccountError::InsufficientScope(message) => Self::InsufficientScope(message),
            AccountError::ScopeNotGrantable(message) => Self::ScopeNotGrantable(message),
            AccountError::AlreadyInitialized => Self::AlreadyInitialized,
            AccountError::ActiveAccountDeletion => {
                Self::Conflict("cannot delete active account; switch away first".into())
            }
            AccountError::ActiveLease => {
                Self::Conflict("account has active background work".into())
            }
            AccountError::Io(error) => Self::Internal(format!("io: {error}")),
            AccountError::Serde(error) => Self::Internal(format!("serde: {error}")),
            AccountError::Crypto(error) => Self::Internal(format!("crypto: {error}")),
            AccountError::Storage(seasnail_storage::Error::InvalidMutation(message)) => {
                Self::InvalidInput(message)
            }
            AccountError::Storage(error) => Self::Internal(format!("storage: {error}")),
            AccountError::Proto(error) => Self::Internal(format!("proto: {error}")),
            AccountError::ContextIntegrity(error) => {
                Self::Internal(format!("context integrity: {error}"))
            }
            AccountError::CleanupIntegrity(error) => {
                Self::Internal(format!("cleanup integrity: {error}"))
            }
        }
    }
}

impl From<seasnail_storage::Error> for ApplicationError {
    fn from(error: seasnail_storage::Error) -> Self {
        match error {
            seasnail_storage::Error::InvalidMutation(message) => Self::InvalidInput(message),
            other => Self::Internal(format!("storage: {other}")),
        }
    }
}
