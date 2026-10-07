//! ST-M3.8 验收：start→stop→reload→health+transcribe（空闲超时卸载、按需 reload 骨架）。

use seasnail_runtime::contract::TranscribeReq;
use seasnail_runtime::{MockRuntime, ModelRuntime};
use std::path::PathBuf;
use std::time::Duration;

#[tokio::test]
async fn mock_reload_lifecycle() {
    let rt = MockRuntime::openai_default();
    rt.start(0).await.expect("start");
    assert!(rt.health().await, "首次 ready");

    rt.stop().await.expect("stop");
    tokio::time::sleep(Duration::from_millis(50)).await;

    // reload：同实例再 start（新随机端口）。
    rt.start(0).await.expect("reload start");
    assert!(rt.health().await, "reload 后 ready");

    let dir = tempfile::tempdir().expect("tempdir");
    let wav = dir.path().join("a.wav");
    std::fs::write(&wav, b"x").expect("write");
    let req = TranscribeReq {
        wav: PathBuf::from(&wav),
        language: None,
        prompt: None,
        punc: None,
        spk: None,
    };
    let segs = rt.transcribe(req).await.expect("transcribe after reload");
    assert_eq!(segs.text, "你好世界");

    rt.stop().await.expect("stop");
}
