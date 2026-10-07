//! SeaSnail Tauri GUI 壳（M6.1）。
//!
//! WebView 只承载界面；守护进程由本模块以 stdin 管道绑定生命周期，bearer token
//! 只在 Rust 侧从 Keychain 读取并封装在 [`DaemonClient`] 中。后续 M6.1a 在此基础
//! 上增加受限 IPC OpenAPI transport，M7 再替换当前占位静态页面为 React 前端。

mod clipboard_collector;
mod clipboard_media_cache;
mod clipboard_time_anchor;
mod platform;
#[cfg(test)]
mod realtime_pipeline_tests;

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use clipboard_media_cache::MediaCache;
use platform::daemon::{
    captured_to_submission_input, health_probe_sync, DaemonClient, DaemonSupervisor,
};
use platform::injection::TextInjector;
use platform::observation::ObservationCoordinator;
use platform::permission::{
    microphone_permission, open_accessibility_privacy_settings, permissions_status,
    request_microphone_access, MicrophonePermission, PermissionsStatus,
};
use platform::recording::{
    CapturedRecording, CpalErrorConsumer, InputDevice, RecordingController, RecordingStart,
    RecordingStatus,
};
use platform::resource::{
    open_validated_resource, open_with_default_application, validate_http_link,
};
#[cfg(test)]
use platform::resource::{validate_resolved_resource_identity, ResolvedResource};
use seasnail_daemon::Bootstrap;
use seasnail_desktop_core::{
    run_after_stop, stable_failure_code, terminal_display_duration, CoordinatorError,
    CoordinatorRecordingPort as RecordingPort, PresentationSink,
    RealtimeCoordinator as RealtimeTaskCoordinator, RealtimeTaskFallback, RealtimeTaskPhase,
    RealtimeTaskSnapshot, RecordingStartInfo,
};
use seasnail_desktop_core::{DeliveryOutcome, DeliveryPermit, TextDeliveryPort, ThreadPollSleeper};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    Emitter, Manager, RunEvent,
};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

const DEFAULT_RECORDING_SHORTCUT: &str = "Command+Shift+Space";
const CPAL_ERROR_POLL_INTERVAL: Duration = Duration::from_millis(100);
const DEV_FILE_KEYCHAIN_MARKER: &str = "SEASNAIL_DEV_FILE_KEYCHAIN";
const REALTIME_TASK_STATUS_EVENT: &str = "realtime-task-status";
const RECORDING_CAPSULE_LABEL: &str = "recording-capsule";
const LEARNING_CAPSULE_LABEL: &str = "learning-capsule";
const MAIN_WINDOW_LABEL: &str = "main";
const TRAY_ICON_ID: &str = "seasnail-status";
const TRAY_OPEN_MAIN_ID: &str = "open-main";
const TRAY_TOGGLE_RECORDING_ID: &str = "toggle-recording";
const TRAY_OPEN_SETTINGS_ID: &str = "open-settings";
const TRAY_QUIT_ID: &str = "quit";
const RECORDING_CAPSULE_BOTTOM_MARGIN: i32 = 28;
// The 360×104 notice canvas reserves 4 logical pixels below its 40px pill.
// Anchor the pill, rather than the canvas, at the usual bottom margin.
const RECORDING_CAPSULE_CARD_HEIGHT: f64 = 104.0;
const RECORDING_CAPSULE_CARD_BOTTOM_INSET: f64 = 4.0;

#[derive(Default)]
struct RealtimeTaskStatusState(Mutex<RealtimeTaskSnapshot>);

/// 全局快捷键和状态栏事件都来自 UI/系统回调线程。录音设备探测与 CPAL stream
/// 创建可能阻塞数百毫秒，必须移到后台；该门闩保持多个入口的用户操作顺序。
#[derive(Default)]
struct RecordingToggleGate(Mutex<()>);

#[derive(Clone, Debug, Serialize)]
struct LearningNotification {
    generation: u64,
    added_terms: Vec<String>,
}

struct LearningNotificationRecord {
    notification: LearningNotification,
    event_id: String,
}

#[derive(Default)]
struct LearningNotificationState {
    generation: std::sync::atomic::AtomicU64,
    current: Mutex<Option<LearningNotificationRecord>>,
}

impl LearningNotificationState {
    fn publish(&self, event_id: String, added_terms: Vec<String>) -> LearningNotification {
        let notification = LearningNotification {
            generation: self.generation.fetch_add(1, Ordering::SeqCst) + 1,
            added_terms,
        };
        *self.current.lock().expect("learning notification mutex") =
            Some(LearningNotificationRecord {
                notification: notification.clone(),
                event_id,
            });
        notification
    }

    fn current(&self) -> Option<LearningNotification> {
        self.current
            .lock()
            .expect("learning notification mutex")
            .as_ref()
            .map(|record| record.notification.clone())
    }

    fn event_id(&self, generation: u64) -> Option<String> {
        self.current
            .lock()
            .expect("learning notification mutex")
            .as_ref()
            .filter(|record| record.notification.generation == generation)
            .map(|record| record.event_id.clone())
    }

    fn clear(&self) -> Option<u64> {
        self.current
            .lock()
            .expect("learning notification mutex")
            .take()
            .map(|record| record.notification.generation)
    }

    fn clear_if_generation(&self, generation: u64) -> bool {
        let mut current = self.current.lock().expect("learning notification mutex");
        if current
            .as_ref()
            .map(|record| record.notification.generation)
            == Some(generation)
        {
            *current = None;
            true
        } else {
            false
        }
    }
}

impl RealtimeTaskStatusState {
    fn snapshot(&self) -> RealtimeTaskSnapshot {
        self.0.lock().expect("realtime task status mutex").clone()
    }

    fn replace(&self, snapshot: RealtimeTaskSnapshot) {
        *self.0.lock().expect("realtime task status mutex") = snapshot;
    }

    /// Serialize capsule geometry changes with snapshot publication and reject
    /// resize callbacks measured against an older task state.
    fn with_resize_gate<R>(
        &self,
        expected: Option<(u64, u64)>,
        operation: impl FnOnce() -> Result<R, String>,
    ) -> Result<Option<R>, String> {
        let snapshot = self.0.lock().expect("realtime task status mutex");
        if expected.is_some_and(|(generation, revision)| {
            snapshot.generation.value() != generation || snapshot.revision != revision
        }) {
            return Ok(None);
        }
        operation().map(Some)
    }
}

struct TauriPresentationSink {
    app: tauri::AppHandle,
}

impl PresentationSink for TauriPresentationSink {
    fn publish(&self, snapshot: RealtimeTaskSnapshot) {
        self.app
            .state::<RealtimeTaskStatusState>()
            .replace(snapshot.clone());
        let preparing = snapshot.phase == RealtimeTaskPhase::Preparing;
        let _ = self.app.emit(REALTIME_TASK_STATUS_EVENT, snapshot.clone());
        if preparing {
            recording_capsule_log(&self.app, "preparing published");
        } else if snapshot.phase == RealtimeTaskPhase::Recording {
            recording_capsule_log(&self.app, "audio ready");
        }
    }
}

#[tauri::command]
fn realtime_task_status(state: tauri::State<'_, RealtimeTaskStatusState>) -> RealtimeTaskSnapshot {
    state.snapshot()
}
// Normal pill width is fixed by the renderer at 260 logical pixels; native bounds
// continue to validate all resize requests, including the 360x104 failure card.
const RECORDING_CAPSULE_FAILURE_WIDTH: u32 = 360;
const RECORDING_CAPSULE_FAILURE_HEIGHT: u32 = 104;
const DAEMON_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const DAEMON_TERM_GRACE: Duration = Duration::from_secs(1);

fn capsule_smoke_enabled() -> bool {
    option_env!("SEASNAIL_CAPSULE_SMOKE") == Some("1")
}

/// 胶囊问题只发生在 GUI 进程，daemon 日志无法观察原生窗口的层级与 Space 属性。
/// 记录的仅是窗口尺寸、位置、可见性和平台适配器返回的诊断信息，不含录音或剪贴板内容。
fn recording_capsule_log(app: &tauri::AppHandle, message: impl AsRef<str>) {
    let path = app
        .state::<Arc<DaemonSupervisor>>()
        .data_dir
        .join("logs/gui.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut file) => {
            let _ = writeln!(
                file,
                "{} recording-capsule pid={} {}",
                chrono::Utc::now().to_rfc3339(),
                std::process::id(),
                message.as_ref()
            );
        }
        Err(error) => eprintln!("无法写入录制胶囊诊断日志 {}: {error}", path.display()),
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct HotkeyConfig {
    recording_shortcut: String,
    #[serde(default = "default_clipboard_context_enabled")]
    clipboard_context_enabled: bool,
    #[serde(default = "default_auto_paste_enabled")]
    auto_paste_enabled: bool,
    #[serde(default = "default_keep_transcription_in_clipboard")]
    keep_transcription_in_clipboard: bool,
    #[serde(default = "default_dictionary_auto_learn_enabled")]
    dictionary_auto_learn_enabled: bool,
    #[serde(default)]
    locale: Option<String>,
}

const fn default_clipboard_context_enabled() -> bool {
    true
}

const fn default_auto_paste_enabled() -> bool {
    true
}

const fn default_keep_transcription_in_clipboard() -> bool {
    true
}

const fn default_dictionary_auto_learn_enabled() -> bool {
    true
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            recording_shortcut: DEFAULT_RECORDING_SHORTCUT.into(),
            clipboard_context_enabled: default_clipboard_context_enabled(),
            auto_paste_enabled: default_auto_paste_enabled(),
            keep_transcription_in_clipboard: default_keep_transcription_in_clipboard(),
            dictionary_auto_learn_enabled: default_dictionary_auto_learn_enabled(),
            locale: None,
        }
    }
}

#[derive(Serialize)]
struct HotkeyStatus {
    recording_shortcut: String,
    registered: bool,
    registration_error: Option<String>,
}

#[derive(Serialize)]
struct ClipboardContextStatus {
    enabled: bool,
}

#[derive(Serialize)]
struct RecordingBehaviorStatus {
    auto_paste_enabled: bool,
    keep_transcription_in_clipboard: bool,
}

struct HotkeyInner {
    config: HotkeyConfig,
    registered: bool,
    registration_error: Option<String>,
    capturing: bool,
}

/// 全局录制快捷键的原生状态。设置保存在应用数据目录，WebView 不直接接触插件能力。
pub struct HotkeyState {
    config_path: PathBuf,
    inner: Mutex<HotkeyInner>,
}

impl HotkeyState {
    fn new(data_dir: &Path) -> Self {
        let config_path = data_dir.join("gui-preferences.json");
        let config = read_hotkey_config(&config_path);
        Self {
            config_path,
            inner: Mutex::new(HotkeyInner {
                config,
                registered: false,
                registration_error: None,
                capturing: false,
            }),
        }
    }

    fn status(&self) -> HotkeyStatus {
        let inner = self.inner.lock().expect("hotkey mutex");
        HotkeyStatus {
            recording_shortcut: inner.config.recording_shortcut.clone(),
            registered: inner.registered,
            registration_error: inner.registration_error.clone(),
        }
    }

    fn clipboard_context_status(&self) -> ClipboardContextStatus {
        let inner = self.inner.lock().expect("hotkey mutex");
        ClipboardContextStatus {
            enabled: inner.config.clipboard_context_enabled,
        }
    }

    fn clipboard_context_enabled(&self) -> bool {
        self.inner
            .lock()
            .expect("hotkey mutex")
            .config
            .clipboard_context_enabled
    }

    fn recording_behavior_status(&self) -> RecordingBehaviorStatus {
        let inner = self.inner.lock().expect("hotkey mutex");
        RecordingBehaviorStatus {
            auto_paste_enabled: inner.config.auto_paste_enabled,
            keep_transcription_in_clipboard: inner.config.keep_transcription_in_clipboard,
        }
    }

    fn locale_preference(&self) -> Option<String> {
        self.inner
            .lock()
            .expect("hotkey mutex")
            .config
            .locale
            .clone()
    }

