//! Daemon transport adapters for the desktop-core realtime ports.

use crate::platform::recording::CapturedRecording;
use prost::Message;
use seasnail_desktop_core::{
    CapturedAudio, ContextPayload, SessionStatus, SessionStatusGateway, SubmissionAccepted,
    SubmissionGateway, SubmissionRequest,
};
use serde::{Deserialize, Serialize};

impl SubmissionGateway for DaemonClient {
    fn submit(
        &self,
        request: SubmissionRequest,
        context: Option<&ContextPayload>,
    ) -> Result<SubmissionAccepted, String> {
        let context = context
            .filter(|payload| !payload.as_bytes().is_empty())
            .map(|payload| ClipboardContextFile::decode(payload.as_bytes()))
            .transpose()
            .map_err(|_| "recording_context_invalid".to_string())?;
        let response: RecordingSubmission =
            self.submit_realtime_wav(request.wav, &request.input_device, context.as_ref())?;
        Ok(SubmissionAccepted {
            session_id: response.id,
            status: response.status,
        })
    }
}

#[derive(Deserialize)]
struct SessionStatusResponse {
    status: String,
    /// Additive: daemons predating `failure_reason` deserialize as `None`.
    #[serde(default)]
    failure_reason: Option<String>,
}

fn map_session_status_response(body: serde_json::Value) -> Result<SessionStatus, String> {
    let value: SessionStatusResponse =
        serde_json::from_value(body).map_err(|_| "recording_status_failed".to_string())?;
    match value.status.as_str() {
        "transcribing" => Ok(SessionStatus::Transcribing),
        "cleaning_up" => Ok(SessionStatus::CleaningUp),
        "completed" => Ok(SessionStatus::Completed),
        "failed" => Ok(SessionStatus::Failed(value.failure_reason)),
        _ => Err("recording_status_invalid".into()),
    }
}

impl SessionStatusGateway for DaemonClient {
    fn status(&self, session_id: &str) -> Result<SessionStatus, String> {
        let response = self.realtime_status_request(session_id)?;
        if !(200..300).contains(&response.status) {
            return Err("recording_status_failed".into());
        }
        map_session_status_response(response.body)
    }
}

use crate::platform::resource::{ResolvedLink, ResolvedResource};
use crate::{validate_api_request, ApiRequest, ApiResponse, InjectionPlan};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use seasnail_crypto::{Argon2Params, KeychainStore};
use seasnail_daemon::{Bootstrap, Crypto};
use seasnail_proto::seasnail::v1::ClipboardContextFile;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
const DAEMON_START_TIMEOUT: Duration = Duration::from_secs(20);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(100);
const DEV_FILE_KEYCHAIN_ENV: &str = "SEASNAIL_DEV_FILE_KEYCHAIN";
const DAEMON_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const DAEMON_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DAEMON_LIVENESS_TIMEOUT: Duration = Duration::from_millis(250);
const DAEMON_STATUS_TIMEOUT: Duration = Duration::from_secs(2);
const DAEMON_INJECTION_PLAN_TIMEOUT: Duration = Duration::from_secs(5);
const DAEMON_UPLOAD_TIMEOUT: Duration = Duration::from_secs(30);
const DAEMON_TERM_GRACE: Duration = Duration::from_secs(1);

#[derive(Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum CredentialWriteRequest<'a> {
    Credential { credential: &'a str },
    NoAuth,
}

#[derive(Deserialize)]
struct CredentialStateResponse {
    credential_state: String,
}
fn build_daemon_client(timeout: Duration) -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        // Credentials are only sent directly to the fixed loopback daemon.
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(DAEMON_CONNECT_TIMEOUT)
        .timeout(timeout)
        .build()
        .expect("daemon HTTP client configuration must be valid")
}

pub(crate) fn build_general_daemon_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        // Credentials are only sent directly to the fixed loopback daemon.
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(DAEMON_CONNECT_TIMEOUT)
        .timeout(DAEMON_REQUEST_TIMEOUT)
        .build()
        .expect("daemon HTTP client configuration must be valid")
}

/// GUI 壳掌握的守护进程连接。bearer 永不序列化给 WebView。
#[derive(Clone)]
pub(crate) struct DaemonClient {
    pub(crate) port: u16,
    pub(crate) bearer: Option<String>,
    pub(crate) request_client: reqwest::blocking::Client,
    pub(crate) status_client: reqwest::blocking::Client,
    pub(crate) upload_client: reqwest::blocking::Client,
    pub(crate) injection_client: reqwest::blocking::Client,
}

impl DaemonClient {
    pub(crate) fn new(port: u16, bearer: Option<String>) -> Self {
        Self {
            port,
            bearer,
            request_client: build_general_daemon_client(),
            status_client: build_daemon_client(DAEMON_STATUS_TIMEOUT),
            upload_client: build_daemon_client(DAEMON_UPLOAD_TIMEOUT),
            injection_client: build_daemon_client(DAEMON_INJECTION_PLAN_TIMEOUT),
        }
    }

