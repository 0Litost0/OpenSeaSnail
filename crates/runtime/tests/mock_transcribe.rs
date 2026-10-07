//! ST-M3.8 验收：MockRuntime transcribe 返回 canned，driver→sidecar HTTP 链路通畅。

use seasnail_runtime::{
    contract::{OpenAiSegment, OpenAiWord, TranscribeReq},
    mock::CannedResponse,
    MockRuntime, ModelRuntime, RuntimeKind,
};
use std::path::PathBuf;

#[tokio::test]
async fn mock_transcribe_returns_canned() {
    let rt = MockRuntime::openai_default();
    rt.start(0).await.expect("start");
    assert!(rt.health().await, "ready");

    // mock 不解析音频内容，任意字节即可。
    let dir = tempfile::tempdir().expect("tempdir");
    let wav = dir.path().join("audio.wav");
    std::fs::write(&wav, b"fake-wav-bytes").expect("write fixture");

    let req = TranscribeReq {
        wav: PathBuf::from(&wav),
        language: Some("zh".into()),
        prompt: None,
        punc: None,
        spk: None,
    };
    let segs = rt.transcribe(req).await.expect("transcribe");

    assert_eq!(segs.text, "你好世界");
    assert_eq!(segs.segments.len(), 1);
    assert_eq!(segs.segments[0].start, 0.0);
    assert_eq!(segs.segments[0].end, 1.0);
    assert_eq!(segs.segments[0].text, "你好世界");
    assert!(segs.segments[0].speaker.is_none());

    rt.stop().await.expect("stop");
}

/// M4.1/M5.3 回归：非空、单调、与 text 对齐的 words 穿过 driver→`validate_words` 存活，
/// 不被清空（词级时间戳链路不退）。`openai_default` 默认 words 空，本测显式置入非空 words。
#[tokio::test]
async fn mock_transcribe_preserves_aligned_words() {
    let canned = CannedResponse {
        text: "你好世界".into(),
        segments: vec![OpenAiSegment {
            start: 0.0,
            end: 1.0,
            text: "你好世界".into(),
            speaker: None,
        }],
        words: vec![
            OpenAiWord {
                start: 0.1,
                end: 0.2,
                text: "你".into(),
            },
            OpenAiWord {
                start: 0.3,
                end: 0.4,
                text: "好".into(),
            },
            OpenAiWord {
                start: 0.5,
                end: 0.6,
                text: "世".into(),
            },
            OpenAiWord {
                start: 0.7,
                end: 0.8,
                text: "界".into(),
            },
        ],
    };
    let rt = MockRuntime::new("mock-words", RuntimeKind::Whisper, canned);
    rt.start(0).await.expect("start");
    assert!(rt.health().await, "ready");

    let dir = tempfile::tempdir().expect("tempdir");
    let wav = dir.path().join("audio.wav");
    std::fs::write(&wav, b"fake-wav-bytes").expect("write fixture");

    let req = TranscribeReq {
        wav: PathBuf::from(&wav),
        language: Some("zh".into()),
        prompt: None,
        punc: None,
        spk: None,
    };
    let segs = rt.transcribe(req).await.expect("transcribe");

    // 对齐 + 单调 → words 存活（不被 validate_words 清空）。
    assert_eq!(segs.words.len(), 4, "对齐 words 应穿过 validate_words 存活");
    assert_eq!(segs.words[0].text, "你");
    assert!(segs.words[0].start <= segs.words[0].end, "时间戳单调");

    rt.stop().await.expect("stop");
}

/// M5.3 回归：misaligned words 经 driver→`validate_words` 被清空（兜底降级不退）。
#[tokio::test]
async fn mock_transcribe_clears_misaligned_words() {
    let canned = CannedResponse {
        text: "你好世界".into(),
        segments: vec![OpenAiSegment {
            start: 0.0,
            end: 1.0,
            text: "你好世界".into(),
            speaker: None,
        }],
        // 拼接 "XYZABC" 不在 text 中 → 对齐失败 → validate_words 清空。
        words: vec![
            OpenAiWord {
                start: 0.1,
                end: 0.2,
                text: "XYZ".into(),
            },
            OpenAiWord {
                start: 0.3,
                end: 0.4,
                text: "ABC".into(),
            },
        ],
    };
    let rt = MockRuntime::new("mock-misalign", RuntimeKind::Whisper, canned);
    rt.start(0).await.expect("start");

    let dir = tempfile::tempdir().expect("tempdir");
    let wav = dir.path().join("audio.wav");
    std::fs::write(&wav, b"fake-wav-bytes").expect("write fixture");

    let req = TranscribeReq {
        wav: PathBuf::from(&wav),
        language: Some("zh".into()),
        prompt: None,
        punc: None,
        spk: None,
    };
    let segs = rt.transcribe(req).await.expect("transcribe");

    assert!(segs.words.is_empty(), "对齐失败 words 应被清空（兜底降级）");
    assert_eq!(segs.text, "你好世界", "text 不受影响");

    rt.stop().await.expect("stop");
}