    fn dictionary_auto_learn_enabled(&self) -> bool {
        self.inner
            .lock()
            .expect("hotkey mutex")
            .config
            .dictionary_auto_learn_enabled
    }
}

fn read_hotkey_config(path: &Path) -> HotkeyConfig {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return HotkeyConfig::default();
    };
    let Ok(config) = serde_json::from_str::<HotkeyConfig>(&contents) else {
        return HotkeyConfig::default();
    };
    Shortcut::from_str(&config.recording_shortcut)
        .map(|_| config)
        .unwrap_or_default()
}

fn write_hotkey_config(path: &Path, config: &HotkeyConfig) -> Result<(), String> {
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| "快捷键配置路径无效".to_string())?,
    )
    .map_err(|err| format!("无法创建快捷键配置目录: {err}"))?;
    let bytes =
        serde_json::to_vec_pretty(config).map_err(|err| format!("无法序列化快捷键配置: {err}"))?;
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&temporary, bytes).map_err(|err| format!("无法写入快捷键配置: {err}"))?;
    std::fs::rename(&temporary, path).map_err(|err| format!("无法保存快捷键配置: {err}"))
}

/// Coordinator 的生产录音适配层：只负责把平台采集配置映射到纯任务端口，
/// 不在这里提交、轮询或注入，避免恢复第二个编排 owner。
struct ProductionRecordingPort {
    controller: Arc<RecordingController>,
    hotkey: Arc<HotkeyState>,
    supervisor: Arc<DaemonSupervisor>,
}

type ProductionCoordinator =
    RealtimeTaskCoordinator<ProductionRecordingPort, TauriPresentationSink>;

fn coordinator_error_code(error: &CoordinatorError) -> String {
    match error {
        CoordinatorError::Busy => "recording_submission_in_progress".into(),
        CoordinatorError::Cancelled => "recording_cancelled".into(),
        CoordinatorError::InvalidState(_) => "recording_invalid_state".into(),
        // 快照与 recording-error 事件共用同一套稳定码归一化，禁止动态原生错误
        // 从事件通道泄露到界面。
        CoordinatorError::Port(code) => stable_failure_code(code),
    }
}

struct ProductionInjectionPort {
    client: DaemonClient,
    injector: Arc<TextInjector>,
    keep_transcription_in_clipboard: bool,
    observation: Arc<ObservationCoordinator>,
    app: tauri::AppHandle,
}

impl TextDeliveryPort for ProductionInjectionPort {
    fn deliver(&self, session_id: &str, permit: DeliveryPermit) -> DeliveryOutcome {
        let _ = permit;
        let plan = match self.client.fetch_injection_plan(session_id) {
            Ok(plan) => plan,
            Err(_) => {
                return DeliveryOutcome::Failed {
                    code: "recording_auto_paste_failed".into(),
                    clipboard_written: false,
                }
            }
        };
        let outcome =
            self.injector
                .inject_plan(&plan, self.keep_transcription_in_clipboard, |_| {});
        match outcome {
            platform::injection::InjectionOutcome::Pasted { .. } => {
                // Read the live preference at delivery time. A transcription may spend seconds in
                // the daemon after this port is constructed; a copied boolean would allow a newly
                // disabled learner to start another observation afterwards.
                let app = self.app.clone();
                self.observation
                    .start(self.client.clone(), plan, move |result| {
                        show_learning_notification(&app, result);
                    });
                DeliveryOutcome::Pasted
            }
            platform::injection::InjectionOutcome::ClipboardOnly { .. } => {
                DeliveryOutcome::ClipboardOnly
            }
            platform::injection::InjectionOutcome::Failed {
                code,
                clipboard_written,
                ..
            } => DeliveryOutcome::Failed {
                code,
                clipboard_written,
            },
        }
    }
}

impl RecordingPort for ProductionRecordingPort {
    type Captured = CapturedRecording;

    fn start(&self) -> Result<RecordingStartInfo, String> {
        let context_enabled = self.hotkey.clipboard_context_enabled();
        let (account_id, media_cache) = if context_enabled {
            (
                self.supervisor.active_account_id(),
                Some(MediaCache::new(&self.supervisor.data_dir)),
            )
        } else {
            (None, None)
        };
        self.controller
            .start(None, context_enabled, account_id, media_cache)
            .map(|started| RecordingStartInfo {
                input_device: started.input_device,
                sample_rate: started.sample_rate,
            })
    }

    fn stop(&self) -> Result<Self::Captured, String> {
        self.controller.stop()
    }

    fn context_count(captured: &Self::Captured) -> usize {
        captured
            .clipboard_manifest
            .as_ref()
            .map(|manifest| manifest.events.len())
            .unwrap_or(0)
    }
}

fn schedule_production_terminal_hide(
    app: &tauri::AppHandle,
    coordinator: &Arc<ProductionCoordinator>,
    task_id: String,
    duration: Duration,
) {
    let app = app.clone();
    coordinator.schedule_terminal_action(task_id, duration, move || {
        if let Some(window) = app.get_webview_window(RECORDING_CAPSULE_LABEL) {
            let _ = window.hide();
        }
    });
}

fn spawn_production_pipeline(
    app: &tauri::AppHandle,
    coordinator: Arc<ProductionCoordinator>,
    supervisor: Arc<DaemonSupervisor>,
    injector: Arc<TextInjector>,
    captured: CapturedRecording,
) {
    let app = app.clone();
    let worker_coordinator = Arc::clone(&coordinator);
    let result = coordinator.spawn_worker(move |cancelled| {
        let submit_token = worker_coordinator
            .worker_token(RealtimeTaskPhase::Submitting)
            .ok();
        let Some(client) = supervisor
            .client
            .lock()
            .expect("daemon client mutex")
            .clone()
        else {
            if let Some(token) = submit_token.as_ref() {
                let _ =
                    worker_coordinator.fail(token, "connection".into(), RealtimeTaskFallback::None);
                report_capsule_result(show_recording_failure_capsule(&app));
                let snapshot = worker_coordinator.snapshot();
                let duration = terminal_display_duration(&snapshot);
                schedule_production_terminal_hide(
                    &app,
                    &worker_coordinator,
                    token.task_id.clone(),
                    duration,
                );
            }
            let _ = app.emit("recording-error", "connection");
            return;
        };
        let keep_transcription_in_clipboard = app
            .state::<Arc<HotkeyState>>()
            .recording_behavior_status()
            .keep_transcription_in_clipboard;
        let injection = ProductionInjectionPort {
            client: client.clone(),
            injector,
            keep_transcription_in_clipboard,
            observation: app.state::<Arc<ObservationCoordinator>>().inner().clone(),
            app: app.clone(),
        };
        let result = run_after_stop(
            &worker_coordinator,
            captured,
            captured_to_submission_input,
            Arc::new(client),
            &ThreadPollSleeper,
            &injection,
            &cancelled,
        );
        let snapshot = worker_coordinator.snapshot();
        let failed = snapshot.phase == RealtimeTaskPhase::Failed || result.is_err();
        if failed {
            report_capsule_result(show_recording_failure_capsule(&app));
        }
        let duration = terminal_display_duration(&snapshot);
        if let Some(task_id) = snapshot.task_id {
            if snapshot.phase != RealtimeTaskPhase::Completed && !failed {
                return;
            }
            schedule_production_terminal_hide(&app, &worker_coordinator, task_id, duration);
        }
    });
    if let Err(error) = result {
        if let Ok(token) = coordinator.worker_token(RealtimeTaskPhase::Submitting) {
            let _ = coordinator.fail(
                &token,
                "recording_submit_duplicate".into(),
                RealtimeTaskFallback::None,
            );
        }
        eprintln!("无法启动实时任务 worker: {error:?}");
    }
}

/// 供打包 `.app` 的 TCC 上机验收使用；正常启动不会进入这些分支。
fn run_tcc_probe() -> Option<Result<(), String>> {
    let argument = std::env::args().nth(1)?;
    match argument.as_str() {
        "--tcc-status" => Some(
            serde_json::to_string_pretty(&permissions_status())
                .map(|status| println!("{status}"))
                .map_err(|err| format!("无法输出权限状态: {err}")),
        ),
        "--request-microphone-permission" => Some(
            request_microphone_access()
                .and_then(|status| {
                    serde_json::to_string_pretty(&status)
                        .map_err(|err| format!("无法输出麦克风权限状态: {err}"))
                })
                .map(|status| println!("{status}")),
        ),
        "--open-accessibility-settings" => Some(open_accessibility_privacy_settings()),
        _ => None,
    }
}

/// ST-M4.4：内部注入计划（原生侧 JSON 镜像 daemon `InjectionPlanDto`）。
/// 自定义 Debug 省略粘贴文本与 learning_ticket，避免调试输出泄漏用户内容或票据。
#[derive(Clone, Deserialize)]
struct InjectionPlan {
    plain: String,
    html: String,
    learning_ticket: String,
}

impl std::fmt::Debug for InjectionPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InjectionPlan")
            .field("plain", &format_args!("<{} bytes>", self.plain.len()))
            .field("html", &format_args!("<{} bytes>", self.html.len()))
            .field("learning_ticket", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct ApiRequest {
    method: String,
    path: String,
    body: Option<Value>,
    query: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Serialize)]
struct ApiResponse {
    status: u16,
    body: Value,
}

/// 只允许 OpenAPI 已定义的相对 API 路径；避免 WebView 借代理访问任意 loopback 服务。
fn validate_api_request(request: &ApiRequest) -> Result<(), String> {
    if request.path.contains("..")
        || !request.path.starts_with('/')
        || request.path.starts_with("//")
    {
        return Err("非法 API 路径".into());
    }
    let path = request.path.split('?').next().unwrap_or_default();
    let allowed = match request.method.as_str() {
        "GET" => {
            matches!(
                path,
                "/auth/status"
                    | "/accounts"
                    | "/sessions"
                    | "/sessions/search"
                    | "/tokens"
                    | "/models"
                    | "/reasoning/providers"
                    | "/reasoning/provider-configs"
                    | "/cleanup/settings"
                    | "/dictionary"
            ) || one_path_parameter(path, "/sessions/", "")
                || one_path_parameter(path, "/sessions/", "/audio")
        }
        "POST" => {
            matches!(
                path,
                "/auth/setup"
                    | "/auth/password"
                    | "/accounts"
                    | "/export"
                    | "/tokens"
                    | "/reasoning/provider-configs"
                    | "/cleanup/test"
                    | "/dictionary/entries"
                    | "/dictionary/imports/preview"
                    | "/dictionary/imports"
            ) || one_path_parameter(path, "/accounts/", "/unlock")
                || one_path_parameter(path, "/sessions/", "/retry")
                || one_path_parameter(path, "/models/", "")
                || one_path_parameter(path, "/models/", "/download")
                || one_path_parameter(path, "/reasoning/provider-configs/", "/probe")
        }
        "PUT" => {
            path == "/cleanup/settings"
                || one_path_parameter(path, "/models/", "")
                || one_path_parameter(path, "/reasoning/provider-configs/", "")
                || one_path_parameter(path, "/dictionary/entries/", "")
        }
        "DELETE" => {
            path == "/dictionary/entries"
                || one_path_parameter(path, "/accounts/", "")
                || one_path_parameter(path, "/sessions/", "")
                || one_path_parameter(path, "/tokens/", "")
                || one_path_parameter(path, "/reasoning/provider-configs/", "")
                || one_path_parameter(path, "/dictionary/entries/", "")
        }
        _ => false,
    };
    allowed
        .then_some(())
        .ok_or_else(|| "该 OpenAPI 操作不允许经 GUI 代理".into())
}

fn one_path_parameter(path: &str, prefix: &str, suffix: &str) -> bool {
    let Some(parameter) = path
        .strip_prefix(prefix)
        .and_then(|path| path.strip_suffix(suffix))
    else {
        return false;
    };
    !parameter.is_empty() && !parameter.contains('/')
}

const GUI_SHUTDOWN_RUNNING: u8 = 0;
const GUI_SHUTDOWN_IN_PROGRESS: u8 = 1;
const GUI_SHUTDOWN_COMPLETE: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExitRequestDisposition {
    StartShutdown,
    WaitForShutdown,
    AllowExit,
}

