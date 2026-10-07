//! ST-M3.5 集成验证：`POST /sessions` 主转译链路端到端。
//!
//! 在进程内起真实 axum（`build_app` + 注入 runtime 句柄），用 reqwest multipart 上传
//! 音频，验四点验收：
//!
//! 1. 成功链路：202 transcribing → 后台 normalize→sidecar transcribe→建 proto→
//!    encrypt 落库 → 行推进 completed；解密 transcript 断言 canned「你好世界」单段。
//! 2. 失败链路：注册未启动 mock（transcribe→NotStarted）→ 行推进 failed，**音频保留**
//!    （audio_path 仍 Some 且可解密读回）。**不依赖 ffmpeg**：有 ffmpeg→normalize ok 但
//!    transcribe 失败；无 ffmpeg→normalize 失败；两者都 → failed。
//! 3. 无活跃 runtime → 409（先于 slot 占用）。
//! 4. slot 占用中第二请求 → 409。
//!
//! imported 成功链路需 ffmpeg（normalize）；realtime 成功链路使用规范 WAV，不依赖 ffmpeg。
//! 与 `tests/auth_endpoints.rs`（鉴权 HTTP 面）正交；与 `tests/m2_integration.rs`
//! （跨账户隔离，无 ASR）正交——本测试首次串联 runtime 句柄到真实 OpenAPI 端点。

use std::env;
use std::f32::consts::PI;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{extract::Json, routing::post, Router};
use prost::Message;
use seasnail_crypto::{Argon2Params, KeychainStore, MemoryKeychain};
use seasnail_daemon::account::Storage;
use seasnail_daemon::cleanup::CleanupService;
use seasnail_daemon::daemon_resources::DaemonResources;
use seasnail_daemon::reasoning::service::ReasoningService;
use seasnail_daemon::{build_app, AppState, Auth, Crypto};
use seasnail_proto::seasnail::v1::{
    CleanupContextPlacement, ClipboardContextFile, ContextEvent, ContextEventKind, Correction,
    CorrectionKind, DiagnosticCaptureStatus, Source,
};
use seasnail_runtime::mock::CannedResponse;
use seasnail_runtime::{
    AudioNormalizer, MockRuntime, ModelRuntime, RuntimeKind, RuntimeOperation,
    RuntimeOperationGate, SidecarRegistry,
};
use seasnail_storage::{ProviderConfigInput, ProviderType, SessionRow};
use tokio::net::TcpListener;
use tokio::process::Command;

/// 测试用小 Argon2 参数（快）；非生产 DEFAULT。
fn fast_params() -> Argon2Params {
    Argon2Params {
        m_kib: 8192,
        t_cost: 1,
        p_cost: 1,
    }
}

/// 解析 ffmpeg：env `FFMPEG_PATH` → PATH `ffmpeg`（`-version` 探测）→ None。
/// 复用 `normalizer_real` 探测策略（非 `#[ignore]`，常见即跑、未装即 skip 成功链路）。
async fn resolve_ffmpeg() -> Option<PathBuf> {
    if let Ok(p) = env::var("FFMPEG_PATH") {
        return Some(PathBuf::from(p));
    }
    let probe = Command::new("ffmpeg").arg("-version").output().await.ok()?;
    if probe.status.success() {
        Some(PathBuf::from("ffmpeg"))
    } else {
        None
    }
}