    pub(crate) fn desktop_auth(
        &self,
        home: &Path,
        action: &str,
        body: Option<Value>,
    ) -> Result<Value, String> {
        if !["status", "login", "create", "logout"].contains(&action) {
            return Err("bad_request".into());
        }
        let key = seasnail_daemon::desktop_auth::read_capability(home)
            .map_err(|_| "connection".to_string())?;
        let mut request = self
            .request_client
            .request(
                if action == "status" {
                    reqwest::Method::GET
                } else {
                    reqwest::Method::POST
                },
                format!(
                    "http://127.0.0.1:{}/internal/desktop-auth/{action}",
                    self.port
                ),
            )
            .header("x-seasnail-desktop-capability", key);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().map_err(|_| "connection".to_string())?;
        let success = response.status().is_success();
        let body: Value = response.json().map_err(|_| "connection".to_string())?;
        if !success {
            return Err(body
                .pointer("/error/code")
                .and_then(Value::as_str)
                .unwrap_or("generic")
                .to_owned());
        }
        Ok(body)
    }

    /// 原生 HTTP transport 的 API 基址；M6.1a 将使用它代理 OpenAPI 请求。
    pub fn api_base_url(&self) -> String {
        format!("http://127.0.0.1:{}/api/v1", self.port)
    }

    /// 仅原生 transport 使用的 Authorization 值，绝不可通过 Tauri command 返回。
    pub fn authorization_header(&self) -> Option<String> {
        self.bearer.as_ref().map(|token| format!("Bearer {token}"))
    }

    pub(crate) fn has_root_token(&self) -> bool {
        self.bearer.is_some()
    }

