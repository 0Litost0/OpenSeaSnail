//! Conservative, native-only extraction of dictionary candidates from a post-paste edit.
//!
//! Target field contents must never leave the desktop process. The daemon receives only the
//! resulting candidates and applies the authoritative dictionary validation again.

use std::collections::HashMap;
use unicode_segmentation::UnicodeSegmentation;

const MAX_WINDOW_DELTA: usize = 4;
const MIN_REGION_OVERLAP: f32 = 0.30;
const MAX_REPLACEMENT_DISTANCE: f32 = 0.65;
const MAX_ORIGINAL_BYTES: usize = 64 * 1024;
const MAX_FIELD_BYTES: usize = 128 * 1024;
const MAX_ORIGINAL_TOKENS: usize = 256;
const MAX_FIELD_TOKENS: usize = 384;

#[derive(Clone, Debug)]
struct Token {
    text: String,
    start: usize,
    end: usize,
}

pub(crate) fn extract_candidates(original: &str, field_value: &str) -> Vec<String> {
    if original.trim().is_empty()
        || field_value.trim().is_empty()
        || original == field_value
        || original.len() > MAX_ORIGINAL_BYTES
        || field_value.len() > MAX_FIELD_BYTES
    {
        return Vec::new();
    }
    let original_tokens = tokenize(original);
    let field_tokens = tokenize(field_value);
    if original_tokens.is_empty()
        || field_tokens.is_empty()
        || original_tokens.len() > MAX_ORIGINAL_TOKENS
        || field_tokens.len() > MAX_FIELD_TOKENS
    {
        return Vec::new();
    }
    let Some((region_start, region_end)) = locate_edited_region(&original_tokens, &field_tokens)
    else {
        return Vec::new();
    };
    let edited = &field_tokens[region_start..region_end];
    let groups = changed_groups(&original_tokens, edited);
    if groups.is_empty() {
        return Vec::new();
    }
    let changed_original = groups
        .iter()
        .map(|group| group.removed.len())
        .sum::<usize>();
    if original_tokens.len() > 1 && changed_original * 2 > original_tokens.len() {
        return Vec::new();
    }

    let mut results = Vec::new();
    for group in groups {
        // Additions, deletions and many-to-many rewrites are not dictionary corrections.
        if group.removed.is_empty() || group.inserted.is_empty() {
            return Vec::new();
        }
        let mut old = token_slice(original, &original_tokens, &group.removed);
        let mut new = token_slice(field_value, edited, &group.inserted);
        if old.to_lowercase() == new.to_lowercase() || new.trim().is_empty() {
            continue;
        }
        let mut old_graphemes = old.graphemes(true).collect::<Vec<_>>();
        let mut new_graphemes = new.graphemes(true).collect::<Vec<_>>();
        let mut max_len = old_graphemes.len().max(new_graphemes.len());
        let mut distance = levenshtein(&old_graphemes, &new_graphemes);
        // A one-character CJK typo has distance 1/1 after token diffing. Include one unchanged
        // neighboring grapheme before evaluating it, rather than allowing every unrelated short
        // CJK replacement through a blanket exception.
        if max_len > 0
            && distance as f32 / max_len as f32 > MAX_REPLACEMENT_DISTANCE
            && old.chars().filter(|ch| ch.is_alphanumeric()).all(is_cjk)
            && new.chars().filter(|ch| ch.is_alphanumeric()).all(is_cjk)
        {
            let old_first = group.removed[0];
            let new_first = group.inserted[0];
            if old_first > 0
                && new_first > 0
                && token_eq(&original_tokens[old_first - 1], &edited[new_first - 1])
                && original_tokens[old_first - 1].text.chars().all(is_cjk)
            {
                old = &original[original_tokens[old_first - 1].start
                    ..original_tokens[*group.removed.last().unwrap()].end];
                new = &field_value
                    [edited[new_first - 1].start..edited[*group.inserted.last().unwrap()].end];
                old_graphemes = old.graphemes(true).collect();
                new_graphemes = new.graphemes(true).collect();
                max_len = old_graphemes.len().max(new_graphemes.len());
                distance = levenshtein(&old_graphemes, &new_graphemes);
            }
        }
        if max_len == 0 || distance as f32 / max_len as f32 > MAX_REPLACEMENT_DISTANCE {
            continue;
        }
        let candidate = new.trim().to_string();
        if !candidate.chars().any(char::is_alphanumeric)
            || results
                .iter()
                .any(|existing: &String| existing.to_lowercase() == candidate.to_lowercase())
        {
            continue;
        }
        results.push(candidate);
        if results.len() == 32 {
            break;
        }
    }
    results
}