/// 返回测试专用的「ffmpeg」替身：直接将 `-i` 指定的输入复制到命令末尾输出。
/// `sine_wav` 已是目标 16kHz mono WAV，因此足以让 pipeline 到达 runtime；避免 M4.5
/// 验收受暂缓的 M3.5 ffmpeg `LIST/INFO` WAV 解析问题影响。
#[cfg(unix)]
fn copying_normalizer_binary(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join("copying-normalizer.sh");
    std::fs::write(
        &path,
        "#!/bin/sh\ninput=\noutput=\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = -i ]; then\n    input=$2\n    shift 2\n  else\n    output=$1\n    shift\n  fi\ndone\ncp \"$input\" \"$output\"\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

#[cfg(unix)]
fn slow_copying_normalizer_binary(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join("slow-copying-normalizer.sh");
    std::fs::write(
        &path,
        "#!/bin/sh\ninput=\noutput=\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = -i ]; then\n    input=$2\n    shift 2\n  else\n    output=$1\n    shift\n  fi\ndone\nsleep 1\ncp \"$input\" \"$output\"\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

/// 1s 440Hz sine @ 16kHz mono f32 → 完整 WAV bytes（`from_pcm` 封头，不经 ffmpeg）。
/// 喂 `/sessions`：已是目标格式，ffmpeg normalize 直通；mock transcribe 不消费内容。
fn sine_wav() -> Vec<u8> {
    let pcm: Vec<f32> = (0..16_000)
        .map(|i| {
            let x = (i as f32) * 2.0 * PI * 440.0 / 16_000.0;
            x.sin() * 0.5
        })
        .collect();
    AudioNormalizer::from_pcm(&pcm, 16_000)
}

/// 起进程内 axum：独立 tempdir + `MemoryKeychain` + 注入 runtime 句柄，建首账户活跃。
/// 返回 `(port, 数据面句柄, auth 句柄, root bearer)`。`storage` 与 `AppState` 内部 Storage
/// 共享同一 `Arc<Crypto>`（同一 data_dir / DB），故测试侧可直查行状态推进（轮询 GET）。
/// `auth` 供签发受限 scope 的第三方 token（验 `require_scope` 403 路径）。
async fn spawn_app_with(
    home: &Path,
    registry: Arc<SidecarRegistry>,
    gate: Arc<RuntimeOperationGate>,
    normalizer: Arc<AudioNormalizer>,
) -> (u16, Arc<Storage>, Arc<Auth>, String) {
    spawn_app_with_cleanup(
        home,
        registry,
        gate,
        normalizer,
        Arc::new(CleanupService::default()),
    )
    .await
}

async fn spawn_app_with_cleanup(
    home: &Path,
    registry: Arc<SidecarRegistry>,
    gate: Arc<RuntimeOperationGate>,
    normalizer: Arc<AudioNormalizer>,
    cleanup: Arc<CleanupService>,
) -> (u16, Arc<Storage>, Arc<Auth>, String) {
    let kc = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
    let crypto = Arc::new(Crypto::new(home.to_path_buf(), kc, fast_params()).unwrap());
    let t = crypto.setup_first_account("alice", "p").unwrap();
    let bearer = t.secret_bearer.clone();
    let auth = Arc::new(Auth::new(crypto.clone()));
    let storage = Arc::new(Storage::new(crypto.clone()));
    // auth.clone()：AppState 持一份，测试侧持一份（签发第三方 token 用）。
    let app_state = AppState::with_runtime_handles_and_cleanup(
        auth.clone(),
        registry,
        gate,
        normalizer,
        home.to_path_buf(),
        cleanup,
    );
    let app = build_app(DaemonResources::new(app_state).http_state());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (port, storage, auth, bearer)
}

/// 使用既有已解锁账户重建完整 HTTP/application graph。新的 CleanupService 不含任何
/// presentation cache，等价覆盖 daemon 重启后的读取语义。
async fn spawn_fresh_app_graph(home: &Path, auth: Arc<Auth>) -> u16 {
    let app_state = AppState::with_runtime_handles(
        auth,
        Arc::new(SidecarRegistry::new()),
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new("ffmpeg-not-used-for-read-path".into())),
        home.to_path_buf(),
    );
    let app = build_app(DaemonResources::new(app_state).http_state());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    port
}

async fn spawn_cleanup_server(delay: Duration) -> u16 {
    spawn_capturing_cleanup_server(delay, CleanupResponseMode::Echo)
        .await
        .0
}

#[derive(Clone, Copy)]
enum CleanupResponseMode {
    Echo,
    DropFirstMarker,
}

async fn spawn_capturing_cleanup_server(
    delay: Duration,
    mode: CleanupResponseMode,
) -> (u16, Arc<Mutex<Vec<serde_json::Value>>>) {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let captured_requests = Arc::clone(&captured);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<serde_json::Value>| {
            let captured_requests = Arc::clone(&captured_requests);
            async move {
                tokio::time::sleep(delay).await;
                captured_requests.lock().unwrap().push(body.clone());
                let user_json = body
                    .pointer("/messages/1/content")
                    .and_then(serde_json::Value::as_str)
                    .expect("cleanup request must contain user JSON");
                let user: serde_json::Value = serde_json::from_str(user_json).unwrap();
                let transcript = user
                    .get("transcript")
                    .and_then(serde_json::Value::as_str)
                    .expect("cleanup request must contain markerized transcript");
                let cleaned_text = match mode {
                    CleanupResponseMode::Echo => transcript.to_owned(),
                    CleanupResponseMode::DropFirstMarker => drop_first_marker(transcript),
                };
                let content = serde_json::json!({
                    "cleaned_text": cleaned_text,
                    "corrections": [],
                });
                Json(serde_json::json!({
                    "choices": [{
                        "finish_reason": "stop",
                        "message": {"content": serde_json::to_string(&content).unwrap()}
                    }]
                }))
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (port, captured)
}

fn drop_first_marker(transcript: &str) -> String {
    let Some(start) = transcript.find("[[SEASNAIL_CTX_V1:") else {
        return transcript.to_owned();
    };
    let Some(relative_end) = transcript[start..].find("]]") else {
        return transcript.to_owned();
    };
    let end = start + relative_end + 2;
    format!("{}{}", &transcript[..start], &transcript[end..])
}

fn enable_cleanup(storage: &Storage, port: u16) {
    let endpoint = format!("http://127.0.0.1:{port}/v1");
    let provider = storage
        .create_provider_config(
            ProviderConfigInput {
                name: "local test provider",
                provider_type: ProviderType::SelfHostedPrivate,
                endpoint: &endpoint,
                model: "cleanup-test-model",
            },
            1,
        )
        .unwrap();
    assert_eq!(
        storage.set_provider_no_auth(&provider.id).unwrap(),
        seasnail_daemon::account::CredentialState::NotRequired
    );
    assert!(
        storage
            .save_cleanup_settings(true, Some(&provider.id), None, 2)
            .unwrap()
            .enabled
    );
}

/// reqwest multipart 上传音频（source=imported, language=zh）→ `(状态码, body)`。
async fn post_sessions(port: u16, bearer: &str, wav: &[u8]) -> (u16, String) {
    post_sessions_with_filename(port, bearer, wav, "audio.wav").await
}

async fn post_sessions_with_filename(
    port: u16,
    bearer: &str,
    wav: &[u8],
    file_name: &str,
) -> (u16, String) {
    let part = reqwest::multipart::Part::bytes(wav.to_vec())
        .file_name(file_name.to_owned())
        .mime_str("audio/wav")
        .unwrap();
    let form = reqwest::multipart::Form::new()
        .part("audio", part)
        .text("source", "imported")
        .text("language", "zh");
    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/api/v1/sessions"))
        .bearer_auth(bearer)
        .multipart(form)
        .send()
        .await
        .unwrap();
    let code = resp.status().as_u16();
    let body = resp.text().await.unwrap();
    (code, body)
}

async fn post_sessions_with_context(
    port: u16,
    bearer: &str,
    wav: &[u8],
    context: ClipboardContextFile,
) -> (u16, String) {
    let audio = reqwest::multipart::Part::bytes(wav.to_vec())
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .unwrap();
    let form = reqwest::multipart::Form::new()
        .part("audio", audio)
        .part(
            "clipboard_context",
            reqwest::multipart::Part::bytes(context.encode_to_vec()),
        )
        .text("source", "realtime")
        .text("language", "zh");
    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/api/v1/sessions"))
        .bearer_auth(bearer)
        .multipart(form)
        .send()
        .await
        .unwrap();
    (response.status().as_u16(), response.text().await.unwrap())
}

async fn add_dictionary_term(port: u16, bearer: &str, term: &str) {
    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/api/v1/dictionary/entries"))
        .bearer_auth(bearer)
        .json(&serde_json::json!({"terms": [term]}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status().as_u16(),
        200,
        "{}",
        response.text().await.unwrap()
    );
}

async fn post_learning(
    port: u16,
    bearer: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    let response = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{port}/api/v1/internal/dictionary/learning-events"
        ))
        .bearer_auth(bearer)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body = response.json().await.unwrap();
    (status, body)
}

async fn undo_learning(port: u16, bearer: &str, event_id: &str) -> (u16, serde_json::Value) {
    let response = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{port}/api/v1/internal/dictionary/learning-events/{event_id}/undo"
        ))
        .bearer_auth(bearer)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body = response.json().await.unwrap();
    (status, body)
}

/// 从 202 body 取 `id`。
fn extract_id(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("id")?.as_str().map(String::from)
}

/// 轮询 Storage 直查行状态至非 transcribing（completed/failed）或超时返 None。
/// 失败链路 handler 已先 INSERT transcribing 行；故行必然可见，poll 仅等推进。
async fn wait_outcome(storage: &Storage, id: &str, timeout: Duration) -> Option<SessionRow> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(Some(row)) = storage.get(id) {
            if row.status != "transcribing" {
                return Some(row);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_cleanup_terminal(
    storage: &Storage,
    id: &str,
    timeout: Duration,
) -> Option<SessionRow> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(Some(row)) = storage.get(id) {
            if row.status == "completed" && row.cleanup_status != "processing" {
                return Some(row);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_cleanup_processing(storage: &Storage, id: &str, timeout: Duration) -> SessionRow {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(Some(row)) = storage.get(id) {
            if row.status == "cleaning_up" && row.cleanup_status == "processing" {
                return row;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "cleanup did not enter cleaning_up/processing"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn cleanup_test_context() -> ClipboardContextFile {
    ClipboardContextFile {
        schema_version: 1,
        session_id: String::new(),
        capture_id: uuid::Uuid::new_v4().to_string(),
        events: vec![ContextEvent {
            sequence: 1,
            source_sample_rate: 48_000,
            sample_offset: 24_000,
            kind: ContextEventKind::ContextEventPlainText as i32,
            plain_text: "secret clipboard".into(),
            html_fragment: String::new(),
            absolute_paths: Vec::new(),
        }],
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn dictionary_snapshot_is_frozen_before_background_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap();
    registry.register(mock).await;
    let (cleanup_port, captured) =
        spawn_capturing_cleanup_server(Duration::from_millis(300), CleanupResponseMode::Echo).await;
    let (port, storage, _auth, bearer) = spawn_app_with_cleanup(
        dir.path(),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new("ffmpeg".into())),
        Arc::new(CleanupService::new(Arc::new(ReasoningService::new()))),
    )
    .await;
    enable_cleanup(&storage, cleanup_port);
    add_dictionary_term(port, &bearer, "FrozenTerm").await;

    let (code, body) =
        post_sessions_with_context(port, &bearer, &sine_wav(), cleanup_test_context()).await;
    assert_eq!(code, 202, "body={body}");
    let id = extract_id(&body).unwrap();
    add_dictionary_term(port, &bearer, "LateTerm").await;
    wait_cleanup_terminal(&storage, &id, Duration::from_secs(10))
        .await
        .expect("cleanup terminal");

    let requests = captured.lock().unwrap();
    let user_json = requests[0]
        .pointer("/messages/1/content")
        .and_then(serde_json::Value::as_str)
        .unwrap();
    let user: serde_json::Value = serde_json::from_str(user_json).unwrap();
    assert_eq!(user["dictionary_terms"], serde_json::json!(["FrozenTerm"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_reconcile_preserves_context_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap();
    registry.register(mock).await;
    let (cleanup_port, _captured) =
        spawn_capturing_cleanup_server(Duration::from_millis(0), CleanupResponseMode::Echo).await;
    let (port, storage, auth, bearer) = spawn_app_with_cleanup(
        dir.path(),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new("ffmpeg".into())),
        Arc::new(CleanupService::new(Arc::new(ReasoningService::new()))),
    )
    .await;
    enable_cleanup(&storage, cleanup_port);

    let (code, body) =
        post_sessions_with_context(port, &bearer, &sine_wav(), cleanup_test_context()).await;
    assert_eq!(code, 202, "body={body}");
    let id = extract_id(&body).unwrap();
    wait_cleanup_terminal(&storage, &id, Duration::from_secs(10))
        .await
        .expect("cleanup terminal");

    let (code, detail) = get_workspace_detail(port, &bearer, &id).await;
    assert_eq!(code, 200, "detail={detail}");
    assert!(serde_json::to_string(&detail)
        .unwrap()
        .contains("secret clipboard"));

    // 生产启动路径会执行 reconcile（main.rs），既有重启用例只重建 graph 而未覆盖它。
    let report = storage.reconcile().unwrap();
    assert_eq!(report.context_downgraded, 0, "report={report:?}");

    let restarted_port = spawn_fresh_app_graph(dir.path(), Arc::clone(&auth)).await;
    let (code, restarted) = get_workspace_detail(restarted_port, &bearer, &id).await;
    assert_eq!(code, 200, "detail={restarted}");
    assert_eq!(restarted["context_layout"], "inline");
    assert_eq!(restarted["separate_contexts"], serde_json::json!([]));
    assert!(
        serde_json::to_string(&restarted)
            .unwrap()
            .contains("secret clipboard"),
        "context lost after reconcile+restart: {restarted}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn realtime_cleanup_success_persists_artifact_and_hides_context_payload() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::new(
        "mock-openai",
        RuntimeKind::Whisper,
        CannedResponse {
            text: "你好世界".into(),
            segments: vec![
                seasnail_runtime::contract::OpenAiSegment {
                    start: 0.0,
                    end: 0.4,
                    text: "你好".into(),
                    speaker: None,
                },
                seasnail_runtime::contract::OpenAiSegment {
                    start: 0.6,
                    end: 1.0,
                    text: "世界".into(),
                    speaker: None,
                },
            ],
            words: Vec::new(),
        },
    )) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap();
    registry.register(mock).await;
    let (cleanup_port, captured) =
        spawn_capturing_cleanup_server(Duration::from_millis(0), CleanupResponseMode::Echo).await;
    let cleanup = Arc::new(CleanupService::new(Arc::new(ReasoningService::new())));
    let (port, storage, auth, bearer) = spawn_app_with_cleanup(
        dir.path(),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new("ffmpeg".into())),
        Arc::clone(&cleanup),
    )
    .await;
    enable_cleanup(&storage, cleanup_port);

    let (code, body) =
        post_sessions_with_context(port, &bearer, &sine_wav(), cleanup_test_context()).await;
    assert_eq!(code, 202, "body={body}");
    let id = extract_id(&body).unwrap();
    let row = wait_cleanup_terminal(&storage, &id, Duration::from_secs(10))
        .await
        .expect("cleanup did not reach terminal state");
    assert_eq!(row.status, "completed");
    assert_eq!(row.cleanup_status, "succeeded", "row={row:?}");
    let cleanup_path = row.cleanup_path.as_deref().expect("cleanup path");
    let mut artifact = storage.read_cleanup(cleanup_path).unwrap();
    assert_eq!(artifact.cleaned_text, "你好世界");
    assert_eq!(artifact.context_placements.len(), 1);
    assert_eq!(
        artifact.context_placements[0].byte_offset,
        "你好".len() as u64
    );
    assert!(artifact.error_code.is_empty());
    let diagnostics = artifact.diagnostics.as_ref().expect("cleanup diagnostics");
    assert_eq!(diagnostics.http_status, Some(200));
    assert!(diagnostics.raw_response_body.is_empty());
    assert_eq!(
        diagnostics.capture_status,
        DiagnosticCaptureStatus::Redacted as i32
    );
    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let serialized_request = serde_json::to_string(&requests[0]).unwrap();
    assert!(!serialized_request.contains("secret clipboard"));
    let user: serde_json::Value = serde_json::from_str(
        requests[0]
            .pointer("/messages/1/content")
            .and_then(serde_json::Value::as_str)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(user.as_object().unwrap().len(), 2);
    assert_eq!(user["context_markers"].as_array().unwrap().len(), 1);
    assert_eq!(
        user["transcript"]
            .as_str()
            .unwrap()
            .matches("[[SEASNAIL_CTX_V1:")
            .count(),
        1
    );
    drop(requests);

    // M6：详情与注入都通过 FinalTextResolver 选择 cleanup；cache 命中时仍恢复
    // context 的相对位置，但不把它写回 raw transcript。
    let (detail_code, detail) = get_workspace_detail(port, &bearer, &id).await;
    assert_eq!(detail_code, 200, "detail={detail}");
    assert_eq!(detail["final_text"], "你好世界");
    let (cleanup_detail_code, cleanup_detail) = get_cleanup_detail(port, &bearer, &id).await;
    assert_eq!(cleanup_detail_code, 200, "cleanup_detail={cleanup_detail}");
    assert_eq!(cleanup_detail["original_text"], "你好世界");
    assert_eq!(cleanup_detail["cleaned_text"], "你好世界");
    assert!(cleanup_detail["cleanup_elapsed_ms"].is_u64());
    assert!(cleanup_detail["diagnostics"]["local_transcription_elapsed_ms"].is_u64());
    assert_eq!(cleanup_detail["diagnostics"]["http_status"], 200);
    assert_eq!(cleanup_detail["diagnostics"]["raw_response"], "");
    assert_eq!(cleanup_detail["diagnostics"]["capture_status"], "redacted");
    assert_eq!(detail["text_source"], "cleanup");
    assert_eq!(detail["cleanup_status"], "succeeded");
    assert_eq!(detail["context_layout"], "inline");
    assert!(detail["display_items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["kind"] == "final_text"));
    artifact.corrections = vec![
        Correction {
            original_text: "sea snale".into(),
            corrected_text: "SeaSnail".into(),
            kind: CorrectionKind::ProperNoun as i32,
        },
        Correction {
            original_text: "filler".into(),
            corrected_text: "MustNotLearn".into(),
            kind: CorrectionKind::OtherAsr as i32,
        },
    ];
    storage
        .write_cleanup(&id, row.created_at, &artifact)
        .unwrap();
    let (plan_code, plan) = get_injection_plan(port, &bearer, &id).await;
    assert_eq!(plan_code, 200, "plan={plan}");
    let plan_plain = plan["plain"].as_str().unwrap();
    assert!(plan_plain.find("你好").unwrap() < plan_plain.find("secret clipboard").unwrap());
    assert!(plan_plain.find("secret clipboard").unwrap() < plan_plain.find("世界").unwrap());
    let ticket = plan["learning_ticket"].as_str().unwrap();
    let (invalid_code, invalid) = post_learning(
        port,
        &bearer,
        serde_json::json!({"ticket": "bad", "mode": "cleanup"}),
    )
    .await;
    assert_eq!(invalid_code, 400, "invalid={invalid}");
    assert_eq!(invalid["error"]["code"], "learning_ticket_invalid");
    let root = auth.verify(&bearer).unwrap();
    let third = auth
        .issue_token(&root, "learning-third-party", vec!["sessions:read".into()])
        .unwrap();
    let (forbidden_code, forbidden) = post_learning(
        port,
        &third.secret_bearer,
        serde_json::json!({"ticket": ticket, "mode": "cleanup"}),
    )
    .await;
    assert_eq!(forbidden_code, 403, "forbidden={forbidden}");
    let (learning_code, learning) = post_learning(
        port,
        &bearer,
        serde_json::json!({"ticket": ticket, "mode": "cleanup"}),
    )
    .await;
    assert_eq!(learning_code, 200, "learning={learning}");
    assert_eq!(learning["added_terms"], serde_json::json!(["SeaSnail"]));
    let event_id = learning["learning_event_id"].as_str().unwrap();
    let (replay_code, replay) = post_learning(
        port,
        &bearer,
        serde_json::json!({"ticket": ticket, "mode": "cleanup"}),
    )
    .await;
    assert_eq!(replay_code, 410, "replay={replay}");
    assert_eq!(replay["error"]["code"], "learning_ticket_consumed");
    let (forbidden_undo_code, forbidden_undo) =
        undo_learning(port, &third.secret_bearer, event_id).await;
    assert_eq!(forbidden_undo_code, 403, "forbidden_undo={forbidden_undo}");
    let (undo_code, undo) = undo_learning(port, &bearer, event_id).await;
    assert_eq!(undo_code, 200);
    assert_eq!(undo["undone_count"], 1);
    let (_, undo_again) = undo_learning(port, &bearer, event_id).await;
    assert_eq!(undo_again["undone_count"], 0);

    let exported = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/api/v1/export"))
        .bearer_auth(&bearer)
        .json(&serde_json::json!({"password":"p","session_ids":[id]}))
        .send()
        .await
        .unwrap();
    assert_eq!(exported.status().as_u16(), 200);
    let archive = zip::ZipArchive::new(Cursor::new(exported.bytes().await.unwrap())).unwrap();
    assert!(archive
        .file_names()
        .any(|name| name.ends_with("/cleanup.json")));

    // diagnostics 独立失效时，核心 cleaned text 仍可读取，详情只隐藏诊断。
    artifact.diagnostics.as_mut().unwrap().schema_version = 99;
    storage
        .write_cleanup(&id, row.created_at, &artifact)
        .unwrap();
    let (cleanup_detail_code, cleanup_detail) = get_cleanup_detail(port, &bearer, &id).await;
    assert_eq!(cleanup_detail_code, 200, "cleanup_detail={cleanup_detail}");
    assert_eq!(cleanup_detail["cleaned_text"], "你好世界");
    assert!(cleanup_detail["diagnostics"].is_null());

    // 重建 application/HTTP graph 后 presentation cache 必然为空；持久化的已校验
    // placement 仍应从 cleanup.pb.enc 恢复原位 context。
    let restarted_port = spawn_fresh_app_graph(dir.path(), Arc::clone(&auth)).await;
    let (detail_code, restarted_detail) = get_workspace_detail(restarted_port, &bearer, &id).await;
    assert_eq!(detail_code, 200, "detail={restarted_detail}");
    assert_eq!(restarted_detail["text_source"], "cleanup");
    assert_eq!(restarted_detail["context_layout"], "inline");
    let restarted_items = restarted_detail["display_items"].as_array().unwrap();
    assert_eq!(restarted_items[0]["kind"], "final_text");
    assert_eq!(restarted_items[0]["text"], "你好");
    assert_eq!(restarted_items[1]["kind"], "context_text");
    assert_eq!(restarted_items[1]["text"], "secret clipboard");
    assert_eq!(restarted_items[2]["kind"], "final_text");
    assert_eq!(restarted_items[2]["text"], "世界");
    assert_eq!(restarted_detail["separate_contexts"], serde_json::json!([]));
    let (restarted_plan_code, restarted_plan) =
        get_injection_plan(restarted_port, &bearer, &id).await;
    assert_eq!(restarted_plan_code, 200, "plan={restarted_plan}");
    let restarted_plain = restarted_plan["plain"].as_str().unwrap();
    assert!(
        restarted_plain.find("你好").unwrap() < restarted_plain.find("secret clipboard").unwrap()
    );
    assert!(
        restarted_plain.find("secret clipboard").unwrap() < restarted_plain.find("世界").unwrap()
    );
    assert!(!restarted_plain.contains("Clipboard Context:"));

    // 旧 schema v1 artifact 没有 placement 字段；当前进程 cache 清除后继续使用
    // separate 降级，保证历史数据可读且不猜测位置。
    artifact.context_placements.clear();
    storage
        .write_cleanup(&id, row.created_at, &artifact)
        .unwrap();
    cleanup
        .presentation_cache()
        .remove_session(auth.active_account_id().unwrap().as_str(), &id);
    let (legacy_code, legacy_detail) = get_workspace_detail(port, &bearer, &id).await;
    assert_eq!(legacy_code, 200, "detail={legacy_detail}");
    assert_eq!(legacy_detail["context_layout"], "separate");
    assert!(serde_json::to_string(&legacy_detail["separate_contexts"])
        .unwrap()
        .contains("secret clipboard"));

    // 核心 artifact 可解密但 placement 无法对应 context 时，不回退 raw，也不使用
    // cache 掩盖错误；详情与注入都明确使用 separate 降级。
    artifact.context_placements = vec![CleanupContextPlacement {
        event_sequence: 999,
        byte_offset: 0,
    }];
    storage
        .write_cleanup(&id, row.created_at, &artifact)
        .unwrap();
    let invalid_mapping_port = spawn_fresh_app_graph(dir.path(), Arc::clone(&auth)).await;
    let (invalid_mapping_code, invalid_mapping_detail) =
        get_workspace_detail(invalid_mapping_port, &bearer, &id).await;
    assert_eq!(invalid_mapping_code, 200, "detail={invalid_mapping_detail}");
    assert_eq!(invalid_mapping_detail["text_source"], "cleanup");
    assert_eq!(invalid_mapping_detail["context_layout"], "separate");
    let (invalid_plan_code, invalid_plan) =
        get_injection_plan(invalid_mapping_port, &bearer, &id).await;
    assert_eq!(invalid_plan_code, 200, "plan={invalid_plan}");
    assert!(invalid_plan["plain"]
        .as_str()
        .unwrap()
        .contains("Clipboard Context:"));

    // artifact 损坏时读取路径只 fail closed 到 raw，并报告诊断，不修改 DB 状态。
    let cleanup_file = dir
        .path()
        .join("data")
        .join(auth.active_account_id().unwrap())
        .join(cleanup_path);
    std::fs::write(cleanup_file, b"corrupt").unwrap();
    let (detail_code, detail) = get_workspace_detail(port, &bearer, &id).await;
    assert_eq!(detail_code, 200, "detail={detail}");
    assert_eq!(detail["text_source"], "raw");
    assert_eq!(detail["cleanup_error_code"], "cleanup_artifact_invalid");
    assert_eq!(
        storage.get(&id).unwrap().unwrap().cleanup_status,
        "succeeded"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn marker_privacy_e2e_covers_zero_multiple_and_tampered_contexts() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap();
    registry.register(mock).await;
    let (cleanup_port, captured) =
        spawn_capturing_cleanup_server(Duration::ZERO, CleanupResponseMode::Echo).await;
    let (port, storage, _auth, bearer) = spawn_app_with_cleanup(
        dir.path(),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new("ffmpeg".into())),
        Arc::new(CleanupService::new(Arc::new(ReasoningService::new()))),
    )
    .await;
    enable_cleanup(&storage, cleanup_port);

    let zero_context = ClipboardContextFile {
        schema_version: 1,
        session_id: String::new(),
        capture_id: uuid::Uuid::new_v4().to_string(),
        events: Vec::new(),
    };
    let (zero_code, zero_body) =
        post_sessions_with_context(port, &bearer, &sine_wav(), zero_context).await;
    assert_eq!(zero_code, 202, "body={zero_body}");
    let zero_id = extract_id(&zero_body).unwrap();
    let zero_row = wait_cleanup_terminal(&storage, &zero_id, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(zero_row.cleanup_status, "succeeded");

    let private_file = dir.path().join("CTX_PATH_SECRET.txt");
    std::fs::write(&private_file, "local-only").unwrap();
    let private_file = private_file.canonicalize().unwrap();
    let capture_id = uuid::Uuid::new_v4().to_string();
    let multiple_context = ClipboardContextFile {
        schema_version: 1,
        session_id: String::new(),
        capture_id: capture_id.clone(),
        events: vec![
            ContextEvent {
                sequence: 1,
                source_sample_rate: 48_000,
                sample_offset: 8_000,
                kind: ContextEventKind::ContextEventRichText as i32,
                plain_text: "CTX_TEXT_SECRET".into(),
                html_fragment: "<b>CTX_HTML_SECRET</b>".into(),
                absolute_paths: Vec::new(),
            },
            ContextEvent {
                sequence: 2,
                source_sample_rate: 48_000,
                sample_offset: 40_000,
                kind: ContextEventKind::ContextEventFiles as i32,
                plain_text: String::new(),
                html_fragment: String::new(),
                absolute_paths: vec![private_file.to_string_lossy().into_owned()],
            },
        ],
    };
    let (multi_code, multi_body) =
        post_sessions_with_context(port, &bearer, &sine_wav(), multiple_context.clone()).await;
    assert_eq!(multi_code, 202, "body={multi_body}");
    let multi_id = extract_id(&multi_body).unwrap();
    let multi_row = wait_cleanup_terminal(&storage, &multi_id, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(multi_row.cleanup_status, "succeeded");

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for (request, marker_count) in requests.iter().zip([0, 2]) {
        let serialized = serde_json::to_string(request).unwrap();
        for forbidden in [
            "CTX_TEXT_SECRET",
            "CTX_HTML_SECRET",
            "CTX_PATH_SECRET",
            capture_id.as_str(),
        ] {
            assert!(
                !serialized.contains(forbidden),
                "provider observed {forbidden}"
            );
        }
        let user: serde_json::Value = serde_json::from_str(
            request
                .pointer("/messages/1/content")
                .and_then(serde_json::Value::as_str)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(user.as_object().unwrap().len(), 2);
        assert_eq!(
            user["context_markers"].as_array().unwrap().len(),
            marker_count
        );
        assert_eq!(
            user["transcript"]
                .as_str()
                .unwrap()
                .matches("[[SEASNAIL_CTX_V1:")
                .count(),
            marker_count
        );
    }
    drop(requests);

    let (plan_code, plan) = get_injection_plan(port, &bearer, &multi_id).await;
    assert_eq!(plan_code, 200, "plan={plan}");
    assert!(plan["plain"].as_str().unwrap().contains("CTX_TEXT_SECRET"));
    assert!(plan["plain"].as_str().unwrap().contains("CTX_PATH_SECRET"));
    assert!(!plan["plain"]
        .as_str()
        .unwrap()
        .contains("[[SEASNAIL_CTX_V1:"));

    let (tamper_port, _) =
        spawn_capturing_cleanup_server(Duration::ZERO, CleanupResponseMode::DropFirstMarker).await;
    enable_cleanup(&storage, tamper_port);
    let (tamper_code, tamper_body) =
        post_sessions_with_context(port, &bearer, &sine_wav(), multiple_context).await;
    assert_eq!(tamper_code, 202, "body={tamper_body}");
    let tamper_id = extract_id(&tamper_body).unwrap();
    let tamper_row = wait_cleanup_terminal(&storage, &tamper_id, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(tamper_row.status, "completed");
    assert_eq!(tamper_row.cleanup_status, "failed");
    assert_eq!(
        tamper_row.cleanup_error_code.as_deref(),
        Some("cleanup_placeholder_invalid")
    );
    let (detail_code, detail) = get_workspace_detail(port, &bearer, &tamper_id).await;
    assert_eq!(detail_code, 200, "detail={detail}");
    assert_eq!(detail["text_source"], "raw");
    assert_eq!(detail["final_text"], "你好世界");
}

#[tokio::test(flavor = "multi_thread")]
async fn cleanup_does_not_hold_asr_reservation_or_recreate_deleted_session() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap();
    registry.register(mock).await;
    let cleanup_port = spawn_cleanup_server(Duration::from_millis(700)).await;
    let cleanup = Arc::new(CleanupService::new(Arc::new(ReasoningService::new())));
    let (port, storage, auth, bearer) = spawn_app_with_cleanup(
        dir.path(),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new("ffmpeg".into())),
        cleanup,
    )
    .await;
    enable_cleanup(&storage, cleanup_port);

    let (first_code, first_body) =
        post_sessions_with_context(port, &bearer, &sine_wav(), cleanup_test_context()).await;
    assert_eq!(first_code, 202, "body={first_body}");
    let first_id = extract_id(&first_body).unwrap();
    let first_row = wait_cleanup_processing(&storage, &first_id, Duration::from_secs(10)).await;
    let (detail_code, detail_body) = get_session(port, &bearer, &first_id).await;
    assert_eq!(detail_code, 200, "body={detail_body}");
    let detail: serde_json::Value = serde_json::from_str(&detail_body).unwrap();
    assert_eq!(detail["status"], "cleaning_up");
    assert!(
        detail["transcript"].is_null(),
        "普通详情在 cleaning_up 时不得提前交付 raw checkpoint"
    );
    let account_id = auth.active_account_id().unwrap();
    let first_dir = dir.path().join("data").join(account_id).join(
        Path::new(first_row.audio_path.as_ref().unwrap())
            .parent()
            .unwrap(),
    );

    // cleanup 正在等待远端响应时，ASR reservation 已经被 transcribe future 消费，
    // 因而第二条 realtime 录音仍可获得 reservation 并被接受。
    let (second_code, second_body) =
        post_sessions_with_context(port, &bearer, &sine_wav(), cleanup_test_context()).await;
    assert_eq!(
        second_code, 202,
        "第二条录音不应被 cleanup 占用，body={second_body}"
    );

    let delete = reqwest::Client::new()
        .delete(format!(
            "http://127.0.0.1:{port}/api/v1/sessions/{first_id}"
        ))
        .bearer_auth(&bearer)
        .send()
        .await
        .unwrap();
    assert_eq!(delete.status().as_u16(), 204);

    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(storage.get(&first_id).unwrap().is_none());
    assert!(
        !first_dir.exists(),
        "迟到 cleanup 不得重建已删除 session 目录"
    );

    let second_id = extract_id(&second_body).unwrap();
    let second_row = wait_cleanup_terminal(&storage, &second_id, Duration::from_secs(10))
        .await
        .expect("second cleanup did not reach terminal state");
    assert_eq!(second_row.status, "completed");
}

#[tokio::test(flavor = "multi_thread")]
async fn cleanup_transport_failure_completes_with_raw_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap();
    registry.register(mock).await;
    let unused_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cleanup_port = unused_listener.local_addr().unwrap().port();
    drop(unused_listener);
    let (port, storage, _auth, bearer) = spawn_app_with(
        dir.path(),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new("ffmpeg".into())),
    )
    .await;
    enable_cleanup(&storage, cleanup_port);

    let (code, body) =
        post_sessions_with_context(port, &bearer, &sine_wav(), cleanup_test_context()).await;
    assert_eq!(code, 202, "body={body}");
    let id = extract_id(&body).unwrap();
    let row = wait_cleanup_terminal(&storage, &id, Duration::from_secs(10))
        .await
        .expect("cleanup failure did not reach terminal state");
    assert_eq!(row.status, "completed");
    assert_eq!(row.cleanup_status, "failed");
    assert_eq!(
        row.cleanup_error_code.as_deref(),
        Some("cleanup_transport_error")
    );
    assert!(
        row.transcript_path.is_some(),
        "raw transcript must remain usable"
    );
    assert!(
        row.cleanup_path.is_some(),
        "failure artifact should be best-effort persisted"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// 验收 1：成功链路（音频→规范化 units→加密落库）
// ──────────────────────────────────────────────────────────────────────────────

/// 202 transcribing → 后台 normalize→transcribe→build proto→encrypt 落库 → completed。
/// 解密 transcript 断言 canned「你好世界」单段（0–1s）；音频仍可解密读回（保留）。
#[tokio::test(flavor = "multi_thread")]
async fn transcription_pipeline_completes() {
    let ffmpeg = match resolve_ffmpeg().await {
        Some(p) => p,
        None => {
            eprintln!("skip: ffmpeg 未找到（设 FFMPEG_PATH 或装系统 ffmpeg）");
            return;
        }
    };
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap(); // port 0 → OS 分配；transcribe 走 loopback
    registry.register(mock).await;
    let gate = Arc::new(RuntimeOperationGate::new_for_test());
    let normalizer = Arc::new(AudioNormalizer::new(ffmpeg));
    let (port, storage, _auth, bearer) =
        spawn_app_with(dir.path(), registry, gate, normalizer).await;

    let wav = sine_wav();
    let (code, body) = post_sessions(port, &bearer, &wav).await;
    assert_eq!(code, 202, "应 202 accepted, body={body}");
    assert!(body.contains("\"status\":\"transcribing\""), "body={body}");
    let id = extract_id(&body).expect("202 应含 id");

    let row = wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .expect("超时未推进 completed（后台转译未完成）");
    assert_eq!(row.status, "completed", "应推进 completed");
    assert!(row.audio_path.is_some(), "audio_path 应存留");
    assert!(
        row.duration_sec > 0.0,
        "duration 应回填（canned 末段 end=1.0）"
    );

    // 解密 transcript：canned 文本 + 单段 + 秒→毫秒换算。
    let trans_rel = row
        .transcript_path
        .as_ref()
        .expect("completed 应回填 transcript_path");
    let t = storage.read_transcript(trans_rel).unwrap();
    assert_eq!(t.full_text, "你好世界", "canned full_text");
    assert_eq!(t.units.len(), 1, "canned 单 unit");
    assert_eq!(t.units[0].text, "你好世界");
    assert_eq!(t.units[0].start_ms, Some(0), "start 0s→0ms");
    assert_eq!(t.units[0].end_ms, Some(1000), "end 1.0s→1000ms");
    assert_eq!(t.duration_ms, 1000, "duration_ms = 末段 end×1000");
    assert_eq!(t.language, "zh", "language 回填");
    assert_eq!(t.source, Source::Imported as i32, "source=imported");
    assert_eq!(t.session_id, id, "proto session_id 对应");
    assert!(!t.model.is_empty(), "model 字段非空");

    // 原始音频仍可解密读回（收到即加密持久化，转译不删原音频）。
    let audio_rel = row.audio_path.as_ref().unwrap();
    assert_eq!(
        storage.read_audio(audio_rel).unwrap(),
        wav,
        "原始音频往返（保留 + 未被删）"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn pipeline_keeps_original_account_storage_across_account_switch() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap();
    registry.register(mock).await;
    let normalizer = Arc::new(AudioNormalizer::new(slow_copying_normalizer_binary(
        dir.path(),
    )));
    let (port, storage, auth, alice_bearer) = spawn_app_with(
        dir.path(),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
        normalizer,
    )
    .await;
    let alice_id = auth.active_account_id().unwrap();

    let (code, body) = post_sessions(port, &alice_bearer, &sine_wav()).await;
    assert_eq!(code, 202, "body={body}");
    let session_id = extract_id(&body).unwrap();

    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/api/v1/accounts"))
        .bearer_auth(&alice_bearer)
        .json(&serde_json::json!({"username":"bob","password":"p2"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 201);
    let bob: serde_json::Value = response.json().await.unwrap();
    let bob_id = bob["account_id"].as_str().unwrap().to_owned();

    let delete = reqwest::Client::new()
        .delete(format!(
            "http://127.0.0.1:{port}/api/v1/accounts/{alice_id}"
        ))
        .bearer_auth(bob["secret"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(delete.status().as_u16(), 409);

    tokio::time::sleep(Duration::from_millis(1_300)).await;
    let unlock = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{port}/api/v1/accounts/{alice_id}/unlock"
        ))
        .json(&serde_json::json!({"password":"p"}))
        .send()
        .await
        .unwrap();
    assert_eq!(unlock.status().as_u16(), 200);
    let row = wait_outcome(&storage, &session_id, Duration::from_secs(5))
        .await
        .expect("original account outcome must complete after switch");
    assert_eq!(row.account_id, alice_id);
    assert_eq!(row.status, "completed");

    let unlock_bob = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{port}/api/v1/accounts/{bob_id}/unlock"
        ))
        .json(&serde_json::json!({"password":"p2"}))
        .send()
        .await
        .unwrap();
    assert_eq!(unlock_bob.status().as_u16(), 200);
    assert!(storage.get(&session_id).unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn transcription_with_context_keeps_authoritative_full_text_clean() {
    // schema v2 端到端：带 context 的 manifest 仍只把纯 ASR 正文写入 transcript。
    let ffmpeg = match resolve_ffmpeg().await {
        Some(p) => p,
        None => {
            eprintln!("skip: ffmpeg 未找到（设 FFMPEG_PATH 或装系统 ffmpeg）");
            return;
        }
    };
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap();
    registry.register(mock).await;
    let (port, storage, _auth, bearer) = spawn_app_with(
        dir.path(),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new(ffmpeg)),
    )
    .await;

    // 1 个 PLAIN_TEXT 事件：offset 24000 frames @ 48000Hz = 500ms，落在 canned 段 [0,1s] 内。
    let context = ClipboardContextFile {
        schema_version: 1,
        session_id: String::new(),
        capture_id: uuid::Uuid::new_v4().to_string(),
        events: vec![ContextEvent {
            sequence: 1,
            source_sample_rate: 48_000,
            sample_offset: 24_000,
            kind: ContextEventKind::ContextEventPlainText as i32,
            plain_text: "代码".into(),
            html_fragment: String::new(),
            absolute_paths: Vec::new(),
        }],
    };
    let (code, body) = post_sessions_with_context(port, &bearer, &sine_wav(), context).await;
    assert_eq!(code, 202, "body={body}");
    let id = extract_id(&body).unwrap();
    let row = wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(row.status, "completed");
    let t = storage
        .read_transcript(row.transcript_path.as_ref().unwrap())
        .unwrap();
    // schema v2 transcript 只保存纯 ASR 正文，上下文独立保存。
    assert_eq!(t.full_text, "你好世界");
    assert_eq!(
        t.units[0].text, "你好世界",
        "unit 保持裸 ASR，不含上下文标记"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// 验收 2：失败链路（音频保留）
// ──────────────────────────────────────────────────────────────────────────────

/// 注册**未启动** mock（transcribe→NotStarted）→ 行推进 failed + 音频保留。
/// 不依赖 ffmpeg：有 ffmpeg→normalize ok 但 transcribe 失败；无 ffmpeg→normalize 失败。
#[tokio::test(flavor = "multi_thread")]
async fn failed_transcription_retains_audio() {
    let ffmpeg = resolve_ffmpeg()
        .await
        .unwrap_or_else(|| PathBuf::from("ffmpeg-definitely-not-found"));
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    registry.register(mock).await; // 注册但不 start → transcribe → NotStarted
    let gate = Arc::new(RuntimeOperationGate::new_for_test());
    let normalizer = Arc::new(AudioNormalizer::new(ffmpeg));
    let (port, storage, _auth, bearer) =
        spawn_app_with(dir.path(), registry, gate, normalizer).await;

    let wav = sine_wav();
    let (code, body) = post_sessions(port, &bearer, &wav).await;
    assert_eq!(code, 202, "失败链路仍应先 202, body={body}");
    let id = extract_id(&body).expect("202 应含 id");

    let row = wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .expect("超时未推进 failed（后台失败兜底未触发）");
    assert_eq!(row.status, "failed", "应推进 failed");
    assert!(row.transcript_path.is_none(), "失败无 transcript_path");
    assert!(row.audio_path.is_some(), "失败仍保留 audio_path");

    // 音频可解密读回（失败不删原音频，符合「失败会话仍持音频」验收）。
    let audio_rel = row.audio_path.as_ref().unwrap();
    assert_eq!(
        storage.read_audio(audio_rel).unwrap(),
        wav,
        "失败会话保留音频往返"
    );
}

/// capsule_notice ST-M3.3：mock 返回合法空结果 → facade `NoSpeech` → 行推进
/// failed + `failure_reason="no_speech"`（无 transcript、音频保留）→ GET 透出；
/// retry 后同一空结果再次落 no_speech，证明无语音会话可重试且原因被重写。
/// 需 ffmpeg（normalize 已存音频，使 pipeline 到达 runtime）；未装 skip。
#[tokio::test(flavor = "multi_thread")]
async fn no_speech_transcription_persists_stable_reason_and_audio() {
    let ffmpeg = match resolve_ffmpeg().await {
        Some(p) => p,
        None => {
            eprintln!("skip: ffmpeg 未找到（no_speech 链路需 normalize 到达 runtime）");
            return;
        }
    };
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::new(
        "mock-openai-empty",
        RuntimeKind::Whisper,
        CannedResponse {
            text: String::new(),
            segments: Vec::new(),
            words: Vec::new(),
        },
    )) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap();
    registry.register(mock).await;
    let gate = Arc::new(RuntimeOperationGate::new_for_test());
    let normalizer = Arc::new(AudioNormalizer::new(ffmpeg));
    let (port, storage, _auth, bearer) =
        spawn_app_with(dir.path(), registry, gate, normalizer).await;

    let wav = sine_wav();
    let (code, body) = post_sessions(port, &bearer, &wav).await;
    assert_eq!(code, 202, "body={body}");
    let id = extract_id(&body).expect("202 应含 id");

    let row = wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .expect("超时未推进 failed（NoSpeech 终态未持久化）");
    assert_eq!(row.status, "failed", "NoSpeech 应保持 failed 会话语义");
    assert_eq!(
        row.failure_reason.as_deref(),
        Some("no_speech"),
        "NoSpeech 应写稳定原因码"
    );
    assert!(row.transcript_path.is_none(), "NoSpeech 不写 transcript");
    assert!(row.audio_path.is_some(), "NoSpeech 仍保留音频");
    let audio_rel = row.audio_path.as_ref().unwrap();
    assert_eq!(storage.read_audio(audio_rel).unwrap(), wav, "音频往返一致");

    // GET 透出 failed + no_speech + 无 transcript。
    let (code, body) = get_session(port, &bearer, &id).await;
    assert_eq!(code, 200, "body={body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["status"], "failed", "body={body}");
    assert_eq!(v["failure_reason"], "no_speech", "body={body}");
    assert!(v["transcript"].is_null(), "body={body}");

    // retry：无语音会话可重试；同一空结果再次落 no_speech（原因被重写而非残留）。
    let (retry_code, retry_body) = retry_session(port, &bearer, &id).await;
    assert_eq!(retry_code, 202, "retry 应 202, body={retry_body}");
    let row = wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .expect("retry 后超时未推进 failed");
    assert_eq!(row.status, "failed");
    assert_eq!(row.failure_reason.as_deref(), Some("no_speech"));
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_transcription_preserves_context_for_retry_with_server_assigned_session_id() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    registry.register(mock).await; // 未启动：后台必失败，但创建/持久化应完整发生。
    let (port, storage, _auth, bearer) = spawn_app_with(
        dir.path(),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new(PathBuf::from(
            "ffmpeg-definitely-not-found",
        ))),
    )
    .await;
    let context = ClipboardContextFile {
        schema_version: 1,
        session_id: String::new(),
        capture_id: uuid::Uuid::new_v4().to_string(),
        events: vec![ContextEvent {
            sequence: 1,
            source_sample_rate: 48_000,
            sample_offset: 24_000,
            kind: ContextEventKind::ContextEventPlainText as i32,
            plain_text: "retry-context".into(),
            html_fragment: String::new(),
            absolute_paths: Vec::new(),
        }],
    };
    let (code, body) = post_sessions_with_context(port, &bearer, &sine_wav(), context).await;
    assert_eq!(code, 202, "body={body}");
    let id = extract_id(&body).unwrap();
    let row = wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(row.status, "failed");
    assert!(row.context_present);
    let stored = storage.read_context(&id, row.created_at).unwrap();
    assert_eq!(stored.session_id, id);
    assert_eq!(stored.events.len(), 1);
    assert_eq!(stored.events[0].plain_text, "retry-context");
}

#[tokio::test(flavor = "multi_thread")]
async fn context_multipart_rejects_invalid_capture_id_before_creating_session() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    registry
        .register(Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>)
        .await;
    let (port, storage, _auth, bearer) = spawn_app_with(
        dir.path(),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new(PathBuf::from(
            "ffmpeg-definitely-not-found",
        ))),
    )
    .await;
    let context = ClipboardContextFile {
        schema_version: 1,
        session_id: String::new(),
        capture_id: "../../outside".into(),
        events: Vec::new(),
    };
    let (code, _) = post_sessions_with_context(port, &bearer, &sine_wav(), context).await;
    assert_eq!(code, 400);
    assert!(storage.list(10, None).unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn retry_rejects_tampered_persisted_context_before_reset() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    registry
        .register(Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>)
        .await;
    let (port, storage, _auth, bearer) = spawn_app_with(
        dir.path(),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new(PathBuf::from(
            "ffmpeg-definitely-not-found",
        ))),
    )
    .await;
    let context = ClipboardContextFile {
        schema_version: 1,
        session_id: String::new(),
        capture_id: uuid::Uuid::new_v4().to_string(),
        events: Vec::new(),
    };
    let (code, body) = post_sessions_with_context(port, &bearer, &sine_wav(), context).await;
    assert_eq!(code, 202, "body={body}");
    let id = extract_id(&body).unwrap();
    let row = wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(row.status, "failed");
    let account_dir = dir.path().join("data").join(&row.account_id);
    let context_path = account_dir.join(format!(
        "{}/{}/context.pb.enc",
        chrono::DateTime::from_timestamp(row.created_at, 0)
            .unwrap()
            .format("%Y-%m-%d"),
        id
    ));
    let mut bytes = std::fs::read(&context_path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(context_path, bytes).unwrap();

    let (retry_code, _) = retry_session(port, &bearer, &id).await;
    assert_eq!(retry_code, 500);
    assert_eq!(storage.get(&id).unwrap().unwrap().status, "failed");
}

// ──────────────────────────────────────────────────────────────────────────────
// 验收 3：无活跃 runtime → 409
// ──────────────────────────────────────────────────────────────────────────────

/// 空 registry（无活跃模型）→ `registry.active()` None → 409 conflict。
/// 先于 slot 占用，故不消耗 gate slot。
#[tokio::test(flavor = "multi_thread")]
async fn no_active_runtime_returns_409() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new()); // 空
    let gate = Arc::new(RuntimeOperationGate::new_for_test());
    let normalizer = Arc::new(AudioNormalizer::new(PathBuf::from("ffmpeg")));
    let (port, _storage, _auth, bearer) =
        spawn_app_with(dir.path(), registry, gate.clone(), normalizer).await;

    let wav = sine_wav();
    let (code, body) = post_sessions(port, &bearer, &wav).await;
    assert_eq!(code, 409, "无活跃 runtime 应 409, body={body}");
    assert!(body.contains("no active ASR runtime"), "body={body}");
    // 未占 slot（registry 先于 gate）。
    assert!(gate.active().is_none(), "无 runtime 时不应占 slot");
}

// ──────────────────────────────────────────────────────────────────────────────
// 验收 4：slot 占用中第二请求 → 409
// ──────────────────────────────────────────────────────────────────────────────

/// 预占 slot（持 guard 跨 POST）→ handler `gate.acquire` → Occupied → 409。
/// 须先注册 mock 使 `registry.active()` 返 Some（否则先命中「无 runtime」409）；
/// mock 无需 start（transcribe 不到达）。
#[tokio::test(flavor = "multi_thread")]
async fn second_request_while_occupied_returns_409() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    registry.register(mock).await; // 使 active() 返 Some；不 start
    let gate = Arc::new(RuntimeOperationGate::new_for_test());
    // 预占 slot：guard 持 Arc 克隆（不借 gate），其后可 move gate 入 helper。
    let _guard = gate
        .acquire(RuntimeOperation::Transcription {
            session_id: "blocking-session".into(),
        })
        .unwrap();
    assert!(matches!(
        gate.active(),
        Some(RuntimeOperation::Transcription { session_id })
            if session_id == "blocking-session"
    ));
    let normalizer = Arc::new(AudioNormalizer::new(PathBuf::from("ffmpeg")));
    let (port, _storage, _auth, bearer) =
        spawn_app_with(dir.path(), registry, gate, normalizer).await;

    let wav = sine_wav();
    let (code, body) = post_sessions(port, &bearer, &wav).await;
    assert_eq!(code, 409, "占用中应 409, body={body}");
    assert!(body.contains("transcription slot occupied"), "body={body}");
    // _guard drop 于此 → slot 释放（RAII）。
}

// ──────────────────────────────────────────────────────────────────────────────
// 契约边界（验收外：输入校验/鉴权层；先于 runtime 返回，不依赖 ffmpeg/mock）
// ──────────────────────────────────────────────────────────────────────────────

/// multipart 缺 audio part → 413（`audio_bytes=None` → `PayloadTooLarge("missing audio")`）。
/// multipart 解析在 runtime 检查前，故空 registry 即可触发，不需 mock。
#[tokio::test(flavor = "multi_thread")]
async fn missing_audio_returns_413() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new()); // 空：413 在 runtime 检查前
    let gate = Arc::new(RuntimeOperationGate::new_for_test());
    let normalizer = Arc::new(AudioNormalizer::new(PathBuf::from("ffmpeg")));
    let (port, _storage, _auth, bearer) =
        spawn_app_with(dir.path(), registry, gate, normalizer).await;

    // multipart 只发 source/language，无 audio part。
    let form = reqwest::multipart::Form::new()
        .text("source", "imported")
        .text("language", "zh");
    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/api/v1/sessions"))
        .bearer_auth(&bearer)
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 413, "缺 audio 应 413");
    let body = resp.text().await.unwrap();
    assert!(body.contains("missing audio"), "body={body}");
}

/// 持 `sessions:read`（无 write）的第三方 token → 403 `insufficient_scope`。
/// `require_scope` 第一行拦截，先于 multipart/runtime；空 registry 即可。
#[tokio::test(flavor = "multi_thread")]
async fn insufficient_scope_returns_403() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new()); // 403 在 runtime 前
    let gate = Arc::new(RuntimeOperationGate::new_for_test());
    let normalizer = Arc::new(AudioNormalizer::new(PathBuf::from("ffmpeg")));
    let (port, _storage, auth, bearer) =
        spawn_app_with(dir.path(), registry, gate, normalizer).await;

    // 用 root 签发仅 sessions:read 的第三方 token（无 sessions:write）。
    let caller = auth.verify(&bearer).unwrap();
    assert!(caller.is_root);
    let third = auth
        .issue_token(&caller, "ci", vec!["sessions:read".into()])
        .unwrap();
    assert!(!third.is_root, "第三方 token 非 root");

    let wav = sine_wav();
    let (code, body) = post_sessions(port, &third.secret_bearer, &wav).await;
    assert_eq!(code, 403, "无 sessions:write 应 403, body={body}");
    assert!(body.contains("insufficient_scope"), "body={body}");
}

/// multipart 缺 source/language → 推导 imported/mixed（handler match 兜底）。
/// 行在 spawn 前 INSERT，故 202 返回时即可查 `row.source/language`。后台 task 可能
/// 已推进 failed，但 `update_outcome` 不动 source/language → 断言稳定。不依赖 ffmpeg：
/// 注册未启动 mock 使 `active()` 返 Some 即过 runtime 检查，transcribe 不到达。
#[tokio::test(flavor = "multi_thread")]
async fn omitted_source_language_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    registry.register(mock).await; // 未启动：active() 返 Some，过 runtime 检查
    let gate = Arc::new(RuntimeOperationGate::new_for_test());
    let normalizer = Arc::new(AudioNormalizer::new(PathBuf::from("ffmpeg")));
    let (port, storage, _auth, bearer) =
        spawn_app_with(dir.path(), registry, gate, normalizer).await;

    // multipart 只发 audio，无 source/language part。
    let part = reqwest::multipart::Part::bytes(sine_wav())
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .unwrap();
    let form = reqwest::multipart::Form::new().part("audio", part);
    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/api/v1/sessions"))
        .bearer_auth(&bearer)
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 202, "应 202 accepted");
    let body = resp.text().await.unwrap();
    let id = extract_id(&body).expect("202 应含 id");

    // 行已 INSERT（handler 同步、spawn 前）；source/language 推导值在 INSERT 时填定。
    let row = storage.get(&id).unwrap().expect("应已 INSERT 行");
    assert_eq!(row.source, "imported", "缺省 source→imported");
    assert_eq!(row.language, "mixed", "缺省 language→mixed");
    // status 不断言：后台 task 可能已推进 failed，但 source/language 不变。
}

/// GUI 壳的实时采集提交 `source=realtime` 与输入设备名；实时规范 WAV 不应依赖 ffmpeg。
#[tokio::test(flavor = "multi_thread")]
async fn realtime_submission_persists_input_device() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default());
    mock.start(0).await.unwrap();
    registry.register(mock as Arc<dyn ModelRuntime>).await;
    let gate = Arc::new(RuntimeOperationGate::new_for_test());
    let normalizer = Arc::new(AudioNormalizer::new(PathBuf::from(
        "ffmpeg-must-not-be-called-for-realtime",
    )));
    let (port, storage, _auth, bearer) =
        spawn_app_with(dir.path(), registry, gate, normalizer).await;

    let part = reqwest::multipart::Part::bytes(sine_wav())
        .file_name("seasnail-realtime.wav")
        .mime_str("audio/wav")
        .unwrap();
    let form = reqwest::multipart::Form::new()
        .part("audio", part)
        .text("source", "realtime")
        .text("language", "mixed")
        .text("input_device", "MacBook Pro 麦克风");
    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/api/v1/sessions"))
        .bearer_auth(&bearer)
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 202);
    let id = extract_id(&response.text().await.unwrap()).expect("202 应含 id");
    let row = storage.get(&id).unwrap().expect("应已 INSERT 行");
    assert_eq!(row.source, "realtime");
    assert_eq!(row.input_device.as_deref(), Some("MacBook Pro 麦克风"));
    let row = wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .expect("实时规范 WAV 不应因缺少 ffmpeg 失败");
    assert_eq!(row.status, "completed");
}

// ──────────────────────────────────────────────────────────────────────────────
// ST-M3.7 状态机 + 失败重试
// ──────────────────────────────────────────────────────────────────────────────

/// `GET /sessions/{id}` 请求封装 → `(状态码, body)`。
async fn get_session(port: u16, bearer: &str, id: &str) -> (u16, String) {
    let resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/api/v1/sessions/{id}"))
        .bearer_auth(bearer)
        .send()
        .await
        .unwrap();
    let code = resp.status().as_u16();
    let body = resp.text().await.unwrap();
    (code, body)
}

/// M5.1 列表 / 搜索请求封装。
async fn get_sessions_path(port: u16, bearer: &str, path: &str) -> (u16, String) {
    let resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/api/v1/{path}"))
        .bearer_auth(bearer)
        .send()
        .await
        .unwrap();
    let code = resp.status().as_u16();
    (code, resp.text().await.unwrap())
}

/// M5.3 编辑请求封装。
async fn put_transcript(
    port: u16,
    bearer: &str,
    id: &str,
    transcript: serde_json::Value,
) -> (u16, String) {
    let resp = reqwest::Client::new()
        .put(format!("http://127.0.0.1:{port}/api/v1/sessions/{id}"))
        .bearer_auth(bearer)
        .json(&serde_json::json!({"transcript": transcript}))
        .send()
        .await
        .unwrap();
    let code = resp.status().as_u16();
    (code, resp.text().await.unwrap())
}

/// M5.4 删除请求封装。
async fn delete_session(port: u16, bearer: &str, id: &str) -> (u16, String) {
    let resp = reqwest::Client::new()
        .delete(format!("http://127.0.0.1:{port}/api/v1/sessions/{id}"))
        .bearer_auth(bearer)
        .send()
        .await
        .unwrap();
    let code = resp.status().as_u16();
    (code, resp.text().await.unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn session_list_and_search_return_decrypted_preview() {
    let Some(ffmpeg) = resolve_ffmpeg().await else {
        eprintln!("skip: ffmpeg 未找到");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap();
    registry.register(mock).await;
    let (port, storage, _auth, bearer) = spawn_app_with(
        dir.path(),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new(ffmpeg)),
    )
    .await;
    let (code, body) = post_sessions(port, &bearer, &sine_wav()).await;
    assert_eq!(code, 202, "{body}");
    let id = extract_id(&body).unwrap();
    let row = wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(row.status, "completed");
    let (code, body) = get_sessions_path(port, &bearer, "sessions?limit=1&source=imported").await;
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(&id));
    assert!(body.contains("你好世界"));
    let (code, body) =
        get_sessions_path(port, &bearer, "sessions/search?q=%E4%BD%A0%E5%A5%BD").await;
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(&id));
    assert!(body.contains("你好世界"));
    let (code, body) = get_sessions_path(
        port,
        &bearer,
        "sessions/search?q=%E4%BD%A0%E5%A5%BD&source=realtime",
    )
    .await;
    assert_eq!(code, 200, "{body}");
    assert!(
        !body.contains(&id),
        "source filter must also apply to search"
    );
}

/// 只读详情拒绝旧 PUT 编辑接口，删除后不可读取。
#[tokio::test(flavor = "multi_thread")]
async fn session_edit_is_rejected_and_delete_round_trips() {
    let Some(ffmpeg) = resolve_ffmpeg().await else {
        eprintln!("skip: ffmpeg 未找到");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap();
    registry.register(mock).await;
    let (port, storage, _auth, bearer) = spawn_app_with(
        dir.path(),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new(ffmpeg)),
    )
    .await;
    let (code, body) = post_sessions(port, &bearer, &sine_wav()).await;
    assert_eq!(code, 202, "{body}");
    let id = extract_id(&body).unwrap();
    assert_eq!(
        wait_outcome(&storage, &id, Duration::from_secs(10))
            .await
            .unwrap()
            .status,
        "completed"
    );

    let edited = serde_json::json!({
        "full_text": "编辑后的转写",
        "segments": [{"speaker":"A", "start":0.0, "end":1.5, "text":"编辑后的转写"}]
    });
    let (code, body) = put_transcript(port, &bearer, &id, edited).await;
    assert_eq!(code, 405, "只读详情不支持编辑: {body}");

    let invalid = serde_json::json!({
        "full_text": "bad",
        "segments": [{"speaker":"A", "start":2.0, "end":1.0, "text":"bad"}]
    });
    let (code, body) = put_transcript(port, &bearer, &id, invalid).await;
    assert_eq!(code, 405, "只读详情不支持编辑: {body}");

    let (code, body) = delete_session(port, &bearer, &id).await;
    assert_eq!(code, 204, "{body}");
    let (code, body) = get_session(port, &bearer, &id).await;
    assert_eq!(code, 404, "{body}");
}

/// `POST /sessions/{id}/retry` 请求封装 → `(状态码, body)`。
async fn retry_session(port: u16, bearer: &str, id: &str) -> (u16, String) {
    let resp = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{port}/api/v1/sessions/{id}/retry"
        ))
        .bearer_auth(bearer)
        .send()
        .await
        .unwrap();
    let code = resp.status().as_u16();
    let body = resp.text().await.unwrap();
    (code, body)
}

/// `GET /sessions/{id}` 行不存在 → 404 `not_found`。先于 runtime/pipeline，不依赖 ffmpeg。
#[tokio::test(flavor = "multi_thread")]
async fn get_session_not_found_returns_404() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let gate = Arc::new(RuntimeOperationGate::new_for_test());
    let normalizer = Arc::new(AudioNormalizer::new(PathBuf::from("ffmpeg")));
    let (port, _storage, _auth, bearer) =
        spawn_app_with(dir.path(), registry, gate, normalizer).await;

    let (code, body) = get_session(port, &bearer, "00000000-0000-0000-0000-000000000000").await;
    assert_eq!(code, 404, "不存在的会话应 404, body={body}");
    assert!(body.contains("not_found"), "body={body}");
}

