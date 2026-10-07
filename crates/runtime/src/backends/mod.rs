//! 运行时 backend 实现的内部组织边界。
//!
//! [`crate::ModelRuntime`]、契约类型和 driver 的顶层 re-export 是对 daemon 与其他
//! 调用方的稳定 API；本模块只收口具体 backend 实现。

pub mod factory;
#[path = "../funasr.rs"]
pub mod funasr;
pub mod sensevoice_gguf;
pub mod sherpa_onnx;
pub mod sherpa_protocol;
#[path = "../whisper.rs"]
pub mod whisper;

pub use factory::{
    BackendRegistry, BackendRegistryError, BackendResources, PreflightedFunAsrResources,
    PreflightedWhisperResources, VerifiedGgufResources, VerifiedSherpaResources,
};
pub use funasr::FunAsrDriver;
pub use sensevoice_gguf::SenseVoiceGgufDriver;
pub use sherpa_onnx::SherpaOnnxDriver;
pub use whisper::WhisperDriver;
