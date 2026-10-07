//! Dictation cleanup 领域能力。marker、输出协议和校验均与 provider/pipeline 解耦。

pub mod correction;
pub mod marker;
pub mod output;
pub mod prompt;
pub mod service;

pub use service::{
    CleanupExecution, CleanupExecutionSnapshot, CleanupPresentationCache, CleanupService,
    CleanupTestError, CleanupTestOutcome, PresentationEntry,
};
