//! FunASR hermetic bundle 的真实 driver 回归（默认忽略，需本地完整 bundle）。
//!
//! ```sh
//! SEASNAIL_FUNASR_ROOT=/path/to/funasr \
//!   cargo test -p seasnail-runtime --test funasr_real -- --ignored --nocapture
//! ```
//!
//! `SEASNAIL_FUNASR_ROOT` 须包含 `python/`、`sidecar.py` 与四个 `models/*`
//! 目录。测试只连接 driver 自行拉起的 loopback sidecar，不访问网络。

use seasnail_runtime::contract::TranscribeReq;
use seasnail_runtime::{FunAsrDriver, ModelRuntime};
use std::path::PathBuf;

#[tokio::test]
#[ignore = "requires a complete local FunASR bundle and a loopback port"]
async fn funasr_driver_transcribes_with_local_bundle() {
    let root = match std::env::var_os("SEASNAIL_FUNASR_ROOT") {
        Some(value) => PathBuf::from(value),
        None => {
            eprintln!("skip: set SEASNAIL_FUNASR_ROOT to a complete FunASR bundle");
            return;
        }
    };
    let driver = FunAsrDriver::new(
        "funasr-default",
        root.join("python/bin/python3"),
        root.join("sidecar.py"),
        root.join("models"),
        "mps",
        None,
    );
    driver.start(0).await.expect("start local FunASR sidecar");
    assert!(
        driver.health().await,
        "sidecar health endpoint should be ready"
    );

    let result = driver
        .transcribe(TranscribeReq {
            wav: root.join("models/asr/example/zh.mp3"),
            language: Some("zh".into()),
            prompt: None,
            punc: None,
            // 该测试断言 CAM++ speaker 标签全段就位，显式启用 spk（M1.1 后默认 None→false 不产标签）。
            spk: Some(true),
        })
        .await
        .expect("transcribe bundled Chinese sample");
    assert!(
        !result.text.trim().is_empty(),
        "transcript should not be empty"
    );
    assert!(
        result
            .segments
            .iter()
            .all(|segment| segment.speaker.is_some()),
        "CAM++ result should carry a speaker label for every segment"
    );
    driver.stop().await.expect("stop local FunASR sidecar");
}
