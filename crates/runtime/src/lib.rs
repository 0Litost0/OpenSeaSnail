//! SeaSnail M3 运行时抽象 crate（ST-M3.1 + ST-M3.8）。
//!
//! 屏蔽 ASR 模型差异的运行时抽象：[`ModelRuntime`] trait 统一 whisper / FunASR /
//! mock 的 lifecycle（start/stop/health）与转写（transcribe）。设计文档同时列了
//! `ModelRuntime::transcribe` 与 `Driver::translate` 两 trait，语义重叠——此处折叠为
//! 单 trait，[`WhisperDriver`] 命名保留 driver 翻译层语义，其 `transcribe` 内含
//! `/inference` 调用 + t0/t1→start/end 归一。
//!
//! ST-M3.6 [`diarize`]：whisper.cpp 后端（diarization=external）另起
//! `sherpa-onnx-diarize` 一次性进程产 speaker 标签，按时间窗合并进 segments
//!（MVP A/B/C）。HTTP 端点 / 状态机 / 模型管理（M3.10）属 daemon crate。
//! ST-M3.3 [`AudioNormalizer`] 与运行时 gate（single-flight）已落地。

pub mod artifact_manifest;
pub mod backends;
pub mod canonical;
pub mod contract;
pub mod diarize;
pub mod error;
pub mod facade;
pub mod gate;
pub mod gguf_manifest;
pub mod mock;
pub mod normalizer;
pub mod registry;
pub mod reservation;
pub mod sidecar;

pub use artifact_manifest::{
    verify_artifact, ArtifactCatalog, ArtifactFactory, ArtifactIdentity, ArtifactManifestError,
    ArtifactRequirements, ArtifactResolver, ArtifactVerificationCache, VerifiedArtifact,
    VerifiedArtifactFile, VerifiedRuntimeResources, ARTIFACT_MANIFEST_FILE,
};
pub use backends::{
    BackendRegistry, BackendRegistryError, BackendResources, PreflightedFunAsrResources,
    PreflightedWhisperResources, VerifiedGgufResources, VerifiedSherpaResources,
};
pub use backends::{FunAsrDriver, WhisperDriver};
pub use backends::{SenseVoiceGgufDriver, SherpaOnnxDriver};
pub use canonical::{CanonicalTranscript, CanonicalUnit, Granularity};
pub use contract::{Capabilities, Diarization, RuntimeKind, UnknownRuntimeKind};
pub use diarize::{
    merge_speakers, parse_diarize_stdout, resolve_diarize_driver, DiarizationSegment, DiarizeDriver,
};
pub use error::RuntimeError;
pub use facade::{
    ActivateRuntime, RegistryRuntimeAdmin, RegistryTranscriptionEngine, RuntimeAdmin,
    RuntimeFailure, RuntimeStatus, TranscriptionEngine, TranscriptionOutcome, TranscriptionRequest,
};
pub use gate::{RuntimeLease, RuntimeOperation, RuntimeOperationGate, RuntimeOperationOccupied};
pub use gguf_manifest::{
    verify_gguf_resources, verify_legacy_gguf_artifact, GgufManifestError, GgufResources,
    GgufVariant,
};
pub use mock::MockRuntime;
pub use normalizer::{AudioNormalizer, NormalizedWav, TARGET_SAMPLE_RATE};
pub use registry::{ReapReport, SidecarRegistry};
pub use reservation::{
    reserve_retry_transcription, reserve_transcription, ReservedTranscription,
    RuntimeReservationError,
};
pub use sidecar::SidecarProcess;

use async_trait::async_trait;
use contract::{OpenAiSegments, TranscribeReq};
use std::io;

/// ASR 运行时抽象。
///
/// 每个 impl 管理 own 生命周期内部状态：真二进制 driver（whisper/funasr）持
/// [`SidecarProcess`]（子进程 + stderr drain + kill/wait），mock 持进程内 axum
/// task（[`MockRuntime`]）。`transcribe` 都是 HTTP POST loopback——验证
/// driver→sidecar 调用链路，而非退化成进程内函数调用。
#[async_trait]
pub trait ModelRuntime: Send + Sync {
    /// 稳定标识，如 "whisper-base" / "mock-openai"。
    fn id(&self) -> &str;
    fn runtime_kind(&self) -> RuntimeKind;
    fn capabilities(&self) -> Capabilities;

    /// spawn sidecar 监听 `port`，health 轮询 GET / 至 ready 或超时。
    async fn start(&self, port: u16) -> io::Result<()>;

    /// 停 sidecar + reap（kill+wait / abort task），释放活跃模型。幂等。
    async fn stop(&self) -> io::Result<()>;

    /// GET / 存活探测。
    async fn health(&self) -> bool;

    /// POST 音频到 sidecar 的 OpenAI 形状端点，返回归一后的 segments。
    async fn transcribe(&self, req: TranscribeReq) -> Result<OpenAiSegments, RuntimeError>;
}

pub mod process_identity;
