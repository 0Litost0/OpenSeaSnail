//! 时间线展示 composer。生产路径使用 [`compose_typed_timeline`] 与
//! [`render_typed_timeline`]，从独立的 transcript/context 事实文件动态生成只读结果；
//! 结果不得回写，也不得作为持久化全文或 HTML。
//!
//! HTML 产出（Phase 1 / M4.2）为转义后的 plain——所有 ASR 文本与标记都 HTML 转义，
//! 是合法且安全的最小 HTML 子集。M4.3 将富文本事件的 `html_fragment` 经 sanitize 后
//! 在同位置嵌入（替换这里的转义 plain 标记）。

use ammonia::Builder;
use seasnail_proto::seasnail::v1::{
    CleanupContextPlacement, ClipboardContextFile, ContextEvent, ContextEventKind, TranscriptFile,
};
use serde::Serialize;

/// M3 typed timeline 的只读展示项；不写回 TranscriptFile，也不把 context 混入正文。
/// 工作台只读展示项；7-variant 联合体（discriminator = `kind`）对齐设计：
/// 正文 run 与各上下文类型各占一个变体，不再用 `kind` 字符串子字段二次区分。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TypedTimelineItem {
    Transcript {
        text: String,
        speaker: String,
    },
    /// Cleanup 成功后无法可靠恢复 speaker/unit 归属的最终正文。
    FinalText {
        text: String,
    },
    ContextText {
        sequence: u32,
        captured_at_ms: i64,
        placement: String,
        text: String,
    },
    ContextRichText {
        sequence: u32,
        captured_at_ms: i64,
        placement: String,
        plain_text: String,
        sanitized_html: String,
    },
    ContextLink {
        sequence: u32,
        captured_at_ms: i64,
        placement: String,
        url: String,
    },
    ContextFile {
        sequence: u32,
        captured_at_ms: i64,
        placement: String,
        /// 工作台 root-only DTO 中的本地绝对路径；普通会话 API 不暴露它。
        resources: Vec<ContextResourceDisplay>,
    },
    ContextImage {
        sequence: u32,
        captured_at_ms: i64,
        placement: String,
        resources: Vec<ContextResourceDisplay>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextResourceDisplay {
    pub index: usize,
    pub path: String,
    pub display_name: String,
    pub mime_type: String,
    pub available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextDisplay {
    Plain {
        text: String,
    },
    Rich {
        plain_text: String,
        sanitized_html: String,
    },
    Link {
        url: String,
    },
    File {
        count: usize,
    },
    Image {
        count: usize,
    },
}

/// 上下文相对于 transcript units 的插入精度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementPrecision {
    /// 事件落在唯一一个有效 unit 内，或明确位于首前/末后/干净间隙。
    Exact,
    /// 时间落在重叠或缺少边界的区域，只能选择最近的已开始 unit。
    Approximate,
    /// transcript 没有任何可用 start timestamp，只能沿用末尾 fallback。
    Fallback,
}

impl PlacementPrecision {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Approximate => "approximate",
            Self::Fallback => "fallback",
        }
    }
}

/// transcript/context 的统一位置槽。`boundary` 是插入到 transcript units 的槽位：
/// 0 表示正文首前，`n` 表示第 n 个 unit 之后；它不代表字符级位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementSlot {
    pub boundary: usize,
    pub precision: PlacementPrecision,
}

/// 根据 transcript unit 时间边界和 context event sample offset 计算统一位置槽。
///
/// 使用有理数比较避免浮点误差，并沿用现有半开区间 `[start, end)` 语义：事件在
/// unit 内时插入该 unit 之后；重叠 unit 选择最后一个已开始的 unit 并标记近似。
/// 没有任何可用 start timestamp 时，所有事件按原有策略落在正文末尾并标记 fallback。
pub fn placement_slot(transcript: &TranscriptFile, event: &ContextEvent) -> PlacementSlot {
    let unit_count = transcript.units.len();
    if transcript.units.iter().all(|unit| unit.start_ms.is_none()) {
        return PlacementSlot {
            boundary: unit_count,
            precision: PlacementPrecision::Fallback,
        };
    }

    let mut containing = None;
    let mut containing_count = 0;
    for (index, unit) in transcript.units.iter().enumerate() {
        if unit
            .start_ms
            .is_some_and(|start| !event_before_ms(event, start))
            && unit.end_ms.is_some_and(|end| event_before_ms(event, end))
        {
            containing = Some(index);
            containing_count += 1;
        }
    }
    if containing_count == 1 {
        return PlacementSlot {
            boundary: containing.expect("one containing unit") + 1,
            precision: PlacementPrecision::Exact,
        };
    }

    if transcript
        .units
        .first()
        .and_then(|unit| unit.start_ms)
        .is_some_and(|start| event_before_ms(event, start))
    {
        return PlacementSlot {
            boundary: 0,
            precision: PlacementPrecision::Exact,
        };
    }

    let last_started = transcript.units.iter().rposition(|unit| {
        unit.start_ms
            .is_some_and(|start| !event_before_ms(event, start))
    });
    let boundary = last_started.map_or(0, |index| index + 1);
    let after_last = transcript
        .units
        .last()
        .and_then(|unit| unit.end_ms)
        .is_some_and(|end| !event_before_ms(event, end));
    let in_clean_gap = last_started.is_some_and(|index| {
        index + 1 < transcript.units.len()
            && transcript.units[index]
                .end_ms
                .is_some_and(|end| !event_before_ms(event, end))
            && transcript.units[index + 1]
                .start_ms
                .is_some_and(|start| event_before_ms(event, start))
    });
    PlacementSlot {
        boundary,
        precision: if after_last || in_clean_gap {
            PlacementPrecision::Exact
        } else {
            PlacementPrecision::Approximate
        },
    }
}

