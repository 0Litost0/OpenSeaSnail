//! Post-paste observation orchestration owned by the trusted native host.
//!
//! 诊断日志（gui.log，tag `observation`）只写固定结果码、计数与 PID；词条、粘贴文本、
//! 目标输入框值、helper stderr 原文与 learning ticket 一律不记录（设计「日志禁项」）。

use crate::platform::correction_learner::extract_candidates;
use crate::platform::daemon::{DaemonClient, LearningResult};
use crate::InjectionPlan;
use prost::Message;
use seasnail_proto::seasnail::native::v1::{monitor_event, MonitorEvent, MonitorRequest};
use seasnail_proto::{encode_native_frame, MAX_NATIVE_FRAME_BYTES};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;

pub(crate) const INITIAL_DELAY: Duration = Duration::from_millis(500);
pub(crate) const OBSERVATION_WINDOW: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub(crate) struct ObservationCoordinator {
    generation: Arc<AtomicU64>,
    target_pid: Arc<AtomicI32>,
    helper_process: Arc<Mutex<Option<(u64, i32)>>>,
    submission_gate: Arc<Mutex<()>>,
    enabled: Arc<AtomicBool>,
    log_path: Arc<Option<std::path::PathBuf>>,
}

impl Default for ObservationCoordinator {
    fn default() -> Self {
        Self::new(true)
    }
}

#[derive(Clone, Copy)]
enum HelperFailure {
    Unavailable,
    Protocol,
}

enum MonitorOutcome {
    Unchanged,
    Edited(String),
    Abandoned,
}