    pub(crate) fn request(&self, request: &ApiRequest) -> Result<ApiResponse, String> {
        validate_api_request(request)?;
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|_| "不支持的 HTTP 方法".to_string())?;
        let mut builder = self
            .request_client
            .request(method, format!("{}{}", self.api_base_url(), request.path))
            .header("x-trace-id", uuid::Uuid::new_v4().to_string());
        if let Some(query) = &request.query {
            builder = builder.query(query);
        }
        if let Some(bearer) = self.authorization_header() {
            builder = builder.header(reqwest::header::AUTHORIZATION, bearer);
        }
        if let Some(body) = &request.body {
            builder = builder.json(body);
        }
        let response = builder.send().map_err(|_| "connection".to_string())?;
        let status = response.status().as_u16();
        let body = response.json().unwrap_or(Value::Null);
        Ok(ApiResponse { status, body })
    }

    /// 专用只写 credential 通道。该路径刻意绕过通用 WebView API 白名单；调用方
    /// 只得到状态，daemon 永不提供读取 secret 的对称端点。
    pub(crate) fn set_provider_credential(
        &self,
        config_id: &str,
        credential: Option<&seasnail_crypto::CredentialSecret>,
    ) -> Result<String, String> {
        validate_provider_config_id(config_id)?;
        let request = match credential {
            Some(secret) => CredentialWriteRequest::Credential {
                credential: std::str::from_utf8(secret.expose_secret())
                    .map_err(|_| "invalid_credential".to_string())?,
            },
            None => CredentialWriteRequest::NoAuth,
        };
        let response = self
            .request_client
            .put(format!(
                "{}/internal/reasoning/provider-configs/{config_id}/credential",
                self.api_base_url()
            ))
            .header("x-trace-id", uuid::Uuid::new_v4().to_string())
            .header(
                reqwest::header::AUTHORIZATION,
                self.authorization_header()
                    .ok_or_else(|| "connection".to_string())?,
            )
            .json(&request)
            .send()
            .map_err(|_| "connection".to_string())?;
        if !response.status().is_success() {
            return Err(daemon_error(response, "credential_write_failed"));
        }
        response
            .json::<CredentialStateResponse>()
            .map(|value| value.credential_state)
            .map_err(|_| "credential_write_failed".to_string())
    }

    pub(crate) fn delete_provider_credential(&self, config_id: &str) -> Result<String, String> {
        validate_provider_config_id(config_id)?;
        let response = self
            .request_client
            .delete(format!(
                "{}/internal/reasoning/provider-configs/{config_id}/credential",
                self.api_base_url()
            ))
            .header("x-trace-id", uuid::Uuid::new_v4().to_string())
            .header(
                reqwest::header::AUTHORIZATION,
                self.authorization_header()
                    .ok_or_else(|| "connection".to_string())?,
            )
            .send()
            .map_err(|_| "connection".to_string())?;
        if !response.status().is_success() {
            return Err(daemon_error(response, "credential_delete_failed"));
        }
        response
            .json::<CredentialStateResponse>()
            .map(|value| value.credential_state)
            .map_err(|_| "credential_delete_failed".to_string())
    }

    pub(crate) fn realtime_status_request(&self, session_id: &str) -> Result<ApiResponse, String> {
        if session_id.is_empty() || session_id.contains('/') {
            return Err("recording_status_invalid".into());
        }
        let mut builder = self
            .status_client
            .get(format!("{}/sessions/{session_id}", self.api_base_url()))
            .header("x-trace-id", uuid::Uuid::new_v4().to_string());
        if let Some(bearer) = self.authorization_header() {
            builder = builder.header(reqwest::header::AUTHORIZATION, bearer);
        }
        let response = builder.send().map_err(|_| "connection".to_string())?;
        let status = response.status().as_u16();
        let body = response.json().unwrap_or(Value::Null);
        Ok(ApiResponse { status, body })
    }

    pub(crate) fn export_zip(&self, body: &Value) -> Result<Vec<u8>, String> {
        let mut builder = self
            .request_client
            .post(format!("{}/export", self.api_base_url()))
            .header("x-trace-id", uuid::Uuid::new_v4().to_string())
            .json(body);
        if let Some(bearer) = self.authorization_header() {
            builder = builder.header(reqwest::header::AUTHORIZATION, bearer);
        }
        let response = builder.send().map_err(|_| "connection".to_string())?;
        if !response.status().is_success() {
            return Err(daemon_error(response, "connection"));
        }
        response
            .bytes()
            .map(|b| b.to_vec())
            .map_err(|_| "connection".to_string())
    }

    /// 直连 root-only Dictionary CSV 导出；完整词表不经过 WebView JSON bridge。
    pub(crate) fn export_dictionary_csv(&self) -> Result<Vec<u8>, String> {
        let response = self
            .request_client
            .get(format!("{}/dictionary/export", self.api_base_url()))
            .header("x-trace-id", uuid::Uuid::new_v4().to_string())
            .header(
                reqwest::header::AUTHORIZATION,
                self.authorization_header()
                    .ok_or_else(|| "connection".to_string())?,
            )
            .send()
            .map_err(|_| "connection".to_string())?;
        if !response.status().is_success() {
            return Err(daemon_error(response, "dictionary_export_failed"));
        }
        response
            .bytes()
            .map(|bytes| bytes.to_vec())
            .map_err(|_| "dictionary_export_failed".to_string())
    }

    pub(crate) fn submit_realtime_wav(
        &self,
        wav: Vec<u8>,
        input_device: &str,
        clipboard_context: Option<&ClipboardContextFile>,
    ) -> Result<RecordingSubmission, String> {
        let mut form = reqwest::blocking::multipart::Form::new()
            .part(
                "audio",
                reqwest::blocking::multipart::Part::bytes(wav)
                    .file_name("seasnail-realtime.wav")
                    .mime_str("audio/wav")
                    .map_err(|_| "recording_submit_failed".to_string())?,
            )
            .text("source", "realtime")
            .text("language", "mixed")
            .text("input_device", input_device.to_owned());
        if let Some(context) = clipboard_context {
            form = form.part(
                "clipboard_context",
                reqwest::blocking::multipart::Part::bytes(context.encode_to_vec())
                    .file_name("clipboard-context.pb")
                    .mime_str("application/octet-stream")
                    .map_err(|_| "recording_submit_failed".to_string())?,
            );
        }
        let mut builder = self
            .upload_client
            .post(format!("{}/sessions", self.api_base_url()))
            .header("x-trace-id", uuid::Uuid::new_v4().to_string())
            .multipart(form);
        if let Some(bearer) = self.authorization_header() {
            builder = builder.header(reqwest::header::AUTHORIZATION, bearer);
        }
        let response = builder
            .send()
            .map_err(|_| "recording_submit_failed".to_string())?;
        if !response.status().is_success() {
            // 复用 daemon 稳定错误码（如 payload_too_large / service_unavailable）；
            // 无可解析码时回退 recording_submit_failed，不向 UI 透传 HTTP 细节。
            let code = response
                .text()
                .ok()
                .and_then(|body| serde_json::from_str::<Value>(&body).ok())
                .and_then(|v| v.get("error")?.get("code")?.as_str().map(str::to_string))
                .unwrap_or_else(|| "recording_submit_failed".to_string());
            return Err(code);
        }
        response
            .json()
            .map_err(|_| "recording_submit_failed".to_string())
    }

    /// ST-M4.4：直连读取内部注入计划（绕过 `validate_api_request`，root bearer）。
    /// 仅原生 Coordinator 的自动粘贴流水线调用；WebView 不接触注入计划。返回 plain 全文 + 安全 html。
    pub(crate) fn fetch_injection_plan(&self, session_id: &str) -> Result<InjectionPlan, String> {
        let mut builder = self
            .injection_client
            .get(format!(
                "{}/sessions/{}/injection-plan",
                self.api_base_url(),
                session_id
            ))
            .header("x-trace-id", uuid::Uuid::new_v4().to_string());
        if let Some(bearer) = self.authorization_header() {
            builder = builder.header(reqwest::header::AUTHORIZATION, bearer);
        }
        let response = builder.send().map_err(|_| "connection".to_string())?;
        if !response.status().is_success() {
            return Err(daemon_error(response, "connection"));
        }
        response.json().map_err(|_| "connection".to_string())
    }

    pub(crate) fn submit_dictionary_learning(
        &self,
        ticket: &str,
        mode: &str,
        candidates: &[String],
    ) -> Result<LearningResult, String> {
        let mut body = serde_json::json!({ "ticket": ticket, "mode": mode });
        if mode == "user_edit" {
            body["candidates"] = serde_json::to_value(candidates)
                .map_err(|_| "dictionary_learning_failed".to_string())?;
        }
        let mut builder = self
            .injection_client
            .post(format!(
                "{}/internal/dictionary/learning-events",
                self.api_base_url()
            ))
            .header("x-trace-id", uuid::Uuid::new_v4().to_string())
            .json(&body);
        if let Some(bearer) = self.authorization_header() {
            builder = builder.header(reqwest::header::AUTHORIZATION, bearer);
        }
        let response = builder.send().map_err(|_| "connection".to_string())?;
        if !response.status().is_success() {
            return Err(daemon_error(response, "dictionary_learning_failed"));
        }
        response
            .json()
            .map_err(|_| "dictionary_learning_failed".to_string())
    }

    pub(crate) fn undo_dictionary_learning(&self, event_id: &str) -> Result<u32, String> {
        uuid::Uuid::parse_str(event_id).map_err(|_| "dictionary_undo_failed".to_string())?;
        let mut builder = self
            .injection_client
            .post(format!(
                "{}/internal/dictionary/learning-events/{event_id}/undo",
                self.api_base_url()
            ))
            .header("x-trace-id", uuid::Uuid::new_v4().to_string());
        if let Some(bearer) = self.authorization_header() {
            builder = builder.header(reqwest::header::AUTHORIZATION, bearer);
        }
        let response = builder.send().map_err(|_| "connection".to_string())?;
        if !response.status().is_success() {
            return Err(daemon_error(response, "dictionary_undo_failed"));
        }
        #[derive(Deserialize)]
        struct UndoResponse {
            undone_count: u32,
        }
        response
            .json::<UndoResponse>()
            .map(|value| value.undone_count)
            .map_err(|_| "dictionary_undo_failed".to_string())
    }

    /// 直连 root-only 工作台详情；不得经过 WebView 通用 API 白名单。
    pub(crate) fn fetch_workspace_detail(&self, session_id: &str) -> Result<Value, String> {
        let response = self
            .request_client
            .get(format!(
                "{}/sessions/{}/workspace-detail",
                self.api_base_url(),
                session_id
            ))
            .header("x-trace-id", uuid::Uuid::new_v4().to_string())
            .header(
                reqwest::header::AUTHORIZATION,
                self.authorization_header()
                    .ok_or_else(|| "connection".to_string())?,
            )
            .send()
            .map_err(|_| "connection".to_string())?;
        if !response.status().is_success() {
            return Err(daemon_error(response, "workspace_detail_failed"));
        }
        response.json().map_err(|_| "connection".to_string())
    }

    /// 直连低频 root-only cleanup 处理详情；provider 原始响应不经过通用 API 白名单。
    pub(crate) fn fetch_cleanup_detail(&self, session_id: &str) -> Result<Value, String> {
        let response = self
            .request_client
            .get(format!(
                "{}/sessions/{}/cleanup-detail",
                self.api_base_url(),
                session_id
            ))
            .header("x-trace-id", uuid::Uuid::new_v4().to_string())
            .header(
                reqwest::header::AUTHORIZATION,
                self.authorization_header()
                    .ok_or_else(|| "connection".to_string())?,
            )
            .send()
            .map_err(|_| "connection".to_string())?;
        if !response.status().is_success() {
            return Err(daemon_error(response, "cleanup_detail_failed"));
        }
        response.json().map_err(|_| "connection".to_string())
    }

    pub(crate) fn resolve_context_resource(
        &self,
        session_id: &str,
        sequence: u32,
        index: usize,
    ) -> Result<ResolvedResource, String> {
        let response = self
            .request_client
            .get(format!(
                "{}/sessions/{}/context/{}/resources/{}/resolve",
                self.api_base_url(),
                session_id,
                sequence,
                index
            ))
            .header("x-trace-id", uuid::Uuid::new_v4().to_string())
            .header(
                reqwest::header::AUTHORIZATION,
                self.authorization_header()
                    .ok_or_else(|| "connection".to_string())?,
            )
            .send()
            .map_err(|_| "connection".to_string())?;
        if !response.status().is_success() {
            return Err(daemon_error(response, "resource_unavailable"));
        }
        response.json().map_err(|_| "connection".to_string())
    }

    pub(crate) fn resolve_context_link(
        &self,
        session_id: &str,
        sequence: u32,
    ) -> Result<ResolvedLink, String> {
        let response = self
            .request_client
            .get(format!(
                "{}/sessions/{}/context/{}/link",
                self.api_base_url(),
                session_id,
                sequence
            ))
            .header("x-trace-id", uuid::Uuid::new_v4().to_string())
            .header(
                reqwest::header::AUTHORIZATION,
                self.authorization_header()
                    .ok_or_else(|| "connection".to_string())?,
            )
            .send()
            .map_err(|_| "connection".to_string())?;
        if !response.status().is_success() {
            return Err(daemon_error(response, "unsupported_scheme"));
        }
        response.json().map_err(|_| "connection".to_string())
    }

    pub(crate) fn fetch_context_thumbnail(
        &self,
        session_id: &str,
        sequence: u32,
        index: usize,
    ) -> Result<Value, String> {
        let response = self
            .request_client
            .get(format!(
                "{}/sessions/{}/context/{}/resources/{}/thumbnail",
                self.api_base_url(),
                session_id,
                sequence,
                index
            ))
            .header("x-trace-id", uuid::Uuid::new_v4().to_string())
            .header(
                reqwest::header::AUTHORIZATION,
                self.authorization_header()
                    .ok_or_else(|| "connection".to_string())?,
            )
            .send()
            .map_err(|_| "connection".to_string())?;
        if !response.status().is_success() {
            return Err(daemon_error(response, "thumbnail_unavailable"));
        }
        let width = response
            .headers()
            .get("x-thumbnail-width")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u32>().ok())
            .ok_or_else(|| "thumbnail_unavailable".to_string())?;
        let height = response
            .headers()
            .get("x-thumbnail-height")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u32>().ok())
            .ok_or_else(|| "thumbnail_unavailable".to_string())?;
        let bytes = response
            .bytes()
            .map_err(|_| "thumbnail_unavailable".to_string())?;
        if bytes.len() > 512 * 1024 {
            return Err("thumbnail_budget_exceeded".into());
        }
        Ok(
            serde_json::json!({ "mime_type": "image/png", "base64": STANDARD.encode(bytes), "width": width, "height": height }),
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct LearningResult {
    pub(crate) learning_event_id: Option<String>,
    pub(crate) added_terms: Vec<String>,
}

fn validate_provider_config_id(config_id: &str) -> Result<(), String> {
    uuid::Uuid::parse_str(config_id)
        .ok()
        .filter(|value| value.hyphenated().to_string() == config_id)
        .map(|_| ())
        .ok_or_else(|| "invalid_provider_config_id".to_string())
}

pub(crate) fn daemon_error(response: reqwest::blocking::Response, fallback: &str) -> String {
    response
        .json::<Value>()
        .ok()
        .and_then(|body| body.get("error")?.get("code")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| fallback.to_owned())
}

/// 持有 daemon stdin 写端。该写端关闭即让 daemon 收到 EOF 并优雅退出。
pub(crate) struct DaemonSupervisor {
    pub(crate) data_dir: PathBuf,
    pub(crate) daemon_path: PathBuf,
    pub(crate) development_file_keychain: bool,
    pub(crate) child: Mutex<Option<Child>>,
    pub(crate) stdin: Mutex<Option<ChildStdin>>,
    pub(crate) client: Mutex<Option<DaemonClient>>,
    pub(crate) auth_gate: Mutex<()>,
}

impl DaemonSupervisor {
    pub fn new(data_dir: PathBuf, daemon_path: PathBuf, development_file_keychain: bool) -> Self {
        Self {
            data_dir,
            daemon_path,
            development_file_keychain,
            child: Mutex::new(None),
            stdin: Mutex::new(None),
            client: Mutex::new(None),
            auth_gate: Mutex::new(()),
        }
    }

    /// 启动 daemon 并等待 bootstrap + liveness。已健康的实例直接复用，不再尝试第二次 spawn。
    pub fn start(&self) -> Result<DaemonClient, String> {
        if let Some(port) = self.healthy_bootstrap_port() {
            let client = self.client_for(port)?;
            *self.client.lock().expect("daemon client mutex") = Some(client.clone());
            return Ok(client);
        }

        let mut command = Command::new(&self.daemon_path);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .env("SEASNAIL_DATA_DIR", &self.data_dir);
        if self.development_file_keychain {
            command.env(DEV_FILE_KEYCHAIN_ENV, "1");
        }
        let mut child = command
            .spawn()
            .map_err(|err| format!("无法启动守护进程 {}: {err}", self.daemon_path.display()))?;
        let stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("守护进程 stdin 管道不可用".to_string());
            }
        };

        let port = match self.wait_for_healthy(&mut child) {
            Ok(port) => port,
            Err(error) => {
                // `Child` 的 Drop 不会替我们终止尚未退出的进程；启动超时/异常
                // 必须在返回错误前 kill+wait，避免 GUI 启动失败留下 daemon 孤儿。
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        *self.child.lock().expect("daemon child mutex") = Some(child);
        *self.stdin.lock().expect("daemon stdin mutex") = Some(stdin);
        let client = self.client_for(port)?;
        *self.client.lock().expect("daemon client mutex") = Some(client.clone());
        Ok(client)
    }

    /// 关闭 stdin 写端触发 daemon 的父进程死亡处理；不会强杀，给其清理 bootstrap 的机会。
    pub fn shutdown(&self) {
        self.stdin.lock().expect("daemon stdin mutex").take();
        self.client.lock().expect("daemon client mutex").take();
    }

    /// Closes the parent-liveness pipe, waits for the daemon, and only sends
    /// TERM/KILL after strict PID + executable identity validation. The
    /// bounded wait keeps GUI exit responsive without touching an unrelated
    /// process that reused the old PID.
    pub fn shutdown_bounded(&self, timeout: Duration) -> bool {
        self.stdin.lock().expect("daemon stdin mutex").take();
        self.client.lock().expect("daemon client mutex").take();
        let (initial_deadline, deadline) = shutdown_wait_deadlines(Instant::now(), timeout);
        // 将总预算的最后一段保留给 TERM 宽限期。旧实现先等待完整 timeout，
        // 到达 deadline 后才发 TERM，导致 term_deadline 立即过期并直接 KILL。
        let mut child_guard = self.child.lock().expect("daemon child mutex");
        let Some(child) = child_guard.as_mut() else {
            return true;
        };

        if wait_for_child_exit(child, initial_deadline) {
            child_guard.take();
            return true;
        }
        let pid = child.id();
        if !daemon_process_identity_matches(pid, &self.daemon_path) {
            return false;
        }
        if !send_process_signal(pid, libc::SIGTERM) {
            return false;
        }
        let term_deadline = deadline;
        if wait_for_child_exit(child, term_deadline) {
            child_guard.take();
            return true;
        }
        if !daemon_process_identity_matches(pid, &self.daemon_path) {
            return false;
        }
        if !send_process_signal(pid, libc::SIGKILL) {
            return false;
        }
        let exited = wait_for_child_exit(child, deadline);
        if exited {
            child_guard.take();
        }
        exited
    }

    pub(crate) fn healthy_bootstrap_port(&self) -> Option<u16> {
        let info = Bootstrap::new(self.data_dir.clone()).read()?;
        health_probe_sync(info.port).then_some(info.port)
    }

    pub(crate) fn wait_for_healthy(&self, child: &mut Child) -> Result<u16, String> {
        let deadline = Instant::now() + DAEMON_START_TIMEOUT;
        loop {
            if let Some(port) = self.healthy_bootstrap_port() {
                return Ok(port);
            }
            if let Some(status) = child
                .try_wait()
                .map_err(|err| format!("无法检查守护进程状态: {err}"))?
            {
                return Err(format!("守护进程启动失败，退出状态: {status}"));
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "守护进程在 {} 秒内未就绪",
                    DAEMON_START_TIMEOUT.as_secs()
                ));
            }
            thread::sleep(HEALTH_POLL_INTERVAL);
        }
    }

    pub(crate) fn client_for(&self, port: u16) -> Result<DaemonClient, String> {
        Ok(DaemonClient::new(
            port,
            active_root_token(&self.data_dir, self.development_file_keychain)?,
        ))
    }

    /// 解析当前活跃账户 id（图片缓存路径需要）。用缓存的 DaemonClient 做一次阻塞
    /// `GET /accounts`，取 `is_active` 账户的 `id`。失败返回 `None`（仅影响图片事件）。
    pub(crate) fn active_account_id(&self) -> Option<String> {
        let client = self.client.lock().expect("daemon client mutex").clone()?;
        let response = client
            .request(&ApiRequest {
                method: "GET".into(),
                path: "/accounts".into(),
                body: None,
                query: None,
            })
            .ok()?;
        if response.status >= 300 {
            return None;
        }
        let accounts = response.body.as_array()?;
        for account in accounts {
            if account
                .get("is_active")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
            {
                if let Some(id) = account.get("id").and_then(|value| value.as_str()) {
                    return Some(id.to_string());
                }
            }
        }
        None
    }

    /// setup/unlock 在 daemon 中更新 Keychain 后，原生 client 重新读取 bearer。
    /// bearer 始终只保存在 Rust 内存，绝不经 IPC 返回。
    pub(crate) fn refresh_bearer(&self) -> Result<(), String> {
        let mut client = self.client.lock().expect("daemon client mutex");
        let port = client
            .as_ref()
            .ok_or_else(|| "connection".to_string())?
            .port;
        *client = Some(self.client_for(port)?);
        Ok(())
    }
}

