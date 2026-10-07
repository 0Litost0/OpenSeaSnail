//! CPAL recording adapter helpers.
//!
//! The coordinator sees `RecordingPort`/`CapturedAudio`; only this module and
//! the controller implementation deal with CPAL's device/error vocabulary.

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecordingAsyncError {
    pub(crate) generation: u64,
    pub(crate) code: String,
}

pub(crate) fn microphone_unavailable_error() -> String {
    "recording_microphone_unavailable".to_string()
}

pub(crate) fn cpal_recording_error(error: &cpal::Error) -> String {
    match error.kind() {
        cpal::ErrorKind::DeviceNotAvailable => microphone_unavailable_error(),
        cpal::ErrorKind::PermissionDenied => "recording_microphone_unauthorized".into(),
        _ => "recording_device_error".into(),
    }
}

/// 不使用 `Device::to_string()`：CPAL 的 Display 实现在设备描述读取失败时会返回
/// fmt::Error，而标准库的 ToString 会将其升级为 panic。
pub(crate) fn input_device_name(device: &cpal::Device) -> Option<String> {
    device
        .description()
        .ok()
        .map(|description| description.name().to_owned())
        .filter(|name| !name.trim().is_empty())
}

pub(crate) fn require_available_input_device<T>(device: Option<T>) -> Result<T, String> {
    device.ok_or_else(microphone_unavailable_error)
}

use crate::clipboard_collector;
use crate::clipboard_collector::ClipboardContextStatusSink;
use crate::clipboard_media_cache::MediaCache;
use crate::ensure_microphone_permission;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use seasnail_proto::seasnail::v1::ClipboardContextFile;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::Emitter;
const TARGET_SAMPLE_RATE: u32 = 16_000;
const RECORDING_STATUS_EVENT: &str = "recording-status";
const RECORDING_STATUS_INTERVAL: Duration = Duration::from_millis(100);
#[derive(Serialize)]
pub(crate) struct InputDevice {
    pub(crate) name: String,
    pub(crate) is_default: bool,
}

pub(crate) struct CapturedRecording {
    pub(crate) pcm: Vec<f32>,
    pub(crate) input_device: String,
    pub(crate) sample_rate: u32,
    /// 录音期剪贴板上下文 manifest；功能关闭时为 None，启用时为 Some（可能含 0 事件；
    /// 提交时空 manifest 会被过滤，不写 context.pb.enc、不置 context_present）。仅原生层持有，
    /// 由 submit_realtime_wav 作为 multipart 一并提交，不经 WebView。
    pub(crate) clipboard_manifest: Option<ClipboardContextFile>,
}

#[derive(Clone, Serialize)]
pub(crate) struct RecordingStatus {
    pub(crate) is_recording: bool,
    pub(crate) elapsed_ms: u64,
    pub(crate) level: f32,
    pub(crate) input_device: Option<String>,
    pub(crate) sample_rate: Option<u32>,
    pub(crate) error: Option<String>,
    pub(crate) clipboard_context_count: usize,
    pub(crate) clipboard_context_error: Option<String>,
}

/// 采集回调只更新内存指标；最多每 100ms 向 WebView 推送一次状态。
struct RecordingTelemetry {
    active: AtomicBool,
    failed: AtomicBool,
    level_bits: AtomicU32,
    started_at: Mutex<Option<Instant>>,
    pub(crate) input_device: Mutex<Option<String>>,
    pub(crate) sample_rate: Mutex<Option<u32>>,
    last_error: Mutex<Option<String>>,
    last_emit: Mutex<Option<Instant>>,
    app: Mutex<Option<tauri::AppHandle>>,
    clipboard: Arc<ClipboardContextStatusSink>,
}