#[derive(Default)]
struct GuiShutdownState(AtomicU8);

impl GuiShutdownState {
    fn request_exit(&self) -> ExitRequestDisposition {
        match self.0.compare_exchange(
            GUI_SHUTDOWN_RUNNING,
            GUI_SHUTDOWN_IN_PROGRESS,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => ExitRequestDisposition::StartShutdown,
            Err(GUI_SHUTDOWN_COMPLETE) => ExitRequestDisposition::AllowExit,
            Err(_) => ExitRequestDisposition::WaitForShutdown,
        }
    }

    fn mark_complete(&self) {
        self.0.store(GUI_SHUTDOWN_COMPLETE, Ordering::Release);
    }
}

fn shutdown_gui_resources(app: &tauri::AppHandle) {
    app.state::<Arc<ObservationCoordinator>>().cancel();
    let coordinator = app.state::<Arc<ProductionCoordinator>>().inner().clone();
    let _ = coordinator.shutdown(DAEMON_TERM_GRACE);
    let controller = app.state::<Arc<RecordingController>>().inner().clone();
    let _ = controller.stop();
    app.state::<Arc<CpalErrorConsumer>>().shutdown();
    let supervisor = app.state::<Arc<DaemonSupervisor>>().inner().clone();
    if !supervisor.shutdown_bounded(DAEMON_SHUTDOWN_TIMEOUT) {
        eprintln!("守护进程未能在有界退出流程内确认回收");
    }
}

fn show_learning_notification(app: &tauri::AppHandle, result: platform::daemon::LearningResult) {
    let Some(event_id) = result.learning_event_id else {
        return;
    };
    if result.added_terms.is_empty() {
        return;
    }
    let notification = app
        .state::<LearningNotificationState>()
        .publish(event_id, result.added_terms);
    let _ = app.emit("dictionary-learning-completed", &notification);
    if let Some(window) = app.get_webview_window(LEARNING_CAPSULE_LABEL) {
        let _ = position_recording_capsule(app, &window, 320.0, 88.0);
        let _ = window.show();
        let _ = window.set_always_on_top(true);
        #[cfg(target_os = "macos")]
        let _ = bring_recording_capsule_to_front(&window);
    }
    let app = app.clone();
    let generation = notification.generation;
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(15));
        if app
            .state::<LearningNotificationState>()
            .clear_if_generation(generation)
        {
            if let Some(window) = app.get_webview_window(LEARNING_CAPSULE_LABEL) {
                let _ = window.hide();
            }
            let _ = app.emit("dictionary-learning-dismissed", generation);
        }
    });
}

fn clear_learning_notification(app: &tauri::AppHandle) {
    let generation = app.state::<LearningNotificationState>().clear();
    if let Some(window) = app.get_webview_window(LEARNING_CAPSULE_LABEL) {
        let _ = window.hide();
    }
    if let Some(generation) = generation {
        let _ = app.emit("dictionary-learning-dismissed", generation);
    }
}

#[tauri::command]
fn learning_notification_status(
    state: tauri::State<'_, LearningNotificationState>,
) -> Option<LearningNotification> {
    state.current()
}

#[tauri::command]
fn undo_latest_dictionary_learning(
    app: tauri::AppHandle,
    state: tauri::State<'_, LearningNotificationState>,
    supervisor: tauri::State<'_, Arc<DaemonSupervisor>>,
    generation: u64,
) -> Result<u32, String> {
    let event_id = state
        .event_id(generation)
        .ok_or_else(|| "dictionary_notification_expired".to_string())?;
    let client = supervisor
        .client
        .lock()
        .expect("daemon client mutex")
        .clone()
        .ok_or_else(|| "connection".to_string())?;
    let undone = client.undo_dictionary_learning(&event_id)?;
    if state.clear_if_generation(generation) {
        if let Some(window) = app.get_webview_window(LEARNING_CAPSULE_LABEL) {
            let _ = window.hide();
        }
        let _ = app.emit("dictionary-learning-undone", generation);
    }
    Ok(undone)
}

fn handle_exit_requested(app: &tauri::AppHandle, api: tauri::ExitRequestApi) {
    match app.state::<GuiShutdownState>().request_exit() {
        ExitRequestDisposition::AllowExit => {}
        ExitRequestDisposition::WaitForShutdown => api.prevent_exit(),
        ExitRequestDisposition::StartShutdown => {
            api.prevent_exit();
            let app = app.clone();
            thread::spawn(move || {
                shutdown_gui_resources(&app);
                app.state::<GuiShutdownState>().mark_complete();
                // AppHandle::exit 会再次触发 ExitRequested。此时状态已是 COMPLETE，
                // handler 不再 prevent_exit，Tauri 才能真正进入 RunEvent::Exit。
                app.exit(0);
            });
        }
    }
}

#[derive(Serialize)]
struct DaemonStatus {
    connected: bool,
    authenticated: bool,
}

/// 安全的壳状态：只告知连接/认证状态，不泄露端口或 bearer。
#[tauri::command]
fn daemon_status(supervisor: tauri::State<'_, Arc<DaemonSupervisor>>) -> DaemonStatus {
    let client = supervisor
        .client
        .lock()
        .expect("daemon client mutex")
        .clone();
    DaemonStatus {
        connected: client
            .as_ref()
            .is_some_and(|client| health_probe_sync(client.port)),
        authenticated: client.as_ref().is_some_and(DaemonClient::has_root_token),
    }
}

/// Desktop auth is main-window-only; capability and root bearer never cross IPC.
#[tauri::command]
async fn desktop_auth(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    supervisor: tauri::State<'_, Arc<DaemonSupervisor>>,
    action: String,
    body: Option<Value>,
) -> Result<Value, String> {
    if window.label() != MAIN_WINDOW_LABEL {
        return Err("forbidden".into());
    }
    let supervisor = supervisor.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Serialize auth transitions against shortcut recording startup.
        let toggle = app.state::<RecordingToggleGate>();
        let _gate = if action == "status" {
            None
        } else {
            Some(
                toggle
                    .0
                    .try_lock()
                    .map_err(|_| "account_busy".to_string())?,
            )
        };
        let _auth_gate = supervisor.auth_gate.lock().expect("desktop auth mutex");
        let phase = app.state::<Arc<ProductionCoordinator>>().snapshot().phase;
        if action != "status"
            && matches!(
                phase,
                RealtimeTaskPhase::Preparing
                    | RealtimeTaskPhase::Recording
                    | RealtimeTaskPhase::Submitting
                    | RealtimeTaskPhase::Transcribing
                    | RealtimeTaskPhase::CleaningUp
                    | RealtimeTaskPhase::AutoPasting
            )
        {
            return Err("account_busy".into());
        }
        let client = supervisor
            .client
            .lock()
            .expect("daemon client mutex")
            .clone()
            .ok_or("connection")?;
        if action != "status" {
            // cancel waits for any learning submission/callback to finish. Clear
            // after cancellation, so a late old-account result cannot republish.
            app.state::<Arc<ObservationCoordinator>>().cancel();
            clear_learning_notification(&app);
        }
        let mut response = client.desktop_auth(&supervisor.data_dir, &action, body)?;
        if response["authenticated"].as_bool() == Some(false) {
            app.state::<Arc<ObservationCoordinator>>().cancel();
            clear_learning_notification(&app);
        }
        let mut cached = supervisor.client.lock().expect("daemon client mutex");
        let cached = cached.as_mut().ok_or("connection")?;
        platform::daemon::synchronize_desktop_auth(cached, &mut response, || {
            platform::daemon::active_root_token(
                &supervisor.data_dir,
                supervisor.development_file_keychain,
            )
        });
        Ok(response)
    })
    .await
    .map_err(|_| "connection".to_string())?
}

/// WebView 的唯一 HTTP 入口：认证、trace ID 与目的地址均由原生层控制。
#[tauri::command]
fn api_request(
    supervisor: tauri::State<'_, Arc<DaemonSupervisor>>,
    request: ApiRequest,
) -> Result<ApiResponse, String> {
    let response = supervisor
        .client
        .lock()
        .expect("daemon client mutex")
        .as_ref()
        .ok_or_else(|| "connection".to_string())?
        .request(&request)?;
    let refresh_bearer = request.method == "POST"
        && (request.path == "/auth/setup"
            || request.path == "/accounts"
            || (request.path.starts_with("/accounts/") && request.path.ends_with("/unlock")));
    if response.status < 300 && refresh_bearer {
        supervisor.refresh_bearer()?;
    }
    Ok(response)
}

#[tauri::command]
fn set_provider_credential(
    supervisor: tauri::State<'_, Arc<DaemonSupervisor>>,
    config_id: String,
    credential: Option<String>,
) -> Result<String, String> {
    let credential = credential
        .map(String::into_bytes)
        .map(seasnail_crypto::CredentialSecret::new)
        .transpose()
        .map_err(|_| "invalid_credential".to_string())?;
    supervisor
        .client
        .lock()
        .expect("daemon client mutex")
        .as_ref()
        .ok_or_else(|| "connection".to_string())?
        .set_provider_credential(&config_id, credential.as_ref())
}

#[tauri::command]
fn delete_provider_credential(
    supervisor: tauri::State<'_, Arc<DaemonSupervisor>>,
    config_id: String,
) -> Result<String, String> {
    supervisor
        .client
        .lock()
        .expect("daemon client mutex")
        .as_ref()
        .ok_or_else(|| "connection".to_string())?
        .delete_provider_credential(&config_id)
}

/// 导出数据直接落到用户在原生对话框中选定的位置，ZIP 不经过 WebView。
#[tauri::command]
async fn export_to_file(
    app: tauri::AppHandle,
    supervisor: tauri::State<'_, Arc<DaemonSupervisor>>,
    request: Value,
) -> Result<bool, String> {
    let client = supervisor
        .client
        .lock()
        .expect("daemon client mutex")
        .as_ref()
        .ok_or_else(|| "connection".to_string())?
        .clone();
    // HTTP、原生阻塞对话框与文件写入都不能占用 UI 线程，也不应持有共享 client 锁。
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = client.export_zip(&request)?;
        let Some(path) = app
            .dialog()
            .file()
            .add_filter("ZIP archive", &["zip"])
            .set_file_name("seasnail-export.zip")
            .blocking_save_file()
        else {
            return Ok(false);
        };
        std::fs::write(
            path.as_path()
                .ok_or_else(|| "当前平台未返回本地保存路径".to_string())?,
            bytes,
        )
        .map_err(|err| format!("写入导出文件失败: {err}"))?;
        Ok(true)
    })
    .await
    .map_err(|_| "export_failed".to_string())?
}

/// Dictionary CSV 使用同一后台执行边界，ZIP/CSV 字节均不经过 WebView。
#[tauri::command]
async fn export_dictionary_to_file(
    app: tauri::AppHandle,
    supervisor: tauri::State<'_, Arc<DaemonSupervisor>>,
) -> Result<bool, String> {
    let client = supervisor
        .client
        .lock()
        .expect("daemon client mutex")
        .as_ref()
        .ok_or_else(|| "connection".to_string())?
        .clone();
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = client.export_dictionary_csv()?;
        let Some(path) = app
            .dialog()
            .file()
            .add_filter("CSV", &["csv"])
            .set_file_name("seasnail-dictionary.csv")
            .blocking_save_file()
        else {
            return Ok(false);
        };
        std::fs::write(
            path.as_path()
                .ok_or_else(|| "当前平台未返回本地保存路径".to_string())?,
            bytes,
        )
        .map_err(|err| format!("写入词典文件失败: {err}"))?;
        Ok(true)
    })
    .await
    .map_err(|_| "dictionary_export_failed".to_string())?
}

#[tauri::command]
async fn get_session_workspace_detail(
    supervisor: tauri::State<'_, Arc<DaemonSupervisor>>,
    session_id: String,
) -> Result<Value, String> {
    let client = supervisor
        .client
        .lock()
        .expect("daemon client mutex")
        .clone()
        .ok_or_else(|| "connection".to_string())?;
    tauri::async_runtime::spawn_blocking(move || client.fetch_workspace_detail(&session_id))
        .await
        .map_err(|_| "connection".to_string())?
}

