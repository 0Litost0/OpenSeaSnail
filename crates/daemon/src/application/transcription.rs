//! 创建、Retry 与 canonical 后台转写用例。
//!
//! HTTP adapter 只负责 multipart 解包与响应映射；提交点、账户绑定、runtime
//! reservation、后台任务所有权和最终状态推进全部由本模块独占。

use prost::Message;
use seasnail_proto::seasnail::v1::{
    CleanupFile, ClipboardContextFile, Source, Speaker, TranscriptFile, TranscriptUnit,
    UnitGranularity,
};
use seasnail_runtime::{
    AudioNormalizer, CanonicalTranscript, Granularity, ReservedTranscription, RuntimeFailure,
    RuntimeOperation, TranscriptionEngine, TranscriptionOutcome, TranscriptionRequest,
};
use seasnail_storage::RawCheckpointDecision;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;

use super::services::require_scope;
use super::{
    AcceptedTranscription, ApplicationError, CreateTranscriptionCommand, ModelCatalog,
    ModelRuntimeKind,
};
use crate::cleanup::{CleanupExecution, CleanupService};
use crate::model_settings::ModelSettings;

const MAX_TRANSCRIBE_DURATION: Duration = Duration::from_secs(60 * 60);
const SENSEVOICE_RETRY_MESSAGE: &str =
    "SenseVoice 本地转写失败，可重试；如持续失败，请恢复上一稳定版本。";
/// Stable failure_reason for a successfully executed transcription that produced
/// no visible speech. Consumed by the desktop as `no_speech_detected`.
const NO_SPEECH_REASON: &str = "no_speech";

/// Explicit business terminal of a pipeline run. `NoSpeech` is a successful
/// execution outcome, never modeled as an error string.
enum PipelineTerminal {
    Completed,
    NoSpeech,
}

/// Real failure channel of the pipeline. Reasons keep the existing safe display
/// strings (SenseVoice maps to a fixed retry message); `NoSpeech` never passes
/// through here.
type PipelineError = anyhow::Error;

fn snapshot_or_default(
    result: Result<super::DictionarySnapshot, super::DictionaryError>,
    session_id: &str,
) -> super::DictionarySnapshot {
    match result {
        Ok(snapshot) => snapshot,
        Err(_) => {
            tracing::warn!(
                session_id,
                error_code = "dictionary_snapshot_unavailable",
                "dictionary snapshot unavailable; continuing with empty snapshot"
            );
            super::DictionarySnapshot::default()
        }
    }
}

#[derive(Default)]
struct BackgroundTaskOwner {
    tasks: Mutex<HashMap<String, TrackedTask>>,
    retired: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    next_generation: AtomicU64,
    closing: AtomicBool,
}

struct TrackedTask {
    generation: u64,
    abort: tokio::task::AbortHandle,
    join: Option<tokio::task::JoinHandle<()>>,
}

struct TaskRegistration {
    owner: std::sync::Weak<BackgroundTaskOwner>,
    session_id: String,
    generation: u64,
}

impl Drop for TaskRegistration {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            owner.finish(&self.session_id, self.generation);
        }
    }
}

impl Drop for BackgroundTaskOwner {
    fn drop(&mut self) {
        for task in self
            .tasks
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
        {
            task.abort.abort();
        }
    }
}

impl BackgroundTaskOwner {
    fn spawn(self: &Arc<Self>, session_id: String, job: PipelineJob) {
        // 启动 barrier 保证 handle 先登记，任务再执行；完成后由任务自身注销。
        let mut tasks = self.tasks.lock().unwrap_or_else(|p| p.into_inner());
        if self.closing.load(Ordering::Acquire) {
            return;
        }
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let owner = Arc::downgrade(self);
        let tracked_id = session_id.clone();
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let task = tokio::spawn(async move {
            let _ = ready_rx.await;
            let _registration = TaskRegistration {
                owner,
                session_id: tracked_id,
                generation,
            };
            run_pipeline(job).await;
        });
        tasks.insert(
            session_id,
            TrackedTask {
                generation,
                abort: task.abort_handle(),
                join: Some(task),
            },
        );
        drop(tasks);
        let _ = ready_tx.send(());
    }

    fn close(&self) {
        self.closing.store(true, Ordering::Release);
        for task in self
            .tasks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
        {
            task.abort.abort();
        }
    }