impl RecordingTelemetry {
    pub(crate) fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            level_bits: AtomicU32::new(0.0_f32.to_bits()),
            started_at: Mutex::new(None),
            input_device: Mutex::new(None),
            sample_rate: Mutex::new(None),
            last_error: Mutex::new(None),
            last_emit: Mutex::new(None),
            app: Mutex::new(None),
            clipboard: ClipboardContextStatusSink::new(),
        }
    }

    fn attach_app(&self, app: tauri::AppHandle) {
        *self.app.lock().expect("recording app mutex") = Some(app);
    }

    pub(crate) fn start(&self, input_device: String, sample_rate: u32) {
        self.active.store(true, Ordering::Release);
        self.failed.store(false, Ordering::Release);
        self.level_bits.store(0.0_f32.to_bits(), Ordering::Release);
        *self.started_at.lock().expect("recording started mutex") = Some(Instant::now());
        *self.input_device.lock().expect("recording device mutex") = Some(input_device);
        *self.sample_rate.lock().expect("recording rate mutex") = Some(sample_rate);
        *self.last_error.lock().expect("recording error mutex") = None;
        self.emit_now();
    }

    pub(crate) fn stop(&self) {
        self.active.store(false, Ordering::Release);
        self.level_bits.store(0.0_f32.to_bits(), Ordering::Release);
        *self.started_at.lock().expect("recording started mutex") = None;
        *self.input_device.lock().expect("recording device mutex") = None;
        *self.sample_rate.lock().expect("recording rate mutex") = None;
        // poll 线程已由 controller.stop() 的 finalize join 回收，此处无写者，安全归零。
        self.clipboard.reset();
        self.emit_now();
    }

    fn record_error(&self, error: String) {
        self.active.store(false, Ordering::Release);
        self.failed.store(true, Ordering::Release);
        *self.last_error.lock().expect("recording error mutex") = Some(error);
        self.emit_now();
    }

    fn failure(&self) -> Option<String> {
        self.failed
            .load(Ordering::Acquire)
            .then(|| {
                self.last_error
                    .lock()
                    .expect("recording error mutex")
                    .clone()
            })
            .flatten()
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub(crate) fn status(&self) -> RecordingStatus {
        let started_at = *self.started_at.lock().expect("recording started mutex");
        RecordingStatus {
            is_recording: self.active.load(Ordering::Acquire),
            elapsed_ms: started_at
                .map(|started| started.elapsed().as_millis() as u64)
                .unwrap_or(0),
            level: f32::from_bits(self.level_bits.load(Ordering::Acquire)),
            input_device: self
                .input_device
                .lock()
                .expect("recording device mutex")
                .clone(),
            sample_rate: *self.sample_rate.lock().expect("recording rate mutex"),
            error: self
                .last_error
                .lock()
                .expect("recording error mutex")
                .clone(),
            clipboard_context_count: self.clipboard.count(),
            clipboard_context_error: self.clipboard.error_label().map(str::to_string),
        }
    }

    fn update_level(&self, level: f32) {
        self.level_bits
            .store(level.clamp(0.0, 1.0).to_bits(), Ordering::Release);
        let now = Instant::now();
        let mut last_emit = self.last_emit.lock().expect("recording emit mutex");
        if last_emit
            .is_some_and(|previous| now.duration_since(previous) < RECORDING_STATUS_INTERVAL)
        {
            return;
        }
        *last_emit = Some(now);
        drop(last_emit);
        self.emit_status();
    }

    fn emit_now(&self) {
        *self.last_emit.lock().expect("recording emit mutex") = Some(Instant::now());
        self.emit_status();
    }

    fn emit_status(&self) {
        if let Some(app) = self.app.lock().expect("recording app mutex").clone() {
            let _ = app.emit(RECORDING_STATUS_EVENT, self.status());
        }
    }
}

struct RecordingInner {
    stream: Option<cpal::Stream>,
    pub(crate) pcm: Option<Arc<Mutex<Vec<f32>>>>,
    pub(crate) input_device: Option<String>,
    pub(crate) sample_rate: Option<u32>,
    source_frames: Option<Arc<AtomicU64>>,
    clipboard_collector: clipboard_collector::ClipboardCollector,
}

/// CPAL 回调错误由独立线程消费；退出时必须先请求停止并 join，避免 GUI 退出后
/// 残留线程继续持有采集器或 Coordinator 引用。
pub(crate) struct CpalErrorConsumer {
    stop: AtomicBool,
    handle: Mutex<Option<thread::JoinHandle<()>>>,
}