#[tauri::command]
fn get_session_cleanup_detail(
    supervisor: tauri::State<'_, Arc<DaemonSupervisor>>,
    session_id: String,
) -> Result<Value, String> {
    let client = supervisor
        .client
        .lock()
        .expect("daemon client mutex")
        .clone()
        .ok_or_else(|| "connection".to_string())?;
    client.fetch_cleanup_detail(&session_id)
}

#[tauri::command]
fn open_context_resource(
    supervisor: tauri::State<'_, Arc<DaemonSupervisor>>,
    session_id: String,
    sequence: u32,
    resource_index: usize,
) -> Result<bool, String> {
    let client = supervisor
        .client
        .lock()
        .expect("daemon client mutex")
        .clone()
        .ok_or_else(|| "resource_unavailable".to_string())?;
    let resource = client.resolve_context_resource(&session_id, sequence, resource_index)?;
    let path = PathBuf::from(&resource.path);
    open_validated_resource(&resource, &path, open_with_default_application)
}

#[tauri::command]
fn open_context_link(
    supervisor: tauri::State<'_, Arc<DaemonSupervisor>>,
    session_id: String,
    sequence: u32,
) -> Result<bool, String> {
    let client = supervisor
        .client
        .lock()
        .expect("daemon client mutex")
        .clone()
        .ok_or_else(|| "resource_unavailable".to_string())?;
    let link = client.resolve_context_link(&session_id, sequence)?;
    validate_http_link(&link.url)?;
    open_with_default_application(&link.url, false)
}

#[tauri::command]
async fn get_context_thumbnail(
    supervisor: tauri::State<'_, Arc<DaemonSupervisor>>,
    session_id: String,
    sequence: u32,
    resource_index: usize,
) -> Result<Value, String> {
    let client = supervisor
        .client
        .lock()
        .expect("daemon client mutex")
        .clone()
        .ok_or_else(|| "resource_unavailable".to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        client.fetch_context_thumbnail(&session_id, sequence, resource_index)
    })
    .await
    .map_err(|_| "resource_unavailable".to_string())?
}

#[tauri::command]
fn copy_text(text: String) -> Result<(), String> {
    platform::clipboard::copy_text(text)
}

#[tauri::command]
fn permission_status() -> PermissionsStatus {
    permissions_status()
}

#[tauri::command]
fn request_microphone_permission() -> Result<MicrophonePermission, String> {
    request_microphone_access()
}

#[tauri::command]
fn open_accessibility_settings() -> Result<(), String> {
    open_accessibility_privacy_settings()
}

#[tauri::command]
fn input_devices() -> Result<Vec<InputDevice>, String> {
    RecordingController::input_devices()
}

/// 当前录制快照；持续状态由 `recording-status` 事件限流推送。
#[tauri::command]
fn recording_status(controller: tauri::State<'_, Arc<RecordingController>>) -> RecordingStatus {
    controller.status()
}

fn ensure_microphone_permission() -> Result<(), String> {
    microphone_permission()
        .granted
        .then_some(())
        .ok_or_else(|| "recording_microphone_unauthorized".to_string())
}

/// 胶囊是透明画布上的无边框窗口。每次显示都按鼠标所在的活动屏幕
/// （不可用时回退至胶囊/主屏幕）重新定位到底部居中；没有拖拽区，因此位置固定。
fn show_recording_capsule(app: &tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window(RECORDING_CAPSULE_LABEL)
        .ok_or_else(|| "录制胶囊窗口不可用".to_string())?;
    reinforce_recording_capsule_window(&window)?;
    // Hide the previous task before publishing the new generation. Its prepared
    // React frame will show the window; never crop an old failure card to a pill.
    window.hide().map_err(|error| error.to_string())?;
    recording_capsule_log(app, "previous capsule hidden before preparation");
    Ok(())
}

/// 失败态不能只等待 WebView 的异步尺寸 effect：连续相同错误的 React 状态可能
/// 不变。原生层直接以设计约束尺寸显示，保证每次快捷键失败都能看到完整提示。
fn show_recording_failure_capsule(app: &tauri::AppHandle) -> Result<(), String> {
    let snapshot = app.state::<RealtimeTaskStatusState>().snapshot();
    if snapshot.phase != RealtimeTaskPhase::Failed {
        return Ok(());
    }
    let window = app
        .get_webview_window(RECORDING_CAPSULE_LABEL)
        .ok_or_else(|| "录制胶囊窗口不可用".to_string())?;
    reinforce_recording_capsule_window(&window)?;
    resize_realtime_progress_capsule_window(
        app.clone(),
        RECORDING_CAPSULE_FAILURE_WIDTH,
        RECORDING_CAPSULE_FAILURE_HEIGHT,
        true,
        Some((snapshot.generation.value(), snapshot.revision)),
    )
}

/// 全局快捷键没有“活跃窗口”的跨平台 Tauri API；鼠标所在屏幕是与用户当前操作
/// 最接近且无需 Accessibility 权限的稳定回退策略。内容尺寸变化后也必须重新定位，
/// 否则失败态会从旧左边界向单侧扩张，偏离设计约定的底部居中位置。
///
/// 居中使用传入的逻辑尺寸经目标屏幕 `scale_factor` 换算后的物理尺寸，而非
/// `window.outer_size()`：`set_size` 由窗口服务器异步应用，紧随其后的 `outer_size()`
/// 会返回旧值，导致居中坐标按旧宽度计算、胶囊在尺寸真正落地后偏移，直到下一次
/// 重新定位才纠正——表现为启停阶段的位置跳变。胶囊为无边框窗口，外尺寸即内容尺寸，
/// 请求尺寸即真实尺寸。
fn position_recording_capsule(
    app: &tauri::AppHandle,
    window: &tauri::WebviewWindow,
    logical_width: f64,
    logical_height: f64,
) -> Result<Option<(i32, i32, u32, u32)>, String> {
    let monitor = app
        .cursor_position()
        .ok()
        .and_then(|position| {
            app.monitor_from_point(position.x, position.y)
                .ok()
                .flatten()
        })
        .or(window
            .current_monitor()
            .map_err(|err| format!("无法读取胶囊所在屏幕: {err}"))?)
        .or(window
            .primary_monitor()
            .map_err(|err| format!("无法读取主屏幕: {err}"))?);
    if let Some(monitor) = monitor {
        let work_area = monitor.work_area();
        let scale = monitor.scale_factor();
        let physical_width = (logical_width * scale).round() as u32;
        let physical_height = (logical_height * scale).round() as u32;
        let x =
            work_area.position.x + (work_area.size.width.saturating_sub(physical_width) / 2) as i32;
        let y = recording_capsule_vertical_origin(
            work_area.position.y,
            work_area.size.height,
            physical_height,
            window.label(),
            logical_height,
            scale,
        );
        window
            .set_position(tauri::PhysicalPosition::new(x, y))
            .map_err(|err| format!("无法定位录制胶囊: {err}"))?;
        return Ok(Some((x, y, physical_width, physical_height)));
    }
    Ok(None)
}

fn recording_capsule_bottom_inset(window_label: &str, logical_height: f64, scale: f64) -> i32 {
    if window_label == RECORDING_CAPSULE_LABEL && logical_height == RECORDING_CAPSULE_CARD_HEIGHT {
        (RECORDING_CAPSULE_CARD_BOTTOM_INSET * scale).round() as i32
    } else {
        0
    }
}

fn recording_capsule_vertical_origin(
    work_area_top: i32,
    work_area_height: u32,
    physical_height: u32,
    window_label: &str,
    logical_height: f64,
    scale: f64,
) -> i32 {
    work_area_top + work_area_height.saturating_sub(physical_height) as i32
        - RECORDING_CAPSULE_BOTTOM_MARGIN
        + recording_capsule_bottom_inset(window_label, logical_height, scale)
}

#[tauri::command]
fn resize_realtime_progress_capsule(
    app: tauri::AppHandle,
    width: u32,
    height: u32,
    generation: u64,
    revision: u64,
) -> Result<(), String> {
    let expected = (!capsule_smoke_enabled()).then_some((generation, revision));
    resize_realtime_progress_capsule_window(app, width, height, false, expected)
}

// All callers (renderer, recording worker and failure worker) enter through this
// dispatcher. Never acquire the snapshot/geometry gate before dispatching to UI.
fn resize_realtime_progress_capsule_window(
    app: tauri::AppHandle,
    width: u32,
    height: u32,
    force_front: bool,
    expected_snapshot: Option<(u64, u64)>,
) -> Result<(), String> {
    if !valid_realtime_progress_capsule_size(width, height) {
        return Err(format!("非法胶囊尺寸: {width}x{height}"));
    }
    let handle = app.clone();
    app.run_on_main_thread(move || {
        report_capsule_result(resize_realtime_progress_capsule_on_main_thread(
            handle,
            width,
            height,
            force_front,
            expected_snapshot,
        ))
    })
    .map_err(|error| error.to_string())
}

fn ensure_capsule_ui_thread() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    if objc2::MainThreadMarker::new().is_none() {
        return Err("胶囊窗口操作必须在主线程执行".into());
    }
    Ok(())
}

fn resize_realtime_progress_capsule_on_main_thread(
    app: tauri::AppHandle,
    width: u32,
    height: u32,
    force_front: bool,
    expected_snapshot: Option<(u64, u64)>,
) -> Result<(), String> {
    ensure_capsule_ui_thread()?;
    if !valid_realtime_progress_capsule_size(width, height) {
        return Err(format!("非法胶囊尺寸: {width}x{height}"));
    }
    let window = app
        .get_webview_window(RECORDING_CAPSULE_LABEL)
        .ok_or_else(|| "录制胶囊窗口不可用".to_string())?;
    // Keep snapshot publication from interleaving with this size+position pair.
    // An older renderer request either finishes before the new phase is published,
    // or sees the new revision and is discarded.
    let result =
        app.state::<RealtimeTaskStatusState>()
            .with_resize_gate(expected_snapshot, || {
                window
                    .set_size(tauri::LogicalSize::new(width, height))
                    .map_err(|err| format!("无法调整录制胶囊尺寸: {err}"))?;
                let placement =
                    position_recording_capsule(&app, &window, width as f64, height as f64)?;
                let just_shown = if !window.is_visible().unwrap_or(false) {
                    window
                        .show()
                        .map_err(|err| format!("无法显示录制胶囊: {err}"))?;
                    true
                } else {
                    false
                };
                Ok((placement, just_shown))
            })?;
    let Some((placement, just_shown)) = result else {
        recording_capsule_log(
            &app,
            format!("stale resize ignored logical={width}x{height} expected={expected_snapshot:?}"),
        );
        return Ok(());
    };
    #[cfg(target_os = "macos")]
    if force_front || just_shown {
        bring_recording_capsule_to_front(&window)?;
    }
    recording_capsule_log(
        &app,
        format!(
            "content resize logical={width}x{height} shown={just_shown} \
             forced_front={force_front} placement={placement:?}"
        ),
    );
    Ok(())
}

fn valid_realtime_progress_capsule_size(width: u32, height: u32) -> bool {
    ((120..=360).contains(&width) && height == 40) || (width == 360 && height == 104)
}

/// `show` 只改变窗口的 hidden 状态；在另一应用的原生全屏 Space 中，它可能仍排在
/// 该应用窗口之后。`orderFrontRegardless` 是公开 AppKit API，不会令非 focusable 的
/// 胶囊获取键盘焦点，却会把它放到当前 Space 的前景窗口序列。
#[cfg(target_os = "macos")]
fn bring_recording_capsule_to_front(window: &tauri::WebviewWindow) -> Result<(), String> {
    platform::macos::bring_recording_capsule_to_front(window)
}