impl ObservationCoordinator {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            generation: Arc::new(AtomicU64::new(0)),
            target_pid: Arc::new(AtomicI32::new(0)),
            helper_process: Arc::new(Mutex::new(None)),
            submission_gate: Arc::new(Mutex::new(())),
            enabled: Arc::new(AtomicBool::new(enabled)),
            log_path: Arc::new(None),
        }
    }

    /// 接入 gui.log 诊断输出；构造后一次性接线，未接线时静默（测试不落地文件）。
    pub(crate) fn with_log_path(mut self, path: std::path::PathBuf) -> Self {
        self.log_path = Arc::new(Some(path));
        self
    }

    /// 追加一条观察诊断。只接受固定结果码/计数/PID，调用方不得传入任何文本内容。
    fn log(&self, code: &str) {
        self.log_with_gen(self.generation.load(Ordering::SeqCst), code);
    }

    fn log_with_gen(&self, generation: u64, code: &str) {
        let Some(path) = self.log_path.as_ref() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(
                file,
                "{} observation pid={} gen={} {}",
                chrono::Utc::now().to_rfc3339(),
                std::process::id(),
                generation,
                code
            );
        }
    }

    pub(crate) fn capture_target(&self) {
        self.cancel();
        let pid = frontmost_process_id();
        self.target_pid.store(pid, Ordering::SeqCst);
        if pid > 0 {
            self.log(&format!("target_captured pid={pid}"));
        } else {
            self.log("target_capture_failed");
        }
    }

    pub(crate) fn start(
        &self,
        client: DaemonClient,
        plan: InjectionPlan,
        on_result: impl Fn(LearningResult) + Send + 'static,
    ) {
        let _submission_guard = self
            .submission_gate
            .lock()
            .expect("observation submission mutex");
        if !self.enabled.load(Ordering::SeqCst) {
            self.log("skip_disabled");
            return;
        }
        let target_pid = self.target_pid.load(Ordering::SeqCst);
        if target_pid <= 0 {
            self.log("skip_no_target");
            return;
        }
        if plan.learning_ticket.is_empty() || plan.plain.is_empty() {
            self.log("skip_invalid_plan");
            return;
        }
        self.terminate_helper();
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.log_with_gen(generation, &format!("begin pid={target_pid}"));
        let coordinator = self.clone();
        thread::spawn(move || {
            thread::sleep(INITIAL_DELAY);
            if !coordinator.is_current(generation) {
                coordinator.log_with_gen(generation, "cancelled_before_observe");
                return;
            }
            let outcome = match coordinator.run_helper(generation, target_pid, &plan.plain) {
                Ok(outcome) => outcome,
                Err(HelperFailure::Unavailable) => {
                    coordinator.poll_fallback(generation, target_pid, &plan.plain)
                }
                Err(HelperFailure::Protocol) => MonitorOutcome::Abandoned,
            };
            if !coordinator.is_current(generation) || !coordinator.enabled.load(Ordering::SeqCst) {
                coordinator.log_with_gen(generation, "cancelled_or_disabled_before_submit");
                return;
            }
            // Serialize the final generation check with cancellation. This closes the gap where
            // preference-off/new-recording could otherwise occur after a check but before POST.
            let _submission_guard = coordinator
                .submission_gate
                .lock()
                .expect("observation submission mutex");
            if !coordinator.is_current(generation) {
                coordinator.log_with_gen(generation, "cancelled_before_submit");
                return;
            }
            let submission = match outcome {
                MonitorOutcome::Unchanged => {
                    coordinator.log_with_gen(generation, "submit mode=cleanup");
                    client.submit_dictionary_learning(&plan.learning_ticket, "cleanup", &[])
                }
                MonitorOutcome::Edited(value) => {
                    let candidates = extract_candidates(&plan.plain, &value);
                    if candidates.is_empty() {
                        coordinator.log_with_gen(generation, "submit_skip candidates_empty");
                        return;
                    }
                    coordinator.log_with_gen(
                        generation,
                        &format!("submit mode=user_edit candidates={}", candidates.len()),
                    );
                    client.submit_dictionary_learning(
                        &plan.learning_ticket,
                        "user_edit",
                        &candidates,
                    )
                }
                MonitorOutcome::Abandoned => {
                    coordinator.log_with_gen(generation, "submit_skip abandoned");
                    return;
                }
            };
            match submission {
                Err(code) => {
                    coordinator.log_with_gen(generation, &format!("submit_failed code={code}"));
                }
                Ok(result) if result.added_terms.is_empty() => {
                    coordinator.log_with_gen(generation, "submit_ok added=0");
                }
                Ok(result) => {
                    if !coordinator.is_current(generation) {
                        coordinator.log_with_gen(generation, "learned_stale_discarded");
                        return;
                    }
                    coordinator.log_with_gen(
                        generation,
                        &format!("learned added={}", result.added_terms.len()),
                    );
                    on_result(result);
                }
            }
        });
    }

    pub(crate) fn cancel(&self) {
        let _submission_guard = self
            .submission_gate
            .lock()
            .expect("observation submission mutex");
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.terminate_helper();
        self.log("cancel");
    }

    pub(crate) fn set_enabled(&self, enabled: bool) {
        let _submission_guard = self
            .submission_gate
            .lock()
            .expect("observation submission mutex");
        self.enabled.store(enabled, Ordering::SeqCst);
        if !enabled {
            self.generation.fetch_add(1, Ordering::SeqCst);
            self.terminate_helper();
            self.log("auto_learn_disabled");
        } else {
            self.log("auto_learn_enabled");
        }
    }

    fn terminate_helper(&self) {
        let process = self
            .helper_process
            .lock()
            .expect("helper process mutex")
            .take();
        if let Some((_, pid)) = process {
            #[cfg(unix)]
            unsafe {
                // Cancellation must be bounded even when a damaged helper ignores SIGTERM.
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }

    fn is_current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == generation
    }

    pub(crate) fn initial_value_matches(pasted: &str, observed: &str) -> bool {
        !pasted.is_empty() && observed.contains(pasted)
    }

    fn run_helper(
        &self,
        generation: u64,
        target_pid: i32,
        pasted: &str,
    ) -> Result<MonitorOutcome, HelperFailure> {
        let helper = match helper_path() {
            Some(helper) => helper,
            None => {
                self.log_with_gen(generation, "helper_missing");
                return Err(HelperFailure::Unavailable);
            }
        };
        let request = MonitorRequest {
            schema_version: 1,
            target_pid,
            pasted_text: pasted.to_string(),
            timeout_ms: OBSERVATION_WINDOW.as_millis() as u32,
        };
        let frame = match encode_native_frame(&request) {
            Ok(frame) => frame,
            Err(_) => {
                self.log_with_gen(generation, "helper_frame_encode_failed");
                return Err(HelperFailure::Protocol);
            }
        };
        // stderr 改为管道：捕获 helper 的固定 E_* 诊断码（白名单过滤后记录），绝不记录原文。
        let mut child = match Command::new(helper)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(_) => {
                self.log_with_gen(generation, "helper_spawn_failed");
                return Err(HelperFailure::Unavailable);
            }
        };
        let child_stderr = child.stderr.take();
        let child_pid = child.id() as i32;
        {
            let mut current = self.helper_process.lock().expect("helper process mutex");
            if !self.is_current(generation) || current.is_some() {
                let _ = child.kill();
                let _ = child.wait();
                self.log_with_gen(generation, "helper_slot_conflict");
                return Ok(MonitorOutcome::Abandoned);
            }
            *current = Some((generation, child_pid));
        }
        let watchdog = self.start_watchdog(generation, child_pid);
        let Some(mut stdin) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            self.clear_helper(generation, child_pid);
            let _ = watchdog.send(());
            self.log_with_gen(generation, "helper_stdin_unavailable");
            return Err(HelperFailure::Protocol);
        };
        if stdin.write_all(&frame).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            self.clear_helper(generation, child_pid);
            let _ = watchdog.send(());
            self.log_with_gen(generation, "helper_stdin_write_failed");
            return Err(HelperFailure::Protocol);
        }
        let Some(mut stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            self.clear_helper(generation, child_pid);
            let _ = watchdog.send(());
            self.log_with_gen(generation, "helper_stdout_unavailable");
            return Err(HelperFailure::Protocol);
        };
        let mut ready = false;
        let mut changed = false;
        let mut latest = String::new();
        let mut helper_failed = false;
        let outcome = loop {
            if !self.is_current(generation) {
                self.log_with_gen(generation, "cancelled_during_observe");
                break MonitorOutcome::Abandoned;
            }
            let event = match read_event(&mut stdout) {
                Ok(event) if event.schema_version == 1 => event,
                _ => {
                    helper_failed = true;
                    self.log_with_gen(generation, "helper_protocol_error");
                    break MonitorOutcome::Abandoned;
                }
            };
            match event.payload {
                Some(monitor_event::Payload::Ready(ready_event)) => {
                    if !Self::initial_value_matches(pasted, &ready_event.initial_value) {
                        self.log_with_gen(generation, "initial_value_mismatch");
                        break MonitorOutcome::Abandoned;
                    }
                    ready = true;
                    latest = ready_event.initial_value;
                }
                Some(monitor_event::Payload::Changed(value)) => {
                    if !ready {
                        helper_failed = true;
                        self.log_with_gen(generation, "helper_changed_before_ready");
                        break MonitorOutcome::Abandoned;
                    }
                    changed = true;
                    latest = value.current_value;
                }
                Some(monitor_event::Payload::Finished(finished)) => {
                    use monitor_event::finished::Reason;
                    let reason = Reason::try_from(finished.reason).unwrap_or(Reason::InternalError);
                    self.log_with_gen(
                        generation,
                        &format!("finished reason={}", reason_code(reason)),
                    );
                    if matches!(
                        reason,
                        Reason::NoElement | Reason::NoValue | Reason::InternalError
                    ) {
                        helper_failed = true;
                        break MonitorOutcome::Abandoned;
                    }
                    if !ready {
                        helper_failed = reason != Reason::Cancelled;
                        break MonitorOutcome::Abandoned;
                    }
                    break match reason {
                        Reason::Timeout if changed => MonitorOutcome::Edited(latest),
                        Reason::Timeout => MonitorOutcome::Unchanged,
                        Reason::FocusLost if changed => MonitorOutcome::Edited(latest),
                        _ => MonitorOutcome::Abandoned,
                    };
                }
                None => {
                    helper_failed = true;
                    self.log_with_gen(generation, "helper_empty_event");
                    break MonitorOutcome::Abandoned;
                }
            }
        };
        let _ = child.kill();
        let _ = child.wait();
        self.clear_helper(generation, child_pid);
        let _ = watchdog.send(());
        if let Some(stderr) = child_stderr {
            let codes = helper_diagnostic_codes(stderr);
            if !codes.is_empty() {
                self.log_with_gen(generation, &format!("helper_diag {}", codes.join(",")));
            }
        }
        self.log_with_gen(
            generation,
            &format!(
                "outcome {}",
                match &outcome {
                    MonitorOutcome::Unchanged => "unchanged",
                    MonitorOutcome::Edited(_) => "edited",
                    MonitorOutcome::Abandoned => "abandoned",
                }
            ),
        );
        if helper_failed {
            Err(HelperFailure::Protocol)
        } else {
            Ok(outcome)
        }
    }

    fn clear_helper(&self, generation: u64, pid: i32) {
        let mut current = self.helper_process.lock().expect("helper process mutex");
        if *current == Some((generation, pid)) {
            *current = None;
        }
    }

    fn start_watchdog(&self, generation: u64, pid: i32) -> std::sync::mpsc::Sender<()> {
        let processes = Arc::clone(&self.helper_process);
        let (cancel, receiver) = std::sync::mpsc::channel();
        thread::spawn(move || {
            if receiver
                .recv_timeout(OBSERVATION_WINDOW + Duration::from_secs(3))
                .is_ok()
            {
                return;
            }
            let current = processes.lock().expect("helper process mutex");
            if *current == Some((generation, pid)) {
                #[cfg(unix)]
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        });
        cancel
    }

    fn poll_fallback(&self, generation: u64, target_pid: i32, pasted: &str) -> MonitorOutcome {
        if !self.is_current(generation) {
            return MonitorOutcome::Abandoned;
        }
        self.log_with_gen(generation, "fallback_begin");
        poll_same_ax_element(self, generation, target_pid, pasted)
    }
}

