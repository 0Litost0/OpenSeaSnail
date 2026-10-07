//! Sessions HTTP adapters。multipart/JSON/query、body limit、HTTP status/header 在此转换；
//! create/retry 的账户绑定、提交点、runtime reservation、canonical pipeline 与后台任务
//! 所有权统一交给 application `TranscriptionService`。

use crate::api::dto::{
    InjectionPlanDto, ResolvedLinkDto, ResolvedResourceDto, SessionListDto, SessionSummaryDto,
    SessionView, SpeakerDto, TranscriptDto,
};
use crate::api::{require_scope, AuthedCaller, HttpState};
use crate::application::resolve_final_text;
use crate::error::AppError;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const THUMBNAIL_WAIT_TIMEOUT: Duration = Duration::from_secs(3);

/// Wait for this session before taking a global slot, so a session's queued
/// images cannot occupy all global capacity. Both waits share one deadline.
pub(super) async fn acquire_thumbnail_permits(
    session: Arc<Semaphore>,
    global: Arc<Semaphore>,
    wait_timeout: Duration,
) -> Result<(OwnedSemaphorePermit, OwnedSemaphorePermit), AppError> {
    let deadline = tokio::time::Instant::now() + wait_timeout;
    let acquire = |slots: Arc<Semaphore>, stage: &'static str| async move {
        match tokio::time::timeout_at(deadline, slots.acquire_owned()).await {
            Ok(Ok(permit)) => Ok(permit),
            result => {
                let reason = if result.is_err() {
                    "wait_timeout"
                } else {
                    "closed"
                };
                tracing::warn!(stage, reason, "thumbnail admission failed");
                Err(AppError::ThumbnailBusy("thumbnail service busy".into()))
            }
        }
    };
    let session_permit = acquire(session, "session").await?;
    let global_permit = acquire(global, "global").await?;
    Ok((session_permit, global_permit))
}