/// 验收 ST-M3.7：失败→retry→202→completed + GET 透出 status/transcript/failure_reason。
///
/// 链路：注册未启动 mock（transcribe→NotStarted）→ POST 202 → pipeline 因 transcribe
/// 失败推 failed（failure_reason 记入行）→ GET 透出 failed+failure_reason → POST retry
/// → handler 见 sidecar 不健康 stop+start 重启（「退避重启 sidecar」）→ 复用已存音频
/// 重跑 pipeline → completed → GET 透出 completed+transcript（full_text/units 秒）。
/// 需 ffmpeg（normalize 已存音频）；未装 skip。
#[tokio::test(flavor = "multi_thread")]
async fn retry_failed_session_completes() {
    let ffmpeg = match resolve_ffmpeg().await {
        Some(p) => p,
        None => {
            eprintln!("skip: ffmpeg 未找到（retry 成功链路需 normalize 已存音频）");
            return;
        }
    };
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    registry.register(mock).await; // 注册但不 start → transcribe → NotStarted → failed
    let gate = Arc::new(RuntimeOperationGate::new_for_test());
    let normalizer = Arc::new(AudioNormalizer::new(ffmpeg));
    let (cleanup_port, captured) =
        spawn_capturing_cleanup_server(Duration::ZERO, CleanupResponseMode::Echo).await;
    let (port, storage, _auth, bearer) = spawn_app_with_cleanup(
        dir.path(),
        registry,
        gate,
        normalizer,
        Arc::new(CleanupService::new(Arc::new(ReasoningService::new()))),
    )
    .await;
    enable_cleanup(&storage, cleanup_port);

    // 1. POST → 202 → pipeline 因 transcribe NotStarted 推 failed；随后 retry 必须复用
    // 这份已加密持久化的上下文，而不能悄悄退化为无上下文结果。
    let wav = sine_wav();
    let context = ClipboardContextFile {
        schema_version: 1,
        session_id: String::new(),
        capture_id: uuid::Uuid::new_v4().to_string(),
        events: vec![ContextEvent {
            sequence: 1,
            source_sample_rate: 48_000,
            sample_offset: 24_000,
            kind: ContextEventKind::ContextEventPlainText as i32,
            plain_text: "retry-context".into(),
            html_fragment: String::new(),
            absolute_paths: Vec::new(),
        }],
    };
    let (code, body) = post_sessions_with_context(port, &bearer, &wav, context).await;
    assert_eq!(code, 202, "首次提交应 202, body={body}");
    let id = extract_id(&body).expect("202 应含 id");
    let row = wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .expect("超时未推进 failed");
    assert_eq!(row.status, "failed", "未启动 mock 应推 failed");
    assert!(
        row.failure_reason.as_ref().is_some_and(|r| !r.is_empty()),
        "failed 行应记 failure_reason"
    );

    // 2. GET 透出 failed + failure_reason + 无 transcript。
    let (code, body) = get_session(port, &bearer, &id).await;
    assert_eq!(code, 200, "GET 应 200, body={body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["status"], "failed", "body={body}");
    assert!(
        v["failure_reason"].is_string(),
        "failed 应透出 failure_reason, body={body}"
    );
    assert!(v["transcript"].is_null(), "failed 无 transcript");

    // 3. retry 必须在重启任务时读取新的 Dictionary 快照。
    add_dictionary_term(port, &bearer, "RetryTerm").await;
    let (code, body) = retry_session(port, &bearer, &id).await;
    assert_eq!(code, 202, "retry 应 202, body={body}");
    assert!(body.contains("\"status\":\"transcribing\""), "body={body}");
    let row = wait_cleanup_terminal(&storage, &id, Duration::from_secs(10))
        .await
        .expect("超时未推进 completed（retry 重启 sidecar 后转译未完成）");
    assert_eq!(row.status, "completed", "retry 后应推 completed");
    assert!(
        row.transcript_path.is_some(),
        "completed 应回填 transcript_path"
    );
    let requests = captured.lock().unwrap();
    let user_json = requests[0]
        .pointer("/messages/1/content")
        .and_then(serde_json::Value::as_str)
        .unwrap();
    let user: serde_json::Value = serde_json::from_str(user_json).unwrap();
    assert_eq!(user["dictionary_terms"], serde_json::json!(["RetryTerm"]));
    drop(requests);

    // 4. GET 透出 completed + transcript（full_text/units 秒）+ 无 failure_reason。
    let (code, body) = get_session(port, &bearer, &id).await;
    assert_eq!(code, 200, "GET 应 200, body={body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["status"], "completed", "body={body}");
    assert!(v["failure_reason"].is_null(), "completed 无 failure_reason");
    assert_eq!(
        v["transcript"]["full_text"], "你好世界",
        "普通详情只返回纯正文，body={body}"
    );
    let unit = &v["transcript"]["units"][0];
    assert_eq!(unit["text"], "你好世界");
    assert_eq!(unit["start"], 0.0, "start 0ms→0.0s");
    assert_eq!(unit["end"], 1.0, "end 1000ms→1.0s");
}

/// M5.5：失败会话属于 A；切换当前 runtime 到 B 后 retry 必须原子更新模型归属，
/// 同时复用原 session id、加密音频路径/内容和 context，而不是创建替代会话。
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn retry_after_model_switch_reuses_artifacts_and_commits_model_b() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let model_a = Arc::new(MockRuntime::new(
        "model-a",
        RuntimeKind::Whisper,
        CannedResponse::default(),
    ));
    registry
        .register(Arc::clone(&model_a) as Arc<dyn ModelRuntime>)
        .await; // A 未启动，首次 pipeline 必然 failed。
    let (port, storage, _auth, bearer) = spawn_app_with(
        dir.path(),
        Arc::clone(&registry),
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new(copying_normalizer_binary(dir.path()))),
    )
    .await;
    let wav = sine_wav();
    let capture_id = uuid::Uuid::new_v4().to_string();
    let context = ClipboardContextFile {
        schema_version: 1,
        session_id: String::new(),
        capture_id: capture_id.clone(),
        events: vec![ContextEvent {
            sequence: 1,
            source_sample_rate: 48_000,
            sample_offset: 24_000,
            kind: ContextEventKind::ContextEventPlainText as i32,
            plain_text: "model-switch-context".into(),
            html_fragment: String::new(),
            absolute_paths: Vec::new(),
        }],
    };
    let (code, body) = post_sessions_with_context(port, &bearer, &wav, context).await;
    assert_eq!(code, 202, "{body}");
    let session_id = extract_id(&body).unwrap();
    let failed = wait_outcome(&storage, &session_id, Duration::from_secs(5))
        .await
        .expect("model A pipeline should fail");
    assert_eq!(failed.status, "failed");
    assert_eq!(failed.model, "model-a");
    let audio_path = failed.audio_path.clone().unwrap();
    assert_eq!(storage.read_audio(&audio_path).unwrap(), wav);
    let persisted_context = storage
        .read_context(&session_id, failed.created_at)
        .unwrap();
    assert_eq!(persisted_context.capture_id, capture_id);

    registry.clear().await;
    let model_b = Arc::new(MockRuntime::new(
        "model-b",
        RuntimeKind::Whisper,
        CannedResponse::default(),
    ));
    model_b.start(0).await.unwrap();
    registry
        .register(Arc::clone(&model_b) as Arc<dyn ModelRuntime>)
        .await;

    let (code, body) = retry_session(port, &bearer, &session_id).await;
    assert_eq!(code, 202, "{body}");
    assert_eq!(extract_id(&body).as_deref(), Some(session_id.as_str()));
    let completed = wait_outcome(&storage, &session_id, Duration::from_secs(10))
        .await
        .expect("model B retry should complete");
    assert_eq!(completed.status, "completed");
    assert_eq!(completed.model, "model-b");
    assert_eq!(completed.audio_path.as_deref(), Some(audio_path.as_str()));
    assert_eq!(storage.read_audio(&audio_path).unwrap(), wav);
    let retried_context = storage
        .read_context(&session_id, completed.created_at)
        .unwrap();
    assert_eq!(retried_context.capture_id, capture_id);
    assert_eq!(retried_context.events[0].plain_text, "model-switch-context");
    let transcript = storage
        .read_transcript(completed.transcript_path.as_deref().unwrap())
        .unwrap();
    assert_eq!(transcript.session_id, session_id);
    assert_eq!(transcript.model, "model-b");
}

