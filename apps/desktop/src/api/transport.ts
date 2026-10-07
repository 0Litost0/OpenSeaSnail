import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { components, paths } from "./openapi";

type OpenApiMethod = "get" | "post" | "put" | "delete";
export type OpenApiPath = keyof paths;
export type ApiRequest<Path extends OpenApiPath = OpenApiPath> = {
  method: Uppercase<Extract<keyof paths[Path], OpenApiMethod>>;
  path: Path;
  body?: unknown;
  query?: Record<string, string | undefined>;
};
export type ApiResponse<T> = { status: number; body: T };
export type Session = components["schemas"]["Session"];
export type SessionList = components["schemas"]["SessionList"];
export type ExportRequest = components["schemas"]["ExportRequest"];

export interface DaemonStatus {
  connected: boolean;
  authenticated: boolean;
}

/** Bearer、端口和 trace ID 均由 Rust 注入，WebView 只传 OpenAPI 操作。 */
export function request<T, Path extends OpenApiPath>(request: ApiRequest<Path>): Promise<ApiResponse<T>> {
  return invoke<ApiResponse<T>>("api_request", { request });
}

/** 导出 ZIP 在 Rust 中请求并落盘，WebView 不接触明文文件字节。 */
export function exportToFile(request: ExportRequest): Promise<boolean> {
  return invoke<boolean>("export_to_file", { request });
}

/** Dictionary CSV 由原生层拉取并通过保存对话框落盘，CSV 明文不进入 WebView。 */
export function exportDictionaryToFile(): Promise<boolean> {
  return invoke<boolean>("export_dictionary_to_file");
}

/** 守护进程连接态来自原生层；端口和 bearer 均不会暴露给 WebView。 */
export function daemonStatus(): Promise<DaemonStatus> {
  return invoke<DaemonStatus>("daemon_status");
}

export type AppLocale = "zh-CN" | "en-US";

export function getLocalePreference(): Promise<AppLocale | null> {
  return invoke<AppLocale | null>("get_locale_preference");
}

export function setLocalePreference(locale: AppLocale): Promise<AppLocale> {
  return invoke<AppLocale>("set_locale_preference", { locale });
}

export function getDictionaryAutoLearnEnabled(): Promise<boolean> {
  return invoke<boolean>("get_dictionary_auto_learn_enabled");
}

export function setDictionaryAutoLearnEnabled(enabled: boolean): Promise<boolean> {
  return invoke<boolean>("set_dictionary_auto_learn_enabled", { enabled });
}

export function onLocaleChanged(handler: (locale: AppLocale) => void): Promise<UnlistenFn> {
  return listen<AppLocale>("locale-changed", (event) => handler(event.payload));
}

/** 原生层在调整胶囊尺寸后按当前屏幕重新底部居中，避免长错误态向单侧扩张。 */
export function resizeRealtimeProgressCapsule(width: number, height: number, generation: number, revision: number): Promise<void> {
  return invoke<void>("resize_realtime_progress_capsule", { width, height, generation, revision });
}

export interface ContextResource {
  index: number;
  /** 工作台详情与复制结果使用的本地绝对路径。普通会话 API 不返回该字段。 */
  path: string;
  display_name: string;
  mime_type: string;
  available: boolean;
}

/** 上下文展示位置；`separate` 为 cleanup cache 缺失时 daemon 降级布局的实际取值。 */
export type ContextPlacement = "exact" | "fallback" | "approximate" | "separate";

/** 7-variant 只读展示项（discriminator = `kind`），对齐设计工作台详情 DTO。 */
export type TimelineItem =
  | { kind: "final_text"; text: string }
  | { kind: "transcript"; text: string; speaker: string }
  | { kind: "context_text"; sequence: number; captured_at_ms: number; placement: ContextPlacement; text: string }
  | { kind: "context_rich_text"; sequence: number; captured_at_ms: number; placement: ContextPlacement; plain_text: string; sanitized_html: string }
  | { kind: "context_link"; sequence: number; captured_at_ms: number; placement: ContextPlacement; url: string }
  | { kind: "context_file"; sequence: number; captured_at_ms: number; placement: ContextPlacement; resources: ContextResource[] }
  | { kind: "context_image"; sequence: number; captured_at_ms: number; placement: ContextPlacement; resources: ContextResource[] };