fn locate_edited_region(original: &[Token], field: &[Token]) -> Option<(usize, usize)> {
    if field.len() * 2 <= original.len().saturating_mul(3) {
        return Some((0, field.len()));
    }
    // Estimate the pasted region from the most common offset of equal tokens. This keeps the
    // search bounded to a small neighborhood instead of running an LCS for every possible window.
    let mut offsets = HashMap::<isize, usize>::new();
    for (left_index, left) in original.iter().enumerate() {
        for (right_index, right) in field.iter().enumerate() {
            if token_eq(left, right) {
                *offsets
                    .entry(right_index as isize - left_index as isize)
                    .or_default() += 1;
            }
        }
    }
    let estimated_start = offsets
        .into_iter()
        .max_by_key(|(offset, count)| (*count, std::cmp::Reverse(offset.unsigned_abs())))?
        .0
        .max(0) as usize;
    let min_len = original.len().saturating_sub(MAX_WINDOW_DELTA).max(1);
    let max_len = field
        .len()
        .min(original.len().saturating_add(MAX_WINDOW_DELTA));
    let mut best: Option<(usize, usize, usize)> = None;
    let start_min = estimated_start.saturating_sub(MAX_WINDOW_DELTA);
    let start_max = field
        .len()
        .saturating_sub(min_len)
        .min(estimated_start.saturating_add(MAX_WINDOW_DELTA));
    for window_len in min_len..=max_len {
        for start in start_min..=start_max.min(field.len().saturating_sub(window_len)) {
            let score = lcs_len(original, &field[start..start + window_len]);
            if best.is_none_or(|(_, previous_len, previous_score)| {
                score > previous_score
                    || (score == previous_score
                        && window_len.abs_diff(original.len())
                            < previous_len.abs_diff(original.len()))
            }) {
                best = Some((start, window_len, score));
            }
        }
    }
    let (start, len, score) = best?;
    ((score as f32 / original.len() as f32) >= MIN_REGION_OVERLAP).then_some((start, start + len))
}

#[derive(Debug)]
struct ChangeGroup {
    removed: Vec<usize>,
    inserted: Vec<usize>,
}

fn changed_groups(original: &[Token], edited: &[Token]) -> Vec<ChangeGroup> {
    let prefix = original
        .iter()
        .zip(edited)
        .take_while(|(left, right)| token_eq(left, right))
        .count();
    let suffix = original[prefix..]
        .iter()
        .rev()
        .zip(edited[prefix..].iter().rev())
        .take_while(|(left, right)| token_eq(left, right))
        .count();
    let original_middle = prefix..original.len() - suffix;
    let edited_middle = prefix..edited.len() - suffix;
    let middle_has_anchor = original[original_middle.clone()].iter().any(|left| {
        edited[edited_middle.clone()]
            .iter()
            .any(|right| token_eq(left, right))
    });
    if !original_middle.is_empty() && !edited_middle.is_empty() && !middle_has_anchor {
        return vec![ChangeGroup {
            removed: original_middle.collect(),
            inserted: edited_middle.collect(),
        }];
    }

    let mut dp = vec![vec![0_usize; edited.len() + 1]; original.len() + 1];
    for i in 1..=original.len() {
        for j in 1..=edited.len() {
            dp[i][j] = if token_eq(&original[i - 1], &edited[j - 1]) {
                dp[i - 1][j - 1] + 1
            } else {
                dp[i - 1][j].max(dp[i][j - 1])
            };
        }
    }
    enum Step {
        Equal,
        Removed(usize),
        Inserted(usize),
    }
    let mut steps = Vec::new();
    let (mut i, mut j) = (original.len(), edited.len());
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && token_eq(&original[i - 1], &edited[j - 1]) {
            steps.push(Step::Equal);
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            steps.push(Step::Inserted(j - 1));
            j -= 1;
        } else {
            steps.push(Step::Removed(i - 1));
            i -= 1;
        }
    }
    steps.reverse();
    let mut groups = Vec::new();
    let mut current: Option<ChangeGroup> = None;
    for step in steps {
        match step {
            Step::Equal => {
                if let Some(group) = current.take() {
                    groups.push(group);
                }
            }
            Step::Removed(index) => current
                .get_or_insert_with(|| ChangeGroup {
                    removed: Vec::new(),
                    inserted: Vec::new(),
                })
                .removed
                .push(index),
            Step::Inserted(index) => current
                .get_or_insert_with(|| ChangeGroup {
                    removed: Vec::new(),
                    inserted: Vec::new(),
                })
                .inserted
                .push(index),
        }
    }
    if let Some(group) = current {
        groups.push(group);
    }
    groups
}