/// 验收 ST-M3.5：Sherpa sidecar 故障不会自动切到其他 backend。失败行保留上下文；
/// retry 仅重启并复用原 Sherpa runtime。
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn sherpa_failure_is_retryable_without_backend_switching() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let sherpa = Arc::new(MockRuntime::new(
        "sensevoice-small-sherpa-int8",
        RuntimeKind::SherpaOnnx,
        CannedResponse::default(),
    )) as Arc<dyn ModelRuntime>;
    registry.register(sherpa).await; // 故意未 start：首次转写模拟 Sherpa sidecar 不可用。
    let (port, storage, _auth, bearer) = spawn_app_with(
        dir.path(),
        registry.clone(),
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new(copying_normalizer_binary(dir.path()))),
    )
    .await;
    let context = ClipboardContextFile {
        schema_version: 1,
        session_id: String::new(),
        capture_id: uuid::Uuid::new_v4().to_string(),
        events: vec![ContextEvent {
            sequence: 1,
            source_sample_rate: 48_000,
            sample_offset: 24_000,
            kind: ContextEventKind::ContextEventPlainText as i32,
            plain_text: "sherpa-retry-context".into(),
            html_fragment: String::new(),
            absolute_paths: Vec::new(),
        }],
    };

    let (code, body) = post_sessions_with_context(port, &bearer, &sine_wav(), context).await;
    assert_eq!(code, 202, "首次提交应 202, body={body}");
    let id = extract_id(&body).expect("202 应含 id");
    let row = wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .expect("超时未推进 Sherpa failed");
    assert_eq!(row.status, "failed");
    assert_eq!(
        row.failure_reason.as_deref(),
        Some("SenseVoice 本地转写失败，可重试；如持续失败，请恢复上一稳定版本。")
    );
    assert!(row.audio_path.is_some(), "失败必须保留音频供 retry 使用");
    assert_eq!(
        registry.active().await.as_deref().map(ModelRuntime::id),
        Some("sensevoice-small-sherpa-int8"),
        "失败不得自动切换到其他 runtime"
    );

    let (code, body) = retry_session(port, &bearer, &id).await;
    assert_eq!(code, 202, "retry 应只重启当前 Sherpa runtime, body={body}");
    let row = wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .expect("超时未完成 Sherpa retry");
    assert_eq!(row.status, "completed");
    assert_eq!(row.model, "sensevoice-small-sherpa-int8");
    assert_eq!(
        registry.active().await.as_deref().map(ModelRuntime::id),
        Some("sensevoice-small-sherpa-int8"),
        "retry 后仍应使用同一 Sherpa runtime"
    );
    let (_, body) = get_session(port, &bearer, &id).await;
    let session: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        session["transcript"]["full_text"], "你好世界",
        "retry 后普通详情仍只返回纯正文"
    );
}

