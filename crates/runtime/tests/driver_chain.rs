//! ST-M3.8 验收：经 SidecarRegistry 取活跃 runtime 再 transcribe（业务层视角）。

use seasnail_runtime::contract::TranscribeReq;
use seasnail_runtime::{MockRuntime, ModelRuntime, SidecarRegistry};
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::test]
async fn driver_via_registry_transcribe() {
    let reg = SidecarRegistry::new();
    let rt = Arc::new(MockRuntime::openai_default());
    rt.start(0).await.expect("start");
    reg.register(rt.clone() as Arc<dyn ModelRuntime>).await;

    let active = reg.active().await.expect("active");
    assert!(active.health().await, "经 registry health");

    let dir = tempfile::tempdir().expect("tempdir");
    let wav = dir.path().join("a.wav");
    std::fs::write(&wav, b"y").expect("write");
    let req = TranscribeReq {
        wav: PathBuf::from(&wav),
        language: None,
        prompt: None,
        punc: None,
        spk: None,
    };
    let segs = active
        .transcribe(req)
        .await
        .expect("transcribe via registry");
    assert_eq!(segs.text, "你好世界");

    active.stop().await.expect("stop");
    reg.clear().await;
}
