//! ST-M2.1：backend 目录边界不改变既有公开 driver 构造和 runtime 契约。

use seasnail_runtime::{backends, FunAsrDriver, ModelRuntime, RuntimeKind, WhisperDriver};
use std::path::PathBuf;

#[test]
fn stable_driver_exports_and_backend_namespace_remain_constructible() {
    let whisper = WhisperDriver::new(
        "whisper-test",
        PathBuf::from("/missing/whisper-server"),
        PathBuf::from("/missing/model.bin"),
    );
    assert_eq!(whisper.runtime_kind(), RuntimeKind::Whisper);

    let funasr = FunAsrDriver::new(
        "funasr-test",
        PathBuf::from("/missing/python"),
        PathBuf::from("/missing/sidecar.py"),
        PathBuf::from("/missing/models"),
        "cpu",
        None,
    );
    assert_eq!(funasr.runtime_kind(), RuntimeKind::FunAsr);

    let namespaced = backends::WhisperDriver::new(
        "whisper-namespaced",
        PathBuf::from("/missing/whisper-server"),
        PathBuf::from("/missing/model.bin"),
    );
    assert_eq!(namespaced.runtime_kind(), RuntimeKind::Whisper);
}