/// 验收 ST-M3.7：占用中 retry → 409。失败会话先就位（normalize 失败→failed，不依赖
/// ffmpeg），再预占 slot，retry 在 acquire 处命中 occupied → 409（先于 sidecar 重启）。
#[tokio::test(flavor = "multi_thread")]
async fn retry_occupied_returns_409() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    registry.register(mock).await; // active()=Some；不 start
    let gate = Arc::new(RuntimeOperationGate::new_for_test());
    // bogus ffmpeg → normalize 失败 → failed（不依赖真 ffmpeg）。
    let normalizer = Arc::new(AudioNormalizer::new(PathBuf::from("ffmpeg-not-found")));
    let (port, storage, _auth, bearer) =
        spawn_app_with(dir.path(), registry, gate.clone(), normalizer).await;

    // 1. 先就位一个 failed 会话（pipeline: normalize 失败 → failed，slot 旋即释放）。
    let wav = sine_wav();
    let (code, body) = post_sessions(port, &bearer, &wav).await;
    assert_eq!(code, 202, "首次提交应 202, body={body}");
    let id = extract_id(&body).expect("202 应含 id");
    let row = wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .expect("超时未推进 failed");
    assert_eq!(row.status, "failed", "应推 failed 以备 retry");

    // 2. 预占 slot（持 guard 跨 retry）。
    let _guard = gate
        .acquire(RuntimeOperation::Transcription {
            session_id: "blocking-retry".into(),
        })
        .unwrap();
    assert!(matches!(
        gate.active(),
        Some(RuntimeOperation::Transcription { session_id })
            if session_id == "blocking-retry"
    ));

    // 3. retry → status==failed 过 → runtime 过 → acquire 命中 occupied → 409。
    let (code, body) = retry_session(port, &bearer, &id).await;
    assert_eq!(code, 409, "占用中 retry 应 409, body={body}");
    assert!(body.contains("transcription slot occupied"), "body={body}");
    // _guard drop 于此 → slot 释放。
}