/// 将 context event 转为工作台展示 DTO；普通 API 不暴露该 root-only 结构。
pub fn context_display(event: &ContextEvent) -> ContextDisplay {
    match ContextEventKind::try_from(event.kind).ok() {
        Some(ContextEventKind::ContextEventFiles) => ContextDisplay::File {
            count: event.absolute_paths.len(),
        },
        Some(ContextEventKind::ContextEventImage) => ContextDisplay::Image {
            count: event.absolute_paths.len(),
        },
        Some(ContextEventKind::ContextEventRichText) => ContextDisplay::Rich {
            plain_text: event.plain_text.clone(),
            sanitized_html: Builder::new()
                .rm_tags(["a", "area", "img", "map"])
                .tag_attributes(std::collections::HashMap::new())
                .url_schemes(std::collections::HashSet::new())
                .clean(&event.html_fragment)
                .to_string(),
        },
        _ if normalized_http_url(&event.plain_text).is_some() => ContextDisplay::Link {
            url: normalized_http_url(&event.plain_text).expect("URL was validated"),
        },
        _ => ContextDisplay::Plain {
            text: event.plain_text.clone(),
        },
    }
}

pub fn is_complete_http_url(value: &str) -> bool {
    normalized_http_url(value).is_some()
}

pub fn normalized_http_url(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.chars().any(char::is_control) {
        return None;
    }
    let authority = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))?;
    if authority.is_empty() || authority.starts_with('/') {
        return None;
    }
    let parsed = reqwest::Url::parse(trimmed).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return None;
    }
    Some(trimmed.to_owned())
}

/// 从 schema v2 transcript 与独立 context 生成展示时间线。
/// 事件采用有理数 sample_offset/rate 比较，避免浮点排序；落在 unit 内时放在该 unit 后。
pub fn compose_typed_timeline(
    transcript: &TranscriptFile,
    context: &ClipboardContextFile,
) -> Vec<TypedTimelineItem> {
    let mut events: Vec<&ContextEvent> = context.events.iter().collect();
    events.sort_by(|a, b| {
        (a.sample_offset as u128 * u128::from(b.source_sample_rate.max(1)))
            .cmp(&(b.sample_offset as u128 * u128::from(a.source_sample_rate.max(1))))
            .then(a.sequence.cmp(&b.sequence))
    });
    let mut output = Vec::new();
    if transcript.units.iter().all(|unit| unit.start_ms.is_none()) {
        for unit in &transcript.units {
            push_text(&mut output, unit.text.clone(), unit.speaker.clone());
        }
        for event in events {
            push_context(&mut output, event, "fallback");
        }
        return output;
    }
    let mut slots: Vec<Vec<(&ContextEvent, PlacementPrecision)>> =
        (0..=transcript.units.len()).map(|_| Vec::new()).collect();
    for event in events {
        let placement = placement_slot(transcript, event);
        slots[placement.boundary].push((event, placement.precision));
    }
    for (event, precision) in slots[0].drain(..) {
        push_context(&mut output, event, precision.label());
    }
    for (index, unit) in transcript.units.iter().enumerate() {
        push_text(&mut output, unit.text.clone(), unit.speaker.clone());
        for (event, precision) in slots[index + 1].drain(..) {
            push_context(&mut output, event, precision.label());
        }
    }
    output
}

pub fn render_typed_timeline(items: &[TypedTimelineItem]) -> (String, String) {
    let mut plain = String::new();
    let mut html = String::new();
    for item in items {
        match item {
            TypedTimelineItem::Transcript { text, .. } => {
                plain.push_str(text);
                html.push_str(&html_escape(text));
            }
            TypedTimelineItem::FinalText { text } => {
                plain.push_str(text);
                html.push_str(&html_escape(text));
            }
            TypedTimelineItem::ContextText { text, .. } => {
                let marker = format!("（剪贴板上下文：{text}）");
                plain.push_str(&marker);
                html.push_str(&html_escape(&marker));
            }
            TypedTimelineItem::ContextRichText {
                plain_text,
                sanitized_html,
                ..
            } => {
                let marker = format!("（剪贴板上下文：{plain_text}）");
                plain.push_str(&marker);
                if sanitized_html.is_empty() {
                    html.push_str(&html_escape(&marker));
                } else {
                    html.push_str(sanitized_html);
                }
            }
            TypedTimelineItem::ContextLink { url, .. } => {
                let marker = format!("（剪贴板上下文：{url}）");
                plain.push_str(&marker);
                html.push_str(&html_escape(&marker));
            }
            TypedTimelineItem::ContextFile { resources, .. }
            | TypedTimelineItem::ContextImage { resources, .. } => {
                for (index, resource) in resources.iter().enumerate() {
                    if index > 0 {
                        plain.push('\n');
                        html.push_str("<br>");
                    }
                    let marker = format!("（剪贴板上下文：{}）", resource.path);
                    plain.push_str(&marker);
                    html.push_str(&html_escape(&marker));
                }
            }
        }
    }
    (plain, html)
}

