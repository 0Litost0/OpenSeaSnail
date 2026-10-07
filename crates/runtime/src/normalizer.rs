//! 音频归一化（ST-M3.3）：内置 ffmpeg 静态二进制 subprocess 调用 + 实时 PCM 封 WAV 头。
//!
//! 两条数据流（对齐设计文档「音频归一化模块」）：
//! - **文件导入**：[`AudioNormalizer::normalize`] 跑 `ffmpeg -i in -ar 16000 -ac 1 -c:a pcm_s16le out.wav`，产明文 16kHz mono s16le WAV 临时文件（[`NormalizedWav`] 持有，drop 即删），喂两运行时。
//! - **实时采集**：[`AudioNormalizer::from_pcm`] 将 cpal f32 mono PCM 直接量化 s16le + 封 WAV 头，**不经 ffmpeg**（cpal 已配 16kHz 采集，详见 ST-M6.3；`from_pcm` 不重采样，`sample_rate` 原样写头）。
//!
//! ffmpeg 路径由构造注入（生产 app bundle `Resources/ffmpeg` 绝对路径，不依赖系统
//! PATH）；失败时 stderr 过滤关键错误行入 [`io::Error`]（复用
//! [`crate::sidecar::strip_control_chars`] 去控制字符，不全量灌——防 ffmpeg 进度刷屏）。
//! 流式大文件后置（设计标注 MVP 先落临时文件）。

use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

use crate::sidecar::strip_control_chars;

/// 归一化目标采样率：16kHz（喂两运行时统一格式）。
/// 实时采集 cpal 须配此率（ST-M6.3），[`AudioNormalizer::from_pcm`] 不重采样。
pub const TARGET_SAMPLE_RATE: u32 = 16_000;
const TARGET_CHANNELS: u16 = 1;
const TARGET_BITS_PER_SAMPLE: u16 = 16;
/// PCM s16le 每样本字节数（f32→i16 量化后）。
const BYTES_PER_SAMPLE: usize = TARGET_BITS_PER_SAMPLE as usize / 8; // 2

/// 音频归一化器。持 ffmpeg 二进制绝对路径（生产内置 `Resources/ffmpeg`）。
///
/// `normalize` 为 async（tokio 子进程，非阻塞 runtime）；设计文档原 trait 草图为同步
/// `fn`——本 crate 全 async（[`crate::ModelRuntime`]），故 async 以免长文件阻塞 reactor。
#[derive(Debug, Clone)]
pub struct AudioNormalizer {
    ffmpeg: PathBuf,
    temp_dir: Option<PathBuf>,
}

/// `normalize` 产出的临时 WAV 文件句柄。**drop 即删**（含 panic 路径），调用方转译完
/// 自然释放即清理。ffmpeg 友好：内部持 [`tempfile::TempPath`]（不持打开 fd），ffmpeg
/// 独占写输出无争用。
#[derive(Debug)]
pub struct NormalizedWav {
    path: tempfile::TempPath,
}

impl NormalizedWav {
    /// 临时 WAV 路径（喂 [`crate::TranscribeReq::wav`]）。
    pub fn path(&self) -> &Path {
        self.path.as_ref()
    }

    /// 读取归一化 PCM WAV 的 data chunk 时长。归一化器固定产出 16kHz、mono、s16le，
    /// 但仍校验 RIFF/WAVE、fmt/data、采样率和字节率，避免把异常临时文件交给 ASR。
    pub fn duration(&self) -> io::Result<Duration> {
        AudioNormalizer::normalized_wav_duration(self.path())
    }
}