/// 音频上限（in-memory 读 + 落库前校验）。必须容纳 60 分钟 16kHz mono s16le WAV
/// （115,200,044 B）及 multipart 余量；时长上限仍由归一化后的 gate 精确裁决。
/// 后置：流式写 temp + 边读边判，避免整块读入内存。
const MAX_AUDIO_BYTES: usize = 128 * 1024 * 1024;
const MAX_CONTEXT_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_PAGE_LIMIT: usize = 50;
const MAX_PAGE_LIMIT: usize = 100;
#[derive(Deserialize)]
struct ListQuery {
    cursor: Option<String>,
    limit: Option<usize>,
    source: Option<String>,
}
#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    cursor: Option<String>,
    limit: Option<usize>,
    source: Option<String>,
}
fn decode_cursor(raw: Option<&str>) -> Result<Option<(i64, String)>, AppError> {
    let Some(raw) = raw else { return Ok(None) };
    let decoded = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| AppError::Conflict("invalid cursor".into()))?;
    let value =
        String::from_utf8(decoded).map_err(|_| AppError::Conflict("invalid cursor".into()))?;
    let (time, id) = value
        .split_once(':')
        .ok_or_else(|| AppError::Conflict("invalid cursor".into()))?;
    Ok(Some((
        time.parse()
            .map_err(|_| AppError::Conflict("invalid cursor".into()))?,
        id.into(),
    )))
}
fn encode_cursor(row: &crate::application::SessionResult) -> String {
    URL_SAFE_NO_PAD.encode(format!("{}:{}", row.created_at, row.id))
}
async fn list_sessions(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Query(query): Query<ListQuery>,
) -> Result<Json<SessionListDto>, AppError> {
    require_scope(&caller, "sessions:read")?;
    let source = match query.source.as_deref() {
        Some("realtime") | Some("imported") => query.source.as_deref(),
        Some(_) => return Err(AppError::Conflict("invalid source".into())),
        None => None,
    };
    let limit = query
        .limit
        .unwrap_or(DEFAULT_PAGE_LIMIT)
        .clamp(1, MAX_PAGE_LIMIT);
    let cursor = decode_cursor(query.cursor.as_deref())?;
    let before = cursor.as_ref().map(|(t, id)| (*t, id.as_str()));
    let mut rows = state.sessions().list(
        &caller,
        (limit + 1) as i64,
        before.map(|(t, id)| (t, id.to_string())),
        source,
    )?;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor =
        has_more.then(|| encode_cursor(rows.last().expect("page has row when has_more")));
    let items = rows
        .iter()
        .map(|row| {
            let resolved = resolve_final_text(caller.repository().storage(), row)?;
            let preview = resolved
                .as_ref()
                .map(|value| crate::application::final_text_preview(&value.text))
                .unwrap_or_default();
            Ok(SessionSummaryDto::from_row(
                row,
                preview,
                resolved.as_ref(),
                caller.is_root(),
            ))
        })
        .collect::<Result<Vec<_>, crate::error::AppError>>()?;
    Ok(Json(SessionListDto { items, next_cursor }))
}
async fn search_sessions(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Query(query): Query<SearchQuery>,
) -> Result<Json<SessionListDto>, AppError> {
    require_scope(&caller, "sessions:read")?;
    let source = match query.source.as_deref() {
        Some("realtime") | Some("imported") => query.source.as_deref(),
        Some(_) => return Err(AppError::Conflict("invalid source".into())),
        None => None,
    };
    let limit = query
        .limit
        .unwrap_or(DEFAULT_PAGE_LIMIT)
        .clamp(1, MAX_PAGE_LIMIT);
    let cursor = decode_cursor(query.cursor.as_deref())?;
    let before = cursor.as_ref().map(|(time, id)| (*time, id.as_str()));
    let mut matched = state.sessions().search(
        &caller,
        &query.q,
        (limit + 1) as i64,
        before.map(|(t, id)| (t, id.to_string())),
        source,
    )?;
    let has_more = matched.len() > limit;
    matched.truncate(limit);
    let next_cursor = has_more.then(|| {
        let hit = matched.last().expect("page has row when has_more");
        URL_SAFE_NO_PAD.encode(format!("{}:{}", hit.created_at, hit.session_id))
    });
    let mut items = Vec::with_capacity(matched.len());
    for hit in matched {
        if let Some(row) = state.sessions().get_optional(&caller, &hit.session_id)? {
            let resolved = resolve_final_text(caller.repository().storage(), &row)?;
            items.push(SessionSummaryDto::from_row(
                &row,
                hit.snippet,
                resolved.as_ref(),
                caller.is_root(),
            ));
        }
    }
    Ok(Json(SessionListDto { items, next_cursor }))
}

/// `POST /sessions` 202 响应：`{id, status:"transcribing"}`。
#[derive(Debug, Serialize)]
struct CreateSessionResp {
    id: String,
    status: String,
}