/// 使用 cleanup cache 的可逆分片恢复上下文相对位置。`parts[i]` 位于第 i 个
/// event 前，cache 中不保存 context payload，事件内容仍从加密 context 文件读取。
pub fn compose_cleanup_timeline(
    parts: &[String],
    event_sequences: &[u32],
    context: &ClipboardContextFile,
) -> Vec<TypedTimelineItem> {
    if parts.len() != event_sequences.len() + 1 {
        return vec![TypedTimelineItem::FinalText {
            text: parts.concat(),
        }];
    }
    let events: std::collections::HashMap<u32, &ContextEvent> = context
        .events
        .iter()
        .map(|event| (event.sequence, event))
        .collect();
    let mut output = Vec::with_capacity(parts.len() + event_sequences.len());
    for (index, part) in parts.iter().enumerate() {
        if !part.is_empty() {
            output.push(TypedTimelineItem::FinalText { text: part.clone() });
        }
        if let Some(event) = event_sequences
            .get(index)
            .and_then(|sequence| events.get(sequence).copied())
        {
            push_context(&mut output, event, "exact");
        }
    }
    output
}

/// 从 cleanup.pb.enc 中经过校验的字节偏移恢复 context 在 cleaned_text 中的位置。
/// 旧 artifact、未知 sequence 或损坏偏移返回 None，由调用方使用 separate 降级。
pub fn compose_persisted_cleanup_timeline(
    cleaned_text: &str,
    placements: &[CleanupContextPlacement],
    context: &ClipboardContextFile,
) -> Option<Vec<TypedTimelineItem>> {
    if placements.len() != context.events.len() {
        return None;
    }
    let events: std::collections::HashMap<u32, &ContextEvent> = context
        .events
        .iter()
        .map(|event| (event.sequence, event))
        .collect();
    if events.len() != context.events.len() {
        return None;
    }

    let mut parts = Vec::with_capacity(placements.len() + 1);
    let mut event_sequences = Vec::with_capacity(placements.len());
    let mut cursor = 0_usize;
    let mut seen = std::collections::HashSet::with_capacity(placements.len());
    for placement in placements {
        let offset = usize::try_from(placement.byte_offset).ok()?;
        if offset < cursor
            || offset > cleaned_text.len()
            || !cleaned_text.is_char_boundary(offset)
            || !seen.insert(placement.event_sequence)
            || !events.contains_key(&placement.event_sequence)
        {
            return None;
        }
        parts.push(cleaned_text[cursor..offset].to_owned());
        event_sequences.push(placement.event_sequence);
        cursor = offset;
    }
    parts.push(cleaned_text[cursor..].to_owned());
    Some(compose_cleanup_timeline(&parts, &event_sequences, context))
}

/// cleanup cache 丢失时的降级布局：最终正文与 context 明确分离，不伪造正文位置。
pub fn compose_separate_context(context: &ClipboardContextFile) -> Vec<TypedTimelineItem> {
    context
        .events
        .iter()
        .map(|event| {
            let mut items = Vec::new();
            push_context(&mut items, event, "separate");
            items
        })
        .flatten()
        .collect()
}

/// 生成独立 context section 的安全纯文本/HTML，正文不与 context 拼成一个 timeline。
pub fn render_separate_context(items: &[TypedTimelineItem]) -> (String, String) {
    if items.is_empty() {
        return (String::new(), String::new());
    }
    let (plain, html) = render_typed_timeline(items);
    (
        format!("\n\nClipboard Context:\n{plain}"),
        format!("<br><br><strong>Clipboard Context:</strong><br>{html}"),
    )
}

fn event_before_ms(event: &ContextEvent, boundary_ms: i64) -> bool {
    (event.sample_offset as u128 * 1000)
        < (boundary_ms.max(0) as u128 * event.source_sample_rate.max(1) as u128)
}

fn push_text(output: &mut Vec<TypedTimelineItem>, text: String, speaker: String) {
    if let Some(TypedTimelineItem::Transcript {
        text: previous,
        speaker: previous_speaker,
    }) = output.last_mut()
    {
        if *previous_speaker == speaker {
            previous.push_str(&text);
            return;
        }
    }
    output.push(TypedTimelineItem::Transcript { text, speaker });
}

