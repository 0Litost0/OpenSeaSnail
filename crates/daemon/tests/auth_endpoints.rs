//! 鉴权端点 HTTP 集成验收（ST-M2.6）。
//!
//! 在进程内用 `build_app` + `MemoryKeychain` 起真实 axum 服务，复用 raw TCP helper
//!（`http_get`/`http_post`）打 OpenAPI 端点。覆盖 10 个鉴权端点的 happy path + 错误码
//! 矩阵，验「各端点行为符合 openapi.yaml；setup 已初始化→410」。
//!
//! **为何不走真守护进程二进制**：生产 `MacKeychain::new()` 走 Data Protection
//! keychain，需签名 `.app` entitlement；未签名的 `cargo test` 二进制写入得
//! `errSecMissingEntitlement`（设计已定，M6 签名包上机确认），`POST /auth/setup`
//! 会 500 而非 201。故本测试注入 `MemoryKeychain`（编排层经 `KeychainStore` trait
//! 抽象，ST-M2.3）隔离系统 keychain，专测 HTTP 面（提取器/handler/路由/错误映射）。
//! 真二进制 E2E（lifecycle/父进程死亡）由 `tests/lifecycle.rs` 覆盖。

use seasnail_crypto::{Argon2Params, KeychainStore, MemoryKeychain};
use seasnail_daemon::daemon_resources::DaemonResources;
use seasnail_daemon::{
    build_app, http_get, http_get_with_headers, http_post, AppState, Auth, Crypto,
};
use std::sync::Arc;
use tokio::net::TcpListener;

/// 测试用小 Argon2 参数（快）；非生产 DEFAULT。
fn fast_params() -> Argon2Params {
    Argon2Params {
        m_kib: 8192,
        t_cost: 1,
        p_cost: 1,
    }
}

/// 起一个进程内 axum 服务：独立 tempdir + `MemoryKeychain`，返回端口。
async fn spawn_app(home: &std::path::Path) -> u16 {
    let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
    let crypto = Arc::new(Crypto::new(home.to_path_buf(), kc, fast_params()).unwrap());
    let auth = Arc::new(Auth::new(crypto));
    let state = DaemonResources::new(AppState::new(auth, home.to_path_buf())).http_state();
    let app = build_app(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    port
}

async fn spawn_cleanup_upstream(cleaned_json: &'static str) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let read = stream.read(&mut chunk).await.unwrap();
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            let Some(header_end) = buffer.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&buffer[..header_end]).to_lowercase();
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if buffer.len() >= header_end + 4 + length {
                break;
            }
        }
        let body = serde_json::json!({
            "choices": [{
                "message": { "content": cleaned_json },
                "finish_reason": "stop"
            }]
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
    });
    port
}

/// 从 JSON body 取字符串字段。
fn json_field(body: &str, key: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get(key)?.as_str().map(String::from)
}

/// 构造 `Authorization: Bearer <secret>` 头值（owned）。调用方持 String 再 `.as_str()`。
fn bearer_value(secret: &str) -> String {
    format!("Bearer {secret}")
}

/// `GET /auth/status`：未初始化 → `{initialized:false}`。
#[tokio::test]
async fn auth_status_uninitialized() {
    let dir = tempfile::tempdir().unwrap();
    let port = spawn_app(dir.path()).await;
    let (code, body) = http_get(port, "/api/v1/auth/status").await.unwrap();
    assert_eq!(code, 200);
    assert!(body.contains("\"initialized\":false"), "body={body}");
}