/// Preserve the daemon's committed auth result independently from native Keychain
/// availability. A status retry re-synchronizes credentials without another login.
pub(crate) fn synchronize_desktop_auth(
    client: &mut DaemonClient,
    status: &mut Value,
    read_bearer: impl FnOnce() -> Result<Option<String>, String>,
) {
    client.bearer = None;
    let authenticated = status["authenticated"].as_bool().unwrap_or(false);
    if authenticated {
        client.bearer = read_bearer().ok().flatten();
    }
    status["credential_ready"] = Value::Bool(!authenticated || client.has_root_token());
}

#[cfg(test)]
mod desktop_auth_sync_tests {
    use super::*;
    #[test]
    fn committed_login_survives_read_failure_and_status_retry_restores_proxy() {
        let mut client = DaemonClient::new(1, Some("old-token".into()));
        let mut status = serde_json::json!({"authenticated":true});
        synchronize_desktop_auth(&mut client, &mut status, || {
            Err("keychain unavailable".into())
        });
        assert_eq!(status["authenticated"], true);
        assert_eq!(status["credential_ready"], false);
        assert!(!client.has_root_token());
        synchronize_desktop_auth(&mut client, &mut status, || Ok(Some("new-token".into())));
        assert_eq!(status["credential_ready"], true);
        assert_eq!(client.bearer.as_deref(), Some("new-token"));
    }
    #[test]
    fn logout_clears_proxy_without_reading_keychain() {
        let mut client = DaemonClient::new(1, Some("old-token".into()));
        let mut status = serde_json::json!({"authenticated":false});
        synchronize_desktop_auth(&mut client, &mut status, || {
            panic!("logout must not read Keychain")
        });
        assert!(!client.has_root_token());
        assert_eq!(status["credential_ready"], true);
    }
}