/// retry 非 failed 会话 → 409（status 守卫）。先就位 completed 会话（需 ffmpeg +
/// 启动 mock），再 retry → 409「not failed」。未装 ffmpeg skip。
#[tokio::test(flavor = "multi_thread")]
async fn retry_non_failed_returns_409() {
    let ffmpeg = match resolve_ffmpeg().await {
        Some(p) => p,
        None => {
            eprintln!("skip: ffmpeg 未找到（需 completed 会话以测 non-failed 守卫）");
            return;
        }
    };
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap(); // 启动 → transcribe 成功 → completed
    registry.register(mock).await;
    let gate = Arc::new(RuntimeOperationGate::new_for_test());
    let normalizer = Arc::new(AudioNormalizer::new(ffmpeg));
    let (port, storage, _auth, bearer) =
        spawn_app_with(dir.path(), registry, gate, normalizer).await;

    // 1. 就位 completed 会话。
    let wav = sine_wav();
    let (code, body) = post_sessions(port, &bearer, &wav).await;
    assert_eq!(code, 202, "首次提交应 202, body={body}");
    let id = extract_id(&body).expect("202 应含 id");
    let row = wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .expect("超时未推进 completed");
    assert_eq!(row.status, "completed", "应先就位 completed");

    // 2. retry completed → status 守卫 → 409「not failed」（先于 slot/重启）。
    let (code, body) = retry_session(port, &bearer, &id).await;
    assert_eq!(code, 409, "非 failed retry 应 409, body={body}");
    assert!(body.contains("not failed"), "body={body}");
}