impl Default for CpalErrorConsumer {
    fn default() -> Self {
        Self {
            stop: AtomicBool::new(false),
            handle: Mutex::new(None),
        }
    }
}

impl CpalErrorConsumer {
    pub(crate) fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    pub(crate) fn install(&self, handle: thread::JoinHandle<()>) {
        *self.handle.lock().expect("CPAL consumer handle mutex") = Some(handle);
    }

    pub(crate) fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self
            .handle
            .lock()
            .expect("CPAL consumer handle mutex")
            .take()
        {
            let _ = handle.join();
        }
    }
}

/// 原生麦克风采集。硬件以受支持的原生参数打开，随后由本模块降混/重采样为 16kHz mono。
pub(crate) struct RecordingController {
    inner: Mutex<RecordingInner>,
    telemetry: Arc<RecordingTelemetry>,
    generation: AtomicU64,
    error_tx: Sender<RecordingAsyncError>,
    error_rx: Mutex<Receiver<RecordingAsyncError>>,
}

impl RecordingController {
    pub(crate) fn new() -> Self {
        let (error_tx, error_rx) = mpsc::channel();
        Self {
            inner: Mutex::new(RecordingInner {
                stream: None,
                pcm: None,
                input_device: None,
                sample_rate: None,
                source_frames: None,
                clipboard_collector: clipboard_collector::ClipboardCollector::new(),
            }),
            telemetry: Arc::new(RecordingTelemetry::new()),
            generation: AtomicU64::new(0),
            error_tx,
            error_rx: Mutex::new(error_rx),
        }
    }

    pub(crate) fn input_devices() -> Result<Vec<InputDevice>, String> {
        let host = cpal::default_host();
        let default_name = host
            .default_input_device()
            .and_then(|device| input_device_name(&device));
        let mut devices = host
            .input_devices()
            .map_err(|err| format!("无法枚举输入设备: {err}"))?
            // CPAL 0.18 的 Device::Display 会在 CoreAudio 无法读取设备描述时
            // 返回 fmt::Error；String::to_string 随后会 panic。设备热插拔或没有
            // 实体输入设备时必须按可恢复错误处理。
            .filter_map(|device| input_device_name(&device))
            .map(|name| InputDevice {
                is_default: default_name.as_deref() == Some(name.as_str()),
                name,
            })
            .collect::<Vec<_>>();
        devices.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(devices)
    }

    pub(crate) fn start(
        &self,
        requested_device: Option<&str>,
        clipboard_context_enabled: bool,
        account_id: Option<String>,
        media_cache: Option<MediaCache>,
    ) -> Result<RecordingStart, String> {
        let mut inner = self.inner.lock().expect("recording mutex");
        if self.telemetry.failure().is_some() {
            // CPAL 已报告设备错误：丢弃这次不完整缓冲，下一次开始使用新流。
            drop(inner.stream.take());
            inner.pcm.take();
            inner.input_device.take();
            inner.sample_rate.take();
            inner.source_frames.take();
            inner.clipboard_collector.reset();
        }
        if inner.stream.is_some() {
            return Err("recording_already_active".to_string());
        }

        let host = cpal::default_host();
        let device = match requested_device.filter(|name| !name.is_empty()) {
            Some(requested) => host
                .input_devices()
                .map_err(|_| "recording_device_error".to_string())?
                .find(|device| input_device_name(device).as_deref() == Some(requested))
                .ok_or_else(microphone_unavailable_error)?,
            None => require_available_input_device(host.default_input_device())?,
        };
        let discovered_name = input_device_name(&device);
        let supported = device.default_input_config().map_err(|error| {
            if discovered_name.is_none() {
                microphone_unavailable_error()
            } else {
                cpal_recording_error(&error)
            }
        })?;
        let name = discovered_name.unwrap_or_else(|| "Unknown input device".into());
        let config = supported.config();
        let channels = config.channels as usize;
        if channels == 0 {
            return Err(microphone_unavailable_error());
        }
        // 先确认系统确实存在且能配置输入设备，再判断 TCC 权限。Mac mini 等
        // 没有内置麦克风的设备由此得到准确提示，不会被误导去反复申请权限。
        ensure_microphone_permission()?;

        let pcm = Arc::new(Mutex::new(Vec::new()));
        let source_frames = Arc::new(AtomicU64::new(0));
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let stream = build_input_stream(
            &device,
            &config,
            supported.sample_format(),
            channels,
            pcm.clone(),
            source_frames.clone(),
            self.telemetry.clone(),
            self.error_tx.clone(),
            generation,
        )?;
        stream
            .play()
            .map_err(|error| cpal_recording_error(&error))?;
        inner.stream = Some(stream);
        inner.pcm = Some(pcm);
        inner.input_device = Some(name.clone());
        inner.sample_rate = Some(config.sample_rate);
        inner.source_frames = Some(source_frames.clone());
        inner.clipboard_collector.start(
            clipboard_context_enabled,
            config.sample_rate,
            source_frames,
            account_id,
            media_cache,
            Some(self.telemetry.clipboard.clone()),
        );
        self.telemetry.start(name.clone(), config.sample_rate);
        Ok(RecordingStart {
            input_device: name,
            sample_rate: config.sample_rate,
        })
    }