fn read_event(reader: &mut impl Read) -> Result<MonitorEvent, ()> {
    let mut header = [0_u8; 4];
    reader.read_exact(&mut header).map_err(|_| ())?;
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_NATIVE_FRAME_BYTES {
        return Err(());
    }
    let mut body = vec![0_u8; len];
    reader.read_exact(&mut body).map_err(|_| ())?;
    MonitorEvent::decode(body.as_slice()).map_err(|_| ())
}

fn reason_code(reason: monitor_event::finished::Reason) -> &'static str {
    use monitor_event::finished::Reason;
    match reason {
        Reason::Unspecified => "unspecified",
        Reason::Timeout => "timeout",
        Reason::NoElement => "no_element",
        Reason::NoValue => "no_value",
        Reason::FocusLost => "focus_lost",
        Reason::Cancelled => "cancelled",
        Reason::InternalError => "internal_error",
    }
}

/// helper stderr 只允许固定 E_* 诊断码：读取上限 4 KiB，逐行白名单过滤后返回，
/// 任何不符合 `E_[A-Z0-9_]+` 形态的内容（可能含文本）都被丢弃，绝不进入日志。
fn helper_diagnostic_codes(stderr: impl Read) -> Vec<String> {
    let mut raw = String::new();
    if stderr.take(4096).read_to_string(&mut raw).is_err() {
        return Vec::new();
    }
    raw.lines()
        .filter(|line| {
            (3..=64).contains(&line.len())
                && line.starts_with("E_")
                && line
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        })
        .map(str::to_owned)
        .collect()
}