    async fn shutdown(&self) {
        self.close();
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(|p| p.into_inner()));
        let retired = std::mem::take(&mut *self.retired.lock().unwrap_or_else(|p| p.into_inner()));
        for join in retired {
            let _ = join.await;
        }
        for task in tasks.into_values() {
            if let Some(join) = task.join {
                let _ = join.await;
            }
        }
    }

    fn finish(&self, session_id: &str, generation: u64) {
        let mut tasks = self
            .tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if tasks
            .get(session_id)
            .is_some_and(|task| task.generation == generation)
        {
            tasks.remove(session_id);
        }
    }

    fn cancel(&self, session_id: &str) {
        if let Some(task) = self
            .tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(session_id)
        {
            task.abort.abort();
            if let Some(join) = task.join {
                let mut retired = self.retired.lock().unwrap_or_else(|p| p.into_inner());
                retired.retain(|task| !task.is_finished());
                retired.push(join);
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct PipelineTaskController {
    owner: Arc<BackgroundTaskOwner>,
}

impl PipelineTaskController {
    pub(crate) fn new() -> Self {
        Self {
            owner: Arc::new(BackgroundTaskOwner::default()),
        }
    }

    fn spawn(&self, session_id: String, job: PipelineJob) {
        self.owner.spawn(session_id, job);
    }

    pub(crate) fn cancel(&self, session_id: &str) {
        self.owner.cancel(session_id);
    }

    pub(crate) fn close(&self) {
        self.owner.close();
    }

    pub(crate) async fn shutdown(&self) {
        self.owner.shutdown().await;
    }
}

pub struct TranscriptionService {
    engine: Arc<dyn TranscriptionEngine>,
    normalizer: Arc<AudioNormalizer>,
    settings: Arc<ModelSettings>,
    catalog: Arc<ModelCatalog>,
    mutation_locks: Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
    task_controller: PipelineTaskController,
    new_id: Arc<dyn Fn() -> String + Send + Sync>,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    cleanup: Arc<CleanupService>,
    dictionary: Arc<super::DictionaryService>,
}

impl std::fmt::Debug for TranscriptionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TranscriptionService")
            .finish_non_exhaustive()
    }
}

impl TranscriptionService {
    pub(crate) fn close_for_shutdown(&self) {
        self.task_controller.close();
    }

    pub(crate) async fn shutdown(&self) {
        self.task_controller.shutdown().await;
    }

    pub(crate) fn new(
        engine: Arc<dyn TranscriptionEngine>,
        normalizer: Arc<AudioNormalizer>,
        settings: Arc<ModelSettings>,
        catalog: Arc<ModelCatalog>,
        mutation_locks: Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
        task_controller: PipelineTaskController,
        cleanup: Arc<CleanupService>,
        dictionary: Arc<super::DictionaryService>,
    ) -> Self {
        Self {
            engine,
            normalizer,
            settings,
            catalog,
            mutation_locks,
            task_controller,
            new_id: Arc::new(|| uuid::Uuid::new_v4().to_string()),
            now: Arc::new(|| chrono::Utc::now().timestamp()),
            cleanup,
            dictionary,
        }
    }

    pub async fn create(
        &self,
        command: CreateTranscriptionCommand,
    ) -> Result<AcceptedTranscription, ApplicationError> {
        require_scope(&command.caller, "sessions:write")?;
        let session_id = (self.new_id)();
        let created_at = (self.now)();
        let source = normalize_source(&command.source);
        let language = normalize_language(command.language.as_deref());
        let file_name = safe_file_name(Some(&command.file_name));
        let repository = command.caller.repository_clone();
        let account_id = command.caller.account_id().to_owned();

        // gate-first reservation 在任何持久化前完成；失败时没有可见 session 或文件。
        let reservation = self
            .engine
            .reserve(&session_id)
            .await
            .map_err(reservation_error)?;
        let model_id = reservation.model_id().to_owned();
        let storage = repository.storage();
        let cleanup_enabled = source == "realtime"
            && storage
                .get_cleanup_settings()
                .map_err(ApplicationError::from)?
                .enabled;
        let context_present = if let Some(bytes) = command.clipboard_context {
            let mut context = ClipboardContextFile::decode(&*bytes)
                .map_err(|_| ApplicationError::InvalidInput("invalid clipboard context".into()))?;
            if !context.session_id.is_empty() {
                return Err(ApplicationError::InvalidInput(
                    "invalid clipboard context metadata".into(),
                ));
            }
            context.session_id = session_id.clone();
            crate::account::Storage::validate_context_manifest(&context)
                .map_err(|_| ApplicationError::InvalidInput("invalid clipboard context".into()))?;
            storage.write_context(&session_id, created_at, &context)?;
            true
        } else {
            false
        };
        let audio_rel = storage.write_audio(&session_id, created_at, &command.audio)?;
        storage.insert_session(&seasnail_storage::SessionRow {
            id: session_id.clone(),
            account_id: account_id.clone(),
            created_at,
            source: source.clone(),
            language: language.clone(),
            duration_sec: 0.0,
            status: "transcribing".into(),
            model: model_id.clone(),
            input_device: command.input_device,
            file_name: Some(file_name.clone()),
            audio_path: Some(audio_rel),
            transcript_path: None,
            failure_reason: None,
            context_present,
            cleanup_status: "not_requested".into(),
            cleanup_path: None,
            cleanup_error_code: None,
        })?;

        let dictionary_snapshot =
            snapshot_or_default(self.dictionary.snapshot(&command.caller), &session_id);
        self.task_controller.spawn(
            session_id.clone(),
            self.pipeline_job(
                repository,
                reservation,
                session_id.clone(),
                account_id,
                created_at,
                source,
                language,
                command.audio,
                extension_for(&file_name),
                model_id,
                cleanup_enabled,
                dictionary_snapshot,
            ),
        );
        Ok(AcceptedTranscription {
            session_id,
            status: "transcribing".into(),
        })
    }

    pub async fn retry(
        &self,
        caller: &super::CallerContext,
        session_id: &str,
    ) -> Result<AcceptedTranscription, ApplicationError> {
        require_scope(caller, "sessions:write")?;
        let lock = mutation_lock(&self.mutation_locks, session_id)?;
        let repository = caller.repository_clone();
        let storage = repository.storage();
        {
            let _mutation = lock
                .lock()
                .map_err(|_| ApplicationError::Internal("session mutation lock poisoned".into()))?;
            validate_retry_row(storage, session_id)?;
        }
        // std::sync::MutexGuard 不跨 await。取得 reservation 后再次在同一 mutation
        // coordinator 内读取并验证，删除/编辑若在等待期间获胜会安全返回 404/409。
        let reservation = self
            .engine
            .reserve_retry(session_id)
            .await
            .map_err(reservation_error)?;
        let model_id = reservation.model_id().to_owned();
        let _mutation = lock
            .lock()
            .map_err(|_| ApplicationError::Internal("session mutation lock poisoned".into()))?;
        let row = validate_retry_row(storage, session_id)?;
        let cleanup_enabled = row.source == "realtime"
            && storage
                .get_cleanup_settings()
                .map_err(ApplicationError::from)?
                .enabled;
        let audio_rel = row
            .audio_path
            .as_deref()
            .ok_or_else(|| ApplicationError::Conflict("cannot retry: no persisted audio".into()))?;
        // 所有可失败输入读取都在状态提交前完成；失败后仍保持 failed 可再次 retry。
        let audio = storage.read_audio(audio_rel)?;
        if row.context_present {
            storage.read_context(session_id, row.created_at)?;
        }
        // 删除上一代 raw/cleanup artifact 后再清空 DB 引用；否则 crash reconcile 可能
        // 把旧 transcript 或 cleanup 当成这一代 retry 的 checkpoint/终态。
        storage.remove_transcript(session_id, row.created_at)?;
        storage.remove_cleanup(session_id, row.created_at)?;
        if !storage.begin_retry(session_id, &row.model, &model_id)? {
            return Err(ApplicationError::Conflict(
                "session changed before retry commit".into(),
            ));
        }
        self.task_controller.spawn(
            session_id.to_owned(),
            self.pipeline_job(
                repository,
                reservation,
                session_id.to_owned(),
                row.account_id,
                row.created_at,
                row.source,
                row.language,
                audio,
                extension_for(row.file_name.as_deref().unwrap_or("audio.wav")),
                model_id,
                cleanup_enabled,
                snapshot_or_default(self.dictionary.snapshot(caller), session_id),
            ),
        );
        Ok(AcceptedTranscription {
            session_id: session_id.to_owned(),
            status: "transcribing".into(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn pipeline_job(
        &self,
        repository: super::AccountScopedRepository,
        reservation: ReservedTranscription,
        session_id: String,
        account_id: String,
        created_at: i64,
        source: String,
        language: String,
        audio: Vec<u8>,
        extension: String,
        model_id: String,
        cleanup_enabled: bool,
        dictionary_snapshot: super::DictionarySnapshot,
    ) -> PipelineJob {
        let sensevoice = self.catalog.get(&model_id).is_some_and(|entry| {
            matches!(
                entry.runtime,
                ModelRuntimeKind::Gguf | ModelRuntimeKind::SherpaOnnx
            )
        });
        PipelineJob {
            repository,
            engine: Arc::clone(&self.engine),
            normalizer: Arc::clone(&self.normalizer),
            settings: Arc::clone(&self.settings),
            reservation: Some(reservation),
            session_id,
            account_id,
            created_at,
            source,
            language,
            audio,
            extension,
            model_id,
            sensevoice,
            cleanup_enabled,
            cleanup: Arc::clone(&self.cleanup),
            dictionary_snapshot,
            mutation_locks: Arc::clone(&self.mutation_locks),
        }
    }
}

fn validate_retry_row(
    storage: &crate::account::Storage,
    session_id: &str,
) -> Result<seasnail_storage::SessionRow, ApplicationError> {
    let row = storage
        .get(session_id)?
        .ok_or_else(|| ApplicationError::NotFound(format!("session not found: {session_id}")))?;
    if row.status != "failed" {
        return Err(ApplicationError::Conflict(format!(
            "session not failed (status={}); cannot retry",
            row.status
        )));
    }
    if row.audio_path.is_none() {
        return Err(ApplicationError::Conflict(
            "cannot retry: no persisted audio".into(),
        ));
    }
    Ok(row)
}

pub(super) fn mutation_lock(
    locks: &Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
    id: &str,
) -> Result<Arc<Mutex<()>>, ApplicationError> {
    let mut locks = locks
        .lock()
        .map_err(|_| ApplicationError::Internal("session mutation lock poisoned".into()))?;
    if let Some(lock) = locks.get(id).and_then(Weak::upgrade) {
        Ok(lock)
    } else {
        let lock = Arc::new(Mutex::new(()));
        locks.insert(id.to_owned(), Arc::downgrade(&lock));
        Ok(lock)
    }
}

fn reservation_error(error: RuntimeFailure) -> ApplicationError {
    match error {
        RuntimeFailure::Busy(occupied) => match occupied.operation {
            RuntimeOperation::Transcription { session_id } => ApplicationError::Conflict(format!(
                "transcription slot occupied by session {session_id}"
            )),
            RuntimeOperation::ModelSwitch { model_id } => ApplicationError::Conflict(format!(
                "transcription slot occupied by model switch {model_id}"
            )),
        },
        RuntimeFailure::Unavailable => {
            ApplicationError::Conflict("no active ASR runtime; activate a model first".into())
        }
        RuntimeFailure::Preparation(message) | RuntimeFailure::Administration(message) => {
            ApplicationError::Internal(message)
        }
        RuntimeFailure::Runtime(error) => ApplicationError::Internal(error.to_string()),
    }
}

struct PipelineJob {
    repository: super::AccountScopedRepository,
    engine: Arc<dyn TranscriptionEngine>,
    normalizer: Arc<AudioNormalizer>,
    settings: Arc<ModelSettings>,
    reservation: Option<ReservedTranscription>,
    session_id: String,
    account_id: String,
    created_at: i64,
    source: String,
    language: String,
    audio: Vec<u8>,
    extension: String,
    model_id: String,
    sensevoice: bool,
    cleanup_enabled: bool,
    cleanup: Arc<CleanupService>,
    dictionary_snapshot: super::DictionarySnapshot,
    mutation_locks: Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
}

async fn run_pipeline(job: PipelineJob) {
    let repository = job.repository.clone();
    let session_id = job.session_id.clone();
    let model_id = job.model_id.clone();
    let mutation_locks = Arc::clone(&job.mutation_locks);
    match run_pipeline_inner(job).await {
        Ok(PipelineTerminal::Completed) => {}
        Ok(PipelineTerminal::NoSpeech) => {
            tracing::info!(
                session = %session_id,
                error_code = "no_speech",
                "transcription completed without visible speech; retaining audio"
            );
            persist_failure_terminal(
                &mutation_locks,
                &repository,
                &session_id,
                &model_id,
                NO_SPEECH_REASON,
            );
        }
        Err(error) => {
            let reason = error.to_string();
            tracing::warn!(
                session = %session_id,
                error_code = "transcription_failed",
                "transcription failed; retaining audio"
            );
            persist_failure_terminal(
                &mutation_locks,
                &repository,
                &session_id,
                &model_id,
                &reason,
            );
        }
    }
}

/// Shared CAS persistence for failure terminals (NoSpeech and real failures).
/// A late result must not overwrite a row that was deleted, retried, or won by
/// a newer model worker: the in-process mutation lock serializes contenders and
/// `fail_transcription`'s `WHERE status='transcribing' AND model=?` is the
/// final atomic guard.
fn persist_failure_terminal(
    mutation_locks: &Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
    repository: &super::AccountScopedRepository,
    session_id: &str,
    model_id: &str,
    reason: &str,
) {
    let persisted = mutation_lock(mutation_locks, session_id).and_then(|lock| {
        let _mutation = lock
            .lock()
            .map_err(|_| ApplicationError::Internal("session mutation lock poisoned".into()))?;
        let storage = repository.storage();
        let row = storage.get(session_id).map_err(ApplicationError::from)?;
        if !row.is_some_and(|row| row.status == "transcribing" && row.model == model_id) {
            return Ok(false);
        }
        storage
            .fail_transcription(session_id, model_id, reason)
            .map_err(ApplicationError::from)
    });
    match persisted {
        Ok(true) => {}
        Ok(false) => tracing::debug!(
            session = %session_id,
            error_code = "failure_outcome_superseded",
            "failure outcome skipped because the session was deleted or superseded"
        ),
        Err(_) => tracing::error!(
            session = %session_id,
            error_code = "failure_outcome_persist_failed",
            "failure terminal persistence also failed; row stuck at transcribing"
        ),
    }
}

async fn run_pipeline_inner(mut job: PipelineJob) -> Result<PipelineTerminal, PipelineError> {
    let local_transcription_started = Instant::now();
    let settings = job.settings.snapshot();
    let raw: NamedTempFile = job
        .normalizer
        .create_temp("seasnail-raw-", &job.extension)?;
    std::fs::write(raw.path(), &job.audio)?;
    // realtime 提交在桌面端已经由 cpal PCM 封装为 16 kHz mono s16le WAV。
    // 文件导入仍需 ffmpeg 处理任意容器、采样率和声道；两条路径在这里明确分流。
    let normalized = if job.source == "realtime" {
        None
    } else {
        Some(job.normalizer.normalize(raw.path()).await?)
    };
    let wav_path = normalized
        .as_ref()
        .map(|wav| wav.path().to_path_buf())
        .unwrap_or_else(|| raw.path().to_path_buf());
    let duration = match normalized.as_ref() {
        Some(wav) => wav.duration()?,
        None => AudioNormalizer::normalized_wav_duration(raw.path())?,
    };
    validate_transcribe_duration(duration)?;
    let request = TranscriptionRequest {
        wav: wav_path,
        language: Some(job.language.clone()),
        prompt: None,
        punc: Some(settings.punc),
        spk: Some(settings.spk),
        duration_ms: duration.as_millis().min(i64::MAX as u128) as i64,
    };
    let outcome = job
        .engine
        .transcribe(
            job.reservation
                .take()
                .ok_or_else(|| anyhow::anyhow!("transcription reservation already consumed"))?,
            request,
        )
        .await
        .map_err(|error| {
            if job.sensevoice {
                anyhow::anyhow!(SENSEVOICE_RETRY_MESSAGE)
            } else {
                anyhow::anyhow!(error.to_string())
            }
        })?;
    let canonical = match outcome {
        TranscriptionOutcome::Transcript(canonical) => canonical,
        // NoSpeech is a successful execution terminal: no transcript checkpoint
        // is written; the caller persists the stable no_speech failure reason.
        TranscriptionOutcome::NoSpeech => return Ok(PipelineTerminal::NoSpeech),
    };
    let transcript = build_transcript(
        &job.session_id,
        &job.account_id,
        job.created_at,
        &job.model_id,
        &job.language,
        &job.source,
        duration,
        canonical,
    );
    commit_completed(
        job.repository.storage(),
        &job.mutation_locks,
        &job.session_id,
        &job.model_id,
        job.created_at,
        &transcript,
        &job.source,
        job.cleanup_enabled,
    )?;
    if job.source == "realtime" && job.cleanup_enabled {
        let local_transcription_elapsed_ms = local_transcription_started
            .elapsed()
            .as_millis()
            .min(u64::MAX as u128) as u64;
        run_cleanup(job, local_transcription_elapsed_ms).await;
    }
    Ok(PipelineTerminal::Completed)
}

async fn run_cleanup(job: PipelineJob, local_transcription_elapsed_ms: u64) {
    let storage = job.repository.storage();
    let row = match storage.get(&job.session_id) {
        Ok(Some(row)) => row,
        Ok(None) => return,
        Err(error) => {
            tracing::error!(session_id = %job.session_id, %error, "cleanup cannot read session before execution");
            return;
        }
    };
    let transcript_path = match row.transcript_path.clone() {
        Some(path) => path,
        None => {
            // 没有 raw transcript 就不满足 cleanup 的提交前提；reconcile 会保留该
            // 不变量异常，不能伪造一个没有 raw checkpoint 的 cleanup 终态。
            return;
        }
    };
    if !cleanup_commit_is_current(Some(&row), &job.session_id, &job.model_id, &transcript_path) {
        return;
    }
    let mut snapshot = match storage.cleanup_execution_snapshot(&job.session_id) {
        Ok(snapshot) => snapshot,
        Err(_) => {
            persist_cleanup_failure(
                &storage,
                &job,
                CleanupService::preflight_failure_artifact(
                    &job.account_id,
                    &job.session_id,
                    "cleanup_not_configured",
                ),
                Some(&transcript_path),
            );
            return;
        }
    };
    snapshot.local_transcription_elapsed_ms = local_transcription_elapsed_ms;
    let transcript = match storage.read_transcript(&transcript_path) {
        Ok(transcript) => transcript,
        Err(_) => {
            persist_cleanup_failure(
                &storage,
                &job,
                CleanupService::failure_artifact_for_snapshot(&snapshot, "cleanup_transport_error"),
                Some(&transcript_path),
            );
            return;
        }
    };
    let context = if row.context_present {
        match storage.read_context(&job.session_id, row.created_at) {
            Ok(context) => context,
            Err(_) => {
                persist_cleanup_failure(
                    &storage,
                    &job,
                    CleanupService::failure_artifact_for_snapshot(
                        &snapshot,
                        "cleanup_transport_error",
                    ),
                    Some(&transcript_path),
                );
                return;
            }
        }
    } else {
        ClipboardContextFile::default()
    };
    let result = job
        .cleanup
        .execute_with_dictionary(
            snapshot,
            &transcript,
            &context,
            &job.dictionary_snapshot.terms,
        )
        .await;
    let lock = match mutation_lock(&job.mutation_locks, &job.session_id) {
        Ok(lock) => lock,
        Err(_) => return,
    };
    let Ok(_mutation) = lock.lock() else { return };

    // 网络返回后必须重新读取 session。model、raw transcript path 和两个状态字段
    // 共同构成本次 worker 的提交身份；删除/retry/新一代 worker 获胜时不得写盘。
    let current_row = match storage.get(&job.session_id) {
        Ok(row) => row,
        Err(error) => {
            tracing::error!(session_id = %job.session_id, %error, "cleanup cannot re-read session before commit");
            return;
        }
    };
    if !cleanup_commit_is_current(
        current_row.as_ref(),
        &job.session_id,
        &job.model_id,
        &transcript_path,
    ) {
        return;
    }

    match result {
        CleanupExecution::Disabled => {
            persist_cleanup_failure(
                &storage,
                &job,
                CleanupService::preflight_failure_artifact(
                    &job.account_id,
                    &job.session_id,
                    "cleanup_not_configured",
                ),
                Some(&transcript_path),
            );
        }
        CleanupExecution::Succeeded {
            artifact,
            presentation,
        } => match write_cleanup_preserving_core(
            &storage,
            &job.session_id,
            job.created_at,
            &artifact,
        ) {
            Ok(path) => {
                let committed = match storage.complete_cleanup(
                    &job.session_id,
                    &job.model_id,
                    seasnail_storage::CleanupCompletion::Succeeded {
                        cleanup_path: &path,
                    },
                ) {
                    Ok(committed) => committed,
                    Err(error) => {
                        tracing::error!(session_id = %job.session_id, %error, "cleanup success CAS failed");
                        return;
                    }
                };
                if committed {
                    let _ = job.cleanup.publish_presentation(
                        &job.account_id,
                        &job.session_id,
                        presentation,
                    );
                } else {
                    discard_unclaimed_cleanup(&storage, &job, &path);
                }
            }
            Err(_) => {
                if let Err(error) = storage.complete_cleanup(
                    &job.session_id,
                    &job.model_id,
                    seasnail_storage::CleanupCompletion::Failed {
                        cleanup_path: None,
                        error_code: seasnail_storage::CleanupFailureCode::ArtifactWriteFailed,
                    },
                ) {
                    tracing::error!(session_id = %job.session_id, %error, "cleanup artifact-write-failure CAS failed");
                }
            }
        },
        CleanupExecution::Failed { artifact } => {
            let error_code = cleanup_failure_code(&artifact.error_code);
            match write_cleanup_preserving_core(
                &storage,
                &job.session_id,
                job.created_at,
                &artifact,
            ) {
                Ok(path) => {
                    let committed = match storage.complete_cleanup(
                        &job.session_id,
                        &job.model_id,
                        seasnail_storage::CleanupCompletion::Failed {
                            cleanup_path: Some(&path),
                            error_code,
                        },
                    ) {
                        Ok(committed) => committed,
                        Err(error) => {
                            tracing::error!(session_id = %job.session_id, %error, "cleanup failure CAS failed");
                            return;
                        }
                    };
                    if !committed {
                        discard_unclaimed_cleanup(&storage, &job, &path);
                    }
                }
                Err(_) => {
                    if let Err(error) = storage.complete_cleanup(
                        &job.session_id,
                        &job.model_id,
                        seasnail_storage::CleanupCompletion::Failed {
                            cleanup_path: None,
                            error_code: seasnail_storage::CleanupFailureCode::ArtifactWriteFailed,
                        },
                    ) {
                        tracing::error!(session_id = %job.session_id, %error, "cleanup failure-artifact-write CAS failed");
                    }
                }
            }
        }
    }
}

fn cleanup_commit_is_current(
    row: Option<&seasnail_storage::SessionRow>,
    session_id: &str,
    model_id: &str,
    transcript_path: &str,
) -> bool {
    row.is_some_and(|row| {
        row.id == session_id
            && row.model == model_id
            && row.status == "cleaning_up"
            && row.cleanup_status == "processing"
            && row.transcript_path.as_deref() == Some(transcript_path)
    })
}

fn discard_unclaimed_cleanup(storage: &crate::account::Storage, job: &PipelineJob, path: &str) {
    // DB 读取失败时保守保留文件，避免把一次可能已经提交成功的 artifact 当作孤儿删掉。
    let owned_by_terminal_row = match storage.get(&job.session_id) {
        Ok(Some(row)) => {
            matches!(row.cleanup_status.as_str(), "succeeded" | "failed")
                && row.cleanup_path.as_deref() == Some(path)
        }
        Ok(None) => false,
        Err(_) => true,
    };
    if !owned_by_terminal_row {
        let _ = storage.remove_cleanup(&job.session_id, job.created_at);
    }
}

fn persist_cleanup_failure(
    storage: &crate::account::Storage,
    job: &PipelineJob,
    artifact: CleanupFile,
    transcript_path: Option<&str>,
) {
    let Ok(lock) = mutation_lock(&job.mutation_locks, &job.session_id) else {
        return;
    };
    let Ok(_mutation) = lock.lock() else { return };
    let Ok(Some(row)) = storage.get(&job.session_id) else {
        return;
    };
    if !cleanup_commit_is_current(
        Some(&row),
        &job.session_id,
        &job.model_id,
        transcript_path.unwrap_or_default(),
    ) {
        return;
    }
    let error_code = cleanup_failure_code(&artifact.error_code);
    match write_cleanup_preserving_core(storage, &job.session_id, job.created_at, &artifact) {
        Ok(path) => {
            let committed = storage
                .complete_cleanup(
                    &job.session_id,
                    &job.model_id,
                    seasnail_storage::CleanupCompletion::Failed {
                        cleanup_path: Some(&path),
                        error_code,
                    },
                )
                .unwrap_or(false);
            if !committed {
                discard_unclaimed_cleanup(storage, job, &path);
            }
        }
        Err(_) => {
            let _ = storage.complete_cleanup(
                &job.session_id,
                &job.model_id,
                seasnail_storage::CleanupCompletion::Failed {
                    cleanup_path: None,
                    error_code: seasnail_storage::CleanupFailureCode::ArtifactWriteFailed,
                },
            );
        }
    }
}

/// diagnostics 和 context placements 都是可丢弃扩展，不能因为扩展异常牺牲核心
/// cleanup。位置扩展先独立校验并在无效时清空；首次原子写失败时再去掉 diagnostics
/// 重试。核心 artifact 仍失败才由调用方收口为 `cleanup_artifact_write_failed`。
fn write_cleanup_preserving_core(
    storage: &crate::account::Storage,
    session_id: &str,
    created_at: i64,
    artifact: &CleanupFile,
) -> Result<String, crate::account::AccountError> {
    let mut candidate = artifact.clone();
    if seasnail_proto::validate_cleanup_context_placements(&candidate).is_err() {
        candidate.context_placements.clear();
    }
    match storage.write_cleanup(session_id, created_at, &candidate) {
        Ok(path) => Ok(path),
        Err(first_error) if candidate.diagnostics.is_some() => {
            let mut core = candidate;
            core.diagnostics = None;
            storage
                .write_cleanup(session_id, created_at, &core)
                .map_err(|_| first_error)
        }
        Err(error) => Err(error),
    }
}

fn cleanup_failure_code(code: &str) -> seasnail_storage::CleanupFailureCode {
    use seasnail_storage::CleanupFailureCode as Code;
    match code {
        "cleanup_endpoint_rejected" => Code::EndpointRejected,
        "cleanup_credential_missing" => Code::CredentialMissing,
        "cleanup_input_too_large" => Code::InputTooLarge,
        "cleanup_timeout" => Code::Timeout,
        "cleanup_http_auth" => Code::HttpAuth,
        "cleanup_http_rate_limit" => Code::HttpRateLimit,
        "cleanup_http_server" => Code::HttpServer,
        "cleanup_response_too_large" => Code::ResponseTooLarge,
        "cleanup_response_invalid_json" => Code::ResponseInvalidJson,
        "cleanup_cleaned_text_invalid" => Code::CleanedTextInvalid,
        "cleanup_placeholder_invalid" => Code::PlaceholderInvalid,
        "cleanup_transport_error" => Code::TransportError,
        _ => Code::NotConfigured,
    }
}

fn commit_completed(
    storage: &crate::account::Storage,
    mutation_locks: &Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
    session_id: &str,
    model_id: &str,
    created_at: i64,
    transcript: &TranscriptFile,
    source: &str,
    cleanup_enabled: bool,
) -> anyhow::Result<()> {
    let lock = mutation_lock(mutation_locks, session_id)?;
    let _mutation = lock
        .lock()
        .map_err(|_| anyhow::anyhow!("session mutation lock poisoned"))?;
    let row = storage.get(session_id)?;
    if !row.is_some_and(|row| row.status == "transcribing" && row.model == model_id) {
        anyhow::bail!("session was deleted or superseded before transcription outcome commit");
    }
    let transcript_rel = storage.write_transcript(session_id, created_at, transcript)?;
    let decision = if source != "realtime" {
        RawCheckpointDecision::NotRequested
    } else if cleanup_enabled {
        RawCheckpointDecision::Processing
    } else {
        RawCheckpointDecision::Disabled
    };
    if !storage.checkpoint_raw(
        session_id,
        model_id,
        &transcript_rel,
        transcript.duration_ms as f64 / 1000.0,
        decision,
    )? {
        anyhow::bail!("session disappeared while committing raw checkpoint");
    }
    Ok(())
}

fn build_transcript(
    session_id: &str,
    account_id: &str,
    created_at: i64,
    model: &str,
    language: &str,
    source: &str,
    audio_duration: Duration,
    canonical: CanonicalTranscript,
) -> TranscriptFile {
    TranscriptFile {
        schema_version: 2,
        session_id: session_id.into(),
        account_id: account_id.into(),
        created_at_ms: created_at * 1000,
        model: model.into(),
        language: language.into(),
        source: match source {
            "realtime" => Source::Realtime,
            _ => Source::Imported,
        } as i32,
        duration_ms: audio_duration.as_millis().min(i64::MAX as u128) as i64,
        speakers: canonical
            .speaker_roster
            .into_iter()
            .map(|id| Speaker {
                label: format!("说话人 {id}"),
                id,
            })
            .collect(),
        units: canonical
            .units
            .into_iter()
            .map(|unit| TranscriptUnit {
                sequence: unit.sequence,
                start_ms: unit.start_ms,
                end_ms: unit.end_ms,
                text: unit.text,
                speaker: unit.speaker.unwrap_or_default(),
                confidence: None,
                granularity: match unit.granularity {
                    Granularity::TimedText => UnitGranularity::TimedText,
                    Granularity::Segment => UnitGranularity::Segment,
                    Granularity::Untimed => UnitGranularity::Untimed,
                } as i32,
            })
            .collect(),
        full_text: canonical.full_text,
    }
}

fn validate_transcribe_duration(duration: Duration) -> anyhow::Result<()> {
    if duration > MAX_TRANSCRIBE_DURATION {
        anyhow::bail!(
            "audio exceeds the 60-minute transcription limit; it was not truncated or split"
        );
    }
    Ok(())
}

fn safe_file_name(value: Option<&str>) -> String {
    value
        .and_then(|name| Path::new(name).file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .unwrap_or("audio.bin")
        .to_string()
}

fn extension_for(file_name: &str) -> String {
    match Path::new(file_name)
        .extension()
        .and_then(|ext| ext.to_str())
    {
        Some(ext) if !ext.is_empty() => format!(".{ext}"),
        _ => ".wav".into(),
    }
}

fn normalize_source(source: &str) -> String {
    match source {
        "realtime" | "imported" => source.to_owned(),
        _ => "imported".into(),
    }
}

fn normalize_language(language: Option<&str>) -> String {
    match language {
        Some("zh" | "en" | "mixed") => language.unwrap().to_owned(),
        _ => "mixed".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seasnail_crypto::{Argon2Params, KeychainStore, MemoryKeychain};

    #[test]
    fn dictionary_snapshot_failure_falls_back_to_empty() {
        let snapshot =
            snapshot_or_default(Err(super::super::DictionaryError::InvalidTerm), "session");
        assert!(snapshot.terms.is_empty());
    }

    fn repository() -> (
        tempfile::TempDir,
        String,
        super::super::AccountScopedRepository,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let keychain = Arc::new(MemoryKeychain::new()) as Arc<dyn KeychainStore>;
        let crypto = Arc::new(
            crate::account::Crypto::new(
                dir.path().to_path_buf(),
                keychain,
                Argon2Params {
                    m_kib: 8192,
                    t_cost: 1,
                    p_cost: 1,
                },
            )
            .unwrap(),
        );
        let services = super::super::ApplicationServices::new_for_test(Arc::new(
            crate::account::Auth::new(crypto),
        ));
        let token = services.auth.setup_first_account("alice", "p").unwrap();
        let caller = services.auth.authenticate_and_bind(&token.secret).unwrap();
        (
            dir,
            caller.account_id().to_owned(),
            caller.repository_clone(),
        )
    }

    fn insert_transcribing(
        storage: &crate::account::Storage,
        account_id: &str,
        session_id: &str,
        created_at: i64,
    ) -> std::path::PathBuf {
        let audio_path = storage
            .write_audio(session_id, created_at, b"audio")
            .unwrap();
        let session_dir = storage
            .bound_account_dir()
            .unwrap()
            .join(Path::new(&audio_path).parent().unwrap());
        storage
            .insert_session(&seasnail_storage::SessionRow {
                id: session_id.into(),
                account_id: account_id.into(),
                created_at,
                source: "imported".into(),
                language: "zh".into(),
                duration_sec: 0.0,
                status: "transcribing".into(),
                model: "model-a".into(),
                input_device: None,
                file_name: Some("audio.wav".into()),
                audio_path: Some(audio_path),
                transcript_path: None,
                failure_reason: None,
                context_present: false,
                cleanup_status: "not_requested".into(),
                cleanup_path: None,
                cleanup_error_code: None,
            })
            .unwrap();
        session_dir
    }

    fn test_transcript(account_id: &str, session_id: &str) -> TranscriptFile {
        TranscriptFile {
            schema_version: 2,
            session_id: session_id.into(),
            account_id: account_id.into(),
            created_at_ms: 1_700_000_000_000,
            model: "model-a".into(),
            language: "zh".into(),
            source: Source::Imported as i32,
            duration_ms: 1_000,
            speakers: Vec::new(),
            units: Vec::new(),
            full_text: String::new(),
        }
    }

    #[tokio::test]
    async fn shutdown_waits_for_cancelled_task_drop() {
        let owner = Arc::new(BackgroundTaskOwner::default());
        let dropped = Arc::new(AtomicBool::new(false));
        struct Guard(Arc<AtomicBool>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let guard = Guard(dropped.clone());
        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _guard = guard;
            let _ = tx.send(());
            std::future::pending::<()>().await;
        });
        owner.tasks.lock().unwrap().insert(
            "cancelled".into(),
            TrackedTask {
                generation: 0,
                abort: task.abort_handle(),
                join: Some(task),
            },
        );
        rx.await.unwrap();
        owner.cancel("cancelled");
        owner.shutdown().await;
        assert!(dropped.load(Ordering::Acquire));
        assert!(owner.closing.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn stale_task_completion_does_not_remove_new_generation_handle() {
        let owner = Arc::new(BackgroundTaskOwner::default());
        let old_worker = tokio::spawn(std::future::pending::<()>());
        let new_worker = tokio::spawn(std::future::pending::<()>());
        {
            let mut tasks = owner.tasks.lock().unwrap();
            tasks.insert(
                "same-session".into(),
                TrackedTask {
                    generation: 1,
                    abort: old_worker.abort_handle(),
                    join: None,
                },
            );
            tasks.insert(
                "same-session".into(),
                TrackedTask {
                    generation: 2,
                    abort: new_worker.abort_handle(),
                    join: None,
                },
            );
        }

        owner.finish("same-session", 1);
        assert_eq!(
            owner
                .tasks
                .lock()
                .unwrap()
                .get("same-session")
                .map(|task| task.generation),
            Some(2)
        );
        old_worker.abort();
        new_worker.abort();
    }

    #[test]
    fn cleanup_commit_requires_current_processing_row_and_raw_checkpoint() {
        let base = seasnail_storage::SessionRow {
            id: "session".into(),
            account_id: "account".into(),
            created_at: 1,
            source: "realtime".into(),
            language: "zh".into(),
            duration_sec: 1.0,
            status: "cleaning_up".into(),
            model: "model-a".into(),
            input_device: None,
            file_name: None,
            audio_path: Some("audio".into()),
            transcript_path: Some("transcript-a".into()),
            failure_reason: None,
            context_present: false,
            cleanup_status: "processing".into(),
            cleanup_path: None,
            cleanup_error_code: None,
        };
        assert!(cleanup_commit_is_current(
            Some(&base),
            "session",
            "model-a",
            "transcript-a"
        ));

        for (status, cleanup_status, model, transcript_path) in [
            ("completed", "succeeded", "model-a", "transcript-a"),
            ("cleaning_up", "processing", "model-b", "transcript-a"),
            ("cleaning_up", "processing", "model-a", "transcript-b"),
            ("cleaning_up", "failed", "model-a", "transcript-a"),
        ] {
            let mut stale = base.clone();
            stale.status = status.into();
            stale.cleanup_status = cleanup_status.into();
            stale.model = model.into();
            stale.transcript_path = Some(transcript_path.into());
            assert!(!cleanup_commit_is_current(
                Some(&stale),
                "session",
                "model-a",
                "transcript-a"
            ));
        }
        assert!(!cleanup_commit_is_current(
            None,
            "session",
            "model-a",
            "transcript-a"
        ));
    }

    #[test]
    fn cleanup_write_drops_invalid_optional_placements_but_keeps_cleaned_text() {
        let (_dir, account_id, repository) = repository();
        let storage = repository.storage();
        let session_id = "00000000-0000-0000-0000-000000000013";
        let created_at = 1_700_000_000;
        let artifact = CleanupFile {
            schema_version: 1,
            session_id: session_id.into(),
            account_id,
            outcome: seasnail_proto::seasnail::v1::CleanupOutcome::Succeeded as i32,
            cleaned_text: "clean".into(),
            provider_config_id: "00000000-0000-4000-8000-000000000001".into(),
            model: "model".into(),
            prompt_sha256: vec![0x42; 32],
            placeholder_validation:
                seasnail_proto::seasnail::v1::PlaceholderValidationStatus::PlaceholderValidationPassed
                    as i32,
            context_placements: vec![
                seasnail_proto::seasnail::v1::CleanupContextPlacement {
                    event_sequence: 1,
                    byte_offset: 99,
                },
            ],
            ..Default::default()
        };

        let path = write_cleanup_preserving_core(storage, session_id, created_at, &artifact)
            .expect("invalid optional placement should be removed");
        let persisted = storage.read_cleanup(&path).unwrap();
        assert_eq!(persisted.cleaned_text, "clean");
        assert!(persisted.context_placements.is_empty());
    }

    #[test]
    fn completed_outcome_and_delete_obey_both_linearization_orders() {
        let (_dir, account_id, repository) = repository();
        let storage = repository.storage();
        let locks = Arc::new(Mutex::new(HashMap::new()));
        let created_at = 1_700_000_000;

        let deleted_id = "00000000-0000-0000-0000-000000000010";
        let deleted_dir = insert_transcribing(storage, &account_id, deleted_id, created_at);
        assert!(storage.delete(deleted_id).unwrap());
        assert!(commit_completed(
            storage,
            &locks,
            deleted_id,
            "model-a",
            created_at,
            &test_transcript(&account_id, deleted_id),
            "imported",
            false,
        )
        .is_err());
        assert!(storage.get(deleted_id).unwrap().is_none());
        assert!(
            !deleted_dir.exists(),
            "delete-first must not recreate files"
        );

        let completed_id = "00000000-0000-0000-0000-000000000011";
        let completed_dir = insert_transcribing(storage, &account_id, completed_id, created_at);
        commit_completed(
            storage,
            &locks,
            completed_id,
            "model-a",
            created_at,
            &test_transcript(&account_id, completed_id),
            "imported",
            false,
        )
        .unwrap();
        let completed = storage.get(completed_id).unwrap().unwrap();
        assert_eq!(completed.status, "completed");
        assert!(completed.transcript_path.is_some());
        assert!(storage.delete(completed_id).unwrap());
        assert!(
            !completed_dir.exists(),
            "outcome-first delete removes files"
        );

        let cleanup_id = "00000000-0000-0000-0000-000000000012";
        let _cleanup_dir = insert_transcribing(storage, &account_id, cleanup_id, created_at);
        commit_completed(
            storage,
            &locks,
            cleanup_id,
            "model-a",
            created_at,
            &test_transcript(&account_id, cleanup_id),
            "realtime",
            true,
        )
        .unwrap();
        let cleanup = storage.get(cleanup_id).unwrap().unwrap();
        assert_eq!(cleanup.status, "cleaning_up");
        assert_eq!(cleanup.cleanup_status, "processing");
    }

    #[test]
    fn duration_limit_and_filename_normalization_are_stable() {
        assert!(validate_transcribe_duration(Duration::from_secs(3600)).is_ok());
        assert!(validate_transcribe_duration(Duration::from_millis(3_600_001)).is_err());
        assert_eq!(safe_file_name(Some("../../voice.wav")), "voice.wav");
        assert_eq!(safe_file_name(Some("..")), "audio.bin");
    }

    #[test]
    fn no_speech_terminal_persists_stable_reason_and_keeps_audio() {
        let (_dir, account_id, repository) = repository();
        let storage = repository.storage();
        insert_transcribing(&storage, &account_id, "s-no-speech", 10);
        let locks = Arc::new(Mutex::new(HashMap::new()));

        persist_failure_terminal(
            &locks,
            &repository,
            "s-no-speech",
            "model-a",
            NO_SPEECH_REASON,
        );

        let row = storage.get("s-no-speech").unwrap().unwrap();
        assert_eq!(row.status, "failed");
        assert_eq!(row.failure_reason.as_deref(), Some(NO_SPEECH_REASON));
        assert!(row.transcript_path.is_none());
        assert!(row.audio_path.is_some(), "NoSpeech 保留音频");
    }

    #[test]
    fn failure_terminal_does_not_overwrite_superseded_rows() {
        let (_dir, account_id, repository) = repository();
        let storage = repository.storage();
        let locks = Arc::new(Mutex::new(HashMap::new()));

        // 新一代 worker 已提交 completed：迟到失败不得覆盖。
        insert_transcribing(&storage, &account_id, "s-completed", 20);
        let transcript = test_transcript(&account_id, "s-completed");
        commit_completed(
            &storage,
            &locks,
            "s-completed",
            "model-a",
            20,
            &transcript,
            "imported",
            false,
        )
        .unwrap();
        persist_failure_terminal(
            &locks,
            &repository,
            "s-completed",
            "model-a",
            NO_SPEECH_REASON,
        );
        let row = storage.get("s-completed").unwrap().unwrap();
        assert_eq!(row.status, "completed");
        assert!(row.failure_reason.is_none());

        // 行已被其他原因收口 failed：迟到失败不得改写既有原因。
        insert_transcribing(&storage, &account_id, "s-failed", 25);
        storage
            .fail_transcription("s-failed", "model-a", "boom")
            .unwrap();
        persist_failure_terminal(&locks, &repository, "s-failed", "model-a", NO_SPEECH_REASON);
        let row = storage.get("s-failed").unwrap().unwrap();
        assert_eq!(row.status, "failed");
        assert_eq!(row.failure_reason.as_deref(), Some("boom"));

        // retry 已换 model 获胜：旧 model 的迟到失败不得覆盖新一代。
        insert_transcribing(&storage, &account_id, "s-retried", 30);
        storage
            .fail_transcription("s-retried", "model-a", "boom")
            .unwrap();
        storage
            .begin_retry("s-retried", "model-a", "model-b")
            .unwrap();
        persist_failure_terminal(
            &locks,
            &repository,
            "s-retried",
            "model-a",
            NO_SPEECH_REASON,
        );
        let row = storage.get("s-retried").unwrap().unwrap();
        assert_eq!(row.status, "transcribing");
        assert_eq!(row.model, "model-b");
        assert!(row.failure_reason.is_none());

        // 已删除：不得报错，也不得重建会话。
        persist_failure_terminal(
            &locks,
            &repository,
            "s-deleted",
            "model-a",
            NO_SPEECH_REASON,
        );
        assert!(storage.get("s-deleted").unwrap().is_none());
    }

    #[test]
    fn retry_clears_no_speech_reason() {
        let (_dir, account_id, repository) = repository();
        let storage = repository.storage();
        insert_transcribing(&storage, &account_id, "s-retry", 40);
        storage
            .fail_transcription("s-retry", "model-a", NO_SPEECH_REASON)
            .unwrap();
        let row = storage.get("s-retry").unwrap().unwrap();
        assert_eq!(row.failure_reason.as_deref(), Some(NO_SPEECH_REASON));

        storage
            .begin_retry("s-retry", "model-a", "model-a")
            .unwrap();
        let row = storage.get("s-retry").unwrap().unwrap();
        assert_eq!(row.status, "transcribing");
        assert!(
            row.failure_reason.is_none(),
            "retry 必须清理旧 no_speech 原因"
        );
    }

    #[test]
    fn canonical_projection_preserves_roster_and_units() {
        let transcript = build_transcript(
            "s",
            "a",
            10,
            "m",
            "en",
            "realtime",
            Duration::from_secs(1),
            CanonicalTranscript {
                full_text: "hello".into(),
                granularity: Granularity::Segment,
                duration_ms: 1_000,
                units: vec![seasnail_runtime::CanonicalUnit {
                    sequence: 0,
                    start_ms: Some(0),
                    end_ms: Some(1_000),
                    text: "hello".into(),
                    speaker: Some("A".into()),
                    granularity: Granularity::Segment,
                }],
                speaker_roster: vec!["A".into()],
            },
        );
        assert_eq!(transcript.full_text, "hello");
        assert_eq!(transcript.speakers[0].label, "说话人 A");
        assert_eq!(
            transcript.units[0].granularity,
            UnitGranularity::Segment as i32
        );
    }
}
