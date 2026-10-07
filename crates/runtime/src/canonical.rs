//! 后端无关的转写正文归一化。
//!
//! 这里只做一次从正文起点开始的严格、单向对齐：候选单元的文本必须按原样出现在
//! `OpenAiSegments.text` 中，候选之间只允许由正文中已有的空白/标点/符号组成的间隔。
//! 对齐失败时调用方可整体降级到更粗粒度，绝不拼接或改写正文。

use crate::contract::{OpenAiSegment, OpenAiSegments};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    TimedText,
    Segment,
    Untimed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalUnit {
    pub sequence: u32,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub text: String,
    pub speaker: Option<String>,
    pub granularity: Granularity,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalTranscript {
    pub full_text: String,
    pub units: Vec<CanonicalUnit>,
    pub granularity: Granularity,
    pub duration_ms: i64,
    /// Speakers in deterministic first-seen order.  This is deliberately
    /// derived from backend segments, including speakers which have no
    /// time-aligned unit, so the protobuf projection never has to retain the
    /// backend response and re-read it later.
    pub speaker_roster: Vec<String>,
}

#[derive(Debug, Clone)]
struct Candidate<'a> {
    start: f64,
    end: f64,
    text: &'a str,
    speaker: Option<&'a str>,
}

impl CanonicalTranscript {
    /// words 全量可严格对齐时使用词/字级；否则整体降级到 segments；两者都不可用时
    /// 保留完整正文并生成一个无时间单元。
    pub fn from_openai(input: &OpenAiSegments) -> Self {
        Self::from_openai_with_duration(input, 0)
    }

    pub fn from_openai_with_duration(input: &OpenAiSegments, duration_ms: i64) -> Self {
        let full_text = input.text.clone();
        let duration_ms = normalize_duration_ms(duration_ms);
        if let Some(units) = build_units(
            &full_text,
            input.words.iter().map(|w| Candidate {
                start: w.start,
                end: w.end,
                text: &w.text,
                speaker: None,
            }),
            duration_ms,
        ) {
            let mut units = project_sequences(units, Granularity::TimedText);
            project_speakers(&mut units, &input.segments);
            return Self {
                full_text,
                units,
                granularity: Granularity::TimedText,
                duration_ms,
                speaker_roster: speaker_roster(&input.segments),
            };
        }
        if let Some(units) = build_units(
            &full_text,
            input.segments.iter().map(|s| Candidate {
                start: s.start,
                end: s.end,
                text: &s.text,
                speaker: s.speaker.as_deref(),
            }),
            duration_ms,
        ) {
            return Self {
                full_text,
                units: project_sequences(units, Granularity::Segment),
                granularity: Granularity::Segment,
                duration_ms,
                speaker_roster: speaker_roster(&input.segments),
            };
        }
        let units = if full_text.is_empty() {
            Vec::new()
        } else {
            vec![CanonicalUnit {
                sequence: 0,
                start_ms: None,
                end_ms: None,
                text: full_text.clone(),
                speaker: None,
                granularity: Granularity::Untimed,
            }]
        };
        Self {
            full_text,
            units,
            granularity: Granularity::Untimed,
            duration_ms,
            speaker_roster: speaker_roster(&input.segments),
        }
    }
}

