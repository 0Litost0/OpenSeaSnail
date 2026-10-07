//! M5.5：密码确认后的明文 ZIP 导出。ZIP 只在响应内存中存在，不落本地明文文件。

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;

use crate::api::{AuthedCaller, HttpState};
use crate::error::AppError;

#[derive(Deserialize)]
struct ExportRequest {
    password: String,
    #[serde(default)]
    session_ids: Vec<String>,
}

async fn export_sessions(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Json(req): Json<ExportRequest>,
) -> Result<Response, AppError> {
    state
        .export_service()
        .verify_password(&caller, &req.password)?;
    let bytes = state
        .export_service()
        .build_zip(&caller, &req.session_ids)?;
    Ok((
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/zip"),
            ),
            (
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("attachment; filename=seasnail-export.zip"),
            ),
        ],
        Bytes::from(bytes),
    )
        .into_response())
}

pub fn routes() -> Router<HttpState> {
    Router::new().route("/export", post(export_sessions))
}