    pub(crate) fn stop(&self) -> Result<CapturedRecording, String> {
        let mut inner = self.inner.lock().expect("recording mutex");
        // 先确认确实有活动 stream。重复/异常 stop 不得改变 Idle collector 的状态。
        let stream = inner
            .stream
            .take()
            .ok_or_else(|| "recording_not_active".to_string())?;
        // 先冻结 collector，拒绝新的 clipboard 事件；再停 stream，并以停止后的源
        // 帧数结束 collector。任何竞争锚点都只会落在最终音频末帧以内。
        inner.clipboard_collector.freeze();
        drop(stream);
        let source_frames = inner
            .source_frames
            .as_ref()
            .map(|frames| frames.load(Ordering::Acquire))
            .unwrap_or(0);
        inner.clipboard_collector.finalize(source_frames);
        // finalize 已 join poll 线程并钳制偏移；此刻取出 manifest（reset 前一次性）。
        let clipboard_manifest = inner.clipboard_collector.take_manifest();
        let pcm = inner
            .pcm
            .take()
            .ok_or_else(|| "recording_pcm_unavailable".to_string())?
            .lock()
            .expect("recording pcm mutex")
            .clone();
        let input_device = inner
            .input_device
            .take()
            .unwrap_or_else(|| "未知输入设备".into());
        let sample_rate = inner.sample_rate.take().unwrap_or(TARGET_SAMPLE_RATE);
        inner.source_frames.take();
        inner.clipboard_collector.reset();
        let failure = self.telemetry.failure();
        self.telemetry.stop();
        if let Some(failure) = failure {
            return Err(failure);
        }
        Ok(CapturedRecording {
            pcm,
            input_device,
            sample_rate,
            clipboard_manifest,
        })
    }

    pub(crate) fn is_recording(&self) -> bool {
        self.telemetry.is_active() && self.inner.lock().expect("recording mutex").stream.is_some()
    }

    pub(crate) fn status(&self) -> RecordingStatus {
        self.telemetry.status()
    }

    pub(crate) fn attach_app(&self, app: tauri::AppHandle) {
        self.telemetry.attach_app(app);
    }

    /// 取出 CPAL 回调产生的异步设备错误；消费方应使用当前 task_id 再交给 Coordinator
    /// 校验，避免旧设备流的错误覆盖新任务。
    pub(crate) fn drain_async_errors(&self) -> Vec<RecordingAsyncError> {
        self.error_rx
            .lock()
            .expect("recording error channel mutex")
            .try_iter()
            .collect()
    }

