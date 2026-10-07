//! ST-M3.10 集成验证：`GET /models` / `POST /models/{id}` 模型管理面。
//!
//! 在进程内起真实 axum（`build_app` + 注入 runtime 句柄），用 reqwest 打 HTTP，
//! 验收十一点：
//!
//! 1. `GET /models` 列清单条目（whisper-tiny bundled+default / whisper-base 非 bundled），
//!    含 size_bytes / languages；无活跃 runtime → 无 active 状态。
//! 2. 预注册 mock（id=whisper-tiny）→ `GET /models` 该条 status=active。
//! 3. `POST /models/{未知 id} action=activate` → 404。
//! 4. 转译进行中（持 gate slot）→ `POST /models/whisper-base activate` → 409
//!    （切换会杀在飞 job 的 sidecar，与 POST /sessions 同 409 耦合）。
//! 5. 已激活同 id → `POST /models/{id} activate` → 幂等 202（不重启）。
//! 6. 路径未就绪（env 未设）→ `POST /models/whisper-base activate` → 409 download-first。
//! 7. `#[ignore]` env 设真实 whisper-server → activate 真链路（construct+start+register）。
//! 8. 无 bearer → 401（鉴权提取先于 scope 守门）。
//! 9. 仅 `sessions:read` 的第三方 token → 403 `insufficient_scope`。
//! 10. download 未 installed → 409（P1 未实现，明示）。
//! 11. 未知 action → 409（openapi 无 400/422，作「不支持」映射）。
//!
//! 不依赖 ffmpeg（models 端点不经 normalizer）。与 `sessions_pipeline.rs` 正交。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use seasnail_crypto::{Argon2Params, KeychainStore, MemoryKeychain};
use seasnail_daemon::daemon_resources::DaemonResources;
use seasnail_daemon::model_settings::ModelSettings;
use seasnail_daemon::{build_app, AppState, Auth, Crypto};
use seasnail_runtime::mock::CannedResponse;
use seasnail_runtime::{
    AudioNormalizer, MockRuntime, ModelRuntime, RuntimeKind, RuntimeOperation,
    RuntimeOperationGate, SidecarRegistry,
};
use tokio::net::TcpListener;

/// 测试用小 Argon2 参数（快）；非生产 DEFAULT。
fn fast_params() -> Argon2Params {
    Argon2Params {
        m_kib: 8192,
        t_cost: 1,
        p_cost: 1,
    }
}

/// 是否设了真实 whisper env（WHISPER_SERVER_PATH + WHISPER_MODEL_PATH 均非空）。
fn whisper_env_set() -> bool {
    std::env::var("WHISPER_SERVER_PATH")
        .map(|s| !s.is_empty())
        .unwrap_or(false)
        && std::env::var("WHISPER_MODEL_PATH")
            .map(|s| !s.is_empty())
            .unwrap_or(false)
}

/// 起进程内 axum：独立 tempdir + MemoryKeychain + 注入 runtime 句柄 + 建首账户活跃。
/// 返回 `(port, root bearer)`。normalizer 用占位路径（models 端点不经 normalizer）。
async fn spawn_app(
    home: &std::path::Path,
    registry: Arc<SidecarRegistry>,
    gate: Arc<RuntimeOperationGate>,
) -> (u16, String) {
    let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
    let crypto = Arc::new(Crypto::new(home.to_path_buf(), kc, fast_params()).unwrap());
    let t = crypto.setup_first_account("alice", "p").unwrap();
    let bearer = t.secret_bearer.clone();
    let auth = Arc::new(Auth::new(crypto.clone()));
    let normalizer = Arc::new(AudioNormalizer::new(PathBuf::from("ffmpeg")));
    let app_state =
        AppState::with_runtime_handles(auth, registry, gate, normalizer, home.to_path_buf());
    let app = build_app(DaemonResources::new(app_state).http_state());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (port, bearer)
}

fn app_state(home: &std::path::Path, registry: Arc<SidecarRegistry>) -> AppState {
    let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
    let crypto = Arc::new(Crypto::new(home.to_path_buf(), kc, fast_params()).unwrap());
    crypto.setup_first_account("alice", "p").unwrap();
    AppState::with_runtime_handles(
        Arc::new(Auth::new(crypto)),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new(PathBuf::from("ffmpeg"))),
        home.to_path_buf(),
    )
}