fn normalized_wav_duration_from_file(
    mut file: std::fs::File,
    file_len: u64,
) -> io::Result<Duration> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid normalized WAV format");
    let mut riff = [0u8; 12];
    file.read_exact(&mut riff)?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid normalized WAV header",
        ));
    }

    let mut pos = 12u64;
    let mut fmt = None;
    let mut data = None;
    while pos + 8 <= file_len {
        let mut chunk = [0u8; 8];
        file.read_exact(&mut chunk)?;
        pos += 8;
        let size = u32::from_le_bytes(chunk[4..8].try_into().expect("fixed slice")) as u64;
        let padded_size = size + size % 2;
        if pos
            .checked_add(padded_size)
            .is_none_or(|end| end > file_len)
        {
            return Err(invalid());
        }

        if &chunk[0..4] == b"fmt " {
            if size < 16 {
                return Err(invalid());
            }
            let mut fields = [0u8; 16];
            file.read_exact(&mut fields)?;
            fmt = Some((
                u16::from_le_bytes(fields[0..2].try_into().expect("fixed slice")),
                u16::from_le_bytes(fields[2..4].try_into().expect("fixed slice")),
                u32::from_le_bytes(fields[4..8].try_into().expect("fixed slice")),
                u32::from_le_bytes(fields[8..12].try_into().expect("fixed slice")),
                u16::from_le_bytes(fields[14..16].try_into().expect("fixed slice")),
            ));
            file.seek(SeekFrom::Current((size - 16) as i64))?;
        } else {
            if &chunk[0..4] == b"data" {
                data = Some(size);
            }
            file.seek(SeekFrom::Current(size as i64))?;
        }
        if size % 2 == 1 {
            file.seek(SeekFrom::Current(1))?;
        }
        pos += padded_size;
        if fmt.is_some() && data.is_some() {
            break;
        }
    }

    let (format, channels, sample_rate, byte_rate, bits) = fmt.ok_or_else(invalid)?;
    let data_size = data.ok_or_else(invalid)?;
    if format != 1
        || channels != TARGET_CHANNELS
        || sample_rate != TARGET_SAMPLE_RATE
        || bits != TARGET_BITS_PER_SAMPLE
        || byte_rate != TARGET_SAMPLE_RATE * TARGET_CHANNELS as u32 * BYTES_PER_SAMPLE as u32
    {
        return Err(invalid());
    }
    Ok(Duration::from_secs_f64(data_size as f64 / byte_rate as f64))
}

#[cfg(test)]
fn normalized_wav_duration(bytes: &[u8], file_len: u64) -> io::Result<Duration> {
    if bytes.len() < 44
        || &bytes[0..4] != b"RIFF"
        || &bytes[8..12] != b"WAVE"
        || &bytes[12..16] != b"fmt "
        || &bytes[36..40] != b"data"
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid normalized WAV header",
        ));
    }
    let format = u16::from_le_bytes(bytes[20..22].try_into().expect("fixed slice"));
    let channels = u16::from_le_bytes(bytes[22..24].try_into().expect("fixed slice"));
    let sample_rate = u32::from_le_bytes(bytes[24..28].try_into().expect("fixed slice"));
    let byte_rate = u32::from_le_bytes(bytes[28..32].try_into().expect("fixed slice"));
    let bits = u16::from_le_bytes(bytes[34..36].try_into().expect("fixed slice"));
    let data_size = u32::from_le_bytes(bytes[40..44].try_into().expect("fixed slice"));
    if format != 1
        || channels != TARGET_CHANNELS
        || sample_rate != TARGET_SAMPLE_RATE
        || bits != TARGET_BITS_PER_SAMPLE
        || byte_rate != TARGET_SAMPLE_RATE * TARGET_CHANNELS as u32 * BYTES_PER_SAMPLE as u32
        || file_len < 44 + data_size as u64
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid normalized WAV format",
        ));
    }
    Ok(Duration::from_secs_f64(data_size as f64 / byte_rate as f64))
}

impl AudioNormalizer {
    /// 构造。`ffmpeg` 应为绝对路径；生产由 app bundle 定位 `Resources/ffmpeg`，
    /// 测试可用 `FFMPEG_PATH` 或系统 PATH `ffmpeg`。
    pub fn new(ffmpeg: PathBuf) -> Self {
        Self {
            ffmpeg,
            temp_dir: None,
        }
    }

    /// Composition-root temp directory, shared by raw and normalized audio.
    pub fn with_temp_dir(ffmpeg: PathBuf, temp_dir: PathBuf) -> Self {
        Self {
            ffmpeg,
            temp_dir: Some(temp_dir),
        }
    }

