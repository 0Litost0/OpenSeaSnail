//! HTTP 应用组装。
//!
//! 路由边界：
//! - `GET /` 内部存活探测（bootstrap `validate` / `wait_health` 用），**非** OpenAPI 契约端点；
//! - `/api/v1/*` OpenAPI 服务面（`api::router`），唯一对外集成面；
//! - `/internal/desktop-auth/*` 仅本机 capability 认证的桌面登录入口。
//!
//! 对应设计文档「后端服务层 · OpenAPI 服务（唯一集成面）」与路线图 ST-M1.4 / ST-M2.6。

use axum::{routing::get, Json, Router};
use serde_json::json;

use crate::api::{self, HttpState};
use crate::logging::trace_id_middleware;

/// 构建完整 HTTP 应用：内部 liveness `GET /` + OpenAPI 面 `/api/v1/*`。
///
/// `state` 是进程组合根创建的唯一 `HttpState`；鉴权端点经 Application Service 提取 caller。
/// trace_id 中间件仅挂 `/api/v1`（OpenAPI 面 = 客户端请求入口），不覆盖内部 `GET /`
/// 存活探测——后者高频轮询，挂中间件会刷屏且无前端 trace_id 可关联。
pub fn build_app(state: HttpState) -> Router {
    let api_routes = api::router()
        .layer(axum::middleware::from_fn(trace_id_middleware))
        .with_state(state.clone());
    Router::<HttpState>::new()
        .route("/", get(liveness))
        .nest("/internal/desktop-auth", api::desktop_auth::routes())
        .nest("/api/v1", api_routes)
        .with_state(state)
}

/// 内部存活探测。返回 `{"status":"ok"}`，供 `health_probe`（raw TCP `GET /` → 200）校验。
async fn liveness() -> Json<serde_json::Value> {
    Json(json!({"status":"ok"}))
}