fn build_units<'a>(
    full_text: &str,
    candidates: impl Iterator<Item = Candidate<'a>>,
    duration_ms: i64,
) -> Option<Vec<CanonicalUnit>> {
    let candidates: Vec<_> = candidates.collect();
    if candidates.is_empty() || full_text.is_empty() {
        return None;
    }
    let mut spans = Vec::with_capacity(candidates.len());
    let mut cursor = 0usize;
    let mut previous_start = 0.0;
    for candidate in &candidates {
        if candidate.text.is_empty()
            || !candidate.start.is_finite()
            || !candidate.end.is_finite()
            || candidate.start > candidate.end
            || candidate.start < 0.0
            || candidate.start < previous_start
        {
            return None;
        }
        let relative = full_text.get(cursor..)?.find(candidate.text)?;
        let start = cursor + relative;
        let gap = full_text.get(cursor..start)?;
        if !gap
            .chars()
            .all(|c| c.is_whitespace() || !c.is_alphanumeric())
        {
            return None;
        }
        let end = start + candidate.text.len();
        spans.push((start, end));
        cursor = end;
        previous_start = candidate.start;
    }
    // Punctuation and whitespace may legitimately be absent from backend word
    // candidates and are attached to the last visible unit. Missing letters or
    // numbers, however, mean that the candidate timeline is incomplete and must
    // be rejected instead of assigning un-timed text to the final timestamp.
    let trailing = full_text.get(cursor..)?;
    if !trailing
        .chars()
        .all(|c| c.is_whitespace() || !c.is_alphanumeric())
    {
        return None;
    }

    let mut units: Vec<CanonicalUnit> = Vec::with_capacity(candidates.len());
    for (index, candidate) in candidates.iter().enumerate() {
        let start = if index == 0 {
            0
        } else {
            let previous_end = spans[index - 1].1;
            let next_start = spans[index].0;
            let gap = full_text.get(previous_end..next_start)?;
            let mut boundary = next_start;
            for (offset, ch) in gap.char_indices().rev() {
                if ch.is_whitespace() {
                    boundary = previous_end + offset;
                } else {
                    break;
                }
            }
            boundary
        };
        let end = if index + 1 == spans.len() {
            full_text.len()
        } else {
            let next_start = spans[index + 1].0;
            let gap = full_text.get(spans[index].1..next_start)?;
            let mut boundary = next_start;
            for (offset, ch) in gap.char_indices().rev() {
                if ch.is_whitespace() {
                    boundary = spans[index].1 + offset;
                } else {
                    break;
                }
            }
            boundary
        };
        let text = full_text.get(start..end)?.to_owned();
        if text.is_empty() {
            return None;
        }
        let mut start_ms = seconds_to_ms(candidate.start);
        let mut end_ms = seconds_to_ms(candidate.end);
        if duration_ms > 0 {
            if start_ms > duration_ms + 1 || end_ms > duration_ms + 1 {
                return None;
            }
            start_ms = start_ms.min(duration_ms);
            end_ms = end_ms.min(duration_ms);
        }
        if let Some(previous) = units.last_mut() {
            if candidates[index - 1].start.to_bits() == candidate.start.to_bits() {
                previous.text.push_str(&text);
                previous.end_ms = Some(previous.end_ms.unwrap_or(end_ms).max(end_ms));
                if previous.speaker.as_deref() != candidate.speaker {
                    previous.speaker = None;
                }
                continue;
            }
        }
        units.push(CanonicalUnit {
            sequence: index as u32,
            start_ms: Some(start_ms),
            end_ms: Some(end_ms),
            text,
            speaker: candidate.speaker.map(str::to_owned),
            granularity: Granularity::TimedText,
        });
    }
    Some(units)
}

fn project_sequences(
    mut units: Vec<CanonicalUnit>,
    granularity: Granularity,
) -> Vec<CanonicalUnit> {
    for (index, unit) in units.iter_mut().enumerate() {
        unit.sequence = index as u32;
        unit.granularity = granularity;
    }
    units
}

fn seconds_to_ms(seconds: f64) -> i64 {
    (seconds * 1000.0).round() as i64
}

pub fn normalize_duration_ms(duration_ms: i64) -> i64 {
    duration_ms.max(0)
}

fn project_speakers(units: &mut [CanonicalUnit], segments: &[OpenAiSegment]) {
    for unit in units.iter_mut() {
        let (Some(start_ms), Some(end_ms)) = (unit.start_ms, unit.end_ms) else {
            continue;
        };
        let mut best_overlap = 0i64;
        let mut best_speaker = None;
        for segment in segments {
            let seg_start = seconds_to_ms(segment.start);
            let seg_end = seconds_to_ms(segment.end);
            let overlap = (end_ms.min(seg_end) - start_ms.max(seg_start)).max(0);
            if overlap > best_overlap {
                best_overlap = overlap;
                best_speaker = segment.speaker.as_deref();
            }
        }
        unit.speaker = best_speaker.map(str::to_owned);
    }
}

