//! tokens 端点：`POST /tokens`、`GET /tokens`、`DELETE /tokens/{id}`。
//!
//! 对应 `proto/openapi.yaml`。均需 root（`tokens:manage`）。第三方 token 绑 caller.account_id、
//! scope 经 `enforce_grantable` 硬约束（非 root 不可授 write/manage、任何人不可授 is_root）。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::api::dto::{TokenCreatedDto, TokenDto};
use crate::api::{require_scope, AuthedCaller, HttpState};
use crate::error::AppError;

/// `POST /tokens` 请求体（`TokenCreate` schema）。
#[derive(Deserialize)]
pub struct TokenCreateReq {
    pub name: String,
    pub scopes: Vec<String>,
}

/// `POST /tokens` — 需 root。签发第三方 token（绑 caller.account_id，is_root=false）。
/// scope 不可授 write/manage/is_root（`Auth::issue_token` 内 `enforce_grantable`）。
async fn create_token(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Json(req): Json<TokenCreateReq>,
) -> Result<(StatusCode, Json<TokenCreatedDto>), AppError> {
    require_scope(&caller, "tokens:manage")?;
    let token = state
        .token_service()
        .issue(&caller, &req.name, req.scopes)?;
    Ok((
        StatusCode::CREATED,
        Json(TokenCreatedDto::from_issued(token)),
    ))
}

/// `GET /tokens` — 需 root。列 caller 账户的全部 token（不含 secret / token_hash）。
async fn list_tokens(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
) -> Result<Json<Vec<TokenDto>>, AppError> {
    require_scope(&caller, "tokens:manage")?;
    let rows = state.token_service().list(&caller)?;
    Ok(Json(rows.into_iter().map(TokenDto::from_row).collect()))
}

/// `DELETE /tokens/{id}` — 需 root。吊销 token（硬删 DB 行，撤销即失效）。
async fn revoke_token(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    require_scope(&caller, "tokens:manage")?;
    state.token_service().revoke(&caller, &id)?;
    Ok(StatusCode::NO_CONTENT)
}

/// tokens 资源子路由。
pub fn routes() -> Router<HttpState> {
    Router::new()
        .route("/tokens", post(create_token).get(list_tokens))
        .route("/tokens/:id", delete(revoke_token))
}