/// 在每次展示和窗口重新获得焦点时重申跨工作区策略。
///
/// 这与 OpenWhispr 的窗口管理模式一致：macOS 在切换 Space、全屏应用或应用激活后
/// 可能丢失 `CanJoinAllSpaces`。窗口层级不在此处重设：Tauri 的
/// `set_always_on_top(true)` 会把原生层级降回普通 floating level，覆盖 setup 时为
/// 全屏覆盖层设定的 floating+1。`focus: false` 保持在配置中，以便 show 不主动抢占
/// 用户当前输入焦点。
fn reinforce_recording_capsule_window(window: &tauri::WebviewWindow) -> Result<(), String> {
    window
        .set_visible_on_all_workspaces(true)
        .map_err(|err| format!("无法让录制胶囊跨工作区显示: {err}"))
}

/// Tauri 的通用“跨工作区”只设置 `CanJoinAllSpaces`，不能进入其他应用的原生全屏
/// Space。该标志是公开的 AppKit API；作为辅助窗口进入全屏 Space，不依赖 Tauri 的
/// macOS 私有 API 功能。`FullScreenPrimary`、`FullScreenAuxiliary` 与
/// `FullScreenNone` 三者互斥；`Primary`、`Auxiliary` 与 `CanJoinAllApplications` 也
/// 互斥。后者是 macOS 13+ 为浮窗/系统覆盖层提供的语义：允许窗口加入**其他应用**的
/// 全屏 Space；仅设置 FullScreenAuxiliary 只说明它可以同全屏窗口共处，不足以请求加入
/// 所有应用。NSPanel 验证包使用 screen-saver level；这是 Apple DTS 对其他应用全屏
/// 覆盖窗口给出的层级要求，普通 Tauri always-on-top 的 floating 层级不足。
#[cfg(target_os = "macos")]
fn configure_recording_capsule_fullscreen_behavior(
    app: &tauri::AppHandle,
    window: &tauri::WebviewWindow,
) -> Result<(), String> {
    let result = platform::macos::configure_recording_capsule_fullscreen_behavior(window);
    if result.is_ok() {
        recording_capsule_log(app, "configured macOS fullscreen behavior");
    }
    result
}

fn report_capsule_result(result: Result<(), String>) {
    if let Err(error) = result {
        eprintln!("{error}");
    }
}

#[derive(Debug, Eq, PartialEq)]
enum ProductionRecordingAction {
    Start,
    Stop,
    IgnoreBusy,
}

/// 将快捷键和状态栏两个入口收敛为同一份纯决策，确保一个入口事件最多派发一个
/// Coordinator 操作；进行中的提交/转译阶段不会旁路创建第二个任务。
fn production_recording_action(
    phase: RealtimeTaskPhase,
    controller_recording: bool,
) -> ProductionRecordingAction {
    if phase == RealtimeTaskPhase::Recording {
        ProductionRecordingAction::Stop
    } else if controller_recording
        || matches!(
            phase,
            RealtimeTaskPhase::Preparing
                | RealtimeTaskPhase::Submitting
                | RealtimeTaskPhase::Transcribing
                | RealtimeTaskPhase::CleaningUp
                | RealtimeTaskPhase::AutoPasting
        )
    {
        ProductionRecordingAction::IgnoreBusy
    } else {
        ProductionRecordingAction::Start
    }
}

fn toggle_recording_with_coordinator(app: &tauri::AppHandle) {
    let coordinator = app.state::<Arc<ProductionCoordinator>>().inner().clone();
    let controller = app.state::<Arc<RecordingController>>();
    let snapshot = coordinator.snapshot();
    match production_recording_action(snapshot.phase, controller.is_recording()) {
        ProductionRecordingAction::Stop => match coordinator.stop() {
            Ok(captured) => spawn_production_pipeline(
                app,
                coordinator,
                app.state::<Arc<DaemonSupervisor>>().inner().clone(),
                app.state::<Arc<TextInjector>>().inner().clone(),
                captured,
            ),
            Err(error) => {
                report_capsule_result(show_recording_failure_capsule(app));
                let snapshot = coordinator.snapshot();
                let duration = terminal_display_duration(&snapshot);
                if let Some(task_id) = snapshot.task_id {
                    schedule_production_terminal_hide(app, &coordinator, task_id, duration);
                }
                let _ = app.emit("recording-error", coordinator_error_code(&error));
            }
        },
        ProductionRecordingAction::IgnoreBusy => {}
        ProductionRecordingAction::Start => {
            if !app
                .state::<Arc<DaemonSupervisor>>()
                .client
                .lock()
                .expect("daemon client mutex")
                .as_ref()
                .is_some_and(DaemonClient::has_root_token)
            {
                let _ = show_main_window(app);
                return;
            }
            app.state::<Arc<ObservationCoordinator>>().capture_target();
            // Hide any previous terminal card before publishing Preparing.
            report_capsule_result(show_recording_capsule(app));
            // Preparing is published before audio initialization; its rendered frame
            // shows the capsule at the stable normal width, without exposing stale content.
            let hotkey = app.state::<Arc<HotkeyState>>();
            match coordinator.start(hotkey.recording_behavior_status().auto_paste_enabled) {
                Ok(started) => {
                    let _ = app.emit(
                        "recording-started",
                        RecordingStart {
                            input_device: started.input_device,
                            sample_rate: started.sample_rate,
                        },
                    );
                }
                Err(error) => {
                    report_capsule_result(show_recording_failure_capsule(app));
                    let snapshot = coordinator.snapshot();
                    let duration = terminal_display_duration(&snapshot);
                    if let Some(task_id) = snapshot.task_id {
                        schedule_production_terminal_hide(app, &coordinator, task_id, duration);
                    }
                    let _ = app.emit("recording-error", coordinator_error_code(&error));
                }
            }
        }
    }
}

/// global-hotkey 在 macOS Carbon C 回调内同步调用 handler。任何 Rust panic 若逃出
/// 该边界都会触发 panic_cannot_unwind/SIGABRT，因此必须在最外层截断展开。
fn toggle_recording_guarded(app: &tauri::AppHandle) {
    let completed = recording_action_did_not_panic(|| {
        toggle_recording_with_coordinator(app);
    });
    if !completed {
        eprintln!("录音快捷操作发生内部 panic，已阻止其越过系统回调边界");
        let _ = app.emit("recording-error", "recording_failed");
    }
}

fn recording_action_did_not_panic(action: impl FnOnce()) -> bool {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(action)).is_ok()
}

fn dispatch_recording_toggle(app: &tauri::AppHandle) {
    recording_capsule_log(app, "toggle received");
    let app = app.clone();
    thread::spawn(move || {
        let gate = app.state::<RecordingToggleGate>();
        // panic 已由 toggle_recording_guarded 截断；若历史 panic 曾污染门闩，仍允许
        // 后续快捷键恢复使用，而不是在系统回调线程再次 panic。
        let _guard = match gate.0.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            // Do not queue a second toggle during device initialization: it would
            // stop the new recording immediately after the startup lock is released.
            Err(std::sync::TryLockError::WouldBlock) => return,
        };
        toggle_recording_guarded(&app);
    });
}

/// Accessory 应用没有 Dock 图标，状态栏菜单是重新打开工作台的常驻入口。
/// 仅用户明确点选该菜单时才聚焦主窗口；录制胶囊本身仍保持 non-activating。
fn show_main_window(app: &tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window(MAIN_WINDOW_LABEL)
        .ok_or_else(|| "无法找到 SeaSnail 主窗口".to_string())?;
    window
        .show()
        .map_err(|err| format!("无法显示 SeaSnail 主窗口: {err}"))?;
    window
        .set_focus()
        .map_err(|err| format!("无法聚焦 SeaSnail 主窗口: {err}"))
}

fn install_status_bar_menu(app: &tauri::App) -> Result<(), String> {
    let open_main = MenuItem::with_id(app, TRAY_OPEN_MAIN_ID, "打开 SeaSnail", true, None::<&str>)
        .map_err(|err| format!("无法创建状态栏“打开”菜单项: {err}"))?;
    let toggle_recording = MenuItem::with_id(
        app,
        TRAY_TOGGLE_RECORDING_ID,
        if capsule_smoke_enabled() {
            "胶囊冒烟模式（录制已停用）"
        } else {
            "切换实时转录"
        },
        !capsule_smoke_enabled(),
        None::<&str>,
    )
    .map_err(|err| format!("无法创建状态栏录制菜单项: {err}"))?;
    let open_settings =
        MenuItem::with_id(app, TRAY_OPEN_SETTINGS_ID, "打开设置", true, None::<&str>)
            .map_err(|err| format!("无法创建状态栏设置菜单项: {err}"))?;
    let separator =
        PredefinedMenuItem::separator(app).map_err(|err| format!("无法创建状态栏分隔线: {err}"))?;
    let quit = MenuItem::with_id(app, TRAY_QUIT_ID, "退出 SeaSnail", true, None::<&str>)
        .map_err(|err| format!("无法创建状态栏退出菜单项: {err}"))?;
    let menu = Menu::with_items(
        app,
        &[
            &open_main,
            &toggle_recording,
            &open_settings,
            &separator,
            &quit,
        ],
    )
    .map_err(|err| format!("无法创建 SeaSnail 状态栏菜单: {err}"))?;

    let tray_icon =
        tauri::image::Image::from_bytes(include_bytes!("../icons/seasnail-trayTemplate.png"))
            .map_err(|err| format!("无法加载 SeaSnail 状态栏图标: {err}"))?;
    let tray = TrayIconBuilder::with_id(TRAY_ICON_ID)
        .icon(tray_icon)
        // PNG 仅用 Alpha 表达透明背景上的 SeaSnail 线稿；交由 macOS Template
        // 模式根据深浅色菜单栏自动着色，不绘制方形底板。
        .icon_as_template(true)
        .tooltip("SeaSnail 本地转写")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            TRAY_OPEN_MAIN_ID => {
                if let Err(err) = show_main_window(app) {
                    eprintln!("状态栏打开主窗口失败: {err}");
                }
            }
            TRAY_TOGGLE_RECORDING_ID if !capsule_smoke_enabled() => dispatch_recording_toggle(app),
            TRAY_OPEN_SETTINGS_ID => {
                if let Err(err) = show_main_window(app) {
                    eprintln!("状态栏打开设置失败: {err}");
                    return;
                }
                let _ = app.emit("open-settings", ());
            }
            TRAY_QUIT_ID => app.exit(0),
            _ => {}
        })
        .build(app)
        .map_err(|err| format!("无法创建 SeaSnail 状态栏入口: {err}"))?;
    // TrayIcon 不由 Tauri 自动持有；放入 state 以保持到进程退出。
    app.manage(tray);
    Ok(())
}

/// 供设置页读取当前快捷键和占用错误。注册失败不会阻止应用启动。
#[tauri::command]
fn recording_shortcut(state: tauri::State<'_, Arc<HotkeyState>>) -> HotkeyStatus {
    state.status()
}

fn set_shortcut_capture(
    app: &tauri::AppHandle,
    state: &HotkeyState,
    capturing: bool,
) -> Result<(), String> {
    let mut inner = state.inner.lock().expect("hotkey mutex");
    if capturing {
        if inner.registered {
            app.global_shortcut()
                .unregister(inner.config.recording_shortcut.as_str())
                .map_err(|_| "shortcut_register_failed".to_string())?;
            inner.registered = false;
        }
        inner.capturing = true;
    } else {
        inner.capturing = false;
        if !inner.registered {
            let result = app
                .global_shortcut()
                .register(inner.config.recording_shortcut.as_str());
            inner.registered = result.is_ok();
            inner.registration_error = result.err().map(|_| "shortcut_register_failed".to_string());
            if !inner.registered {
                return Err("shortcut_register_failed".into());
            }
        }
    }
    Ok(())
}

#[tauri::command]
fn set_recording_shortcut_capture(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Arc<HotkeyState>>,
    capturing: bool,
) -> Result<(), String> {
    if window.label() != MAIN_WINDOW_LABEL || (capturing && !window.is_focused().unwrap_or(false)) {
        return Err("shortcut_invalid".into());
    }
    set_shortcut_capture(&app, &state, capturing)
}