/// `POST /sessions` — 创建会话（提交音频做转译）。
async fn create_session(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<CreateSessionResp>), AppError> {
    // HTTP adapter 只解析 multipart 与执行传输大小限制；业务默认、context 校验、
    // runtime reservation、提交点及后台任务均归 TranscriptionService。
    let mut audio_bytes: Option<Vec<u8>> = None;
    let mut file_name = "audio.wav".to_string();
    let mut source: Option<String> = None;
    let mut language: Option<String> = None;
    let mut input_device: Option<String> = None;
    let mut clipboard_context: Option<Vec<u8>> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::PayloadTooLarge(format!("invalid multipart: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        let fname = field.file_name().map(|s| s.to_string());
        let bytes = field
            .bytes()
            .await
            .map_err(|e| AppError::PayloadTooLarge(format!("invalid multipart field: {e}")))?;
        match name.as_str() {
            "audio" => {
                if bytes.len() > MAX_AUDIO_BYTES {
                    return Err(AppError::PayloadTooLarge(format!(
                        "audio exceeds {MAX_AUDIO_BYTES} bytes"
                    )));
                }
                file_name = fname.unwrap_or_else(|| "audio.wav".into());
                audio_bytes = Some(bytes.to_vec());
            }
            "source" => {
                source = Some(String::from_utf8_lossy(&bytes).trim().to_string());
            }
            "language" => {
                language = Some(String::from_utf8_lossy(&bytes).trim().to_string());
            }
            "input_device" => {
                let value = String::from_utf8_lossy(&bytes).trim().to_string();
                if !value.is_empty() {
                    input_device = Some(value);
                }
            }
            "clipboard_context" => {
                if bytes.len() > MAX_CONTEXT_BYTES || clipboard_context.is_some() {
                    return Err(AppError::PayloadTooLarge(
                        "invalid clipboard context field".into(),
                    ));
                }
                clipboard_context = Some(bytes.to_vec());
            }
            _ => {}
        }
    }
    let audio_bytes =
        audio_bytes.ok_or_else(|| AppError::PayloadTooLarge("missing audio field".into()))?;
    let accepted = state
        .transcription_service()
        .create(crate::application::CreateTranscriptionCommand {
            caller,
            audio: audio_bytes,
            file_name,
            source: source.unwrap_or_default(),
            language,
            input_device,
            clipboard_context,
        })
        .await
        .map_err(AppError::from)?;

    Ok((
        StatusCode::ACCEPTED,
        Json(CreateSessionResp {
            id: accepted.session_id,
            status: accepted.status,
        }),
    ))
}

/// `GET /sessions/{id}` — 普通会话详情（只读、无剪贴板上下文）。
///
/// 返回 `Session` 视图：元数据 + speakers + transcript（status=completed 时由
/// `read_transcript` 解密拼装）+ failure_reason（status=failed 时）。行不存在 → 404。
/// transcript 解密或完整性失败返回稳定 conflict，避免伪装为 completed + null。鉴权 `sessions:read`。
async fn get_session(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path(id): Path<String>,
) -> Result<Json<SessionView>, AppError> {
    require_scope(&caller, "sessions:read")?;
    let row = state.sessions().get(&caller, &id)?;
    // transcript + speakers 仅 completed 且 transcript_path 存在时拼装。
    let (transcript, speakers) = if row.status == "completed" {
        match row.transcript_path.as_ref() {
            Some(tp) => match state.sessions().read_transcript(&caller, tp) {
                Ok(t) => {
                    let speakers = t.speakers.iter().map(SpeakerDto::from).collect::<Vec<_>>();
                    (Some(TranscriptDto::from_result(&t)), speakers)
                }
                // schema/完整性错误不能伪装为 completed + transcript:null。
                Err(_) => return Err(AppError::Conflict("transcript unavailable".into())),
            },
            None => (None, Vec::new()),
        }
    } else {
        (None, Vec::new())
    };
    Ok(Json(SessionView::from_row(&row, transcript, speakers)))
}

/// 本地工作台专用详情（root-only）：最终正文与 typed `display_items`。
/// 对齐设计：普通 `sessions:read` 不返回此结构，第三方无依赖。
#[derive(Serialize)]
struct WorkspaceDetailDto {
    full_text: String,
    final_text: String,
    text_source: &'static str,
    cleanup_status: String,
    cleanup_error_code: Option<String>,
    context_layout: &'static str,
    display_items: Vec<crate::composer::TypedTimelineItem>,
    separate_contexts: Vec<crate::composer::TypedTimelineItem>,
    context_degraded: bool,
}

#[derive(Serialize)]
struct CleanupDetailDto {
    original_text: String,
    cleaned_text: Option<String>,
    corrections: Vec<CleanupCorrectionDto>,
    cleanup_elapsed_ms: Option<u64>,
    diagnostics: Option<CleanupDiagnosticsDto>,
}

#[derive(Serialize)]
struct CleanupCorrectionDto {
    original_text: String,
    corrected_text: String,
    kind: &'static str,
}

#[derive(Serialize)]
struct CleanupDiagnosticsDto {
    local_transcription_elapsed_ms: Option<u64>,
    trace_id: String,
    request_started_at_ms: i64,
    response_started_at_ms: Option<i64>,
    response_completed_at_ms: Option<i64>,
    http_status: Option<u32>,
    response_content_type: Option<String>,
    provider_request_id: Option<String>,
    raw_response: Option<String>,
    raw_response_base64: String,
    response_sha256: String,
    response_body_bytes: u64,
    capture_status: &'static str,
}

/// 本地工作台专用详情；root token 才能读取结构化剪贴板上下文。
async fn get_workspace_detail(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path(id): Path<String>,
) -> Result<Json<WorkspaceDetailDto>, AppError> {
    let result = state.sessions().workspace_detail(&caller, &id)?;
    Ok(Json(WorkspaceDetailDto {
        full_text: result.full_text,
        final_text: result.final_text,
        text_source: result.text_source,
        cleanup_status: result.cleanup_status,
        cleanup_error_code: result.cleanup_error_code,
        context_layout: result.context_layout,
        display_items: result.display_items,
        separate_contexts: result.separate_contexts,
        context_degraded: result.context_degraded,
    }))
}

/// 低频、按需读取的 cleanup 处理详情；root token 才能读取 provider 原始响应。
async fn get_cleanup_detail(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path(id): Path<String>,
) -> Result<Json<CleanupDetailDto>, AppError> {
    let result = state.sessions().cleanup_detail(&caller, &id)?;
    Ok(Json(CleanupDetailDto {
        original_text: result.original_text,
        cleaned_text: result.cleaned_text,
        corrections: result
            .corrections
            .into_iter()
            .map(|correction| CleanupCorrectionDto {
                original_text: correction.original_text,
                corrected_text: correction.corrected_text,
                kind: correction.kind,
            })
            .collect(),
        cleanup_elapsed_ms: result.cleanup_elapsed_ms,
        diagnostics: result.diagnostics.map(|diagnostics| CleanupDiagnosticsDto {
            local_transcription_elapsed_ms: diagnostics.local_transcription_elapsed_ms,
            trace_id: diagnostics.trace_id,
            request_started_at_ms: diagnostics.request_started_at_ms,
            response_started_at_ms: diagnostics.response_started_at_ms,
            response_completed_at_ms: diagnostics.response_completed_at_ms,
            http_status: diagnostics.http_status,
            response_content_type: diagnostics.response_content_type,
            provider_request_id: diagnostics.provider_request_id,
            raw_response: diagnostics.raw_response,
            raw_response_base64: diagnostics.raw_response_base64,
            response_sha256: diagnostics.response_sha256,
            response_body_bytes: diagnostics.response_body_bytes,
            capture_status: diagnostics.capture_status,
        }),
    }))
}

/// 内部注入计划读取（`GET /sessions/{id}/injection-plan`）。
///
/// 仅 root token（原生 `DaemonClient` 持有）可达；第三方 token `is_root=false` → 403，
/// WebView 经 `validate_api_request` 白名单隔离（本路径不在白名单）。进程内一次性：
/// 首读消费后返回 plain 全文 + 同位置安全 html，再读 410 Gone。普通 `GET /sessions/{id}`
/// 不暴露 html（`TranscriptDto` 不含 html）。
async fn get_injection_plan(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path(id): Path<String>,
) -> Result<Json<InjectionPlanDto>, AppError> {
    match state.sessions().injection_plan(&caller, &id) {
        Ok(result) => Ok(Json(InjectionPlanDto {
            plain: result.plain,
            html: result.html,
            learning_ticket: result.learning_ticket,
        })),
        Err(crate::application::ApplicationError::Conflict(message))
            if message.contains("already consumed") =>
        {
            Err(AppError::Gone(message))
        }
        Err(error) => Err(error.into()),
    }
}

/// Root-only resource resolver. The caller supplies only logical indexes; the path is
/// re-derived from the encrypted context and never accepted as a request parameter.
async fn resolve_context_resource(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path((id, sequence, index)): Path<(String, u32, usize)>,
) -> Result<Json<ResolvedResourceDto>, AppError> {
    let resource = state
        .sessions()
        .resolve_context_resource(&caller, &id, sequence, index, false)?;
    Ok(Json(ResolvedResourceDto {
        path: resource.path.to_string_lossy().into_owned(),
        kind: resource.kind.into(),
        size: resource.size,
        modified_unix_ms: resource.modified_unix_ms,
        device: resource.device,
        inode: resource.inode,
    }))
}

/// Root-only link resolver. URL is re-derived from encrypted context; callers cannot supply one.
async fn resolve_context_link(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path((id, sequence)): Path<(String, u32)>,
) -> Result<Json<ResolvedLinkDto>, AppError> {
    let url = state
        .sessions()
        .resolve_context_link(&caller, &id, sequence)?;
    Ok(Json(ResolvedLinkDto { url }))
}

async fn get_context_thumbnail(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path((id, sequence, index)): Path<(String, u32, usize)>,
) -> Result<Response, AppError> {
    let started = Instant::now();
    let resource = state
        .sessions()
        .resolve_context_resource(&caller, &id, sequence, index, true)
        .inspect_err(|_| {
            tracing::warn!(
                sequence,
                resource_index = index,
                "thumbnail resource resolution failed"
            );
        })?;
    let path = resource.path;
    let (session_permit, global_permit) = acquire_thumbnail_permits(
        state.thumbnail_session_slots(&id),
        state.thumbnail_slots().clone(),
        THUMBNAIL_WAIT_TIMEOUT,
    )
    .await?;
    let queue_wait_ms = started.elapsed().as_millis() as u64;
    let span = tracing::Span::current();
    let (png, width, height) = tokio::task::spawn_blocking(move || {
        let _entered = span.enter();
        let result = crate::resource::thumbnail_png(&path);
        drop(session_permit);
        drop(global_permit);
        result
    })
    .await
    .map_err(|_| {
        tracing::error!(sequence, resource_index = index, "thumbnail worker failed");
        AppError::Conflict("thumbnail unavailable".into())
    })?
    .map_err(|error| {
        tracing::warn!(sequence, resource_index = index, reason = ?error,
            queue_wait_ms, elapsed_ms = started.elapsed().as_millis() as u64,
            "thumbnail generation failed");
        match error {
            crate::resource::ResourceError::BudgetExceeded => {
                AppError::ThumbnailBudgetExceeded("thumbnail budget exceeded".into())
            }
            crate::resource::ResourceError::Unavailable => {
                AppError::ResourceUnavailable("resource unavailable".into())
            }
            crate::resource::ResourceError::Unsafe => {
                AppError::ResourceUnsafe("resource unsafe".into())
            }
        }
    })?;
    tracing::info!(
        sequence,
        resource_index = index,
        width,
        height,
        output_bytes = png.len(),
        queue_wait_ms,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "thumbnail generated"
    );
    let mut response = Response::new(Body::from(png));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    response.headers_mut().insert(
        "x-thumbnail-width",
        HeaderValue::from_str(&width.to_string()).unwrap(),
    );
    response.headers_mut().insert(
        "x-thumbnail-height",
        HeaderValue::from_str(&height.to_string()).unwrap(),
    );
    Ok(response)
}

/// 删除会话：存储层先删除 DB 行作为提交点，再 best-effort 清理该会话加密文件目录。
async fn delete_session(
    State(_state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    require_scope(&caller, "sessions:delete")?;
    if _state.sessions().delete(&caller, &id)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound(format!("session not found: {id}")))
    }
}

