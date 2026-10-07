//! Versioned, private protocol shared by `SherpaOnnxDriver` and the native sidecar.
//! These types are intentionally not part of the public OpenAPI or persisted data.

use crate::contract::OpenAiWord;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PROTOCOL_VERSION: u32 = 1;
pub const CAPABILITY_HEADER: &str = "x-seasnail-capability";
pub const PROTOCOL_HEADER: &str = "x-seasnail-protocol-version";
pub const HEALTH_PATH: &str = "/health";
pub const TRANSCRIBE_PATH: &str = "/v1/transcribe";
pub const AUDIO_CONTENT_TYPE: &str = "audio/wav";
pub const MAX_AUDIO_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_SEGMENTS: usize = 4096;
pub const MAX_DECODED_TOKENS: usize = 65_536;
pub const HEALTH_DEADLINE_SECS: u64 = 30;
pub const TRANSCRIBE_DEADLINE_SECS: u64 = 300;
pub const SIDECAR_INFERENCE_DEADLINE_SECS: u64 = 270;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthResponse {
    pub protocol_version: u32,
    pub status: HealthStatus,
    pub catalog_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Ready,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscribeResponse {
    pub protocol_version: u32,
    pub catalog_id: String,
    pub text: String,
    pub segments: Vec<VadSegment>,
    pub transforms: TextTransforms,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VadSegment {
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub text: String,
    pub decoded_tokens: Vec<String>,
    /// Parallel to `decoded_tokens`. `None` means upstream did not expose a
    /// trustworthy start timeline for this segment; callers must not invent one.
    pub token_start_seconds: Option<Vec<f64>>,
    pub language: Option<String>,
    pub event: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextTransforms {
    pub use_itn: bool,
    pub rule_fsts: bool,
    pub homophone_replacer: bool,
    /// True when visible text changed after decoded-token production. M4 must
    /// then treat exact token-to-text reconstruction as unproven.
    pub post_decode_text_modified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorResponse {
    pub protocol_version: u32,
    pub error: ProtocolErrorBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolErrorBody {
    pub code: ErrorCode,
    pub retryable: bool,
    /// Stable, non-sensitive diagnostic; never contains audio, text, or tokens.
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    Unauthorized,
    NotFound,
    MethodNotAllowed,
    Busy,
    RequestTooLarge,
    UnsupportedMediaType,
    InvalidAudio,
    InferenceFailed,
    NotReady,
    InferenceTimeout,
    ProtocolMismatch,
}

impl ErrorCode {
    pub const fn expected_retryable(self) -> bool {
        matches!(
            self,
            Self::Busy | Self::InferenceFailed | Self::NotReady | Self::InferenceTimeout
        )
    }

    pub const fn http_status(self) -> u16 {
        match self {
            Self::InvalidRequest | Self::ProtocolMismatch => 400,
            Self::Unauthorized => 401,
            Self::NotFound => 404,
            Self::MethodNotAllowed => 405,
            Self::Busy => 409,
            Self::RequestTooLarge => 413,
            Self::UnsupportedMediaType => 415,
            Self::InvalidAudio => 422,
            Self::InferenceFailed => 500,
            Self::NotReady => 503,
            Self::InferenceTimeout => 504,
        }
    }
}

pub fn valid_capability(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProtocolValidationError {
    #[error("sidecar response exceeds {MAX_RESPONSE_BYTES} bytes")]
    ResponseTooLarge,
    #[error("sidecar response JSON is invalid: {0}")]
    Json(String),
    #[error("sidecar protocol version mismatch")]
    Version,
    #[error("sidecar catalog identity mismatch")]
    CatalogIdentity,
    #[error("sidecar response violates the v1 contract: {0}")]
    Contract(&'static str),
}

pub fn parse_health_response(
    bytes: &[u8],
    expected_catalog_id: &str,
) -> Result<HealthResponse, ProtocolValidationError> {
    check_response_size(bytes)?;
    let response: HealthResponse = serde_json::from_slice(bytes)
        .map_err(|error| ProtocolValidationError::Json(error.to_string()))?;
    if response.protocol_version != PROTOCOL_VERSION {
        return Err(ProtocolValidationError::Version);
    }
    if response.catalog_id != expected_catalog_id {
        return Err(ProtocolValidationError::CatalogIdentity);
    }
    Ok(response)
}

pub fn parse_transcribe_response(
    bytes: &[u8],
    expected_catalog_id: &str,
) -> Result<TranscribeResponse, ProtocolValidationError> {
    check_response_size(bytes)?;
    let response: TranscribeResponse = serde_json::from_slice(bytes)
        .map_err(|error| ProtocolValidationError::Json(error.to_string()))?;
    validate_transcribe_response(&response, expected_catalog_id)?;
    Ok(response)
}

pub fn parse_error_response(bytes: &[u8]) -> Result<ErrorResponse, ProtocolValidationError> {
    check_response_size(bytes)?;
    let response: ErrorResponse = serde_json::from_slice(bytes)
        .map_err(|error| ProtocolValidationError::Json(error.to_string()))?;
    if response.protocol_version != PROTOCOL_VERSION {
        return Err(ProtocolValidationError::Version);
    }
    if response.error.message.is_empty()
        || response.error.message.len() > 512
        || response.error.retryable != response.error.code.expected_retryable()
    {
        return Err(ProtocolValidationError::Contract(
            "invalid error classification",
        ));
    }
    Ok(response)
}

/// Convert a validated Sherpa response into the smallest trustworthy visible
/// timeline. A token (or consecutive tokens sharing a start time) is kept as
/// one word; it is never split into separately timed characters.
///
/// `None` means that transcription succeeded but exact token-to-visible-text
/// alignment is not proven. Callers must preserve the authoritative text and
/// use the existing segment/append-only fallback instead of inventing words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AlignmentFailure {
    TimestampsMissing,
    TokenTextMismatch,
    PostDecodeTextModified,
    TimelineInvalid,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TokenSpan {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TokenTimeline {
    pub spans: Vec<TokenSpan>,
}

impl AlignmentFailure {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::TimestampsMissing => "timestamps_missing",
            Self::TokenTextMismatch => "token_text_mismatch",
            Self::PostDecodeTextModified => "post_decode_text_modified",
            Self::TimelineInvalid => "timeline_invalid",
        }
    }
}

pub(crate) fn visible_words(
    response: &TranscribeResponse,
) -> Result<Vec<OpenAiWord>, AlignmentFailure> {
    let timeline = visible_timeline(response)?;
    Ok(timeline
        .spans
        .into_iter()
        .map(|span| OpenAiWord {
            start: span.start,
            end: span.end,
            text: span.text,
        })
        .collect())
}

pub(crate) fn visible_timeline(
    response: &TranscribeResponse,
) -> Result<TokenTimeline, AlignmentFailure> {
    if response.transforms.post_decode_text_modified {
        return Err(AlignmentFailure::PostDecodeTextModified);
    }

    let mut spans = Vec::new();
    for segment in &response.segments {
        let Some(starts) = &segment.token_start_seconds else {
            return Err(AlignmentFailure::TimestampsMissing);
        };
        if starts.len() != segment.decoded_tokens.len() {
            return Err(AlignmentFailure::TimelineInvalid);
        }

        let decoded = segment.decoded_tokens.concat();
        if decoded != segment.text {
            return Err(AlignmentFailure::TokenTextMismatch);
        }

        let mut index = 0;
        while index < starts.len() {
            let start = starts[index];
            let mut end_index = index + 1;
            while end_index < starts.len() && starts[end_index] == start {
                end_index += 1;
            }
            let end = starts
                .get(end_index)
                .copied()
                .unwrap_or(segment.end_seconds);
            if !start.is_finite()
                || !end.is_finite()
                || start < segment.start_seconds
                || end <= start
                || end > segment.end_seconds
            {
                return Err(AlignmentFailure::TimelineInvalid);
            }
            let text = segment.decoded_tokens[index..end_index].concat();
            if text.is_empty() {
                return Err(AlignmentFailure::TokenTextMismatch);
            }
            spans.push(TokenSpan { start, end, text });
            index = end_index;
        }
    }

    if spans.is_empty() {
        Err(AlignmentFailure::TimestampsMissing)
    } else {
        Ok(TokenTimeline { spans })
    }
}

fn check_response_size(bytes: &[u8]) -> Result<(), ProtocolValidationError> {
    if bytes.len() > MAX_RESPONSE_BYTES {
        Err(ProtocolValidationError::ResponseTooLarge)
    } else {
        Ok(())
    }
}

fn validate_transcribe_response(
    response: &TranscribeResponse,
    expected_catalog_id: &str,
) -> Result<(), ProtocolValidationError> {
    if response.protocol_version != PROTOCOL_VERSION {
        return Err(ProtocolValidationError::Version);
    }
    if response.catalog_id != expected_catalog_id {
        return Err(ProtocolValidationError::CatalogIdentity);
    }
    if response.text.trim().is_empty() != response.segments.is_empty()
        || response.segments.len() > MAX_SEGMENTS
    {
        return Err(ProtocolValidationError::Contract(
            "text and segments must be empty together and remain within limits",
        ));
    }
    if !response.transforms.use_itn
        || response.transforms.rule_fsts
        || response.transforms.homophone_replacer
    {
        return Err(ProtocolValidationError::Contract(
            "unsupported text transform configuration",
        ));
    }
    let mut previous_end = 0.0_f64;
    let mut token_count = 0_usize;
    for segment in &response.segments {
        if !segment.start_seconds.is_finite()
            || !segment.end_seconds.is_finite()
            || segment.start_seconds < previous_end
            || segment.start_seconds < 0.0
            || segment.end_seconds <= segment.start_seconds
            || segment.text.trim().is_empty()
            || segment.decoded_tokens.iter().any(|token| token.is_empty())
        {
            return Err(ProtocolValidationError::Contract("invalid VAD segment"));
        }
        token_count = token_count
            .checked_add(segment.decoded_tokens.len())
            .ok_or(ProtocolValidationError::Contract(
                "decoded token count overflow",
            ))?;
        if token_count > MAX_DECODED_TOKENS {
            return Err(ProtocolValidationError::Contract("too many decoded tokens"));
        }
        // Token timestamps are optional alignment evidence. Malformed
        // evidence must not turn an otherwise valid transcription into a
        // retryable request failure; `visible_timeline` classifies it as an
        // alignment fallback and preserves the authoritative text.
        previous_end = segment.end_seconds;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const VALID: &[u8] = include_bytes!("../../tests/fixtures/sherpa_protocol/valid-response.json");
    const ERROR: &[u8] = include_bytes!("../../tests/fixtures/sherpa_protocol/error-response.json");

    #[test]
    fn accepts_locked_v1_fixtures() {
        let response = parse_transcribe_response(VALID, "sensevoice-small-sherpa-int8").unwrap();
        assert_eq!(response.segments.len(), 2);
        assert_eq!(response.segments[0].decoded_tokens.len(), 2);
        assert_eq!(
            parse_error_response(ERROR).unwrap().error.code,
            ErrorCode::Busy
        );
    }

    #[test]
    fn accepts_exact_empty_success_and_rejects_one_sided_empty_shapes() {
        let empty = serde_json::json!({
            "protocol_version": PROTOCOL_VERSION,
            "catalog_id": "sensevoice-small-sherpa-int8",
            "text": "",
            "segments": [],
            "transforms": {
                "use_itn": true,
                "rule_fsts": false,
                "homophone_replacer": false,
                "post_decode_text_modified": false
            }
        });
        let response = parse_transcribe_response(
            &serde_json::to_vec(&empty).unwrap(),
            "sensevoice-small-sherpa-int8",
        )
        .unwrap();
        assert!(response.text.is_empty());
        assert!(response.segments.is_empty());

        let mut text_only = empty.clone();
        text_only["text"] = "visible".into();
        assert!(matches!(
            parse_transcribe_response(
                &serde_json::to_vec(&text_only).unwrap(),
                "sensevoice-small-sherpa-int8"
            ),
            Err(ProtocolValidationError::Contract(_))
        ));

        let mut segment_only = empty;
        segment_only["segments"] = serde_json::json!([{
            "start_seconds": 0.0,
            "end_seconds": 0.5,
            "text": "visible",
            "decoded_tokens": ["visible"],
            "token_start_seconds": [0.0],
            "language": "en",
            "event": null
        }]);
        assert!(matches!(
            parse_transcribe_response(
                &serde_json::to_vec(&segment_only).unwrap(),
                "sensevoice-small-sherpa-int8"
            ),
            Err(ProtocolValidationError::Contract(_))
        ));

        for whitespace in [" \t\n", "\u{00a0}\u{2003}"] {
            let mut whitespace_with_segment = segment_only.clone();
            whitespace_with_segment["text"] = whitespace.into();
            assert!(matches!(
                parse_transcribe_response(
                    &serde_json::to_vec(&whitespace_with_segment).unwrap(),
                    "sensevoice-small-sherpa-int8"
                ),
                Err(ProtocolValidationError::Contract(_))
            ));
        }
    }

    #[test]
    fn rejects_segment_and_token_limits_after_relaxing_empty_results() {
        let transforms = TextTransforms {
            use_itn: true,
            rule_fsts: false,
            homophone_replacer: false,
            post_decode_text_modified: false,
        };
        let segment = |start: f64| VadSegment {
            start_seconds: start,
            end_seconds: start + 0.5,
            text: "visible".into(),
            decoded_tokens: vec!["visible".into()],
            token_start_seconds: None,
            language: None,
            event: None,
        };
        let response_with = |segments: Vec<VadSegment>| TranscribeResponse {
            protocol_version: PROTOCOL_VERSION,
            catalog_id: "sensevoice-small-sherpa-int8".into(),
            text: "visible".into(),
            segments,
            transforms,
        };

        // Exactly at the limits the response stays legal: a future `>` vs `>=`
        // regression must fail these cases.
        let at_segment_limit =
            response_with((0..MAX_SEGMENTS).map(|i| segment(i as f64)).collect());
        assert!(
            validate_transcribe_response(&at_segment_limit, "sensevoice-small-sherpa-int8").is_ok()
        );

        let at_token_limit = response_with(vec![
            VadSegment {
                decoded_tokens: vec!["t".into(); MAX_DECODED_TOKENS - 1],
                ..segment(0.0)
            },
            segment(1.0),
        ]);
        assert!(
            validate_transcribe_response(&at_token_limit, "sensevoice-small-sherpa-int8").is_ok()
        );

        let over_segments = response_with((0..=MAX_SEGMENTS).map(|i| segment(i as f64)).collect());
        assert_eq!(
            validate_transcribe_response(&over_segments, "sensevoice-small-sherpa-int8"),
            Err(ProtocolValidationError::Contract(
                "text and segments must be empty together and remain within limits"
            ))
        );

        let token_heavy = response_with(vec![
            VadSegment {
                decoded_tokens: vec!["t".into(); MAX_DECODED_TOKENS],
                ..segment(0.0)
            },
            segment(1.0),
        ]);
        assert_eq!(
            validate_transcribe_response(&token_heavy, "sensevoice-small-sherpa-int8"),
            Err(ProtocolValidationError::Contract("too many decoded tokens"))
        );
    }

    #[test]
    fn visible_words_require_exact_text_and_group_shared_timestamps() {
        let mut value: Value = serde_json::from_slice(VALID).unwrap();
        value["segments"][0]["token_start_seconds"] = serde_json::json!([0.1, 0.1]);
        value["segments"][1]["token_start_seconds"] = serde_json::json!([1.0]);
        let response: TranscribeResponse = serde_json::from_value(value).unwrap();
        let words = visible_words(&response).unwrap();
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].text, "你好");
        assert_eq!(words[0].end, 0.8);
        assert_eq!(words[1].text, " world");
        assert_eq!(words[1].end, 1.8);
    }

    #[test]
    fn visible_words_degrade_when_token_text_or_post_decode_text_changes() {
        let mut value: Value = serde_json::from_slice(VALID).unwrap();
        value["segments"][0]["decoded_tokens"][0] = "错".into();
        let response: TranscribeResponse = serde_json::from_value(value).unwrap();
        assert_eq!(
            visible_words(&response),
            Err(AlignmentFailure::TokenTextMismatch)
        );

        let mut value: Value = serde_json::from_slice(VALID).unwrap();
        value["transforms"]["post_decode_text_modified"] = true.into();
        let response: TranscribeResponse = serde_json::from_value(value).unwrap();
        assert_eq!(
            visible_words(&response),
            Err(AlignmentFailure::PostDecodeTextModified)
        );
    }

    #[test]
    fn rejects_bad_version_missing_fields_unknown_fields_and_identity() {
        let mut value: Value = serde_json::from_slice(VALID).unwrap();
        value["protocol_version"] = 2.into();
        assert_eq!(
            parse_transcribe_response(
                &serde_json::to_vec(&value).unwrap(),
                "sensevoice-small-sherpa-int8"
            ),
            Err(ProtocolValidationError::Version)
        );

        let mut value: Value = serde_json::from_slice(VALID).unwrap();
        value.as_object_mut().unwrap().remove("text");
        assert!(matches!(
            parse_transcribe_response(
                &serde_json::to_vec(&value).unwrap(),
                "sensevoice-small-sherpa-int8"
            ),
            Err(ProtocolValidationError::Json(_))
        ));

        let mut value: Value = serde_json::from_slice(VALID).unwrap();
        value["unexpected"] = true.into();
        assert!(matches!(
            parse_transcribe_response(
                &serde_json::to_vec(&value).unwrap(),
                "sensevoice-small-sherpa-int8"
            ),
            Err(ProtocolValidationError::Json(_))
        ));

        assert_eq!(
            parse_transcribe_response(VALID, "other"),
            Err(ProtocolValidationError::CatalogIdentity)
        );
    }

    #[test]
    fn rejects_invalid_segments_and_transforms_but_degrades_bad_timeline() {
        let mut value: Value = serde_json::from_slice(VALID).unwrap();
        value["segments"][1]["start_seconds"] = 0.5.into();
        assert!(matches!(
            parse_transcribe_response(
                &serde_json::to_vec(&value).unwrap(),
                "sensevoice-small-sherpa-int8"
            ),
            Err(ProtocolValidationError::Contract(_))
        ));

        let mut value: Value = serde_json::from_slice(VALID).unwrap();
        value["segments"][0]["token_start_seconds"] = serde_json::json!([0.2]);
        let response = parse_transcribe_response(
            &serde_json::to_vec(&value).unwrap(),
            "sensevoice-small-sherpa-int8",
        )
        .unwrap();
        assert_eq!(
            visible_timeline(&response),
            Err(AlignmentFailure::TimelineInvalid)
        );

        let mut value: Value = serde_json::from_slice(VALID).unwrap();
        value["segments"][0]["token_start_seconds"] = serde_json::json!([0.05, 0.4]);
        let response = parse_transcribe_response(
            &serde_json::to_vec(&value).unwrap(),
            "sensevoice-small-sherpa-int8",
        )
        .unwrap();
        assert_eq!(
            visible_timeline(&response),
            Err(AlignmentFailure::TimelineInvalid),
            "token starts before their VAD segment must not produce words"
        );

        let mut value: Value = serde_json::from_slice(VALID).unwrap();
        value["transforms"]["rule_fsts"] = true.into();
        assert!(matches!(
            parse_transcribe_response(
                &serde_json::to_vec(&value).unwrap(),
                "sensevoice-small-sherpa-int8"
            ),
            Err(ProtocolValidationError::Contract(_))
        ));

        let mut value: Value = serde_json::from_slice(ERROR).unwrap();
        value["error"]["retryable"] = false.into();
        assert!(matches!(
            parse_error_response(&serde_json::to_vec(&value).unwrap()),
            Err(ProtocolValidationError::Contract(_))
        ));
    }

    #[test]
    fn rejects_response_over_limit_before_json_parsing() {
        let oversized = vec![b' '; MAX_RESPONSE_BYTES + 1];
        assert_eq!(
            parse_transcribe_response(&oversized, "id"),
            Err(ProtocolValidationError::ResponseTooLarge)
        );
    }

    #[test]
    fn locks_capability_shape_error_status_and_retryability() {
        assert!(valid_capability(&"a1".repeat(32)));
        assert!(!valid_capability(&"A1".repeat(32)));
        assert!(!valid_capability("short"));
        let cases = [
            (ErrorCode::InvalidRequest, 400, false),
            (ErrorCode::Unauthorized, 401, false),
            (ErrorCode::NotFound, 404, false),
            (ErrorCode::MethodNotAllowed, 405, false),
            (ErrorCode::Busy, 409, true),
            (ErrorCode::RequestTooLarge, 413, false),
            (ErrorCode::UnsupportedMediaType, 415, false),
            (ErrorCode::InvalidAudio, 422, false),
            (ErrorCode::InferenceFailed, 500, true),
            (ErrorCode::NotReady, 503, true),
            (ErrorCode::InferenceTimeout, 504, true),
            (ErrorCode::ProtocolMismatch, 400, false),
        ];
        for (code, status, retryable) in cases {
            assert_eq!(code.http_status(), status);
            assert_eq!(code.expected_retryable(), retryable);
        }
    }
}
