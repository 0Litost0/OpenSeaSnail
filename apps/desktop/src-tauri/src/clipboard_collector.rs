//! 录音期剪贴板 collector：事件归一、去重与后台轮询。
//!
//! 本模块不在 cpal 回调中读取 Pasteboard。音频回调只递增 `source_frames`；一个独立
//! 的后台线程在录音期以 50ms 轮询 `changeCount`，把每次新变化按优先级归一为单个
//! [`ContextEvent`]，连续相同事件按内容摘要+相邻时间窗去重，图片在后台线程编码
//! 落盘到私有媒体缓存。事件累积成内存中的 [`ClipboardContextFile`]，由 ST-M3.6 提交。
//!
//! 线程安全：poll 线程只锁 [`CollectorShared`] 的 mutex，绝不锁 `RecordingInner`，
//! 因此 `RecordingController::stop()` 持有录制锁调用 `finalize`→join 不会死锁。
//! 文本/HTML/路径/图片字节/摘要一律不入日志，只记录脱敏类型、序号、偏移与错误码。

use std::collections::hash_map::DefaultHasher;
use std::fs::OpenOptions;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use arboard::Clipboard;
use chrono::Utc;
use prost::Message;
use seasnail_daemon::Bootstrap;
use seasnail_proto::seasnail::v1::{ClipboardContextFile, ContextEvent, ContextEventKind};
use uuid::Uuid;

use crate::clipboard_media_cache::MediaCache;
use crate::clipboard_time_anchor::{clamp_sample_offset, PASTEBOARD_POLL_INTERVAL_MS};

/// 镜像 daemon 端 `validate_context_manifest` 的硬限制，原生 fail-fast（ST-M3.3）。
const MAX_CONTEXT_EVENTS: usize = 100;
const MAX_CONTEXT_TEXT_BYTES: usize = 512 * 1024;
const MAX_CONTEXT_HTML_BYTES: usize = 2 * 1024 * 1024;
const MAX_CONTEXT_PATHS: usize = 32;
/// daemon `MAX_CONTEXT_BYTES`：multipart 字段上限，以编码后字节数为准。
const MAX_CONTEXT_BYTES: usize = 4 * 1024 * 1024;
/// 连续相同事件的去重窗口（秒）。覆盖 Cmd-C 抖动与应用双写。
const DEDUP_WINDOW_SECS: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectorPhase {
    Idle,
    Collecting,
    Finalizing,
    Finalized,
}

/// 单次 `changeCount` 归一后的内容。按 files>image>rich_text>plain_text 优先级
/// 在 [`classify_content`] 中确定；图片字节随事件进后台线程，落盘后路径才入 manifest。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizedContent {
    /// 本地文件绝对路径（多文件保序）。
    Files(Vec<String>),
    /// 图片 RGBA8 像素 + 尺寸（落盘后路径进 `absolute_paths`）。
    Image {
        width: usize,
        height: usize,
        rgba: Vec<u8>,
    },
    /// 富文本：纯文本替代 + 原始 HTML（HTML 可空，仅 plain 兜底）。
    RichText { plain: String, html: String },
    /// 纯文本。
    PlainText(String),
}

impl NormalizedContent {
    fn kind(&self) -> ContextEventKind {
        match self {
            Self::Files(_) => ContextEventKind::ContextEventFiles,
            Self::Image { .. } => ContextEventKind::ContextEventImage,
            Self::RichText { .. } => ContextEventKind::ContextEventRichText,
            Self::PlainText(_) => ContextEventKind::ContextEventPlainText,
        }
    }
}

/// 采集失败的脱敏错误码。仅记录类型，绝不携带文本/路径/图片内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectErrorCode {
    EventCountExceeded,
    PlainTextTooLarge,
    HtmlTooLarge,
    PathsTooMany,
    NonAbsolutePath,
    SampleRateOutOfRange,
    ImageWriteFailed,
    EncodedBudgetExceeded,
}

impl CollectErrorCode {
    /// 映射到稳定的非零标签（0 保留为“无错误”），供原子 sink 跨线程传递。
    fn tag(&self) -> u8 {
        match self {
            CollectErrorCode::EventCountExceeded => 1,
            CollectErrorCode::PlainTextTooLarge => 2,
            CollectErrorCode::HtmlTooLarge => 3,
            CollectErrorCode::PathsTooMany => 4,
            CollectErrorCode::NonAbsolutePath => 5,
            CollectErrorCode::SampleRateOutOfRange => 6,
            CollectErrorCode::ImageWriteFailed => 7,
            CollectErrorCode::EncodedBudgetExceeded => 8,
        }
    }
}

/// 标签 → 稳定错误码（无内容）。0 = 无错误。UI 经 `localizedError` 按 `error.<code>`
/// 本地化，不展示原始标签。码与前端 `KNOWN_CODES` 一一对应，不得改名。
fn collect_error_tag_to_label(tag: u8) -> Option<&'static str> {
    match tag {
        0 => None,
        1 => Some("clipboard_overflow"),
        2 => Some("clipboard_text_too_large"),
        3 => Some("clipboard_rich_too_large"),
        4 => Some("clipboard_too_many_files"),
        5 => Some("clipboard_invalid_path"),
        6 => Some("clipboard_sample_rate_out_of_range"),
        7 => Some("clipboard_image_write_failed"),
        8 => Some("clipboard_context_limit"),
        _ => Some("clipboard_collect_error"),
    }
}

/// 胶囊可见的剪贴板采集状态镜像：计数与脱敏错误标签。全部用原子，供 cpal 回调线程
/// 经 `RecordingTelemetry::status` 无锁读取，绝不阻塞音频回调。poll 线程是唯一写者。
pub struct ClipboardContextStatusSink {
    count: AtomicUsize,
    error_tag: AtomicU8,
}

impl ClipboardContextStatusSink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            count: AtomicUsize::new(0),
            error_tag: AtomicU8::new(0),
        })
    }

    /// 新一次录音开始时归零。
    pub fn reset(&self) {
        self.count.store(0, Ordering::Relaxed);
        self.error_tag.store(0, Ordering::Relaxed);
    }

    pub fn bump_count(&self) {
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    /// sticky first-wins：仅当当前无错误时记录首个错误标签。
    pub fn set_error_tag(&self, code: CollectErrorCode) {
        let _ =
            self.error_tag
                .compare_exchange(0, code.tag(), Ordering::Relaxed, Ordering::Relaxed);
    }

    pub fn count(&self) -> usize {
        self.count.load(Ordering::Relaxed)
    }

    pub fn error_label(&self) -> Option<&'static str> {
        collect_error_tag_to_label(self.error_tag.load(Ordering::Relaxed))
    }
}

