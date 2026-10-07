//! Dictionary 领域服务：统一规范化、校验、CSV 与事务语义。

use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;
use seasnail_storage::rusqlite::{Connection, TransactionBehavior};
use seasnail_storage::{DictionaryCursor, DictionaryEntryRow};

use super::CallerContext;

pub const MAX_TERM_SCALARS: usize = 128;
pub const MAX_TERM_BYTES: usize = 512;
pub const MAX_BATCH_TERMS: usize = 1_000;
pub const MAX_CSV_BYTES: usize = 1024 * 1024;
pub const MAX_DICTIONARY_ENTRIES: i64 = 1_000;

#[derive(Debug, thiserror::Error)]
pub enum DictionaryError {
    #[error("dictionary access requires a root token")]
    Forbidden,
    #[error("dictionary term is invalid")]
    InvalidTerm,
    #[error("dictionary request exceeds its item limit")]
    TooManyTerms,
    #[error("dictionary request exceeds its byte limit")]
    RequestTooLarge,
    #[error("dictionary CSV is invalid")]
    InvalidCsv,
    #[error("dictionary capacity would be exceeded")]
    LimitExceeded,
    #[error("dictionary entry conflicts with an existing normalized term")]
    Conflict,
    #[error("dictionary entry not found")]
    NotFound,
    #[error("dictionary storage failure: {0}")]
    Storage(#[from] seasnail_storage::Error),
    #[error("dictionary transaction failure: {0}")]
    Sqlite(#[from] seasnail_storage::rusqlite::Error),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DictionaryEntry {
    pub id: String,
    pub term: String,
    pub source: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<DictionaryEntryRow> for DictionaryEntry {
    fn from(row: DictionaryEntryRow) -> Self {
        Self {
            id: row.id,
            term: row.term,
            source: row.source,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DictionaryMutationResult {
    pub added: Vec<DictionaryEntry>,
    pub promoted: Vec<DictionaryEntry>,
    pub skipped_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DictionaryImportPreview {
    pub parsed_count: usize,
    pub valid_count: usize,
    pub added_count: usize,
    pub promoted_count: usize,
    pub skipped_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DictionaryImportResult {
    pub parsed_count: usize,
    pub mutation: DictionaryMutationResult,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LearningMutationResult {
    pub learning_event_id: Option<String>,
    pub added: Vec<DictionaryEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DictionaryPage {
    pub items: Vec<DictionaryEntry>,
    pub next_cursor: Option<DictionaryCursor>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DictionarySnapshot {
    pub terms: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedDictionaryTerm {
    pub term: String,
    pub normalized_term: String,
}

fn is_ecmascript_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// 对齐 OpenWhispr 的 JavaScript `trim().toLowerCase()`。不折叠内部空白，
/// 不执行 Unicode normalization 或 full case folding。
pub fn normalize_dictionary_term(input: &str) -> Result<NormalizedDictionaryTerm, DictionaryError> {
    let term = input.trim_matches(is_ecmascript_whitespace);
    if term.is_empty()
        || term.chars().count() > MAX_TERM_SCALARS
        || term.len() > MAX_TERM_BYTES
        || term.chars().any(char::is_control)
    {
        return Err(DictionaryError::InvalidTerm);
    }
    static MEANINGFUL: OnceLock<Regex> = OnceLock::new();
    let meaningful = MEANINGFUL.get_or_init(|| {
        Regex::new(r"[\p{L}\p{M}\p{N}\p{S}]").expect("static Unicode category regex must compile")
    });
    if !meaningful.is_match(term) {
        return Err(DictionaryError::InvalidTerm);
    }
    Ok(NormalizedDictionaryTerm {
        term: term.to_owned(),
        normalized_term: term.to_lowercase(),
    })
}

pub(crate) fn is_safe_cleanup_learning_candidate(original: &str, corrected: &str) -> bool {
    let Ok(corrected_term) = normalize_dictionary_term(corrected) else {
        return false;
    };
    let compact = |value: &str| {
        value
            .chars()
            .filter(|character| character.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    if compact(original) == compact(&corrected_term.term) {
        return false;
    }
    let word_count = corrected_term.term.split_whitespace().count();
    if word_count > 6 {
        return false;
    }
    let meaningful_count = corrected_term
        .term
        .chars()
        .filter(|character| character.is_alphanumeric())
        .count();
    let sentence_terminal = corrected_term
        .term
        .chars()
        .last()
        .is_some_and(|character| matches!(character, '.' | '!' | '?' | '。' | '！' | '？'));
    let compact_cjk_sentence = !corrected_term.term.chars().any(char::is_whitespace)
        && meaningful_count > 8
        && corrected_term.term.chars().any(is_cjk_character);
    !compact_cjk_sentence && !(sentence_terminal && (word_count >= 2 || meaningful_count > 8))
}

fn is_cjk_character(character: char) -> bool {
    matches!(character as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF |
        0x3040..=0x30FF | 0xAC00..=0xD7AF)
}

fn require_root(caller: &CallerContext) -> Result<(), DictionaryError> {
    caller
        .is_root()
        .then_some(())
        .ok_or(DictionaryError::Forbidden)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn validate_and_dedupe(
    terms: &[String],
) -> Result<(Vec<NormalizedDictionaryTerm>, usize), DictionaryError> {
    if terms.is_empty() || terms.len() > MAX_BATCH_TERMS {
        return Err(DictionaryError::TooManyTerms);
    }
    let mut seen = HashSet::with_capacity(terms.len());
    let mut normalized = Vec::with_capacity(terms.len());
    let mut duplicates = 0;
    for term in terms {
        let term = normalize_dictionary_term(term)?;
        if seen.insert(term.normalized_term.clone()) {
            normalized.push(term);
        } else {
            duplicates += 1;
        }
    }
    Ok((normalized, duplicates))
}

pub fn normalize_dictionary_query(input: &str) -> Result<Option<String>, DictionaryError> {
    let query = input.trim_matches(is_ecmascript_whitespace);
    if query.is_empty() {
        return Ok(None);
    }
    if query.chars().count() > MAX_TERM_SCALARS
        || query.len() > MAX_TERM_BYTES
        || query.chars().any(char::is_control)
    {
        return Err(DictionaryError::InvalidTerm);
    }
    Ok(Some(query.to_lowercase()))
}

fn validate_rfc4180_syntax(value: &str) -> Result<(), DictionaryError> {
    #[derive(Clone, Copy)]
    enum State {
        Start,
        Unquoted,
        Quoted,
        QuoteClosed,
    }
    let bytes = value.as_bytes();
    let mut state = State::Start;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        match state {
            State::Start => match byte {
                b'"' => state = State::Quoted,
                b',' => {}
                b'\n' => {}
                b'\r' if bytes.get(index + 1) == Some(&b'\n') => index += 1,
                b'\r' => return Err(DictionaryError::InvalidCsv),
                _ => state = State::Unquoted,
            },
            State::Unquoted => match byte {
                b'"' => return Err(DictionaryError::InvalidCsv),
                b',' => state = State::Start,
                b'\n' => state = State::Start,
                b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                    index += 1;
                    state = State::Start;
                }
                b'\r' => return Err(DictionaryError::InvalidCsv),
                _ => {}
            },
            State::Quoted => {
                if byte == b'"' {
                    state = State::QuoteClosed;
                }
            }
            State::QuoteClosed => match byte {
                b'"' => state = State::Quoted,
                b',' => state = State::Start,
                b'\n' => state = State::Start,
                b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                    index += 1;
                    state = State::Start;
                }
                b'\r' => return Err(DictionaryError::InvalidCsv),
                _ => return Err(DictionaryError::InvalidCsv),
            },
        }
        index += 1;
    }
    if matches!(state, State::Quoted) {
        Err(DictionaryError::InvalidCsv)
    } else {
        Ok(())
    }
}

fn parse_csv(csv_text: &str) -> Result<(usize, Vec<String>), DictionaryError> {
    if csv_text.is_empty() {
        return Err(DictionaryError::InvalidCsv);
    }
    if csv_text.len() > MAX_CSV_BYTES {
        return Err(DictionaryError::RequestTooLarge);
    }
    let csv_text = csv_text.strip_prefix('\u{FEFF}').unwrap_or(csv_text);
    validate_rfc4180_syntax(csv_text)?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(false)
        .from_reader(csv_text.as_bytes());
    let headers = reader.headers().map_err(|_| DictionaryError::InvalidCsv)?;
    if headers.len() != 1 || headers.get(0) != Some("term") {
        return Err(DictionaryError::InvalidCsv);
    }
    let mut terms = Vec::new();
    let mut parsed_count = 0;
    for record in reader.records() {
        let record = record.map_err(|_| DictionaryError::InvalidCsv)?;
        if record.len() != 1 {
            return Err(DictionaryError::InvalidCsv);
        }
        parsed_count += 1;
        if parsed_count > MAX_BATCH_TERMS {
            return Err(DictionaryError::InvalidCsv);
        }
        terms.push(record.get(0).unwrap_or_default().to_owned());
    }
    if terms.is_empty() {
        return Err(DictionaryError::InvalidCsv);
    }
    Ok((parsed_count, terms))
}

fn row_for(
    term: &NormalizedDictionaryTerm,
    source: &str,
    learning_event_id: Option<&str>,
    now: i64,
) -> DictionaryEntryRow {
    DictionaryEntryRow {
        id: uuid::Uuid::new_v4().to_string(),
        term: term.term.clone(),
        normalized_term: term.normalized_term.clone(),
        source: source.to_owned(),
        learning_event_id: learning_event_id.map(str::to_owned),
        created_at: now,
        updated_at: now,
    }
}

fn mutation_preview(
    conn: &Connection,
    terms: &[NormalizedDictionaryTerm],
    duplicate_count: usize,
) -> Result<DictionaryImportPreview, DictionaryError> {
    let mut added_count = 0;
    let mut promoted_count = 0;
    let mut skipped_count = duplicate_count;
    for term in terms {
        match seasnail_storage::get_dictionary_entry_by_normalized(conn, &term.normalized_term)? {
            None => added_count += 1,
            Some(existing) if existing.source == "learned" => promoted_count += 1,
            Some(_) => skipped_count += 1,
        }
    }
    if seasnail_storage::count_dictionary_entries(conn)? + added_count as i64
        > MAX_DICTIONARY_ENTRIES
    {
        return Err(DictionaryError::LimitExceeded);
    }
    Ok(DictionaryImportPreview {
        parsed_count: terms.len() + duplicate_count,
        valid_count: terms.len(),
        added_count,
        promoted_count,
        skipped_count,
    })
}

fn add_manual_on_connection(
    conn: &mut Connection,
    terms: &[String],
    now: i64,
) -> Result<DictionaryMutationResult, DictionaryError> {
    let (terms, duplicate_count) = validate_and_dedupe(terms)?;
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let preview = mutation_preview(&transaction, &terms, duplicate_count)?;
    let mut result = DictionaryMutationResult {
        skipped_count: preview.skipped_count,
        ..DictionaryMutationResult::default()
    };
    for term in terms {
        match seasnail_storage::get_dictionary_entry_by_normalized(
            &transaction,
            &term.normalized_term,
        )? {
            None => {
                let row = row_for(&term, "manual", None, now);
                seasnail_storage::insert_dictionary_entry(&transaction, &row)?;
                result.added.push(row.into());
            }
            Some(existing) if existing.source == "learned" => {
                seasnail_storage::promote_dictionary_entry(
                    &transaction,
                    &existing.id,
                    &term.term,
                    now,
                )?;
                result.promoted.push(DictionaryEntry {
                    id: existing.id,
                    term: term.term,
                    source: "manual".into(),
                    created_at: existing.created_at,
                    updated_at: now,
                });
            }
            Some(_) => {}
        }
    }
    transaction.commit()?;
    Ok(result)
}

fn learn_on_connection(
    conn: &mut Connection,
    candidates: &[String],
    now: i64,
) -> Result<LearningMutationResult, DictionaryError> {
    if candidates.is_empty() {
        return Ok(LearningMutationResult {
            learning_event_id: None,
            added: Vec::new(),
        });
    }
    let (terms, _) = validate_and_dedupe(candidates)?;
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut additions = Vec::new();
    for term in terms {
        if seasnail_storage::get_dictionary_entry_by_normalized(
            &transaction,
            &term.normalized_term,
        )?
        .is_none()
        {
            additions.push(term);
        }
    }
    if seasnail_storage::count_dictionary_entries(&transaction)? + additions.len() as i64
        > MAX_DICTIONARY_ENTRIES
    {
        return Err(DictionaryError::LimitExceeded);
    }
    let event_id = (!additions.is_empty()).then(|| uuid::Uuid::new_v4().to_string());
    let mut added = Vec::with_capacity(additions.len());
    for term in additions {
        let row = row_for(&term, "learned", event_id.as_deref(), now);
        seasnail_storage::insert_dictionary_entry(&transaction, &row)?;
        added.push(row.into());
    }
    transaction.commit()?;
    Ok(LearningMutationResult {
        learning_event_id: event_id,
        added,
    })
}

#[derive(Clone, Default)]
pub struct DictionaryService;

impl DictionaryService {
    /// 在任务创建时读取不可变词条快照；调用方只需拥有当前账户的有效绑定。
    pub fn snapshot(&self, caller: &CallerContext) -> Result<DictionarySnapshot, DictionaryError> {
        let conn = caller.repository().open_db().map_err(|error| {
            DictionaryError::Storage(seasnail_storage::Error::Migrate(error.to_string()))
        })?;
        let rows = seasnail_storage::list_dictionary_entries_for_snapshot(&conn)?;
        Ok(DictionarySnapshot {
            terms: rows.into_iter().map(|row| row.term).collect(),
        })
    }

    pub fn list(
        &self,
        caller: &CallerContext,
        query: Option<&str>,
        cursor: Option<&DictionaryCursor>,
        limit: usize,
    ) -> Result<DictionaryPage, DictionaryError> {
        require_root(caller)?;
        if !(1..=200).contains(&limit) {
            return Err(DictionaryError::TooManyTerms);
        }
        let normalized_query = query.map(normalize_dictionary_query).transpose()?.flatten();
        let conn = caller.repository().open_db().map_err(|error| {
            DictionaryError::Storage(seasnail_storage::Error::Migrate(error.to_string()))
        })?;
        let mut rows = seasnail_storage::list_dictionary_entries(
            &conn,
            normalized_query.as_deref(),
            cursor,
            limit as i64 + 1,
        )?;
        let next_cursor = if rows.len() > limit {
            rows.truncate(limit);
            rows.last().map(|row| DictionaryCursor {
                source_rank: i64::from(row.source != "manual"),
                updated_at: row.updated_at,
                id: row.id.clone(),
            })
        } else {
            None
        };
        Ok(DictionaryPage {
            items: rows.into_iter().map(Into::into).collect(),
            next_cursor,
        })
    }

    pub fn add_manual(
        &self,
        caller: &CallerContext,
        terms: &[String],
    ) -> Result<DictionaryMutationResult, DictionaryError> {
        require_root(caller)?;
        let mut conn = caller.repository().open_db().map_err(|error| {
            DictionaryError::Storage(seasnail_storage::Error::Migrate(error.to_string()))
        })?;
        add_manual_on_connection(&mut conn, terms, now_secs())
    }

    pub fn preview_csv(
        &self,
        caller: &CallerContext,
        csv_text: &str,
    ) -> Result<DictionaryImportPreview, DictionaryError> {
        require_root(caller)?;
        let (parsed_count, terms) = parse_csv(csv_text)?;
        let (terms, duplicates) = validate_and_dedupe(&terms)?;
        let mut conn = caller.repository().open_db().map_err(|error| {
            DictionaryError::Storage(seasnail_storage::Error::Migrate(error.to_string()))
        })?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let mut preview = mutation_preview(&transaction, &terms, duplicates)?;
        preview.parsed_count = parsed_count;
        transaction.rollback()?;
        Ok(preview)
    }

    pub fn import_csv(
        &self,
        caller: &CallerContext,
        csv_text: &str,
    ) -> Result<DictionaryImportResult, DictionaryError> {
        require_root(caller)?;
        let (parsed_count, terms) = parse_csv(csv_text)?;
        let mut conn = caller.repository().open_db().map_err(|error| {
            DictionaryError::Storage(seasnail_storage::Error::Migrate(error.to_string()))
        })?;
        let mutation = add_manual_on_connection(&mut conn, &terms, now_secs())?;
        Ok(DictionaryImportResult {
            parsed_count,
            mutation,
        })
    }

    pub fn export_csv(&self, caller: &CallerContext) -> Result<String, DictionaryError> {
        require_root(caller)?;
        let conn = caller.repository().open_db().map_err(|error| {
            DictionaryError::Storage(seasnail_storage::Error::Migrate(error.to_string()))
        })?;
        let rows = seasnail_storage::list_dictionary_entries_for_export(&conn)?;
        let mut writer = csv::WriterBuilder::new()
            .terminator(csv::Terminator::CRLF)
            .from_writer(Vec::new());
        writer
            .write_record(["term"])
            .map_err(|_| DictionaryError::InvalidCsv)?;
        for row in rows {
            writer
                .write_record([row.term])
                .map_err(|_| DictionaryError::InvalidCsv)?;
        }
        let bytes = writer
            .into_inner()
            .map_err(|_| DictionaryError::InvalidCsv)?;
        String::from_utf8(bytes).map_err(|_| DictionaryError::InvalidCsv)
    }

    pub fn edit(
        &self,
        caller: &CallerContext,
        id: &str,
        term: &str,
    ) -> Result<DictionaryEntry, DictionaryError> {
        require_root(caller)?;
        let term = normalize_dictionary_term(term)?;
        let mut conn = caller.repository().open_db().map_err(|error| {
            DictionaryError::Storage(seasnail_storage::Error::Migrate(error.to_string()))
        })?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = seasnail_storage::get_dictionary_entry(&transaction, id)?
            .ok_or(DictionaryError::NotFound)?;
        if let Some(conflict) = seasnail_storage::get_dictionary_entry_by_normalized(
            &transaction,
            &term.normalized_term,
        )? {
            if conflict.id != id {
                return Err(DictionaryError::Conflict);
            }
        }
        let now = now_secs();
        seasnail_storage::update_dictionary_entry(
            &transaction,
            id,
            &term.term,
            &term.normalized_term,
            now,
        )?;
        transaction.commit()?;
        Ok(DictionaryEntry {
            id: existing.id,
            term: term.term,
            source: "manual".into(),
            created_at: existing.created_at,
            updated_at: now,
        })
    }

    pub fn delete(&self, caller: &CallerContext, id: &str) -> Result<(), DictionaryError> {
        require_root(caller)?;
        let conn = caller.repository().open_db().map_err(|error| {
            DictionaryError::Storage(seasnail_storage::Error::Migrate(error.to_string()))
        })?;
        seasnail_storage::delete_dictionary_entry(&conn, id)?;
        Ok(())
    }

    pub fn clear(&self, caller: &CallerContext) -> Result<usize, DictionaryError> {
        require_root(caller)?;
        let conn = caller.repository().open_db().map_err(|error| {
            DictionaryError::Storage(seasnail_storage::Error::Migrate(error.to_string()))
        })?;
        Ok(seasnail_storage::delete_dictionary_entries(&conn)?)
    }

    pub fn learn(
        &self,
        caller: &CallerContext,
        candidates: &[String],
    ) -> Result<LearningMutationResult, DictionaryError> {
        require_root(caller)?;
        let mut conn = caller.repository().open_db().map_err(|error| {
            DictionaryError::Storage(seasnail_storage::Error::Migrate(error.to_string()))
        })?;
        learn_on_connection(&mut conn, candidates, now_secs())
    }

    pub fn undo_learning(
        &self,
        caller: &CallerContext,
        event_id: &str,
    ) -> Result<usize, DictionaryError> {
        require_root(caller)?;
        let conn = caller.repository().open_db().map_err(|error| {
            DictionaryError::Storage(seasnail_storage::Error::Migrate(error.to_string()))
        })?;
        Ok(seasnail_storage::delete_dictionary_learning_event(
            &conn, event_id,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{Auth, Crypto};
    use crate::application::ApplicationServices;
    use seasnail_crypto::{Argon2Params, KeychainStore, MemoryKeychain};
    use seasnail_storage::migrate;
    use std::sync::{Arc, Barrier};

    fn services() -> ApplicationServices {
        let directory = tempfile::tempdir().unwrap().keep();
        let keychain = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let crypto = Arc::new(
            Crypto::new(
                directory,
                keychain,
                Argon2Params {
                    m_kib: 8192,
                    t_cost: 1,
                    p_cost: 1,
                },
            )
            .unwrap(),
        );
        ApplicationServices::new_for_test(Arc::new(Auth::new(crypto)))
    }

    #[test]
    fn normalization_matches_openwhispr_boundaries() {
        let normalized = normalize_dictionary_term("\u{feff} SeaSnail \u{3000}").unwrap();
        assert_eq!(normalized.term, "SeaSnail");
        assert_eq!(normalized.normalized_term, "seasnail");
        assert_ne!(
            normalize_dictionary_term("a  b").unwrap().normalized_term,
            normalize_dictionary_term("a b").unwrap().normalized_term
        );
        assert_ne!(
            normalize_dictionary_term("Straße").unwrap().normalized_term,
            normalize_dictionary_term("STRASSE")
                .unwrap()
                .normalized_term
        );
        assert_ne!(
            normalize_dictionary_term("Ａ").unwrap().normalized_term,
            normalize_dictionary_term("A").unwrap().normalized_term
        );
        assert_eq!(
            normalize_dictionary_term("ΟΣ").unwrap().normalized_term,
            normalize_dictionary_term("ος").unwrap().normalized_term
        );
        assert_ne!(
            normalize_dictionary_term("ΟΣ").unwrap().normalized_term,
            normalize_dictionary_term("οσ").unwrap().normalized_term
        );
        assert!(normalize_dictionary_term("，！？").is_err());
        assert!(normalize_dictionary_term("line\nbreak").is_err());
    }

    #[test]
    fn cleanup_learning_rejects_non_semantic_edits_and_obvious_sentences() {
        assert!(!is_safe_cleanup_learning_candidate("Sea Snail", "SeaSnail"));
        assert!(!is_safe_cleanup_learning_candidate("HELLO", "hello"));
        assert!(!is_safe_cleanup_learning_candidate(
            "old",
            "This is clearly a complete corrected sentence."
        ));
        assert!(!is_safe_cleanup_learning_candidate(
            "旧句子",
            "这是一个完整的中文句子没有句号"
        ));
        assert!(is_safe_cleanup_learning_candidate("sea snail", "SeaSnailX"));
        assert!(is_safe_cleanup_learning_candidate("旧名称", "北京大学"));
    }

    #[test]
    fn csv_parser_and_writer_handle_rfc4180_bom_and_line_endings() {
        let (count, terms) = parse_csv("\u{feff}term\r\n\"ACME, Inc.\"\r\n\"a\"\"b\"\r\n").unwrap();
        assert_eq!(count, 2);
        assert_eq!(terms, ["ACME, Inc.", "a\"b"]);
        let (_, quoted_header_terms) = parse_csv("\u{feff}\"term\"\r\nSeaSnail\r\n").unwrap();
        assert_eq!(quoted_header_terms, ["SeaSnail"]);
        assert!(parse_csv("wrong\nvalue\n").is_err());
        assert!(parse_csv("term,extra\na,b\n").is_err());
        assert!(parse_csv("term\n\"unterminated\n").is_err());
    }

    #[test]
    fn preview_is_read_only_and_import_is_atomic_with_promotions() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let learned = DictionaryEntryRow {
            id: "learned".into(),
            term: "seasnail".into(),
            normalized_term: "seasnail".into(),
            source: "learned".into(),
            learning_event_id: Some("event".into()),
            created_at: 1,
            updated_at: 1,
        };
        seasnail_storage::insert_dictionary_entry(&conn, &learned).unwrap();
        let terms = vec!["SeaSnail".into(), "RAG".into(), "rag".into()];
        let (normalized, duplicates) = validate_and_dedupe(&terms).unwrap();
        let preview = mutation_preview(&conn, &normalized, duplicates).unwrap();
        assert_eq!(preview.promoted_count, 1);
        assert_eq!(preview.added_count, 1);
        assert_eq!(preview.skipped_count, 1);
        assert_eq!(
            seasnail_storage::count_dictionary_entries(&conn).unwrap(),
            1
        );

        let result = add_manual_on_connection(&mut conn, &terms, 5).unwrap();
        assert_eq!(result.promoted.len(), 1);
        assert_eq!(result.added.len(), 1);
        assert_eq!(result.skipped_count, 1);
        assert_eq!(
            seasnail_storage::count_dictionary_entries(&conn).unwrap(),
            2
        );

        let before = seasnail_storage::count_dictionary_entries(&conn).unwrap();
        assert!(add_manual_on_connection(&mut conn, &["valid".into(), "!!!".into()], 6).is_err());
        assert_eq!(
            seasnail_storage::count_dictionary_entries(&conn).unwrap(),
            before
        );
    }

    #[test]
    fn learning_event_undo_preserves_promoted_rows() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let learned = learn_on_connection(
            &mut conn,
            &["SeaSnail".into(), "SenseVoice".into(), "seasnail".into()],
            1,
        )
        .unwrap();
        assert_eq!(learned.added.len(), 2);
        let event_id = learned.learning_event_id.unwrap();
        add_manual_on_connection(&mut conn, &["SEASNAIL".into()], 2).unwrap();
        assert_eq!(
            seasnail_storage::delete_dictionary_learning_event(&conn, &event_id).unwrap(),
            1
        );
        let remaining = seasnail_storage::list_dictionary_entries_for_export(&conn).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].source, "manual");
        assert_eq!(remaining[0].term, "SEASNAIL");
        assert_eq!(remaining[0].learning_event_id, None);
    }

    #[test]
    fn capacity_failure_is_atomic() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn.execute_batch(
            "WITH RECURSIVE seq(n) AS (
               SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 1000
             )
             INSERT INTO dictionary_entries
               (id, term, normalized_term, source, created_at, updated_at)
             SELECT printf('id-%05d', n), printf('Term-%05d', n),
                    printf('term-%05d', n), 'manual', 1, 1
             FROM seq;",
        )
        .unwrap();
        assert!(matches!(
            add_manual_on_connection(&mut conn, &["overflow".into()], 2),
            Err(DictionaryError::LimitExceeded)
        ));
        assert_eq!(
            seasnail_storage::count_dictionary_entries(&conn).unwrap(),
            1_000
        );
    }

    #[test]
    fn concurrent_manual_adds_converge_to_one_entry() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dictionary.db");
        let key = [0x42; 32];
        drop(seasnail_storage::open_db(&path, &key).unwrap());
        let barrier = Arc::new(Barrier::new(2));
        let mut workers = Vec::new();
        for term in ["SeaSnail", "seasnail"] {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                let mut conn = seasnail_storage::open_db(&path, &key).unwrap();
                barrier.wait();
                add_manual_on_connection(&mut conn, &[term.into()], 1).unwrap()
            }));
        }
        let results = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            results
                .iter()
                .map(|result| result.added.len())
                .sum::<usize>(),
            1
        );
        assert_eq!(
            results
                .iter()
                .map(|result| result.skipped_count)
                .sum::<usize>(),
            1
        );
        let conn = seasnail_storage::open_db(&path, &key).unwrap();
        assert_eq!(
            seasnail_storage::count_dictionary_entries(&conn).unwrap(),
            1
        );
    }

    #[test]
    fn preview_reads_one_database_snapshot_during_concurrent_change() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preview.db");
        let key = [0x43; 32];
        let mut preview_conn = seasnail_storage::open_db(&path, &key).unwrap();
        preview_conn
            .execute_batch("PRAGMA journal_mode = WAL;")
            .unwrap();
        let writer_conn = seasnail_storage::open_db(&path, &key).unwrap();
        let transaction = preview_conn
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .unwrap();
        assert_eq!(
            seasnail_storage::count_dictionary_entries(&transaction).unwrap(),
            0
        );

        let normalized = normalize_dictionary_term("SeaSnail").unwrap();
        let concurrent = row_for(&normalized, "manual", None, 1);
        seasnail_storage::insert_dictionary_entry(&writer_conn, &concurrent).unwrap();

        let preview = mutation_preview(&transaction, &[normalized], 0).unwrap();
        assert_eq!(
            preview.added_count, 1,
            "preview must stay on its initial snapshot"
        );
        transaction.rollback().unwrap();
        assert_eq!(
            seasnail_storage::count_dictionary_entries(&writer_conn).unwrap(),
            1
        );
    }

    #[test]
    fn service_crud_csv_and_accounts_are_isolated() {
        let services = services();
        let alice = services.auth.setup_first_account("alice", "p1").unwrap();
        let alice_caller = services.auth.authenticate_and_bind(&alice.secret).unwrap();

        let preview = services
            .dictionary
            .preview_csv(&alice_caller, "term\r\n\"ACME, Inc.\"\r\nSeaSnail\r\n")
            .unwrap();
        assert_eq!(preview.parsed_count, 2);
        assert_eq!(preview.added_count, 2);
        assert!(services
            .dictionary
            .list(&alice_caller, None, None, 50)
            .unwrap()
            .items
            .is_empty());

        let imported = services
            .dictionary
            .import_csv(&alice_caller, "term\n\"ACME, Inc.\"\nSeaSnail\n")
            .unwrap();
        assert_eq!(imported.mutation.added.len(), 2);
        let page = services
            .dictionary
            .list(&alice_caller, Some("%_"), None, 50)
            .unwrap();
        assert!(page.items.is_empty(), "LIKE metacharacters must be literal");
        let exported = services.dictionary.export_csv(&alice_caller).unwrap();
        assert!(exported.starts_with("term\r\n"));
        assert!(exported.contains("\"ACME, Inc.\""));
        let imported_ids = imported
            .mutation
            .added
            .iter()
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>();
        assert!(matches!(
            services
                .dictionary
                .edit(&alice_caller, &imported_ids[0], "SeaSnail"),
            Err(DictionaryError::Conflict)
        ));
        services.dictionary.clear(&alice_caller).unwrap();
        let roundtrip = services
            .dictionary
            .import_csv(&alice_caller, &exported)
            .unwrap();
        assert_eq!(roundtrip.mutation.added.len(), 2);
        assert_eq!(
            services.dictionary.export_csv(&alice_caller).unwrap(),
            exported
        );

        let learned = services
            .dictionary
            .learn(&alice_caller, &["RAG".into()])
            .unwrap();
        let learned_id = learned.added[0].id.clone();
        services
            .dictionary
            .edit(&alice_caller, &learned_id, "Rag")
            .unwrap();
        assert_eq!(
            services
                .dictionary
                .undo_learning(&alice_caller, learned.learning_event_id.as_deref().unwrap())
                .unwrap(),
            0
        );

        let bob = services
            .accounts
            .create(&alice_caller, "bob", "p2")
            .unwrap();
        let bob_caller = services.auth.authenticate_and_bind(&bob.secret).unwrap();
        assert!(services
            .dictionary
            .list(&bob_caller, None, None, 50)
            .unwrap()
            .items
            .is_empty());
        assert_eq!(
            services
                .dictionary
                .list(&alice_caller, None, None, 50)
                .unwrap()
                .items
                .len(),
            3
        );
        let one_id = services
            .dictionary
            .list(&alice_caller, None, None, 1)
            .unwrap()
            .items[0]
            .id
            .clone();
        services.dictionary.delete(&alice_caller, &one_id).unwrap();
        services.dictionary.clear(&alice_caller).unwrap();
        assert!(services
            .dictionary
            .list(&alice_caller, None, None, 50)
            .unwrap()
            .items
            .is_empty());
    }
}
