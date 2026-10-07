//! Cleanup marker output 校验与分片（ST-M4.3）。

use std::collections::HashMap;

use super::marker::MarkerBinding;

const MARKER_FAMILY_PREFIX: &str = "[[SEASNAIL_CTX_";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerValidationError {
    Missing,
    Duplicate,
    Reordered,
    Unknown,
    Truncated,
    Malformed,
}

impl std::fmt::Display for MarkerValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Missing => "expected cleanup marker is missing",
            Self::Duplicate => "cleanup marker occurs more than once",
            Self::Reordered => "cleanup markers are out of order",
            Self::Unknown => "cleanup output contains an unknown marker",
            Self::Truncated => "cleanup output contains a truncated marker",
            Self::Malformed => "cleanup output contains a malformed marker",
        })
    }
}

impl std::error::Error for MarkerValidationError {}

/// Marker 校验通过后的纯正文和可逆分片。`parts.len() == bindings.len() + 1`，第 i
/// 个 marker 位于 `parts[i]` 与 `parts[i + 1]` 之间；不保存 marker 本身。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedMarkerOutput {
    pub cleaned_text: String,
    pub parts: Vec<String>,
}

/// 单次线性扫描并校验 marker 完整性、顺序与精确字节内容。
pub fn validate_and_split(
    cleaned_text: &str,
    bindings: &[MarkerBinding],
) -> Result<ValidatedMarkerOutput, MarkerValidationError> {
    let mut parts = Vec::with_capacity(bindings.len() + 1);
    let mut cleaned = String::with_capacity(cleaned_text.len());
    let expected_indices: HashMap<&str, usize> = bindings
        .iter()
        .enumerate()
        .map(|(index, binding)| (binding.marker.as_str(), index))
        .collect();
    let mut cursor = 0;
    let mut marker_index = 0;

    for (start, _) in cleaned_text.match_indices(MARKER_FAMILY_PREFIX) {
        if start < cursor {
            continue;
        }
        cleaned.push_str(&cleaned_text[cursor..start]);
        parts.push(cleaned_text[cursor..start].to_owned());

        let end = cleaned_text[start..]
            .find("]]")
            .map(|offset| start + offset + 2)
            .ok_or(MarkerValidationError::Truncated)?;
        let candidate = &cleaned_text[start..end];
        if !is_well_formed_marker(candidate) {
            return Err(MarkerValidationError::Malformed);
        }
        let expected = bindings
            .get(marker_index)
            .ok_or(MarkerValidationError::Unknown)?;
        if candidate != expected.marker {
            match expected_indices.get(candidate) {
                Some(&index) if index < marker_index => {
                    return Err(MarkerValidationError::Duplicate)
                }
                Some(_) => return Err(MarkerValidationError::Reordered),
                None => return Err(MarkerValidationError::Unknown),
            }
        }
        marker_index += 1;
        cursor = end;
    }

    cleaned.push_str(&cleaned_text[cursor..]);
    parts.push(cleaned_text[cursor..].to_owned());
    if marker_index != bindings.len() {
        return Err(MarkerValidationError::Missing);
    }
    Ok(ValidatedMarkerOutput {
        cleaned_text: cleaned,
        parts,
    })
}

fn is_well_formed_marker(value: &str) -> bool {
    let Some(body) = value
        .strip_prefix("[[SEASNAIL_CTX_V1:")
        .and_then(|value| value.strip_suffix("]]"))
    else {
        return false;
    };
    let Some((sequence, random_hex)) = body.split_once(':') else {
        return false;
    };
    !sequence.is_empty()
        && sequence.bytes().all(|byte| byte.is_ascii_digit())
        && random_hex.len() == 32
        && random_hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cleanup::marker::{build_marker_input_with_rng, MarkerInput};
    use rand::rngs::mock::StepRng;
    use seasnail_proto::seasnail::v1::{ClipboardContextFile, ContextEvent, TranscriptFile};

    fn input() -> MarkerInput {
        let transcript = TranscriptFile {
            full_text: "原文".into(),
            ..Default::default()
        };
        let context = ClipboardContextFile {
            events: vec![
                ContextEvent {
                    sequence: 1,
                    source_sample_rate: 1000,
                    sample_offset: 0,
                    ..Default::default()
                },
                ContextEvent {
                    sequence: 2,
                    source_sample_rate: 1000,
                    sample_offset: 1000,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut rng = StepRng::new(1, 1);
        build_marker_input_with_rng(&transcript, &context, &mut rng)
    }

    fn valid_text(input: &MarkerInput) -> String {
        format!(
            "前{}中{}后",
            input.bindings[0].marker, input.bindings[1].marker
        )
    }

    #[test]
    fn valid_output_is_marker_free_and_reversible() {
        let input = input();
        let output = validate_and_split(&valid_text(&input), &input.bindings).unwrap();
        assert_eq!(output.cleaned_text, "前中后");
        assert_eq!(output.parts, vec!["前", "中", "后"]);
    }

    #[test]
    fn missing_duplicate_reordered_unknown_and_truncated_markers_fail() {
        let input = input();
        let first = &input.bindings[0].marker;
        let second = &input.bindings[1].marker;
        assert_eq!(
            validate_and_split(&format!("前{second}后"), &input.bindings),
            Err(MarkerValidationError::Reordered)
        );
        assert_eq!(
            validate_and_split(&format!("{first}{first}{second}"), &input.bindings),
            Err(MarkerValidationError::Duplicate)
        );
        assert_eq!(
            validate_and_split(&format!("{first}后"), &input.bindings),
            Err(MarkerValidationError::Missing)
        );
        assert_eq!(
            validate_and_split(
                &format!("{first}[[SEASNAIL_CTX_V1:99:00000000000000000000000000000000]]"),
                &input.bindings
            ),
            Err(MarkerValidationError::Unknown)
        );
        assert_eq!(
            validate_and_split(&format!("{first}[[SEASNAIL_CTX_V1:1:abc"), &input.bindings),
            Err(MarkerValidationError::Truncated)
        );
    }

    #[test]
    fn malformed_and_tampered_markers_fail() {
        let input = input();
        let first = &input.bindings[0].marker;
        let mut tampered = first.clone();
        let random_digit = MARKER_FAMILY_PREFIX.len() + 5;
        tampered.replace_range(random_digit..random_digit + 1, "f");
        assert_eq!(
            validate_and_split(&tampered, &input.bindings),
            Err(MarkerValidationError::Unknown)
        );
        assert_eq!(
            validate_and_split(
                &format!("{first}[[SEASNAIL_CTX_V2:1:00000000000000000000000000000000]]"),
                &input.bindings
            ),
            Err(MarkerValidationError::Malformed)
        );
    }

    #[test]
    fn zero_expected_markers_reject_any_marker_family() {
        assert_eq!(
            validate_and_split(
                "plain[[SEASNAIL_CTX_V1:0:00000000000000000000000000000000]]",
                &[]
            ),
            Err(MarkerValidationError::Unknown)
        );
        let output = validate_and_split("plain", &[]).unwrap();
        assert_eq!(output.parts, vec!["plain"]);
    }
}