fn helper_path() -> Option<std::path::PathBuf> {
    let path = std::env::current_exe()
        .ok()?
        .parent()?
        .join("seasnail-post-paste-monitor");
    path.is_file().then_some(path)
}

#[cfg(target_os = "macos")]
fn frontmost_process_id() -> i32 {
    crate::platform::macos::frontmost_process_id().unwrap_or(0)
}

#[cfg(not(target_os = "macos"))]
fn frontmost_process_id() -> i32 {
    0
}

#[cfg(target_os = "macos")]
fn poll_same_ax_element(
    coordinator: &ObservationCoordinator,
    generation: u64,
    target_pid: i32,
    pasted: &str,
) -> MonitorOutcome {
    const SCRIPT: &str = r#"
on run argv
  set targetPid to (item 1 of argv) as integer
  set fifoPath to item 2 of argv
  set pastedText to do shell script "/bin/cat " & quoted form of fifoPath
  tell application "System Events"
    try
      set targetProcess to first application process whose unix id is targetPid
      set targetElement to value of attribute "AXFocusedUIElement" of targetProcess
      set latestValue to value of attribute "AXValue" of targetElement as text
      if latestValue does not contain pastedText then return "__SEASNAIL_ABANDONED_INITIAL__"
      set didChange to false
      repeat 60 times
        delay 0.5
        set currentElement to value of attribute "AXFocusedUIElement" of targetProcess
        if currentElement is not targetElement then return "__SEASNAIL_ABANDONED_ELEMENT__"
        set currentValue to value of attribute "AXValue" of targetElement as text
        if currentValue is not latestValue then
          set didChange to true
          set latestValue to currentValue
        end if
      end repeat
      if didChange then return "__SEASNAIL_EDITED__" & latestValue
      return "__SEASNAIL_UNCHANGED__"
    on error
      return "__SEASNAIL_ABANDONED_ERROR__"
    end try
  end tell
end run
"#;
    let Some((fifo_dir, fifo_path)) = create_private_fifo() else {
        coordinator.log_with_gen(generation, "fallback_fifo_failed");
        return MonitorOutcome::Abandoned;
    };
    let fifo_argument = fifo_path.to_string_lossy().into_owned();
    let mut child = match Command::new("osascript")
        .args(["-e", SCRIPT, "--", &target_pid.to_string(), &fifo_argument])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            cleanup_fifo(&fifo_dir, &fifo_path);
            coordinator.log_with_gen(generation, "fallback_spawn_failed");
            return MonitorOutcome::Abandoned;
        }
    };
    let pid = child.id() as i32;
    {
        let mut current = coordinator
            .helper_process
            .lock()
            .expect("helper process mutex");
        if !coordinator.is_current(generation) || current.is_some() {
            let _ = child.kill();
            let _ = child.wait();
            cleanup_fifo(&fifo_dir, &fifo_path);
            coordinator.log_with_gen(generation, "fallback_slot_conflict");
            return MonitorOutcome::Abandoned;
        }
        *current = Some((generation, pid));
    }
    let watchdog = coordinator.start_watchdog(generation, pid);
    if !write_fifo(&fifo_path, pasted.as_bytes()) {
        let _ = child.kill();
        let _ = child.wait();
        coordinator.clear_helper(generation, pid);
        let _ = watchdog.send(());
        cleanup_fifo(&fifo_dir, &fifo_path);
        coordinator.log_with_gen(generation, "fallback_fifo_write_failed");
        return MonitorOutcome::Abandoned;
    }
    cleanup_fifo(&fifo_dir, &fifo_path);
    let output = child.wait_with_output();
    coordinator.clear_helper(generation, pid);
    let _ = watchdog.send(());
    let Ok(output) = output else {
        coordinator.log_with_gen(generation, "fallback_wait_failed");
        return MonitorOutcome::Abandoned;
    };
    if !output.status.success() {
        coordinator.log_with_gen(generation, "fallback_status_nonzero");
        return MonitorOutcome::Abandoned;
    }
    if !coordinator.is_current(generation) {
        coordinator.log_with_gen(generation, "cancelled_during_observe");
        return MonitorOutcome::Abandoned;
    }
    let value = String::from_utf8_lossy(&output.stdout);
    // stdout 可能携带输入框全文（EDITED 分支），只匹配固定标记，绝不记录原文。
    if value.starts_with("__SEASNAIL_UNCHANGED__") {
        coordinator.log_with_gen(generation, "fallback_outcome unchanged");
        MonitorOutcome::Unchanged
    } else if let Some(edited) = value.strip_prefix("__SEASNAIL_EDITED__") {
        coordinator.log_with_gen(generation, "fallback_outcome edited");
        MonitorOutcome::Edited(edited.trim_end_matches(['\r', '\n']).to_string())
    } else {
        let reason = if value.starts_with("__SEASNAIL_ABANDONED_INITIAL__") {
            "initial_mismatch"
        } else if value.starts_with("__SEASNAIL_ABANDONED_ELEMENT__") {
            "element_changed"
        } else if value.starts_with("__SEASNAIL_ABANDONED_ERROR__") {
            "script_error"
        } else {
            "unknown"
        };
        coordinator.log_with_gen(generation, &format!("fallback_abandoned reason={reason}"));
        MonitorOutcome::Abandoned
    }
}

