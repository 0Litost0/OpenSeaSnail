//! Capability-authenticated desktop login entrypoints. Public account APIs stay root-only.
use crate::{api::HttpState, error::AppError};
use axum::{
    extract::State,
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::Value;
fn authorize(state: &HttpState, headers: &HeaderMap) -> Result<(), AppError> {
    let key = headers
        .get("x-seasnail-desktop-capability")
        .and_then(|value| value.to_str().ok());
    let valid = match (key, state.desktop_capability.as_deref()) {
        (Some(actual), Some(expected)) if actual.len() == expected.len() => {
            actual
                .bytes()
                .zip(expected.bytes())
                .fold(0u8, |diff, (a, b)| diff | (a ^ b))
                == 0
        }
        _ => false,
    };
    if !valid {
        return Err(AppError::Unauthorized("desktop capability required".into()));
    }
    Ok(())
}
async fn status(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    authorize(&state, &headers)?;
    Ok(Json(state.account_service().desktop_status()))
}
#[derive(Deserialize)]
struct Credentials {
    username: String,
    password: String,
}
#[derive(Deserialize)]
struct Login {
    id: String,
    password: String,
}
async fn create(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(input): Json<Credentials>,
) -> Result<Json<Value>, AppError> {
    authorize(&state, &headers)?;
    if input.username.trim().is_empty()
        || input.username.len() > 128
        || input.password.is_empty()
        || input.password.len() > 1024
    {
        return Err(AppError::BadRequest("invalid account credentials".into()));
    }
    state
        .account_service()
        .desktop_create(input.username.trim(), &input.password)?;
    Ok(Json(state.account_service().desktop_status()))
}
async fn login(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(input): Json<Login>,
) -> Result<Json<Value>, AppError> {
    authorize(&state, &headers)?;
    state
        .account_service()
        .desktop_login(&input.id, &input.password)?;
    Ok(Json(state.account_service().desktop_status()))
}
async fn logout(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    authorize(&state, &headers)?;
    state.account_service().desktop_logout()?;
    Ok(Json(state.account_service().desktop_status()))
}
pub(crate) fn routes() -> Router<HttpState> {
    Router::new()
        .route("/status", get(status))
        .route("/create", post(create))
        .route("/login", post(login))
        .route("/logout", post(logout))
}