/// poll 线程与控制面共享的可变状态。所有字段 `Send+Sync`，故 `Arc<Self>` 可跨线程。
struct CollectorShared {
    stop: AtomicBool,
    events: Mutex<Vec<ContextEvent>>,
    error: Mutex<Option<CollectErrorCode>>,
    capture_id: String,
    source_sample_rate: u32,
    source_frames: Arc<AtomicU64>,
    media_cache: Option<MediaCache>,
    account_id: Option<String>,
    last_digest: Mutex<Option<u64>>,
    last_offset: Mutex<u64>,
    /// 胶囊状态镜像；poll 线程写，telemetry 无锁读。
    status_sink: Option<Arc<ClipboardContextStatusSink>>,
    /// ST-M3.4：注入器自写抑制摘要队列。注入器一次注入会有多次剪贴板写入（set_html
    /// 写入 + 恢复原剪贴板），故预注册多个期望摘要；poll_loop 每匹配一个就移除该条，
    /// 仅忽略这些自写，不忽略随后用户复制。
    self_write_filter: Mutex<Vec<u64>>,
    /// 仅开发诊断开关开启时存在。独立文件便于与 daemon 的 slot 决策日志对照；不写正文。
    diagnostic_started_at: Option<Instant>,
    diagnostic_log_path: Option<PathBuf>,
}

impl CollectorShared {
    /// sticky first-wins：首个错误码保留至本次录音结束，并镜像到胶囊 sink。
    fn set_error(&self, code: CollectErrorCode) {
        let mut slot = self.error.lock().expect("collector error mutex");
        if slot.is_none() {
            *slot = Some(code);
            if let Some(sink) = &self.status_sink {
                sink.set_error_tag(code);
            }
        }
    }

