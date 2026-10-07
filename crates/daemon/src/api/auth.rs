//! 鉴权相关端点：`GET /auth/status`、`POST /auth/setup`、`POST /auth/password`。
//!
//! 对应 `proto/openapi.yaml` 该组端点。`/auth/status`、`/auth/setup` 免鉴权
//!（GUI 启动前查初始化态 / 首启建账户）；`/auth/password` 需 root（`tokens:manage`）+ 当前密码。

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::dto::TokenCreatedDto;
use crate::api::{require_scope, AuthedCaller, HttpState};
use crate::error::AppError;

/// `GET /auth/status` 200 响应体（对应 openapi 内联 schema）。
#[derive(Serialize)]
pub struct AuthStatus {
    /// 是否已初始化（是否存在任何账户）。查全局 registry 内存副本，零 I/O。
    pub initialized: bool,
}

/// `POST /auth/setup` 请求体。
#[derive(Deserialize)]
pub struct SetupReq {
    pub username: String,
    pub password: String,
}

/// `POST /auth/password` 请求体。
#[derive(Deserialize)]
pub struct PasswordReq {
    pub current_password: String,
    pub new_password: String,
}

/// `GET /auth/status` — 免鉴权。GUI 启动据此走 setup 或正常态。
async fn auth_status(State(state): State<HttpState>) -> Json<AuthStatus> {
    Json(AuthStatus {
        initialized: state.auth_service().is_initialized(),
    })
}

/// `POST /auth/setup` — 免鉴权，仅未初始化。建首账户 + 签发该账户 root token。
async fn setup(
    State(state): State<HttpState>,
    Json(req): Json<SetupReq>,
) -> Result<(StatusCode, Json<TokenCreatedDto>), AppError> {
    let token = state
        .auth_service()
        .setup_first_account(&req.username, &req.password)?;
    Ok((
        StatusCode::CREATED,
        Json(TokenCreatedDto::from_issued(token)),
    ))
}

/// `POST /auth/password` — 需 root（`tokens:manage`）+ 当前密码。作用于 caller 的账户。
async fn password(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Json(req): Json<PasswordReq>,
) -> Result<StatusCode, AppError> {
    require_scope(&caller, "tokens:manage")?;
    state
        .auth_service()
        .change_password(&caller, &req.current_password, &req.new_password)?;
    Ok(StatusCode::OK)
}

/// auth 资源子路由。
pub fn routes() -> Router<HttpState> {
    Router::new()
        .route("/auth/status", get(auth_status))
        .route("/auth/setup", post(setup))
        .route("/auth/password", post(password))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{Auth, Crypto};
    use seasnail_crypto::{Argon2Params, MemoryKeychain};
    use std::sync::Arc;

    fn fast_params() -> Argon2Params {
        Argon2Params {
            m_kib: 8192,
            t_cost: 1,
            p_cost: 1,
        }
    }

    /// `/auth/status` 未初始化 → initialized=false；setup 后 → true。
    ///（端到端集成测试见 `tests/auth_endpoints.rs`；此处仅 handler 直调单测。）
    #[tokio::test]
    async fn auth_status_reflects_initialized() {
        let dir = tempfile::tempdir().unwrap();
        let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn seasnail_crypto::KeychainStore>;
        let crypto = Arc::new(Crypto::new(dir.path().to_path_buf(), kc, fast_params()).unwrap());
        let auth = Arc::new(Auth::new(crypto));
        let state = HttpState::new(crate::api::AppState::new(auth, dir.path().to_path_buf()));

        let resp = auth_status(State(state.clone())).await.0;
        assert!(!resp.initialized, "未初始化应为 false");

        state
            .auth_service()
            .setup_first_account("alice", "p")
            .unwrap();
        let resp = auth_status(State(state)).await.0;
        assert!(resp.initialized, "setup 后应为 true");
    }
}