// ──────────────────────────────────────────────────────────────────────────────
// ST-M3.9 GET /sessions/{id}/audio（解密返回原始音频）
// ─────────────────────────────────────────────────────────────────────────────────

/// `GET /sessions/{id}/audio` 请求封装 → `(状态码, content-type, body bytes)`。
async fn get_session_audio_raw(
    port: u16,
    bearer: &str,
    id: &str,
) -> (u16, Option<String>, Vec<u8>) {
    let resp = reqwest::Client::new()
        .get(format!(
            "http://127.0.0.1:{port}/api/v1/sessions/{id}/audio"
        ))
        .bearer_auth(bearer)
        .send()
        .await
        .unwrap();
    let code = resp.status().as_u16();
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let bytes = resp.bytes().await.unwrap().to_vec();
    (code, ct, bytes)
}

/// `GET /sessions/{id}/audio` 行不存在 → 404 `not_found`。先于 runtime/pipeline，不依赖 ffmpeg。
#[tokio::test(flavor = "multi_thread")]
async fn get_session_audio_not_found_returns_404() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let gate = Arc::new(RuntimeOperationGate::new_for_test());
    let normalizer = Arc::new(AudioNormalizer::new(PathBuf::from("ffmpeg")));
    let (port, _storage, _auth, bearer) =
        spawn_app_with(dir.path(), registry, gate, normalizer).await;

    let (code, _ct, body) =
        get_session_audio_raw(port, &bearer, "00000000-0000-0000-0000-000000000000").await;
    assert_eq!(code, 404, "不存在的会话音频应 404");
    assert!(
        String::from_utf8_lossy(&body).contains("not_found"),
        "body={}",
        String::from_utf8_lossy(&body)
    );
}

/// 验收 ST-M3.9：completed 会话 GET audio → 200 + 原始上传字节逐字节相等（AEAD 解密往返）
/// + content-type 由 `file_name`(audio.wav) 推 audio/wav。需 ffmpeg（成功链路）；未装 skip。
#[tokio::test(flavor = "multi_thread")]
async fn get_session_audio_returns_decrypted_bytes() {
    let ffmpeg = match resolve_ffmpeg().await {
        Some(p) => p,
        None => {
            eprintln!("skip: ffmpeg 未找到（completed audio 往返需成功链路）");
            return;
        }
    };
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap(); // 启动 → transcribe 成功 → completed
    registry.register(mock).await;
    let gate = Arc::new(RuntimeOperationGate::new_for_test());
    let normalizer = Arc::new(AudioNormalizer::new(ffmpeg));
    let (port, storage, _auth, bearer) =
        spawn_app_with(dir.path(), registry, gate, normalizer).await;

    let wav = sine_wav();
    let (code, body) = post_sessions(port, &bearer, &wav).await;
    assert_eq!(code, 202, "body={body}");
    let id = extract_id(&body).expect("202 应含 id");
    let row = wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .expect("超时未推进 completed");
    assert_eq!(row.status, "completed", "应先就位 completed");

    // GET /sessions/{id}/audio → 200 + 原始字节（解密往返）+ content-type audio/wav。
    let (code, ct, bytes) = get_session_audio_raw(port, &bearer, &id).await;
    assert_eq!(code, 200, "audio 应 200");
    assert_eq!(
        bytes, wav,
        "返回字节应与原始上传音频逐字节相等（AEAD 解密往返）"
    );
    assert_eq!(
        ct.as_deref(),
        Some("audio/wav"),
        "content-type 由 file_name(audio.wav) 推 audio/wav"
    );
}

/// 验收 ST-M3.9（失败会话音频）：bogus ffmpeg → normalize 失败 → failed，但音频已
/// 在 handler 同步阶段 `write_audio` 落库并保留 → GET audio 仍 200 + 原始字节往返。
/// 不依赖真 ffmpeg（bogus 路径即触发 normalize 失败）。
#[tokio::test(flavor = "multi_thread")]
async fn get_session_audio_for_failed_retains_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    registry.register(mock).await; // 不 start（normalize 先失败，transcribe 不到达）
    let gate = Arc::new(RuntimeOperationGate::new_for_test());
    // bogus ffmpeg → normalize 失败 → pipeline 推 failed（slot 旋即释放）。
    let normalizer = Arc::new(AudioNormalizer::new(PathBuf::from("ffmpeg-not-found")));
    let (port, storage, _auth, bearer) =
        spawn_app_with(dir.path(), registry, gate, normalizer).await;

    let wav = sine_wav();
    let (code, body) = post_sessions(port, &bearer, &wav).await;
    assert_eq!(code, 202, "body={body}");
    let id = extract_id(&body).expect("202 应含 id");
    let row = wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .expect("超时未推进 failed");
    assert_eq!(row.status, "failed", "bogus ffmpeg 应推 failed");

    // 失败会话音频仍可 GET 200 + 原始字节（handler 同步落库、失败不删）。
    let (code, _ct, bytes) = get_session_audio_raw(port, &bearer, &id).await;
    assert_eq!(code, 200, "失败会话音频应仍可 GET 200");
    assert_eq!(bytes, wav, "返回字节=原始上传音频（失败保留）");
}

/// 验收 ST-M3.9（完整性故障→500）：completed 会话就位后，把 `audio.enc` 写成损坏
/// 字节（AEAD 认证必失败）→ GET audio 不应被吞成 404「未找到」，应 **500 internal**
/// （真实完整性故障，与 retry 的 `read_audio ?` 一致；reconcile 只判 `.exists()` 不清
/// 此类 zombie）。需 ffmpeg（completed 会话就位）；未装 skip。
#[tokio::test(flavor = "multi_thread")]
async fn get_session_audio_decrypt_failure_returns_500() {
    let ffmpeg = match resolve_ffmpeg().await {
        Some(p) => p,
        None => {
            eprintln!("skip: ffmpeg 未找到（需 completed 会话以 corrupt 其 audio）");
            return;
        }
    };
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap();
    registry.register(mock).await;
    let gate = Arc::new(RuntimeOperationGate::new_for_test());
    let normalizer = Arc::new(AudioNormalizer::new(ffmpeg));
    let (port, storage, _auth, bearer) =
        spawn_app_with(dir.path(), registry, gate, normalizer).await;

    let wav = sine_wav();
    let (code, body) = post_sessions(port, &bearer, &wav).await;
    assert_eq!(code, 202, "body={body}");
    let id = extract_id(&body).expect("202 应含 id");
    let row = wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .expect("超时未推进 completed");
    assert_eq!(row.status, "completed");

    // 损坏 audio.enc（AEAD 认证必失败）：拼 abs = {data_dir}/data/{account_id}/{audio_rel}。
    let audio_rel = row.audio_path.as_ref().expect("audio_path 应存留");
    let abs = dir
        .path()
        .join("data")
        .join(&row.account_id)
        .join(audio_rel);
    std::fs::write(&abs, b"corrupt-bytes-not-audio").unwrap();

    // GET audio → 500 internal（解密失败 = 完整性故障，不再吞成 404）。
    let (code, _ct, body) = get_session_audio_raw(port, &bearer, &id).await;
    assert_eq!(
        code,
        500,
        "损坏音频应 500 internal（非 404），body={}",
        String::from_utf8_lossy(&body)
    );
    assert!(
        String::from_utf8_lossy(&body).contains("internal"),
        "body={}",
        String::from_utf8_lossy(&body)
    );
}

/// M5.5：密码确认后 ZIP 含同构目录下的原始音频与 transcript.json；错密→403。
#[tokio::test(flavor = "multi_thread")]
async fn export_returns_zip_and_rejects_wrong_password() {
    let Some(ffmpeg) = resolve_ffmpeg().await else {
        eprintln!("skip: ffmpeg 未找到");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap();
    registry.register(mock).await;
    let (port, storage, _auth, bearer) = spawn_app_with(
        dir.path(),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new(ffmpeg)),
    )
    .await;
    let wav = sine_wav();
    let (code, body) = post_sessions_with_filename(port, &bearer, &wav, "../../outside.wav").await;
    assert_eq!(code, 202, "{body}");
    let id = extract_id(&body).unwrap();
    assert_eq!(
        wait_outcome(&storage, &id, Duration::from_secs(10))
            .await
            .unwrap()
            .status,
        "completed"
    );
    let client = reqwest::Client::new();
    let wrong = client
        .post(format!("http://127.0.0.1:{port}/api/v1/export"))
        .bearer_auth(&bearer)
        .json(&serde_json::json!({"password":"bad"}))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status().as_u16(), 403);
    let ok = client
        .post(format!("http://127.0.0.1:{port}/api/v1/export"))
        .bearer_auth(&bearer)
        .json(&serde_json::json!({"password":"p","session_ids":[id]}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status().as_u16(), 200);
    assert_eq!(ok.headers()["content-type"], "application/zip");
    let bytes = ok.bytes().await.unwrap();
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let names: Vec<String> = zip.file_names().map(str::to_owned).collect();
    assert!(names.iter().any(|name| name.ends_with("/outside.wav")));
    assert!(names.iter().any(|name| name.ends_with("/transcript.json")));
    assert!(
        names
            .iter()
            .all(|name| !name.contains("..") && !name.starts_with('/')),
        "ZIP entry 不得包含路径穿越：{names:?}"
    );

    let audio_name = names
        .iter()
        .find(|name| name.ends_with("/outside.wav"))
        .unwrap();
    let mut audio = Vec::new();
    zip.by_name(audio_name)
        .unwrap()
        .read_to_end(&mut audio)
        .unwrap();
    assert_eq!(audio, wav, "导出音频应与上传原始字节一致");
    let transcript_name = names
        .iter()
        .find(|name| name.ends_with("/transcript.json"))
        .unwrap();
    let mut transcript_json = String::new();
    zip.by_name(transcript_name)
        .unwrap()
        .read_to_string(&mut transcript_json)
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&transcript_json).unwrap();
    assert_eq!(json["session"]["id"], id);
    assert_eq!(json["transcript"]["full_text"], "你好世界");
    assert!(json["speakers"].is_array(), "应始终导出 speakers：{json}");
}

// ──────────────────────────────────────────────────────────────────────────────
// ST-M4.4：内部注入计划端点（root-only、进程内一次性、第三方/webview 不可达）
// ──────────────────────────────────────────────────────────────────────────────

/// reqwest GET 注入计划 → (状态码, body JSON)。
async fn get_injection_plan(port: u16, bearer: &str, id: &str) -> (u16, serde_json::Value) {
    let resp = reqwest::Client::new()
        .get(format!(
            "http://127.0.0.1:{port}/api/v1/sessions/{id}/injection-plan"
        ))
        .bearer_auth(bearer)
        .send()
        .await
        .unwrap();
    let code = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    (code, body)
}

async fn get_workspace_detail(port: u16, bearer: &str, id: &str) -> (u16, serde_json::Value) {
    let resp = reqwest::Client::new()
        .get(format!(
            "http://127.0.0.1:{port}/api/v1/sessions/{id}/workspace-detail"
        ))
        .bearer_auth(bearer)
        .send()
        .await
        .unwrap();
    let code = resp.status().as_u16();
    let body = resp.json().await.unwrap_or(serde_json::Value::Null);
    (code, body)
}

async fn get_cleanup_detail(port: u16, bearer: &str, id: &str) -> (u16, serde_json::Value) {
    let resp = reqwest::Client::new()
        .get(format!(
            "http://127.0.0.1:{port}/api/v1/sessions/{id}/cleanup-detail"
        ))
        .bearer_auth(bearer)
        .send()
        .await
        .unwrap();
    let code = resp.status().as_u16();
    let body = resp.json().await.unwrap_or(serde_json::Value::Null);
    (code, body)
}

async fn get_resolve_resource(
    port: u16,
    bearer: &str,
    id: &str,
    sequence: u32,
    index: usize,
) -> (u16, serde_json::Value) {
    let resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/api/v1/sessions/{id}/context/{sequence}/resources/{index}/resolve"))
        .bearer_auth(bearer)
        .send().await.unwrap();
    let code = resp.status().as_u16();
    let body = resp.json().await.unwrap_or(serde_json::Value::Null);
    (code, body)
}

async fn get_resolve_link(
    port: u16,
    bearer: &str,
    id: &str,
    sequence: u32,
) -> (u16, serde_json::Value) {
    let resp = reqwest::Client::new()
        .get(format!(
            "http://127.0.0.1:{port}/api/v1/sessions/{id}/context/{sequence}/link"
        ))
        .bearer_auth(bearer)
        .send()
        .await
        .unwrap();
    let code = resp.status().as_u16();
    let body = resp.json().await.unwrap_or(serde_json::Value::Null);
    (code, body)
}

