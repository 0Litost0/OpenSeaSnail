//! Cleanup JSON/correction validation（ST-M4.5）。

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::Value;

use super::output::ValidatedMarkerOutput;

pub const MAX_RESPONSE_BODY_BYTES: usize = 256 * 1024;
pub const MAX_CLEANED_TEXT_BYTES: usize = 128 * 1024;
pub const MAX_RAW_CORRECTION_ITEMS: usize = 128;
pub const MAX_CORRECTIONS: usize = 32;
pub const MAX_CORRECTION_FIELD_CHARS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupJsonError {
    ResponseTooLarge,
    TopLevelInvalid,
    CleanedTextInvalid,
}

impl std::fmt::Display for CleanupJsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ResponseTooLarge => "cleanup JSON response exceeds the body limit",
            Self::TopLevelInvalid => "cleanup response must be exactly one JSON object",
            Self::CleanedTextInvalid => "cleanup cleaned_text is invalid",
        })
    }
}

impl std::error::Error for CleanupJsonError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CorrectionKind {
    Phonetic,
    ProperNoun,
    OtherAsr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedCorrection {
    pub original_text: String,
    pub corrected_text: String,
    pub kind: CorrectionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedCleanupOutput {
    pub cleaned_text: String,
    pub corrections: Vec<ValidatedCorrection>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorrectionCandidate {
    original_text: String,
    corrected_text: String,
    kind: String,
}

/// 严格解析顶层 cleanup JSON，并在不影响正文的前提下逐条过滤 correction。
/// marker 的结构/顺序校验由 [`super::output::validate_and_split`] 完成。
pub fn validate_cleanup_json(
    response: &str,
    raw_full_text: &str,
    marker_output: &ValidatedMarkerOutput,
) -> Result<ValidatedCleanupOutput, CleanupJsonError> {
    if response.len() > MAX_RESPONSE_BODY_BYTES {
        return Err(CleanupJsonError::ResponseTooLarge);
    }
    let value: Value =
        serde_json::from_str(response).map_err(|_| CleanupJsonError::TopLevelInvalid)?;
    let object = value.as_object().ok_or(CleanupJsonError::TopLevelInvalid)?;
    if object.len() != 2
        || !object.contains_key("cleaned_text")
        || !object.contains_key("corrections")
    {
        return Err(CleanupJsonError::TopLevelInvalid);
    }
    let response_cleaned_text = object
        .get("cleaned_text")
        .and_then(Value::as_str)
        .ok_or(CleanupJsonError::CleanedTextInvalid)?;
    if response_cleaned_text.trim().is_empty()
        || response_cleaned_text.len() > MAX_CLEANED_TEXT_BYTES
    {
        return Err(CleanupJsonError::CleanedTextInvalid);
    }
    // The marker pass consumes the model's markerized cleaned_text and returns the canonical
    // marker-free正文. A response with markers is valid only after that pass has validated the
    // exact marker sequence; a marker-free response must still match the validated projection.
    let cleaned_text = if response_cleaned_text.contains("[[SEASNAIL_CTX_") {
        marker_output.cleaned_text.clone()
    } else if marker_output.cleaned_text == response_cleaned_text {
        response_cleaned_text.to_owned()
    } else {
        return Err(CleanupJsonError::CleanedTextInvalid);
    };

    let corrections = object
        .get("corrections")
        .and_then(Value::as_array)
        .ok_or(CleanupJsonError::TopLevelInvalid)?;
    let mut output = Vec::with_capacity(MAX_CORRECTIONS.min(corrections.len()));
    let mut seen = HashSet::new();
    for item in corrections.iter().take(MAX_RAW_CORRECTION_ITEMS) {
        let Ok(candidate) = serde_json::from_value::<CorrectionCandidate>(item.clone()) else {
            continue;
        };
        let original_text = candidate.original_text.trim().to_owned();
        let corrected_text = candidate.corrected_text.trim().to_owned();
        if original_text.is_empty()
            || corrected_text.is_empty()
            || original_text == corrected_text
            || original_text.chars().count() > MAX_CORRECTION_FIELD_CHARS
            || corrected_text.chars().count() > MAX_CORRECTION_FIELD_CHARS
            || original_text.chars().any(char::is_control)
            || corrected_text.chars().any(char::is_control)
            || original_text.contains("[[SEASNAIL_CTX_")
            || corrected_text.contains("[[SEASNAIL_CTX_")
            || !raw_full_text.contains(&original_text)
            || !marker_output.cleaned_text.contains(&corrected_text)
        {
            continue;
        }
        let Some(kind) = parse_kind(&candidate.kind) else {
            continue;
        };
        let key = (original_text.clone(), corrected_text.clone(), kind);
        if !seen.insert(key) {
            continue;
        }
        output.push(ValidatedCorrection {
            original_text,
            corrected_text,
            kind,
        });
        if output.len() == MAX_CORRECTIONS {
            // Remaining entries cannot affect the stable first-32 result.
            break;
        }
    }
    Ok(ValidatedCleanupOutput {
        cleaned_text: cleaned_text.to_owned(),
        corrections: output,
    })
}

fn parse_kind(value: &str) -> Option<CorrectionKind> {
    match value {
        "phonetic" => Some(CorrectionKind::Phonetic),
        "proper_noun" => Some(CorrectionKind::ProperNoun),
        "other_asr" => Some(CorrectionKind::OtherAsr),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cleanup::marker::build_marker_input_with_rng;
    use crate::cleanup::output::validate_and_split;
    use rand::rngs::mock::StepRng;
    use seasnail_proto::seasnail::v1::{ClipboardContextFile, TranscriptFile};

    fn markerless() -> (String, ValidatedMarkerOutput) {
        let transcript = TranscriptFile {
            full_text: "原始文本中的错误词".into(),
            ..Default::default()
        };
        let input = build_marker_input_with_rng(
            &transcript,
            &ClipboardContextFile::default(),
            &mut StepRng::new(1, 1),
        );
        let output = ValidatedMarkerOutput {
            cleaned_text: "原始文本中的正确词".into(),
            parts: vec!["原始文本中的正确词".into()],
        };
        // Keep marker construction in this helper so tests exercise the same binding path.
        let _ = validate_and_split("原始文本中的错误词", &input.bindings);
        (transcript.full_text, output)
    }

    #[test]
    fn strict_top_level_and_cleaned_text_validation_is_fail_closed() {
        let (raw, markerless) = markerless();
        assert_eq!(
            validate_cleanup_json(
                r#"{"cleaned_text":"原始文本中的错误词","corrections":[],"extra":1}"#,
                &raw,
                &markerless,
            ),
            Err(CleanupJsonError::TopLevelInvalid)
        );
        assert_eq!(
            validate_cleanup_json(
                r#"{"cleaned_text":"   ","corrections":[]}"#,
                &raw,
                &markerless,
            ),
            Err(CleanupJsonError::CleanedTextInvalid)
        );
        let oversized = "x".repeat(MAX_CLEANED_TEXT_BYTES + 1);
        let response = format!(r#"{{"cleaned_text":"{oversized}","corrections":[]}}"#);
        assert_eq!(
            validate_cleanup_json(&response, &raw, &markerless),
            Err(CleanupJsonError::CleanedTextInvalid)
        );
    }

    #[test]
    fn invalid_correction_is_dropped_but_valid_correction_survives() {
        let (raw, markerless) = markerless();
        let response = r#"{
          "cleaned_text":"原始文本中的正确词",
          "corrections":[
            {"original_text":"错误词","corrected_text":"正确词","kind":"phonetic"},
            {"original_text":"不存在","corrected_text":"正确词","kind":"phonetic"},
            {"original_text":"错误词","corrected_text":"错误词","kind":"other_asr"},
            {"original_text":"错误词","corrected_text":"正确词","kind":"unknown"},
            {"original_text":"错误词","corrected_text":"正确词","kind":"phonetic","extra":1}
          ]
        }"#;
        let output = validate_cleanup_json(response, &raw, &markerless).unwrap();
        assert_eq!(output.corrections.len(), 1);
        assert_eq!(output.corrections[0].kind, CorrectionKind::Phonetic);
    }

    #[test]
    fn corrections_trim_deduplicate_and_enforce_limits() {
        let (raw, markerless) = markerless();
        let mut items = vec![
            r#"{"original_text":" 错误词 ","corrected_text":" 正确词 ","kind":"phonetic"}"#
                .to_owned(),
            r#"{"original_text":"错误词","corrected_text":"正确词","kind":"phonetic"}"#.to_owned(),
        ];
        for _ in 0..130 {
            items.push(
                r#"{"original_text":"错误词","corrected_text":"正确词","kind":"other_asr"}"#
                    .to_owned(),
            );
        }
        items.push(format!(
            r#"{{"original_text":"{}","corrected_text":"正确词","kind":"other_asr"}}"#,
            "x".repeat(MAX_CORRECTION_FIELD_CHARS + 1)
        ));
        let response = format!(
            r#"{{"cleaned_text":"原始文本中的正确词","corrections":[{}]}}"#,
            items.join(",")
        );
        let output = validate_cleanup_json(&response, &raw, &markerless).unwrap();
        assert_eq!(output.corrections.len(), 2);
        assert_eq!(output.corrections[0].original_text, "错误词");
        assert_eq!(output.corrections[0].corrected_text, "正确词");
    }

    #[test]
    fn response_body_limit_is_enforced_before_parsing() {
        let (_raw, markerless) = markerless();
        let oversized = "x".repeat(MAX_RESPONSE_BODY_BYTES + 1);
        assert_eq!(
            validate_cleanup_json(&oversized, "raw", &markerless),
            Err(CleanupJsonError::ResponseTooLarge)
        );
    }
}
