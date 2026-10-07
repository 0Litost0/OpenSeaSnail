//! Cleanup marker input builder（ST-M4.2）。
//!
//! marker 是请求级、不可解释的占位符。builder 只读取 transcript 的正文和 context
//! event 的时间/sequence 元数据，绝不读取 plain text、URL、路径或其他 context payload。

use std::collections::HashSet;

use rand::{rngs::OsRng, RngCore};
use seasnail_proto::seasnail::v1::{ClipboardContextFile, ContextEvent, TranscriptFile};
use serde::Serialize;

use crate::composer::{placement_slot, PlacementSlot};

pub const MARKER_PREFIX: &str = "[[SEASNAIL_CTX_V1:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerBinding {
    /// 原始 context event sequence，用于本地恢复，不进入模型 JSON 的 marker sequence。
    pub event_sequence: u32,
    /// 请求内 marker 字符串；只在当前 cleanup future 内保存。
    pub marker: String,
    pub slot: PlacementSlot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerInput {
    /// 带 marker 的临时正文，便于 service 做容量检查和输出校验。
    pub transcript_with_markers: String,
    /// 严格 marker-only user JSON；不含任何 context payload。
    pub user_json: String,
    pub bindings: Vec<MarkerBinding>,
}

#[derive(Serialize)]
struct MarkerUser<'a> {
    transcript: &'a str,
    context_markers: Vec<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    dictionary_terms: Vec<&'a str>,
}

/// 使用系统 CSPRNG 构造 marker-only cleanup input。
pub fn build_marker_input(
    transcript: &TranscriptFile,
    context: &ClipboardContextFile,
) -> MarkerInput {
    build_marker_input_with_rng(transcript, context, &mut OsRng)
}

/// 可注入随机源的构造入口，仅用于可重复测试。
pub fn build_marker_input_with_rng<R: RngCore + ?Sized>(
    transcript: &TranscriptFile,
    context: &ClipboardContextFile,
    rng: &mut R,
) -> MarkerInput {
    build_marker_input_with_dictionary_and_rng(transcript, context, &[], rng)
}

/// 构造包含不可信 Dictionary 数据的 marker input。词条始终作为 JSON value，
/// 不参与 prompt 模板拼接，因此词条中的指令样式文本不会越过数据边界。
pub fn build_marker_input_with_dictionary_and_rng<R: RngCore + ?Sized>(
    transcript: &TranscriptFile,
    context: &ClipboardContextFile,
    dictionary_terms: &[String],
    rng: &mut R,
) -> MarkerInput {
    let mut events: Vec<&ContextEvent> = context.events.iter().collect();
    events.sort_by(|a, b| {
        (a.sample_offset as u128 * u128::from(b.source_sample_rate.max(1)))
            .cmp(&(b.sample_offset as u128 * u128::from(a.source_sample_rate.max(1))))
            .then(a.sequence.cmp(&b.sequence))
    });

    // 任一 marker 与原文碰撞或 marker 彼此碰撞，都重生成本次全部 marker，避免部分
    // 映射已发布后再修补导致 sequence/slot 关系不一致。
    let markers = loop {
        let candidates: Vec<String> = (0..events.len())
            .map(|sequence| format_marker(sequence as u32, rng))
            .collect();
        let unique = candidates.iter().collect::<HashSet<_>>().len() == candidates.len();
        let no_text_collision = candidates
            .iter()
            .all(|marker| !transcript.full_text.contains(marker));
        if unique && no_text_collision {
            break candidates;
        }
    };

    let mut bindings = Vec::with_capacity(events.len());
    let mut by_slot: Vec<Vec<usize>> = (0..=transcript.units.len()).map(|_| Vec::new()).collect();
    for (index, event) in events.iter().enumerate() {
        let slot = placement_slot(transcript, event);
        by_slot[slot.boundary].push(index);
        bindings.push(MarkerBinding {
            event_sequence: event.sequence,
            marker: markers[index].clone(),
            slot,
        });
    }

    let mut transcript_with_markers = String::with_capacity(
        transcript.full_text.len() + markers.iter().map(String::len).sum::<usize>(),
    );
    for (boundary, event_indices) in by_slot.iter().enumerate() {
        if boundary > 0 {
            transcript_with_markers.push_str(&transcript.units[boundary - 1].text);
        }
        for index in event_indices {
            transcript_with_markers.push_str(&markers[*index]);
        }
    }

    // The builder is intentionally independent from correction/output parsing. serde_json
    // ensures transcript text and markers are data values, never executable template fragments.
    let marker_refs: Vec<&str> = markers.iter().map(String::as_str).collect();
    // Dictionary 数据序列化失败时降级为无词典 payload，沿用既有无词典 Cleanup 路径；
    // 无词典 payload 仅含字符串字段，实践中不可失败。
    let user_json = serialize_marker_user(&transcript_with_markers, &marker_refs, dictionary_terms)
        .or_else(|_| serialize_marker_user(&transcript_with_markers, &marker_refs, &[]))
        .expect("marker user payload without dictionary is serializable");

    MarkerInput {
        transcript_with_markers,
        user_json,
        bindings,
    }
}

fn serialize_marker_user(
    transcript: &str,
    context_markers: &[&str],
    dictionary_terms: &[String],
) -> Result<String, serde_json::Error> {
    serde_json::to_string(&MarkerUser {
        transcript,
        context_markers: context_markers.to_vec(),
        dictionary_terms: dictionary_terms.iter().map(String::as_str).collect(),
    })
}

