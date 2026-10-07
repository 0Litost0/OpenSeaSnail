//! SeaSnail 转译 proto（prost 生成，ST-M2.7）。
//!
//! 由 `build.rs` 编 `proto/seasnail/v1/{transcript,cleanup}.proto` 生成 `seasnail.v1`
//! 模块。生成代码落 `OUT_DIR`，经 `include!` 挂入本 crate 的 `seasnail::v1`。
//!
//! proto 时间字段一律 int64 毫秒、带 `_ms` 后缀（与 DB 的 epoch 秒、OpenAPI wire
//! 秒不同——daemon 在 API/proto 边界换算）。序列化后经 AEAD(chacha20-poly1305,
//! key=K_files) 加密落盘为 `transcript.pb.enc`。

/// prost 生成的 `seasnail.v1` 模块。
pub mod seasnail {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/seasnail.v1.rs"));
    }
    pub mod native {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/seasnail.native.v1.rs"));
        }
    }
}

/// Length-delimited protobuf framing for the native helper (4-byte big-endian length).
pub const MAX_NATIVE_FRAME_BYTES: usize = 1024 * 1024;

pub fn encode_native_frame<M: prost::Message>(message: &M) -> Result<Vec<u8>, String> {
    let body = message.encode_to_vec();
    if body.len() > MAX_NATIVE_FRAME_BYTES {
        return Err("native frame exceeds 1 MiB".into());
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

pub fn decode_native_frame<M: prost::Message + Default>(frame: &[u8]) -> Result<M, String> {
    if frame.len() < 4 {
        return Err("truncated frame header".into());
    }
    let len = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
    if len > MAX_NATIVE_FRAME_BYTES {
        return Err("native frame exceeds 1 MiB".into());
    }
    if frame.len() != len + 4 {
        return Err("invalid frame length".into());
    }
    M::decode(&frame[4..]).map_err(|_| "invalid protobuf payload".into())
}

pub fn decode_native_frames<M: prost::Message + Default>(
    mut bytes: &[u8],
) -> Result<Vec<M>, String> {
    let mut messages = Vec::new();
    while !bytes.is_empty() {
        if bytes.len() < 4 {
            return Err("truncated frame header".into());
        }
        let len = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
        if len > MAX_NATIVE_FRAME_BYTES {
            return Err("native frame exceeds 1 MiB".into());
        }
        let end = 4_usize
            .checked_add(len)
            .ok_or_else(|| "invalid frame length".to_string())?;
        if bytes.len() < end {
            return Err("truncated frame payload".into());
        }
        messages
            .push(M::decode(&bytes[4..end]).map_err(|_| "invalid protobuf payload".to_string())?);
        bytes = &bytes[end..];
    }
    Ok(messages)
}

pub fn validate_monitor_request(
    request: &seasnail::native::v1::MonitorRequest,
) -> Result<(), String> {
    if request.schema_version != 1 {
        return Err("unsupported monitor schema version".into());
    }
    if request.target_pid <= 0 || request.pasted_text.is_empty() {
        return Err("invalid monitor request".into());
    }
    if request.timeout_ms == 0 || request.timeout_ms > 30_000 {
        return Err("invalid monitor timeout".into());
    }
    Ok(())
}

#[cfg(test)]
mod native_frame_tests {
    use super::*;
    use crate::seasnail::native::v1::{monitor_event, MonitorEvent, MonitorRequest};

    #[test]
    fn native_frame_round_trip_and_rejects_truncation() {
        let request = MonitorRequest {
            schema_version: 1,
            target_pid: 42,
            pasted_text: "a\n样式".into(),
            timeout_ms: 30_000,
        };
        let frame = encode_native_frame(&request).unwrap();
        let decoded: MonitorRequest = decode_native_frame(&frame).unwrap();
        assert_eq!(decoded, request);
        assert!(decode_native_frame::<MonitorRequest>(&frame[..3]).is_err());
        let mut extra = frame.clone();
        extra.push(0);
        assert!(decode_native_frame::<MonitorRequest>(&extra).is_err());
        assert!(validate_monitor_request(&decoded).is_ok());
        let mut unsupported = decoded;
        unsupported.schema_version = 2;
        assert!(validate_monitor_request(&unsupported).is_err());
    }

    #[test]
    fn native_stream_decodes_consecutive_frames_and_enforces_limit() {
        let event = MonitorEvent {
            schema_version: 1,
            payload: Some(monitor_event::Payload::Ready(monitor_event::Ready {
                initial_value: "text\n样式".into(),
            })),
        };
        let first = encode_native_frame(&event).unwrap();
        let mut stream = first.clone();
        stream.extend_from_slice(&first);
        assert_eq!(
            decode_native_frames::<MonitorEvent>(&stream).unwrap(),
            vec![event.clone(), event]
        );
        assert!(decode_native_frames::<MonitorEvent>(&stream[..stream.len() - 1]).is_err());
        let oversized = ((MAX_NATIVE_FRAME_BYTES as u32) + 1).to_be_bytes();
        assert!(decode_native_frames::<MonitorEvent>(&oversized).is_err());
    }
}

/// 校验 schema v2 转写文件的跨字段语义不变量。
pub fn validate_transcript_file(transcript: &seasnail::v1::TranscriptFile) -> Result<(), String> {
    use seasnail::v1::UnitGranularity;
    if transcript.schema_version != 2 {
        return Err(format!(
            "unsupported transcript schema version {}",
            transcript.schema_version
        ));
    }
    if transcript.session_id.is_empty() || transcript.account_id.is_empty() {
        return Err("session_id and account_id are required".into());
    }
    if transcript.duration_ms < 0 {
        return Err("duration_ms must be non-negative".into());
    }
    let speaker_ids: std::collections::HashSet<_> =
        transcript.speakers.iter().map(|s| s.id.as_str()).collect();
    if speaker_ids.len() != transcript.speakers.len() || speaker_ids.iter().any(|id| id.is_empty())
    {
        return Err("speaker ids must be non-empty and unique".into());
    }
    let mut joined = String::new();
    let mut expected_granularity = None;
    let mut previous_start_ms = None;
    for (index, unit) in transcript.units.iter().enumerate() {
        if unit.sequence as usize != index {
            return Err("unit sequence must be contiguous from zero".into());
        }
        if unit.text.is_empty() {
            return Err("unit text must not be empty".into());
        }
        let granularity = UnitGranularity::try_from(unit.granularity)
            .map_err(|_| "unit granularity is invalid".to_owned())?;
        if granularity == UnitGranularity::Unspecified {
            return Err("unit granularity must be specified".into());
        }
        if expected_granularity.get_or_insert(granularity) != &granularity {
            return Err("all units must use one granularity".into());
        }
        match granularity {
            UnitGranularity::Untimed => {
                if unit.start_ms.is_some() || unit.end_ms.is_some() {
                    return Err("untimed unit must not have timestamps".into());
                }
            }
            UnitGranularity::TimedText | UnitGranularity::Segment => {
                let (Some(start), Some(end)) = (unit.start_ms, unit.end_ms) else {
                    return Err("timed unit must have both timestamps".into());
                };
                if start < 0 || end < start || end > transcript.duration_ms {
                    return Err("unit timestamp is outside audio duration".into());
                }
                if previous_start_ms.is_some_and(|previous| start < previous) {
                    return Err("unit timestamps must be ordered by start time".into());
                }
                previous_start_ms = Some(start);
            }
            UnitGranularity::Unspecified => unreachable!(),
        }
        if !unit.speaker.is_empty() && !speaker_ids.contains(unit.speaker.as_str()) {
            return Err("unit speaker must reference speakers".into());
        }
        joined.push_str(&unit.text);
    }
    if joined != transcript.full_text {
        return Err("full_text must equal the exact concatenation of unit text".into());
    }
    if transcript.full_text.is_empty() && !transcript.units.is_empty() {
        return Err("empty full_text cannot have units".into());
    }
    if !transcript.full_text.is_empty() && transcript.units.is_empty() {
        return Err("non-empty full_text requires units".into());
    }
    Ok(())
}

/// 校验 cleanup schema v1 的自包含 wire 语义。
///
/// 依赖 raw transcript 的 substring/correction 业务校验由 cleanup service 执行；这里
/// 只接受可安全进入 artifact 读取链路的枚举、字段边界和 outcome 组合。
pub fn validate_cleanup_file(cleanup: &seasnail::v1::CleanupFile) -> Result<(), String> {
    use seasnail::v1::{CleanupOutcome, CorrectionKind, PlaceholderValidationStatus};
    use std::collections::HashSet;

    if cleanup.schema_version != 1 {
        return Err(format!(
            "unsupported cleanup schema version {}",
            cleanup.schema_version
        ));
    }
    if cleanup.session_id.is_empty() || cleanup.account_id.is_empty() {
        return Err("session_id and account_id are required".into());
    }
    if cleanup.prompt_sha256.len() != 32 {
        return Err("prompt_sha256 must be exactly 32 bytes".into());
    }
    if cleanup.cleaned_text.len() > 128 * 1024 {
        return Err("cleaned_text exceeds 128 KiB".into());
    }
    if cleanup.corrections.len() > 32 {
        return Err("corrections exceeds 32 entries".into());
    }
    if cleanup.cleaned_text.contains("[[SEASNAIL_CTX_") {
        return Err("cleaned_text must not contain context markers".into());
    }
    if cleanup.model.chars().count() > 256 || cleanup.model.chars().any(char::is_control) {
        return Err("model exceeds bounds or contains control characters".into());
    }

    let mut correction_keys = HashSet::new();
    for correction in &cleanup.corrections {
        let kind = CorrectionKind::try_from(correction.kind)
            .map_err(|_| "correction kind is invalid".to_owned())?;
        if kind == CorrectionKind::Unspecified {
            return Err("correction kind must be specified".into());
        }
        let original = correction.original_text.trim();
        let corrected = correction.corrected_text.trim();
        if original.is_empty() || corrected.is_empty() || original == corrected {
            return Err("correction text must be non-empty and different".into());
        }
        if original != correction.original_text || corrected != correction.corrected_text {
            return Err("correction text must already be trimmed".into());
        }
        if original.chars().count() > 256 || corrected.chars().count() > 256 {
            return Err("correction text exceeds 256 Unicode scalars".into());
        }
        if original.chars().any(char::is_control)
            || corrected.chars().any(char::is_control)
            || original.contains("[[SEASNAIL_CTX_")
            || corrected.contains("[[SEASNAIL_CTX_")
        {
            return Err("correction text contains forbidden content".into());
        }
        if !correction_keys.insert((original, corrected, kind as i32)) {
            return Err("corrections must be deduplicated".into());
        }
    }

    let outcome = CleanupOutcome::try_from(cleanup.outcome)
        .map_err(|_| "cleanup outcome is invalid".to_owned())?;
    let placeholder = PlaceholderValidationStatus::try_from(cleanup.placeholder_validation)
        .map_err(|_| "placeholder validation status is invalid".to_owned())?;
    if outcome == CleanupOutcome::Unspecified {
        return Err("cleanup outcome must be specified".into());
    }
    if placeholder == PlaceholderValidationStatus::Unspecified {
        return Err("placeholder validation status must be specified".into());
    }

    match outcome {
        CleanupOutcome::Succeeded => {
            if cleanup.cleaned_text.trim().is_empty()
                || !cleanup.error_code.is_empty()
                || cleanup.provider_config_id.is_empty()
                || cleanup.model.is_empty()
                || placeholder != PlaceholderValidationStatus::PlaceholderValidationPassed
            {
                return Err("invalid succeeded cleanup fields".into());
            }
        }
        CleanupOutcome::Failed => {
            if !cleanup.cleaned_text.is_empty()
                || !cleanup.corrections.is_empty()
                || cleanup.error_code.is_empty()
                || !matches!(
                    placeholder,
                    PlaceholderValidationStatus::PlaceholderValidationNotRun
                        | PlaceholderValidationStatus::PlaceholderValidationFailed
                )
            {
                return Err("invalid failed cleanup fields".into());
            }
            if cleanup.provider_config_id.is_empty() != cleanup.model.is_empty() {
                return Err("provider_config_id and model must be present together".into());
            }
            if !is_stable_cleanup_error_code(&cleanup.error_code) {
                return Err("cleanup error_code is not a stable code".into());
            }
            if cleanup.provider_config_id.is_empty()
                && cleanup.error_code != "cleanup_not_configured"
            {
                return Err(
                    "provider_config_id and model are required after snapshot creation".into(),
                );
            }
        }
        CleanupOutcome::Unspecified => unreachable!(),
    }
    // diagnostics 是可选、可独立移除的调试扩展；其版本或字段异常不能令已经通过
    // 校验的 cleaned text 失效。需要展示诊断的调用方单独调用下方校验函数。
    Ok(())
}

/// 单独校验可选的 context 展示位置扩展。
///
/// 写入时拒绝无效位置；读取时不应让该扩展破坏核心 cleanup 正文，展示层会在无效时
/// 降级为 separate 布局。
pub fn validate_cleanup_context_placements(
    cleanup: &seasnail::v1::CleanupFile,
) -> Result<(), String> {
    use seasnail::v1::CleanupOutcome;
    use std::collections::HashSet;

    if cleanup.context_placements.len() > 100 {
        return Err("context_placements exceeds 100 entries".into());
    }
    if CleanupOutcome::try_from(cleanup.outcome).ok() == Some(CleanupOutcome::Failed)
        && !cleanup.context_placements.is_empty()
    {
        return Err("failed cleanup must not contain context placements".into());
    }

    let mut placement_sequences = HashSet::new();
    let mut previous_offset = 0_u64;
    for placement in &cleanup.context_placements {
        let Ok(offset) = usize::try_from(placement.byte_offset) else {
            return Err("context placement offset exceeds platform bounds".into());
        };
        if offset > cleanup.cleaned_text.len() || !cleanup.cleaned_text.is_char_boundary(offset) {
            return Err("context placement offset is not a cleaned_text UTF-8 boundary".into());
        }
        if placement.byte_offset < previous_offset {
            return Err("context placement offsets must be nondecreasing".into());
        }
        if !placement_sequences.insert(placement.event_sequence) {
            return Err("context placement event_sequence must be unique".into());
        }
        previous_offset = placement.byte_offset;
    }
    Ok(())
}

/// 校验可选 cleanup diagnostics。该函数与核心 artifact 校验刻意解耦。
pub fn validate_cleanup_diagnostics(
    diagnostics: &seasnail::v1::CleanupDiagnostics,
) -> Result<(), String> {
    use seasnail::v1::DiagnosticCaptureStatus;
    const MAX_JAVASCRIPT_DATE_MS: i64 = 8_640_000_000_000_000;

    if diagnostics.schema_version != 1 {
        return Err(format!(
            "unsupported cleanup diagnostics schema version {}",
            diagnostics.schema_version
        ));
    }
    if diagnostics
        .local_transcription_elapsed_ms
        .is_some_and(|elapsed_ms| elapsed_ms > 24 * 60 * 60 * 1000)
    {
        return Err("diagnostics local transcription duration is invalid".into());
    }
    if uuid::Uuid::parse_str(&diagnostics.trace_id).is_err()
        || !(1..=MAX_JAVASCRIPT_DATE_MS).contains(&diagnostics.request_started_at_ms)
        || diagnostics.response_started_at_ms > MAX_JAVASCRIPT_DATE_MS
        || diagnostics.response_completed_at_ms > MAX_JAVASCRIPT_DATE_MS
    {
        return Err("diagnostics trace_id or request timestamp is invalid".into());
    }
    if diagnostics.response_content_type.chars().count() > 256
        || diagnostics.provider_request_id.chars().count() > 256
        || diagnostics
            .response_content_type
            .chars()
            .chain(diagnostics.provider_request_id.chars())
            .any(char::is_control)
    {
        return Err("diagnostics response metadata exceeds bounds".into());
    }
    if diagnostics
        .http_status
        .is_some_and(|status| !(100..=599).contains(&status))
    {
        return Err("diagnostics HTTP status is invalid".into());
    }
    let capture = DiagnosticCaptureStatus::try_from(diagnostics.capture_status)
        .map_err(|_| "diagnostics capture status is invalid".to_owned())?;
    if capture == DiagnosticCaptureStatus::Unspecified {
        return Err("diagnostics capture status must be specified".into());
    }
    let response_timestamps_valid = diagnostics.response_started_at_ms
        >= diagnostics.request_started_at_ms
        && diagnostics.response_completed_at_ms >= diagnostics.response_started_at_ms;
    match capture {
        DiagnosticCaptureStatus::NotReceived => {
            if diagnostics.response_started_at_ms != 0
                || diagnostics.response_completed_at_ms != 0
                || diagnostics.http_status.is_some()
                || !diagnostics.raw_response_body.is_empty()
                || !diagnostics.response_sha256.is_empty()
                || diagnostics.response_body_bytes != 0
                || !diagnostics.response_content_type.is_empty()
                || !diagnostics.provider_request_id.is_empty()
            {
                return Err("not-received diagnostics contains response data".into());
            }
        }
        DiagnosticCaptureStatus::Complete => {
            use sha2::{Digest, Sha256};
            if !response_timestamps_valid
                || diagnostics.http_status.is_none()
                || diagnostics.raw_response_body.len() > 256 * 1024
                || diagnostics.response_body_bytes != diagnostics.raw_response_body.len() as u64
                || diagnostics.response_sha256.len() != 32
                || diagnostics.response_sha256
                    != Sha256::digest(&diagnostics.raw_response_body).to_vec()
            {
                return Err("complete diagnostics fields are inconsistent".into());
            }
        }
        DiagnosticCaptureStatus::TooLarge | DiagnosticCaptureStatus::ReadFailed => {
            if !response_timestamps_valid
                || diagnostics.http_status.is_none()
                || !diagnostics.raw_response_body.is_empty()
                || !diagnostics.response_sha256.is_empty()
            {
                return Err("incomplete diagnostics fields are inconsistent".into());
            }
        }
        DiagnosticCaptureStatus::Redacted => {
            if !response_timestamps_valid
                || diagnostics.http_status.is_none()
                || !diagnostics.raw_response_body.is_empty()
                || diagnostics.response_sha256.len() != 32
                || diagnostics.response_body_bytes > 256 * 1024
            {
                return Err("redacted diagnostics fields are inconsistent".into());
            }
        }
        DiagnosticCaptureStatus::Unspecified => unreachable!(),
    }
    Ok(())
}

fn is_stable_cleanup_error_code(code: &str) -> bool {
    matches!(
        code,
        "cleanup_not_configured"
            | "cleanup_endpoint_rejected"
            | "cleanup_credential_missing"
            | "cleanup_input_too_large"
            | "cleanup_timeout"
            | "cleanup_transport_error"
            | "cleanup_http_auth"
            | "cleanup_http_rate_limit"
            | "cleanup_http_server"
            | "cleanup_response_too_large"
            | "cleanup_response_invalid_json"
            | "cleanup_cleaned_text_invalid"
            | "cleanup_placeholder_invalid"
            | "cleanup_artifact_write_failed"
            | "cleanup_artifact_invalid"
            | "cleanup_interrupted"
            | "cleanup_busy"
    )
}

#[cfg(test)]
mod tests {
    use super::seasnail::v1::{
        CleanupContextPlacement, CleanupDiagnostics, CleanupFile, CleanupOutcome,
        ClipboardContextFile, ContextEvent, ContextEventKind, Correction, CorrectionKind,
        DiagnosticCaptureStatus, PlaceholderValidationStatus, Source, Speaker, TranscriptFile,
        TranscriptUnit, UnitGranularity,
    };
    use super::{
        validate_cleanup_context_placements, validate_cleanup_diagnostics, validate_cleanup_file,
        validate_transcript_file,
    };
    use prost::Message;

    /// TranscriptFile 编解码往返：字段一致。
    #[test]
    fn transcript_file_roundtrip() {
        let t = TranscriptFile {
            schema_version: 2,
            session_id: "s1".into(),
            account_id: "a1".into(),
            created_at_ms: 1_700_000_000_000,
            model: "whisper".into(),
            language: "zh".into(),
            source: Source::Realtime as i32,
            duration_ms: 5000,
            speakers: vec![Speaker {
                id: "A".into(),
                label: "说话人 A".into(),
            }],
            units: vec![TranscriptUnit {
                sequence: 0,
                start_ms: Some(0),
                end_ms: Some(1000),
                text: "你好".into(),
                speaker: "A".into(),
                confidence: Some(0.9),
                granularity: UnitGranularity::TimedText as i32,
            }],
            full_text: "你好".into(),
        };
        let bytes = t.encode_to_vec();
        let dec = TranscriptFile::decode(&*bytes).unwrap();
        assert_eq!(dec.session_id, "s1");
        assert_eq!(dec.full_text, "你好");
        assert_eq!(dec.units.len(), 1);
        assert_eq!(dec.units[0].text, "你好");
        assert_eq!(dec.source, Source::Realtime as i32);
        assert_eq!(dec.created_at_ms, 1_700_000_000_000);
        validate_transcript_file(&dec).unwrap();
    }

    #[test]
    fn transcript_semantic_validation_rejects_inconsistent_units() {
        let mut t = TranscriptFile {
            schema_version: 2,
            session_id: "s".into(),
            account_id: "a".into(),
            duration_ms: 1000,
            full_text: "ab".into(),
            units: vec![TranscriptUnit {
                sequence: 0,
                start_ms: Some(0),
                end_ms: Some(1000),
                text: "a".into(),
                speaker: String::new(),
                confidence: None,
                granularity: UnitGranularity::TimedText as i32,
            }],
            ..Default::default()
        };
        assert!(validate_transcript_file(&t).is_err());
        t.full_text = "a".into();
        assert!(validate_transcript_file(&t).is_ok());
        t.units[0].start_ms = None;
        assert!(validate_transcript_file(&t).is_err());
    }

    #[test]
    fn clipboard_context_round_trips_without_changing_transcript_wire_format() {
        let context = ClipboardContextFile {
            schema_version: 1,
            session_id: "session-1".into(),
            capture_id: "capture-1".into(),
            events: vec![ContextEvent {
                sequence: 1,
                source_sample_rate: 48_000,
                sample_offset: 50_400,
                kind: ContextEventKind::ContextEventPlainText as i32,
                plain_text: "示例".into(),
                html_fragment: String::new(),
                absolute_paths: Vec::new(),
            }],
        };
        let encoded = context.encode_to_vec();
        assert_eq!(
            ClipboardContextFile::decode(&*encoded)
                .unwrap()
                .events
                .len(),
            1
        );

        let transcript = TranscriptFile::default();
        assert!(TranscriptFile::decode(&*transcript.encode_to_vec()).is_ok());
    }

    fn succeeded_cleanup() -> CleanupFile {
        CleanupFile {
            schema_version: 1,
            session_id: "session-1".into(),
            account_id: "account-1".into(),
            outcome: CleanupOutcome::Succeeded as i32,
            cleaned_text: "SeaSnail 已完成。".into(),
            corrections: vec![Correction {
                original_text: "Sea Snail".into(),
                corrected_text: "SeaSnail".into(),
                kind: CorrectionKind::ProperNoun as i32,
            }],
            provider_config_id: "00000000-0000-4000-8000-000000000001".into(),
            model: "example-model".into(),
            prompt_sha256: vec![0xa5; 32],
            error_code: String::new(),
            elapsed_ms: 321,
            placeholder_validation: PlaceholderValidationStatus::PlaceholderValidationPassed as i32,
            context_placements: vec![CleanupContextPlacement {
                event_sequence: 1,
                byte_offset: 8,
            }],
            diagnostics: None,
        }
    }

    #[test]
    fn cleanup_success_roundtrip_and_validation() {
        let cleanup = succeeded_cleanup();
        let decoded = CleanupFile::decode(cleanup.encode_to_vec().as_slice()).unwrap();
        assert_eq!(decoded, cleanup);
        validate_cleanup_file(&decoded).unwrap();
    }

    #[test]
    fn cleanup_context_placements_require_utf8_boundaries_order_and_unique_sequences() {
        let mut cleanup = succeeded_cleanup();
        cleanup.cleaned_text = "你a好".into();
        cleanup.context_placements = vec![
            CleanupContextPlacement {
                event_sequence: 1,
                byte_offset: 3,
            },
            CleanupContextPlacement {
                event_sequence: 2,
                byte_offset: 4,
            },
        ];
        validate_cleanup_context_placements(&cleanup).unwrap();

        cleanup.context_placements[0].byte_offset = 1;
        assert!(validate_cleanup_context_placements(&cleanup).is_err());
        // 可选位置扩展损坏不能连带使核心 cleanup 正文失效。
        assert!(validate_cleanup_file(&cleanup).is_ok());
        cleanup.context_placements[0].byte_offset = 4;
        cleanup.context_placements[1].byte_offset = 3;
        assert!(validate_cleanup_context_placements(&cleanup).is_err());
        cleanup.context_placements[0].byte_offset = 3;
        cleanup.context_placements[1].byte_offset = 4;
        cleanup.context_placements[1].event_sequence = 1;
        assert!(validate_cleanup_context_placements(&cleanup).is_err());
    }

    #[test]
    fn diagnostics_are_independently_validated_and_do_not_invalidate_cleanup() {
        use sha2::{Digest, Sha256};

        let body = br#"{"choices":[]}"#.to_vec();
        let diagnostics = CleanupDiagnostics {
            schema_version: 1,
            trace_id: "00000000-0000-4000-8000-000000000001".into(),
            request_started_at_ms: 100,
            response_started_at_ms: 110,
            response_completed_at_ms: 120,
            http_status: Some(200),
            response_content_type: "application/json".into(),
            provider_request_id: "request-1".into(),
            raw_response_body: body.clone(),
            response_sha256: Sha256::digest(&body).to_vec(),
            response_body_bytes: body.len() as u64,
            capture_status: DiagnosticCaptureStatus::Complete as i32,
            local_transcription_elapsed_ms: Some(50),
        };
        validate_cleanup_diagnostics(&diagnostics).unwrap();

        let mut cleanup = succeeded_cleanup();
        cleanup.diagnostics = Some(diagnostics);
        validate_cleanup_file(&cleanup).unwrap();
        cleanup.diagnostics.as_mut().unwrap().schema_version = 99;
        assert!(validate_cleanup_diagnostics(cleanup.diagnostics.as_ref().unwrap()).is_err());
        validate_cleanup_file(&cleanup).unwrap();

        let diagnostics = cleanup.diagnostics.as_mut().unwrap();
        diagnostics.schema_version = 1;
        diagnostics.request_started_at_ms = i64::MAX;
        diagnostics.response_started_at_ms = i64::MAX;
        diagnostics.response_completed_at_ms = i64::MAX;
        assert!(validate_cleanup_diagnostics(diagnostics).is_err());
        validate_cleanup_file(&cleanup).unwrap();
    }

    #[test]
    fn redacted_diagnostics_keep_only_hash_and_size() {
        let diagnostics = CleanupDiagnostics {
            schema_version: 1,
            trace_id: "00000000-0000-4000-8000-000000000001".into(),
            request_started_at_ms: 100,
            response_started_at_ms: 110,
            response_completed_at_ms: 120,
            http_status: Some(200),
            response_sha256: vec![7; 32],
            response_body_bytes: 128,
            capture_status: DiagnosticCaptureStatus::Redacted as i32,
            ..Default::default()
        };
        validate_cleanup_diagnostics(&diagnostics).unwrap();

        let mut invalid = diagnostics;
        invalid.raw_response_body = b"secret".to_vec();
        assert!(validate_cleanup_diagnostics(&invalid).is_err());
    }

    #[test]
    fn cleanup_failure_roundtrip_and_validation() {
        let cleanup = CleanupFile {
            schema_version: 1,
            session_id: "s".into(),
            account_id: "a".into(),
            outcome: CleanupOutcome::Failed as i32,
            provider_config_id: "00000000-0000-4000-8000-000000000001".into(),
            model: "example-model".into(),
            prompt_sha256: vec![0; 32],
            error_code: "cleanup_timeout".into(),
            elapsed_ms: 15_000,
            placeholder_validation: PlaceholderValidationStatus::PlaceholderValidationNotRun as i32,
            ..Default::default()
        };
        let decoded = CleanupFile::decode(cleanup.encode_to_vec().as_slice()).unwrap();
        assert_eq!(decoded, cleanup);
        validate_cleanup_file(&decoded).unwrap();

        let mut placeholder_failed = cleanup;
        placeholder_failed.placeholder_validation =
            PlaceholderValidationStatus::PlaceholderValidationFailed as i32;
        validate_cleanup_file(&placeholder_failed).unwrap();
    }

    #[test]
    fn cleanup_failure_wire_golden_is_stable() {
        let cleanup = CleanupFile {
            schema_version: 1,
            session_id: "s".into(),
            account_id: "a".into(),
            outcome: CleanupOutcome::Failed as i32,
            provider_config_id: "p".into(),
            model: "m".into(),
            prompt_sha256: vec![0; 32],
            error_code: "cleanup_timeout".into(),
            elapsed_ms: 7,
            placeholder_validation: PlaceholderValidationStatus::PlaceholderValidationNotRun as i32,
            ..Default::default()
        };
        let mut expected = vec![0x08, 0x01, 0x12, 0x01, b's', 0x1a, 0x01, b'a', 0x20, 0x02];
        expected.extend_from_slice(&[0x3a, 0x01, b'p', 0x42, 0x01, b'm', 0x4a, 0x20]);
        expected.extend_from_slice(&[0; 32]);
        expected.extend_from_slice(&[0x52, 0x0f]);
        expected.extend_from_slice(b"cleanup_timeout");
        expected.extend_from_slice(&[0x58, 0x07, 0x60, 0x01]);
        assert_eq!(cleanup.encode_to_vec(), expected);
        validate_cleanup_file(&CleanupFile::decode(expected.as_slice()).unwrap()).unwrap();
    }

    #[test]
    fn cleanup_validation_rejects_unknown_and_unspecified_enums() {
        let mut cleanup = succeeded_cleanup();
        cleanup.outcome = 99;
        assert!(validate_cleanup_file(&cleanup).is_err());
        cleanup.outcome = CleanupOutcome::Unspecified as i32;
        assert!(validate_cleanup_file(&cleanup).is_err());

        cleanup = succeeded_cleanup();
        cleanup.placeholder_validation = 99;
        assert!(validate_cleanup_file(&cleanup).is_err());
        cleanup.placeholder_validation = PlaceholderValidationStatus::Unspecified as i32;
        assert!(validate_cleanup_file(&cleanup).is_err());

        cleanup = succeeded_cleanup();
        cleanup.corrections[0].kind = 99;
        assert!(validate_cleanup_file(&cleanup).is_err());
        cleanup.corrections[0].kind = CorrectionKind::Unspecified as i32;
        assert!(validate_cleanup_file(&cleanup).is_err());
    }

    #[test]
    fn cleanup_validation_rejects_noncanonical_duplicate_corrections() {
        let mut cleanup = succeeded_cleanup();
        cleanup.corrections[0].original_text = " Sea Snail".into();
        assert!(validate_cleanup_file(&cleanup).is_err());

        cleanup = succeeded_cleanup();
        cleanup.corrections.push(cleanup.corrections[0].clone());
        assert!(validate_cleanup_file(&cleanup).is_err());
    }

    #[test]
    fn cleanup_validation_rejects_non_stable_failure_error_code() {
        let mut cleanup = CleanupFile {
            schema_version: 1,
            session_id: "s".into(),
            account_id: "a".into(),
            outcome: CleanupOutcome::Failed as i32,
            prompt_sha256: vec![0; 32],
            error_code: "upstream said secret=example".into(),
            placeholder_validation: PlaceholderValidationStatus::PlaceholderValidationNotRun as i32,
            ..Default::default()
        };
        assert!(validate_cleanup_file(&cleanup).is_err());
        cleanup.error_code = "cleanup_transport_error".into();
        cleanup.provider_config_id = "provider".into();
        cleanup.model = "model".into();
        validate_cleanup_file(&cleanup).unwrap();
    }

    #[test]
    fn cleanup_failure_allows_empty_provider_only_before_snapshot() {
        let mut cleanup = CleanupFile {
            schema_version: 1,
            session_id: "s".into(),
            account_id: "a".into(),
            outcome: CleanupOutcome::Failed as i32,
            prompt_sha256: vec![0; 32],
            error_code: "cleanup_not_configured".into(),
            placeholder_validation: PlaceholderValidationStatus::PlaceholderValidationNotRun as i32,
            ..Default::default()
        };
        validate_cleanup_file(&cleanup).unwrap();

        cleanup.error_code = "cleanup_timeout".into();
        assert!(validate_cleanup_file(&cleanup).is_err());
        cleanup.provider_config_id = "provider".into();
        cleanup.model = "model".into();
        validate_cleanup_file(&cleanup).unwrap();
    }
}