#[ignore = "requires SEASNAIL_SHERPA_REAL_ASR_ROOT"]
#[tokio::test]
async fn no_user_configuration_activates_real_sherpa_default() {
    let Some(root) = std::env::var_os("SEASNAIL_SHERPA_REAL_ASR_ROOT") else {
        return;
    };
    std::env::set_var("SEASNAIL_ASR_ROOT", root);
    let home = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let resources = DaemonResources::new(app_state(home.path(), registry.clone()));
    resources.activate_default_model().await.unwrap();
    assert_eq!(
        registry.active_id().await.as_deref(),
        Some("sensevoice-small-sherpa-int8")
    );
    registry.active().await.unwrap().stop().await.unwrap();
    std::env::remove_var("SEASNAIL_ASR_ROOT");
}

#[cfg(not(feature = "debug-backend-switching"))]
#[ignore = "requires SEASNAIL_SHERPA_REAL_ASR_ROOT"]
#[tokio::test]
async fn saved_legacy_id_is_ignored_by_release_and_recovers_to_sherpa() {
    let Some(root) = std::env::var_os("SEASNAIL_SHERPA_REAL_ASR_ROOT") else {
        return;
    };
    std::env::set_var("SEASNAIL_ASR_ROOT", root);
    let home = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    ModelSettings::new(home.path().to_path_buf())
        .set_backend("sensevoice-small")
        .unwrap();
    let app_state = app_state(home.path(), registry.clone());
    let resources = DaemonResources::new(app_state);
    resources.activate_default_model().await.unwrap();
    assert_eq!(
        registry.active_id().await.as_deref(),
        Some("sensevoice-small-sherpa-int8")
    );
    registry.active().await.unwrap().stop().await.unwrap();
    std::env::remove_var("SEASNAIL_ASR_ROOT");
}

#[cfg(feature = "debug-backend-switching")]
#[ignore = "requires SEASNAIL_SHERPA_REAL_ASR_ROOT"]
#[tokio::test]
async fn stale_saved_id_recovers_to_real_bundled_default() {
    let Some(root) = std::env::var_os("SEASNAIL_SHERPA_REAL_ASR_ROOT") else {
        return;
    };
    std::env::set_var("SEASNAIL_ASR_ROOT", root);
    let home = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    ModelSettings::new(home.path().to_path_buf())
        .set_backend("removed-catalog-id")
        .unwrap();
    let app_state = app_state(home.path(), registry.clone());
    let resources = DaemonResources::new(app_state);
    resources.activate_default_model().await.unwrap();
    assert_eq!(
        registry.active_id().await.as_deref(),
        Some("sensevoice-small-sherpa-int8")
    );
    registry.active().await.unwrap().stop().await.unwrap();
    std::env::remove_var("SEASNAIL_ASR_ROOT");
}

/// 起 app 并返 auth 句柄（签发受限 scope 第三方 token 用，验 403 路径）。
/// 其余与 [`spawn_app`] 同。
async fn spawn_app_with_auth(
    home: &std::path::Path,
    registry: Arc<SidecarRegistry>,
    gate: Arc<RuntimeOperationGate>,
) -> (u16, Arc<Auth>, String) {
    let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
    let crypto = Arc::new(Crypto::new(home.to_path_buf(), kc, fast_params()).unwrap());
    let t = crypto.setup_first_account("alice", "p").unwrap();
    let bearer = t.secret_bearer.clone();
    let auth = Arc::new(Auth::new(crypto.clone()));
    let normalizer = Arc::new(AudioNormalizer::new(PathBuf::from("ffmpeg")));
    let app_state = AppState::with_runtime_handles(
        auth.clone(),
        registry,
        gate,
        normalizer,
        home.to_path_buf(),
    );
    let app = build_app(DaemonResources::new(app_state).http_state());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (port, auth, bearer)
}

/// `GET /api/v1/models` → `(状态码, body JSON)`。
async fn get_models(port: u16, bearer: &str) -> (u16, serde_json::Value) {
    let resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/api/v1/models"))
        .bearer_auth(bearer)
        .send()
        .await
        .unwrap();
    let code = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap();
    (code, body)
}

/// `POST /api/v1/models/{id}` body `{action}` → `(状态码, body JSON)`。
async fn post_model_action(
    port: u16,
    bearer: &str,
    id: &str,
    action: &str,
) -> (u16, serde_json::Value) {
    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/api/v1/models/{id}"))
        .bearer_auth(bearer)
        .json(&serde_json::json!({ "action": action }))
        .send()
        .await
        .unwrap();
    let code = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap();
    (code, body)
}

/// 由数组取指定 id 的条目。
fn find_model<'a>(arr: &'a serde_json::Value, id: &str) -> Option<&'a serde_json::Value> {
    arr.as_array()?
        .iter()
        .find(|v| v.get("id").and_then(|i| i.as_str()) == Some(id))
}