#[cfg(target_os = "macos")]
fn create_private_fifo() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    let directory = std::env::temp_dir().join(format!(
        "seasnail-post-paste-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir(&directory).ok()?;
    if std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).is_err() {
        let _ = std::fs::remove_dir(&directory);
        return None;
    }
    let path = directory.join("input.fifo");
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        let _ = std::fs::remove_dir(&directory);
        return None;
    };
    if unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) } != 0 {
        let _ = std::fs::remove_dir(&directory);
        return None;
    }
    Some((directory, path))
}

#[cfg(target_os = "macos")]
fn write_fifo(path: &std::path::Path, value: &[u8]) -> bool {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
        {
            Ok(mut file) => {
                let descriptor = file.as_raw_fd();
                let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
                if flags < 0
                    || unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags & !libc::O_NONBLOCK) }
                        < 0
                {
                    return false;
                }
                return file.write_all(value).is_ok();
            }
            Err(error)
                if error.raw_os_error() == Some(libc::ENXIO) && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return false,
        }
    }
}

#[cfg(target_os = "macos")]
fn cleanup_fifo(directory: &std::path::Path, path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_dir(directory);
}

#[cfg(not(target_os = "macos"))]
fn poll_same_ax_element(_: &ObservationCoordinator, _: u64, _: i32, _: &str) -> MonitorOutcome {
    MonitorOutcome::Abandoned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_observation_cancels_previous_and_initial_guard_uses_contains() {
        let coordinator = ObservationCoordinator::default();
        let first = coordinator.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let second = coordinator.generation.fetch_add(1, Ordering::SeqCst) + 1;
        assert!(!coordinator.is_current(first));
        assert!(coordinator.is_current(second));
        coordinator.cancel();
        assert!(!coordinator.is_current(second));
        assert!(ObservationCoordinator::initial_value_matches(
            "same",
            "before same after"
        ));
        assert!(!ObservationCoordinator::initial_value_matches(
            "same", "changed"
        ));
        assert_eq!(INITIAL_DELAY, Duration::from_millis(500));
        assert_eq!(OBSERVATION_WINDOW, Duration::from_secs(30));
    }

    #[test]
    fn framed_event_reader_handles_newlines_and_rejects_oversize() {
        let event = MonitorEvent {
            schema_version: 1,
            payload: Some(monitor_event::Payload::Changed(monitor_event::Changed {
                current_value: "line one\nline two".into(),
            })),
        };
        let frame = encode_native_frame(&event).unwrap();
        assert_eq!(read_event(&mut frame.as_slice()).unwrap(), event);
        let oversized = ((MAX_NATIVE_FRAME_BYTES as u32) + 1).to_be_bytes();
        assert!(read_event(&mut oversized.as_slice()).is_err());
    }

    #[test]
    fn helper_diagnostic_codes_whitelist_filters_arbitrary_text() {
        let codes = helper_diagnostic_codes(
            "E_NO_ELEMENT\nnot a code 含文本\nE_OK_2\ne_lower\nE_\n".as_bytes(),
        );
        assert_eq!(codes, ["E_NO_ELEMENT", "E_OK_2"]);
    }

    #[test]
    fn observation_log_appends_fixed_codes_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs/gui.log");
        let coordinator = ObservationCoordinator::default().with_log_path(path.clone());
        coordinator.log("skip_disabled");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("observation"));
        assert!(content.contains("gen=0"));
        assert!(content.contains("skip_disabled"));
    }
}
