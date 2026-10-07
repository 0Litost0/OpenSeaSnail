//! 平台无关的桌面业务核心。
//!
//! 实时任务状态、reducer、generation/cancellation 与平台能力语义均不依赖 Tauri、
//! OS SDK、daemon/runtime 实现、OpenAPI DTO 或 protobuf。Coordinator 按 M6 路线图
//! 继续逐切片迁入。

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use thiserror::Error;

/// 单调递增的本地实时任务代际。旧 worker 必须同时匹配 generation、phase 和 revision。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TaskGeneration(u64);

impl TaskGeneration {
    pub const fn value(self) -> u64 {
        self.0
    }

    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// 桌面内部取消只停止本地等待/交付，不向 daemon 传播取消。
#[derive(Clone, Debug)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

fn cancellation_token() -> CancellationToken {
    CancellationToken::new()
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeTaskPhase {
    Idle,
    Preparing,
    Recording,
    Submitting,
    Transcribing,
    CleaningUp,
    AutoPasting,
    Completed,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeTaskFallback {
    None,
    Clipboard,
    History,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RealtimeTaskSnapshot {
    pub generation: TaskGeneration,
    pub revision: u64,
    pub task_id: Option<String>,
    pub phase: RealtimeTaskPhase,
    pub session_id: Option<String>,
    pub clipboard_context_count: usize,
    pub auto_paste_enabled: bool,
    pub failure_code: Option<String>,
    pub fallback: RealtimeTaskFallback,
}

impl Default for RealtimeTaskSnapshot {
    fn default() -> Self {
        Self {
            generation: TaskGeneration::default(),
            revision: 0,
            task_id: None,
            phase: RealtimeTaskPhase::Idle,
            session_id: None,
            clipboard_context_count: 0,
            auto_paste_enabled: false,
            failure_code: None,
            fallback: RealtimeTaskFallback::None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RealtimeTaskEvent {
    Prepare {
        task_id: String,
        auto_paste_enabled: bool,
    },
    RecordingReady,
    Start {
        task_id: String,
        auto_paste_enabled: bool,
    },
    RecordingStopped {
        clipboard_context_count: usize,
    },
    SubmissionAccepted {
        session_id: String,
    },
    SessionCleaningUp,
    SessionCompleted,
    Completed,
    Failed {
        code: String,
        fallback: RealtimeTaskFallback,
    },
    Reset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionError {
    InvalidTransition,
    MissingTaskId,
    MissingSessionId,
    InvalidFailureCode,
}

pub fn reduce(
    current: &RealtimeTaskSnapshot,
    event: RealtimeTaskEvent,
) -> Result<RealtimeTaskSnapshot, TransitionError> {
    let mut next = current.clone();
    match event {
        RealtimeTaskEvent::Prepare {
            task_id,
            auto_paste_enabled,
        } => {
            let mut prepared = reduce(
                current,
                RealtimeTaskEvent::Start {
                    task_id,
                    auto_paste_enabled,
                },
            )?;
            prepared.phase = RealtimeTaskPhase::Preparing;
            return Ok(prepared);
        }
        RealtimeTaskEvent::RecordingReady if current.phase == RealtimeTaskPhase::Preparing => {
            next.phase = RealtimeTaskPhase::Recording;
        }
        RealtimeTaskEvent::Start {
            task_id,
            auto_paste_enabled,
        } if current.phase == RealtimeTaskPhase::Idle
            || matches!(
                current.phase,
                RealtimeTaskPhase::Completed | RealtimeTaskPhase::Failed
            ) =>
        {
            if task_id.is_empty() {
                return Err(TransitionError::MissingTaskId);
            }
            next.generation = current.generation.next();
            next.task_id = Some(task_id);
            next.phase = RealtimeTaskPhase::Recording;
            next.session_id = None;
            next.clipboard_context_count = 0;
            next.auto_paste_enabled = auto_paste_enabled;
            next.failure_code = None;
            next.fallback = RealtimeTaskFallback::None;
        }
        RealtimeTaskEvent::RecordingStopped {
            clipboard_context_count,
        } if current.phase == RealtimeTaskPhase::Recording => {
            next.phase = RealtimeTaskPhase::Submitting;
            next.clipboard_context_count = clipboard_context_count;
        }
        RealtimeTaskEvent::SubmissionAccepted { session_id }
            if current.phase == RealtimeTaskPhase::Submitting =>
        {
            if session_id.is_empty() {
                return Err(TransitionError::MissingSessionId);
            }
            next.phase = RealtimeTaskPhase::Transcribing;
            next.session_id = Some(session_id);
        }
        RealtimeTaskEvent::SessionCleaningUp
            if current.phase == RealtimeTaskPhase::Transcribing =>
        {
            next.phase = RealtimeTaskPhase::CleaningUp;
        }
        RealtimeTaskEvent::SessionCompleted
            if matches!(
                current.phase,
                RealtimeTaskPhase::Transcribing | RealtimeTaskPhase::CleaningUp
            ) && !current.auto_paste_enabled =>
        {
            next.phase = RealtimeTaskPhase::Completed;
        }
        RealtimeTaskEvent::SessionCompleted
            if matches!(
                current.phase,
                RealtimeTaskPhase::Transcribing | RealtimeTaskPhase::CleaningUp
            ) && current.auto_paste_enabled =>
        {
            next.phase = RealtimeTaskPhase::AutoPasting;
        }
        RealtimeTaskEvent::Completed if current.phase == RealtimeTaskPhase::AutoPasting => {
            next.phase = RealtimeTaskPhase::Completed;
        }
        RealtimeTaskEvent::Failed { code, fallback }
            if matches!(
                current.phase,
                RealtimeTaskPhase::Preparing
                    | RealtimeTaskPhase::Recording
                    | RealtimeTaskPhase::Submitting
                    | RealtimeTaskPhase::Transcribing
                    | RealtimeTaskPhase::CleaningUp
                    | RealtimeTaskPhase::AutoPasting
            ) =>
        {
            if code.is_empty() {
                return Err(TransitionError::InvalidFailureCode);
            }
            if fallback == RealtimeTaskFallback::Clipboard
                && current.phase != RealtimeTaskPhase::AutoPasting
            {
                return Err(TransitionError::InvalidTransition);
            }
            next.phase = RealtimeTaskPhase::Failed;
            next.failure_code = Some(stable_failure_code(&code));
            next.fallback = fallback;
        }
        RealtimeTaskEvent::Reset
            if matches!(
                current.phase,
                RealtimeTaskPhase::Completed | RealtimeTaskPhase::Failed
            ) =>
        {
            next = RealtimeTaskSnapshot {
                generation: current.generation,
                revision: current.revision,
                ..Default::default()
            };
        }
        _ => return Err(TransitionError::InvalidTransition),
    }
    next.revision = current.revision.saturating_add(1);
    Ok(next)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerToken {
    pub generation: TaskGeneration,
    pub task_id: String,
    pub expected_phase: RealtimeTaskPhase,
    pub revision: u64,
}

pub fn worker_is_current(snapshot: &RealtimeTaskSnapshot, token: &WorkerToken) -> bool {
    snapshot.generation == token.generation
        && snapshot.task_id.as_deref() == Some(token.task_id.as_str())
        && snapshot.phase == token.expected_phase
        && snapshot.revision == token.revision
}

pub fn stable_failure_code(code: &str) -> String {
    const KNOWN: &[&str] = &[
        "connection",
        "no_speech_detected",
        "recording_accessibility_required",
        "recording_auto_paste_failed",
        "recording_cancelled",
        "recording_context_invalid",
        "recording_device_error",
        "recording_microphone_unavailable",
        "recording_microphone_unauthorized",
        "recording_no_audio",
        "recording_stale_worker",
        "recording_submit_failed",
        "recording_submit_duplicate",
        "recording_status_failed",
        "recording_status_invalid",
        "recording_transcription_failed",
        "clipboard_restore_failed",
    ];
    if KNOWN.contains(&code) {
        code.to_string()
    } else {
        "recording_failed".into()
    }
}

/// Failure codes presented as neutral hints: nothing is broken and no user
/// action is required. Everything else failed is error-level.
pub const HINT_LEVEL_FAILURE_CODES: &[&str] = &["no_speech_detected", "recording_no_audio"];

/// Pure decision for how long a terminal presentation stays visible:
/// completed 1s, hint-level failures 3s, every other failure 5s. Callers that
/// present a failure for a non-terminal snapshot (e.g. a busy stop attempt)
/// also get the error-level duration.
pub fn terminal_display_duration(snapshot: &RealtimeTaskSnapshot) -> Duration {
    match snapshot.phase {
        RealtimeTaskPhase::Completed => Duration::from_secs(1),
        RealtimeTaskPhase::Failed => match snapshot.failure_code.as_deref() {
            Some(code) if HINT_LEVEL_FAILURE_CODES.contains(&code) => Duration::from_secs(3),
            _ => Duration::from_secs(5),
        },
        _ => Duration::from_secs(5),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CapturedAudio {
    pub pcm: Vec<f32>,
    pub sample_rate: u32,
    pub input_device: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextPayload(pub Vec<u8>);

impl ContextPayload {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityFailure {
    Unavailable,
    PermissionDenied,
    Busy,
    Unsupported,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum PlatformError {
    #[error("platform capability unavailable: {0:?}")]
    Capability(CapabilityFailure),
    #[error("platform operation failed")]
    Failed,
}

pub trait RecordingPort: Send + Sync {
    fn start(&self) -> Result<(), PlatformError>;
    fn stop(&self) -> Result<CapturedAudio, PlatformError>;
}

pub trait ClipboardPort: Send + Sync {
    fn capture_context(&self) -> Result<Option<ContextPayload>, PlatformError>;
}

pub trait PermissionPort: Send + Sync {
    fn microphone_allowed(&self) -> Result<bool, PlatformError>;
    fn accessibility_allowed(&self) -> Result<bool, PlatformError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeliveryOutcome {
    Pasted,
    ClipboardOnly,
    Failed {
        code: String,
        clipboard_written: bool,
    },
}

impl DeliveryOutcome {
    pub fn failure(self) -> Option<(String, RealtimeTaskFallback)> {
        match self {
            Self::Pasted => None,
            Self::ClipboardOnly => Some((
                "recording_accessibility_required".into(),
                RealtimeTaskFallback::Clipboard,
            )),
            Self::Failed {
                code,
                clipboard_written,
            } => Some((
                stable_failure_code(&code),
                if clipboard_written {
                    RealtimeTaskFallback::Clipboard
                } else {
                    RealtimeTaskFallback::History
                },
            )),
        }
    }
}

pub trait TextDeliveryPort: Send + Sync {
    fn deliver(&self, session_id: &str, permit: DeliveryPermit) -> DeliveryOutcome;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubmissionAccepted {
    pub session_id: String,
    pub status: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubmissionRequest {
    pub wav: Vec<u8>,
    pub input_device: String,
}

pub trait SubmissionGateway: Send + Sync {
    fn submit(
        &self,
        request: SubmissionRequest,
        context: Option<&ContextPayload>,
    ) -> Result<SubmissionAccepted, String>;
}

impl<T: SubmissionGateway + ?Sized> SubmissionGateway for Arc<T> {
    fn submit(
        &self,
        request: SubmissionRequest,
        context: Option<&ContextPayload>,
    ) -> Result<SubmissionAccepted, String> {
        (**self).submit(request, context)
    }
}

pub const TARGET_SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, Eq, PartialEq)]
pub enum SubmissionWorkerError {
    AlreadySubmitted,
    StaleWorker,
    NoAudio,
    InvalidSampleRate,
    Submit(String),
}

/// Single-use submission worker. The coordinator additionally claims the
/// generation before constructing this worker; the local guard protects the
/// worker itself if an adapter accidentally invokes it twice.
pub struct SubmissionWorker<G>
where
    G: SubmissionGateway,
{
    gateway: G,
    submitted: AtomicBool,
}

impl<G> SubmissionWorker<G>
where
    G: SubmissionGateway,
{
    pub fn new(gateway: G) -> Self {
        Self {
            gateway,
            submitted: AtomicBool::new(false),
        }
    }

    pub fn run(
        &self,
        audio: CapturedAudio,
        context: Option<ContextPayload>,
    ) -> Result<SubmissionAccepted, SubmissionWorkerError> {
        self.submitted
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| SubmissionWorkerError::AlreadySubmitted)?;
        let request = prepare_submission(audio)?;
        self.gateway
            .submit(request, context.as_ref())
            .map_err(SubmissionWorkerError::Submit)
    }

    pub fn run_with_token(
        &self,
        audio: CapturedAudio,
        context: Option<ContextPayload>,
        snapshot: &RealtimeTaskSnapshot,
        token: &WorkerToken,
    ) -> Result<SubmissionAccepted, SubmissionWorkerError> {
        if !worker_is_current(snapshot, token) {
            return Err(SubmissionWorkerError::StaleWorker);
        }
        self.run(audio, context)
    }
}

fn prepare_submission(audio: CapturedAudio) -> Result<SubmissionRequest, SubmissionWorkerError> {
    if audio.pcm.is_empty() {
        return Err(SubmissionWorkerError::NoAudio);
    }
    if audio.sample_rate == 0 {
        return Err(SubmissionWorkerError::InvalidSampleRate);
    }
    let pcm = resample_to_target(&audio.pcm, audio.sample_rate);
    if pcm.is_empty() || !pcm.iter().copied().any(|sample| pcm16_sample(sample) != 0) {
        return Err(SubmissionWorkerError::NoAudio);
    }
    Ok(SubmissionRequest {
        wav: encode_wav(&pcm, TARGET_SAMPLE_RATE),
        input_device: audio.input_device,
    })
}

/// Pure linear resampling for realtime mono PCM.
pub fn resample_to_target(pcm: &[f32], source_rate: u32) -> Vec<f32> {
    if source_rate == 0 || pcm.is_empty() {
        return Vec::new();
    }
    if source_rate == TARGET_SAMPLE_RATE {
        return pcm.to_vec();
    }
    let output_len = ((pcm.len() as u64 * TARGET_SAMPLE_RATE as u64) / source_rate as u64) as usize;
    (0..output_len)
        .map(|index| {
            let position = index as f64 * source_rate as f64 / TARGET_SAMPLE_RATE as f64;
            let lower = position.floor() as usize;
            let upper = (lower + 1).min(pcm.len() - 1);
            let fraction = (position - lower as f64) as f32;
            pcm[lower] + (pcm[upper] - pcm[lower]) * fraction
        })
        .collect()
}

/// Pure f32 mono PCM -> 16-bit little-endian RIFF/WAVE encoding.
pub fn encode_wav(pcm: &[f32], sample_rate: u32) -> Vec<u8> {
    let mut data = Vec::with_capacity(pcm.len() * 2);
    for &sample in pcm {
        let value = pcm16_sample(sample);
        data.extend_from_slice(&value.to_le_bytes());
    }
    let data_size = data.len() as u32;
    let mut wav = Vec::with_capacity(44 + data.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_size).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    wav.extend_from_slice(&data);
    wav
}

fn pcm16_sample(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Transcribing,
    CleaningUp,
    Completed,
    /// Carries the daemon's optional free-form `failure_reason`. Older daemons
    /// that do not return the field deserialize as `None` in the adapter.
    Failed(Option<String>),
}

/// Map a daemon failure reason to the stable desktop code. Only `no_speech`
/// is a first-class business semantic; unknown or missing reasons keep the
/// existing transcription-failure presentation.
fn session_failure_code(reason: Option<&str>) -> String {
    match reason {
        Some("no_speech") => "no_speech_detected".into(),
        _ => "recording_transcription_failed".into(),
    }
}

/// 读取 daemon 会话状态的稳定业务端口。
///
/// HTTP 状态码和响应 DTO 由桌面 adapter 负责归一化；core 只看会话状态
/// 和稳定的轮询失败语义。
pub trait SessionStatusGateway: Send + Sync {
    fn status(&self, session_id: &str) -> Result<SessionStatus, String>;
}

impl<T: SessionStatusGateway + ?Sized> SessionStatusGateway for Arc<T> {
    fn status(&self, session_id: &str) -> Result<SessionStatus, String> {
        (**self).status(session_id)
    }
}

pub trait CancellationSignal: Send + Sync {
    fn is_cancelled(&self) -> bool;
}

impl CancellationSignal for AtomicBool {
    fn is_cancelled(&self) -> bool {
        self.load(Ordering::Acquire)
    }
}

impl CancellationSignal for CancellationToken {
    fn is_cancelled(&self) -> bool {
        self.is_cancelled()
    }
}

pub trait PollSleeper: Send + Sync {
    /// 返回 false 表示在等待期间收到取消信号。
    fn sleep(&self, duration: Duration, cancelled: &dyn CancellationSignal) -> bool;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PollOutcome {
    Completed,
    Failed(String),
    Cancelled,
}

/// 轮询会话直到 daemon 返回终态。
///
/// 连续状态读取失败最多重试三次，第四次连续失败映射为稳定的 connection
/// 错误；收到 transcribing 后会把失败退避计数清零。取消只结束桌面侧等待，
/// 不向 daemon 发送取消请求。
pub fn poll_until_terminal<G, S, C>(
    gateway: &G,
    sleeper: &S,
    session_id: &str,
    cancelled: &C,
) -> PollOutcome
where
    G: SessionStatusGateway,
    S: PollSleeper,
    C: CancellationSignal,
{
    let mut consecutive_failures = 0u8;
    loop {
        if cancelled.is_cancelled() {
            return PollOutcome::Cancelled;
        }
        match gateway.status(session_id) {
            Ok(SessionStatus::Completed) => return PollOutcome::Completed,
            Ok(SessionStatus::Failed(reason)) => {
                return PollOutcome::Failed(session_failure_code(reason.as_deref()))
            }
            Ok(SessionStatus::Transcribing | SessionStatus::CleaningUp) => {
                consecutive_failures = 0;
                if !sleeper.sleep(Duration::from_secs(1), cancelled) {
                    return PollOutcome::Cancelled;
                }
            }
            Err(_) => {
                consecutive_failures += 1;
                if consecutive_failures >= 4 {
                    return PollOutcome::Failed("connection".into());
                }
                let backoff = Duration::from_secs(1 << (consecutive_failures - 1));
                if !sleeper.sleep(backoff, cancelled) {
                    return PollOutcome::Cancelled;
                }
            }
        }
    }
}

/// 与 `poll_until_terminal` 相同，但每次请求和等待前都验证当前 task worker。
/// observer 只能以当前精确 token 原子推进展示状态，并返回 coordinator 新签发的
/// phase/revision token；这样同 generation worker 可跨展示阶段继续，而旧 revision
/// 或旧 generation 即使迟到，也不能继续读取 daemon 或回写桌面状态。
pub fn poll_until_terminal_with_token<G, S, C, V, O>(
    gateway: &G,
    sleeper: &S,
    session_id: &str,
    cancelled: &C,
    token: &mut WorkerToken,
    snapshot: &RealtimeTaskSnapshot,
    is_current: V,
    mut observe_status: O,
) -> PollOutcome
where
    G: SessionStatusGateway,
    S: PollSleeper,
    C: CancellationSignal,
    V: Fn(&WorkerToken) -> bool,
    O: FnMut(&WorkerToken, SessionStatus) -> Option<WorkerToken>,
{
    if !worker_is_current(snapshot, token) || !is_current(token) {
        return PollOutcome::Cancelled;
    }
    let mut consecutive_failures = 0u8;
    loop {
        if cancelled.is_cancelled() || !is_current(token) {
            return PollOutcome::Cancelled;
        }
        match gateway.status(session_id) {
            Ok(SessionStatus::Completed) => return PollOutcome::Completed,
            Ok(SessionStatus::Failed(reason)) => {
                return PollOutcome::Failed(session_failure_code(reason.as_deref()))
            }
            Ok(status @ (SessionStatus::Transcribing | SessionStatus::CleaningUp)) => {
                if cancelled.is_cancelled() {
                    return PollOutcome::Cancelled;
                }
                let Some(refreshed) = observe_status(token, status) else {
                    return PollOutcome::Cancelled;
                };
                *token = refreshed;
                if !is_current(token) {
                    return PollOutcome::Cancelled;
                }
                consecutive_failures = 0;
                if !sleeper.sleep(Duration::from_secs(1), cancelled) || !is_current(token) {
                    return PollOutcome::Cancelled;
                }
            }
            Err(_) => {
                consecutive_failures += 1;
                if consecutive_failures >= 4 {
                    return PollOutcome::Failed("connection".into());
                }
                let backoff = Duration::from_secs(1 << (consecutive_failures - 1));
                if !sleeper.sleep(backoff, cancelled) || !is_current(token) {
                    return PollOutcome::Cancelled;
                }
            }
        }
    }
}

/// 生产环境的可取消阻塞等待。测试使用 fake sleeper，不依赖真实时间。
pub struct ThreadPollSleeper;

impl PollSleeper for ThreadPollSleeper {
    fn sleep(&self, duration: Duration, cancelled: &dyn CancellationSignal) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < duration {
            if cancelled.is_cancelled() {
                return false;
            }
            let remaining = duration.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                break;
            }
            thread::sleep(Duration::from_millis(20).min(remaining));
        }
        true
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordingStartInfo {
    pub input_device: String,
    pub sample_rate: u32,
}

pub trait CoordinatorRecordingPort: Send + Sync + 'static {
    type Captured: Send + 'static;

    fn start(&self) -> Result<RecordingStartInfo, String>;
    fn stop(&self) -> Result<Self::Captured, String>;
    fn context_count(captured: &Self::Captured) -> usize;
}

pub trait PresentationSink: Send + Sync + 'static {
    fn publish(&self, snapshot: RealtimeTaskSnapshot);
}

pub trait TerminalTimer: Send + Sync + 'static {
    fn schedule(&self, duration: Duration, callback: Box<dyn FnOnce() + Send>);
}

struct ThreadTerminalTimer;

impl TerminalTimer for ThreadTerminalTimer {
    fn schedule(&self, duration: Duration, callback: Box<dyn FnOnce() + Send>) {
        thread::spawn(move || {
            thread::sleep(duration);
            callback();
        });
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operation {
    Starting,
    Stopping,
}

#[derive(Debug, Eq, PartialEq)]
pub enum CoordinatorError {
    Busy,
    Cancelled,
    InvalidState(TransitionError),
    Port(String),
}

/// Opaque ownership of the one worker belonging to an active local task.
/// Dropping a handle detaches the thread; the worker itself must use the
/// coordinator APIs for every state transition.
struct WorkerHandle {
    _join: thread::JoinHandle<()>,
}

/// Coordinator 发放的一次性文本交付许可。
///
/// 字段保持私有，调用方只能持有并消费该 token，不能伪造、复制或用它
/// 重新取得第二次交付机会。
#[derive(Debug, Eq, PartialEq)]
pub struct DeliveryPermit {
    _private: (),
}

/// The only mutable owner of a local realtime task's lifecycle resources.
///
/// Its fields deliberately remain private: adapters receive observation and
/// operation methods, never a replacement cancellation token or business state.
pub struct ActiveRealtimeTask {
    generation: TaskGeneration,
    cancellation: CancellationToken,
    worker: Option<WorkerHandle>,
    submission_committed: bool,
    delivery_committed: bool,
}

struct CoordinatorState {
    snapshot: RealtimeTaskSnapshot,
    operation: Option<Operation>,
    active: Option<ActiveRealtimeTask>,
    shutting_down: bool,
}

/// Serializes the shortcut/tray realtime entry and owns its active worker.
pub struct RealtimeCoordinator<P, E>
where
    P: CoordinatorRecordingPort,
    E: PresentationSink,
{
    state: Mutex<CoordinatorState>,
    operation_gate: Mutex<()>,
    recording: Arc<P>,
    presentation: Arc<E>,
    timer: Arc<dyn TerminalTimer>,
}

impl<P, E> RealtimeCoordinator<P, E>
where
    P: CoordinatorRecordingPort,
    E: PresentationSink,
{
    pub fn new(recording: Arc<P>, presentation: Arc<E>) -> Self {
        Self {
            state: Mutex::new(CoordinatorState {
                snapshot: RealtimeTaskSnapshot::default(),
                operation: None,
                active: None,
                shutting_down: false,
            }),
            operation_gate: Mutex::new(()),
            recording,
            presentation,
            timer: Arc::new(ThreadTerminalTimer),
        }
    }

    pub fn snapshot(&self) -> RealtimeTaskSnapshot {
        self.state
            .lock()
            .expect("task state mutex")
            .snapshot
            .clone()
    }

    fn update(&self, event: RealtimeTaskEvent) -> Result<RealtimeTaskSnapshot, CoordinatorError> {
        let snapshot = {
            let mut state = self.state.lock().expect("task state mutex");
            let next = reduce(&state.snapshot, event).map_err(CoordinatorError::InvalidState)?;
            state.snapshot = next.clone();
            next
        };
        self.presentation.publish(snapshot.clone());
        Ok(snapshot)
    }

    fn retire_active(state: &mut CoordinatorState) {
        if let Some(active) = state.active.take() {
            active.cancellation.cancel();
        }
    }

    pub fn start(&self, auto_paste_enabled: bool) -> Result<RecordingStartInfo, CoordinatorError> {
        let _gate = self.operation_gate.lock().expect("operation gate mutex");
        let generation = {
            let mut state = self.state.lock().expect("task state mutex");
            if state.shutting_down
                || state.operation.is_some()
                || matches!(
                    state.snapshot.phase,
                    RealtimeTaskPhase::Preparing
                        | RealtimeTaskPhase::Recording
                        | RealtimeTaskPhase::Submitting
                        | RealtimeTaskPhase::Transcribing
                        | RealtimeTaskPhase::CleaningUp
                        | RealtimeTaskPhase::AutoPasting
                )
            {
                return Err(CoordinatorError::Busy);
            }
            state.operation = Some(Operation::Starting);
            Self::retire_active(&mut state);
            state.snapshot.generation.next()
        };

        let task_id = format!("task-{}", generation.value());
        let started = self.update(RealtimeTaskEvent::Prepare {
            task_id,
            auto_paste_enabled,
        });
        if let Err(error) = started {
            self.state.lock().expect("task state mutex").operation = None;
            return Err(error);
        }
        {
            let mut state = self.state.lock().expect("task state mutex");
            state.active = Some(ActiveRealtimeTask {
                generation,
                cancellation: cancellation_token(),
                worker: None,
                submission_committed: false,
                delivery_committed: false,
            });
        }

        let result = self.recording.start();
        let response = match result {
            Ok(info) => {
                self.update(RealtimeTaskEvent::RecordingReady)?;
                Ok(info)
            }
            Err(error) => {
                let _ = self.update(RealtimeTaskEvent::Failed {
                    code: error.clone(),
                    fallback: RealtimeTaskFallback::None,
                });
                if let Some(active) = self.state.lock().expect("task state mutex").active.as_ref() {
                    active.cancellation.cancel();
                }
                Err(CoordinatorError::Port(error))
            }
        };
        self.state.lock().expect("task state mutex").operation = None;
        response
    }

    pub fn stop(&self) -> Result<P::Captured, CoordinatorError> {
        let _gate = self.operation_gate.lock().expect("operation gate mutex");
        {
            let mut state = self.state.lock().expect("task state mutex");
            if state.operation.is_some() || state.snapshot.phase != RealtimeTaskPhase::Recording {
                return Err(CoordinatorError::Busy);
            }
            state.operation = Some(Operation::Stopping);
        }
        let result = self.recording.stop();
        let response = match result {
            Ok(captured) => match self.update(RealtimeTaskEvent::RecordingStopped {
                clipboard_context_count: P::context_count(&captured),
            }) {
                Ok(_) => Ok(captured),
                Err(error) => Err(error),
            },
            Err(error) => {
                let _ = self.update(RealtimeTaskEvent::Failed {
                    code: error.clone(),
                    fallback: RealtimeTaskFallback::None,
                });
                Err(CoordinatorError::Port(error))
            }
        };
        self.state.lock().expect("task state mutex").operation = None;
        response
    }

    /// Give a worker a coordinator-issued token for the exact current state.
    pub fn worker_token(
        &self,
        expected_phase: RealtimeTaskPhase,
    ) -> Result<WorkerToken, CoordinatorError> {
        let state = self.state.lock().expect("task state mutex");
        let Some(active) = state.active.as_ref() else {
            return Err(CoordinatorError::Busy);
        };
        if state.snapshot.phase != expected_phase || state.snapshot.generation != active.generation
        {
            return Err(CoordinatorError::Busy);
        }
        let Some(task_id) = state.snapshot.task_id.clone() else {
            return Err(CoordinatorError::Busy);
        };
        Ok(WorkerToken {
            generation: active.generation,
            task_id,
            expected_phase,
            revision: state.snapshot.revision,
        })
    }

    pub fn is_worker_current(&self, token: &WorkerToken) -> bool {
        let state = self.state.lock().expect("task state mutex");
        state
            .active
            .as_ref()
            .is_some_and(|active| active.generation == token.generation)
            && worker_is_current(&state.snapshot, token)
    }

    /// Linearization point for the one submission allowed by a local task
    /// generation. This check happens before the gateway is called, so stale
    /// workers and duplicate pipeline entry cannot reach the daemon adapter.
    pub fn commit_submission(&self, token: &WorkerToken) -> bool {
        let _gate = self.operation_gate.lock().expect("operation gate mutex");
        if !self.is_worker_current(token) || token.expected_phase != RealtimeTaskPhase::Submitting {
            return false;
        }
        let mut state = self.state.lock().expect("task state mutex");
        let Some(active) = state.active.as_mut() else {
            return false;
        };
        if active.generation != token.generation || active.submission_committed {
            return false;
        }
        active.submission_committed = true;
        true
    }

    /// Run exactly one worker under the active task's cancellation token.
    /// The adapter cannot install a different token or retain worker ownership.
    pub fn spawn_worker<F>(self: &Arc<Self>, worker: F) -> Result<(), CoordinatorError>
    where
        F: FnOnce(CancellationToken) + Send + 'static,
    {
        let (generation, cancellation) = {
            let state = self.state.lock().expect("task state mutex");
            let Some(active) = state.active.as_ref() else {
                return Err(CoordinatorError::Busy);
            };
            if active.worker.is_some()
                || matches!(
                    state.snapshot.phase,
                    RealtimeTaskPhase::Idle
                        | RealtimeTaskPhase::Completed
                        | RealtimeTaskPhase::Failed
                )
            {
                return Err(CoordinatorError::Busy);
            }
            (active.generation, active.cancellation.clone())
        };

        // Do not let a very short worker finish before its handle is installed.
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
        let coordinator = Arc::clone(self);
        let handle = thread::spawn(move || {
            if ready_rx.recv().is_err() {
                return;
            }
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                worker(cancellation);
            }));
            coordinator.worker_finished(generation);
        });
        {
            let mut state = self.state.lock().expect("task state mutex");
            let Some(active) = state.active.as_mut() else {
                return Err(CoordinatorError::Busy);
            };
            if active.generation != generation || active.worker.is_some() {
                return Err(CoordinatorError::Busy);
            }
            active.worker = Some(WorkerHandle { _join: handle });
        }
        ready_tx.send(()).map_err(|_| CoordinatorError::Busy)
    }

    fn worker_finished(&self, generation: TaskGeneration) {
        let mut state = self.state.lock().expect("task state mutex");
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.generation == generation)
        {
            state.active.as_mut().expect("active task").worker = None;
        }
    }

    /// Internal lifecycle cancellation. It intentionally does not contact the daemon.
    pub fn cancel_active(&self, generation: TaskGeneration) -> bool {
        let _gate = self.operation_gate.lock().expect("operation gate mutex");
        let state = self.state.lock().expect("task state mutex");
        let Some(active) = state.active.as_ref() else {
            return false;
        };
        if active.generation != generation {
            return false;
        }
        active.cancellation.cancel();
        true
    }

    /// Stops accepting new realtime work, cancels the active desktop wait and
    /// joins its worker within the supplied deadline. A timed-out join is
    /// detached so GUI shutdown is still bounded; the worker itself remains
    /// unable to write state after the active task is retired.
    pub fn shutdown(&self, timeout: Duration) -> bool {
        let worker = {
            let _gate = self.operation_gate.lock().expect("operation gate mutex");
            let mut state = self.state.lock().expect("task state mutex");
            state.shutting_down = true;
            if let Some(mut active) = state.active.take() {
                active.cancellation.cancel();
                active.worker.take()
            } else {
                None
            }
        };
        let Some(worker) = worker else {
            return true;
        };
        let (finished_tx, finished_rx) = std::sync::mpsc::sync_channel(0);
        thread::spawn(move || {
            let _ = worker._join.join();
            let _ = finished_tx.send(());
        });
        finished_rx.recv_timeout(timeout).is_ok()
    }

    /// 取得一次性文本交付许可。
    ///
    /// 取消与交付在同一个 coordinator gate 上线性化：先取消则不会发放
    /// permit；先发放 permit 则该次交付可以继续完成，即使随后发生取消。
    pub fn acquire_delivery(
        &self,
        token: &WorkerToken,
    ) -> Result<DeliveryPermit, CoordinatorError> {
        let _gate = self.operation_gate.lock().expect("operation gate mutex");
        let mut state = self.state.lock().expect("task state mutex");
        if !worker_is_current(&state.snapshot, token)
            || state.snapshot.phase != RealtimeTaskPhase::AutoPasting
        {
            return Err(CoordinatorError::Busy);
        }
        let Some(active) = state.active.as_mut() else {
            return Err(CoordinatorError::Busy);
        };
        if active.cancellation.is_cancelled() {
            return Err(CoordinatorError::Cancelled);
        }
        if active.generation != token.generation || active.delivery_committed {
            return Err(CoordinatorError::Busy);
        }
        active.delivery_committed = true;
        Ok(DeliveryPermit { _private: () })
    }

    pub fn recording_error(
        &self,
        token: &WorkerToken,
        code: String,
    ) -> Result<(), CoordinatorError> {
        let _gate = self.operation_gate.lock().expect("operation gate mutex");
        if !self.is_worker_current(token) || token.expected_phase != RealtimeTaskPhase::Recording {
            return Err(CoordinatorError::Busy);
        }
        self.update(RealtimeTaskEvent::Failed {
            code,
            fallback: RealtimeTaskFallback::None,
        })?;
        Ok(())
    }

    pub fn complete(&self, token: &WorkerToken) -> Result<(), CoordinatorError> {
        let _gate = self.operation_gate.lock().expect("operation gate mutex");
        if !self.is_worker_current(token) || token.expected_phase != RealtimeTaskPhase::AutoPasting
        {
            return Err(CoordinatorError::Busy);
        }
        self.update(RealtimeTaskEvent::Completed).map(|_| ())
    }

    pub fn transcription_completed(
        &self,
        token: &WorkerToken,
    ) -> Result<RealtimeTaskSnapshot, CoordinatorError> {
        self.session_completed(token)
    }

    /// Apply a non-terminal daemon status to the presentation state and issue the exact
    /// replacement token. Repeated `cleaning_up` and a late `transcribing` observation after
    /// `cleaning_up` are no-ops, so the local presentation remains monotonic.
    pub fn observe_session_status(
        &self,
        token: &WorkerToken,
        status: SessionStatus,
    ) -> Result<WorkerToken, CoordinatorError> {
        let _gate = self.operation_gate.lock().expect("operation gate mutex");
        if !self.is_worker_current(token)
            || !matches!(
                token.expected_phase,
                RealtimeTaskPhase::Transcribing | RealtimeTaskPhase::CleaningUp
            )
        {
            return Err(CoordinatorError::Busy);
        }
        let snapshot = match (token.expected_phase, status) {
            (RealtimeTaskPhase::Transcribing, SessionStatus::CleaningUp) => {
                self.update(RealtimeTaskEvent::SessionCleaningUp)?
            }
            (
                RealtimeTaskPhase::Transcribing | RealtimeTaskPhase::CleaningUp,
                SessionStatus::Transcribing | SessionStatus::CleaningUp,
            ) => self.snapshot(),
            _ => return Err(CoordinatorError::Busy),
        };
        Ok(WorkerToken {
            generation: snapshot.generation,
            task_id: snapshot.task_id.ok_or(CoordinatorError::Busy)?,
            expected_phase: snapshot.phase,
            revision: snapshot.revision,
        })
    }

    pub fn session_completed(
        &self,
        token: &WorkerToken,
    ) -> Result<RealtimeTaskSnapshot, CoordinatorError> {
        let _gate = self.operation_gate.lock().expect("operation gate mutex");
        if !self.is_worker_current(token)
            || !matches!(
                token.expected_phase,
                RealtimeTaskPhase::Transcribing | RealtimeTaskPhase::CleaningUp
            )
        {
            return Err(CoordinatorError::Busy);
        }
        self.update(RealtimeTaskEvent::SessionCompleted)
    }

    pub fn schedule_terminal_hide(self: &Arc<Self>, task_id: String, duration: Duration) {
        self.schedule_terminal_action(task_id, duration, || {});
    }

    pub fn schedule_terminal_action<F>(
        self: &Arc<Self>,
        task_id: String,
        duration: Duration,
        on_reset: F,
    ) where
        F: FnOnce() + Send + 'static,
    {
        let revision = self.snapshot().revision;
        let coordinator = Arc::clone(self);
        self.timer.schedule(
            duration,
            Box::new(move || {
                if coordinator.reset_if_current(&task_id, revision) {
                    on_reset();
                }
            }),
        );
    }

    pub fn submission_accepted(
        &self,
        token: &WorkerToken,
        session_id: String,
    ) -> Result<(), CoordinatorError> {
        let _gate = self.operation_gate.lock().expect("operation gate mutex");
        if !self.is_worker_current(token) || token.expected_phase != RealtimeTaskPhase::Submitting {
            return Err(CoordinatorError::Busy);
        }
        self.update(RealtimeTaskEvent::SubmissionAccepted { session_id })
            .map(|_| ())
    }

    pub fn fail(
        &self,
        token: &WorkerToken,
        code: String,
        fallback: RealtimeTaskFallback,
    ) -> Result<(), CoordinatorError> {
        let _gate = self.operation_gate.lock().expect("operation gate mutex");
        if !self.is_worker_current(token) {
            return Err(CoordinatorError::Busy);
        }
        self.update(RealtimeTaskEvent::Failed { code, fallback })
            .map(|_| ())
    }

    pub fn reset_if_current(&self, task_id: &str, terminal_revision: u64) -> bool {
        let _gate = self.operation_gate.lock().expect("operation gate mutex");
        let snapshot = self.snapshot();
        if snapshot.task_id.as_deref() != Some(task_id)
            || snapshot.revision != terminal_revision
            || !matches!(
                snapshot.phase,
                RealtimeTaskPhase::Completed | RealtimeTaskPhase::Failed
            )
        {
            return false;
        }
        let updated = self.update(RealtimeTaskEvent::Reset).is_ok();
        if updated {
            let mut state = self.state.lock().expect("task state mutex");
            Self::retire_active(&mut state);
        }
        updated
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum RealtimePipelineError {
    Coordinator(CoordinatorError),
    Submission(String),
    Cancelled,
    Polling(String),
    AutoPaste(String),
}

fn submission_error_code(error: &SubmissionWorkerError) -> String {
    match error {
        SubmissionWorkerError::AlreadySubmitted => "recording_submit_duplicate".into(),
        SubmissionWorkerError::StaleWorker => "recording_stale_worker".into(),
        SubmissionWorkerError::NoAudio => "recording_no_audio".into(),
        SubmissionWorkerError::InvalidSampleRate => "recording_no_audio".into(),
        SubmissionWorkerError::Submit(_) => "recording_submit_failed".into(),
    }
}

/// 录音停止后的唯一提交、轮询和交付编排。
///
/// 该流程只依赖 desktop-core 的语义端口；daemon HTTP、剪贴板和平台实现均由
/// 调用方注入，避免 Tauri 侧保留第二套 realtime pipeline owner。
pub fn run_after_stop<P, E, G, S, I, F, C>(
    coordinator: &RealtimeCoordinator<P, E>,
    captured: P::Captured,
    to_audio: F,
    gateway: Arc<G>,
    sleeper: &S,
    injector: &I,
    cancelled: &C,
) -> Result<(), RealtimePipelineError>
where
    P: CoordinatorRecordingPort,
    E: PresentationSink,
    G: SubmissionGateway + SessionStatusGateway + 'static,
    S: PollSleeper,
    I: TextDeliveryPort,
    F: FnOnce(P::Captured) -> (CapturedAudio, Option<ContextPayload>),
    C: CancellationSignal,
{
    let stopped = coordinator.snapshot();
    if stopped.task_id.is_none() {
        return Err(RealtimePipelineError::Coordinator(CoordinatorError::Busy));
    }
    let submit_token = coordinator
        .worker_token(RealtimeTaskPhase::Submitting)
        .map_err(|_| RealtimePipelineError::Submission("recording_stale_worker".into()))?;
    if cancelled.is_cancelled() {
        let _ = coordinator.fail(
            &submit_token,
            "recording_cancelled".into(),
            RealtimeTaskFallback::History,
        );
        return Err(RealtimePipelineError::Cancelled);
    }
    if !coordinator.commit_submission(&submit_token) {
        let code = stable_failure_code(&submission_error_code(
            &SubmissionWorkerError::AlreadySubmitted,
        ));
        return Err(RealtimePipelineError::Submission(code));
    }
    let worker = SubmissionWorker::new(Arc::clone(&gateway));
    let (audio, context) = to_audio(captured);
    let accepted = worker
        .run_with_token(audio, context, &stopped, &submit_token)
        .map_err(|error| {
            let code = stable_failure_code(&submission_error_code(&error));
            let _ = coordinator.fail(&submit_token, code.clone(), RealtimeTaskFallback::None);
            RealtimePipelineError::Submission(code)
        })?;
    coordinator
        .submission_accepted(&submit_token, accepted.session_id.clone())
        .map_err(RealtimePipelineError::Coordinator)?;

    let transcribing = coordinator.snapshot();
    let mut poll_token = coordinator
        .worker_token(RealtimeTaskPhase::Transcribing)
        .map_err(RealtimePipelineError::Coordinator)?;
    match poll_until_terminal_with_token(
        gateway.as_ref(),
        sleeper,
        &accepted.session_id,
        cancelled,
        &mut poll_token,
        &transcribing,
        |token| coordinator.is_worker_current(token),
        |token, status| coordinator.observe_session_status(token, status).ok(),
    ) {
        PollOutcome::Cancelled => {
            let _ = coordinator.fail(
                &poll_token,
                "recording_cancelled".into(),
                RealtimeTaskFallback::History,
            );
            Err(RealtimePipelineError::Cancelled)
        }
        PollOutcome::Failed(code) => {
            // A definite terminal failure (transcription failed or no speech)
            // has nothing recoverable in History: the session is retained but
            // carries no transcript. Only uncertain states (e.g. connection,
            // where the daemon may still finish) guide the user to History.
            let fallback = match code.as_str() {
                "recording_transcription_failed" | "no_speech_detected" => {
                    RealtimeTaskFallback::None
                }
                _ => RealtimeTaskFallback::History,
            };
            coordinator
                .fail(&poll_token, code.clone(), fallback)
                .map_err(RealtimePipelineError::Coordinator)?;
            Err(RealtimePipelineError::Polling(code))
        }
        PollOutcome::Completed => {
            if cancelled.is_cancelled() {
                let _ = coordinator.fail(
                    &poll_token,
                    "recording_cancelled".into(),
                    RealtimeTaskFallback::History,
                );
                return Err(RealtimePipelineError::Cancelled);
            }
            let next = coordinator
                .session_completed(&poll_token)
                .map_err(RealtimePipelineError::Coordinator)?;
            if next.phase == RealtimeTaskPhase::Completed {
                return Ok(());
            }
            if cancelled.is_cancelled() {
                let _ = coordinator.fail(
                    &poll_token,
                    "recording_cancelled".into(),
                    RealtimeTaskFallback::History,
                );
                return Err(RealtimePipelineError::Cancelled);
            }
            let auto_paste_token = coordinator
                .worker_token(RealtimeTaskPhase::AutoPasting)
                .map_err(RealtimePipelineError::Coordinator)?;
            let permit = match coordinator.acquire_delivery(&auto_paste_token) {
                Ok(permit) => permit,
                Err(CoordinatorError::Cancelled) => {
                    let _ = coordinator.fail(
                        &auto_paste_token,
                        "recording_cancelled".into(),
                        RealtimeTaskFallback::History,
                    );
                    return Err(RealtimePipelineError::Cancelled);
                }
                Err(error) => return Err(RealtimePipelineError::Coordinator(error)),
            };
            match injector.deliver(&accepted.session_id, permit).failure() {
                None => coordinator
                    .complete(&auto_paste_token)
                    .map_err(RealtimePipelineError::Coordinator),
                Some((code, fallback)) => {
                    coordinator
                        .fail(&auto_paste_token, code.clone(), fallback)
                        .map_err(RealtimePipelineError::Coordinator)?;
                    Err(RealtimePipelineError::AutoPaste(code))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn apply(snapshot: &RealtimeTaskSnapshot, event: RealtimeTaskEvent) -> RealtimeTaskSnapshot {
        reduce(snapshot, event).expect("valid transition")
    }

    #[test]
    fn ports_express_capability_semantics_without_platform_types() {
        assert_eq!(
            serde_json::to_string(&CapabilityFailure::PermissionDenied).unwrap(),
            "\"permission_denied\""
        );
        assert_eq!(TaskGeneration::default().next().value(), 1);
    }

    #[test]
    fn reducer_preserves_generation_across_reset_and_increments_on_start() {
        let mut snapshot = apply(
            &RealtimeTaskSnapshot::default(),
            RealtimeTaskEvent::Start {
                task_id: "task-1".into(),
                auto_paste_enabled: false,
            },
        );
        assert_eq!(snapshot.generation.value(), 1);
        snapshot = apply(
            &snapshot,
            RealtimeTaskEvent::Failed {
                code: "recording_no_audio".into(),
                fallback: RealtimeTaskFallback::None,
            },
        );
        snapshot = apply(&snapshot, RealtimeTaskEvent::Reset);
        assert_eq!(snapshot.generation.value(), 1);
        snapshot = apply(
            &snapshot,
            RealtimeTaskEvent::Start {
                task_id: "task-2".into(),
                auto_paste_enabled: true,
            },
        );
        assert_eq!(snapshot.generation.value(), 2);
    }

    #[test]
    fn full_auto_paste_path_is_valid_and_revisions_are_monotonic() {
        let mut snapshot = apply(
            &RealtimeTaskSnapshot::default(),
            RealtimeTaskEvent::Start {
                task_id: "task-1".into(),
                auto_paste_enabled: true,
            },
        );
        snapshot = apply(
            &snapshot,
            RealtimeTaskEvent::RecordingStopped {
                clipboard_context_count: 3,
            },
        );
        snapshot = apply(
            &snapshot,
            RealtimeTaskEvent::SubmissionAccepted {
                session_id: "session-1".into(),
            },
        );
        snapshot = apply(&snapshot, RealtimeTaskEvent::SessionCleaningUp);
        assert_eq!(snapshot.phase, RealtimeTaskPhase::CleaningUp);
        snapshot = apply(&snapshot, RealtimeTaskEvent::SessionCompleted);
        assert_eq!(snapshot.phase, RealtimeTaskPhase::AutoPasting);
        snapshot = apply(&snapshot, RealtimeTaskEvent::Completed);
        assert_eq!(snapshot.phase, RealtimeTaskPhase::Completed);
        assert_eq!(snapshot.revision, 6);
        assert_eq!(snapshot.clipboard_context_count, 3);
    }

    #[test]
    fn auto_paste_disabled_skips_auto_pasting() {
        let mut snapshot = apply(
            &RealtimeTaskSnapshot::default(),
            RealtimeTaskEvent::Start {
                task_id: "task-1".into(),
                auto_paste_enabled: false,
            },
        );
        snapshot = apply(
            &snapshot,
            RealtimeTaskEvent::RecordingStopped {
                clipboard_context_count: 0,
            },
        );
        snapshot = apply(
            &snapshot,
            RealtimeTaskEvent::SubmissionAccepted {
                session_id: "session-1".into(),
            },
        );
        snapshot = apply(&snapshot, RealtimeTaskEvent::SessionCleaningUp);
        assert_eq!(snapshot.phase, RealtimeTaskPhase::CleaningUp);
        snapshot = apply(&snapshot, RealtimeTaskEvent::SessionCompleted);
        assert_eq!(snapshot.phase, RealtimeTaskPhase::Completed);
    }

    #[test]
    fn cleaning_up_transitions_are_strict_and_failures_stay_stable() {
        let mut snapshot = apply(
            &RealtimeTaskSnapshot::default(),
            RealtimeTaskEvent::Start {
                task_id: "task-1".into(),
                auto_paste_enabled: false,
            },
        );
        assert_eq!(
            reduce(&snapshot, RealtimeTaskEvent::SessionCleaningUp),
            Err(TransitionError::InvalidTransition)
        );
        snapshot = apply(
            &snapshot,
            RealtimeTaskEvent::RecordingStopped {
                clipboard_context_count: 0,
            },
        );
        snapshot = apply(
            &snapshot,
            RealtimeTaskEvent::SubmissionAccepted {
                session_id: "session-1".into(),
            },
        );
        snapshot = apply(&snapshot, RealtimeTaskEvent::SessionCleaningUp);
        assert_eq!(snapshot.phase, RealtimeTaskPhase::CleaningUp);
        assert_eq!(
            reduce(&snapshot, RealtimeTaskEvent::SessionCleaningUp),
            Err(TransitionError::InvalidTransition)
        );
        assert_eq!(
            reduce(
                &snapshot,
                RealtimeTaskEvent::Start {
                    task_id: "task-2".into(),
                    auto_paste_enabled: false,
                }
            ),
            Err(TransitionError::InvalidTransition)
        );
        let cancelled = apply(
            &snapshot,
            RealtimeTaskEvent::Failed {
                code: "recording_cancelled".into(),
                fallback: RealtimeTaskFallback::None,
            },
        );
        assert_eq!(cancelled.phase, RealtimeTaskPhase::Failed);
        assert_eq!(
            cancelled.failure_code.as_deref(),
            Some("recording_cancelled")
        );
        snapshot = apply(
            &snapshot,
            RealtimeTaskEvent::Failed {
                code: "provider-secret-detail".into(),
                fallback: RealtimeTaskFallback::None,
            },
        );
        assert_eq!(snapshot.phase, RealtimeTaskPhase::Failed);
        assert_eq!(snapshot.failure_code.as_deref(), Some("recording_failed"));
    }

    #[test]
    fn active_task_rejects_second_start_and_idle_fields_are_safe() {
        let snapshot = apply(
            &RealtimeTaskSnapshot::default(),
            RealtimeTaskEvent::Start {
                task_id: "task-1".into(),
                auto_paste_enabled: true,
            },
        );
        assert_eq!(
            reduce(
                &snapshot,
                RealtimeTaskEvent::Start {
                    task_id: "task-2".into(),
                    auto_paste_enabled: true,
                }
            ),
            Err(TransitionError::InvalidTransition)
        );
        let reset = apply(
            &apply(
                &snapshot,
                RealtimeTaskEvent::Failed {
                    code: "recording_device_error".into(),
                    fallback: RealtimeTaskFallback::None,
                },
            ),
            RealtimeTaskEvent::Reset,
        );
        assert_eq!(reset.phase, RealtimeTaskPhase::Idle);
        assert!(
            reset.task_id.is_none() && reset.session_id.is_none() && reset.failure_code.is_none()
        );
        assert_eq!(reset.fallback, RealtimeTaskFallback::None);
    }

    #[test]
    fn invalid_transitions_do_not_change_snapshot() {
        let snapshot = RealtimeTaskSnapshot::default();
        assert_eq!(
            reduce(
                &snapshot,
                RealtimeTaskEvent::SubmissionAccepted {
                    session_id: "x".into(),
                }
            ),
            Err(TransitionError::InvalidTransition)
        );
        assert_eq!(snapshot, RealtimeTaskSnapshot::default());
    }

    #[test]
    fn failed_is_terminal_and_old_terminal_reset_cannot_touch_new_task() {
        let failed = apply(
            &apply(
                &RealtimeTaskSnapshot::default(),
                RealtimeTaskEvent::Start {
                    task_id: "task-1".into(),
                    auto_paste_enabled: true,
                },
            ),
            RealtimeTaskEvent::Failed {
                code: "recording_device_error".into(),
                fallback: RealtimeTaskFallback::None,
            },
        );
        assert_eq!(
            reduce(
                &failed,
                RealtimeTaskEvent::Failed {
                    code: "other".into(),
                    fallback: RealtimeTaskFallback::None,
                }
            ),
            Err(TransitionError::InvalidTransition)
        );
    }

    #[test]
    fn stale_worker_from_prior_generation_is_rejected_even_if_other_fields_match() {
        let snapshot = apply(
            &RealtimeTaskSnapshot::default(),
            RealtimeTaskEvent::Start {
                task_id: "reused-id".into(),
                auto_paste_enabled: false,
            },
        );
        let current = WorkerToken {
            generation: snapshot.generation,
            task_id: "reused-id".into(),
            expected_phase: RealtimeTaskPhase::Recording,
            revision: snapshot.revision,
        };
        assert!(worker_is_current(&snapshot, &current));
        let stale = WorkerToken {
            generation: TaskGeneration::default(),
            ..current
        };
        assert!(!worker_is_current(&snapshot, &stale));
    }

    #[test]
    fn cancellation_is_shared_and_monotonic() {
        let token = cancellation_token();
        let observer = token.clone();
        assert!(!observer.is_cancelled());
        token.cancel();
        assert!(observer.is_cancelled());
    }

    struct FakeRecording {
        starts: AtomicUsize,
        stops: AtomicUsize,
    }

    impl CoordinatorRecordingPort for FakeRecording {
        type Captured = Vec<f32>;

        fn start(&self) -> Result<RecordingStartInfo, String> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(RecordingStartInfo {
                input_device: "fake".into(),
                sample_rate: 16_000,
            })
        }

        fn stop(&self) -> Result<Self::Captured, String> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            Ok(vec![0.0; 4])
        }

        fn context_count(_: &Self::Captured) -> usize {
            0
        }
    }

    struct Events(Mutex<Vec<RealtimeTaskSnapshot>>);

    impl PresentationSink for Events {
        fn publish(&self, snapshot: RealtimeTaskSnapshot) {
            self.0.lock().expect("events mutex").push(snapshot);
        }
    }

    struct BlockingRecording {
        entered: Mutex<Option<std::sync::mpsc::Sender<()>>>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl CoordinatorRecordingPort for BlockingRecording {
        type Captured = Vec<f32>;

        fn start(&self) -> Result<RecordingStartInfo, String> {
            if let Some(sender) = self.entered.lock().expect("entered mutex").take() {
                sender.send(()).expect("announce recording start");
            }
            self.release
                .lock()
                .expect("release mutex")
                .recv()
                .expect("release recording start");
            Ok(RecordingStartInfo {
                input_device: "fake".into(),
                sample_rate: 16_000,
            })
        }

        fn stop(&self) -> Result<Self::Captured, String> {
            Ok(Vec::new())
        }

        fn context_count(_: &Self::Captured) -> usize {
            0
        }
    }

    #[test]
    fn preparation_failure_is_terminal_and_cannot_claim_audio_is_ready() {
        let prepared = reduce(
            &RealtimeTaskSnapshot::default(),
            RealtimeTaskEvent::Prepare {
                task_id: "prepare-failure".into(),
                auto_paste_enabled: false,
            },
        )
        .expect("prepare");
        let failed = reduce(
            &prepared,
            RealtimeTaskEvent::Failed {
                code: "recording_microphone_unavailable".into(),
                fallback: RealtimeTaskFallback::None,
            },
        )
        .expect("failure");
        assert_eq!(failed.phase, RealtimeTaskPhase::Failed);
        assert!(reduce(&failed, RealtimeTaskEvent::RecordingReady).is_err());
        assert!(reduce(
            &prepared,
            RealtimeTaskEvent::Prepare {
                task_id: "duplicate".into(),
                auto_paste_enabled: false,
            }
        )
        .is_err());
    }

    #[test]
    fn start_reserves_task_generation_before_recording_port_returns() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let recording = Arc::new(BlockingRecording {
            entered: Mutex::new(Some(entered_tx)),
            release: Mutex::new(release_rx),
        });
        let coordinator = Arc::new(RealtimeCoordinator::new(
            recording,
            Arc::new(Events(Mutex::new(Vec::new()))),
        ));
        let start_coordinator = Arc::clone(&coordinator);
        let start = thread::spawn(move || start_coordinator.start(true));

        entered_rx.recv().expect("recording port entered");
        let reserved = coordinator.snapshot();
        assert_eq!(reserved.phase, RealtimeTaskPhase::Preparing);
        assert!(reduce(
            &reserved,
            RealtimeTaskEvent::RecordingStopped {
                clipboard_context_count: 0
            }
        )
        .is_err());
        assert!(reserved.task_id.is_some());

        release_tx.send(()).expect("release recording start");
        start.join().expect("start thread").expect("start result");
        assert_eq!(coordinator.snapshot().phase, RealtimeTaskPhase::Recording);
    }

    #[test]
    fn coordinator_serializes_start_and_stop_and_publishes_snapshots() {
        let recording = Arc::new(FakeRecording {
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
        });
        let events = Arc::new(Events(Mutex::new(Vec::new())));
        let coordinator = RealtimeCoordinator::new(recording.clone(), events.clone());
        coordinator.start(true).expect("start");
        assert_eq!(coordinator.start(true), Err(CoordinatorError::Busy));
        let captured = coordinator.stop().expect("stop");
        assert_eq!(captured.len(), 4);
        assert_eq!(coordinator.stop(), Err(CoordinatorError::Busy));
        assert_eq!(recording.starts.load(Ordering::SeqCst), 1);
        assert_eq!(recording.stops.load(Ordering::SeqCst), 1);
        assert_eq!(
            events
                .0
                .lock()
                .expect("events mutex")
                .iter()
                .map(|snapshot| snapshot.phase)
                .collect::<Vec<_>>(),
            vec![
                RealtimeTaskPhase::Preparing,
                RealtimeTaskPhase::Recording,
                RealtimeTaskPhase::Submitting
            ]
        );
    }

    #[test]
    fn terminal_reset_requires_task_and_revision() {
        let recording = Arc::new(FakeRecording {
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
        });
        let events = Arc::new(Events(Mutex::new(Vec::new())));
        let coordinator = Arc::new(RealtimeCoordinator::new(recording, events));
        coordinator.start(false).expect("start");
        let token = coordinator
            .worker_token(RealtimeTaskPhase::Recording)
            .expect("recording token");
        coordinator
            .recording_error(&token, "recording_device_error".into())
            .expect("fail");
        let snapshot = coordinator.snapshot();
        assert!(!coordinator.reset_if_current(
            snapshot.task_id.as_deref().expect("task id"),
            snapshot.revision - 1,
        ));
        assert!(coordinator.reset_if_current(
            snapshot.task_id.as_deref().expect("task id"),
            snapshot.revision,
        ));
        assert_eq!(coordinator.snapshot().phase, RealtimeTaskPhase::Idle);
        assert!(coordinator
            .worker_token(RealtimeTaskPhase::Recording)
            .is_err());
    }

    #[test]
    fn concurrent_start_and_stop_have_one_effective_operation() {
        let recording = Arc::new(FakeRecording {
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
        });
        let events = Arc::new(Events(Mutex::new(Vec::new())));
        let coordinator = Arc::new(RealtimeCoordinator::new(recording.clone(), events));
        let left = Arc::clone(&coordinator);
        let right = Arc::clone(&coordinator);
        let starts = thread::spawn(move || left.start(true).is_ok());
        let other_start = thread::spawn(move || right.start(true).is_ok());
        let start_results = [
            starts.join().expect("start thread"),
            other_start.join().expect("second start thread"),
        ];
        assert_eq!(start_results.iter().filter(|result| **result).count(), 1);
        assert_eq!(recording.starts.load(Ordering::SeqCst), 1);

        let left = Arc::clone(&coordinator);
        let right = Arc::clone(&coordinator);
        let stops = thread::spawn(move || left.stop().is_ok());
        let other_stop = thread::spawn(move || right.stop().is_ok());
        let stop_results = [
            stops.join().expect("stop thread"),
            other_stop.join().expect("second stop thread"),
        ];
        assert_eq!(stop_results.iter().filter(|result| **result).count(), 1);
        assert_eq!(recording.stops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn coordinator_is_the_only_owner_of_worker_and_cancellation() {
        let coordinator = Arc::new(RealtimeCoordinator::new(
            Arc::new(FakeRecording {
                starts: AtomicUsize::new(0),
                stops: AtomicUsize::new(0),
            }),
            Arc::new(Events(Mutex::new(Vec::new()))),
        ));
        coordinator.start(false).expect("start");
        coordinator.stop().expect("stop");
        let generation = coordinator.snapshot().generation;
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (observed_cancel_tx, observed_cancel_rx) = std::sync::mpsc::channel();
        coordinator
            .spawn_worker(move |cancellation| {
                entered_tx.send(()).expect("worker entered");
                while !cancellation.is_cancelled() {
                    thread::yield_now();
                }
                observed_cancel_tx.send(()).expect("worker observed cancel");
            })
            .expect("spawn worker");
        entered_rx.recv().expect("worker entered");
        assert_eq!(
            coordinator.spawn_worker(|_| {}),
            Err(CoordinatorError::Busy)
        );
        assert!(coordinator.cancel_active(generation));
        observed_cancel_rx.recv().expect("worker observed cancel");
        assert!(!coordinator.cancel_active(TaskGeneration::default()));
        let token = coordinator
            .worker_token(RealtimeTaskPhase::Submitting)
            .expect("submitting token");
        assert!(coordinator
            .fail(
                &token,
                "recording_cancelled".into(),
                RealtimeTaskFallback::History,
            )
            .is_ok());
    }

    #[test]
    fn stale_worker_token_cannot_mutate_a_new_generation() {
        let coordinator = RealtimeCoordinator::new(
            Arc::new(FakeRecording {
                starts: AtomicUsize::new(0),
                stops: AtomicUsize::new(0),
            }),
            Arc::new(Events(Mutex::new(Vec::new()))),
        );
        coordinator.start(false).expect("first start");
        let stale = coordinator
            .worker_token(RealtimeTaskPhase::Recording)
            .expect("stale token");
        coordinator
            .recording_error(&stale, "recording_device_error".into())
            .expect("first failure");
        coordinator.start(false).expect("second start");
        assert_eq!(coordinator.snapshot().phase, RealtimeTaskPhase::Recording);
        assert_eq!(
            coordinator.recording_error(&stale, "recording_device_error".into()),
            Err(CoordinatorError::Busy)
        );
        assert_eq!(coordinator.snapshot().phase, RealtimeTaskPhase::Recording);
        assert_ne!(coordinator.snapshot().generation, stale.generation);
    }

    #[test]
    fn callbacks_require_the_phase_issued_by_the_coordinator() {
        let coordinator = RealtimeCoordinator::new(
            Arc::new(FakeRecording {
                starts: AtomicUsize::new(0),
                stops: AtomicUsize::new(0),
            }),
            Arc::new(Events(Mutex::new(Vec::new()))),
        );
        coordinator.start(false).expect("start");
        let recording = coordinator
            .worker_token(RealtimeTaskPhase::Recording)
            .expect("recording token");
        assert_eq!(
            coordinator.submission_accepted(&recording, "session".into()),
            Err(CoordinatorError::Busy)
        );
        coordinator.stop().expect("stop");
        let submitting = coordinator
            .worker_token(RealtimeTaskPhase::Submitting)
            .expect("submitting token");
        coordinator
            .submission_accepted(&submitting, "session".into())
            .expect("accepted");
        let transcribing = coordinator
            .worker_token(RealtimeTaskPhase::Transcribing)
            .expect("transcribing token");
        assert_eq!(
            coordinator.complete(&transcribing),
            Err(CoordinatorError::Busy)
        );
        coordinator
            .transcription_completed(&transcribing)
            .expect("completed transcription");
    }

    #[test]
    fn worker_panic_releases_the_coordinator_worker_slot() {
        let coordinator = Arc::new(RealtimeCoordinator::new(
            Arc::new(FakeRecording {
                starts: AtomicUsize::new(0),
                stops: AtomicUsize::new(0),
            }),
            Arc::new(Events(Mutex::new(Vec::new()))),
        ));
        coordinator.start(false).expect("start");
        coordinator.stop().expect("stop");
        coordinator
            .spawn_worker(|_| panic!("worker failure"))
            .expect("first worker");

        let mut replacement_started = false;
        for _ in 0..10_000 {
            if coordinator
                .spawn_worker(|cancellation| {
                    while !cancellation.is_cancelled() {
                        thread::yield_now();
                    }
                })
                .is_ok()
            {
                replacement_started = true;
                break;
            }
            thread::yield_now();
        }
        assert!(replacement_started, "panicked worker slot was not released");
        assert!(coordinator.cancel_active(coordinator.snapshot().generation));
    }

    #[test]
    fn stale_worker_completion_cannot_clear_the_new_generation_worker() {
        let coordinator = Arc::new(RealtimeCoordinator::new(
            Arc::new(FakeRecording {
                starts: AtomicUsize::new(0),
                stops: AtomicUsize::new(0),
            }),
            Arc::new(Events(Mutex::new(Vec::new()))),
        ));
        coordinator.start(false).expect("first start");
        coordinator.stop().expect("first stop");
        let old_generation = coordinator.snapshot().generation;
        let (old_done_tx, old_done_rx) = std::sync::mpsc::channel();
        coordinator
            .spawn_worker(move |cancellation| {
                while !cancellation.is_cancelled() {
                    thread::yield_now();
                }
                old_done_tx.send(()).expect("old worker done");
            })
            .expect("old worker");
        let old_token = coordinator
            .worker_token(RealtimeTaskPhase::Submitting)
            .expect("old token");
        coordinator
            .fail(
                &old_token,
                "recording_submit_failed".into(),
                RealtimeTaskFallback::None,
            )
            .expect("old failure");

        coordinator.start(false).expect("new start");
        old_done_rx
            .recv()
            .expect("old worker observes cancellation");
        coordinator.stop().expect("new stop");
        coordinator
            .spawn_worker(|cancellation| {
                while !cancellation.is_cancelled() {
                    thread::yield_now();
                }
            })
            .expect("new worker");
        assert_eq!(coordinator.snapshot().generation, old_generation.next());
        assert_eq!(
            coordinator.spawn_worker(|_| {}),
            Err(CoordinatorError::Busy)
        );
        assert!(coordinator.cancel_active(coordinator.snapshot().generation));
    }

    #[test]
    fn coordinator_issues_current_tokens_and_delivery_permit_is_one_shot() {
        let coordinator = RealtimeCoordinator::new(
            Arc::new(FakeRecording {
                starts: AtomicUsize::new(0),
                stops: AtomicUsize::new(0),
            }),
            Arc::new(Events(Mutex::new(Vec::new()))),
        );
        coordinator.start(true).expect("start");
        coordinator.stop().expect("stop");
        let submitting = coordinator
            .worker_token(RealtimeTaskPhase::Submitting)
            .expect("submitting token");
        coordinator
            .submission_accepted(&submitting, "session".into())
            .expect("accepted");
        let transcribing = coordinator
            .worker_token(RealtimeTaskPhase::Transcribing)
            .expect("transcribing token");
        coordinator
            .transcription_completed(&transcribing)
            .expect("transcription completed");
        let token = coordinator
            .worker_token(RealtimeTaskPhase::AutoPasting)
            .expect("coordinator token");
        assert!(coordinator.is_worker_current(&token));
        assert!(coordinator.acquire_delivery(&token).is_ok());
        assert_eq!(
            coordinator.acquire_delivery(&token),
            Err(CoordinatorError::Busy)
        );
        assert_eq!(
            coordinator.worker_token(RealtimeTaskPhase::Transcribing),
            Err(CoordinatorError::Busy)
        );
    }

    fn coordinator_in_auto_pasting() -> (RealtimeCoordinator<FakeRecording, Events>, WorkerToken) {
        let coordinator = RealtimeCoordinator::new(
            Arc::new(FakeRecording {
                starts: AtomicUsize::new(0),
                stops: AtomicUsize::new(0),
            }),
            Arc::new(Events(Mutex::new(Vec::new()))),
        );
        coordinator.start(true).expect("start");
        coordinator.stop().expect("stop");
        let submitting = coordinator
            .worker_token(RealtimeTaskPhase::Submitting)
            .expect("submitting token");
        coordinator
            .submission_accepted(&submitting, "session".into())
            .expect("accepted");
        let transcribing = coordinator
            .worker_token(RealtimeTaskPhase::Transcribing)
            .expect("transcribing token");
        coordinator
            .transcription_completed(&transcribing)
            .expect("transcription completed");
        let delivery = coordinator
            .worker_token(RealtimeTaskPhase::AutoPasting)
            .expect("delivery token");
        (coordinator, delivery)
    }

    #[test]
    fn cancellation_wins_before_delivery_permit_is_issued() {
        let (coordinator, token) = coordinator_in_auto_pasting();
        assert!(coordinator.cancel_active(token.generation));
        assert_eq!(
            coordinator.acquire_delivery(&token),
            Err(CoordinatorError::Cancelled)
        );
    }

    #[test]
    fn delivery_permit_wins_before_later_cancellation() {
        let (coordinator, token) = coordinator_in_auto_pasting();
        {
            let _permit = coordinator
                .acquire_delivery(&token)
                .expect("delivery permit");
            assert!(coordinator.cancel_active(token.generation));
        }
    }

    #[test]
    fn delivery_outcome_keeps_clipboard_and_history_fallback_semantics() {
        assert_eq!(DeliveryOutcome::Pasted.failure(), None);
        assert_eq!(
            DeliveryOutcome::ClipboardOnly.failure(),
            Some((
                "recording_accessibility_required".into(),
                RealtimeTaskFallback::Clipboard
            ))
        );
        assert_eq!(
            DeliveryOutcome::Failed {
                code: "recording_auto_paste_failed".into(),
                clipboard_written: true,
            }
            .failure(),
            Some((
                "recording_auto_paste_failed".into(),
                RealtimeTaskFallback::Clipboard
            ))
        );
        assert_eq!(
            DeliveryOutcome::Failed {
                code: "recording_auto_paste_failed".into(),
                clipboard_written: false,
            }
            .failure(),
            Some((
                "recording_auto_paste_failed".into(),
                RealtimeTaskFallback::History
            ))
        );
    }

    #[test]
    fn shutdown_blocks_new_work_cancels_and_joins_active_worker() {
        let coordinator = Arc::new(RealtimeCoordinator::new(
            Arc::new(FakeRecording {
                starts: AtomicUsize::new(0),
                stops: AtomicUsize::new(0),
            }),
            Arc::new(Events(Mutex::new(Vec::new()))),
        ));
        coordinator.start(false).expect("start");
        coordinator.stop().expect("stop");
        coordinator
            .spawn_worker(|cancellation| {
                while !cancellation.is_cancelled() {
                    thread::yield_now();
                }
            })
            .expect("worker");
        assert!(coordinator.shutdown(Duration::from_secs(1)));
        assert_eq!(coordinator.start(false), Err(CoordinatorError::Busy));
    }

    struct SubmissionFake {
        calls: AtomicUsize,
        wav_lengths: Mutex<Vec<usize>>,
    }

    impl SubmissionGateway for SubmissionFake {
        fn submit(
            &self,
            request: SubmissionRequest,
            _context: Option<&ContextPayload>,
        ) -> Result<SubmissionAccepted, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.wav_lengths
                .lock()
                .expect("wav lengths mutex")
                .push(request.wav.len());
            Ok(SubmissionAccepted {
                session_id: "session-1".into(),
                status: "transcribing".into(),
            })
        }
    }

    fn captured_audio(pcm: Vec<f32>, sample_rate: u32) -> CapturedAudio {
        CapturedAudio {
            pcm,
            sample_rate,
            input_device: "fake microphone".into(),
        }
    }

    #[test]
    fn submission_core_resamples_and_encodes_without_runtime_dependency() {
        let gateway = SubmissionFake {
            calls: AtomicUsize::new(0),
            wav_lengths: Mutex::new(Vec::new()),
        };
        let worker = SubmissionWorker::new(gateway);
        let accepted = worker
            .run(
                captured_audio(vec![0.25; 48_000], 48_000),
                Some(ContextPayload::new(vec![1, 2, 3])),
            )
            .expect("submission");
        assert_eq!(accepted.session_id, "session-1");
        assert_eq!(worker.gateway.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            worker.gateway.wav_lengths.lock().unwrap()[0],
            44 + 16_000 * 2
        );
    }

    #[test]
    fn empty_audio_and_invalid_sample_rate_do_not_call_gateway() {
        for (pcm, sample_rate, expected) in [
            (Vec::new(), 16_000, SubmissionWorkerError::NoAudio),
            (vec![0.0; 16_000], 16_000, SubmissionWorkerError::NoAudio),
            (
                vec![1.0 / (32767.0 * 3.0); 16_000],
                16_000,
                SubmissionWorkerError::NoAudio,
            ),
            (vec![0.0], 0, SubmissionWorkerError::InvalidSampleRate),
        ] {
            let gateway = SubmissionFake {
                calls: AtomicUsize::new(0),
                wav_lengths: Mutex::new(Vec::new()),
            };
            let worker = SubmissionWorker::new(gateway);
            assert_eq!(
                worker.run(captured_audio(pcm, sample_rate), None),
                Err(expected)
            );
            assert_eq!(worker.gateway.calls.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn stale_submission_token_is_rejected_before_gateway_call() {
        let gateway = SubmissionFake {
            calls: AtomicUsize::new(0),
            wav_lengths: Mutex::new(Vec::new()),
        };
        let worker = SubmissionWorker::new(gateway);
        let snapshot = RealtimeTaskSnapshot {
            generation: TaskGeneration(2),
            task_id: Some("task-2".into()),
            phase: RealtimeTaskPhase::Submitting,
            revision: 3,
            ..Default::default()
        };
        let stale = WorkerToken {
            generation: TaskGeneration(1),
            task_id: "task-1".into(),
            expected_phase: RealtimeTaskPhase::Submitting,
            revision: 3,
        };
        assert_eq!(
            worker.run_with_token(
                captured_audio(vec![0.0; 16], 16_000),
                None,
                &snapshot,
                &stale,
            ),
            Err(SubmissionWorkerError::StaleWorker)
        );
        assert_eq!(worker.gateway.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn coordinator_claims_submission_once_per_generation() {
        let coordinator = RealtimeCoordinator::new(
            Arc::new(FakeRecording {
                starts: AtomicUsize::new(0),
                stops: AtomicUsize::new(0),
            }),
            Arc::new(Events(Mutex::new(Vec::new()))),
        );
        coordinator.start(false).expect("start");
        coordinator.stop().expect("stop");
        let token = coordinator
            .worker_token(RealtimeTaskPhase::Submitting)
            .expect("submitting token");
        assert!(coordinator.commit_submission(&token));
        assert!(!coordinator.commit_submission(&token));
    }

    struct PollGateway {
        statuses: Mutex<Vec<Result<SessionStatus, String>>>,
        calls: AtomicUsize,
    }

    impl SessionStatusGateway for PollGateway {
        fn status(&self, _session_id: &str) -> Result<SessionStatus, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.statuses.lock().expect("poll status mutex").remove(0)
        }
    }

    struct PollSleeperFake(Mutex<Vec<Duration>>);

    impl PollSleeper for PollSleeperFake {
        fn sleep(&self, duration: Duration, _cancelled: &dyn CancellationSignal) -> bool {
            self.0.lock().expect("poll sleeper mutex").push(duration);
            true
        }
    }

    #[test]
    fn polling_resets_failure_backoff_after_transcribing() {
        let gateway = PollGateway {
            statuses: Mutex::new(vec![
                Err("connection".into()),
                Ok(SessionStatus::Transcribing),
                Err("connection".into()),
                Ok(SessionStatus::Completed),
            ]),
            calls: AtomicUsize::new(0),
        };
        let sleeper = PollSleeperFake(Mutex::new(Vec::new()));
        assert_eq!(
            poll_until_terminal(&gateway, &sleeper, "session-1", &AtomicBool::new(false)),
            PollOutcome::Completed
        );
        assert_eq!(
            *sleeper.0.lock().expect("poll sleeper mutex"),
            vec![
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1)
            ]
        );
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn polling_treats_cleaning_up_as_non_terminal() {
        let gateway = PollGateway {
            statuses: Mutex::new(vec![
                Ok(SessionStatus::CleaningUp),
                Ok(SessionStatus::Completed),
            ]),
            calls: AtomicUsize::new(0),
        };
        let sleeper = PollSleeperFake(Mutex::new(Vec::new()));
        assert_eq!(
            poll_until_terminal(&gateway, &sleeper, "session-1", &AtomicBool::new(false)),
            PollOutcome::Completed
        );
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            *sleeper.0.lock().expect("poll sleeper mutex"),
            vec![Duration::from_secs(1)]
        );
    }

    #[test]
    fn polling_maps_fourth_consecutive_failure_to_connection() {
        let gateway = PollGateway {
            statuses: Mutex::new(vec![
                Err("connection".into()),
                Err("connection".into()),
                Err("connection".into()),
                Err("connection".into()),
            ]),
            calls: AtomicUsize::new(0),
        };
        let sleeper = PollSleeperFake(Mutex::new(Vec::new()));
        assert_eq!(
            poll_until_terminal(&gateway, &sleeper, "session-1", &AtomicBool::new(false)),
            PollOutcome::Failed("connection".into())
        );
        assert_eq!(
            *sleeper.0.lock().expect("poll sleeper mutex"),
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4)
            ]
        );
    }

    #[test]
    fn polling_cancellation_prevents_status_request() {
        let gateway = PollGateway {
            statuses: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        };
        let sleeper = PollSleeperFake(Mutex::new(Vec::new()));
        assert_eq!(
            poll_until_terminal(&gateway, &sleeper, "session-1", &AtomicBool::new(true)),
            PollOutcome::Cancelled
        );
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn stale_polling_worker_is_cancelled_before_status_request() {
        let gateway = PollGateway {
            statuses: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        };
        let sleeper = PollSleeperFake(Mutex::new(Vec::new()));
        let snapshot = RealtimeTaskSnapshot {
            generation: TaskGeneration(2),
            task_id: Some("task-2".into()),
            phase: RealtimeTaskPhase::Transcribing,
            revision: 4,
            ..Default::default()
        };
        let mut stale = WorkerToken {
            generation: TaskGeneration(1),
            task_id: "task-1".into(),
            expected_phase: RealtimeTaskPhase::Transcribing,
            revision: 4,
        };
        assert_eq!(
            poll_until_terminal_with_token(
                &gateway,
                &sleeper,
                "session-1",
                &AtomicBool::new(false),
                &mut stale,
                &snapshot,
                |_| true,
                |token, _| Some(token.clone()),
            ),
            PollOutcome::Cancelled
        );
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn polling_refreshes_exact_token_when_cleaning_up_is_observed() {
        let coordinator = RealtimeCoordinator::new(
            Arc::new(FakeRecording {
                starts: AtomicUsize::new(0),
                stops: AtomicUsize::new(0),
            }),
            Arc::new(Events(Mutex::new(Vec::new()))),
        );
        coordinator.start(false).expect("start");
        coordinator.stop().expect("stop");
        let submitting = coordinator
            .worker_token(RealtimeTaskPhase::Submitting)
            .expect("submitting token");
        coordinator
            .submission_accepted(&submitting, "session-1".into())
            .expect("accepted");
        let transcribing = coordinator.snapshot();
        let mut token = coordinator
            .worker_token(RealtimeTaskPhase::Transcribing)
            .expect("transcribing token");
        let old_token = token.clone();
        let gateway = PollGateway {
            statuses: Mutex::new(vec![
                Ok(SessionStatus::CleaningUp),
                Ok(SessionStatus::CleaningUp),
                Ok(SessionStatus::Transcribing),
                Ok(SessionStatus::Completed),
            ]),
            calls: AtomicUsize::new(0),
        };
        let sleeper = PollSleeperFake(Mutex::new(Vec::new()));

        assert_eq!(
            poll_until_terminal_with_token(
                &gateway,
                &sleeper,
                "session-1",
                &AtomicBool::new(false),
                &mut token,
                &transcribing,
                |candidate| coordinator.is_worker_current(candidate),
                |candidate, status| coordinator.observe_session_status(candidate, status).ok(),
            ),
            PollOutcome::Completed
        );
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 4);
        assert_eq!(token.generation, old_token.generation);
        assert_eq!(token.expected_phase, RealtimeTaskPhase::CleaningUp);
        assert_eq!(token.revision, old_token.revision + 1);
        assert!(coordinator.is_worker_current(&token));
        assert!(!coordinator.is_worker_current(&old_token));

        let forged_old_revision = WorkerToken {
            expected_phase: RealtimeTaskPhase::CleaningUp,
            ..old_token.clone()
        };
        assert_eq!(
            coordinator.observe_session_status(&forged_old_revision, SessionStatus::CleaningUp),
            Err(CoordinatorError::Busy)
        );
        coordinator
            .session_completed(&token)
            .expect("current polling token completes session");
        assert_eq!(coordinator.snapshot().phase, RealtimeTaskPhase::Completed);
        assert!(!coordinator.is_worker_current(&token));
        assert_eq!(
            coordinator.observe_session_status(&token, SessionStatus::CleaningUp),
            Err(CoordinatorError::Busy)
        );
    }

    #[test]
    fn polling_maps_daemon_failure_reason_to_stable_codes() {
        for (reason, expected) in [
            (Some("no_speech"), "no_speech_detected"),
            (Some("unexpected-reason"), "recording_transcription_failed"),
            (None, "recording_transcription_failed"),
        ] {
            let gateway = PollGateway {
                statuses: Mutex::new(vec![Ok(SessionStatus::Failed(reason.map(str::to_owned)))]),
                calls: AtomicUsize::new(0),
            };
            let sleeper = PollSleeperFake(Mutex::new(Vec::new()));
            assert_eq!(
                poll_until_terminal(&gateway, &sleeper, "session-1", &AtomicBool::new(false)),
                PollOutcome::Failed(expected.into()),
                "reason={reason:?}"
            );
        }
        assert_eq!(
            stable_failure_code("no_speech_detected"),
            "no_speech_detected"
        );
    }

    struct PipelineGateway {
        statuses: Mutex<Vec<Result<SessionStatus, String>>>,
    }

    impl SubmissionGateway for PipelineGateway {
        fn submit(
            &self,
            _: SubmissionRequest,
            _: Option<&ContextPayload>,
        ) -> Result<SubmissionAccepted, String> {
            Ok(SubmissionAccepted {
                session_id: "session-1".into(),
                status: "transcribing".into(),
            })
        }
    }

    impl SessionStatusGateway for PipelineGateway {
        fn status(&self, _: &str) -> Result<SessionStatus, String> {
            self.statuses
                .lock()
                .expect("pipeline gateway mutex")
                .remove(0)
        }
    }

    struct PasteInjector;

    impl TextDeliveryPort for PasteInjector {
        fn deliver(&self, _: &str, _: DeliveryPermit) -> DeliveryOutcome {
            DeliveryOutcome::Pasted
        }
    }

    fn to_audio(pcm: Vec<f32>) -> (CapturedAudio, Option<ContextPayload>) {
        (
            CapturedAudio {
                pcm,
                sample_rate: 16_000,
                input_device: "fake".into(),
            },
            None,
        )
    }

    fn failed_pipeline_snapshot(
        statuses: Vec<Result<SessionStatus, String>>,
    ) -> (RealtimeTaskSnapshot, RealtimePipelineError) {
        let coordinator = RealtimeCoordinator::new(
            Arc::new(FakeRecording {
                starts: AtomicUsize::new(0),
                stops: AtomicUsize::new(0),
            }),
            Arc::new(Events(Mutex::new(Vec::new()))),
        );
        coordinator.start(false).expect("start");
        // stop() 只推进状态机；提交音频必须非全零，否则 prepare_submission 按
        // 纯静音拦截为 recording_no_audio，轮询阶段根本不会发生。
        let _ = coordinator.stop().expect("stop");
        let gateway = Arc::new(PipelineGateway {
            statuses: Mutex::new(statuses),
        });
        let error = run_after_stop(
            &coordinator,
            vec![0.2; 160],
            to_audio,
            gateway,
            &PollSleeperFake(Mutex::new(Vec::new())),
            &PasteInjector,
            &AtomicBool::new(false),
        )
        .expect_err("terminal failure must surface as a pipeline error");
        (coordinator.snapshot(), error)
    }

    #[test]
    fn no_speech_failure_keeps_session_but_offers_no_history_fallback() {
        let (snapshot, error) =
            failed_pipeline_snapshot(vec![Ok(SessionStatus::Failed(Some("no_speech".into())))]);
        assert_eq!(
            error,
            RealtimePipelineError::Polling("no_speech_detected".into())
        );
        assert_eq!(snapshot.phase, RealtimeTaskPhase::Failed);
        assert_eq!(snapshot.failure_code.as_deref(), Some("no_speech_detected"));
        // 会话仍保留（session_id 未丢），但不存在可恢复正文，因此不给 History 引导。
        assert_eq!(snapshot.session_id.as_deref(), Some("session-1"));
        assert_eq!(snapshot.fallback, RealtimeTaskFallback::None);
    }

    #[test]
    fn definite_transcription_failure_offers_no_history_fallback() {
        for reason in [None, Some("unexpected-reason")] {
            let (snapshot, error) = failed_pipeline_snapshot(vec![Ok(SessionStatus::Failed(
                reason.map(str::to_owned),
            ))]);
            assert_eq!(
                error,
                RealtimePipelineError::Polling("recording_transcription_failed".into()),
                "reason={reason:?}"
            );
            assert_eq!(
                snapshot.fallback,
                RealtimeTaskFallback::None,
                "reason={reason:?}"
            );
        }
    }

    #[test]
    fn uncertain_connection_failure_keeps_history_fallback() {
        let (snapshot, error) = failed_pipeline_snapshot(vec![
            Err("recording_status_failed".into()),
            Err("recording_status_failed".into()),
            Err("recording_status_failed".into()),
            Err("recording_status_failed".into()),
        ]);
        assert_eq!(error, RealtimePipelineError::Polling("connection".into()));
        assert_eq!(snapshot.fallback, RealtimeTaskFallback::History);
    }

    #[test]
    fn terminal_display_duration_splits_completed_hint_and_error_levels() {
        let base = RealtimeTaskSnapshot::default();
        let completed = RealtimeTaskSnapshot {
            phase: RealtimeTaskPhase::Completed,
            ..base.clone()
        };
        assert_eq!(
            terminal_display_duration(&completed),
            Duration::from_secs(1)
        );

        for code in HINT_LEVEL_FAILURE_CODES {
            let hint = RealtimeTaskSnapshot {
                phase: RealtimeTaskPhase::Failed,
                failure_code: Some((*code).into()),
                ..base.clone()
            };
            assert_eq!(
                terminal_display_duration(&hint),
                Duration::from_secs(3),
                "hint code={code}"
            );
        }
        for code in [
            "recording_transcription_failed",
            "connection",
            "recording_microphone_unauthorized",
        ] {
            let error = RealtimeTaskSnapshot {
                phase: RealtimeTaskPhase::Failed,
                failure_code: Some(code.into()),
                ..base.clone()
            };
            assert_eq!(
                terminal_display_duration(&error),
                Duration::from_secs(5),
                "error code={code}"
            );
        }
        let non_terminal = RealtimeTaskSnapshot {
            phase: RealtimeTaskPhase::Recording,
            ..base
        };
        assert_eq!(
            terminal_display_duration(&non_terminal),
            Duration::from_secs(5),
            "failure presented for a non-terminal snapshot stays error-level"
        );
    }
}