fn token_slice<'a>(source: &'a str, tokens: &[Token], indexes: &[usize]) -> &'a str {
    let first = &tokens[*indexes.first().expect("non-empty change indexes")];
    let last = &tokens[*indexes.last().expect("non-empty change indexes")];
    &source[first.start..last.end]
}

fn lcs_len(left: &[Token], right: &[Token]) -> usize {
    let mut previous = vec![0_usize; right.len() + 1];
    for left_token in left {
        let mut current = vec![0_usize; right.len() + 1];
        for (index, right_token) in right.iter().enumerate() {
            current[index + 1] = if token_eq(left_token, right_token) {
                previous[index] + 1
            } else {
                previous[index + 1].max(current[index])
            };
        }
        previous = current;
    }
    previous[right.len()]
}

fn token_eq(left: &Token, right: &Token) -> bool {
    left.text.to_lowercase() == right.text.to_lowercase()
}

fn tokenize(value: &str) -> Vec<Token> {
    let graphemes = value.grapheme_indices(true).collect::<Vec<_>>();
    let mut out: Vec<Token> = Vec::new();
    for (index, (start, grapheme)) in graphemes.iter().enumerate() {
        if !grapheme.chars().any(char::is_alphanumeric) {
            continue;
        }
        let end = graphemes
            .get(index + 1)
            .map(|(next, _)| *next)
            .unwrap_or(value.len());
        let cjk = grapheme.chars().any(is_cjk);
        if !cjk {
            if let Some(last) = out.last_mut() {
                let gap = &value[last.end..*start];
                let last_cjk = last.text.chars().any(is_cjk);
                if !last_cjk && gap.is_empty() {
                    last.text.push_str(grapheme);
                    last.end = end;
                    continue;
                }
            }
        }
        out.push(Token {
            text: (*grapheme).to_string(),
            start: *start,
            end,
        });
    }
    out
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF |
        0x3040..=0x30FF | 0xAC00..=0xD7AF)
}

fn levenshtein<T: Eq>(left: &[T], right: &[T]) -> usize {
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    for (i, left_value) in left.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, right_value) in right.iter().enumerate() {
            current.push(if left_value == right_value {
                previous[j]
            } else {
                1 + previous[j].min(previous[j + 1]).min(current[j])
            });
        }
        previous = current;
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_english_word_phrase_cjk_and_mixed_replacements() {
        assert_eq!(
            extract_candidates("ship report", "ship reports"),
            ["reports"]
        );
        assert_eq!(
            extract_candidates("use open whisper today", "use OpenWhispr today"),
            ["OpenWhispr"]
        );
        assert_eq!(extract_candidates("今天海电很好", "今天海淀很好"), ["海淀"]);
        assert_eq!(
            extract_candidates("使用Sea Snale输入", "使用SeaSnail输入"),
            ["SeaSnail"]
        );
    }

    #[test]
    fn locates_the_edited_paste_inside_a_larger_field() {
        assert_eq!(
            extract_candidates(
                "send project atlas report",
                "prefix words send project AtlasX report suffix words"
            ),
            ["AtlasX"]
        );
    }

    #[test]
    fn returns_multiple_local_corrections_as_one_submission_candidate_set() {
        assert_eq!(
            extract_candidates("alpha betta gamma deltta", "alpha beta gamma delta"),
            ["beta", "delta"]
        );
    }

    #[test]
    fn rejects_non_semantic_edits_and_rewrites() {
        assert!(extract_candidates("Hello world", "hello world!").is_empty());
        assert!(extract_candidates("alpha beta", "alpha  beta").is_empty());
        assert!(extract_candidates("Sea Snail", "SeaSnail").is_empty());
        assert!(extract_candidates("one two three four", "five six seven four").is_empty());
        assert!(extract_candidates("ship report", "completely unrelated sentence").is_empty());
        assert!(extract_candidates("alpha beta gamma", "alpha beta gamma extra").is_empty());
        assert!(extract_candidates("alpha beta gamma", "alpha gamma").is_empty());
        assert!(extract_candidates("今天开会讨论", "今天吃饭讨论").is_empty());
        assert!(extract_candidates(&"字".repeat(300), &"词".repeat(300)).is_empty());
        assert!(extract_candidates("short", &"x".repeat(MAX_FIELD_BYTES + 1)).is_empty());
    }

    #[test]
    fn grapheme_tokenization_keeps_combining_sequences_intact() {
        assert_eq!(tokenize("Cafe\u{301}").len(), 1);
        assert_eq!(extract_candidates("Cafe", "Cafe\u{301}"), ["Cafe\u{301}"]);
    }
}