/// `POST /sessions/{id}/retry` — 重试失败会话（ST-M3.7）。
///
/// 仅 status=failed 的会话可重试：对已持久化音频（免重传）重新跑转译链路。
/// 退避重启 sidecar：若活跃 runtime 不健康（崩溃/未启动），stop+start 后再转译
/// （guard 持有期间重启，故并发 retry→409 占用）。重置行→transcribing（清
/// failure_reason/transcript_path/duration），spawn `run_pipeline` 复用，返 202。
/// 404 行不存在；409 非 failed / slot 占用 / 无活跃 runtime / 无持久化音频。
async fn retry_session(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<CreateSessionResp>), AppError> {
    let accepted = state
        .transcription_service()
        .retry(&caller, &id)
        .await
        .map_err(AppError::from)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(CreateSessionResp {
            id: accepted.session_id,
            status: accepted.status,
        }),
    ))
}

/// `GET /sessions/{id}/audio` — 获取原始音频（ST-M3.9，解密后返回二进制）。
///
/// 对已持久化音频（POST /sessions 时 `write_audio` 落库的原始上传字节，AEAD 解密）
/// 原样返回。content-type 由行 `file_name` 扩展名推（wav→audio/wav 等，未知→
/// octet-stream）。错误映射：行不存在 / 无 audio_path / **音频文件缺失 → 404**
/// （reconcile 兜底清孤儿行）；解密失败/损坏（AEAD 认证失败、密钥失配）→ **500**
/// `internal`（真实完整性故障，非「未找到」，与 retry 的 `read_audio ?` 一致，
/// reconcile 只判 `.exists()` 不清此类 zombie）；未解锁 → 423。鉴权 `sessions:read`。
async fn get_session_audio(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    require_scope(&caller, "sessions:read")?;
    let row = state.sessions().get(&caller, &id)?;
    let audio_rel = row
        .audio_path
        .as_ref()
        .ok_or_else(|| AppError::NotFound(format!("no persisted audio for session {id}")))?;
    // 解密读：**文件缺失 → 404**（reconcile 兜底清孤儿行）；其余（`NotUnlocked`→423、
    // `Crypto` 解密失败/损坏 →500、`Storage`/`Proto` →500）走 `From` 默认映射——与
    // retry 的 `read_audio ?` 一致。不把 AEAD 认证失败/密钥失配这类真实完整性故障
    // 吞成 404，否则 corrupt-audio 文件因 reconcile 只判 `.exists()` 而成不可恢复 zombie。
    let bytes = state
        .sessions()
        .read_audio(&caller, audio_rel)
        .map_err(|e| {
            tracing::error!(
                session = %id,
                error_code = "audio_read_integrity_failed",
                "audio read failed (integrity fault)"
            );
            AppError::from(e)
        })?;
    let ct = audio_content_type(row.file_name.as_deref());
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, HeaderValue::from_static(ct))],
        Bytes::from(bytes),
    )
        .into_response())
}