/// 修改快捷键时先注册新组合；只有成功后才移除旧组合并持久化，避免用户失去可用快捷键。
#[tauri::command]
fn set_recording_shortcut(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<HotkeyState>>,
    shortcut: String,
) -> Result<HotkeyStatus, String> {
    let shortcut = shortcut.trim().to_owned();
    if shortcut.is_empty() || shortcut.len() > 128 {
        return Err("shortcut_invalid".to_string());
    }
    Shortcut::from_str(&shortcut).map_err(|_| "shortcut_invalid".to_string())?;

    let mut inner = state.inner.lock().expect("hotkey mutex");
    let previous = inner.config.recording_shortcut.clone();
    if shortcut == previous {
        return Ok(HotkeyStatus {
            recording_shortcut: previous,
            registered: inner.registered,
            registration_error: inner.registration_error.clone(),
        });
    }

    app.global_shortcut()
        .register(shortcut.as_str())
        .map_err(|_| "shortcut_register_failed".to_string())?;

    if inner.registered {
        if let Err(_) = app.global_shortcut().unregister(previous.as_str()) {
            let _ = app.global_shortcut().unregister(shortcut.as_str());
            return Err("shortcut_register_failed".to_string());
        }
    }

    let updated = HotkeyConfig {
        recording_shortcut: shortcut.clone(),
        clipboard_context_enabled: inner.config.clipboard_context_enabled,
        auto_paste_enabled: inner.config.auto_paste_enabled,
        keep_transcription_in_clipboard: inner.config.keep_transcription_in_clipboard,
        dictionary_auto_learn_enabled: inner.config.dictionary_auto_learn_enabled,
        locale: inner.config.locale.clone(),
    };
    if write_hotkey_config(&state.config_path, &updated).is_err() {
        let _ = app.global_shortcut().unregister(shortcut.as_str());
        if inner.registered {
            let _ = app.global_shortcut().register(previous.as_str());
        }
        return Err("shortcut_register_failed".to_string());
    }

    inner.config = updated;
    inner.registered = true;
    inner.registration_error = None;
    Ok(HotkeyStatus {
        recording_shortcut: shortcut,
        registered: true,
        registration_error: None,
    })
}

/// 剪贴板上下文采集拥有独立的持久化开关。M3 collector 启用后会在录制开始时读取该值，
/// 关闭时完全不访问 Pasteboard。
#[tauri::command]
fn clipboard_context_status(state: tauri::State<'_, Arc<HotkeyState>>) -> ClipboardContextStatus {
    state.clipboard_context_status()
}

#[tauri::command]
fn set_clipboard_context_enabled(
    state: tauri::State<'_, Arc<HotkeyState>>,
    enabled: bool,
) -> Result<ClipboardContextStatus, String> {
    let mut inner = state.inner.lock().expect("hotkey mutex");
    let updated = HotkeyConfig {
        recording_shortcut: inner.config.recording_shortcut.clone(),
        clipboard_context_enabled: enabled,
        auto_paste_enabled: inner.config.auto_paste_enabled,
        keep_transcription_in_clipboard: inner.config.keep_transcription_in_clipboard,
        dictionary_auto_learn_enabled: inner.config.dictionary_auto_learn_enabled,
        locale: inner.config.locale.clone(),
    };
    write_hotkey_config(&state.config_path, &updated)?;
    inner.config = updated;
    Ok(ClipboardContextStatus { enabled })
}

#[tauri::command]
fn recording_behavior_status(state: tauri::State<'_, Arc<HotkeyState>>) -> RecordingBehaviorStatus {
    state.recording_behavior_status()
}

#[tauri::command]
fn set_auto_paste_enabled(
    state: tauri::State<'_, Arc<HotkeyState>>,
    enabled: bool,
) -> Result<RecordingBehaviorStatus, String> {
    let mut inner = state.inner.lock().expect("hotkey mutex");
    let previous = inner.config.auto_paste_enabled;
    inner.config.auto_paste_enabled = enabled;
    if let Err(error) = write_hotkey_config(&state.config_path, &inner.config) {
        inner.config.auto_paste_enabled = previous;
        return Err(error);
    }
    Ok(RecordingBehaviorStatus {
        auto_paste_enabled: enabled,
        keep_transcription_in_clipboard: inner.config.keep_transcription_in_clipboard,
    })
}

#[tauri::command]
fn set_keep_transcription_in_clipboard(
    state: tauri::State<'_, Arc<HotkeyState>>,
    enabled: bool,
) -> Result<RecordingBehaviorStatus, String> {
    let mut inner = state.inner.lock().expect("hotkey mutex");
    let previous = inner.config.keep_transcription_in_clipboard;
    inner.config.keep_transcription_in_clipboard = enabled;
    if let Err(error) = write_hotkey_config(&state.config_path, &inner.config) {
        inner.config.keep_transcription_in_clipboard = previous;
        return Err(error);
    }
    Ok(RecordingBehaviorStatus {
        auto_paste_enabled: inner.config.auto_paste_enabled,
        keep_transcription_in_clipboard: enabled,
    })
}

#[tauri::command]
fn get_locale_preference(state: tauri::State<'_, Arc<HotkeyState>>) -> Option<String> {
    state.locale_preference()
}

#[tauri::command]
fn get_dictionary_auto_learn_enabled(state: tauri::State<'_, Arc<HotkeyState>>) -> bool {
    state.dictionary_auto_learn_enabled()
}

#[tauri::command]
fn set_dictionary_auto_learn_enabled(
    state: tauri::State<'_, Arc<HotkeyState>>,
    observation: tauri::State<'_, Arc<ObservationCoordinator>>,
    enabled: bool,
) -> Result<bool, String> {
    let mut inner = state.inner.lock().expect("hotkey mutex");
    let previous = inner.config.dictionary_auto_learn_enabled;
    if !enabled {
        // Disable the native gate before persisting the visible preference. Final learning POSTs
        // and this transition share the same gate, so neither can pass the other halfway.
        observation.set_enabled(false);
    }
    inner.config.dictionary_auto_learn_enabled = enabled;
    if let Err(error) = write_hotkey_config(&state.config_path, &inner.config) {
        inner.config.dictionary_auto_learn_enabled = previous;
        if !enabled {
            observation.set_enabled(previous);
        }
        return Err(error);
    }
    drop(inner);
    if enabled {
        observation.set_enabled(true);
    }
    Ok(enabled)
}

#[tauri::command]
fn set_locale_preference(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<HotkeyState>>,
    locale: String,
) -> Result<String, String> {
    validate_locale(&locale)?;
    let mut inner = state.inner.lock().expect("hotkey mutex");
    let previous = inner.config.locale.clone();
    inner.config.locale = Some(locale.clone());
    if let Err(error) = write_hotkey_config(&state.config_path, &inner.config) {
        inner.config.locale = previous;
        return Err(error);
    }
    let _ = app.emit("locale-changed", locale.clone());
    Ok(locale)
}

fn validate_locale(locale: &str) -> Result<(), String> {
    if locale == "zh-CN" || locale == "en-US" {
        Ok(())
    } else {
        Err("invalid_locale".into())
    }
}

fn register_configured_shortcut(app: &tauri::AppHandle, state: &HotkeyState) {
    let mut inner = state.inner.lock().expect("hotkey mutex");
    match app
        .global_shortcut()
        .register(inner.config.recording_shortcut.as_str())
    {
        Ok(()) => {
            inner.registered = true;
            inner.registration_error = None;
        }
        Err(err) => {
            inner.registered = false;
            inner.registration_error = Some(format!(
                "无法注册快捷键 {}: {err}",
                inner.config.recording_shortcut
            ));
        }
    }
}

fn default_daemon_path() -> Result<PathBuf, String> {
    let current =
        std::env::current_exe().map_err(|err| format!("无法定位 GUI 可执行文件: {err}"))?;
    let dir = current
        .parent()
        .ok_or_else(|| "GUI 可执行文件没有父目录".to_string())?;
    let executable = if cfg!(windows) {
        "seasnail-daemon.exe"
    } else {
        "seasnail-daemon"
    };
    Ok(dir.join(executable))
}

/// 开发打包脚本在 Resources 放入此标记。只接受 App bundle 内的固定资源路径，
/// 不读取 WebView 或用户可控配置，避免发行版被意外降级为文件 Keychain。
fn uses_development_file_keychain() -> bool {
    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    let Some(macos_dir) = executable.parent() else {
        return false;
    };
    let Some(contents_dir) = macos_dir.parent() else {
        return false;
    };
    contents_dir
        .join("Resources")
        .join(DEV_FILE_KEYCHAIN_MARKER)
        .is_file()
}

/// 同一个用户会话只允许一个 GUI 实例持有全局快捷键和悬浮胶囊。
/// 使用 flock 而不是仅依赖锁文件存在，进程异常退出时内核会自动释放锁。
fn acquire_single_instance_lock() -> Option<File> {
    let path = std::env::temp_dir().join(gui_lock_filename(capsule_smoke_enabled()));
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)
        .ok()?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            return None;
        }
    }
    Some(file)
}

fn gui_lock_filename(capsule_smoke: bool) -> &'static str {
    if capsule_smoke {
        "seasnail-capsule-smoke-gui-single-instance.lock"
    } else {
        "seasnail-gui-single-instance.lock"
    }
}

fn gui_data_dir(capsule_smoke: bool) -> std::io::Result<PathBuf> {
    if capsule_smoke {
        // Never read the production bootstrap, preferences, logs or account data.
        // A unique root prevents stale fixture state from being reused even if
        // the OS eventually recycles a PID.
        Ok(std::env::temp_dir().join(format!("seasnail-capsule-smoke-{}", uuid::Uuid::new_v4())))
    } else {
        Bootstrap::default_dir()
    }
}