    pub fn create_temp(&self, prefix: &str, suffix: &str) -> io::Result<tempfile::NamedTempFile> {
        let mut builder = tempfile::Builder::new();
        builder.prefix(prefix).suffix(suffix);
        match &self.temp_dir {
            Some(dir) => builder.tempfile_in(dir),
            None => builder.tempfile(),
        }
    }

    /// 校验一个已经由实时采集端生成的 16 kHz mono s16le WAV，并返回时长。
    ///
    /// 实时录音在桌面端已经完成 PCM→WAV 封装，不应再启动 ffmpeg；daemon
    /// 只需要复用该文件并执行同一份格式校验。
    pub fn normalized_wav_duration(path: &Path) -> io::Result<Duration> {
        let file_len = std::fs::metadata(path)?.len();
        normalized_wav_duration_from_file(std::fs::File::open(path)?, file_len)
    }

    /// 任意格式文件 → 16kHz mono s16le WAV 临时文件。
    ///
    /// ffmpeg 自动探测容器/编码（无需预判）；`-vn` 弃视频流（纯音频 ASR，防 mp4 等附
    /// 带视频轨道）。退出码非 0 → stderr 关键错误行入 [`io::Error`]。临时文件
    /// [`NormalizedWav`] drop 即删——原始音频单独加密落库（不经此路径，见 M3.5）。
    pub async fn normalize(&self, input: &Path) -> io::Result<NormalizedWav> {
        // TempPath：drop 删文件、不持 fd（ffmpeg 独占写）。失败时 out_path 随 Err 返回
        // 路径析构即清空文件，不泄漏。
        let out = self.create_temp("seasnail-norm-", ".wav")?;
        let out_path = out.into_temp_path();
        // 显式 &Path 绑定消歧（TempPath 同时 impl AsRef<Path>/AsRef<OsStr>）；
        // out_p 借用在 Command 链用毕即止（NLL），其后 out_path 可 move 入返回值。
        let out_p: &Path = out_path.as_ref();

        let pending = crate::process_identity::SpawnRecord::before_spawn("normalizer")?;
        let pending_path = pending.as_ref().map(|record| record.path().to_path_buf());
        let child = Command::new(&self.ffmpeg)
            .kill_on_drop(true)
            .arg("-y") // 覆写（TempPath 已存在空文件）
            .arg("-i")
            .arg(input)
            .arg("-vn") // 弃视频流
            .arg("-ar")
            .arg(TARGET_SAMPLE_RATE.to_string())
            .arg("-ac")
            .arg(TARGET_CHANNELS.to_string())
            .arg("-c:a")
            .arg("pcm_s16le")
            .arg("-f")
            .arg("wav")
            .arg(out_p)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(error) => {
                if let Some(record) = pending {
                    record.no_child()?;
                }
                return Err(error);
            }
        };
        let record = match pending
            .map(|record| {
                record.register(
                    child
                        .id()
                        .ok_or_else(|| io::Error::other("missing normalizer PID"))?,
                )
            })
            .transpose()
        {
            Ok(record) => record,
            Err(error) => {
                let _ = child.start_kill();
                if matches!(
                    tokio::time::timeout(Duration::from_secs(3), child.wait()).await,
                    Ok(Ok(_))
                ) {
                    if let Some(path) = pending_path {
                        let _ = std::fs::remove_file(path);
                    }
                }
                return Err(error);
            }
        };
        let output = child.wait_with_output().await?;
        if let Some(path) = record {
            crate::process_identity::clear_if_exited(&path)?;
        }