pub(crate) fn wait_for_child_exit(child: &mut Child, deadline: Instant) -> bool {
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => return false,
            Err(_) => return false,
        }
    }
}

pub(crate) fn shutdown_wait_deadlines(now: Instant, timeout: Duration) -> (Instant, Instant) {
    (
        now + timeout.saturating_sub(DAEMON_TERM_GRACE),
        now + timeout,
    )
}

pub(crate) fn daemon_process_identity_matches(pid: u32, daemon_path: &Path) -> bool {
    let Ok(output) = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let command = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let expected = daemon_path.to_string_lossy();
    command == expected.as_ref()
        || command
            .strip_prefix(expected.as_ref())
            .is_some_and(|suffix| suffix.chars().next().is_some_and(char::is_whitespace))
}

pub(crate) fn send_process_signal(pid: u32, signal: i32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as libc::pid_t, signal) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, signal);
        false
    }
}

impl Drop for DaemonSupervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// GUI 与 daemon 使用相同的 app 私有 Keychain 条目；root bearer 只进入 Rust 内存。
pub(crate) fn active_root_token(
    data_dir: &Path,
    development_file_keychain: bool,
) -> Result<Option<String>, String> {
    let keychain = app_keychain(development_file_keychain);
    let crypto = Crypto::new(data_dir.to_path_buf(), keychain, Argon2Params::default())
        .map_err(|err| format!("无法读取账户状态: {err}"))?;
    crypto
        .active_root_token_bearer()
        .map_err(|err| format!("无法从 Keychain 读取 root token: {err}"))
}

