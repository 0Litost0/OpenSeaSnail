//! M3 运行时契约类型（ST-M3.1）。
//!
//! driver→业务层的 OpenAI 形状转写结果；时间秒（float），与 proto `TranscriptFile`
//! （毫秒 int64）不同——daemon 在 API/proto 边界换算（M3.5）。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::str::FromStr;
use thiserror::Error;

/// 运行时种类。字符串值用于资源 manifest 的 `runtime` 字段，而不是模型规格。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeKind {
    Whisper,
    FunAsr,
    /// SenseVoiceSmall + 当前锁定的 GGUF `sensevoice-server` HTTP 契约。
    Gguf,
    /// SenseVoiceSmall + sherpa-onnx 本地 sidecar。
    SherpaOnnx,
}

impl RuntimeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Whisper => "whisper",
            Self::FunAsr => "funasr",
            Self::Gguf => "gguf",
            Self::SherpaOnnx => "sherpa_onnx",
        }
    }
}

impl FromStr for RuntimeKind {
    type Err = UnknownRuntimeKind;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "whisper" => Ok(Self::Whisper),
            "funasr" => Ok(Self::FunAsr),
            "gguf" => Ok(Self::Gguf),
            "sherpa_onnx" => Ok(Self::SherpaOnnx),
            _ => Err(UnknownRuntimeKind(value.to_owned())),
        }
    }
}

/// manifest 给出未知 runtime 时的安全、可理解错误；不得猜测或回退到其他 backend。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unsupported runtime kind {0:?}; expected one of: whisper, funasr, gguf, sherpa_onnx")]
pub struct UnknownRuntimeKind(pub String);

/// 说话人分离能力三态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Diarization {
    /// FunASR cam++ 内联，无额外进程。
    Builtin,
    /// whisper.cpp 委托 sherpa-onnx-diarize 一次性进程（M3.6）。
    External,
    /// 无此后端能力。
    None,
}

/// 运行时能力自述。
#[derive(Debug, Clone)]
pub struct Capabilities {
    pub diarization: Diarization,
    pub streaming: bool,
    pub languages: Vec<String>,
    pub max_audio_seconds: u64,
    /// 是否可产出词/字级时间戳（FunASR 默认 bundle 可，whisper.cpp 暂不可）。
    /// 仅为静态能力信号；运行时仍逐结果校验，不满足时 `words` 置空。
    pub word_timestamps: bool,
}

impl Capabilities {
    /// 当前锁定的 SenseVoice GGUF server 的能力声明。
    ///
    /// WebSocket 是上游 server 能力，但 SeaSnail 本期尚未接入实时协议，故 `streaming`
    /// 为 false；该 backend 也不承诺经过验证的词/字级时间戳。
    pub fn sensevoice_gguf() -> Self {
        Self {
            diarization: Diarization::None,
            streaming: false,
            languages: vec!["zh".into(), "en".into(), "mixed".into()],
            max_audio_seconds: 3600,
            word_timestamps: false,
        }
    }
}

/// 归一后的 OpenAI 形状转写结果（driver→业务层）。时间秒。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiSegments {
    /// 全文（segments 拼接）。
    pub text: String,
    /// 分段（可为空，仅 text）。
    #[serde(default)]
    pub segments: Vec<OpenAiSegment>,
    /// 词/字级时间戳（M4.1，可选）。FunASR 经 sidecar 导出；whisper 暂留空。
    /// 仅在通过 [`OpenAiSegments::validate_words`] 校验后可信，否则应置空（composer 走句段降级）。
    #[serde(default)]
    pub words: Vec<OpenAiWord>,
}

impl OpenAiSegments {
    /// 校验词级时间戳：非空、每词 `start<=end` 且文本非空、单调非降、与 `text` 大致对齐。
    /// 任一不满足则清空 `words`（composer 降级到句段边界，绝不伪造词级插入）。
    pub fn validate_words(&mut self) {
        let words = &self.words;
        let ok = !words.is_empty()
            && words.iter().all(|w| {
                w.start.is_finite() && w.end.is_finite() && w.start <= w.end && !w.text.is_empty()
            })
            && words.windows(2).all(|pair| pair[0].end <= pair[1].start)
            && aligns_with_text(
                &self.text,
                &words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(),
            );
        if !ok {
            self.words.clear();
        }
    }
}

pub(crate) fn has_visible_text(value: &str) -> bool {
    !value.trim().is_empty()
}

pub(crate) fn valid_visible_segment(segment: &OpenAiSegment) -> bool {
    has_visible_text(&segment.text)
        && segment.start.is_finite()
        && segment.end.is_finite()
        && segment.start >= 0.0
        && segment.end >= segment.start
}

/// 词应能重建文本（忽略**全部**标点/空白，含句中逗号）。FunASR punc 模型给 `text` 加标点，
/// 但 `words` 是原始字 token；二者在去标点后应相等。不满足则视为不可对齐 → 清空（走句段降级）。
/// 用相等而非子串/子序列：避免接受与文本不符的 token 序列（会误导 composer 按错误边界重建）。
fn aligns_with_text(text: &str, words: &[&str]) -> bool {
    let clean = |s: &str| -> String { s.chars().filter(|c| c.is_alphanumeric()).collect() };
    let text_clean = clean(text);
    let concat_clean = clean(&words.concat());
    !concat_clean.is_empty() && text_clean == concat_clean
}

/// 单段（OpenAI 形状）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
}