    fn log_anchor_diagnostic(&self, phase: &str, change_count: i64, offset: u64, extra: &str) {
        let (Some(started_at), Some(path)) =
            (&self.diagnostic_started_at, &self.diagnostic_log_path)
        else {
            return;
        };
        let elapsed_ms = started_at.elapsed().as_millis();
        let line = format!(
            "{} clipboard-anchor phase={} elapsed_ms={} change_count={} sample_offset={} source_sample_rate={} {}\n",
            Utc::now().to_rfc3339(),
            phase,
            elapsed_ms,
            change_count,
            offset,
            self.source_sample_rate,
            extra,
        );
        // 诊断不可影响录音/采集。路径固定在 app data dir，内容不含剪贴板或转写原文。
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

fn anchor_diagnostics_path() -> Option<PathBuf> {
    (std::env::var("SEASNAIL_ANCHOR_DIAGNOSTICS").as_deref() == Ok("1")).then(|| {
        Bootstrap::default_dir()
            .ok()
            .map(|dir| dir.join("logs/clipboard-anchor.log"))
    })?
}

/// 单次录制的 collector 控制面。所有状态变换由 `RecordingController` 的同一把录制锁
/// 串行化；音频回调只持有 `source_frames` 原子计数。
pub struct ClipboardCollector {
    phase: CollectorPhase,
    enabled: bool,
    source_sample_rate: Option<u32>,
    source_frames: Option<Arc<AtomicU64>>,
    final_frame_count: Option<u64>,
    /// 录音开始时读到的 pasteboard `changeCount` 基线。poll 线程以
    /// `当前 changeCount > 基线` 判定“本次录音期间出现的新复制”，故基线必须在
    /// `start` 时一次读定。仅 `enabled` 时读取；不读取基线剪贴板正文。
    baseline_change_count: Option<i64>,
    /// poll 线程的可变状态；`Collecting` 时存在。
    shared: Option<Arc<CollectorShared>>,
    /// poll 线程句柄；`finalize`/`reset` 经 [`shutdown_thread`] join 回收。
    join: Option<JoinHandle<()>>,
    /// `take_manifest` 一次性标记：取过一次后再次调用返回 `None`，避免 M3.6 双取拿到空 manifest。
    manifest_taken: bool,
}

impl ClipboardCollector {
    pub const fn new() -> Self {
        Self {
            phase: CollectorPhase::Idle,
            enabled: false,
            source_sample_rate: None,
            source_frames: None,
            final_frame_count: None,
            baseline_change_count: None,
            shared: None,
            join: None,
            manifest_taken: false,
        }
    }

    /// 生产入口。仅在 `enabled=true` 且调用方已成功启动音频流后进入 `Collecting`
    /// 并 spawn poll 线程；关闭时保持 `Idle`，不读 pasteboard、不起线程。
    /// `account_id`/`media_cache` 仅图片事件需要；缺省时图片降级（错误码 + 跳过）。
    pub fn start(
        &mut self,
        enabled: bool,
        source_sample_rate: u32,
        source_frames: Arc<AtomicU64>,
        account_id: Option<String>,
        media_cache: Option<MediaCache>,
        status_sink: Option<Arc<ClipboardContextStatusSink>>,
    ) {
        self.start_with_change_count(
            enabled,
            source_sample_rate,
            source_frames,
            account_id,
            media_cache,
            status_sink,
            Box::new(crate::platform::clipboard::pasteboard_change_count),
        );
    }

    /// 测试入口：注入受控 changeCount 源，避免真实 pasteboard 抖动导致线程测试 flaky。
    /// 生产经由 [`start`]，此方法仅供单测注入返回固定值的计数器。
    fn start_with_change_count(
        &mut self,
        enabled: bool,
        source_sample_rate: u32,
        source_frames: Arc<AtomicU64>,
        account_id: Option<String>,
        media_cache: Option<MediaCache>,
        status_sink: Option<Arc<ClipboardContextStatusSink>>,
        change_count: Box<dyn Fn() -> i64 + Send + Sync>,
    ) {
        // 若上一轮异常残留活动线程，先回收（幂等）。
        self.shutdown_thread();
        // 无论本次是否启用，都先归零胶囊 sink：关闭态录制不得继承上一会话的计数。
        if let Some(sink) = &status_sink {
            sink.reset();
        }
        self.phase = if enabled {
            CollectorPhase::Collecting
        } else {
            CollectorPhase::Idle
        };
        self.enabled = enabled;
        self.source_sample_rate = Some(source_sample_rate);
        self.source_frames = Some(source_frames.clone());
        self.final_frame_count = None;
        if !enabled {
            self.shared = None;
            self.baseline_change_count = None;
            return;
        }
        let baseline = change_count();
        self.baseline_change_count = Some(baseline);
        let diagnostic_log_path = anchor_diagnostics_path();
        let shared = Arc::new(CollectorShared {
            stop: AtomicBool::new(false),
            events: Mutex::new(Vec::new()),
            error: Mutex::new(None),
            capture_id: Uuid::new_v4().to_string(),
            source_sample_rate,
            source_frames,
            media_cache,
            account_id,
            last_digest: Mutex::new(None),
            last_offset: Mutex::new(0),
            status_sink,
            self_write_filter: Mutex::new(Vec::new()),
            diagnostic_started_at: diagnostic_log_path.as_ref().map(|_| Instant::now()),
            diagnostic_log_path,
        });
        let handle = thread::Builder::new()
            .name("clipboard-collector".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || poll_loop(shared, baseline, change_count)
            })
            .expect("spawn clipboard collector thread");
        self.shared = Some(shared);
        self.join = Some(handle);
    }

    /// 停止的线性化点：拒绝新 `changeCount`。调用方随后才停音频流，以让在途回调收尾。
    pub fn freeze(&mut self) {
        self.phase = CollectorPhase::Finalizing;
        if let Some(shared) = &self.shared {
            shared.stop.store(true, Ordering::Release);
        }
    }

    /// 音频流停止后的最终提交：join poll 线程，并把所有事件偏移钳制到末帧以内，
    /// 避免停止竞争中读到的未来帧数越界。
    pub fn finalize(&mut self, final_frame_count: u64) {
        self.phase = CollectorPhase::Finalized;
        self.final_frame_count = Some(final_frame_count);
        self.shutdown_thread();
        if let Some(shared) = &self.shared {
            if let Ok(mut events) = shared.events.lock() {
                for event in events.iter_mut() {
                    event.sample_offset =
                        clamp_sample_offset(event.sample_offset, final_frame_count);
                }
            }
        }
    }

    /// 取出本次录音的 manifest（Finalized 后、reset 前）。M3.6 提交；M3.3 测试直接调。
    /// 重新按 `index+1` 编排 `sequence`（`sequence==index+1` 不变量的唯一真源），并以
    /// 编码后字节数校验 4MiB 总预算（field-sum 会漏 proto varint 开销，故必须编码后测）。
    pub fn take_manifest(&mut self) -> Option<ClipboardContextFile> {
        if self.phase != CollectorPhase::Finalized || self.shared.is_none() || self.manifest_taken {
            return None;
        }
        self.manifest_taken = true;
        let shared = self
            .shared
            .as_ref()
            .expect("finalized collector has shared state");
        let mut events = shared
            .events
            .lock()
            .expect("collector events mutex")
            .clone();
        if events.len() > MAX_CONTEXT_EVENTS {
            events.truncate(MAX_CONTEXT_EVENTS);
            shared.set_error(CollectErrorCode::EventCountExceeded);
        }
        for (index, event) in events.iter_mut().enumerate() {
            event.sequence = (index + 1) as u32;
        }
        let manifest = ClipboardContextFile {
            schema_version: 1,
            session_id: String::new(), // daemon 分配，原生留空。
            capture_id: shared.capture_id.clone(),
            events,
        };
        // 以编码后字节数为准，超限时从尾部丢弃事件直至达标（不 panic）。
        let mut manifest = manifest;
        while manifest.encode_to_vec().len() > MAX_CONTEXT_BYTES {
            if manifest.events.pop().is_none() {
                break;
            }
            shared.set_error(CollectErrorCode::EncodedBudgetExceeded);
        }
        if let Ok(mut slot) = shared.events.lock() {
            slot.clear();
        }
        Some(manifest)
    }

    pub fn reset(&mut self) {
        // start 恢复路径会对一个尚活线程的 Collecting collector 调 reset；
        // 必须先 join 回收，否则 detach JoinHandle → 线程泄漏 + 永久轮询。
        self.shutdown_thread();
        self.phase = CollectorPhase::Idle;
        self.enabled = false;
        self.source_sample_rate = None;
        self.source_frames = None;
        self.final_frame_count = None;
        self.baseline_change_count = None;
        self.shared = None;
        self.manifest_taken = false;
    }

    /// 幂等回收 poll 线程：置 stop 后 join。`finalize` 与 `reset`（含 start 恢复路径）均调。
    fn shutdown_thread(&mut self) {
        if let Some(shared) = &self.shared {
            shared.stop.store(true, Ordering::Release);
        }
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }

    /// 已捕获事件数（供 ST-M3.5 胶囊显示）。
    #[cfg(test)]
    pub fn context_count(&self) -> usize {
        self.shared
            .as_ref()
            .and_then(|s| s.events.lock().ok())
            .map(|e| e.len())
            .unwrap_or(0)
    }

    /// 采集错误码（供 ST-M3.5 胶囊显示，不含内容）。
    #[cfg(test)]
    pub fn context_error_code(&self) -> Option<CollectErrorCode> {
        self.shared
            .as_ref()
            .and_then(|s| s.error.lock().ok().and_then(|e| *e))
    }

    #[cfg(test)]
    pub const fn phase(&self) -> CollectorPhase {
        self.phase
    }

    #[cfg(test)]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    #[cfg(test)]
    pub fn source_sample_rate(&self) -> Option<u32> {
        self.source_sample_rate
    }

    /// 返回 `start` 时读到的 pasteboard `changeCount` 基线；未启用或未开始时为 `None`。
    #[cfg(test)]
    pub fn baseline_change_count(&self) -> Option<i64> {
        self.baseline_change_count
    }

    /// 返回当前源帧偏移（M3.2 遗留访问器，仅在 `Collecting` 时有意义；事件级偏移见
    /// manifest）。
    #[cfg(test)]
    pub fn observed_sample_offset(&self) -> Option<u64> {
        (self.phase == CollectorPhase::Collecting && self.enabled)
            .then(|| {
                self.source_frames
                    .as_ref()
                    .map(|frames| frames.load(Ordering::Acquire))
            })
            .flatten()
            .map(|offset| {
                self.final_frame_count
                    .map(|last| clamp_sample_offset(offset, last))
                    .unwrap_or(offset)
            })
    }

    /// ST-M3.4：注册一个待抑制的自写摘要。注入器一次注入含多次剪贴板写入
    ///（set_html 写入 + 恢复原剪贴板），每次写入前调用本方法注册对应摘要；
    /// poll_loop 每匹配一个就移除该条。仅在 `Collecting`（有活动录音）时生效；
    /// 无活动录音 → 返回 false（无重叠无需抑制）。
    #[cfg(test)]
    pub fn register_self_write(&self, digest: u64) -> bool {
        if self.phase != CollectorPhase::Collecting {
            return false;
        }
        if let Some(shared) = &self.shared {
            shared
                .self_write_filter
                .lock()
                .expect("self-write filter mutex")
                .push(digest);
            true
        } else {
            false
        }
    }
}

// ── 纯函数（不碰真实 pasteboard，全部可单测）────────────────────────────────────

/// 按 files>image>rich_text>plain_text 优先级归一。同一 changeCount 至多产一个事件。
/// `files` 应已是绝对路径（由调用方从 arboard file_list 取得）；`html`/`plain` 可空。
pub fn classify_content(
    files: Vec<String>,
    image: Option<(usize, usize, Vec<u8>)>,
    html: String,
    plain: String,
) -> Option<NormalizedContent> {
    if !files.is_empty() {
        return Some(NormalizedContent::Files(files));
    }
    if let Some((width, height, rgba)) = image {
        return Some(NormalizedContent::Image {
            width,
            height,
            rgba,
        });
    }
    if !html.is_empty() && !plain.is_empty() {
        return Some(NormalizedContent::RichText { plain, html });
    }
    if !plain.is_empty() {
        return Some(NormalizedContent::PlainText(plain));
    }
    None
}

/// 镜像 `validate_context_manifest` 的每事件约束。返回 `(Option<事件>, Option<错误码>)`：
/// 超限且无法保留 → `(None, code)`（丢该事件）；可截断保留（paths>32）→ `(Some(..), code)`。
/// 不赋 `sequence`（由 [`ClipboardCollector::take_manifest`] 统一按 `index+1` 编排）。
pub fn build_event(
    source_sample_rate: u32,
    sample_offset: u64,
    normalized: &NormalizedContent,
) -> (Option<ContextEvent>, Option<CollectErrorCode>) {
    if !(8_000..=192_000).contains(&source_sample_rate) {
        return (None, Some(CollectErrorCode::SampleRateOutOfRange));
    }
    let kind = normalized.kind();
    match normalized {
        NormalizedContent::Files(paths) => {
            if paths.iter().any(|p| !Path::new(p).is_absolute()) {
                return (None, Some(CollectErrorCode::NonAbsolutePath));
            }
            let mut paths = paths.clone();
            let mut err = None;
            if paths.len() > MAX_CONTEXT_PATHS {
                paths.truncate(MAX_CONTEXT_PATHS);
                err = Some(CollectErrorCode::PathsTooMany);
            }
            (
                Some(ContextEvent {
                    sequence: 0,
                    source_sample_rate,
                    sample_offset,
                    kind: kind as i32,
                    plain_text: String::new(),
                    html_fragment: String::new(),
                    absolute_paths: paths,
                }),
                err,
            )
        }
        NormalizedContent::Image { .. } => {
            // 路径由 poll_loop 落盘后填入；此处先空，validator 要求非空由落盘结果保证。
            (
                Some(ContextEvent {
                    sequence: 0,
                    source_sample_rate,
                    sample_offset,
                    kind: kind as i32,
                    plain_text: String::new(),
                    html_fragment: String::new(),
                    absolute_paths: Vec::new(),
                }),
                None,
            )
        }
        NormalizedContent::RichText { plain, html } => {
            if plain.len() > MAX_CONTEXT_TEXT_BYTES {
                return (None, Some(CollectErrorCode::PlainTextTooLarge));
            }
            if html.len() > MAX_CONTEXT_HTML_BYTES {
                return (None, Some(CollectErrorCode::HtmlTooLarge));
            }
            (
                Some(ContextEvent {
                    sequence: 0,
                    source_sample_rate,
                    sample_offset,
                    kind: kind as i32,
                    plain_text: plain.clone(),
                    html_fragment: html.clone(),
                    absolute_paths: Vec::new(),
                }),
                None,
            )
        }
        NormalizedContent::PlainText(text) => {
            if text.len() > MAX_CONTEXT_TEXT_BYTES {
                return (None, Some(CollectErrorCode::PlainTextTooLarge));
            }
            (
                Some(ContextEvent {
                    sequence: 0,
                    source_sample_rate,
                    sample_offset,
                    kind: kind as i32,
                    plain_text: text.clone(),
                    html_fragment: String::new(),
                    absolute_paths: Vec::new(),
                }),
                None,
            )
        }
    }
}

/// 内容摘要：仅内存用于去重比较，绝不入日志或持久化。对 `Image` 哈希像素尺寸与字节，
/// 否则不同图片的摘要会相同，导致窗内误去重（违反“仅相同事件去重”契约）。
pub fn content_digest(content: &NormalizedContent) -> u64 {
    let mut hasher = DefaultHasher::new();
    (content.kind() as i32).hash(&mut hasher);
    match content {
        NormalizedContent::Files(paths) => {
            paths.len().hash(&mut hasher);
            for path in paths {
                path.hash(&mut hasher);
            }
        }
        NormalizedContent::Image {
            width,
            height,
            rgba,
        } => {
            width.hash(&mut hasher);
            height.hash(&mut hasher);
            rgba.hash(&mut hasher);
        }
        NormalizedContent::RichText { plain, html } => {
            plain.hash(&mut hasher);
            html.hash(&mut hasher);
        }
        NormalizedContent::PlainText(text) => {
            text.hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// 仅当新事件与**上一条已提交**事件摘要相同、且偏移落在窗口内时才抑制（连续相同去重）。
/// 间夹不同事件或窗外重复都不抑制；被丢事件不更新 last 状态（由调用方在提交后更新）。
pub fn should_dedup(
    new_digest: u64,
    last_digest: Option<u64>,
    new_offset: u64,
    last_offset: u64,
    window_frames: u64,
) -> bool {
    match last_digest {
        Some(last) if last == new_digest => new_offset.saturating_sub(last_offset) <= window_frames,
        _ => false,
    }
}

/// ST-M3.4 纯函数：在待抑制摘要队列中查找检测到的剪贴板摘要。返回首个匹配下标
///（由 poll_loop `swap_remove` 移除该条）；不匹配 → `None`，保留全部注册等下次
///（intervening 用户复制摘要不同，不应消耗注册）。
pub fn self_write_matches(digest: u64, filter: &[u64]) -> Option<usize> {
    filter.iter().position(|expected| *expected == digest)
}

/// 读取一次剪贴板内容（files/image/html/plain）。每次新建 `Clipboard`，不跨线程存
/// `Retained<NSPasteboard>`。仅在 poll 线程检测到新 changeCount 时调用。
fn read_clipboard() -> (Vec<String>, Option<(usize, usize, Vec<u8>)>, String, String) {
    let mut clipboard = match Clipboard::new() {
        Ok(clipboard) => clipboard,
        Err(_) => return (Vec::new(), None, String::new(), String::new()),
    };
    let files = clipboard
        .get()
        .file_list()
        .map(|paths| {
            paths
                .into_iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    let image = clipboard
        .get_image()
        .ok()
        .map(|image| (image.width, image.height, image.bytes.as_ref().to_vec()));
    let html = clipboard.get().html().unwrap_or_default();
    let plain = clipboard.get_text().ok().unwrap_or_default();
    (files, image, html, plain)
}

/// 后台轮询线程。只锁 `shared` 的 mutex，绝不锁 `RecordingInner`；故 `stop()` 持锁
/// 调 `finalize`→join 不会死锁。`baseline` 为 `start` 时读定的基线，避免漏掉 start
/// 与首轮之间的变化。
fn poll_loop(
    shared: Arc<CollectorShared>,
    mut running_baseline: i64,
    change_count: Box<dyn Fn() -> i64 + Send + Sync>,
) {
    let poll = Duration::from_millis(PASTEBOARD_POLL_INTERVAL_MS);
    let window_frames = shared.source_sample_rate as u64 * DEDUP_WINDOW_SECS;
    loop {
        if shared.stop.load(Ordering::Acquire) {
            break;
        }
        thread::sleep(poll);
        if shared.stop.load(Ordering::Acquire) {
            break; // 睡后复检，freeze 快速退出。
        }
        let current = change_count();
        if current <= running_baseline {
            continue;
        }
        running_baseline = current; // 单写者，无双计/漏计。
        let offset = shared.source_frames.load(Ordering::Acquire); // 检测瞬时；TOCTOU 由 finalize 钳制兜底。
        shared.log_anchor_diagnostic("change_detected", current, offset, "");

        let (files, image, html, plain) = read_clipboard();
        let Some(normalized) = classify_content(files, image, html, plain) else {
            shared.log_anchor_diagnostic("empty_or_unreadable", current, offset, "");
            continue;
        };
        let digest = content_digest(&normalized);
        // ST-M3.4 自写抑制：注入器预注册的自写摘要。摘要匹配 → 跳过该事件
        //（running_baseline 已在上方推进），清注册；不匹配则保留注册（intervening
        // 用户复制摘要不同，不应消耗注册）。仅忽略这一次自写，不忽略随后用户复制。
        let self_write_suppressed = {
            let mut filter = shared
                .self_write_filter
                .lock()
                .expect("self-write filter mutex");
            if let Some(idx) = self_write_matches(digest, &filter) {
                filter.swap_remove(idx);
                true
            } else {
                false
            }
        };
        if self_write_suppressed {
            shared.log_anchor_diagnostic("self_write_suppressed", current, offset, "");
            *shared.last_digest.lock().expect("collector digest mutex") = Some(digest);
            *shared.last_offset.lock().expect("collector offset mutex") = offset;
            continue;
        }
        let (last_digest, last_offset) = {
            let ld = shared.last_digest.lock().expect("collector digest mutex");
            let lo = shared.last_offset.lock().expect("collector offset mutex");
            (*ld, *lo)
        };
        if should_dedup(digest, last_digest, offset, last_offset, window_frames) {
            shared.log_anchor_diagnostic("deduplicated", current, offset, "");
            continue;
        }
        {
            let events = shared.events.lock().expect("collector events mutex");
            if events.len() >= MAX_CONTEXT_EVENTS {
                drop(events);
                shared.set_error(CollectErrorCode::EventCountExceeded);
                shared.log_anchor_diagnostic("event_limit_rejected", current, offset, "");
                continue;
            }
        }
        let (maybe_event, err) = build_event(shared.source_sample_rate, offset, &normalized);
        if let Some(code) = err {
            shared.set_error(code);
            shared.log_anchor_diagnostic("normalized_with_error", current, offset, "");
        }
        let Some(mut event) = maybe_event else {
            shared.log_anchor_diagnostic("normalization_rejected", current, offset, "");
            continue;
        };
        // 图片：后台线程编码落盘，成功后填绝对路径；失败则丢该事件（不阻塞录音）。
        if let NormalizedContent::Image {
            width,
            height,
            rgba,
        } = &normalized
        {
            match (shared.media_cache.as_ref(), shared.account_id.as_deref()) {
                (Some(cache), Some(account_id)) => {
                    match cache.write_rgba_png(
                        account_id,
                        &shared.capture_id,
                        Utc::now(),
                        &Uuid::new_v4().to_string(),
                        *width,
                        *height,
                        rgba,
                    ) {
                        Ok(path) => event.absolute_paths = vec![path.to_string_lossy().to_string()],
                        Err(_) => {
                            shared.set_error(CollectErrorCode::ImageWriteFailed);
                            continue;
                        }
                    }
                }
                _ => {
                    shared.set_error(CollectErrorCode::ImageWriteFailed);
                    continue;
                }
            }
        }
        {
            let mut events = shared.events.lock().expect("collector events mutex");
            let provisional_event_index = events.len() + 1;
            shared.log_anchor_diagnostic(
                "accepted",
                current,
                offset,
                &format!(
                    "provisional_event_index={provisional_event_index} event_kind={}",
                    event.kind
                ),
            );
            events.push(event);
        }
        if let Some(sink) = &shared.status_sink {
            sink.bump_count();
        }
        // 提交后才更新 dedup 状态，避免被 dedup 又被丢的事件污染窗口。
        *shared.last_digest.lock().expect("collector digest mutex") = Some(digest);
        *shared.last_offset.lock().expect("collector offset mutex") = offset;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;

    /// 注入恒为 `seed` 的 changeCount：永不产生“新变化”，故 poll 线程空转无事件，
    /// 测试不依赖真实 pasteboard，跨平台且不 flaky。
    fn static_change_count(seed: i64) -> Box<dyn Fn() -> i64 + Send + Sync> {
        Box::new(move || seed)
    }

    #[test]
    fn status_sink_counts_bumps_and_sticky_first_error() {
        let sink = ClipboardContextStatusSink::new();
        assert_eq!(sink.count(), 0);
        assert!(sink.error_label().is_none());
        sink.bump_count();
        sink.bump_count();
        assert_eq!(sink.count(), 2);
        sink.set_error_tag(CollectErrorCode::ImageWriteFailed);
        assert_eq!(sink.error_label(), Some("clipboard_image_write_failed"));
        // sticky first-wins：后续错误不覆盖首错。
        sink.set_error_tag(CollectErrorCode::EventCountExceeded);
        assert_eq!(sink.error_label(), Some("clipboard_image_write_failed"));
        // reset 归零，供下一次录音复用。
        sink.reset();
        assert_eq!(sink.count(), 0);
        assert!(sink.error_label().is_none());
    }

    #[test]
    fn disabled_start_still_resets_the_status_sink() {
        // 关闭态录制不得继承上一会话的计数：start 须在 enabled 守卫之前归零 sink。
        let sink = ClipboardContextStatusSink::new();
        sink.bump_count();
        sink.set_error_tag(CollectErrorCode::ImageWriteFailed);
        assert_eq!(sink.count(), 1);
        assert!(sink.error_label().is_some());
        let frames = Arc::new(AtomicU64::new(0));
        let mut collector = ClipboardCollector::new();
        collector.start_with_change_count(
            false,
            48_000,
            frames,
            None,
            None,
            Some(sink.clone()),
            static_change_count(0),
        );
        assert_eq!(collector.phase(), CollectorPhase::Idle);
        assert_eq!(sink.count(), 0);
        assert!(sink.error_label().is_none());
    }

    // ── M3.2 生命周期（保持绿色，start 已 3→5 参，改用注入入口）─────────────────

    #[test]
    fn collector_freezes_before_audio_stop_and_exposes_no_new_offsets_afterward() {
        let frames = Arc::new(AtomicU64::new(48_000));
        let mut collector = ClipboardCollector::new();
        collector.start_with_change_count(
            true,
            48_000,
            frames.clone(),
            None,
            None,
            None,
            static_change_count(0),
        );
        assert_eq!(collector.phase(), CollectorPhase::Collecting);
        assert_eq!(collector.observed_sample_offset(), Some(48_000));

        collector.freeze();
        assert_eq!(collector.phase(), CollectorPhase::Finalizing);
        frames.store(48_120, Ordering::Release); // 模拟与 stop 竞争的晚到 callback。
        collector.finalize(frames.load(Ordering::Acquire));
        assert_eq!(collector.phase(), CollectorPhase::Finalized);
        assert_eq!(collector.observed_sample_offset(), None);
    }

    #[test]
    fn disabled_collector_never_exposes_a_pasteboard_anchor() {
        let frames = Arc::new(AtomicU64::new(1));
        let mut collector = ClipboardCollector::new();
        collector.start_with_change_count(
            false,
            16_000,
            frames,
            None,
            None,
            None,
            static_change_count(0),
        );
        assert_eq!(collector.phase(), CollectorPhase::Idle);
        assert!(!collector.enabled());
        assert_eq!(collector.observed_sample_offset(), None);
        assert_eq!(collector.baseline_change_count(), None);
        assert_eq!(collector.source_sample_rate(), Some(16_000));
    }

    #[test]
    fn enabled_collector_reads_pasteboard_baseline_once() {
        let frames = Arc::new(AtomicU64::new(0));
        let mut collector = ClipboardCollector::new();
        collector.start_with_change_count(
            true,
            48_000,
            frames,
            None,
            None,
            None,
            static_change_count(42),
        );
        assert_eq!(collector.phase(), CollectorPhase::Collecting);
        assert_eq!(collector.source_sample_rate(), Some(48_000));
        // start 一次读定基线（取自注入源）；不读正文。
        assert_eq!(collector.baseline_change_count(), Some(42));
        collector.reset(); // 回收 poll 线程，避免泄漏。
    }

    // ── ST-M3.3 纯函数 ───────────────────────────────────────────────────────

    #[test]
    fn classify_content_priority_files_then_image_then_rich_then_plain() {
        // files 优先于其它。
        let n = classify_content(vec!["/a".into()], None, "<b>x</b>".into(), "x".into());
        assert!(matches!(n, Some(NormalizedContent::Files(_))));
        // image 优先于 rich/plain。
        let n = classify_content(
            Vec::new(),
            Some((1, 1, vec![0; 4])),
            "<b>x</b>".into(),
            "x".into(),
        );
        assert!(matches!(n, Some(NormalizedContent::Image { .. })));
        // rich（需 html + plain 同时非空）优先于 plain。
        let n = classify_content(Vec::new(), None, "<b>x</b>".into(), "x".into());
        assert!(matches!(n, Some(NormalizedContent::RichText { .. })));
        // 只有 plain。
        let n = classify_content(Vec::new(), None, String::new(), "x".into());
        assert!(matches!(n, Some(NormalizedContent::PlainText(_))));
        // html 在但 plain 缺失 → 不归一为 rich（validator 要求 plain 非空），也不退回 plain。
        let n = classify_content(Vec::new(), None, "<b>x</b>".into(), String::new());
        assert!(n.is_none());
        // 全空 → None。
        assert!(classify_content(Vec::new(), None, String::new(), String::new()).is_none());
    }

    #[test]
    fn build_event_constructs_each_kind_and_enforces_limits() {
        // FILES：多文件保序、字段一致。
        let (ev, err) = build_event(
            48_000,
            960,
            &NormalizedContent::Files(vec!["/a".into(), "/b".into()]),
        );
        let ev = ev.expect("files event");
        assert_eq!(ev.absolute_paths, vec!["/a".to_string(), "/b".to_string()]);
        assert!(ev.plain_text.is_empty() && ev.html_fragment.is_empty());
        assert_eq!(ev.kind, ContextEventKind::ContextEventFiles as i32);
        assert!(err.is_none());

        // FILES >32 → 截断至 32 + PathsTooMany。
        let big: Vec<String> = (0..40).map(|i| format!("/f{i}")).collect();
        let (ev, err) = build_event(48_000, 0, &NormalizedContent::Files(big));
        assert_eq!(ev.unwrap().absolute_paths.len(), MAX_CONTEXT_PATHS);
        assert_eq!(err, Some(CollectErrorCode::PathsTooMany));

        // 非绝对路径 → 丢整事件。
        let (ev, err) = build_event(
            48_000,
            0,
            &NormalizedContent::Files(vec!["rel/path".into()]),
        );
        assert!(ev.is_none());
        assert_eq!(err, Some(CollectErrorCode::NonAbsolutePath));

        // RICH_TEXT：plain + html，无 paths。
        let (ev, err) = build_event(
            48_000,
            0,
            &NormalizedContent::RichText {
                plain: "x".into(),
                html: "<b>x</b>".into(),
            },
        );
        let ev = ev.unwrap();
        assert_eq!(
            (ev.plain_text, ev.html_fragment),
            ("x".to_string(), "<b>x</b>".to_string())
        );
        assert!(ev.absolute_paths.is_empty() && err.is_none());

        // PLAIN_TEXT 过大 → 丢弃。
        let huge = "a".repeat(MAX_CONTEXT_TEXT_BYTES + 1);
        let (ev, err) = build_event(48_000, 0, &NormalizedContent::PlainText(huge));
        assert!(ev.is_none());
        assert_eq!(err, Some(CollectErrorCode::PlainTextTooLarge));

        // HTML 过大 → 丢弃。
        let huge_html = "a".repeat(MAX_CONTEXT_HTML_BYTES + 1);
        let (ev, err) = build_event(
            48_000,
            0,
            &NormalizedContent::RichText {
                plain: "x".into(),
                html: huge_html,
            },
        );
        assert!(ev.is_none());
        assert_eq!(err, Some(CollectErrorCode::HtmlTooLarge));

        // 采样率越界 → 丢弃。
        let (ev, err) = build_event(7_999, 0, &NormalizedContent::PlainText("x".into()));
        assert!(ev.is_none() && err == Some(CollectErrorCode::SampleRateOutOfRange));

        // sequence 一律不赋（由 take_manifest 编排）。
        let (ev, _) = build_event(48_000, 0, &NormalizedContent::PlainText("x".into()));
        assert_eq!(ev.unwrap().sequence, 0);
    }

    #[test]
    fn dedup_suppresses_only_consecutive_identical_within_window() {
        let d = content_digest(&NormalizedContent::PlainText("x".into()));
        let other = content_digest(&NormalizedContent::PlainText("y".into()));
        let window = 48_000; // 1s @48kHz
                             // 连续相同、窗内 → 抑制。
        assert!(should_dedup(d, Some(d), 48_000, 0, window));
        // 相同但窗外 → 不抑制。
        assert!(!should_dedup(d, Some(d), 48_001 + window, 0, window));
        // 上一条不同 → 不抑制（即使窗内）。
        assert!(!should_dedup(d, Some(other), 1, 0, window));
        // 首条（last=None）→ 不抑制。
        assert!(!should_dedup(d, None, 0, 0, window));
    }

    // ── ST-M3.3 线程生命周期 + manifest ────────────────────────────────────────

    #[test]
    fn take_manifest_returns_valid_empty_manifest_after_finalize() {
        let frames = Arc::new(AtomicU64::new(0));
        let mut collector = ClipboardCollector::new();
        collector.start_with_change_count(
            true,
            48_000,
            frames,
            None,
            None,
            None,
            static_change_count(0),
        );
        collector.freeze();
        collector.finalize(0);
        let manifest = collector.take_manifest().expect("manifest after finalize");
        assert_eq!(manifest.schema_version, 1);
        assert!(manifest.session_id.is_empty());
        assert!(uuid::Uuid::parse_str(&manifest.capture_id).is_ok());
        assert!(manifest.events.is_empty());
        assert!(manifest.encode_to_vec().len() <= MAX_CONTEXT_BYTES);
        assert_eq!(collector.context_count(), 0);
        assert_eq!(collector.context_error_code(), None);
    }

    #[test]
    fn reset_without_finalize_joins_the_live_poll_thread() {
        // 模拟 start 恢复路径：Collecting 中直接 reset，不得 detach 线程。
        let frames = Arc::new(AtomicU64::new(0));
        let mut collector = ClipboardCollector::new();
        collector.start_with_change_count(
            true,
            48_000,
            frames,
            None,
            None,
            None,
            static_change_count(0),
        );
        assert_eq!(collector.phase(), CollectorPhase::Collecting);
        collector.reset(); // 必须回收线程而非 detach。
        assert_eq!(collector.phase(), CollectorPhase::Idle);
        assert!(collector.shared.is_none());
        assert_eq!(collector.context_count(), 0);
    }

    /// 直接构造 `CollectorShared`，绕过 poll loop 测 take_manifest 的重排/预算/截断逻辑。
    fn shared_with(events: Vec<ContextEvent>) -> Arc<CollectorShared> {
        Arc::new(CollectorShared {
            stop: AtomicBool::new(true),
            events: Mutex::new(events),
            error: Mutex::new(None),
            capture_id: Uuid::new_v4().to_string(),
            source_sample_rate: 48_000,
            source_frames: Arc::new(AtomicU64::new(0)),
            media_cache: None,
            account_id: None,
            last_digest: Mutex::new(None),
            last_offset: Mutex::new(0),
            status_sink: None,
            self_write_filter: Mutex::new(Vec::new()),
            diagnostic_started_at: None,
            diagnostic_log_path: None,
        })
    }

    fn plain_event(seq_zero: u32, offset: u64, text: &str) -> ContextEvent {
        ContextEvent {
            sequence: seq_zero,
            source_sample_rate: 48_000,
            sample_offset: offset,
            kind: ContextEventKind::ContextEventPlainText as i32,
            plain_text: text.into(),
            html_fragment: String::new(),
            absolute_paths: Vec::new(),
        }
    }

    #[test]
    fn take_manifest_renumbers_sequence_and_is_one_shot() {
        let mut collector = ClipboardCollector::new();
        let events: Vec<ContextEvent> = (0..3)
            .map(|i| plain_event(0, (i as u64) * 48_000, &format!("e{i}")))
            .collect();
        collector.phase = CollectorPhase::Finalized;
        collector.shared = Some(shared_with(events));
        let manifest = collector.take_manifest().expect("manifest");
        assert_eq!(
            manifest
                .events
                .iter()
                .map(|e| e.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(manifest.encode_to_vec().len() <= MAX_CONTEXT_BYTES);
        // take 一次性：事件已清空（计数归零），再取返回 None。
        assert_eq!(collector.context_count(), 0);
        assert!(collector.take_manifest().is_none());
    }

    #[test]
    fn distinct_images_have_distinct_digests_so_different_images_are_not_deduped() {
        // 修复点：图片摘要必须哈希像素，否则不同图片摘要相同 → 窗内误去重。
        let a = content_digest(&NormalizedContent::Image {
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0, 255],
        });
        let b = content_digest(&NormalizedContent::Image {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 255],
        });
        assert_ne!(a, b, "different images must hash differently");
        let same = content_digest(&NormalizedContent::Image {
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0, 255],
        });
        assert_eq!(a, same, "identical images must hash equally");
    }

    #[test]
    fn take_manifest_truncates_events_above_max_count() {
        let mut collector = ClipboardCollector::new();
        let events: Vec<ContextEvent> = (0..(MAX_CONTEXT_EVENTS + 5) as u32)
            .map(|i| plain_event(0, i as u64, &format!("e{i}")))
            .collect();
        collector.phase = CollectorPhase::Finalized;
        collector.shared = Some(shared_with(events));
        let manifest = collector.take_manifest().expect("manifest");
        assert_eq!(manifest.events.len(), MAX_CONTEXT_EVENTS);
        for (i, event) in manifest.events.iter().enumerate() {
            assert_eq!(event.sequence, (i + 1) as u32);
        }
        assert_eq!(
            collector.context_error_code(),
            Some(CollectErrorCode::EventCountExceeded)
        );
    }

    #[test]
    fn take_manifest_pops_events_until_under_encoded_budget() {
        let mut collector = ClipboardCollector::new();
        // 3 个 RICH_TEXT，每个 html_fragment 1.4 MiB → 编码后超 4 MiB，触发尾部 pop。
        let big_html = "a".repeat(1_400_000);
        let events: Vec<ContextEvent> = (0..3)
            .map(|i| ContextEvent {
                sequence: 0,
                source_sample_rate: 48_000,
                sample_offset: 0,
                kind: ContextEventKind::ContextEventRichText as i32,
                plain_text: format!("e{i}"),
                html_fragment: big_html.clone(),
                absolute_paths: Vec::new(),
            })
            .collect();
        collector.phase = CollectorPhase::Finalized;
        collector.shared = Some(shared_with(events));
        let manifest = collector.take_manifest().expect("manifest");
        assert!(manifest.encode_to_vec().len() <= MAX_CONTEXT_BYTES);
        assert!(
            manifest.events.len() < 3,
            "must pop at least one event to fit budget; got {}",
            manifest.events.len()
        );
        for (i, event) in manifest.events.iter().enumerate() {
            assert_eq!(event.sequence, (i + 1) as u32);
        }
        assert_eq!(
            collector.context_error_code(),
            Some(CollectErrorCode::EncodedBudgetExceeded)
        );
    }

    // ── ST-M3.4 自写抑制 ────────────────────────────────────────────────────────

    #[test]
    fn self_write_matches_finds_index_of_registered_digest() {
        let digest = content_digest(&NormalizedContent::PlainText("x".into()));
        let other = digest + 1;
        // 队列含该摘要 → 返回其下标。
        assert_eq!(self_write_matches(digest, &[other, digest]), Some(1));
        // 队列不含 → None（intervening 用户复制不消耗注册）。
        assert_eq!(self_write_matches(digest, &[other]), None);
        // 空队列 → None。
        assert_eq!(self_write_matches(digest, &[]), None);
    }

    #[test]
    fn self_write_filter_queue_suppresses_each_registered_once_then_clears() {
        // 模拟 inject_plan 的两次自写（set_html 写入 + 恢复原剪贴板）：预注册两个摘要，
        // 逐个匹配移除；其间/之后的用户复制（不同摘要）不匹配 → 不抑制。
        let plan = content_digest(&NormalizedContent::PlainText("plan".into()));
        let prev = content_digest(&NormalizedContent::PlainText("prev".into()));
        let user = content_digest(&NormalizedContent::PlainText("user".into()));
        let mut filter = vec![plan, prev];
        // 写 #1（plan）：匹配、移除。
        let i = self_write_matches(plan, &filter).expect("plan 在队列");
        filter.swap_remove(i);
        // 中间的用户复制（user）：不匹配 → 不抑制、不移除。
        assert!(self_write_matches(user, &filter).is_none());
        // 写 #2（prev）：匹配、移除。
        let i = self_write_matches(prev, &filter).expect("prev 在队列");
        filter.swap_remove(i);
        // 队列空 → 之后任何变更都不抑制。
        assert!(filter.is_empty());
        assert!(self_write_matches(plan, &filter).is_none());
    }

    #[test]
    fn register_self_write_returns_false_when_idle_true_when_collecting() {
        let frames = Arc::new(AtomicU64::new(0));
        let mut collector = ClipboardCollector::new();
        // Idle：无活动录音 → 无重叠，注册无意义，返回 false。
        assert!(!collector.register_self_write(123));
        collector.start_with_change_count(
            true,
            48_000,
            frames,
            None,
            None,
            None,
            static_change_count(0),
        );
        // Collecting：有活动录音 → 注册成功，poll_loop 将据此忽略该摘要自写。
        assert!(collector.register_self_write(123));
        collector.reset(); // 回收 poll 线程。
                           // 回到 Idle → 不再注册。
        assert!(!collector.register_self_write(123));
    }
}