/// `POST /auth/setup`：未初始化 → 201 TokenCreated（secret 以 `ss_live_` 开头）。
#[tokio::test]
async fn setup_first_account() {
    let dir = tempfile::tempdir().unwrap();
    let port = spawn_app(dir.path()).await;
    let (code, body) = http_post(
        port,
        "/api/v1/auth/setup",
        r#"{"username":"alice","password":"p@ss"}"#,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(code, 201, "body={body}");
    let secret = json_field(&body, "secret").expect("应含 secret");
    assert!(secret.starts_with("ss_live_"), "secret={secret}");
    assert!(json_field(&body, "id").is_some());
    assert!(
        body.contains("\"is_root\":true"),
        "root token is_root=true, body={body}"
    );
    // setup 后 status → initialized:true。
    let (_, body2) = http_get(port, "/api/v1/auth/status").await.unwrap();
    assert!(body2.contains("\"initialized\":true"), "body={body2}");
}

/// `POST /auth/setup`：已初始化 → 410 gone。
#[tokio::test]
async fn setup_already_initialized_returns_410() {
    let dir = tempfile::tempdir().unwrap();
    let port = spawn_app(dir.path()).await;
    http_post(
        port,
        "/api/v1/auth/setup",
        r#"{"username":"alice","password":"p"}"#,
        &[],
    )
    .await
    .unwrap();
    let (code, body) = http_post(
        port,
        "/api/v1/auth/setup",
        r#"{"username":"bob","password":"p"}"#,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(code, 410);
    assert!(body.contains("\"code\":\"gone\""), "body={body}");
}

/// 无 bearer 调受保护端点 → 401。
#[tokio::test]
async fn protected_endpoint_without_bearer_returns_401() {
    let dir = tempfile::tempdir().unwrap();
    let port = spawn_app(dir.path()).await;
    let (code, body) = http_get(port, "/api/v1/accounts").await.unwrap();
    assert_eq!(code, 401);
    assert!(body.contains("\"code\":\"unauthorized\""), "body={body}");
}

/// `GET /accounts`：无 bearer → 401；root bearer → 200 AccountSummary[]（含 is_active）。
#[tokio::test]
async fn list_accounts_with_root() {
    let dir = tempfile::tempdir().unwrap();
    let port = spawn_app(dir.path()).await;
    let (_, body) = http_post(
        port,
        "/api/v1/auth/setup",
        r#"{"username":"alice","password":"p"}"#,
        &[],
    )
    .await
    .unwrap();
    let secret = json_field(&body, "secret").unwrap();
    let auth = bearer_value(&secret);

    let (code, body) = http_get(port, "/api/v1/accounts").await.unwrap();
    assert_eq!(code, 401);
    assert!(body.contains("\"code\":\"unauthorized\""), "body={body}");

    let (code, body) = http_get_with_headers(
        port,
        "/api/v1/accounts",
        &[("Authorization", auth.as_str())],
    )
    .await
    .unwrap();
    assert_eq!(code, 200, "body={body}");
    assert!(body.contains("\"username\":\"alice\""), "body={body}");
    assert!(body.contains("\"is_active\":true"), "body={body}");
}

/// `POST /accounts`：root bearer → 201 TokenCreated；create 后活跃切到新账户，
/// 后续 verify 须用新账户的 bearer（旧账户已锁定）。
#[tokio::test]
async fn create_account_with_root() {
    let dir = tempfile::tempdir().unwrap();
    let port = spawn_app(dir.path()).await;
    let (_, body) = http_post(
        port,
        "/api/v1/auth/setup",
        r#"{"username":"alice","password":"p"}"#,
        &[],
    )
    .await
    .unwrap();
    let alice_secret = json_field(&body, "secret").unwrap();
    let alice_auth = bearer_value(&alice_secret);
    let (code, body) = http_post(
        port,
        "/api/v1/accounts",
        r#"{"username":"bob","password":"p2"}"#,
        &[("Authorization", alice_auth.as_str())],
    )
    .await
    .unwrap();
    assert_eq!(code, 201, "body={body}");
    let bob_secret = json_field(&body, "secret").unwrap();
    assert!(bob_secret.starts_with("ss_live_"));
    // 活跃已切到 bob；用 bob 的 bearer 列账户（alice 旧 bearer 已锁定 → 423）。
    let bob_auth = bearer_value(&bob_secret);
    let (_, body) = http_get_with_headers(
        port,
        "/api/v1/accounts",
        &[("Authorization", bob_auth.as_str())],
    )
    .await
    .unwrap();
    assert!(body.contains("\"username\":\"bob\""), "body={body}");
    assert!(body.contains("\"username\":\"alice\""), "body={body}");
}

/// `POST /accounts/{id}/unlock`：正确密码 → 200；错密码 → 403；未知 id → 404。
#[tokio::test]
async fn unlock_account_flow() {
    let dir = tempfile::tempdir().unwrap();
    let port = spawn_app(dir.path()).await;
    let (_, body) = http_post(
        port,
        "/api/v1/auth/setup",
        r#"{"username":"alice","password":"p1"}"#,
        &[],
    )
    .await
    .unwrap();
    let alice_id = json_field(&body, "account_id").unwrap();
    let secret = json_field(&body, "secret").unwrap();
    let auth = bearer_value(&secret);
    // 追加 bob，bob 活跃，alice 非活跃。
    http_post(
        port,
        "/api/v1/accounts",
        r#"{"username":"bob","password":"p2"}"#,
        &[("Authorization", auth.as_str())],
    )
    .await
    .unwrap();

    // 错密码 → 403 wrong_password。
    let (code, body) = http_post(
        port,
        &format!("/api/v1/accounts/{alice_id}/unlock"),
        r#"{"password":"wrong"}"#,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(code, 403, "body={body}");
    assert!(body.contains("\"code\":\"wrong_password\""), "body={body}");

    // 未知 id → 404。
    let (code, body) = http_post(
        port,
        "/api/v1/accounts/00000000-0000-0000-0000-000000000000/unlock",
        r#"{"password":"p1"}"#,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(code, 404);
    assert!(body.contains("\"code\":\"not_found\""), "body={body}");

    // 正确密码 → 200 TokenCreated。
    let (code, body) = http_post(
        port,
        &format!("/api/v1/accounts/{alice_id}/unlock"),
        r#"{"password":"p1"}"#,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(code, 200, "body={body}");
    assert!(json_field(&body, "secret").unwrap().starts_with("ss_live_"));
}

/// `POST /tokens`：root + read scope → 201；write scope → 403 scope_not_grantable。
#[tokio::test]
async fn create_token_scope_rules() {
    let dir = tempfile::tempdir().unwrap();
    let port = spawn_app(dir.path()).await;
    let (_, body) = http_post(
        port,
        "/api/v1/auth/setup",
        r#"{"username":"alice","password":"p"}"#,
        &[],
    )
    .await
    .unwrap();
    let secret = json_field(&body, "secret").unwrap();
    let auth = bearer_value(&secret);

    let (code, body) = http_post(
        port,
        "/api/v1/tokens",
        r#"{"name":"ci","scopes":["sessions:read"]}"#,
        &[("Authorization", auth.as_str())],
    )
    .await
    .unwrap();
    assert_eq!(code, 201, "body={body}");
    assert!(
        body.contains("\"is_root\":false"),
        "第三方非 root, body={body}"
    );

    let (code, body) = http_post(
        port,
        "/api/v1/tokens",
        r#"{"name":"bad","scopes":["sessions:write"]}"#,
        &[("Authorization", auth.as_str())],
    )
    .await
    .unwrap();
    assert_eq!(code, 403);
    assert!(
        body.contains("\"code\":\"scope_not_grantable\""),
        "body={body}"
    );
}

/// `GET /tokens`：root → 200 Token[]（含 root + 第三方），不含 secret。
#[tokio::test]
async fn list_tokens_excludes_secret() {
    let dir = tempfile::tempdir().unwrap();
    let port = spawn_app(dir.path()).await;
    let (_, body) = http_post(
        port,
        "/api/v1/auth/setup",
        r#"{"username":"alice","password":"p"}"#,
        &[],
    )
    .await
    .unwrap();
    let secret = json_field(&body, "secret").unwrap();
    let auth = bearer_value(&secret);
    http_post(
        port,
        "/api/v1/tokens",
        r#"{"name":"ci","scopes":["sessions:read"]}"#,
        &[("Authorization", auth.as_str())],
    )
    .await
    .unwrap();
    let (code, body) =
        http_get_with_headers(port, "/api/v1/tokens", &[("Authorization", auth.as_str())])
            .await
            .unwrap();
    assert_eq!(code, 200, "body={body}");
    assert!(
        !body.contains("secret"),
        "GET /tokens 不得含 secret, body={body}"
    );
    assert!(body.contains("\"prefix\":\"ss_live_"), "body={body}");
}

/// `DELETE /accounts/{id}`：非活跃 → 204；活跃 → 409；未知 → 404。
/// 活跃切到 bob 后，用 bob 的 bearer（alice 旧 bearer 已锁定）。
#[tokio::test]
async fn delete_account_rules() {
    let dir = tempfile::tempdir().unwrap();
    let port = spawn_app(dir.path()).await;
    let (_, body) = http_post(
        port,
        "/api/v1/auth/setup",
        r#"{"username":"alice","password":"p1"}"#,
        &[],
    )
    .await
    .unwrap();
    let alice_id = json_field(&body, "account_id").unwrap();
    let alice_auth = bearer_value(&json_field(&body, "secret").unwrap());
    // 追加 bob（活跃切到 bob）。
    let (_, body) = http_post(
        port,
        "/api/v1/accounts",
        r#"{"username":"bob","password":"p2"}"#,
        &[("Authorization", alice_auth.as_str())],
    )
    .await
    .unwrap();
    let bob_auth = bearer_value(&json_field(&body, "secret").unwrap());

    // 用 bob 的 bearer 列账户、取 bob id（bob 活跃）。
    let bob_id = {
        let (_, b) = http_get_with_headers(
            port,
            "/api/v1/accounts",
            &[("Authorization", bob_auth.as_str())],
        )
        .await
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&b).unwrap();
        v.as_array()
            .unwrap()
            .iter()
            .find(|a| a["username"] == "bob")
            .map(|a| a["id"].as_str().unwrap().to_string())
            .unwrap()
    };

    // 删活跃 bob → 409。
    let (code, body) = http_delete(
        port,
        &format!("/api/v1/accounts/{bob_id}"),
        bob_auth.as_str(),
    )
    .await
    .unwrap();
    assert_eq!(code, 409);
    assert!(body.contains("\"code\":\"conflict\""), "body={body}");
    assert!(
        body.contains("cannot delete active account; switch away first"),
        "body={body}"
    );

    // 删非活跃 alice → 204。
    let (code, _body) = http_delete(
        port,
        &format!("/api/v1/accounts/{alice_id}"),
        bob_auth.as_str(),
    )
    .await
    .unwrap();
    assert_eq!(code, 204);

    // 删未知 → 404。
    let (code, body) = http_delete(
        port,
        "/api/v1/accounts/00000000-0000-0000-0000-000000000000",
        bob_auth.as_str(),
    )
    .await
    .unwrap();
    assert_eq!(code, 404);
    assert!(body.contains("\"code\":\"not_found\""), "body={body}");
}

/// `DELETE /tokens/{id}`：存在 → 204；不存在 → 404。
#[tokio::test]
async fn revoke_token_flow() {
    let dir = tempfile::tempdir().unwrap();
    let port = spawn_app(dir.path()).await;
    let (_, body) = http_post(
        port,
        "/api/v1/auth/setup",
        r#"{"username":"alice","password":"p"}"#,
        &[],
    )
    .await
    .unwrap();
    let secret = json_field(&body, "secret").unwrap();
    let auth = bearer_value(&secret);
    let (_, b) = http_post(
        port,
        "/api/v1/tokens",
        r#"{"name":"ci","scopes":["sessions:read"]}"#,
        &[("Authorization", auth.as_str())],
    )
    .await
    .unwrap();
    let token_id = json_field(&b, "id").unwrap();

    let (code, _body) = http_delete(port, &format!("/api/v1/tokens/{token_id}"), auth.as_str())
        .await
        .unwrap();
    assert_eq!(code, 204);

    let (code, body) = http_delete(port, &format!("/api/v1/tokens/{token_id}"), auth.as_str())
        .await
        .unwrap();
    assert_eq!(code, 404);
    assert!(body.contains("\"code\":\"not_found\""), "body={body}");
}

/// `POST /auth/password`：root + 正确当前密码 → 200；错当前密码 → 403。
#[tokio::test]
async fn change_password_flow() {
    let dir = tempfile::tempdir().unwrap();
    let port = spawn_app(dir.path()).await;
    let (_, body) = http_post(
        port,
        "/api/v1/auth/setup",
        r#"{"username":"alice","password":"old"}"#,
        &[],
    )
    .await
    .unwrap();
    let secret = json_field(&body, "secret").unwrap();
    let auth = bearer_value(&secret);

    let (code, body) = http_post(
        port,
        "/api/v1/auth/password",
        r#"{"current_password":"wrong","new_password":"new"}"#,
        &[("Authorization", auth.as_str())],
    )
    .await
    .unwrap();
    assert_eq!(code, 403);
    assert!(body.contains("\"code\":\"wrong_password\""), "body={body}");

    let (code, _body) = http_post(
        port,
        "/api/v1/auth/password",
        r#"{"current_password":"old","new_password":"new"}"#,
        &[("Authorization", auth.as_str())],
    )
    .await
    .unwrap();
    assert_eq!(code, 200);
}

/// M7.2 provider/settings API：真实 router 鉴权、输入错误和 Probe 稳定码。
#[tokio::test]
async fn reasoning_configuration_routes_enforce_root_and_stable_errors() {
    let dir = tempfile::tempdir().unwrap();
    let port = spawn_app(dir.path()).await;
    let (_, body) = http_post(
        port,
        "/api/v1/auth/setup",
        r#"{"username":"alice","password":"p"}"#,
        &[],
    )
    .await
    .unwrap();
    let root_secret = json_field(&body, "secret").unwrap();
    let root_auth = bearer_value(&root_secret);

    let (code, body) = http_post(
        port,
        "/api/v1/tokens",
        r#"{"name":"reader","scopes":["sessions:read"]}"#,
        &[("Authorization", root_auth.as_str())],
    )
    .await
    .unwrap();
    assert_eq!(code, 201, "body={body}");
    let reader_auth = bearer_value(&json_field(&body, "secret").unwrap());
    let (code, body) = http_get_with_headers(
        port,
        "/api/v1/reasoning/provider-configs",
        &[("Authorization", reader_auth.as_str())],
    )
    .await
    .unwrap();
    assert_eq!(code, 403, "body={body}");
    assert!(
        body.contains("\"code\":\"insufficient_scope\""),
        "body={body}"
    );

    let invalid = r#"{"name":"Primary","provider_type":"openai","endpoint":"http://api.openai.com/v1","model":"gpt-test"}"#;
    let (code, body) = http_post(
        port,
        "/api/v1/reasoning/provider-configs",
        invalid,
        &[("Authorization", root_auth.as_str())],
    )
    .await
    .unwrap();
    assert_eq!(code, 400, "body={body}");
    assert!(body.contains("\"code\":\"bad_request\""), "body={body}");

    let valid = r#"{"name":"Primary","provider_type":"openai","endpoint":"https://api.openai.com/v1","model":"gpt-test"}"#;
    let (code, body) = http_post(
        port,
        "/api/v1/reasoning/provider-configs",
        valid,
        &[("Authorization", root_auth.as_str())],
    )
    .await
    .unwrap();
    assert_eq!(code, 201, "body={body}");
    assert!(!body.contains("api_key"), "body={body}");
    assert!(!body.contains("credential\":"), "body={body}");
    let config_id = json_field(&body, "id").unwrap();

    let (code, body) = http_post(
        port,
        &format!("/api/v1/reasoning/provider-configs/{config_id}/probe"),
        "{}",
        &[("Authorization", root_auth.as_str())],
    )
    .await
    .unwrap();
    assert_eq!(code, 422, "body={body}");
    assert!(
        body.contains("\"code\":\"cleanup_credential_missing\""),
        "body={body}"
    );
}

/// credential body 只进入专用 internal route；日志中只允许出现 method/path。
#[tokio::test]
async fn credential_internal_route_is_write_only_and_request_body_is_not_logged() {
    const SENTINEL: &str = "credential-sentinel-must-never-appear-in-logs";
    let dir = tempfile::tempdir().unwrap();
    let log_dir = dir.path().join("logs");
    let log_guard = seasnail_daemon::init_logging(&log_dir);
    let port = spawn_app(dir.path()).await;
    let (_, body) = http_post(
        port,
        "/api/v1/auth/setup",
        r#"{"username":"alice","password":"p"}"#,
        &[],
    )
    .await
    .unwrap();
    let root_auth = bearer_value(&json_field(&body, "secret").unwrap());
    let valid = r#"{"name":"Primary","provider_type":"openai","endpoint":"https://api.openai.com/v1","model":"gpt-test"}"#;
    let (_, body) = http_post(
        port,
        "/api/v1/reasoning/provider-configs",
        valid,
        &[("Authorization", root_auth.as_str())],
    )
    .await
    .unwrap();
    let config_id = json_field(&body, "id").unwrap();
    let path = format!("/api/v1/internal/reasoning/provider-configs/{config_id}/credential");
    let credential_body = format!(r#"{{"mode":"credential","credential":"{SENTINEL}"}}"#);
    let (code, body) = http_put(
        port,
        &path,
        &credential_body,
        &[("Authorization", root_auth.as_str())],
    )
    .await
    .unwrap();
    assert_eq!(code, 200, "body={body}");
    assert!(body.contains("\"credential_state\":\"bound\""));
    assert!(!body.contains(SENTINEL));

    let (code, _) = http_get_with_headers(port, &path, &[("Authorization", root_auth.as_str())])
        .await
        .unwrap();
    assert_eq!(code, 405, "credential must not have a read method");

    drop(log_guard);
    let logs = std::fs::read_dir(&log_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .collect::<String>();
    assert!(logs.contains("/internal/reasoning/provider-configs/"));
    assert!(
        !logs.contains(SENTINEL),
        "credential request body leaked to logs"
    );
}

/// M7.5：cleanup test 真实 HTTP 路由只允许 root，成功结果已校验且不产生历史会话。
#[tokio::test]
async fn cleanup_test_route_is_root_only_and_does_not_create_history() {
    let dir = tempfile::tempdir().unwrap();
    let upstream_port = spawn_cleanup_upstream(
        r#"{"cleaned_text":"正确词","corrections":[{"original_text":"错误词","corrected_text":"正确词","kind":"other_asr"}]}"#,
    )
    .await;
    let port = spawn_app(dir.path()).await;
    let (_, body) = http_post(
        port,
        "/api/v1/auth/setup",
        r#"{"username":"alice","password":"p"}"#,
        &[],
    )
    .await
    .unwrap();
    let root_auth = bearer_value(&json_field(&body, "secret").unwrap());
    let (_, body) = http_post(
        port,
        "/api/v1/tokens",
        r#"{"name":"reader","scopes":["sessions:read"]}"#,
        &[("Authorization", root_auth.as_str())],
    )
    .await
    .unwrap();
    let reader_auth = bearer_value(&json_field(&body, "secret").unwrap());
    let config = format!(
        r#"{{"name":"Local","provider_type":"openai_compatible_self_hosted_private","endpoint":"http://127.0.0.1:{upstream_port}/v1","model":"local-model"}}"#
    );
    let (code, body) = http_post(
        port,
        "/api/v1/reasoning/provider-configs",
        &config,
        &[("Authorization", root_auth.as_str())],
    )
    .await
    .unwrap();
    assert_eq!(code, 201, "body={body}");
    let config_id = json_field(&body, "id").unwrap();
    let credential_path =
        format!("/api/v1/internal/reasoning/provider-configs/{config_id}/credential");
    let (code, body) = http_put(
        port,
        &credential_path,
        r#"{"mode":"no_auth"}"#,
        &[("Authorization", root_auth.as_str())],
    )
    .await
    .unwrap();
    assert_eq!(code, 200, "body={body}");
    let request = format!(
        r#"{{"provider_config_id":"{config_id}","text":"错误词","prompt_draft":"DRAFT_PROMPT_SENTINEL"}}"#
    );
    let (code, body) = http_post(
        port,
        "/api/v1/cleanup/test",
        &request,
        &[("Authorization", reader_auth.as_str())],
    )
    .await
    .unwrap();
    assert_eq!(code, 403, "body={body}");
    assert!(body.contains("\"code\":\"insufficient_scope\""));

    let (code, body) = http_post(
        port,
        "/api/v1/cleanup/test",
        &request,
        &[("Authorization", root_auth.as_str())],
    )
    .await
    .unwrap();
    assert_eq!(code, 200, "body={body}");
    let result: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(result["cleaned_text"], "正确词");
    assert_eq!(result["corrections"][0]["kind"], "other_asr");
    assert!(result.get("upstream_body").is_none());

    let (code, body) = http_get_with_headers(
        port,
        "/api/v1/sessions",
        &[("Authorization", root_auth.as_str())],
    )
    .await
    .unwrap();
    assert_eq!(code, 200, "body={body}");
    let history: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(history["items"].as_array().map(Vec::len), Some(0));
}

async fn http_put(
    port: u16,
    path: &str,
    body: &str,
    headers: &[(&str, &str)],
) -> std::io::Result<(u16, String)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    let mut request = format!(
        "PUT {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("Connection: close\r\n\r\n");
    request.push_str(body);
    stream.write_all(request.as_bytes()).await?;
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await?;
    let text = String::from_utf8_lossy(&bytes);
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad status"))?;
    Ok((
        status,
        text.split("\r\n\r\n").nth(1).unwrap_or("").to_owned(),
    ))
}

/// raw TCP DELETE（harness 仅有 GET/POST，本地补 DELETE）。
async fn http_delete(port: u16, path: &str, auth_value: &str) -> std::io::Result<(u16, String)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    let req = format!(
        "DELETE {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {auth_value}\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).await?;
    let mut buf = Vec::with_capacity(8192);
    s.read_to_end(&mut buf).await?;
    let text = String::from_utf8_lossy(&buf);
    let status_line = text.split("\r\n").next().unwrap_or("");
    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bad status: {status_line}"),
            )
        })?;
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    Ok((code, body))
}