    /// 由 Coordinator 的错误消费循环调用。只清理仍属于该 generation 的活动流，
    /// 并保持幂等；旧流错误不会触碰新一代录音。
    pub(crate) fn cleanup_after_async_error(&self, generation: u64) -> bool {
        if self.generation.load(Ordering::Acquire) != generation {
            return false;
        }
        let mut inner = self.inner.lock().expect("recording mutex");
        let Some(stream) = inner.stream.take() else {
            return false;
        };
        inner.clipboard_collector.freeze();
        drop(stream);
        let source_frames = inner
            .source_frames
            .as_ref()
            .map(|frames| frames.load(Ordering::Acquire))
            .unwrap_or(0);
        inner.clipboard_collector.finalize(source_frames);
        let _ = inner.clipboard_collector.take_manifest();
        inner.pcm.take();
        inner.input_device.take();
        inner.sample_rate.take();
        inner.source_frames.take();
        inner.clipboard_collector.reset();
        self.telemetry.stop();
        true
    }
}

fn build_input_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    channels: usize,
    pcm: Arc<Mutex<Vec<f32>>>,
    source_frames: Arc<AtomicU64>,
    telemetry: Arc<RecordingTelemetry>,
    error_tx: Sender<RecordingAsyncError>,
    generation: u64,
) -> Result<cpal::Stream, String> {
    match sample_format {
        cpal::SampleFormat::F32 => {
            let callback_telemetry = telemetry.clone();
            let error_telemetry = telemetry.clone();
            let error_tx = error_tx.clone();
            device
                .build_input_stream(
                    *config,
                    move |data: &[f32], _| {
                        append_mono(
                            data,
                            channels,
                            &pcm,
                            &source_frames,
                            &callback_telemetry,
                            |sample| sample,
                        )
                    },
                    move |err| {
                        let error = cpal_recording_error(&err);
                        error_telemetry.record_error(error.clone());
                        let _ = error_tx.send(RecordingAsyncError {
                            generation,
                            code: error,
                        });
                    },
                    None,
                )
                .map_err(|error| cpal_recording_error(&error))
        }
        cpal::SampleFormat::I16 => {
            let callback_telemetry = telemetry.clone();
            let error_telemetry = telemetry.clone();
            let error_tx = error_tx.clone();
            device
                .build_input_stream(
                    *config,
                    move |data: &[i16], _| {
                        append_mono(
                            data,
                            channels,
                            &pcm,
                            &source_frames,
                            &callback_telemetry,
                            |sample| sample as f32 / i16::MAX as f32,
                        )
                    },
                    move |err| {
                        let error = cpal_recording_error(&err);
                        error_telemetry.record_error(error.clone());
                        let _ = error_tx.send(RecordingAsyncError {
                            generation,
                            code: error,
                        });
                    },
                    None,
                )
                .map_err(|error| cpal_recording_error(&error))
        }
        cpal::SampleFormat::U16 => {
            let callback_telemetry = telemetry.clone();
            let error_telemetry = telemetry.clone();
            let error_tx = error_tx.clone();
            device
                .build_input_stream(
                    *config,
                    move |data: &[u16], _| {
                        append_mono(
                            data,
                            channels,
                            &pcm,
                            &source_frames,
                            &callback_telemetry,
                            |sample| (sample as f32 / u16::MAX as f32) * 2.0 - 1.0,
                        )
                    },
                    move |err| {
                        let error = cpal_recording_error(&err);
                        error_telemetry.record_error(error.clone());
                        let _ = error_tx.send(RecordingAsyncError {
                            generation,
                            code: error,
                        });
                    },
                    None,
                )
                .map_err(|error| cpal_recording_error(&error))
        }
        _ => Err("recording_device_error".into()),
    }
}

fn append_mono<T>(
    data: &[T],
    channels: usize,
    pcm: &Arc<Mutex<Vec<f32>>>,
    source_frames: &AtomicU64,
    telemetry: &RecordingTelemetry,
    to_f32: impl Fn(T) -> f32,
) where
    T: Copy,
{
    let mut target = pcm.lock().expect("recording pcm mutex");
    target.reserve(data.len() / channels);
    let mut energy = 0.0;
    let mut frames = 0usize;
    for frame in data.chunks_exact(channels) {
        let sum = frame.iter().copied().map(&to_f32).sum::<f32>();
        let sample = (sum / channels as f32).clamp(-1.0, 1.0);
        target.push(sample);
        energy += sample * sample;
        frames += 1;
    }
    if frames > 0 {
        source_frames.fetch_add(frames as u64, Ordering::Release);
        telemetry.update_level((energy / frames as f32).sqrt());
    }
}