/// 词/字级时间戳项（M4.1）。秒。FunASR 字级、whisper 词级（暂不映射）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpenAiWord {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// 业务层→driver 的请求（POST /sessions → M3.5 编排 → transcribe）。
/// `wav` 假设已是 16kHz mono WAV（M3.3 AudioNormalizer 产出）。
#[derive(Debug, Clone)]
pub struct TranscribeReq {
    pub wav: PathBuf,
    /// 语言提示（"zh"/"en"/...）；None=自动检测。
    pub language: Option<String>,
    /// 可选 prompt（whisper prompt；FunASR 可忽略）。
    pub prompt: Option<String>,
    /// 标点增强特性标志（M1.1）。运行时无关：FunASR 释为 punc 模型懒挂卸，whisper 释
    /// 为 no-op（原生带标点）。None=不启用（默认）。daemon 据 `/models` 配置注入，非客户端发。
    pub punc: Option<bool>,
    /// 说话人识别特性标志（M1.1）。运行时无关：FunASR 释为 spk 懒挂卸，whisper 释为 External
    /// diarization。None=不启用（默认）。SPK 本次后置；daemon 注入。
    pub spk: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(start: f64, end: f64, text: &str) -> OpenAiWord {
        OpenAiWord {
            start,
            end,
            text: text.into(),
        }
    }

    #[test]
    fn validate_words_keeps_monotonic_aligned() {
        // spike 样例：13 字 token 去标点后与 text 相等（仅差句末「。」）。
        let mut segs = OpenAiSegments {
            text: "开饭时间早上九点至下午五点。".into(),
            segments: Vec::new(),
            words: vec![
                word(0.75, 0.81, "开"),
                word(0.93, 0.99, "饭"),
                word(1.50, 1.56, "时"),
                word(1.70, 1.76, "间"),
                word(2.00, 2.06, "早"),
                word(2.20, 2.26, "上"),
                word(2.50, 2.56, "九"),
                word(2.80, 2.86, "点"),
                word(3.10, 3.16, "至"),
                word(3.50, 3.56, "下"),
                word(3.80, 3.86, "午"),
                word(4.20, 4.26, "五"),
                word(4.77, 4.83, "点"),
            ],
        };
        segs.validate_words();
        assert_eq!(segs.words.len(), 13, "单调+对齐（去标点后相等）→ 保留");
    }

    #[test]
    fn validate_words_keeps_multi_clause_with_mid_punctuation() {
        // punc 模型给 text 加句中逗号，但 words 是原始字 → 去标点后仍应相等。
        let mut segs = OpenAiSegments {
            text: "你好，世界。".into(),
            segments: Vec::new(),
            words: vec![
                word(0.0, 0.1, "你"),
                word(0.1, 0.2, "好"),
                word(0.3, 0.4, "世"),
                word(0.4, 0.5, "界"),
            ],
        };
        segs.validate_words();
        assert_eq!(segs.words.len(), 4, "句中逗号不破坏对齐 → 保留");
    }

    #[test]
    fn validate_words_clears_when_misaligned_or_non_monotonic_or_empty() {
        // 拼接不在 text 中 → 清空。
        let mut segs = OpenAiSegments {
            text: "你好世界".into(),
            segments: Vec::new(),
            words: vec![word(0.0, 0.1, "XYZ"), word(0.1, 0.2, "ABC")],
        };
        segs.validate_words();
        assert!(segs.words.is_empty(), "对齐失败 → 清空");

        // 非单调（end > next start）→ 清空。
        let mut segs = OpenAiSegments {
            text: "ab".into(),
            segments: Vec::new(),
            words: vec![word(0.2, 0.5, "a"), word(0.3, 0.4, "b")],
        };
        segs.validate_words();
        assert!(segs.words.is_empty(), "非单调 → 清空");

        // start>end → 清空。
        let mut segs = OpenAiSegments {
            text: "a".into(),
            segments: Vec::new(),
            words: vec![word(0.5, 0.1, "a")],
        };
        segs.validate_words();
        assert!(segs.words.is_empty(), "start>end → 清空");

        // NaN/±Inf 不能成为可信时间边界，即使比较关系表面成立也必须清空。
        for (start, end) in [
            (f64::NAN, 0.1),
            (0.0, f64::INFINITY),
            (f64::NEG_INFINITY, 0.1),
        ] {
            let mut segs = OpenAiSegments {
                text: "a".into(),
                segments: Vec::new(),
                words: vec![word(start, end, "a")],
            };
            segs.validate_words();
            assert!(segs.words.is_empty(), "非有限时间 → 清空");
        }

        // 空 → 保持空（no-op，composer 走句段降级）。
        let mut segs = OpenAiSegments {
            text: "a".into(),
            segments: Vec::new(),
            words: Vec::new(),
        };
        segs.validate_words();
        assert!(segs.words.is_empty());
    }

    #[test]
    fn runtime_kind_parses_only_supported_manifest_values() {
        for (text, expected) in [
            ("whisper", RuntimeKind::Whisper),
            ("funasr", RuntimeKind::FunAsr),
            ("gguf", RuntimeKind::Gguf),
            ("sherpa_onnx", RuntimeKind::SherpaOnnx),
        ] {
            assert_eq!(text.parse::<RuntimeKind>(), Ok(expected));
            assert_eq!(expected.as_str(), text);
        }
        assert_eq!(
            "onnx".parse::<RuntimeKind>().unwrap_err().to_string(),
            "unsupported runtime kind \"onnx\"; expected one of: whisper, funasr, gguf, sherpa_onnx"
        );
    }

    #[test]
    fn sensevoice_gguf_capabilities_do_not_overstate_support() {
        let capabilities = Capabilities::sensevoice_gguf();
        assert_eq!(capabilities.diarization, Diarization::None);
        assert_eq!(capabilities.max_audio_seconds, 3600);
        assert_eq!(capabilities.languages, ["zh", "en", "mixed"]);
        assert!(!capabilities.streaming);
        assert!(!capabilities.word_timestamps);
    }
}