pub fn run() {
    if let Some(result) = run_tcc_probe() {
        if let Err(error) = result {
            eprintln!("TCC 验收命令失败: {error}");
            std::process::exit(1);
        }
        return;
    }
    let Some(_instance_lock) = acquire_single_instance_lock() else {
        eprintln!("SeaSnail 已有实例运行，忽略本次启动");
        return;
    };
    let data_dir = gui_data_dir(capsule_smoke_enabled()).expect("无法确定 SeaSnail 数据目录");
    let daemon_path = default_daemon_path().expect("无法定位 seasnail-daemon");
    let development_file_keychain = uses_development_file_keychain();
    if development_file_keychain {
        eprintln!("SeaSnail 开发包：使用普通文件 Keychain，不可用于分发");
    }
    let supervisor = Arc::new(DaemonSupervisor::new(
        data_dir,
        daemon_path,
        development_file_keychain,
    ));
    supervisor.start().expect("无法启动 SeaSnail 守护进程");
    let hotkey = Arc::new(HotkeyState::new(&supervisor.data_dir));
    let controller = Arc::new(RecordingController::new());
    let injector = Arc::new(TextInjector::new());
    let observation = Arc::new(
        ObservationCoordinator::new(hotkey.dictionary_auto_learn_enabled())
            .with_log_path(supervisor.data_dir.join("logs/gui.log")),
    );
    let error_consumer = Arc::new(CpalErrorConsumer::default());

    let builder = tauri::Builder::default().plugin(tauri_plugin_dialog::init());
    #[cfg(target_os = "macos")]
    let builder = builder.plugin(tauri_nspanel::init());
    builder
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if !capsule_smoke_enabled()
                        && event.state == ShortcutState::Pressed
                        && !app
                            .state::<Arc<HotkeyState>>()
                            .inner
                            .lock()
                            .expect("hotkey mutex")
                            .capturing
                    {
                        dispatch_recording_toggle(app);
                    }
                })
                .build(),
        )
        .manage(Arc::clone(&supervisor))
        .manage(Arc::clone(&hotkey))
        .manage(Arc::clone(&controller))
        .manage(Arc::clone(&injector))
        .manage(Arc::clone(&observation))
        .manage(Arc::clone(&error_consumer))
        .manage(GuiShutdownState::default())
        .manage(RealtimeTaskStatusState::default())
        .manage(RecordingToggleGate::default())
        .manage(LearningNotificationState::default())
        .setup(move |app| {
            let recording_port = Arc::new(ProductionRecordingPort {
                controller: Arc::clone(&controller),
                hotkey: Arc::clone(&hotkey),
                supervisor: Arc::clone(&supervisor),
            });
            let event_sink = Arc::new(TauriPresentationSink {
                app: app.handle().clone(),
            });
            app.manage(Arc::new(RealtimeTaskCoordinator::new(
                recording_port,
                event_sink,
            )));
            if !capsule_smoke_enabled() {
                let error_controller = Arc::clone(&controller);
                let error_coordinator = app.state::<Arc<ProductionCoordinator>>().inner().clone();
                let error_app = app.handle().clone();
                let error_consumer_for_thread = Arc::clone(&error_consumer);
                let handle = thread::spawn(move || {
                    while !error_consumer_for_thread.stop_requested() {
                        for error in error_controller.drain_async_errors() {
                            let Ok(token) =
                                error_coordinator.worker_token(RealtimeTaskPhase::Recording)
                            else {
                                continue;
                            };
                            if token.generation.value() != error.generation {
                                continue;
                            }
                            if error_controller.cleanup_after_async_error(error.generation) {
                                if error_coordinator
                                    .recording_error(&token, error.code)
                                    .is_ok()
                                {
                                    report_capsule_result(show_recording_failure_capsule(
                                        &error_app,
                                    ));
                                    let snapshot = error_coordinator.snapshot();
                                    let duration = terminal_display_duration(&snapshot);
                                    schedule_production_terminal_hide(
                                        &error_app,
                                        &error_coordinator,
                                        token.task_id.clone(),
                                        duration,
                                    );
                                }
                            }
                        }
                        thread::sleep(CPAL_ERROR_POLL_INTERVAL);
                    }
                });
                error_consumer.install(handle);
            }
            #[cfg(target_os = "macos")]
            app.handle()
                .set_activation_policy(tauri::ActivationPolicy::Regular)
                .map_err(|err| format!("无法启用 SeaSnail 的 Regular 激活策略: {err}"))?;
            install_status_bar_menu(app)?;
            if !capsule_smoke_enabled() {
                let hotkey = app.state::<Arc<HotkeyState>>();
                register_configured_shortcut(&app.handle(), &hotkey);
            }
            app.state::<Arc<RecordingController>>()
                .attach_app(app.handle().clone());
            if let Some(main) = app.get_webview_window(MAIN_WINDOW_LABEL) {
                let handle = app.handle().clone();
                main.on_window_event(move |event| {
                    if matches!(
                        event,
                        tauri::WindowEvent::Focused(false)
                            | tauri::WindowEvent::Destroyed
                            | tauri::WindowEvent::CloseRequested { .. }
                    ) {
                        let state = handle.state::<Arc<HotkeyState>>();
                        if let Err(error) = set_shortcut_capture(&handle, &state, false) {
                            eprintln!("恢复录音快捷键失败: {error}");
                        }
                    }
                });
            }
            if let Some(capsule) = app.get_webview_window(RECORDING_CAPSULE_LABEL) {
                #[cfg(target_os = "macos")]
                {
                    platform::macos::configure_recording_capsule_window(&capsule)?;
                    recording_capsule_log(&app.handle(), "nspanel configured");
                    configure_recording_capsule_fullscreen_behavior(&app.handle(), &capsule)?;
                }
                // 用户点击胶囊时，macOS 仍可能临时激活该窗口；立即重申窗口层级和
                // 跨工作区策略，避免它在切换应用或 Space 后落到前台应用之下。
                let app_handle = app.handle().clone();
                let capsule_for_events = capsule.clone();
                capsule.clone().on_window_event(move |event| match event {
                    tauri::WindowEvent::Resized(size) => recording_capsule_log(
                        &app_handle,
                        format!("resized physical={}x{}", size.width, size.height),
                    ),
                    tauri::WindowEvent::Focused(true) => {
                        report_capsule_result(reinforce_recording_capsule_window(
                            &capsule_for_events,
                        ));
                    }
                    _ => {}
                });
            }
            if let Some(capsule) = app.get_webview_window(LEARNING_CAPSULE_LABEL) {
                #[cfg(target_os = "macos")]
                {
                    platform::macos::configure_recording_capsule_window(&capsule)?;
                    configure_recording_capsule_fullscreen_behavior(&app.handle(), &capsule)?;
                }
            }
            if capsule_smoke_enabled() {
                recording_capsule_log(
                    &app.handle(),
                    "isolated fixture smoke enabled; production recording entries disabled",
                );
                report_capsule_result(show_recording_capsule(&app.handle()));
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            daemon_status,
            api_request,
            desktop_auth,
            set_provider_credential,
            delete_provider_credential,
            export_to_file,
            export_dictionary_to_file,
            get_session_workspace_detail,
            get_session_cleanup_detail,
            open_context_resource,
            open_context_link,
            get_context_thumbnail,
            copy_text,
            permission_status,
            request_microphone_permission,
            open_accessibility_settings,
            input_devices,
            recording_status,
            recording_shortcut,
            set_recording_shortcut,
            set_recording_shortcut_capture,
            clipboard_context_status,
            set_clipboard_context_enabled,
            recording_behavior_status,
            set_auto_paste_enabled,
            set_keep_transcription_in_clipboard,
            resize_realtime_progress_capsule,
            realtime_task_status,
            get_locale_preference,
            set_locale_preference,
            get_dictionary_auto_learn_enabled,
            set_dictionary_auto_learn_enabled,
            learning_notification_status,
            undo_latest_dictionary_learning
        ])
        .build(tauri::generate_context!())
        .expect("构建 Tauri 应用失败")
        .run(|app, event| match event {
            RunEvent::ExitRequested { api, .. } => handle_exit_requested(app, api),
            RunEvent::Exit => {
                app.state::<Arc<CpalErrorConsumer>>().shutdown();
            }
            _ => {}
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn capsule_window_operations_reject_worker_thread_before_taking_state_lock() {
        assert!(std::thread::spawn(ensure_capsule_ui_thread)
            .join()
            .expect("worker")
            .is_err());
    }

    #[test]
    fn capsule_smoke_uses_distinct_lock_and_process_data_root() {
        assert_ne!(gui_lock_filename(true), gui_lock_filename(false));
        let smoke_dir = gui_data_dir(true).expect("smoke data dir");
        assert_eq!(smoke_dir.parent(), Some(std::env::temp_dir().as_path()));
        assert!(smoke_dir
            .to_string_lossy()
            .contains("seasnail-capsule-smoke-"));
        assert_ne!(
            smoke_dir,
            gui_data_dir(true).expect("second smoke data dir")
        );
        assert_ne!(smoke_dir, gui_data_dir(false).expect("production data dir"));
    }

    #[test]
    fn gui_shutdown_prevents_exit_until_background_cleanup_completes() {
        let state = GuiShutdownState::default();

        assert_eq!(state.request_exit(), ExitRequestDisposition::StartShutdown);
        assert_eq!(
            state.request_exit(),
            ExitRequestDisposition::WaitForShutdown
        );

        state.mark_complete();
        assert_eq!(state.request_exit(), ExitRequestDisposition::AllowExit);
        assert_eq!(state.request_exit(), ExitRequestDisposition::AllowExit);
    }

    #[test]
    fn production_entries_collapse_to_one_action_per_snapshot() {
        assert_eq!(
            production_recording_action(RealtimeTaskPhase::Idle, false),
            ProductionRecordingAction::Start
        );
        assert_eq!(
            production_recording_action(RealtimeTaskPhase::Recording, true),
            ProductionRecordingAction::Stop
        );
        for phase in [
            RealtimeTaskPhase::Preparing,
            RealtimeTaskPhase::Submitting,
            RealtimeTaskPhase::Transcribing,
            RealtimeTaskPhase::CleaningUp,
            RealtimeTaskPhase::AutoPasting,
        ] {
            assert_eq!(
                production_recording_action(phase, false),
                ProductionRecordingAction::IgnoreBusy
            );
        }
        assert_eq!(
            production_recording_action(RealtimeTaskPhase::Completed, false),
            ProductionRecordingAction::Start
        );
    }

    #[test]
    fn capsule_resize_gate_rejects_stale_generation_or_revision() {
        let state = RealtimeTaskStatusState::default();
        let mut current = RealtimeTaskSnapshot::default();
        current.generation = current.generation.next().next().next();
        current.revision = 23;
        state.replace(current);

        let mut ran = false;
        assert!(state
            .with_resize_gate(Some((2, 23)), || {
                ran = true;
                Ok(())
            })
            .unwrap()
            .is_none());
        assert!(!ran);
        assert!(state
            .with_resize_gate(Some((3, 22)), || {
                ran = true;
                Ok(())
            })
            .unwrap()
            .is_none());
        assert!(!ran);
        assert_eq!(
            state
                .with_resize_gate(Some((3, 23)), || Ok("applied"))
                .unwrap(),
            Some("applied")
        );
    }

    #[test]
    fn absent_input_device_has_a_stable_recoverable_error() {
        assert_eq!(
            platform::recording::require_available_input_device::<()>(None),
            Err("recording_microphone_unavailable".into())
        );
        let error = CoordinatorError::Port("recording_microphone_unavailable".into());
        assert_eq!(
            coordinator_error_code(&error),
            "recording_microphone_unavailable".to_string()
        );
        assert_eq!(
            stable_failure_code("recording_microphone_unavailable"),
            "recording_microphone_unavailable"
        );
    }

    #[test]
    fn shortcut_panic_is_caught_before_the_platform_callback_boundary() {
        assert!(!recording_action_did_not_panic(|| panic!("test panic")));
        assert!(recording_action_did_not_panic(|| {}));
    }

    #[test]
    fn realtime_progress_capsule_size_is_bounded_to_the_design_contract() {
        assert!(valid_realtime_progress_capsule_size(120, 40));
        assert!(valid_realtime_progress_capsule_size(360, 40));
        assert!(valid_realtime_progress_capsule_size(360, 104));
        assert!(!valid_realtime_progress_capsule_size(119, 40));
        assert!(!valid_realtime_progress_capsule_size(361, 40));
        assert!(!valid_realtime_progress_capsule_size(320, 104));
        assert!(!valid_realtime_progress_capsule_size(360, 80));
        assert!(!valid_realtime_progress_capsule_size(240, 41));
    }

    #[test]
    fn notice_canvas_keeps_the_pill_at_the_normal_screen_position() {
        for scale in [1.0_f64, 1.25, 2.0] {
            let normal_height = (40.0 * scale).round() as i32;
            let card_height = (RECORDING_CAPSULE_CARD_HEIGHT * scale).round() as i32;
            let card_inset = recording_capsule_bottom_inset(
                RECORDING_CAPSULE_LABEL,
                RECORDING_CAPSULE_CARD_HEIGHT,
                scale,
            );
            let normal_origin = recording_capsule_vertical_origin(
                0,
                1000,
                normal_height as u32,
                RECORDING_CAPSULE_LABEL,
                40.0,
                scale,
            );
            let card_origin = recording_capsule_vertical_origin(
                0,
                1000,
                card_height as u32,
                RECORDING_CAPSULE_LABEL,
                RECORDING_CAPSULE_CARD_HEIGHT,
                scale,
            );
            assert_eq!(
                normal_origin + normal_height,
                card_origin + card_height - card_inset
            );
            assert_eq!(
                normal_origin + normal_height,
                1000 - RECORDING_CAPSULE_BOTTOM_MARGIN
            );
            assert_eq!(card_inset, (4.0 * scale).round() as i32);
            assert_eq!(
                recording_capsule_bottom_inset(LEARNING_CAPSULE_LABEL, 104.0, scale),
                0
            );
            assert_eq!(
                recording_capsule_bottom_inset(RECORDING_CAPSULE_LABEL, 80.0, scale),
                0
            );
        }
    }

    #[test]
    fn realtime_task_status_state_returns_the_latest_snapshot() {
        let state = RealtimeTaskStatusState::default();
        assert_eq!(state.snapshot().phase, RealtimeTaskPhase::Idle);

        let mut snapshot = state.snapshot();
        snapshot.revision = 7;
        snapshot.task_id = Some("task-status".into());
        snapshot.phase = RealtimeTaskPhase::Transcribing;
        state.replace(snapshot.clone());

        assert_eq!(state.snapshot(), snapshot);
    }

    #[test]
    fn api_proxy_only_allows_known_dynamic_routes() {
        let request = |method: &str, path: &str| ApiRequest {
            method: method.into(),
            path: path.into(),
            body: None,
            query: None,
        };

        assert!(validate_api_request(&request("GET", "/sessions/session-1")).is_ok());
        assert!(validate_api_request(&request("GET", "/sessions/session-1/audio")).is_ok());
        assert!(validate_api_request(&request("POST", "/sessions/session-1/retry")).is_ok());
        assert!(validate_api_request(&request("PUT", "/sessions/session-1")).is_err());
        assert!(validate_api_request(&request("POST", "/accounts/account-1/unlock")).is_ok());
        assert!(
            validate_api_request(&request("PUT", "/models/punc")).is_ok(),
            "PUT /models/punc 放行（M3.4）"
        );
        assert!(
            validate_api_request(&request("PUT", "/models/spk")).is_ok(),
            "PUT /models/spk 放行"
        );
        assert!(
            validate_api_request(&request("POST", "/models/punc/download")).is_ok(),
            "POST /models/punc/download 放行（M4）"
        );
        assert!(
            validate_api_request(&request("POST", "/models//download")).is_err(),
            "空 component 应拒"
        );
        assert!(
            validate_api_request(&request("POST", "/models/a/b/download")).is_err(),
            "多段 component 应拒"
        );
        assert!(
            validate_api_request(&request("PUT", "/models/punc/extra")).is_err(),
            "PUT /models 多段应拒"
        );
        assert!(validate_api_request(&request("GET", "/sessions/session-1/future")).is_err());
        assert!(validate_api_request(&request("POST", "/models/model-1/future")).is_err());
        assert!(validate_api_request(&request("GET", "/reasoning/providers")).is_ok());
        assert!(validate_api_request(&request("GET", "/reasoning/provider-configs")).is_ok());
        assert!(validate_api_request(&request("POST", "/reasoning/provider-configs")).is_ok());
        assert!(validate_api_request(&request("POST", "/cleanup/test")).is_ok());
        assert!(validate_api_request(&request(
            "POST",
            "/reasoning/provider-configs/config-1/probe"
        ))
        .is_ok());
        assert!(
            validate_api_request(&request("PUT", "/reasoning/provider-configs/config-1")).is_ok()
        );
        assert!(
            validate_api_request(&request("DELETE", "/reasoning/provider-configs/config-1"))
                .is_ok()
        );
        assert!(validate_api_request(&request("GET", "/cleanup/settings")).is_ok());
        assert!(validate_api_request(&request("PUT", "/cleanup/settings")).is_ok());
        assert!(validate_api_request(&request(
            "PUT",
            "/internal/reasoning/provider-configs/config-1/credential"
        ))
        .is_err());
        // ST-M4.4：内部注入计划端点不得经 WebView api_request 代理（仅原生 DaemonClient 直连）。
        assert!(
            validate_api_request(&request("GET", "/sessions/session-1/injection-plan")).is_err()
        );
        assert!(validate_api_request(&request("GET", "/dictionary")).is_ok());
        assert!(validate_api_request(&request("POST", "/dictionary/entries")).is_ok());
        assert!(validate_api_request(&request("POST", "/dictionary/imports/preview")).is_ok());
        assert!(validate_api_request(&request("POST", "/dictionary/imports")).is_ok());
        assert!(validate_api_request(&request("PUT", "/dictionary/entries/entry-1")).is_ok());
        assert!(validate_api_request(&request("DELETE", "/dictionary/entries/entry-1")).is_ok());
        assert!(validate_api_request(&request("DELETE", "/dictionary/entries")).is_ok());
        assert!(validate_api_request(&request("GET", "/dictionary/export")).is_err());
        assert!(
            validate_api_request(&request("POST", "/internal/dictionary/learning-events")).is_err()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn general_pasteboard_exposes_a_change_counter() {
        assert!(platform::clipboard::pasteboard_change_count() >= 0);
    }

    #[test]
    fn recorder_combinations_are_accepted_by_native_parser() {
        for shortcut in [
            "Command+Alt+A",
            "Control+Shift+2",
            "Command+ArrowUp",
            "Alt+Backquote",
            "Control+F24",
        ] {
            assert!(Shortcut::from_str(shortcut).is_ok(), "{shortcut}");
        }
    }

    #[test]
    fn default_recording_shortcut_is_command_shift_space() {
        let config = HotkeyConfig::default();
        assert_eq!(config.recording_shortcut, "Command+Shift+Space");
        assert!(config.clipboard_context_enabled);
        assert!(config.locale.is_none());
        assert!(Shortcut::from_str(&config.recording_shortcut).is_ok());
    }

    #[test]
    fn locale_validation_accepts_allowlist_only() {
        assert!(validate_locale("zh-CN").is_ok());
        assert!(validate_locale("en-US").is_ok());
        assert_eq!(validate_locale("zh-TW"), Err("invalid_locale".into()));
        assert_eq!(validate_locale("fr-FR"), Err("invalid_locale".into()));
    }

    #[test]
    fn resource_identity_rejects_replaced_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resource.txt");
        std::fs::write(&path, b"original").unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        #[cfg(unix)]
        let (device, inode) = {
            use std::os::unix::fs::MetadataExt;
            (Some(metadata.dev()), Some(metadata.ino()))
        };
        #[cfg(not(unix))]
        let (device, inode) = (None, None);
        let resource = ResolvedResource {
            path: path.to_string_lossy().into_owned(),
            kind: "file".into(),
            size: metadata.len(),
            modified_unix_ms: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis()),
            device,
            inode,
        };
        std::fs::write(&path, b"replacement with a different size").unwrap();
        let replaced = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!(
            validate_resolved_resource_identity(&resource, &replaced),
            Err("resource_changed".into())
        );
    }

    #[test]
    fn resource_identity_rejects_unsupported_kind() {
        let metadata = std::fs::metadata(std::env::current_exe().unwrap()).unwrap();
        #[cfg(unix)]
        let (device, inode) = {
            use std::os::unix::fs::MetadataExt;
            (Some(metadata.dev()), Some(metadata.ino()))
        };
        #[cfg(not(unix))]
        let (device, inode) = (None, None);
        let resource = ResolvedResource {
            path: String::new(),
            kind: "directory".into(),
            size: metadata.len(),
            modified_unix_ms: None,
            device,
            inode,
        };
        assert_eq!(
            validate_resolved_resource_identity(&resource, &metadata),
            Err("resource_unsafe".into())
        );
    }

    #[test]
    fn fake_opener_runs_only_after_resource_validation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resource.txt");
        std::fs::write(&path, b"safe").unwrap();
        let path = path.canonicalize().unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        #[cfg(unix)]
        let (device, inode) = {
            use std::os::unix::fs::MetadataExt;
            (Some(metadata.dev()), Some(metadata.ino()))
        };
        #[cfg(not(unix))]
        let (device, inode) = (None, None);
        let resource = ResolvedResource {
            path: path.to_string_lossy().into_owned(),
            kind: "file".into(),
            size: metadata.len(),
            modified_unix_ms: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis()),
            device,
            inode,
        };
        let mut opened = None;
        let result = open_validated_resource(&resource, &path, |target, is_file| {
            opened = Some((target.to_owned(), is_file));
            Ok(true)
        });
        assert_eq!(result, Ok(true));
        assert_eq!(opened, Some((path.to_string_lossy().into_owned(), true)));

        std::fs::write(&path, b"replaced").unwrap();
        let mut called = false;
        let result = open_validated_resource(&resource, &path, |_, _| {
            called = true;
            Ok(true)
        });
        assert_eq!(result, Err("resource_changed".into()));
        assert!(!called, "opener must not run after identity mismatch");
    }

    #[cfg(unix)]
    #[test]
    fn fake_opener_rechecks_executable_policy_after_resolve() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resource.txt");
        std::fs::write(&path, b"safe").unwrap();
        let path = path.canonicalize().unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        use std::os::unix::fs::MetadataExt;
        let resource = ResolvedResource {
            path: path.to_string_lossy().into_owned(),
            kind: "file".into(),
            size: metadata.len(),
            modified_unix_ms: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis()),
            device: Some(metadata.dev()),
            inode: Some(metadata.ino()),
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();

        let mut called = false;
        let result = open_validated_resource(&resource, &path, |_, _| {
            called = true;
            Ok(true)
        });
        assert_eq!(result, Err("resource_unsafe".into()));
        assert!(!called, "opener must not run after executable-bit change");
    }

    #[test]
    fn fake_link_opener_accepts_only_http_https() {
        for url in ["https://example.com/a", "http://localhost:43123"] {
            let mut opened = false;
            let result = validate_http_link(url).and_then(|_| {
                opened = true;
                Ok(())
            });
            assert_eq!(result, Ok(()));
            assert!(opened);
        }
        for url in [
            "file:///tmp/a",
            "javascript:alert(1)",
            "https:///missing-host",
        ] {
            assert_eq!(validate_http_link(url), Err("unsupported_scheme".into()));
        }
    }

    #[test]
    fn missing_hotkey_preferences_fall_back_to_default() {
        let path = std::env::temp_dir().join(format!(
            "seasnail-no-hotkey-preferences-{}",
            std::process::id()
        ));
        assert_eq!(
            read_hotkey_config(&path).recording_shortcut,
            DEFAULT_RECORDING_SHORTCUT
        );
        assert!(read_hotkey_config(&path).clipboard_context_enabled);
        assert!(read_hotkey_config(&path).auto_paste_enabled);
    }

    #[test]
    fn old_preferences_default_clipboard_context_to_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gui-preferences.json");
        std::fs::write(&path, r#"{"recording_shortcut":"Command+Shift+Space"}"#).unwrap();
        let config = read_hotkey_config(&path);
        assert!(config.clipboard_context_enabled);
        assert!(config.auto_paste_enabled);
        assert!(config.dictionary_auto_learn_enabled);
    }

    #[test]
    fn recording_behavior_preference_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gui-preferences.json");
        let config = HotkeyConfig {
            recording_shortcut: DEFAULT_RECORDING_SHORTCUT.into(),
            clipboard_context_enabled: true,
            auto_paste_enabled: false,
            keep_transcription_in_clipboard: true,
            dictionary_auto_learn_enabled: false,
            locale: Some("en-US".into()),
        };
        write_hotkey_config(&path, &config).unwrap();
        let restored = read_hotkey_config(&path);
        assert!(!restored.auto_paste_enabled);
        assert!(!restored.dictionary_auto_learn_enabled);
        assert_eq!(restored.locale.as_deref(), Some("en-US"));
    }

    #[test]
    fn account_transition_clears_learning_state_and_old_timer_cannot_clear_new_account() {
        let state = LearningNotificationState::default();
        let alice = state.publish("alice-event".into(), vec!["AlicePrivateTerm".into()]);
        assert_eq!(state.clear(), Some(alice.generation));
        assert!(state.current().is_none());
        assert!(state.event_id(alice.generation).is_none());
        let bob = state.publish("bob-event".into(), vec!["BobTerm".into()]);
        assert!(!state.clear_if_generation(alice.generation));
        assert_eq!(state.current().unwrap().generation, bob.generation);
    }

    #[test]
    fn learning_notification_keeps_event_id_native_and_is_generation_bound() {
        let state = LearningNotificationState::default();
        let first = state.publish("event-private".into(), vec!["SeaSnail".into()]);
        assert_eq!(first.generation, 1);
        assert_eq!(
            state.event_id(first.generation).as_deref(),
            Some("event-private")
        );
        assert!(!serde_json::to_string(&first)
            .unwrap()
            .contains("event-private"));
        assert!(!state.clear_if_generation(first.generation + 1));
        assert!(state.current().is_some());
        assert!(state.clear_if_generation(first.generation));
        assert!(state.current().is_none());
    }
}