pub(crate) fn app_keychain(development_file_keychain: bool) -> Arc<dyn KeychainStore> {
    #[cfg(target_os = "macos")]
    {
        if development_file_keychain {
            return Arc::new(seasnail_crypto::MacKeychain::with_protected(false));
        }
        Arc::new(seasnail_crypto::MacKeychain::new())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Arc::new(seasnail_crypto::MemoryKeychain::new())
    }
}

/// 同步 liveness probe，避免 GUI 初始化阶段依赖 Tokio runtime。
pub(crate) fn health_probe_sync(port: u16) -> bool {
    let deadline = Instant::now() + DAEMON_LIVENESS_TIMEOUT;
    let Ok(mut stream) = TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}")
            .parse()
            .expect("loopback socket addr"),
        DAEMON_LIVENESS_TIMEOUT,
    ) else {
        return false;
    };
    let remaining = || deadline.saturating_duration_since(Instant::now());
    let timeout = remaining();
    if timeout.is_zero() {
        return false;
    }
    let _ = stream.set_write_timeout(Some(timeout));
    if stream
        .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let timeout = remaining();
    if timeout.is_zero() {
        return false;
    }
    let _ = stream.set_read_timeout(Some(timeout));
    let mut response = [0u8; 32];
    matches!(stream.read(&mut response), Ok(n) if n > 0 && response.starts_with(b"HTTP/1.1 200"))
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct RecordingSubmission {
    pub(crate) id: String,
    pub(crate) status: String,
}