        if !output.status.success() {
            let key = filter_stderr(&String::from_utf8_lossy(&output.stderr));
            return Err(io::Error::other(format!(
                "ffmpeg normalize failed (exit {}): {}",
                output
                    .status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_default(),
                key,
            )));
        }
        tracing::debug!(
            input = %input.display(),
            exit = ?output.status.code(),
            "ffmpeg normalize ok"
        );
        Ok(NormalizedWav { path: out_path })
    }

    /// 实时采集 PCM（f32 mono，cpal 已采目标率）→ 完整 WAV bytes，不经 ffmpeg。
    ///
    /// f32 \[-1.0, 1.0\] → s16le（clamp + ×32767 + round）；`sample_rate` 原样写入头
    /// （**不重采样**——cpal 须配 [`TARGET_SAMPLE_RATE`]）。返回 44B 头 + s16le data。
    pub fn from_pcm(pcm: &[f32], sample_rate: u32) -> Vec<u8> {
        let mut data = Vec::with_capacity(pcm.len() * BYTES_PER_SAMPLE);
        for &s in pcm {
            let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            data.extend_from_slice(&v.to_le_bytes());
        }
        let mut wav = Vec::with_capacity(44 + data.len());
        wav.extend_from_slice(&wav_header(sample_rate, pcm.len()));
        wav.extend_from_slice(&data);
        wav
    }
}

/// 标准 RIFF/WAVE/PCM s16le mono 头（44B）。
///
/// `num_samples` 为帧数（mono 即样本数）。data_size = num_samples × channels ×
/// bytes_per_sample。RIFF chunk_size = 36 + data_size（文件总长 - 8）。
fn wav_header(sample_rate: u32, num_samples: usize) -> [u8; 44] {
    let channels = TARGET_CHANNELS as u32;
    let byte_rate = sample_rate * channels * (TARGET_BITS_PER_SAMPLE as u32 / 8);
    let block_align = TARGET_CHANNELS * TARGET_BITS_PER_SAMPLE / 8;
    let data_size = (num_samples * TARGET_CHANNELS as usize * BYTES_PER_SAMPLE) as u32;
    let riff_size = 36 + data_size;

    let mut h = [0u8; 44];
    let mut p = 0;
    h[p..p + 4].copy_from_slice(b"RIFF");
    p += 4;
    h[p..p + 4].copy_from_slice(&riff_size.to_le_bytes());
    p += 4;
    h[p..p + 4].copy_from_slice(b"WAVE");
    p += 4;
    h[p..p + 4].copy_from_slice(b"fmt ");
    p += 4;
    h[p..p + 4].copy_from_slice(&16u32.to_le_bytes()); // subchunk1 size（PCM=16）
    p += 4;
    h[p..p + 2].copy_from_slice(&1u16.to_le_bytes()); // audio_format = PCM
    p += 2;
    h[p..p + 2].copy_from_slice(&TARGET_CHANNELS.to_le_bytes());
    p += 2;
    h[p..p + 4].copy_from_slice(&sample_rate.to_le_bytes());
    p += 4;
    h[p..p + 4].copy_from_slice(&byte_rate.to_le_bytes());
    p += 4;
    h[p..p + 2].copy_from_slice(&block_align.to_le_bytes());
    p += 2;
    h[p..p + 2].copy_from_slice(&TARGET_BITS_PER_SAMPLE.to_le_bytes());
    p += 2;
    h[p..p + 4].copy_from_slice(b"data");
    p += 4;
    h[p..p + 4].copy_from_slice(&data_size.to_le_bytes());
    debug_assert_eq!(p, 40);
    h
}