/// `PUT /api/v1/models/{component}` body `{enabled}` → 状态码。
async fn put_component(port: u16, bearer: &str, component: &str, enabled: bool) -> u16 {
    let resp = reqwest::Client::new()
        .put(format!("http://127.0.0.1:{port}/api/v1/models/{component}"))
        .bearer_auth(bearer)
        .json(&serde_json::json!({ "enabled": enabled }))
        .send()
        .await
        .unwrap();
    resp.status().as_u16()
}

/// `POST /api/v1/models/{component}/download` → 状态码（M4 按需下载端点）。
async fn post_download(port: u16, bearer: &str, component: &str) -> u16 {
    let resp = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{port}/api/v1/models/{component}/download"
        ))
        .bearer_auth(bearer)
        .send()
        .await
        .unwrap();
    resp.status().as_u16()
}

/// Component 端点独立于正式模型 catalog，仍保持既有状态码契约。
#[tokio::test]
async fn component_toggle_contract_is_preserved() {
    let home = tempfile::tempdir().unwrap();
    let (port, bearer) = spawn_app(
        home.path(),
        Arc::new(SidecarRegistry::new()),
        Arc::new(RuntimeOperationGate::new_for_test()),
    )
    .await;
    assert_eq!(put_component(port, &bearer, "punc", true).await, 422);
    assert_eq!(put_component(port, &bearer, "punc", false).await, 202);
    assert_eq!(
        put_component(port, &bearer, "unknown", true).await,
        404,
        "未知 component → 404"
    );
    assert_eq!(put_component(port, &bearer, "spk", true).await, 422);
    std::fs::create_dir_all(home.path().join("models/punc")).unwrap();
    assert_eq!(put_component(port, &bearer, "punc", true).await, 202);
}

/// 1. `GET /models` 仅列出具有可信 artifact manifest 锚点的正式 Sherpa 条目；
///    无活跃 runtime → 无条目 status=active。
#[tokio::test]
async fn get_models_lists_catalog_entries() {
    let home = tempfile::tempdir().unwrap();
    let (port, bearer) = spawn_app(
        home.path(),
        Arc::new(SidecarRegistry::new()),
        Arc::new(RuntimeOperationGate::new_for_test()),
    )
    .await;
    let (code, body) = get_models(port, &bearer).await;
    assert_eq!(code, 200);
    let arr = body.as_array().expect("应为数组");
    assert_eq!(arr.len(), 1, "无可信锚点的旧候选不得进入正式 catalog");

    let sherpa = find_model(&body, "sensevoice-small-sherpa-int8").expect("应含 Sherpa default");
    assert_eq!(sherpa["runtime"], "sherpa_onnx");
    assert_eq!(sherpa["bundled"], true);
    assert_eq!(sherpa["default"], true, "Sherpa 应为唯一默认模型");
    assert_eq!(sherpa["size_bytes"], 269138669_u64);
}

#[tokio::test]
#[cfg(feature = "debug-backend-switching")]
async fn legacy_gguf_without_trusted_catalog_anchor_is_not_selectable() {
    let home = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let old = Arc::new(MockRuntime::new(
        "funasr-default",
        RuntimeKind::FunAsr,
        CannedResponse::default(),
    )) as Arc<dyn ModelRuntime>;
    old.start(0).await.unwrap();
    registry.register(old.clone()).await;
    let (port, bearer) = spawn_app(
        home.path(),
        registry.clone(),
        Arc::new(RuntimeOperationGate::new_for_test()),
    )
    .await;
    let (code, body) = post_model_action(port, &bearer, "sensevoice-small", "activate").await;
    assert_eq!(code, 404, "body={body}");
    assert_eq!(
        registry.active_id().await.as_deref(),
        Some("funasr-default")
    );
    assert!(
        old.health().await,
        "artifact rejection must not stop the old runtime"
    );
    old.stop().await.unwrap();
}

/// 2. 预注册正式 Sherpa mock → `GET /models` 该条 status=active。
#[tokio::test]
async fn get_models_marks_registered_entry_active() {
    let home = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::new(
        "sensevoice-small-sherpa-int8",
        RuntimeKind::SherpaOnnx,
        CannedResponse::default(),
    )) as Arc<dyn ModelRuntime>;
    registry.register(mock).await;
    let (port, bearer) = spawn_app(
        home.path(),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
    )
    .await;

    let (code, body) = get_models(port, &bearer).await;
    assert_eq!(code, 200);
    let sherpa = find_model(&body, "sensevoice-small-sherpa-int8").expect("应含 Sherpa");
    assert_eq!(sherpa["status"], "active", "已注册 Sherpa 应 active");
}