/// 上下文事件 → 对应 7-variant 展示项。文件/图片事件的资源按 `absolute_paths` 派生为
/// 资源列表；打开仍使用逻辑引用，路径仅用于 root-only 工作台展示和复制。
fn push_context(output: &mut Vec<TypedTimelineItem>, event: &ContextEvent, placement: &str) {
    let sequence = event.sequence;
    let captured_at_ms = ((event.sample_offset as u128 * 1000
        + u128::from(event.source_sample_rate.max(1)) / 2)
        / u128::from(event.source_sample_rate.max(1)))
    .min(i64::MAX as u128) as i64;
    let placement = placement.to_owned();
    let item = match context_display(event) {
        ContextDisplay::Plain { text } => TypedTimelineItem::ContextText {
            sequence,
            captured_at_ms,
            placement,
            text,
        },
        ContextDisplay::Rich {
            plain_text,
            sanitized_html,
        } => TypedTimelineItem::ContextRichText {
            sequence,
            captured_at_ms,
            placement,
            plain_text,
            sanitized_html,
        },
        ContextDisplay::Link { url } => TypedTimelineItem::ContextLink {
            sequence,
            captured_at_ms,
            placement,
            url,
        },
        ContextDisplay::File { .. } => TypedTimelineItem::ContextFile {
            sequence,
            captured_at_ms,
            placement,
            resources: context_resources(event),
        },
        ContextDisplay::Image { .. } => TypedTimelineItem::ContextImage {
            sequence,
            captured_at_ms,
            placement,
            resources: context_resources(event),
        },
    };
    output.push(item);
}

/// 从 `absolute_paths` 派生工作台资源展示：保留绝对路径供所见即所得复制，
/// 但普通会话 DTO 仍不会携带该结构。
fn context_resources(event: &ContextEvent) -> Vec<ContextResourceDisplay> {
    event
        .absolute_paths
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            let path = std::path::Path::new(raw);
            let display_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("resource")
                .to_owned();
            let mime_type = match path
                .extension()
                .and_then(|value| value.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("png") => "image/png",
                Some("jpg" | "jpeg") => "image/jpeg",
                Some("gif") => "image/gif",
                Some("pdf") => "application/pdf",
                Some("txt" | "md") => "text/plain",
                _ => "application/octet-stream",
            }
            .to_owned();
            // Keep the display state aligned with the same baseline file policy used by
            // resolve/open. The resolver still re-checks the account/cache boundary and
            // identity at action time, because this is only a read-time snapshot.
            let available = crate::resource::validate_openable_file(raw).is_ok();
            ContextResourceDisplay {
                index,
                path: raw.to_owned(),
                display_name,
                mime_type,
                available,
            }
        })
        .collect()
}

