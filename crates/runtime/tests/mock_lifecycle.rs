//! ST-M3.1 验收：MockRuntime spawn→health ready→stop 释放。

use seasnail_runtime::{MockRuntime, ModelRuntime};
use std::time::Duration;

#[tokio::test]
async fn mock_start_health_stop() {
    let rt = MockRuntime::openai_default();
    // port 0 = 随机端口，避免冲突。
    rt.start(0).await.expect("start");
    assert!(rt.health().await, "start 后 health true");

    rt.stop().await.expect("stop");
    // 等待 abort 生效（listener drop、端口释放）。
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!rt.health().await, "stop 后 health false");
}