/// 由持久化 `file_name` 扩展名推音频 content-type（GET /sessions/{id}/audio）。
/// 原始音频即上传 multipart audio 字段字节，格式随扩展名；未知扩展名→octet-stream。
fn audio_content_type(file_name: Option<&str>) -> &'static str {
    let ext = file_name
        .and_then(|n| std::path::Path::new(n).extension())
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("wav") => "audio/wav",
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("aac") => "audio/aac",
        Some("flac") => "audio/flac",
        Some("ogg") => "audio/ogg",
        Some("webm") => "audio/webm",
        _ => "application/octet-stream",
    }
}

/// sessions 资源子路由。`DefaultBodyLimit` 限上传体大小（POST /sessions）。
pub fn routes() -> Router<HttpState> {
    Router::new()
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/search", get(search_sessions))
        .route("/sessions/:id", get(get_session).delete(delete_session))
        .route("/sessions/:id/audio", get(get_session_audio))
        .route("/sessions/:id/workspace-detail", get(get_workspace_detail))
        .route("/sessions/:id/cleanup-detail", get(get_cleanup_detail))
        .route("/sessions/:id/retry", post(retry_session))
        .route("/sessions/:id/injection-plan", get(get_injection_plan))
        .route(
            "/sessions/:id/context/:sequence/resources/:index/resolve",
            get(resolve_context_resource),
        )
        .route(
            "/sessions/:id/context/:sequence/link",
            get(resolve_context_link),
        )
        .route(
            "/sessions/:id/context/:sequence/resources/:index/thumbnail",
            get(get_context_thumbnail),
        )
        .layer(DefaultBodyLimit::max(MAX_AUDIO_BYTES))
}
