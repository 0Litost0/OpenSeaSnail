//! Root-only Dictionary HTTP endpoints.

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::dto::{
    AddDictionaryEntriesRequest, DictionaryEntryDto, DictionaryImportPreviewDto,
    DictionaryImportResultDto, DictionaryMutationResultDto, DictionaryPageDto,
    EditDictionaryEntryRequest, ImportDictionaryCsvRequest, LearningEventRequest,
    LearningEventResponse, UndoLearningEventResponse,
};
use super::{AuthedCaller, HttpState};
use crate::application::{normalize_dictionary_query, DictionaryError};
use crate::error::AppError;

#[derive(Debug, Deserialize)]
struct DictionaryListQuery {
    query: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CursorWire {
    source_rank: i64,
    updated_at: i64,
    id: String,
    query_hash: String,
}

fn invalid_request() -> AppError {
    AppError::BadRequest("invalid dictionary request".into())
}

fn map_json_rejection(rejection: JsonRejection) -> AppError {
    if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
        AppError::RequestTooLarge("dictionary request exceeds 1 MiB".into())
    } else {
        invalid_request()
    }
}

fn validate_entry_id(id: &str) -> Result<(), AppError> {
    uuid::Uuid::parse_str(id)
        .map(|_| ())
        .map_err(|_| AppError::BadRequest("invalid dictionary entry id".into()))
}

fn query_hash(query: Option<&str>) -> Result<String, AppError> {
    let normalized = query
        .map(normalize_dictionary_query)
        .transpose()
        .map_err(|error| match error {
            DictionaryError::InvalidTerm | DictionaryError::TooManyTerms => {
                AppError::BadRequest("invalid dictionary query".into())
            }
            other => AppError::from(other),
        })?
        .flatten()
        .unwrap_or_default();
    Ok(format!("{:x}", Sha256::digest(normalized.as_bytes())))
}

fn decode_cursor(
    value: &str,
    expected_query_hash: &str,
) -> Result<seasnail_storage::DictionaryCursor, AppError> {
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| AppError::BadRequest("invalid dictionary cursor".into()))?;
    let cursor: CursorWire = serde_json::from_slice(&decoded)
        .map_err(|_| AppError::BadRequest("invalid dictionary cursor".into()))?;
    if cursor.query_hash != expected_query_hash || !(0..=1).contains(&cursor.source_rank) {
        return Err(AppError::BadRequest(
            "dictionary cursor does not match query".into(),
        ));
    }
    uuid::Uuid::parse_str(&cursor.id)
        .map_err(|_| AppError::BadRequest("invalid dictionary cursor".into()))?;
    Ok(seasnail_storage::DictionaryCursor {
        source_rank: cursor.source_rank,
        updated_at: cursor.updated_at,
        id: cursor.id,
    })
}

fn encode_cursor(
    cursor: seasnail_storage::DictionaryCursor,
    query_hash: String,
) -> Result<String, AppError> {
    let bytes = serde_json::to_vec(&CursorWire {
        source_rank: cursor.source_rank,
        updated_at: cursor.updated_at,
        id: cursor.id,
        query_hash,
    })
    .map_err(|error| AppError::Internal(format!("dictionary cursor encoding: {error}")))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

async fn list_entries(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    query: Result<Query<DictionaryListQuery>, QueryRejection>,
) -> Result<Json<DictionaryPageDto>, AppError> {
    let Query(query) = query.map_err(|_| invalid_request())?;
    let limit = query.limit.unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(AppError::BadRequest(
            "dictionary limit must be between 1 and 200".into(),
        ));
    }
    let hash = query_hash(query.query.as_deref())?;
    let cursor = query
        .cursor
        .as_deref()
        .map(|cursor| decode_cursor(cursor, &hash))
        .transpose()?;
    let page = state
        .dictionary_service()
        .list(&caller, query.query.as_deref(), cursor.as_ref(), limit)
        .map_err(AppError::from)?;
    let next_cursor = page
        .next_cursor
        .map(|cursor| encode_cursor(cursor, hash))
        .transpose()?;
    Ok(Json(DictionaryPageDto {
        items: page.items.into_iter().map(Into::into).collect(),
        next_cursor,
    }))
}

async fn add_entries(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    request: Result<Json<AddDictionaryEntriesRequest>, JsonRejection>,
) -> Result<Json<DictionaryMutationResultDto>, AppError> {
    let Json(request) = request.map_err(map_json_rejection)?;
    Ok(Json(
        state
            .dictionary_service()
            .add_manual(&caller, &request.terms)
            .map_err(AppError::from)?
            .into(),
    ))
}

