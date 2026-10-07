use seasnail_proto::seasnail::native::v1::{monitor_event, MonitorEvent, MonitorRequest};
use seasnail_proto::{decode_native_frames, encode_native_frame};
use std::io::Write;
use std::process::{Command, Stdio};

fn main() -> Result<(), String> {
    let arguments = std::env::args().collect::<Vec<_>>();
    let helper = arguments
        .get(1)
        .cloned()
        .ok_or_else(|| "usage: post_paste_monitor_smoke <helper>".to_string())?;
    let mut child = Command::new(helper)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("spawn failed: {error}"))?;
    let live = arguments.len() >= 5;
    let request = MonitorRequest {
        schema_version: 1,
        target_pid: if live {
            arguments[2].parse().map_err(|_| "invalid pid")?
        } else {
            i32::MAX
        },
        pasted_text: if live {
            arguments[3].clone()
        } else {
            "line one\n协议样式文本".into()
        },
        timeout_ms: if live {
            arguments[4].parse().map_err(|_| "invalid timeout")?
        } else {
            1
        },
    };
    child
        .stdin
        .take()
        .ok_or_else(|| "missing stdin".to_string())?
        .write_all(&encode_native_frame(&request)?)
        .map_err(|error| format!("write failed: {error}"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| format!("wait failed: {error}"))?;
    let events = decode_native_frames::<MonitorEvent>(&output.stdout)?;
    let ready = events.iter().any(|event| {
        matches!(&event.payload, Some(monitor_event::Payload::Ready(value)) if value.initial_value.contains(&request.pasted_text))
    });
    let changed = events
        .iter()
        .any(|event| matches!(&event.payload, Some(monitor_event::Payload::Changed(_))));
    let reason = events.iter().find_map(|event| match &event.payload {
        Some(monitor_event::Payload::Finished(finished)) => {
            monitor_event::finished::Reason::try_from(finished.reason).ok()
        }
        _ => None,
    });
    if live {
        if !ready || reason != Some(monitor_event::finished::Reason::Timeout) {
            return Err(format!(
                "live helper did not complete READY -> TIMEOUT (ready={ready}, changed={changed}, reason={reason:?})"
            ));
        }
        if arguments
            .get(5)
            .is_some_and(|value| value == "require-changed")
            && !changed
        {
            return Err("live helper did not emit CHANGED".into());
        }
    } else if reason != Some(monitor_event::finished::Reason::NoElement) {
        return Err("helper did not return framed NO_ELEMENT".into());
    }
    Ok(())
}