pub(crate) fn captured_to_submission_input(
    captured: CapturedRecording,
) -> (CapturedAudio, Option<ContextPayload>) {
    (
        CapturedAudio {
            pcm: captured.pcm,
            sample_rate: captured.sample_rate,
            input_device: captured.input_device,
        },
        effective_clipboard_context(captured.clipboard_manifest.as_ref())
            .map(|manifest| ContextPayload::new(manifest.encode_to_vec())),
    )
}

/// 空事件 manifest 不作为 context 提交：避免无复制时仍写 context.pb.enc + 置 context_present。
pub(crate) fn effective_clipboard_context(
    manifest: Option<&ClipboardContextFile>,
) -> Option<&ClipboardContextFile> {
    manifest.filter(|m| !m.events.is_empty())
}

#[cfg(test)]
mod status_response_tests {
    use super::*;

    #[test]
    fn missing_failure_reason_deserializes_as_none_for_old_daemons() {
        let mapped = map_session_status_response(serde_json::json!({"status": "failed"}))
            .expect("failed without reason stays a valid terminal");
        assert_eq!(mapped, SessionStatus::Failed(None));
    }

    #[test]
    fn failure_reason_is_carried_additively() {
        let mapped = map_session_status_response(
            serde_json::json!({"status": "failed", "failure_reason": "no_speech"}),
        )
        .expect("failed with reason stays a valid terminal");
        assert_eq!(mapped, SessionStatus::Failed(Some("no_speech".into())));

        // 其余状态不消费 failure_reason，也不因多余字段失败。
        let mapped = map_session_status_response(
            serde_json::json!({"status": "completed", "failure_reason": null}),
        )
        .expect("completed stays valid");
        assert_eq!(mapped, SessionStatus::Completed);
    }

