//! Dictionary HTTP contract integration tests.

use std::sync::Arc;

use reqwest::{Client, Method, StatusCode};
use seasnail_crypto::{Argon2Params, KeychainStore, MemoryKeychain};
use seasnail_daemon::daemon_resources::DaemonResources;
use seasnail_daemon::{build_app, AppState, Auth, Crypto};
use serde_json::{json, Value};
use tokio::net::TcpListener;

fn fast_params() -> Argon2Params {
    Argon2Params {
        m_kib: 8192,
        t_cost: 1,
        p_cost: 1,
    }
}

async fn spawn_app(home: &std::path::Path) -> (Client, String) {
    let keychain = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
    let crypto = Arc::new(Crypto::new(home.to_path_buf(), keychain, fast_params()).unwrap());
    let auth = Arc::new(Auth::new(crypto));
    let state = DaemonResources::new(AppState::new(auth, home.to_path_buf())).http_state();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, build_app(state)).await;
    });
    (Client::new(), format!("http://{address}/api/v1"))
}

async fn setup_root(client: &Client, base: &str) -> String {
    let response = client
        .post(format!("{base}/auth/setup"))
        .json(&json!({"username": "alice", "password": "password"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    response.json::<Value>().await.unwrap()["secret"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn json_request(
    client: &Client,
    method: Method,
    url: &str,
    token: &str,
    body: Value,
) -> reqwest::Response {
    client
        .request(method, url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap()
}

async fn assert_error(response: reqwest::Response, status: StatusCode, code: &str) {
    assert_eq!(response.status(), status);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"]["code"],
        code
    );
}

#[tokio::test]
async fn dictionary_crud_cursor_and_error_contract() {
    let dir = tempfile::tempdir().unwrap();
    let (client, base) = spawn_app(dir.path()).await;
    let dictionary = format!("{base}/dictionary");

    assert_error(
        client.get(&dictionary).send().await.unwrap(),
        StatusCode::UNAUTHORIZED,
        "unauthorized",
    )
    .await;

    let root = setup_root(&client, &base).await;
    let malformed = client
        .post(format!("{dictionary}/entries"))
        .bearer_auth(&root)
        .header("content-type", "application/json")
        .body("{")
        .send()
        .await
        .unwrap();
    assert_error(malformed, StatusCode::BAD_REQUEST, "bad_request").await;

    let added = json_request(
        &client,
        Method::POST,
        &format!("{dictionary}/entries"),
        &root,
        json!({"terms": ["SeaSnail", " seasnail ", "OpenAI"]}),
    )
    .await;
    assert_eq!(added.status(), StatusCode::OK);
    let added = added.json::<Value>().await.unwrap();
    assert_eq!(added["added"].as_array().unwrap().len(), 2);
    assert_eq!(added["skipped_count"], 1);

    let page = client
        .get(format!("{dictionary}?limit=1"))
        .bearer_auth(&root)
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    let page = page.json::<Value>().await.unwrap();
    let cursor = page["next_cursor"].as_str().unwrap();

    assert_error(
        client
            .get(format!("{dictionary}?query=other&cursor={cursor}"))
            .bearer_auth(&root)
            .send()
            .await
            .unwrap(),
        StatusCode::BAD_REQUEST,
        "bad_request",
    )
    .await;

    let all = client
        .get(&dictionary)
        .bearer_auth(&root)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    let seasnail_id = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["term"] == "SeaSnail")
        .unwrap()["id"]
        .as_str()
        .unwrap();

    let conflict = json_request(
        &client,
        Method::PUT,
        &format!("{dictionary}/entries/{seasnail_id}"),
        &root,
        json!({"term": "OpenAI"}),
    )
    .await;
    assert_error(conflict, StatusCode::CONFLICT, "dictionary_conflict").await;

    let invalid_id = client
        .request(Method::DELETE, format!("{dictionary}/entries/not-a-uuid"))
        .bearer_auth(&root)
        .send()
        .await
        .unwrap();
    assert_error(invalid_id, StatusCode::BAD_REQUEST, "bad_request").await;

    let cleared = client
        .request(Method::DELETE, format!("{dictionary}/entries"))
        .bearer_auth(&root)
        .send()
        .await
        .unwrap();
    assert_eq!(cleared.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn dictionary_preview_import_export_and_limits() {
    let dir = tempfile::tempdir().unwrap();
    let (client, base) = spawn_app(dir.path()).await;
    let root = setup_root(&client, &base).await;
    let dictionary = format!("{base}/dictionary");
    let csv = "term\r\n\"hello,world\"\r\n\"say \"\"hi\"\"\"\r\n";

    let preview = json_request(
        &client,
        Method::POST,
        &format!("{dictionary}/imports/preview"),
        &root,
        json!({"csv": csv}),
    )
    .await;
    assert_eq!(preview.status(), StatusCode::OK);
    let preview = preview.json::<Value>().await.unwrap();
    assert_eq!(preview["parsed_count"], 2);
    assert_eq!(preview["valid_count"], 2);
    assert_eq!(preview["added_count"], 2);

    let empty = client
        .get(&dictionary)
        .bearer_auth(&root)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(empty["items"].as_array().unwrap().is_empty());

    let imported = json_request(
        &client,
        Method::POST,
        &format!("{dictionary}/imports"),
        &root,
        json!({"csv": csv}),
    )
    .await;
    assert_eq!(imported.status(), StatusCode::OK);
    assert_eq!(
        imported.json::<Value>().await.unwrap()["added"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let exported = client
        .get(format!("{dictionary}/export"))
        .bearer_auth(&root)
        .send()
        .await
        .unwrap();
    assert_eq!(exported.status(), StatusCode::OK);
    assert_eq!(
        exported.headers()["content-type"],
        "text/csv; charset=utf-8"
    );
    let exported = exported.text().await.unwrap();
    assert!(exported.contains("\"hello,world\""));
    assert!(exported.contains("\"say \"\"hi\"\"\""));

    let too_many = format!(
        "term\n{}",
        (0..=1000)
            .map(|index| format!("term-{index}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert_error(
        json_request(
            &client,
            Method::POST,
            &format!("{dictionary}/imports/preview"),
            &root,
            json!({"csv": too_many}),
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "dictionary_csv_invalid",
    )
    .await;

    let oversized = format!("term\n{}", "a".repeat(1024 * 1024));
    assert_error(
        json_request(
            &client,
            Method::POST,
            &format!("{dictionary}/imports/preview"),
            &root,
            json!({"csv": oversized}),
        )
        .await,
        StatusCode::PAYLOAD_TOO_LARGE,
        "request_too_large",
    )
    .await;
}

#[tokio::test]
async fn dictionary_rejects_non_root_token() {
    let dir = tempfile::tempdir().unwrap();
    let (client, base) = spawn_app(dir.path()).await;
    let root = setup_root(&client, &base).await;
    let token = json_request(
        &client,
        Method::POST,
        &format!("{base}/tokens"),
        &root,
        json!({"name": "reader", "scopes": ["sessions:read"]}),
    )
    .await;
    assert_eq!(token.status(), StatusCode::CREATED);
    let token = token.json::<Value>().await.unwrap()["secret"]
        .as_str()
        .unwrap()
        .to_owned();

    let non_root_get = client
        .get(format!("{base}/dictionary"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_error(non_root_get, StatusCode::FORBIDDEN, "insufficient_scope").await;

    for response in [
        json_request(
            &client,
            Method::POST,
            &format!("{base}/dictionary/entries"),
            &token,
            json!({"terms": ["term"]}),
        )
        .await,
        json_request(
            &client,
            Method::POST,
            &format!("{base}/dictionary/imports/preview"),
            &token,
            json!({"csv": "term\nterm\n"}),
        )
        .await,
        json_request(
            &client,
            Method::POST,
            &format!("{base}/dictionary/imports"),
            &token,
            json!({"csv": "term\nterm\n"}),
        )
        .await,
        json_request(
            &client,
            Method::PUT,
            &format!("{base}/dictionary/entries/00000000-0000-4000-8000-000000000001"),
            &token,
            json!({"term": "term"}),
        )
        .await,
    ] {
        assert_error(response, StatusCode::FORBIDDEN, "insufficient_scope").await;
    }
    for (method, path) in [
        (Method::DELETE, format!("{base}/dictionary/entries")),
        (
            Method::DELETE,
            format!("{base}/dictionary/entries/00000000-0000-4000-8000-000000000001"),
        ),
    ] {
        assert_error(
            client
                .request(method, path)
                .bearer_auth(&token)
                .send()
                .await
                .unwrap(),
            StatusCode::FORBIDDEN,
            "insufficient_scope",
        )
        .await;
    }
    assert_error(
        client
            .get(format!("{base}/dictionary/export"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap(),
        StatusCode::FORBIDDEN,
        "insufficient_scope",
    )
    .await;
}

#[tokio::test]
async fn learning_event_rejects_unknown_fields_and_caps_candidate_count() {
    let dir = tempfile::tempdir().unwrap();
    let (client, base) = spawn_app(dir.path()).await;
    let root = setup_root(&client, &base).await;
    let learning = format!("{base}/internal/dictionary/learning-events");

    // OpenAPI `additionalProperties: false`：未知字段必须是 400，而非静默忽略。
    let unknown_field = json_request(
        &client,
        Method::POST,
        &learning,
        &root,
        json!({"mode": "user_edit", "ticket": "dGlja2V0", "unexpected": true}),
    )
    .await;
    assert_error(unknown_field, StatusCode::BAD_REQUEST, "bad_request").await;

    // user_edit 候选最多 32 项；词条数量不合法按契约归 422（不是 400）。
    let too_many: Vec<String> = (0..33).map(|index| format!("term-{index}")).collect();
    let over_cap = json_request(
        &client,
        Method::POST,
        &learning,
        &root,
        json!({"mode": "user_edit", "ticket": "dGlja2V0", "candidates": too_many}),
    )
    .await;
    assert_error(
        over_cap,
        StatusCode::UNPROCESSABLE_ENTITY,
        "dictionary_invalid_term",
    )
    .await;

    // cleanup 模式附带候选仍是请求形状错误 400。
    let cleanup_with_candidates = json_request(
        &client,
        Method::POST,
        &learning,
        &root,
        json!({"mode": "cleanup", "ticket": "dGlja2V0", "candidates": ["term"]}),
    )
    .await;
    assert_error(
        cleanup_with_candidates,
        StatusCode::BAD_REQUEST,
        "bad_request",
    )
    .await;
}