/** 上下文展示项（非 transcript）的联合类型，供 renderer 按变体分发。 */
export type ContextItem = Exclude<TimelineItem, { kind: "transcript" | "final_text" }>;

export interface WorkspaceDetail {
  full_text: string;
  final_text: string;
  text_source: "raw" | "cleanup";
  cleanup_status: "not_requested" | "disabled" | "processing" | "succeeded" | "failed";
  cleanup_error_code: string | null;
  context_layout: "none" | "inline" | "separate";
  display_items: TimelineItem[];
  /** 仅在 cleanup + presentation cache 缺失分支非空；daemon 此分支只产生 ContextItem 变体。 */
  separate_contexts: ContextItem[];
  context_degraded: boolean;
}

export interface CleanupDetail {
  original_text: string;
  cleaned_text: string | null;
  corrections: Array<{ original_text: string; corrected_text: string; kind: "phonetic" | "proper_noun" | "other_asr" | "unknown" }>;
  cleanup_elapsed_ms: number | null;
  diagnostics: {
    local_transcription_elapsed_ms: number | null;
    trace_id: string;
    request_started_at_ms: number;
    response_started_at_ms: number | null;
    response_completed_at_ms: number | null;
    http_status: number | null;
    response_content_type: string | null;
    provider_request_id: string | null;
    raw_response: string | null;
    raw_response_base64: string;
    response_sha256: string;
    response_body_bytes: number;
    capture_status: "not_received" | "complete" | "too_large" | "read_failed" | "redacted" | "unknown";
  } | null;
}

export interface ContextThumbnail {
  mime_type: "image/png";
  base64: string;
  width: number;
  height: number;
}

export function getSessionWorkspaceDetail(sessionId: string): Promise<WorkspaceDetail> {
  return invoke<WorkspaceDetail>("get_session_workspace_detail", { sessionId });
}

export function getSessionCleanupDetail(sessionId: string): Promise<CleanupDetail> {
  return invoke<CleanupDetail>("get_session_cleanup_detail", { sessionId });
}

export function getContextThumbnail(sessionId: string, sequence: number, resourceIndex: number): Promise<ContextThumbnail> {
  return invoke<ContextThumbnail>("get_context_thumbnail", { sessionId, sequence, resourceIndex });
}

export function openContextResource(sessionId: string, sequence: number, resourceIndex: number): Promise<boolean> {
  return invoke<boolean>("open_context_resource", { sessionId, sequence, resourceIndex });
}

export function openContextLink(sessionId: string, sequence: number): Promise<boolean> {
  return invoke<boolean>("open_context_link", { sessionId, sequence });
}

export function copyText(text: string): Promise<void> {
  return invoke<void>("copy_text", { text });
}

export interface RecordingShortcutStatus {
  recording_shortcut: string;
  registered: boolean;
  registration_error: string | null;
}

/** 设置页通过原生层读取快捷键；WebView 不直接调用全局快捷键插件。 */
export function recordingShortcut(): Promise<RecordingShortcutStatus> {
  return invoke<RecordingShortcutStatus>("recording_shortcut");
}

/** 原生层先注册新组合、再移除旧组合；失败时旧组合继续可用。 */
export function setRecordingShortcutCapture(capturing: boolean): Promise<void> {
  return invoke<void>("set_recording_shortcut_capture", { capturing });
}

export function setRecordingShortcut(shortcut: string): Promise<RecordingShortcutStatus> {
  return invoke<RecordingShortcutStatus>("set_recording_shortcut", { shortcut });
}

export interface ClipboardContextStatus {
  enabled: boolean;
}

/** 剪贴板上下文采集开关由原生偏好存储；内容永不经过 WebView。 */
export function clipboardContextStatus(): Promise<ClipboardContextStatus> {
  return invoke<ClipboardContextStatus>("clipboard_context_status");
}

export function setClipboardContextEnabled(enabled: boolean): Promise<ClipboardContextStatus> {
  return invoke<ClipboardContextStatus>("set_clipboard_context_enabled", { enabled });
}

export interface RecordingBehaviorStatus {
  auto_paste_enabled: boolean;
  keep_transcription_in_clipboard: boolean;
}

