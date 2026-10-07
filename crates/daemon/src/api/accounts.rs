//! accounts 端点：`GET /accounts`、`POST /accounts`、`DELETE /accounts/{id}`、
//! `POST /accounts/{id}/unlock`。
//!
//! 对应 `proto/openapi.yaml`。`GET/POST/DELETE /accounts` 与 `DELETE /accounts/{id}` 需
//! root（`tokens:manage`）；`POST /accounts/{id}/unlock` 免鉴权（密码作知识因子）。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::api::dto::{AccountSummaryDto, TokenCreatedDto};
use crate::api::{require_scope, AuthedCaller, HttpState};
use crate::error::AppError;

/// `POST /accounts` 请求体（`AccountCreate` schema）。
#[derive(Deserialize)]
pub struct AccountCreateReq {
    pub username: String,
    pub password: String,
}

/// `POST /accounts/{id}/unlock` 请求体（仅 password）。
#[derive(Deserialize)]
pub struct UnlockReq {
    pub password: String,
}

/// `GET /accounts` — 需 root。返回所有账户摘要（含 `is_active`）。
async fn list_accounts(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
) -> Result<Json<Vec<AccountSummaryDto>>, AppError> {
    require_scope(&caller, "tokens:manage")?;
    let summaries = state.account_service().list(&caller)?;
    Ok(Json(
        summaries
            .into_iter()
            .map(AccountSummaryDto::from_summary)
            .collect(),
    ))
}

/// `POST /accounts` — 需 root。追加账户 + 切换活跃 + 签发新账户 root token。
async fn create_account(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Json(req): Json<AccountCreateReq>,
) -> Result<(StatusCode, Json<TokenCreatedDto>), AppError> {
    require_scope(&caller, "tokens:manage")?;
    let token = state
        .account_service()
        .create(&caller, &req.username, &req.password)?;
    Ok((
        StatusCode::CREATED,
        Json(TokenCreatedDto::from_issued(token)),
    ))
}

/// `DELETE /accounts/{id}` — 需 root。活跃账户不可直接删（409），否则删全部数据。
async fn delete_account(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    require_scope(&caller, "tokens:manage")?;
    state.account_service().delete(&caller, &id)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /accounts/{id}/unlock` — 免鉴权。输该账户密码 → 切活跃 + 签发该账户 root token。
/// 密码错 → 403 `wrong_password`；账户不存在 → 404。
async fn unlock_account(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    Json(req): Json<UnlockReq>,
) -> Result<(StatusCode, Json<TokenCreatedDto>), AppError> {
    let token = state.account_service().unlock(&id, &req.password)?;
    Ok((StatusCode::OK, Json(TokenCreatedDto::from_issued(token))))
}

/// accounts 资源子路由。
pub fn routes() -> Router<HttpState> {
    Router::new()
        .route("/accounts", get(list_accounts).post(create_account))
        .route("/accounts/:id", delete(delete_account))
        .route("/accounts/:id/unlock", post(unlock_account))
}