/// 最小 HTML 转义（文本节点级安全）。ASR 文本与系统标记经本函数转义后嵌入 HTML。
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use seasnail_proto::seasnail::v1::{
        ClipboardContextFile, ContextEvent, ContextEventKind, TranscriptFile, TranscriptUnit,
        UnitGranularity,
    };

    fn placement_transcript(ranges: &[(Option<i64>, Option<i64>)]) -> TranscriptFile {
        TranscriptFile {
            units: ranges
                .iter()
                .enumerate()
                .map(|(index, (start_ms, end_ms))| TranscriptUnit {
                    sequence: index as u32,
                    start_ms: *start_ms,
                    end_ms: *end_ms,
                    text: ((b'a' + index as u8) as char).to_string(),
                    speaker: String::new(),
                    confidence: None,
                    granularity: UnitGranularity::Segment as i32,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn placement_event(sample_offset: u64) -> ContextEvent {
        ContextEvent {
            source_sample_rate: 1000,
            sample_offset,
            ..Default::default()
        }
    }

    #[test]
    fn placement_slot_covers_boundaries_gaps_and_no_timestamp_fallback() {
        let transcript = placement_transcript(&[(Some(10), Some(20)), (Some(30), Some(40))]);
        assert_eq!(
            placement_slot(&transcript, &placement_event(5)),
            PlacementSlot {
                boundary: 0,
                precision: PlacementPrecision::Exact
            }
        );
        assert_eq!(
            placement_slot(&transcript, &placement_event(15)),
            PlacementSlot {
                boundary: 1,
                precision: PlacementPrecision::Exact
            }
        );
        assert_eq!(
            placement_slot(&transcript, &placement_event(25)),
            PlacementSlot {
                boundary: 1,
                precision: PlacementPrecision::Exact
            }
        );
        assert_eq!(
            placement_slot(&transcript, &placement_event(50)),
            PlacementSlot {
                boundary: 2,
                precision: PlacementPrecision::Exact
            }
        );

        let untimed = placement_transcript(&[(None, None)]);
        assert_eq!(
            placement_slot(&untimed, &placement_event(15)),
            PlacementSlot {
                boundary: 1,
                precision: PlacementPrecision::Fallback
            }
        );
    }

    #[test]
    fn placement_slot_marks_overlapping_units_approximate_and_uses_last_started() {
        let transcript = placement_transcript(&[(Some(0), Some(100)), (Some(50), Some(150))]);
        assert_eq!(
            placement_slot(&transcript, &placement_event(75)),
            PlacementSlot {
                boundary: 2,
                precision: PlacementPrecision::Approximate
            }
        );
    }

    #[test]
    fn typed_timeline_keeps_context_in_source_order_without_mutating_body() {
        let transcript = TranscriptFile {
            schema_version: 2,
            session_id: "s".into(),
            account_id: "a".into(),
            duration_ms: 2000,
            full_text: "ab".into(),
            units: vec![
                TranscriptUnit {
                    sequence: 0,
                    start_ms: Some(0),
                    end_ms: Some(1000),
                    text: "a".into(),
                    speaker: "A".into(),
                    confidence: None,
                    granularity: UnitGranularity::Segment as i32,
                },
                TranscriptUnit {
                    sequence: 1,
                    start_ms: Some(1000),
                    end_ms: Some(2000),
                    text: "b".into(),
                    speaker: "A".into(),
                    confidence: None,
                    granularity: UnitGranularity::Segment as i32,
                },
            ],
            ..Default::default()
        };
        let context = ClipboardContextFile {
            schema_version: 1,
            session_id: "s".into(),
            capture_id: "c".into(),
            events: vec![ContextEvent {
                sequence: 1,
                source_sample_rate: 1000,
                sample_offset: 500,
                kind: ContextEventKind::ContextEventPlainText as i32,
                plain_text: "x".into(),
                html_fragment: String::new(),
                absolute_paths: vec![],
            }],
        };
        let items = compose_typed_timeline(&transcript, &context);
        assert_eq!(
            items[0],
            TypedTimelineItem::Transcript {
                text: "a".into(),
                speaker: "A".into()
            }
        );
        assert!(matches!(
            items[1],
            TypedTimelineItem::ContextText { sequence: 1, .. }
        ));
        assert_eq!(
            items[2],
            TypedTimelineItem::Transcript {
                text: "b".into(),
                speaker: "A".into()
            }
        );
        assert_eq!(transcript.full_text, "ab");
    }

    #[test]
    fn workspace_resource_display_retains_absolute_path_for_copy() {
        let event = ContextEvent {
            absolute_paths: vec!["/Users/example/Documents/plan.pdf".into()],
            ..Default::default()
        };
        let resources = context_resources(&event);
        assert_eq!(resources[0].path, "/Users/example/Documents/plan.pdf");
        assert_eq!(resources[0].display_name, "plan.pdf");
    }

    #[test]
    fn typed_timeline_render_injects_file_and_image_absolute_paths() {
        let items = vec![
            TypedTimelineItem::ContextFile {
                sequence: 1,
                captured_at_ms: 0,
                placement: "timed".into(),
                resources: vec![
                    ContextResourceDisplay {
                        index: 0,
                        path: "/Users/example/Documents/plan & notes.pdf".into(),
                        display_name: "plan & notes.pdf".into(),
                        mime_type: "application/pdf".into(),
                        available: true,
                    },
                    ContextResourceDisplay {
                        index: 1,
                        path: "/Users/example/Documents/data.csv".into(),
                        display_name: "data.csv".into(),
                        mime_type: "application/octet-stream".into(),
                        available: true,
                    },
                ],
            },
            TypedTimelineItem::ContextImage {
                sequence: 2,
                captured_at_ms: 1,
                placement: "timed".into(),
                resources: vec![ContextResourceDisplay {
                    index: 0,
                    path: "/Users/example/Pictures/capture.png".into(),
                    display_name: "capture.png".into(),
                    mime_type: "image/png".into(),
                    available: true,
                }],
            },
        ];

        let (plain, html) = render_typed_timeline(&items);
        assert!(plain.contains("（剪贴板上下文：/Users/example/Documents/plan & notes.pdf）"));
        assert!(plain.contains("\n（剪贴板上下文：/Users/example/Documents/data.csv）"));
        assert!(plain.contains("（剪贴板上下文：/Users/example/Pictures/capture.png）"));
        assert!(!plain.contains("剪贴板上下文：资源"));
        assert!(html.contains("plan &amp; notes.pdf"));
        assert!(html.contains("<br>"));
        assert!(!html.contains("plan & notes.pdf"));
    }

    #[test]
    fn typed_timeline_keeps_untimed_body() {
        let transcript = TranscriptFile {
            schema_version: 2,
            session_id: "s".into(),
            account_id: "a".into(),
            full_text: "无时间正文".into(),
            units: vec![TranscriptUnit {
                sequence: 0,
                start_ms: None,
                end_ms: None,
                text: "无时间正文".into(),
                speaker: String::new(),
                confidence: None,
                granularity: UnitGranularity::Untimed as i32,
            }],
            ..Default::default()
        };
        let context = ClipboardContextFile {
            schema_version: 1,
            session_id: "s".into(),
            capture_id: "c".into(),
            events: vec![],
        };
        assert_eq!(
            compose_typed_timeline(&transcript, &context),
            vec![TypedTimelineItem::Transcript {
                text: "无时间正文".into(),
                speaker: String::new()
            }]
        );
    }

    #[test]
    fn typed_timeline_appends_events_after_untimed_body_with_fallback_placement() {
        let transcript = TranscriptFile {
            schema_version: 2,
            session_id: "s".into(),
            account_id: "a".into(),
            full_text: "无时间正文".into(),
            units: vec![TranscriptUnit {
                sequence: 0,
                start_ms: None,
                end_ms: None,
                text: "无时间正文".into(),
                speaker: String::new(),
                confidence: None,
                granularity: UnitGranularity::Untimed as i32,
            }],
            ..Default::default()
        };
        let context = ClipboardContextFile {
            schema_version: 1,
            session_id: "s".into(),
            capture_id: "c".into(),
            events: vec![
                ContextEvent {
                    sequence: 2,
                    source_sample_rate: 1000,
                    sample_offset: 800,
                    kind: ContextEventKind::ContextEventPlainText as i32,
                    plain_text: "晚到".into(),
                    html_fragment: String::new(),
                    absolute_paths: vec![],
                },
                ContextEvent {
                    sequence: 1,
                    source_sample_rate: 1000,
                    sample_offset: 300,
                    kind: ContextEventKind::ContextEventPlainText as i32,
                    plain_text: "早到".into(),
                    html_fragment: String::new(),
                    absolute_paths: vec![],
                },
            ],
        };
        let items = compose_typed_timeline(&transcript, &context);
        // 正文在前，上下文按事件时间升序置后并标记 fallback。
        assert!(
            matches!(items.first(), Some(TypedTimelineItem::Transcript { text, .. }) if text == "无时间正文")
        );
        let labels: Vec<_> = items
            .iter()
            .filter_map(|item| match item {
                TypedTimelineItem::ContextText {
                    text, placement, ..
                } => Some((placement.as_str(), text.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(labels, vec![("fallback", "早到"), ("fallback", "晚到")]);
    }

    #[test]
    fn typed_timeline_handles_zero_duration_unit_without_containing_the_event() {
        // 半开区间 [start,end) 下零时长单元不“包含”任何事件：事件不得落入其内部。
        let transcript = TranscriptFile {
            schema_version: 2,
            session_id: "s".into(),
            account_id: "a".into(),
            duration_ms: 1500,
            full_text: "ab".into(),
            units: vec![
                TranscriptUnit {
                    sequence: 0,
                    start_ms: Some(0),
                    end_ms: Some(1000),
                    text: "a".into(),
                    speaker: String::new(),
                    confidence: None,
                    granularity: UnitGranularity::Segment as i32,
                },
                TranscriptUnit {
                    sequence: 1,
                    start_ms: Some(1000),
                    end_ms: Some(1000),
                    text: "b".into(),
                    speaker: String::new(),
                    confidence: None,
                    granularity: UnitGranularity::TimedText as i32,
                },
            ],
            ..Default::default()
        };
        let context = ClipboardContextFile {
            schema_version: 1,
            session_id: "s".into(),
            capture_id: "c".into(),
            events: vec![ContextEvent {
                sequence: 9,
                source_sample_rate: 1000,
                sample_offset: 1000,
                kind: ContextEventKind::ContextEventPlainText as i32,
                plain_text: "x".into(),
                html_fragment: String::new(),
                absolute_paths: vec![],
            }],
        };
        let items = compose_typed_timeline(&transcript, &context);
        // 同 speaker 的 a/b 合并为一个 run；事件落在零时长单元之后。
        assert!(
            matches!(items.first(), Some(TypedTimelineItem::Transcript { text, .. }) if text == "ab")
        );
        assert!(
            matches!(items.last(), Some(TypedTimelineItem::ContextText { sequence: 9, placement, .. }) if placement == "exact")
        );
    }

    #[test]
    fn typed_timeline_places_overlap_after_last_started_unit_and_marks_approximate() {
        let transcript = TranscriptFile {
            schema_version: 2,
            session_id: "s".into(),
            account_id: "a".into(),
            duration_ms: 1500,
            full_text: "ab".into(),
            units: vec![
                TranscriptUnit {
                    sequence: 0,
                    start_ms: Some(0),
                    end_ms: Some(1000),
                    text: "a".into(),
                    speaker: String::new(),
                    confidence: None,
                    granularity: UnitGranularity::Segment as i32,
                },
                TranscriptUnit {
                    sequence: 1,
                    start_ms: Some(500),
                    end_ms: Some(1500),
                    text: "b".into(),
                    speaker: String::new(),
                    confidence: None,
                    granularity: UnitGranularity::Segment as i32,
                },
            ],
            ..Default::default()
        };
        let context = ClipboardContextFile {
            schema_version: 1,
            session_id: "s".into(),
            capture_id: "c".into(),
            events: vec![ContextEvent {
                sequence: 7,
                source_sample_rate: 1000,
                sample_offset: 750,
                kind: ContextEventKind::ContextEventPlainText as i32,
                plain_text: "x".into(),
                html_fragment: String::new(),
                absolute_paths: vec![],
            }],
        };
        let items = compose_typed_timeline(&transcript, &context);
        assert_eq!(
            items[0],
            TypedTimelineItem::Transcript {
                text: "ab".into(),
                speaker: String::new()
            }
        );
        assert!(
            matches!(items[1], TypedTimelineItem::ContextText { sequence: 7, captured_at_ms: 750, ref placement, .. } if placement == "approximate")
        );
    }

    #[test]
    fn typed_timeline_uses_rational_order_and_half_open_boundaries() {
        let transcript = TranscriptFile {
            schema_version: 2,
            session_id: "s".into(),
            account_id: "a".into(),
            duration_ms: 2000,
            full_text: "ab".into(),
            units: vec![
                TranscriptUnit {
                    sequence: 0,
                    start_ms: Some(0),
                    end_ms: Some(1000),
                    text: "a".into(),
                    speaker: String::new(),
                    confidence: None,
                    granularity: UnitGranularity::Segment as i32,
                },
                TranscriptUnit {
                    sequence: 1,
                    start_ms: Some(1000),
                    end_ms: Some(2000),
                    text: "b".into(),
                    speaker: String::new(),
                    confidence: None,
                    granularity: UnitGranularity::Segment as i32,
                },
            ],
            ..Default::default()
        };
        let context = ClipboardContextFile {
            schema_version: 1,
            session_id: "s".into(),
            capture_id: "c".into(),
            events: vec![
                ContextEvent {
                    sequence: 2,
                    source_sample_rate: 2000,
                    sample_offset: 2,
                    kind: ContextEventKind::ContextEventPlainText as i32,
                    plain_text: "same-b".into(),
                    html_fragment: String::new(),
                    absolute_paths: vec![],
                },
                ContextEvent {
                    sequence: 1,
                    source_sample_rate: 1000,
                    sample_offset: 1,
                    kind: ContextEventKind::ContextEventPlainText as i32,
                    plain_text: "same-a".into(),
                    html_fragment: String::new(),
                    absolute_paths: vec![],
                },
                ContextEvent {
                    sequence: 3,
                    source_sample_rate: 1000,
                    sample_offset: 1000,
                    kind: ContextEventKind::ContextEventPlainText as i32,
                    plain_text: "boundary".into(),
                    html_fragment: String::new(),
                    absolute_paths: vec![],
                },
            ],
        };
        let items = compose_typed_timeline(&transcript, &context);
        let labels: Vec<_> = items
            .iter()
            .filter_map(|item| match item {
                TypedTimelineItem::ContextText { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(labels, vec!["same-a", "same-b", "boundary"]);
        assert!(
            matches!(items.last(), Some(TypedTimelineItem::ContextText { text, .. }) if text == "boundary")
        );
    }

    #[test]
    fn context_display_only_promotes_complete_http_url_and_hides_paths() {
        let mut event = ContextEvent {
            sequence: 1,
            source_sample_rate: 1000,
            sample_offset: 0,
            kind: ContextEventKind::ContextEventPlainText as i32,
            plain_text: "https://example.com/a".into(),
            html_fragment: String::new(),
            absolute_paths: vec![],
        };
        assert_eq!(
            context_display(&event),
            ContextDisplay::Link {
                url: "https://example.com/a".into()
            }
        );
        event.plain_text = "  https://example.com/trimmed  ".into();
        assert_eq!(
            context_display(&event),
            ContextDisplay::Link {
                url: "https://example.com/trimmed".into()
            }
        );
        event.plain_text = "see https://example.com".into();
        assert_eq!(
            context_display(&event),
            ContextDisplay::Plain {
                text: "see https://example.com".into()
            }
        );
        for malformed in [
            "https://",
            "https:///path",
            "file:///tmp/a",
            "https://exa mple.com",
        ] {
            event.plain_text = malformed.into();
            assert!(matches!(
                context_display(&event),
                ContextDisplay::Plain { .. }
            ));
        }
        event.kind = ContextEventKind::ContextEventFiles as i32;
        event.plain_text.clear();
        event.absolute_paths = vec!["/private/a.txt".into()];
        assert_eq!(context_display(&event), ContextDisplay::File { count: 1 });
        event.kind = ContextEventKind::ContextEventRichText as i32;
        event.html_fragment = "<a href=\"https://example.com\">x</a><b>y</b>".into();
        assert!(
            matches!(context_display(&event), ContextDisplay::Rich { sanitized_html, .. } if !sanitized_html.contains("<a") && sanitized_html.contains("<b>y</b>"))
        );
    }

    #[test]
    fn consolidates_five_hundred_adjacent_text_units() {
        let transcript = TranscriptFile {
            schema_version: 2,
            session_id: "s".into(),
            full_text: "x".repeat(500),
            units: (0..500)
                .map(|sequence| TranscriptUnit {
                    sequence,
                    start_ms: Some(i64::from(sequence)),
                    end_ms: Some(i64::from(sequence + 1)),
                    text: "x".into(),
                    speaker: String::new(),
                    confidence: None,
                    granularity: UnitGranularity::TimedText as i32,
                })
                .collect(),
            ..Default::default()
        };
        let items = compose_typed_timeline(&transcript, &ClipboardContextFile::default());
        assert_eq!(items.len(), 1);
        assert!(
            matches!(&items[0], TypedTimelineItem::Transcript { text, .. } if text.len() == 500)
        );
    }

    #[test]
    fn typed_timeline_render_strips_script_and_event_attrs_from_rich_context() {
        // 生产渲染路径对富文本上下文做 ammonia sanitize：剥离 <script> 与事件属性（onerror 等）。
        let transcript = TranscriptFile {
            schema_version: 2,
            session_id: "s".into(),
            account_id: "a".into(),
            duration_ms: 1000,
            full_text: "a".into(),
            units: vec![TranscriptUnit {
                sequence: 0,
                start_ms: Some(0),
                end_ms: Some(1000),
                text: "a".into(),
                speaker: String::new(),
                confidence: None,
                granularity: UnitGranularity::Segment as i32,
            }],
            ..Default::default()
        };
        let context = ClipboardContextFile {
            schema_version: 1,
            session_id: "s".into(),
            capture_id: "c".into(),
            events: vec![ContextEvent {
                sequence: 1,
                source_sample_rate: 1000,
                sample_offset: 500,
                kind: ContextEventKind::ContextEventRichText as i32,
                plain_text: "富文本".into(),
                html_fragment: "<script>alert(1)</script><b>ok</b><img src=x onerror=alert(1)>"
                    .into(),
                absolute_paths: vec![],
            }],
        };
        let items = compose_typed_timeline(&transcript, &context);
        let (_plain, html) = render_typed_timeline(&items);
        assert!(html.contains("<b>ok</b>"), "保留安全标签: {html}");
        assert!(!html.contains("<script"), "剥离 script: {html}");
        assert!(!html.contains("onerror"), "剥离事件属性: {html}");
    }

    #[test]
    fn rich_context_strips_external_resources_before_display_and_injection() {
        let event = ContextEvent {
            sequence: 1,
            source_sample_rate: 1_000,
            sample_offset: 0,
            kind: ContextEventKind::ContextEventRichText as i32,
            plain_text: "safe text".into(),
            html_fragment: "<p>safe</p><img src=\"https://tracker.example/pixel.png\"><blockquote cite=\"https://tracker.example/source\">quote</blockquote>".into(),
            absolute_paths: vec![],
        };
        let ContextDisplay::Rich { sanitized_html, .. } = context_display(&event) else {
            panic!("rich context expected");
        };
        assert!(sanitized_html.contains("<p>safe</p>"));
        assert!(!sanitized_html.contains("<img"));
        assert!(!sanitized_html.contains("tracker.example"));
    }

    #[test]
    fn persisted_cleanup_placements_restore_unicode_and_reject_invalid_mapping() {
        let context = ClipboardContextFile {
            schema_version: 1,
            session_id: "s".into(),
            capture_id: "c".into(),
            events: vec![
                ContextEvent {
                    sequence: 7,
                    source_sample_rate: 1_000,
                    sample_offset: 100,
                    kind: ContextEventKind::ContextEventPlainText as i32,
                    plain_text: "first".into(),
                    ..Default::default()
                },
                ContextEvent {
                    sequence: 8,
                    source_sample_rate: 1_000,
                    sample_offset: 200,
                    kind: ContextEventKind::ContextEventPlainText as i32,
                    plain_text: "second".into(),
                    ..Default::default()
                },
            ],
        };
        let placements = vec![
            CleanupContextPlacement {
                event_sequence: 7,
                byte_offset: 3,
            },
            CleanupContextPlacement {
                event_sequence: 8,
                byte_offset: 4,
            },
        ];
        let items = compose_persisted_cleanup_timeline("你a好", &placements, &context).unwrap();
        assert!(matches!(&items[0], TypedTimelineItem::FinalText { text } if text == "你"));
        assert!(matches!(
            &items[1],
            TypedTimelineItem::ContextText { sequence: 7, .. }
        ));
        assert!(matches!(&items[2], TypedTimelineItem::FinalText { text } if text == "a"));
        assert!(matches!(
            &items[3],
            TypedTimelineItem::ContextText { sequence: 8, .. }
        ));
        assert!(matches!(&items[4], TypedTimelineItem::FinalText { text } if text == "好"));

        let same_start = vec![
            CleanupContextPlacement {
                event_sequence: 7,
                byte_offset: 0,
            },
            CleanupContextPlacement {
                event_sequence: 8,
                byte_offset: 0,
            },
        ];
        let items = compose_persisted_cleanup_timeline("你a好", &same_start, &context).unwrap();
        assert!(matches!(
            &items[0],
            TypedTimelineItem::ContextText { sequence: 7, .. }
        ));
        assert!(matches!(
            &items[1],
            TypedTimelineItem::ContextText { sequence: 8, .. }
        ));

        let same_end = same_start
            .into_iter()
            .map(|placement| CleanupContextPlacement {
                byte_offset: "你a好".len() as u64,
                ..placement
            })
            .collect::<Vec<_>>();
        let items = compose_persisted_cleanup_timeline("你a好", &same_end, &context).unwrap();
        assert!(
            matches!(items.first(), Some(TypedTimelineItem::FinalText { text }) if text == "你a好")
        );
        assert!(matches!(
            items.last(),
            Some(TypedTimelineItem::ContextText { sequence: 8, .. })
        ));

        let mut invalid = placements.clone();
        invalid[0].byte_offset = 1;
        assert!(compose_persisted_cleanup_timeline("你a好", &invalid, &context).is_none());
        invalid = placements;
        invalid[1].event_sequence = 99;
        assert!(compose_persisted_cleanup_timeline("你a好", &invalid, &context).is_none());
        assert!(compose_persisted_cleanup_timeline("你a好", &[], &context).is_none());
    }
}