export type RealtimeTaskPhase = "idle" | "preparing" | "recording" | "submitting" | "transcribing" | "cleaning_up" | "auto_pasting" | "completed" | "failed";
export type RealtimeTaskFallback = "none" | "clipboard" | "history";
export interface RealtimeTaskSnapshot {
  generation: number;
  revision: number;
  task_id: string | null;
  phase: RealtimeTaskPhase;
  session_id: string | null;
  clipboard_context_count: number;
  auto_paste_enabled: boolean;
  failure_code: string | null;
  fallback: RealtimeTaskFallback;
}

export function realtimeTaskSnapshot(): Promise<RealtimeTaskSnapshot> {
  return invoke<RealtimeTaskSnapshot>("realtime_task_status");
}

export function onRealtimeTaskSnapshot(handler: (snapshot: RealtimeTaskSnapshot) => void): Promise<UnlistenFn> {
  return listen<RealtimeTaskSnapshot>("realtime-task-status", (event) => handler(event.payload));
}

export function recordingBehaviorStatus(): Promise<RecordingBehaviorStatus> {
  return invoke<RecordingBehaviorStatus>("recording_behavior_status");
}

export function setAutoPasteEnabled(enabled: boolean): Promise<RecordingBehaviorStatus> {
  return invoke<RecordingBehaviorStatus>("set_auto_paste_enabled", { enabled });
}

export function setKeepTranscriptionInClipboard(enabled: boolean): Promise<RecordingBehaviorStatus> {
  return invoke<RecordingBehaviorStatus>("set_keep_transcription_in_clipboard", { enabled });
}

/** Provider credential 只经专用原生命令写入；WebView 无读取 secret 的能力。null 表示显式无认证。 */
export function setProviderCredential(configId: string, credential: string | null): Promise<string> {
  return invoke<string>("set_provider_credential", { configId, credential });
}

export function deleteProviderCredential(configId: string): Promise<string> {
  return invoke<string>("delete_provider_credential", { configId });
}

export interface InputDevice {
  name: string;
  is_default: boolean;
}

export interface RecordingStatus {
  is_recording: boolean;
  elapsed_ms: number;
  level: number;
  input_device: string | null;
  sample_rate: number | null;
  error: string | null;
  clipboard_context_count: number;
  clipboard_context_error: string | null;
}

export interface MicrophonePermission {
  granted: boolean;
  status: "authorized" | "not_determined" | "denied" | "restricted" | "unknown" | "unsupported";
}

export interface PermissionsStatus {
  microphone: MicrophonePermission;
  accessibility_granted: boolean;
}

/** 设备枚举和采集均在原生层，录音字节不会经过 WebView。 */
export function inputDevices(): Promise<InputDevice[]> {
  return invoke<InputDevice[]>("input_devices");
}

export function recordingStatus(): Promise<RecordingStatus> {
  return invoke<RecordingStatus>("recording_status");
}

/** 状态由原生层至多每 100ms 推送一次；渲染侧可再用 rAF 合帧。 */
export function onRecordingStatus(handler: (status: RecordingStatus) => void): Promise<UnlistenFn> {
  return listen<RecordingStatus>("recording-status", (event) => handler(event.payload));
}

export function onRecordingError(handler: (message: string) => void): Promise<UnlistenFn> {
  return listen<string>("recording-error", (event) => handler(event.payload));
}

/** 设置页据此显示 TCC 缺失项；状态读取本身不会触发系统弹窗。 */
export function permissionStatus(): Promise<PermissionsStatus> {
  return invoke<PermissionsStatus>("permission_status");
}

/** 仅由用户手势调用，触发或复用 macOS 麦克风授权决定。 */
export function requestMicrophonePermission(): Promise<MicrophonePermission> {
  return invoke<MicrophonePermission>("request_microphone_permission");
}

/** 辅助功能必须由用户在 macOS 隐私设置中显式启用。 */
export function openAccessibilitySettings(): Promise<void> {
  return invoke<void>("open_accessibility_settings");
}


export interface DesktopAuthStatus {
  credential_ready?: boolean;
  initialized: boolean;
  authenticated: boolean;
  accounts: Array<{ id: string; username: string; is_active: boolean }>;
}
export function desktopAuthStatus(): Promise<DesktopAuthStatus> {
  return invoke<DesktopAuthStatus>("desktop_auth", { action: "status", body: null });
}
export function desktopAccountAction(action: "login" | "create" | "logout", body?: { id?: string; username?: string; password: string }): Promise<DesktopAuthStatus> {
  return invoke<DesktopAuthStatus>("desktop_auth", { action, body: body ?? null });
}