/// 3. activate 未知 id → 404。
#[tokio::test]
async fn activate_unknown_model_returns_404() {
    let home = tempfile::tempdir().unwrap();
    let (port, bearer) = spawn_app(
        home.path(),
        Arc::new(SidecarRegistry::new()),
        Arc::new(RuntimeOperationGate::new_for_test()),
    )
    .await;
    let (code, body) = post_model_action(port, &bearer, "no-such-model", "activate").await;
    assert_eq!(code, 404);
    assert!(
        body["error"]["code"]
            .as_str()
            .unwrap_or("")
            .contains("not_found")
            || body["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("not found")
    );
}

/// 4. 转译进行中（持 gate slot）→ activate → 409（不切换杀在飞 job）。
#[tokio::test]
async fn activate_while_transcribing_returns_409() {
    let home = tempfile::tempdir().unwrap();
    let gate = Arc::new(RuntimeOperationGate::new_for_test());
    let (port, bearer) =
        spawn_app(home.path(), Arc::new(SidecarRegistry::new()), gate.clone()).await;
    // 占 slot 模拟转译进行中（RuntimeLease 持续到函数末）。
    let _guard = gate
        .acquire(RuntimeOperation::Transcription {
            session_id: "in-flight-session".into(),
        })
        .unwrap();
    let (code, _body) =
        post_model_action(port, &bearer, "sensevoice-small-sherpa-int8", "activate").await;
    assert_eq!(code, 409);
    // _guard drop 于此释放 slot。
}

/// 5. 已激活同 id → activate 幂等 202（不重启）。
#[tokio::test]
async fn activate_idempotent_when_already_active() {
    let home = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::new(
        "sensevoice-small-sherpa-int8",
        RuntimeKind::SherpaOnnx,
        CannedResponse::default(),
    )) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap();
    registry.register(mock).await;
    let (port, bearer) = spawn_app(
        home.path(),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
    )
    .await;

    let (code, body) =
        post_model_action(port, &bearer, "sensevoice-small-sherpa-int8", "activate").await;
    assert_eq!(code, 202);
    assert_eq!(body["id"], "sensevoice-small-sherpa-int8");
    assert_eq!(body["status"], "active", "幂等激活仍应 active");
}

/// 6. 路径未就绪（env 未设）→ activate → 409 download-first。
/// 默认 Sherpa artifact 未打包时必须明确 409。
#[tokio::test]
async fn activate_not_installed_returns_409() {
    let home = tempfile::tempdir().unwrap();
    let (port, bearer) = spawn_app(
        home.path(),
        Arc::new(SidecarRegistry::new()),
        Arc::new(RuntimeOperationGate::new_for_test()),
    )
    .await;
    let (code, body) =
        post_model_action(port, &bearer, "sensevoice-small-sherpa-int8", "activate").await;
    assert_eq!(code, 409);
    let msg = body["error"]["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("not installed"),
        "应明示 not installed，实际: {msg}"
    );
}