async fn clear_entries(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
) -> Result<StatusCode, AppError> {
    state
        .dictionary_service()
        .clear(&caller)
        .map_err(AppError::from)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn edit_entry(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path(id): Path<String>,
    request: Result<Json<EditDictionaryEntryRequest>, JsonRejection>,
) -> Result<Json<DictionaryEntryDto>, AppError> {
    validate_entry_id(&id)?;
    let Json(request) = request.map_err(map_json_rejection)?;
    Ok(Json(
        state
            .dictionary_service()
            .edit(&caller, &id, &request.term)
            .map_err(AppError::from)?
            .into(),
    ))
}

async fn delete_entry(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    validate_entry_id(&id)?;
    state
        .dictionary_service()
        .delete(&caller, &id)
        .map_err(AppError::from)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn preview_import(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    request: Result<Json<ImportDictionaryCsvRequest>, JsonRejection>,
) -> Result<Json<DictionaryImportPreviewDto>, AppError> {
    let Json(request) = request.map_err(map_json_rejection)?;
    Ok(Json(
        state
            .dictionary_service()
            .preview_csv(&caller, &request.csv)
            .map_err(AppError::from)?
            .into(),
    ))
}

async fn import_csv(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    request: Result<Json<ImportDictionaryCsvRequest>, JsonRejection>,
) -> Result<Json<DictionaryImportResultDto>, AppError> {
    let Json(request) = request.map_err(map_json_rejection)?;
    Ok(Json(
        state
            .dictionary_service()
            .import_csv(&caller, &request.csv)
            .map_err(AppError::from)?
            .into(),
    ))
}

async fn export_csv(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
) -> Result<Response<String>, AppError> {
    let csv = state
        .dictionary_service()
        .export_csv(&caller)
        .map_err(AppError::from)?;
    let mut response = Response::new(csv);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"seasnail-dictionary.csv\""),
    );
    Ok(response)
}

async fn learning_event(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    request: Result<Json<LearningEventRequest>, JsonRejection>,
) -> Result<Json<LearningEventResponse>, AppError> {
    let Json(request) = request.map_err(map_json_rejection)?;
    if !caller.is_root() {
        return Err(AppError::InsufficientScope("requires root token".into()));
    }
    let supplied_candidates = request.candidates.unwrap_or_default();
    if supplied_candidates.len() > 32 {
        return Err(AppError::DictionaryInvalidTerm(
            "too many learning candidates".into(),
        ));
    }
    if request.mode == "cleanup" && !supplied_candidates.is_empty() {
        return Err(AppError::BadRequest(
            "cleanup learning candidates must be empty".into(),
        ));
    }
    if request.mode != "cleanup" && request.mode != "user_edit" {
        return Err(AppError::BadRequest("invalid learning mode".into()));
    }
    let ticket = request
        .ticket
        .as_deref()
        .ok_or_else(|| AppError::LearningTicketInvalid("learning ticket is invalid".into()))?;
    let ticket_data = state
        .sessions()
        .consume_learning_ticket(&caller, ticket)
        .map_err(|error| match error {
            crate::application::LearningTicketError::Invalid => {
                AppError::LearningTicketInvalid("learning ticket is invalid".into())
            }
            crate::application::LearningTicketError::Expired => {
                AppError::LearningTicketExpired("learning ticket expired".into())
            }
            crate::application::LearningTicketError::Consumed => {
                AppError::LearningTicketConsumed("learning ticket already consumed".into())
            }
            crate::application::LearningTicketError::Mismatch => AppError::LearningTicketMismatch(
                "learning ticket does not match current session".into(),
            ),
        })?;
    let candidates = match request.mode.as_str() {
        "cleanup" => ticket_data.cleanup_candidates,
        "user_edit" => supplied_candidates,
        _ => return Err(AppError::BadRequest("invalid learning mode".into())),
    };
    let result = state
        .dictionary_service()
        .learn(&caller, &candidates)
        .map_err(AppError::from)?;
    Ok(Json(LearningEventResponse {
        learning_event_id: result.learning_event_id,
        added_terms: result.added.into_iter().map(|entry| entry.term).collect(),
    }))
}

async fn undo_learning_event(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path(id): Path<String>,
) -> Result<Json<UndoLearningEventResponse>, AppError> {
    if !caller.is_root() {
        return Err(AppError::InsufficientScope("requires root token".into()));
    }
    let undone_count = state
        .dictionary_service()
        .undo_learning(&caller, &id)
        .map_err(AppError::from)?;
    Ok(Json(UndoLearningEventResponse { undone_count }))
}

pub fn routes() -> Router<HttpState> {
    Router::new()
        .route("/dictionary", get(list_entries))
        .route(
            "/dictionary/entries",
            post(add_entries).delete(clear_entries),
        )
        .route(
            "/dictionary/entries/:id",
            put(edit_entry).delete(delete_entry),
        )
        .route("/dictionary/imports/preview", post(preview_import))
        .route("/dictionary/imports", post(import_csv))
        .route("/dictionary/export", get(export_csv))
        .route("/internal/dictionary/learning-events", post(learning_event))
        .route(
            "/internal/dictionary/learning-events/:id/undo",
            post(undo_learning_event),
        )
        .layer(DefaultBodyLimit::max(crate::application::MAX_CSV_BYTES))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_is_bound_to_normalized_query() {
        let hash = query_hash(Some(" SeaSnail ")).unwrap();
        assert_eq!(hash, query_hash(Some("seasnail")).unwrap());
        let encoded = encode_cursor(
            seasnail_storage::DictionaryCursor {
                source_rank: 0,
                updated_at: 1,
                id: uuid::Uuid::new_v4().to_string(),
            },
            hash.clone(),
        )
        .unwrap();
        assert!(decode_cursor(&encoded, &hash).is_ok());
        assert!(decode_cursor(&encoded, &query_hash(Some("other")).unwrap()).is_err());
    }
}
