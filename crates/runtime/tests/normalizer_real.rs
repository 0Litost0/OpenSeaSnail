//! ST-M3.3 上机回归：AudioNormalizer 端到端（需 ffmpeg 二进制）。
//!
//! 非 `#[ignore]`——ffmpeg 常见（brew 装机），探测到即跑、未装即 skip。
//! 路径解析：`FFMPEG_PATH` env → 系统 PATH `ffmpeg` → skip。
//! ```sh
//! FFMPEG_PATH=/opt/homebrew/bin/ffmpeg cargo test -p seasnail-runtime --test normalizer_real -- --nocapture
//! ```

use seasnail_runtime::{AudioNormalizer, TARGET_SAMPLE_RATE};
use std::env;
use std::f32::consts::PI;
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// 解析 ffmpeg：env `FFMPEG_PATH` → PATH `ffmpeg`（`-version` 探测）→ None。
async fn resolve_ffmpeg() -> Option<PathBuf> {
    if let Ok(p) = env::var("FFMPEG_PATH") {
        return Some(PathBuf::from(p));
    }
    let probe = Command::new("ffmpeg").arg("-version").output().await.ok()?;
    if probe.status.success() {
        Some(PathBuf::from("ffmpeg"))
    } else {
        None
    }
}

/// 1s 440Hz sine @ 44100 mono f32（输入非目标率，验 normalize 真转码）。
fn sine_44100() -> Vec<f32> {
    (0..44_100)
        .map(|i| (2.0 * PI * 440.0 * (i as f32) / 44_100.0).sin() * 0.8)
        .collect()
}

/// WAV 头解析——**chunk 扫描**（ffmpeg 产出的 WAV 在 fmt 与 data 间可能插 LIST/INFO
/// 等额外 chunk，不能假定 data 在固定 offset 36；from_pcm 自产 WAV 无额外 chunk，
/// 其单测用固定 offset 验自身布局，此解析器用于验 ffmpeg 产出）。
struct WavInfo {
    audio_format: u16,
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
    data_size: u32,
}

fn parse_wav(b: &[u8]) -> WavInfo {
    assert_eq!(&b[0..4], b"RIFF");
    assert_eq!(&b[8..12], b"WAVE");
    let mut p = 12; // RIFF 12B 头后逐 chunk：id4 + size4 + body[+pad]
    let (mut audio_format, mut channels, mut sample_rate, mut bits_per_sample, mut data_size) =
        (0u16, 0u16, 0u32, 0u16, 0u32);
    while p + 8 <= b.len() {
        let id = &b[p..p + 4];
        let sz = u32::from_le_bytes(b[p + 4..p + 8].try_into().unwrap()) as usize;
        let body = p + 8;
        match id {
            b"fmt " if body + 16 <= b.len() => {
                audio_format = u16::from_le_bytes(b[body..body + 2].try_into().unwrap());
                channels = u16::from_le_bytes(b[body + 2..body + 4].try_into().unwrap());
                sample_rate = u32::from_le_bytes(b[body + 4..body + 8].try_into().unwrap());
                bits_per_sample = u16::from_le_bytes(b[body + 14..body + 16].try_into().unwrap());
            }
            b"data" => {
                data_size = sz as u32;
                break; // data 后无需再扫
            }
            _ => {}
        }
        p = body + sz + (sz & 1); // chunk 体 pad 到偶数
    }
    assert_ne!(audio_format, 0, "fmt chunk 未找到");
    assert_ne!(data_size, 0, "data chunk 未找到");
    WavInfo {
        audio_format,
        channels,
        sample_rate,
        bits_per_sample,
        data_size,
    }
}

#[tokio::test]
async fn normalize_roundtrip_44100_to_16000() {
    let ffmpeg = match resolve_ffmpeg().await {
        Some(p) => p,
        None => {
            eprintln!("skip: ffmpeg 未找到（设 FFMPEG_PATH 或装系统 ffmpeg）");
            return;
        }
    };
    let norm = AudioNormalizer::new(ffmpeg);

    // 输入：from_pcm 产 44100 mono WAV（不经 ffmpeg），写临时文件喂 normalize。
    let pcm = sine_44100();
    let in_wav = AudioNormalizer::from_pcm(&pcm, 44_100);
    let in_file = tempfile::Builder::new()
        .prefix("seasnail-in-")
        .suffix(".wav")
        .tempfile()
        .expect("input tempfile");
    let in_path = in_file.path().to_path_buf();
    std::fs::write(&in_path, &in_wav).expect("write input wav");

    let out = norm.normalize(&in_path).await.expect("normalize");
    let out_path = out.path().to_path_buf();

    // 产出须不同于输入（临时输出文件）。
    assert_ne!(out_path, in_path, "输出为独立临时文件");

    let bytes = std::fs::read(&out_path).expect("read output wav");
    let info = parse_wav(&bytes);
    assert_eq!(info.audio_format, 1, "PCM");
    assert_eq!(info.channels, 1, "mono");
    assert_eq!(
        info.sample_rate, TARGET_SAMPLE_RATE,
        "16kHz（非输入 44100）"
    );
    assert_eq!(info.bits_per_sample, 16, "s16le");
    // 1s @ 16kHz ≈ 16000 样本 × 2B = 32000B；容 ffmpeg 重采样边界 ±。
    assert!(
        info.data_size > 28_000 && info.data_size < 36_000,
        "data_size 合理: {}",
        info.data_size
    );

    // drop 即删：NormalizedWav 释放后临时输出文件不存在。
    drop(out);
    assert!(
        std::fs::metadata(&out_path).is_err(),
        "drop 后临时输出文件应删"
    );
    eprintln!(
        "normalize ok: 44100→{}Hz mono s16le, data {}B",
        info.sample_rate, info.data_size
    );
}