/// 7. 没有可信 artifact 锚点的 FunASR 不进入正式 catalog。
#[tokio::test]
#[cfg(feature = "debug-backend-switching")]
async fn funasr_without_trusted_catalog_anchor_is_not_selectable() {
    if std::env::var_os("SEASNAIL_FUNASR_ROOT").is_some() {
        eprintln!("已设 SEASNAIL_FUNASR_ROOT，跳过缺 bundle 回归测试");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let (port, bearer) = spawn_app(
        home.path(),
        Arc::new(SidecarRegistry::new()),
        Arc::new(RuntimeOperationGate::new_for_test()),
    )
    .await;
    let (code, body) = post_model_action(port, &bearer, "funasr-default", "activate").await;
    assert_eq!(code, 404, "body={body}");
}

/// 8.（#[ignore]）真实 whisper-server：activate 真链路（construct+start+register）。
/// 需 `WHISPER_SERVER_PATH` + `WHISPER_MODEL_PATH` env；`cargo test --ignored` 手动跑。
/// env 未设时静默跳过（不 fail）。
#[ignore]
#[tokio::test]
async fn activate_real_whisper_starts_and_registers() {
    if !whisper_env_set() {
        eprintln!("未设 WHISPER_SERVER_PATH/WHISPER_MODEL_PATH，跳过真实 whisper activate");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let (port, bearer) = spawn_app(
        home.path(),
        registry.clone(),
        Arc::new(RuntimeOperationGate::new_for_test()),
    )
    .await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap();
    let resp = client
        .post(format!(
            "http://127.0.0.1:{port}/api/v1/models/whisper-tiny"
        ))
        .bearer_auth(&bearer)
        .json(&serde_json::json!({ "action": "activate" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 202);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["id"], "whisper-tiny");
    assert_eq!(body["status"], "active");
    // registry 侧确认 active_id 已注册。
    assert_eq!(registry.active_id().await.as_deref(), Some("whisper-tiny"));
}

/// 8. 无 bearer → 401（`AuthedCaller` 提取先于 scope 守门）。
#[tokio::test]
async fn activate_without_bearer_returns_401() {
    let home = tempfile::tempdir().unwrap();
    let (port, _bearer) = spawn_app(
        home.path(),
        Arc::new(SidecarRegistry::new()),
        Arc::new(RuntimeOperationGate::new_for_test()),
    )
    .await;
    // 不带 Authorization 头 → AuthedCaller 提取失败 → 401 unauthorized。
    let resp = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{port}/api/v1/models/whisper-base"
        ))
        .json(&serde_json::json!({ "action": "activate" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
    // 验完整错误契约（status + code）：仅断 401 不足以察觉变体漂移
    // （若提取器误返 Forbidden→403 则 401≠403 能抓，但若误返同 401 的别 code 则漏）。
    let body: serde_json::Value = resp.json().await.unwrap();
    let code_str = body["error"]["code"].as_str().unwrap_or("");
    assert!(
        code_str.contains("unauthorized"),
        "应 unauthorized，实际: {code_str}"
    );
}

/// 9. 仅 `sessions:read`（无 write）的第三方 token → 403 `insufficient_scope`。
#[tokio::test]
async fn activate_with_insufficient_scope_returns_403() {
    let home = tempfile::tempdir().unwrap();
    let (port, auth, bearer) = spawn_app_with_auth(
        home.path(),
        Arc::new(SidecarRegistry::new()),
        Arc::new(RuntimeOperationGate::new_for_test()),
    )
    .await;
    // 用 root 签发仅 sessions:read 的第三方 token（无 sessions:write）。
    let caller = auth.verify(&bearer).unwrap();
    let third = auth
        .issue_token(&caller, "ci", vec!["sessions:read".into()])
        .unwrap();
    assert!(!third.is_root, "第三方 token 非 root");

    let (code, body) =
        post_model_action(port, &third.secret_bearer, "whisper-base", "activate").await;
    assert_eq!(code, 403, "无 sessions:write 应 403");
    let code_str = body["error"]["code"].as_str().unwrap_or("");
    assert!(
        code_str.contains("insufficient_scope"),
        "应 insufficient_scope，实际: {code_str}"
    );
}

/// 10. 未知 action → 409（openapi 无 400/422，未知动作作「不支持」映射 409）。
#[tokio::test]
async fn unknown_action_returns_409() {
    let home = tempfile::tempdir().unwrap();
    let (port, bearer) = spawn_app(
        home.path(),
        Arc::new(SidecarRegistry::new()),
        Arc::new(RuntimeOperationGate::new_for_test()),
    )
    .await;
    let (code, body) =
        post_model_action(port, &bearer, "sensevoice-small-sherpa-int8", "install").await;
    assert_eq!(code, 409);
    let code_str = body["error"]["code"].as_str().unwrap_or("");
    assert!(
        code_str.contains("conflict"),
        "应 conflict，实际: {code_str}"
    );
    let msg = body["error"]["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("unsupported action"),
        "应明示 unsupported action，实际: {msg}"
    );
}

/// Component 下载面独立于正式模型 catalog，保持既有状态码契约。
#[tokio::test]
async fn download_endpoint_validation() {
    if std::env::var_os("SEASNAIL_FUNASR_ROOT").is_some() {
        eprintln!("已设 SEASNAIL_FUNASR_ROOT，跳过 download 端点校验（需无 bundle 才测 503）");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let (port, bearer) = spawn_app(
        home.path(),
        Arc::new(SidecarRegistry::new()),
        Arc::new(RuntimeOperationGate::new_for_test()),
    )
    .await;
    assert_eq!(post_download(port, &bearer, "spk").await, 422);
    assert_eq!(
        post_download(port, &bearer, "unknown").await,
        404,
        "未知 component → 404"
    );
    assert_eq!(post_download(port, &bearer, "punc").await, 503);
    assert_eq!(post_download(port, &bearer, "punc").await, 503);
}
