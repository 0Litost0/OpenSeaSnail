//! ST-M3.2 上机回归：WhisperDriver 端到端（需真 whisper-server 二进制 + 模型 + wav）。
//!
//! `#[ignore]`——CI 不跑（无真二进制）。本地：
//! ```sh
//! export WHISPER_SERVER_PATH=~/project/github/whisper.cpp/build/bin/whisper-server
//! export WHISPER_MODEL_PATH=~/project/github/whisper.cpp/models/ggml-tiny.bin
//! export WHISPER_TEST_WAV=~/project/github/whisper.cpp/samples/jfk.wav
//! cargo test --ignored --test whisper_real -- --nocapture
//! ```
//! 未设 `WHISPER_SERVER_PATH` 则 skip。

use seasnail_runtime::contract::TranscribeReq;
use seasnail_runtime::{ModelRuntime, WhisperDriver};
use std::env;
use std::path::PathBuf;

#[tokio::test]
#[ignore]
async fn whisper_driver_real_inference() {
    let bin = match env::var("WHISPER_SERVER_PATH") {
        Ok(v) => v,
        Err(_) => {
            eprintln!("skip: WHISPER_SERVER_PATH 未设（真二进制上机测试，CI 不跑）");
            return;
        }
    };
    let model = env::var("WHISPER_MODEL_PATH").expect("WHISPER_MODEL_PATH 必填");
    let wav = env::var("WHISPER_TEST_WAV").expect("WHISPER_TEST_WAV 必填");

    let drv = WhisperDriver::new("whisper-tiny", PathBuf::from(&bin), PathBuf::from(&model));
    drv.start(8098).await.expect("start whisper-server");
    assert!(drv.health().await, "whisper-server health ready");

    let req = TranscribeReq {
        wav: PathBuf::from(&wav),
        language: None,
        prompt: None,
        punc: None,
        spk: None,
    };
    let segs = drv.transcribe(req).await.expect("transcribe");

    // 上机实测：verbose_json 返 {text, segments:[{id,text,start,end,...}]}。
    assert!(!segs.text.is_empty(), "全文非空");
    assert!(
        !segs.segments.is_empty(),
        "segments 非空（verbose_json 才有）"
    );
    assert!(
        segs.segments.iter().all(|s| s.speaker.is_none()),
        "whisper 无 speaker（M3.6 external）"
    );
    // 时间字段秒（float），首段 start=0。
    assert!(segs.segments[0].start >= 0.0);
    assert!(segs.segments[0].end > segs.segments[0].start, "end > start");

    eprintln!("text: {}", segs.text.trim());
    eprintln!("segments: {} 段", segs.segments.len());
    for s in &segs.segments {
        eprintln!("  [{:.2}-{:.2}] {}", s.start, s.end, s.text.trim());
    }

    drv.stop().await.expect("stop whisper-server");
}