    #[test]
    fn malformed_and_unknown_status_keep_stable_error_codes() {
        assert_eq!(
            map_session_status_response(serde_json::json!({"status": 42})),
            Err("recording_status_failed".to_string())
        );
        assert_eq!(
            map_session_status_response(
                serde_json::json!({"status": "failed", "failure_reason": 42})
            ),
            Err("recording_status_failed".to_string())
        );
        assert_eq!(
            map_session_status_response(serde_json::json!({"status": "mystery"})),
            Err("recording_status_invalid".to_string())
        );
    }
}

#[cfg(test)]
mod moved_tests {
    use super::*;
    use seasnail_proto::seasnail::v1::ClipboardContextFile;
    #[test]
    fn empty_clipboard_manifest_is_filtered_out_of_submission() {
        use seasnail_proto::seasnail::v1::{ContextEvent, ContextEventKind};
        // 启用但无复制 → 空 manifest；不得提交（避免 context_present=true 与空 context.pb.enc）。
        let empty = ClipboardContextFile {
            schema_version: 1,
            session_id: String::new(),
            capture_id: uuid::Uuid::new_v4().to_string(),
            events: Vec::new(),
        };
        assert!(
            effective_clipboard_context(Some(&empty)).is_none(),
            "0-event manifest must not be submitted as context"
        );
        let with_event = ClipboardContextFile {
            schema_version: 1,
            session_id: String::new(),
            capture_id: uuid::Uuid::new_v4().to_string(),
            events: vec![ContextEvent {
                sequence: 1,
                source_sample_rate: 48_000,
                sample_offset: 0,
                kind: ContextEventKind::ContextEventPlainText as i32,
                plain_text: "x".into(),
                html_fragment: String::new(),
                absolute_paths: Vec::new(),
            }],
        };
        assert!(effective_clipboard_context(Some(&with_event)).is_some());
        assert!(effective_clipboard_context(None).is_none());
    }
}

#[cfg(test)]
mod owner_tests {
    use super::*;

    #[test]
    fn daemon_client_never_serializes_bearer() {
        let client = DaemonClient::new(43123, Some("ss_live_secret".into()));
        assert_eq!(client.api_base_url(), "http://127.0.0.1:43123/api/v1");
        assert_eq!(
            client.authorization_header().as_deref(),
            Some("Bearer ss_live_secret")
        );
    }

    #[test]
    fn absent_listener_is_unhealthy() {
        assert!(!health_probe_sync(1));
    }

    #[test]
    fn provider_credential_transport_only_accepts_canonical_uuid() {
        let canonical = "fae74816-6731-4cd9-81e5-f5392a4b317c";
        assert!(validate_provider_config_id(canonical).is_ok());
        assert!(validate_provider_config_id(&canonical.to_uppercase()).is_err());
        assert!(validate_provider_config_id("config-1").is_err());
        assert!(validate_provider_config_id("../credential").is_err());
    }

    #[test]
    fn bounded_shutdown_reserves_term_grace_inside_total_timeout() {
        let now = Instant::now();
        let (initial, deadline) = shutdown_wait_deadlines(now, Duration::from_secs(5));
        assert_eq!(deadline.duration_since(initial), DAEMON_TERM_GRACE);

        let (initial, deadline) = shutdown_wait_deadlines(now, Duration::from_millis(200));
        assert_eq!(initial, now);
        assert_eq!(deadline.duration_since(initial), Duration::from_millis(200));
    }
}