#[derive(Clone, Serialize)]
pub(crate) struct RecordingStart {
    pub(crate) input_device: String,
    pub(crate) sample_rate: u32,
}

#[cfg(test)]
mod moved_tests {
    use super::*;
    #[test]
    fn cpal_errors_distinguish_missing_permission_and_generic_failures() {
        assert_eq!(
            cpal_recording_error(&cpal::Error::new(cpal::ErrorKind::DeviceNotAvailable)),
            "recording_microphone_unavailable"
        );
        assert_eq!(
            cpal_recording_error(&cpal::Error::new(cpal::ErrorKind::PermissionDenied)),
            "recording_microphone_unauthorized"
        );
        assert_eq!(
            cpal_recording_error(&cpal::Error::new(cpal::ErrorKind::UnsupportedConfig)),
            "recording_device_error"
        );
    }
}

#[cfg(test)]
mod owner_tests {
    use super::*;
    #[test]
    fn cpal_error_consumer_shutdown_joins_worker_and_is_idempotent() {
        let consumer = Arc::new(CpalErrorConsumer::default());
        let exited = Arc::new(AtomicBool::new(false));
        let consumer_for_worker = Arc::clone(&consumer);
        let exited_for_worker = Arc::clone(&exited);
        consumer.install(thread::spawn(move || {
            while !consumer_for_worker.stop_requested() {
                thread::sleep(Duration::from_millis(1));
            }
            exited_for_worker.store(true, Ordering::Release);
        }));

        consumer.shutdown();
        consumer.shutdown();
        assert!(exited.load(Ordering::Acquire));
    }

    #[test]
    fn audio_callback_counts_source_frames_after_downmixing() {
        let pcm = Arc::new(Mutex::new(Vec::new()));
        let frames = AtomicU64::new(0);
        let telemetry = RecordingTelemetry::new();
        // 两声道的三个 source frame，计数必须按帧而非样本数递增。
        append_mono(
            &[0.25_f32, 0.75, -0.5, 0.5, 1.0, -1.0],
            2,
            &pcm,
            &frames,
            &telemetry,
            |sample| sample,
        );
        assert_eq!(frames.load(Ordering::Acquire), 3);
        assert_eq!(*pcm.lock().unwrap(), vec![0.5, 0.0, 0.0]);
    }

    #[test]
    fn stopping_when_idle_does_not_freeze_the_collector() {
        let controller = RecordingController::new();
        assert!(controller.stop().is_err());
        assert_eq!(
            controller.inner.lock().unwrap().clipboard_collector.phase(),
            clipboard_collector::CollectorPhase::Idle
        );
    }

    #[test]
    fn recording_telemetry_reports_start_and_stop() {
        let telemetry = RecordingTelemetry::new();
        assert!(!telemetry.status().is_recording);
        telemetry.start("测试麦克风".into(), 48_000);
        telemetry.update_level(0.4);
        let active = telemetry.status();
        assert!(active.is_recording);
        assert_eq!(active.input_device.as_deref(), Some("测试麦克风"));
        assert_eq!(active.sample_rate, Some(48_000));
        assert!((active.level - 0.4).abs() < f32::EPSILON);
        telemetry.stop();
        let idle = telemetry.status();
        assert!(!idle.is_recording);
        assert_eq!(idle.elapsed_ms, 0);
        assert_eq!(idle.level, 0.0);
    }

    #[test]
    fn recording_device_error_ends_active_recording() {
        let telemetry = RecordingTelemetry::new();
        telemetry.start("测试麦克风".into(), 48_000);
        telemetry.record_error("recording_device_error".to_string());
        assert!(!telemetry.is_active());
        assert_eq!(
            telemetry.failure().as_deref(),
            Some("recording_device_error")
        );
    }
}
