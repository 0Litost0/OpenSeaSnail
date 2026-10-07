//! FinalTextResolver（M6.1）。
//!
//! 这里是 cleanup/raw 正文选择的唯一读路径。它只读取 session 行和加密事实文件，
//! 不在读取过程中修改 DB；cleanup artifact 任一完整性问题都 fail closed 到 raw。

use seasnail_proto::seasnail::v1::{CleanupFile, CleanupOutcome, TranscriptFile};

use crate::account::Storage;

use super::{ApplicationError, SessionResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextSource {
    Raw,
    Cleanup,
}

impl TextSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Cleanup => "cleanup",
        }
    }
}

#[derive(Debug)]
pub struct ResolvedFinalText {
    pub text: String,
    pub source: TextSource,
    pub cleanup_status: String,
    pub cleanup_error_code: Option<String>,
    /// 仅在 cleanup 成功且 artifact 完整时返回，用于导出 cleanup.json。
    pub cleanup_artifact: Option<CleanupFile>,
    pub raw_transcript: TranscriptFile,
}

/// 统一选择一个已完成 session 的最终正文。
pub fn resolve(
    storage: &Storage,
    session: &SessionResult,
) -> Result<Option<ResolvedFinalText>, ApplicationError> {
    if session.status != "completed" {
        return Ok(None);
    }
    let transcript_path = session
        .transcript_path
        .as_deref()
        .ok_or_else(|| ApplicationError::NotFound("session has no transcript".into()))?;
    let transcript = storage
        .read_transcript(transcript_path)
        .map_err(|_| ApplicationError::Conflict("transcript unavailable".into()))?;

    if session.cleanup_status == "succeeded" {
        let expected_cleanup_path =
            Storage::canonical_cleanup_path(&session.id, session.created_at);
        let cleanup = session
            .cleanup_path
            .as_deref()
            .filter(|path| *path == expected_cleanup_path.as_str())
            .and_then(|path| storage.read_cleanup(path).ok())
            .filter(|artifact| {
                CleanupOutcome::try_from(artifact.outcome)
                    .is_ok_and(|outcome| outcome == CleanupOutcome::Succeeded)
            });
        if let Some(cleanup) = cleanup {
            return Ok(Some(ResolvedFinalText {
                text: cleanup.cleaned_text.clone(),
                source: TextSource::Cleanup,
                cleanup_status: session.cleanup_status.clone(),
                cleanup_error_code: None,
                cleanup_artifact: Some(cleanup),
                raw_transcript: transcript,
            }));
        }
        return Ok(Some(ResolvedFinalText {
            text: transcript.full_text.clone(),
            source: TextSource::Raw,
            cleanup_status: session.cleanup_status.clone(),
            cleanup_error_code: Some("cleanup_artifact_invalid".into()),
            cleanup_artifact: None,
            raw_transcript: transcript,
        }));
    }

    Ok(Some(ResolvedFinalText {
        text: transcript.full_text.clone(),
        source: TextSource::Raw,
        cleanup_status: session.cleanup_status.clone(),
        cleanup_error_code: session.cleanup_error_code.clone(),
        cleanup_artifact: None,
        raw_transcript: transcript,
    }))
}

/// 只在 application 层执行的 Unicode-safe preview 截取。
pub fn preview(text: &str) -> String {
    text.chars().take(160).collect()
}

/// 取命中处前后 30 个字符的稳定 snippet。
pub fn snippet(text: &str, query: &str) -> Option<String> {
    let start = text.find(query)?;
    let lo = start.saturating_sub(30);
    let hi = (start + query.len() + 30).min(text.len());
    let lo = text.floor_char_boundary(lo);
    let hi = text.ceil_char_boundary(hi);
    let mut result = String::new();
    if lo > 0 {
        result.push('…');
    }
    result.push_str(&text[lo..hi]);
    if hi < text.len() {
        result.push('…');
    }
    Some(result)
}
