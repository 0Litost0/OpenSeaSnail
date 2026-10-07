//! 剪贴板事件与录音源帧时间轴的换算。
//!
//! Pasteboard 只能告知“被发现变化”的时刻，不能证明用户实际按下复制键的瞬间。
//! 因此锚点固定记录该检测瞬间读到的源帧偏移，并在录音停止时钳制到最终音频帧数。

/// collector 的目标轮询周期；调度延迟是额外的运行时误差，不能被承诺为硬上界。
pub const PASTEBOARD_POLL_INTERVAL_MS: u64 = 50;

/// 将源帧偏移换算为秒。调用方必须持久化原始帧数与采样率，不可只保存此派生值。
#[cfg(test)]
pub fn source_frame_offset_seconds(sample_offset: u64, source_sample_rate: u32) -> Option<f64> {
    (source_sample_rate != 0).then(|| sample_offset as f64 / source_sample_rate as f64)
}

/// 停止线性化后的安全锚点：collector 在停止竞争中读到未来帧数时，只能落在末帧。
pub fn clamp_sample_offset(sample_offset: u64, final_frame_count: u64) -> u64 {
    sample_offset.min(final_frame_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_detected_change_to_the_source_audio_clock() {
        // 48 kHz 源音频：变化实际发生在 1.000s，下一次 50ms 轮询在 1.050s 观测到。
        // 产品锚定的是 1.050s，不伪称复制操作的墙钟时刻。
        let actual_frame = 48_000;
        let observed_frame = 50_400;
        assert_eq!(
            source_frame_offset_seconds(observed_frame, 48_000),
            Some(1.05)
        );
        assert_eq!(
            (observed_frame - actual_frame) * 1_000 / 48_000,
            PASTEBOARD_POLL_INTERVAL_MS
        );
    }

    #[test]
    fn clamps_a_stop_race_to_the_last_audio_frame() {
        assert_eq!(clamp_sample_offset(48_100, 48_000), 48_000);
        assert_eq!(clamp_sample_offset(47_900, 48_000), 47_900);
        assert_eq!(source_frame_offset_seconds(1, 0), None);
    }
}