fn speaker_roster(segments: &[OpenAiSegment]) -> Vec<String> {
    let mut roster = Vec::new();
    for speaker in segments
        .iter()
        .filter_map(|segment| segment.speaker.as_deref())
    {
        if !speaker.is_empty() && !roster.iter().any(|known| known == speaker) {
            roster.push(speaker.to_owned());
        }
    }
    roster
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{OpenAiSegment, OpenAiSegments, OpenAiWord};

    fn word(start: f64, end: f64, text: &str) -> OpenAiWord {
        OpenAiWord {
            start,
            end,
            text: text.into(),
        }
    }

    #[test]
    fn preserves_punctuation_spaces_and_repeated_words() {
        let input = OpenAiSegments {
            text: "hello, hello!\n🙂".into(),
            segments: vec![],
            words: vec![
                word(0.0, 0.1, "hello"),
                word(0.2, 0.3, "hello"),
                word(0.4, 0.5, "🙂"),
            ],
        };
        let output = CanonicalTranscript::from_openai(&input);
        assert_eq!(output.granularity, Granularity::TimedText);
        assert_eq!(
            output
                .units
                .iter()
                .map(|u| u.text.as_str())
                .collect::<String>(),
            input.text
        );
        assert_eq!(output.units[0].text, "hello,");
        assert_eq!(output.units[1].text, " hello!");
        assert_eq!(output.units[2].text, "\n🙂");
    }

    #[test]
    fn falls_back_as_a_whole_when_one_word_is_invalid() {
        let input = OpenAiSegments {
            text: "你好世界".into(),
            segments: vec![OpenAiSegment {
                start: 0.0,
                end: 1.0,
                text: "你好世界".into(),
                speaker: None,
            }],
            words: vec![word(0.0, 0.1, "你"), word(0.2, 0.1, "好")],
        };
        let output = CanonicalTranscript::from_openai(&input);
        assert_eq!(output.granularity, Granularity::Segment);
        assert_eq!(output.units.len(), 1);
        assert_eq!(output.units[0].text, input.text);
    }

    #[test]
    fn incomplete_candidate_tail_falls_back_instead_of_inventing_timing() {
        let input = OpenAiSegments {
            text: "hello missing words".into(),
            segments: vec![],
            words: vec![word(0.0, 0.2, "hello")],
        };
        let output = CanonicalTranscript::from_openai(&input);
        assert_eq!(output.granularity, Granularity::Untimed);
        assert_eq!(output.units[0].text, input.text);
        assert!(output.units[0].start_ms.is_none());
    }

    #[test]
    fn falls_back_to_single_untimed_unit_when_segments_also_fail() {
        let input = OpenAiSegments {
            text: "保留完整正文".into(),
            segments: vec![OpenAiSegment {
                start: 2.0,
                end: 1.0,
                text: "保留".into(),
                speaker: None,
            }],
            words: vec![word(0.0, 0.1, "错误")],
        };
        let output = CanonicalTranscript::from_openai(&input);
        assert_eq!(output.granularity, Granularity::Untimed);
        assert_eq!(output.units.len(), 1);
        assert_eq!(output.units[0].text, input.text);
        assert!(output.units[0].start_ms.is_none());
    }

    #[test]
    fn preserves_emoji_zwj_and_nfd_bytes() {
        let text = "👩‍💻 e\u{301}";
        let input = OpenAiSegments {
            text: text.into(),
            segments: vec![],
            words: vec![word(0.0, 0.1, "👩‍💻"), word(0.2, 0.3, "e\u{301}")],
        };
        let output = CanonicalTranscript::from_openai(&input);
        assert_eq!(
            output
                .units
                .iter()
                .map(|u| u.text.as_str())
                .collect::<String>(),
            text
        );
    }

    #[test]
    fn rounds_time_and_projects_max_overlap_speaker() {
        let input = OpenAiSegments {
            text: "ab".into(),
            segments: vec![
                OpenAiSegment {
                    start: 0.0,
                    end: 0.15,
                    text: "a".into(),
                    speaker: Some("A".into()),
                },
                OpenAiSegment {
                    start: 0.10,
                    end: 0.50,
                    text: "ab".into(),
                    speaker: Some("B".into()),
                },
            ],
            words: vec![word(0.049, 0.151, "a"), word(0.251, 0.351, "b")],
        };
        let output = CanonicalTranscript::from_openai_with_duration(&input, -20);
        assert_eq!(output.duration_ms, 0);
        assert_eq!(output.units[0].start_ms, Some(49));
        assert_eq!(output.units[0].end_ms, Some(151));
        assert_eq!(output.units[0].speaker.as_deref(), Some("A"));
        assert_eq!(output.units[1].speaker.as_deref(), Some("B"));
    }

    #[test]
    fn equal_boundary_has_no_overlap() {
        let input = OpenAiSegments {
            text: "a".into(),
            segments: vec![OpenAiSegment {
                start: 0.1,
                end: 0.2,
                text: "a".into(),
                speaker: Some("A".into()),
            }],
            words: vec![word(0.0, 0.1, "a")],
        };
        let output = CanonicalTranscript::from_openai(&input);
        assert!(output.units[0].speaker.is_none());
    }

    #[test]
    fn overlapping_segment_windows_remain_segment_granularity() {
        let input = OpenAiSegments {
            text: "ab".into(),
            segments: vec![
                OpenAiSegment {
                    start: 0.0,
                    end: 0.8,
                    text: "a".into(),
                    speaker: Some("A".into()),
                },
                OpenAiSegment {
                    start: 0.5,
                    end: 1.0,
                    text: "b".into(),
                    speaker: Some("B".into()),
                },
            ],
            words: vec![],
        };
        let output = CanonicalTranscript::from_openai(&input);
        assert_eq!(output.granularity, Granularity::Segment);
        assert_eq!(output.units.len(), 2);
    }

    #[test]
    fn merges_visible_candidates_with_the_same_start_time() {
        let input = OpenAiSegments {
            text: "hello".into(),
            segments: vec![],
            words: vec![word(0.0, 0.1, "he"), word(0.0, 0.1, "llo")],
        };
        let output = CanonicalTranscript::from_openai_with_duration(&input, 1000);
        assert_eq!(output.granularity, Granularity::TimedText);
        assert_eq!(output.units.len(), 1);
        assert_eq!(output.units[0].text, "hello");
    }

    #[test]
    fn invalid_word_time_falls_back_and_one_ms_rounding_overflow_is_clamped() {
        let negative = OpenAiSegments {
            text: "hello".into(),
            segments: vec![OpenAiSegment {
                start: 0.0,
                end: 0.5,
                text: "hello".into(),
                speaker: None,
            }],
            words: vec![word(-0.01, 0.2, "hello")],
        };
        assert_eq!(
            CanonicalTranscript::from_openai_with_duration(&negative, 1000).granularity,
            Granularity::Segment
        );

        let rounded = OpenAiSegments {
            text: "hello".into(),
            segments: vec![],
            words: vec![word(0.0, 1.0006, "hello")],
        };
        let output = CanonicalTranscript::from_openai_with_duration(&rounded, 1000);
        assert_eq!(output.units[0].end_ms, Some(1000));
    }

    #[test]
    fn roster_is_first_seen_and_keeps_unreferenced_speakers() {
        let input = OpenAiSegments {
            text: "hello".into(),
            segments: vec![
                OpenAiSegment {
                    start: 0.0,
                    end: 1.0,
                    text: "hello".into(),
                    speaker: Some("B".into()),
                },
                OpenAiSegment {
                    start: 1.0,
                    end: 2.0,
                    text: "".into(),
                    speaker: Some("A".into()),
                },
                OpenAiSegment {
                    start: 2.0,
                    end: 3.0,
                    text: "".into(),
                    speaker: Some("B".into()),
                },
            ],
            words: vec![],
        };
        let output = CanonicalTranscript::from_openai(&input);
        assert_eq!(output.speaker_roster, vec!["B", "A"]);
    }
}