#[tokio::test(flavor = "multi_thread")]
#[cfg(unix)]
async fn concurrent_context_images_both_return_thumbnails() {
    let dir = tempfile::tempdir().unwrap();
    // Keep the storage root and image paths in the same canonical namespace
    // (macOS temporary directories may otherwise use /var -> /private/var).
    let home = dir.path().canonicalize().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap();
    registry.register(mock).await;
    let (port, storage, auth, bearer) = spawn_app_with(
        &home,
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new(copying_normalizer_binary(&home))),
    )
    .await;
    let capture_id = uuid::Uuid::new_v4().to_string();
    let image_dir = home
        .join("cache/clipboard-context")
        .join(auth.active_account_id().unwrap())
        .join(&capture_id);
    std::fs::create_dir_all(&image_dir).unwrap();
    let image_dir = image_dir.canonicalize().unwrap();
    let mut events = Vec::new();
    for sequence in 1..=2 {
        let path = image_dir.join(format!("{sequence}.png"));
        let mut encoder = png::Encoder::new(std::fs::File::create(&path).unwrap(), 2000, 1000);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&vec![255; 2000 * 1000 * 4])
            .unwrap();
        events.push(ContextEvent {
            sequence,
            source_sample_rate: 48_000,
            sample_offset: sequence as u64 * 12_000,
            kind: ContextEventKind::ContextEventImage as i32,
            plain_text: String::new(),
            html_fragment: String::new(),
            absolute_paths: vec![path.to_string_lossy().into_owned()],
        });
    }
    let context = ClipboardContextFile {
        schema_version: 1,
        session_id: String::new(),
        capture_id,
        events,
    };
    let (code, body) = post_sessions_with_context(port, &bearer, &sine_wav(), context).await;
    assert_eq!(code, 202, "{body}");
    let id = extract_id(&body).unwrap();
    wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .unwrap();
    let client = reqwest::Client::new();
    let thumbnail = |sequence| {
        client
            .get(format!(
        "http://127.0.0.1:{port}/api/v1/sessions/{id}/context/{sequence}/resources/0/thumbnail"
    ))
            .bearer_auth(&bearer)
            .send()
    };
    let (first, second) = tokio::join!(thumbnail(1), thumbnail(2));
    for response in [first.unwrap(), second.unwrap()] {
        if response.status() != 200 {
            panic!("thumbnail failed: {}", response.text().await.unwrap());
        }
        assert_eq!(response.headers()["content-type"], "image/png");
        assert_eq!(response.headers()["x-thumbnail-width"], "320");
        assert_eq!(response.headers()["x-thumbnail-height"], "160");
        let bytes = response.bytes().await.unwrap();
        let reader = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .unwrap();
        assert_eq!((reader.info().width, reader.info().height), (320, 160));
    }
    // Opening the original remains independent of thumbnail generation.
    let (code, _) = get_resolve_resource(port, &bearer, &id, 1, 0).await;
    assert_eq!(code, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn resource_endpoints_are_root_only_and_resolve_file_refs_with_stable_errors() {
    let ffmpeg = match resolve_ffmpeg().await {
        Some(p) => p,
        None => {
            eprintln!("skip: ffmpeg 未找到");
            return;
        }
    };
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap();
    registry.register(mock).await;
    let (port, storage, auth, bearer) = spawn_app_with(
        dir.path(),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new(ffmpeg)),
    )
    .await;
    // 真实常规文件资源：resolve 端点需验证文件仍存在、非可执行。先 canonicalize 以
    // 解析 macOS $TMPDIR 的 /var→/private/var 符号链接，避免父链 symlink 校验误拒。
    let file = dir.path().join("plan.txt");
    std::fs::write(&file, "x").unwrap();
    let file = file.canonicalize().unwrap();
    let context = ClipboardContextFile {
        schema_version: 1,
        session_id: String::new(),
        capture_id: uuid::Uuid::new_v4().to_string(),
        events: vec![ContextEvent {
            sequence: 1,
            source_sample_rate: 48_000,
            sample_offset: 24_000,
            kind: ContextEventKind::ContextEventFiles as i32,
            plain_text: String::new(),
            html_fragment: String::new(),
            absolute_paths: vec![file.to_string_lossy().into_owned()],
        }],
    };
    let (code, body) = post_sessions_with_context(port, &bearer, &sine_wav(), context).await;
    assert_eq!(code, 202, "body={body}");
    let id = extract_id(&body).unwrap();
    wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .unwrap();

    let root = auth.verify(&bearer).unwrap();
    let third = auth
        .issue_token(&root, "third-party", vec!["sessions:read".into()])
        .unwrap();

    // root 可解析文件资源；路径归属当前会话、kind=file。
    let (code, resolved) = get_resolve_resource(port, &bearer, &id, 1, 0).await;
    assert_eq!(code, 200, "{resolved}");
    assert_eq!(resolved["kind"].as_str(), Some("file"));
    assert_eq!(resolved["path"].as_str(), Some(file.to_str().unwrap()));
    // 第三方 sessions:read 不得解析资源：root-only 门禁。
    let (code, _) = get_resolve_resource(port, &third.secret_bearer, &id, 1, 0).await;
    assert_eq!(code, 403);
    // 越权索引返回稳定 resource_unavailable，不崩、不泄露路径。
    let (code, body) = get_resolve_resource(port, &bearer, &id, 1, 99).await;
    assert_eq!(code, 409);
    assert_eq!(body["error"]["code"].as_str(), Some("resource_unavailable"));
    // 非链接事件解析 link 返回稳定 unsupported_scheme。
    let (code, body) = get_resolve_link(port, &bearer, &id, 1).await;
    assert_eq!(code, 409);
    assert_eq!(body["error"]["code"].as_str(), Some("unsupported_scheme"));
    // 不存在的事件序号返回稳定 context_not_found（区别于通用 not_found）。
    let (code, body) = get_resolve_resource(port, &bearer, &id, 999, 0).await;
    assert_eq!(code, 404);
    assert_eq!(body["error"]["code"].as_str(), Some("context_not_found"));
    // 第三方 link 同样 403。
    let (code, _) = get_resolve_link(port, &third.secret_bearer, &id, 1).await;
    assert_eq!(code, 403);
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_detail_is_root_only_and_returns_typed_items() {
    let ffmpeg = match resolve_ffmpeg().await {
        Some(p) => p,
        None => {
            eprintln!("skip: ffmpeg 未找到");
            return;
        }
    };
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap();
    registry.register(mock).await;
    let (port, storage, auth, bearer) = spawn_app_with(
        dir.path(),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new(ffmpeg)),
    )
    .await;
    let (code, body) = post_sessions(port, &bearer, &sine_wav()).await;
    assert_eq!(code, 202);
    let id = extract_id(&body).unwrap();
    wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .unwrap();
    let (code, detail) = get_workspace_detail(port, &bearer, &id).await;
    assert_eq!(code, 200, "{detail}");
    assert!(detail["display_items"].is_array());
    assert!(detail["full_text"].is_string());
    assert_eq!(detail["context_degraded"], false);
    let root = auth.verify(&bearer).unwrap();
    let third = auth
        .issue_token(&root, "third-party", vec!["sessions:read".into()])
        .unwrap();
    let (code, _) = get_workspace_detail(port, &third.secret_bearer, &id).await;
    assert_eq!(code, 403);
    let (code, _) = get_cleanup_detail(port, &third.secret_bearer, &id).await;
    assert_eq!(code, 403);
}

/// root 取计划 200（plain + 安全 html，script 剥离）→ 二次 410 Gone（capability 不可复用）。
#[tokio::test(flavor = "multi_thread")]
async fn injection_plan_root_serves_once_then_gone() {
    let ffmpeg = match resolve_ffmpeg().await {
        Some(p) => p,
        None => {
            eprintln!("skip: ffmpeg 未找到");
            return;
        }
    };
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap();
    registry.register(mock).await;
    let (port, storage, _auth, bearer) = spawn_app_with(
        dir.path(),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new(ffmpeg)),
    )
    .await;

    // RichText 事件：plain=hi，html=<b>hi</b>+<script>（验 sanitize 后 script 剥离、富文本嵌入）。
    let context = ClipboardContextFile {
        schema_version: 1,
        session_id: String::new(),
        capture_id: uuid::Uuid::new_v4().to_string(),
        events: vec![ContextEvent {
            sequence: 1,
            source_sample_rate: 48_000,
            sample_offset: 24_000,
            kind: ContextEventKind::ContextEventRichText as i32,
            plain_text: "hi".into(),
            html_fragment: "<b>hi</b><script>alert(1)</script>".into(),
            absolute_paths: Vec::new(),
        }],
    };
    let (code, body) = post_sessions_with_context(port, &bearer, &sine_wav(), context).await;
    assert_eq!(code, 202, "body={body}");
    let id = extract_id(&body).unwrap();
    let row = wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .expect("超时未 completed");
    assert_eq!(row.status, "completed");

    let (code, plan) = get_injection_plan(port, &bearer, &id).await;
    assert_eq!(code, 200, "root 首读应 200: {plan}");
    let plain = plan["plain"].as_str().expect("plain 字段");
    let html = plan["html"].as_str().expect("html 字段");
    assert!(
        plain.contains("你好世界") && plain.contains("剪贴板上下文：hi"),
        "动态合成 plain: {plain}"
    );
    assert!(
        html.contains("<b>hi</b>") || html.contains("hi"),
        "动态合成 html: {html}"
    );
    assert!(
        !html.contains("script") && !html.contains("alert"),
        "html 不含 script: {html}"
    );

    // capability 不可复用：二次同会话 → 410。
    let (code, _body) = get_injection_plan(port, &bearer, &id).await;
    assert_eq!(code, 410, "二次取应 410 gone");
}

/// 第三方 sessions:read token → 403（is_root=false，require_root 拒绝）。
#[tokio::test(flavor = "multi_thread")]
async fn injection_plan_third_party_token_forbidden() {
    let ffmpeg = match resolve_ffmpeg().await {
        Some(p) => p,
        None => {
            eprintln!("skip: ffmpeg 未找到");
            return;
        }
    };
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap();
    registry.register(mock).await;
    let (port, storage, auth, bearer) = spawn_app_with(
        dir.path(),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new(ffmpeg)),
    )
    .await;

    let (code, body) = post_sessions(port, &bearer, &sine_wav()).await;
    assert_eq!(code, 202, "body={body}");
    let id = extract_id(&body).unwrap();
    wait_outcome(&storage, &id, Duration::from_secs(10))
        .await
        .expect("超时未 completed");

    // 签发第三方 sessions:read token（非 root）。
    let root_caller = auth.verify(&bearer).unwrap();
    let third = auth
        .issue_token(&root_caller, "third-party", vec!["sessions:read".into()])
        .unwrap();
    let (code, _body) = get_injection_plan(port, &third.secret_bearer, &id).await;
    assert_eq!(code, 403, "第三方 token 应 403 insufficient_scope");
}

/// 不存在的会话 → 404。
#[tokio::test(flavor = "multi_thread")]
async fn injection_plan_missing_session_not_found() {
    let ffmpeg = match resolve_ffmpeg().await {
        Some(p) => p,
        None => {
            eprintln!("skip: ffmpeg 未找到");
            return;
        }
    };
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(SidecarRegistry::new());
    let mock = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    mock.start(0).await.unwrap();
    registry.register(mock).await;
    let (port, _storage, _auth, bearer) = spawn_app_with(
        dir.path(),
        registry,
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new(ffmpeg)),
    )
    .await;

    let missing = uuid::Uuid::new_v4().to_string();
    let (code, _body) = get_injection_plan(port, &bearer, &missing).await;
    assert_eq!(code, 404, "不存在的会话应 404");
}

/// 非完成会话 → 409；已完成但无 transcript_path（不一致状态）→ 404。不经 pipeline，
/// 直接插行测端点分支（无需 ffmpeg/mock runtime）。
#[tokio::test(flavor = "multi_thread")]
async fn injection_plan_non_completed_and_missing_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let (port, storage, auth, bearer) = spawn_app_with(
        dir.path(),
        Arc::new(SidecarRegistry::new()),
        Arc::new(RuntimeOperationGate::new_for_test()),
        Arc::new(AudioNormalizer::new(PathBuf::from("ffmpeg"))),
    )
    .await;
    let account_id = auth.verify(&bearer).unwrap().account_id;

    // transcribing 行 → status != completed → 409（在 mark_consumed 之前，不消费计划）。
    let transcribing = SessionRow {
        id: uuid::Uuid::new_v4().to_string(),
        account_id: account_id.clone(),
        created_at: 1_700_000_000,
        source: "realtime".into(),
        language: "zh".into(),
        duration_sec: 0.0,
        status: "transcribing".into(),
        model: "mock".into(),
        input_device: None,
        file_name: None,
        audio_path: None,
        transcript_path: None,
        failure_reason: None,
        context_present: false,
        cleanup_status: "not_requested".into(),
        cleanup_path: None,
        cleanup_error_code: None,
    };
    storage.insert_session(&transcribing).unwrap();
    let (code, _body) = get_injection_plan(port, &bearer, &transcribing.id).await;
    assert_eq!(code, 409, "transcribing 应 409 conflict");

    // completed 但无 transcript_path（不一致状态）→ 404。
    let no_transcript = SessionRow {
        id: uuid::Uuid::new_v4().to_string(),
        created_at: 1_700_000_001,
        status: "completed".into(),
        ..transcribing
    };
    storage.insert_session(&no_transcript).unwrap();
    let (code, _body) = get_injection_plan(port, &bearer, &no_transcript.id).await;
    assert_eq!(code, 404, "completed 但无 transcript_path 应 404");
}