#[tokio::test]
async fn normalize_error_on_missing_input() {
    let ffmpeg = match resolve_ffmpeg().await {
        Some(p) => p,
        None => {
            eprintln!("skip: ffmpeg 未找到");
            return;
        }
    };
    let norm = AudioNormalizer::new(ffmpeg);
    let bogus = Path::new("/nonexistent/seasnail-xyz-no-such-file.wav");
    let err = norm
        .normalize(bogus)
        .await
        .expect_err("missing input 须报错");
    let msg = err.to_string();
    assert!(msg.contains("ffmpeg"), "错误提及 ffmpeg: {msg}");
    eprintln!("normalize error ok: {msg}");
}

/// mp3 输入 → 16kHz mono WAV：验 ffmpeg 格式自动探测（任意容器/编码归一）。
/// 先用 ffmpeg 把 sine wav 编成 mp3（libmp3lame），再喂 normalize。无 libmp3lame 则 skip。
#[tokio::test]
async fn normalize_mp3_input_to_16000() {
    let ffmpeg = match resolve_ffmpeg().await {
        Some(p) => p,
        None => {
            eprintln!("skip: ffmpeg 未找到");
            return;
        }
    };

    // 1) from_pcm 产 44100 sine wav，写临时文件。
    let pcm = sine_44100();
    let in_wav_bytes = AudioNormalizer::from_pcm(&pcm, 44_100);
    let in_wav = tempfile::Builder::new()
        .prefix("seasnail-mp3src-")
        .suffix(".wav")
        .tempfile()
        .expect("wav tempfile");
    let in_wav_path = in_wav.path().to_path_buf();
    std::fs::write(&in_wav_path, &in_wav_bytes).expect("write wav");

    // 2) wav→mp3（libmp3lame）。编码失败（ffmpeg 无 mp3 编码器）→ skip 不 fail。
    let mp3 = tempfile::Builder::new()
        .prefix("seasnail-in-")
        .suffix(".mp3")
        .tempfile()
        .expect("mp3 tempfile");
    let mp3_path = mp3.path().to_path_buf();
    let enc = Command::new(&ffmpeg)
        .arg("-y")
        .arg("-i")
        .arg(&in_wav_path)
        .arg("-c:a")
        .arg("libmp3lame")
        .arg("-b:a")
        .arg("64k")
        .arg(&mp3_path)
        .output()
        .await
        .expect("encode mp3");
    if !enc.status.success() {
        eprintln!(
            "skip: mp3 编码失败（ffmpeg 缺 libmp3lame？）: {}",
            String::from_utf8_lossy(&enc.stderr)
                .lines()
                .last()
                .unwrap_or("")
        );
        return;
    }
    assert!(
        std::fs::metadata(&mp3_path)
            .map(|m| m.len() > 0)
            .unwrap_or(false),
        "mp3 非空"
    );

    // 3) normalize mp3 → 16kHz mono s16le wav。
    let norm = AudioNormalizer::new(ffmpeg);
    let out = norm.normalize(&mp3_path).await.expect("normalize mp3");
    let bytes = std::fs::read(out.path()).expect("read output wav");
    let info = parse_wav(&bytes);
    assert_eq!(info.audio_format, 1, "PCM");
    assert_eq!(info.channels, 1, "mono");
    assert_eq!(info.sample_rate, TARGET_SAMPLE_RATE, "16kHz");
    assert_eq!(info.bits_per_sample, 16, "s16le");
    assert!(
        info.data_size > 28_000,
        "mp3→wav data_size: {}",
        info.data_size
    );
    eprintln!(
        "normalize mp3→{}Hz mono s16le ok, data {}B",
        info.sample_rate, info.data_size
    );
}
