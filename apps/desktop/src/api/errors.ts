import type { TranslationKey } from "@/i18n/dictionaries";

/**
 * 稳定错误码模型（ST-M5.6）。
 *
 * daemon 的所有错误响应统一为 `{"error":{"code":<snake_case>,"message":<str>}}`；
 * `code` 是稳定契约串，前端按 `code` 本地化，不直接展示 `message`。原生命令
 * （Tauri invoke）失败时也以稳定码字符串 reject；两端共用同一组码。
 *
 * `unwrap` 负责 HTTP 路径：成功返回 body，失败按 daemon `error.code` 或 HTTP
 * 状态回退抛 `ApiError`。传输层错误（invoke reject）经 `errorCode` 归一为码。
 * `localizedError` 是 UI 唯一出口：已知码映射 `error.<code>`，未知串一律回退
 * `error.generic`，避免任何 daemon/原生中文消息泄露到界面。
 */

/** 翻译函数签名（与 `I18nProvider` 的 `t` 一致，避免在此引入 React 依赖）。 */
export type Translate = (key: TranslationKey, variables?: Record<string, string | number>) => string;

/** 携带稳定错误码的错误。`.message` 即码，便于旧代码按字符串处理。 */
export class ApiError extends Error {
  readonly code: string;
  constructor(code: string) {
    super(code);
    this.name = "ApiError";
    this.code = code;
  }
}

/** HTTP 状态码到稳定错误码的回退（daemon 未给出 `error.code` 时）。 */
const STATUS_CODE: Record<number, string> = {
  400: "bad_request",
  401: "unauthorized",
  403: "forbidden",
  404: "not_found",
  409: "conflict",
  410: "gone",
  413: "payload_too_large",
  422: "unprocessable_entity",
  423: "locked",
  500: "internal",
  503: "service_unavailable",
};

/** 从任意拒绝值归一稳定错误码；未知值原样返回（由 `localizedError` 兜底）。 */
export function errorCode(reason: unknown): string {
  if (reason instanceof ApiError) return reason.code;
  if (typeof reason === "string") return reason;
  if (reason instanceof Error) return reason.message;
  return "generic";
}

/**
 * 成功返回 body；失败按 daemon `error.code`（优先）或 HTTP 状态回退抛 `ApiError`。
 * 传输层错误（invoke reject，daemon 不可达等）也归一为 `ApiError`，码来自原生。
 */
export async function unwrap<T>(promise: Promise<{ status: number; body: T }>): Promise<T> {
  let response: { status: number; body: T };
  try {
    response = await promise;
  } catch (reason) {
    throw new ApiError(errorCode(reason));
  }
  if (response.status >= 200 && response.status < 300) return response.body;
  const body = response.body as { error?: { code?: string } } | null | undefined;
  throw new ApiError(body?.error?.code ?? STATUS_CODE[response.status] ?? "generic");
}

/** 已知稳定错误码集合；集合外的字符串一律按 `generic` 兜底，不展示原始消息。 */
const KNOWN_CODES: ReadonlySet<string> = new Set([
  // daemon HTTP 错误码
  "bad_request",
  "unauthorized",
  "forbidden",
  "wrong_password",
  "account_busy",
  "insufficient_scope",
  "cross_account",
  "scope_not_grantable",
  "not_found",
  "context_not_found",
  "conflict",
  "unprocessable_entity",
  "gone",
  "locked",
  "payload_too_large",
  "request_too_large",
  "dictionary_invalid_term",
  "dictionary_csv_invalid",
  "dictionary_limit_exceeded",
  "dictionary_conflict",
  "internal",
  "service_unavailable",
  // 原生资源/剪贴板/语言
  "resource_unavailable",
  "resource_unsafe",
  "resource_changed",
  "unsupported_scheme",
  "thumbnail_budget_exceeded",
  "thumbnail_busy",
  "thumbnail_unavailable",
  "open_failed",
  "clipboard_write_failed",
  "locale_save_failed",
  "invalid_locale",
  "clipboard_unavailable",
  // 传输与连接
  "connection",
  "daemon_request_failed",
  // 录制
  "recording_microphone_unavailable",
  "recording_microphone_unauthorized",
  "recording_failed",
  "recording_submission_in_progress",
  "recording_no_audio",
  "recording_already_active",
  "recording_not_active",
  "recording_device_error",
  "recording_submit_failed",
  "recording_pcm_unavailable",
  "recording_accessibility_required",
  "recording_auto_paste_failed",
  "shortcut_invalid",
  "shortcut_register_failed",
  // Cleanup/provider configuration and execution
  "invalid_credential",
  "cleanup_not_configured",
  "cleanup_endpoint_rejected",
  "cleanup_credential_missing",
  "cleanup_input_too_large",
  "cleanup_timeout",
  "cleanup_http_auth",
  "cleanup_http_rate_limit",
  "cleanup_http_server",
  "cleanup_response_too_large",
  "cleanup_response_invalid_json",
  "cleanup_cleaned_text_invalid",
  "cleanup_placeholder_invalid",
  "cleanup_transport_error",
  "cleanup_busy",
  // 剪贴板上下文采集
  "clipboard_overflow",
  "clipboard_text_too_large",
  "clipboard_rich_too_large",
  "clipboard_too_many_files",
  "clipboard_invalid_path",
  "clipboard_sample_rate_out_of_range",
  "clipboard_image_write_failed",
  "clipboard_context_limit",
  "clipboard_collect_error",
  // 通用
  "generic",
]);

/** 稳定码到 i18n key 的别名（码与 key 不一致时）。 */
const CODE_KEY_ALIAS: Record<string, TranslationKey> = {
  connection: "error.connection",
  daemon_request_failed: "error.connection",
};

/**
 * UI 错误展示的唯一出口：把任意拒绝值映射为本地化文案。已知码映射 `error.<code>`
 *（或别名），未知串（含任何残留 daemon/原生中文）一律回退 `error.generic`，
 * 不向用户展示内部消息或路径。
 */
export function localizedError(t: Translate, reason: unknown): string {
  const code = errorCode(reason);
  if (!KNOWN_CODES.has(code)) return t("error.generic");
  const alias = CODE_KEY_ALIAS[code];
  return t(alias ?? (`error.${code}` as TranslationKey));
}