/// 过滤 ffmpeg stderr 关键错误行：去控制字符 + 取含 error/invalid/not found/failed
/// 等关键词的行，最多 5 行（不全量灌，防进度刷屏）。无关键词命中则取末尾 3 非空行
/// 兜底（ffmpeg 常把错误摘要放最后）；皆空回 "unknown ffmpeg error"。
fn filter_stderr(stderr: &str) -> String {
    const KEYWORDS: &[&str] = &[
        "error",
        "invalid",
        "not found",
        "no such",
        "failed",
        "could not",
        "cannot",
        "unknown",
    ];
    let mut lines: Vec<String> = stderr
        .lines()
        .map(strip_control_chars)
        .filter(|l| {
            let low = l.to_ascii_lowercase();
            KEYWORDS.iter().any(|k| low.contains(k))
        })
        .take(5)
        .collect();
    if lines.is_empty() {
        // 无关键词：取末尾几非空行兜底。
        let mut tail: Vec<String> = stderr
            .lines()
            .map(strip_control_chars)
            .filter(|l| !l.trim().is_empty())
            .rev()
            .take(3)
            .collect();
        tail.reverse();
        lines = tail;
    }
    if lines.is_empty() {
        "unknown ffmpeg error".into()
    } else {
        lines.join(" | ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 解析 WAV 头（44B PCM），返回关键字段（仅验证 normalize/from_pcm 产出）。
    struct WavInfo {
        audio_format: u16,
        channels: u16,
        sample_rate: u32,
        bits_per_sample: u16,
        data_size: u32,
    }

    fn parse_wav(b: &[u8]) -> WavInfo {
        assert_eq!(&b[0..4], b"RIFF", "RIFF marker");
        assert_eq!(&b[8..12], b"WAVE", "WAVE marker");
        assert_eq!(&b[12..16], b"fmt ", "fmt marker");
        let audio_format = u16::from_le_bytes(b[20..22].try_into().unwrap());
        let channels = u16::from_le_bytes(b[22..24].try_into().unwrap());
        let sample_rate = u32::from_le_bytes(b[24..28].try_into().unwrap());
        let bits_per_sample = u16::from_le_bytes(b[34..36].try_into().unwrap());
        assert_eq!(&b[36..40], b"data", "data marker");
        let data_size = u32::from_le_bytes(b[40..44].try_into().unwrap());
        WavInfo {
            audio_format,
            channels,
            sample_rate,
            bits_per_sample,
            data_size,
        }
    }

    #[test]
    fn from_pcm_header_fields() {
        let pcm = vec![0.0f32; 100];
        let wav = AudioNormalizer::from_pcm(&pcm, 16_000);
        assert_eq!(wav.len(), 44 + 100 * 2, "44B 头 + 100×2B data");
        let info = parse_wav(&wav);
        assert_eq!(info.audio_format, 1, "PCM");
        assert_eq!(info.channels, 1, "mono");
        assert_eq!(info.sample_rate, 16_000);
        assert_eq!(info.bits_per_sample, 16);
        assert_eq!(info.data_size, 200);
    }

    #[test]
    fn from_pcm_sample_rate_reflected() {
        // cpal 采 44100（非目标率）时，from_pcm 原样写头（不重采样）。
        let wav = AudioNormalizer::from_pcm(&[0.0; 10], 44_100);
        let info = parse_wav(&wav);
        assert_eq!(info.sample_rate, 44_100);
    }

    #[test]
    fn from_pcm_quantization() {
        // 已知 f32 → i16le：0→0、1.0→32767、-1.0→-32767、0.5≈16384。
        let wav = AudioNormalizer::from_pcm(&[0.0, 1.0, -1.0, 0.5], 16_000);
        let samples: Vec<i16> = wav[44..]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(
            samples,
            vec![0, 32767, -32767, (0.5_f32 * 32767.0).round() as i16]
        );
    }

    #[test]
    fn from_pcm_clamps_overflow() {
        // 超域 f32 须 clamp 不溢出（>1.0 → 32767，<-1.0 → -32767）。
        let wav = AudioNormalizer::from_pcm(&[2.0, -3.0], 16_000);
        let s: Vec<i16> = wav[44..]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(s, vec![32767, -32767]);
    }

    #[test]
    fn from_pcm_empty() {
        let wav = AudioNormalizer::from_pcm(&[], 16_000);
        assert_eq!(wav.len(), 44, "仅头");
        let info = parse_wav(&wav);
        assert_eq!(info.data_size, 0);
    }

    #[test]
    fn from_pcm_roundtrip_decode() {
        // 一段 sine：解码回 i16 与直接量化一致。
        let pcm: Vec<f32> = (0..256).map(|i| ((i as f32) * 0.05).sin() * 0.8).collect();
        let wav = AudioNormalizer::from_pcm(&pcm, 16_000);
        let expected: Vec<i16> = pcm
            .iter()
            .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0).round() as i16)
            .collect();
        let decoded: Vec<i16> = wav[44..]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn wav_header_riff_size() {
        // RIFF chunk_size = 36 + data_size = 36 + num_samples*2。
        let h = wav_header(16_000, 50);
        let riff = u32::from_le_bytes(h[4..8].try_into().unwrap());
        assert_eq!(riff, 36 + 50 * 2);
        let data = u32::from_le_bytes(h[40..44].try_into().unwrap());
        assert_eq!(data, 50 * 2);
        // byte_rate = sample_rate × channels × bytes/sample = 16000×1×2。
        let byte_rate = u32::from_le_bytes(h[28..32].try_into().unwrap());
        assert_eq!(byte_rate, 16_000 * 2);
        // block_align = channels × bytes/sample = 2。
        let block_align = u16::from_le_bytes(h[32..34].try_into().unwrap());
        assert_eq!(block_align, 2);
    }

    #[test]
    fn normalized_wav_duration_preserves_sixty_minute_boundary() {
        let at_limit_samples = (TARGET_SAMPLE_RATE as usize) * 60 * 60;
        let at_limit = wav_header(TARGET_SAMPLE_RATE, at_limit_samples);
        assert_eq!(
            normalized_wav_duration(&at_limit, 44 + at_limit_samples as u64 * 2).unwrap(),
            Duration::from_secs(60 * 60)
        );

        let over_limit_samples = at_limit_samples + 1;
        let over_limit = wav_header(TARGET_SAMPLE_RATE, over_limit_samples);
        assert!(
            normalized_wav_duration(&over_limit, 44 + over_limit_samples as u64 * 2).unwrap()
                > Duration::from_secs(60 * 60)
        );
    }

    #[test]
    fn normalized_wav_duration_rejects_truncated_data() {
        let header = wav_header(TARGET_SAMPLE_RATE, 10);
        assert!(normalized_wav_duration(&header, 44).is_err());
    }

    #[test]
    fn normalized_wav_duration_skips_list_chunk_before_data() {
        // ffmpeg may emit a LIST/INFO metadata chunk between fmt and data.  Do not
        // assume the data header is always at byte 36.
        let data_size = TARGET_SAMPLE_RATE as usize * BYTES_PER_SAMPLE;
        let list_payload = b"INFOISFT\0";
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&0u32.to_le_bytes()); // Set once the full file is built.
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&TARGET_CHANNELS.to_le_bytes());
        wav.extend_from_slice(&TARGET_SAMPLE_RATE.to_le_bytes());
        wav.extend_from_slice(&(TARGET_SAMPLE_RATE * BYTES_PER_SAMPLE as u32).to_le_bytes());
        wav.extend_from_slice(&(BYTES_PER_SAMPLE as u16).to_le_bytes());
        wav.extend_from_slice(&TARGET_BITS_PER_SAMPLE.to_le_bytes());
        wav.extend_from_slice(b"LIST");
        wav.extend_from_slice(&(list_payload.len() as u32).to_le_bytes());
        wav.extend_from_slice(list_payload);
        if list_payload.len() % 2 == 1 {
            wav.push(0);
        }
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data_size as u32).to_le_bytes());
        wav.resize(wav.len() + data_size, 0);
        let riff_size = (wav.len() - 8) as u32;
        wav[4..8].copy_from_slice(&riff_size.to_le_bytes());

        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&wav).unwrap();
        let duration =
            normalized_wav_duration_from_file(file.reopen().unwrap(), wav.len() as u64).unwrap();
        assert_eq!(duration, Duration::from_secs(1));
    }

    #[test]
    fn filter_stderr_picks_keywords() {
        let s = "Input #0: ... \rProgress: 50%\n[error] no such file: x.mp3\nInvalid data found";
        let out = filter_stderr(s);
        assert!(out.contains("no such file"), "命中 not found 关键词");
        assert!(out.contains("Invalid data"), "命中 invalid 关键词");
        assert!(!out.contains("Progress"), "进度行过滤");
        assert!(!out.contains('\r'), "控制字符去除");
    }

    #[test]
    fn filter_stderr_fallback_tail() {
        // 无关键词命中 → 末尾非空行兜底。
        let s = "line one\nline two\nline three";
        let out = filter_stderr(s);
        assert!(out.contains("line one") || out.contains("line two") || out.contains("line three"));
    }

    #[test]
    fn filter_stderr_empty() {
        assert_eq!(filter_stderr(""), "unknown ffmpeg error");
    }
}