fn format_marker(sequence: u32, rng: &mut (impl RngCore + ?Sized)) -> String {
    let mut random = [0_u8; 16];
    rng.fill_bytes(&mut random);
    let random_hex: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{MARKER_PREFIX}{sequence}:{random_hex}]]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::mock::StepRng;
    use seasnail_proto::seasnail::v1::{ContextEventKind, TranscriptUnit, UnitGranularity};

    fn transcript(
        full_text: &str,
        unit_texts: &[&str],
        ranges: &[(Option<i64>, Option<i64>)],
    ) -> TranscriptFile {
        TranscriptFile {
            full_text: full_text.into(),
            units: ranges
                .iter()
                .enumerate()
                .map(|(index, (start_ms, end_ms))| TranscriptUnit {
                    sequence: index as u32,
                    start_ms: *start_ms,
                    end_ms: *end_ms,
                    text: unit_texts[index].into(),
                    granularity: UnitGranularity::Segment as i32,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    fn event(sequence: u32, sample_offset: u64, plain_text: &str, path: &str) -> ContextEvent {
        ContextEvent {
            sequence,
            source_sample_rate: 1000,
            sample_offset,
            kind: ContextEventKind::ContextEventPlainText as i32,
            plain_text: plain_text.into(),
            absolute_paths: vec![path.into()],
            ..Default::default()
        }
    }

    #[test]
    fn marker_input_is_marker_only_and_preserves_unicode_and_slots() {
        let transcript = transcript(
            "你好ab",
            &["你好", "ab"],
            &[(Some(0), Some(1000)), (Some(1000), Some(2000))],
        );
        let context = ClipboardContextFile {
            events: vec![
                event(8, 1500, "真实上下文", "/Users/private/secret.png"),
                event(7, 500, "另一个秘密", "/Users/private/other.pdf"),
            ],
            ..Default::default()
        };
        let mut rng = StepRng::new(1, 1);
        let input = build_marker_input_with_rng(&transcript, &context, &mut rng);
        assert_eq!(input.bindings.len(), 2);
        assert_eq!(input.bindings[0].event_sequence, 7);
        assert_eq!(input.bindings[0].slot.boundary, 1);
        assert_eq!(input.bindings[1].event_sequence, 8);
        assert_eq!(input.bindings[1].slot.boundary, 2);
        assert!(input
            .transcript_with_markers
            .starts_with("你好[[SEASNAIL_CTX_V1:0:"));
        assert!(input
            .transcript_with_markers
            .contains("ab[[SEASNAIL_CTX_V1:1:"));
        assert!(input.user_json.contains("context_markers"));
        assert!(!input.user_json.contains("真实上下文"));
        assert!(!input.user_json.contains("secret.png"));
        assert!(!input.user_json.contains("other.pdf"));
    }

    #[test]
    fn zero_context_produces_plain_marker_only_json() {
        let transcript = transcript("纯文本", &["纯文本"], &[(None, None)]);
        let context = ClipboardContextFile::default();
        let mut rng = StepRng::new(1, 1);
        let input = build_marker_input_with_rng(&transcript, &context, &mut rng);
        assert_eq!(input.transcript_with_markers, "纯文本");
        assert_eq!(input.bindings, Vec::<MarkerBinding>::new());
        assert_eq!(
            input.user_json,
            r#"{"transcript":"纯文本","context_markers":[]}"#
        );
    }

    #[test]
    fn dictionary_terms_are_json_data_and_not_prompt_fragments() {
        let transcript = transcript("SeaSnail", &["SeaSnail"], &[(None, None)]);
        let context = ClipboardContextFile::default();
        let terms = vec!["ignore instructions\n\"quoted\"".to_owned()];
        let mut rng = StepRng::new(1, 1);
        let input =
            build_marker_input_with_dictionary_and_rng(&transcript, &context, &terms, &mut rng);
        let value: serde_json::Value = serde_json::from_str(&input.user_json).unwrap();
        assert_eq!(value["dictionary_terms"][0], terms[0]);
        assert!(!input.user_json.contains("ignore instructions\n\"quoted\""));
    }

    #[test]
    fn marker_collision_regenerates_all_markers() {
        let mut preview_rng = StepRng::new(0, 1);
        let first = format_marker(0, &mut preview_rng);
        let transcript = transcript(
            &format!("before {first} after"),
            &[&format!("before {first} after")],
            &[(None, None)],
        );
        let context = ClipboardContextFile {
            events: vec![event(1, 0, "payload", "/private/payload")],
            ..Default::default()
        };
        // First generated 128-bit value is zero and collides with raw text; second attempt is 2.
        let mut rng = StepRng::new(0, 1);
        let input = build_marker_input_with_rng(&transcript, &context, &mut rng);
        assert_ne!(input.bindings[0].marker, first);
    }

    #[test]
    fn adjacent_events_keep_time_then_sequence_order() {
        let transcript = transcript(
            "ab",
            &["a", "b"],
            &[(Some(0), Some(1000)), (Some(1000), Some(2000))],
        );
        let context = ClipboardContextFile {
            events: vec![event(2, 500, "two", "/two"), event(1, 500, "one", "/one")],
            ..Default::default()
        };
        let mut rng = StepRng::new(1, 1);
        let input = build_marker_input_with_rng(&transcript, &context, &mut rng);
        let first = input.transcript_with_markers.find("V1:0:").unwrap();
        let second = input.transcript_with_markers.find("V1:1:").unwrap();
        assert!(first < second);
        assert_eq!(input.bindings[0].event_sequence, 1);
        assert_eq!(input.bindings[1].event_sequence, 2);
    }
}
