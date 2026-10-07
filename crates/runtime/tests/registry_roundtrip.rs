//! ST-M3.1 验收：SidecarRegistry 注册/取活跃/清空往返，活跃唯一、切换原子。

use seasnail_runtime::{MockRuntime, ModelRuntime, RuntimeKind, SidecarRegistry};
use std::sync::Arc;

#[tokio::test]
async fn registry_roundtrip_and_switch() {
    let reg = SidecarRegistry::new();
    assert!(reg.active().await.is_none(), "初始无活跃");

    let rt = Arc::new(MockRuntime::openai_default()) as Arc<dyn ModelRuntime>;
    reg.register(rt).await;
    assert!(reg.active().await.is_some(), "注册后有活跃");
    assert_eq!(
        reg.active().await.unwrap().id(),
        "mock-openai",
        "活跃 = 最近注册"
    );

    // 切换：活跃唯一，后者覆盖。
    let rt2 = Arc::new(MockRuntime::new(
        "mock-2",
        RuntimeKind::Whisper,
        Default::default(),
    )) as Arc<dyn ModelRuntime>;
    reg.register(rt2).await;
    assert_eq!(reg.active().await.unwrap().id(), "mock-2", "切换后活跃更新");

    reg.clear().await;
    assert!(reg.active().await.is_none(), "清空后无活跃");
}

// Native records bind both process birth identity and owner. The old two-line
// PID/executable format is deliberately untrusted and must never authorize a kill.
#[tokio::test]
async fn reap_orphans_retains_unrecognized_legacy_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.pid");
    std::fs::write(&path, "999999\n/private/does-not-exist\n").unwrap();
    let reg = SidecarRegistry::new_with_orphan_dir(dir.path().to_path_buf());
    let report = reg.reap_orphans().await;
    assert_eq!(report.orphan_pids_killed, 0);
    assert_eq!(report.unresolved_records, 1);
    assert!(
        path.exists(),
        "unknown records must be retained for diagnosis"
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct TestProcess(std::process::Child);

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl TestProcess {
    fn spawn() -> Self {
        Self(
            std::process::Command::new("/bin/sleep")
                .arg("30")
                .spawn()
                .unwrap(),
        )
    }

    fn exit(&mut self) {
        let _ = self.0.kill();
        self.0.wait().unwrap();
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Drop for TestProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn reap_orphans_removes_stale_pid_records() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = TestProcess::spawn();
    let path =
        seasnail_runtime::process_identity::record(child.0.id(), "test", dir.path()).unwrap();
    child.exit();
    let reg = SidecarRegistry::new_with_orphan_dir(dir.path().to_path_buf());
    let report = reg.reap_orphans().await;
    assert_eq!(report.orphan_pids_killed, 0);
    assert_eq!(report.unresolved_records, 0);
    assert!(
        !path.exists(),
        "confirmed exited children must lose their stale record"
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn reap_orphans_kills_matching_process_and_keeps_mismatched_process() {
    use seasnail_runtime::process_identity::{inspect, record, ProcessRecord};
    let dir = tempfile::tempdir().unwrap();
    let mut owner = TestProcess::spawn();
    let departed_owner = inspect(owner.0.id()).unwrap().unwrap();
    owner.exit();

    let mut child = TestProcess::spawn();
    let path = record(child.0.id(), "test", dir.path()).unwrap();
    let mut matching: ProcessRecord =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    matching.owner = departed_owner.clone();
    std::fs::write(&path, serde_json::to_vec(&matching).unwrap()).unwrap();
    let reg = SidecarRegistry::new_with_orphan_dir(dir.path().to_path_buf());
    let report = reg.reap_orphans().await;
    assert_eq!(report.orphan_pids_killed, 1);
    assert_eq!(report.unresolved_records, 0);
    assert!(!path.exists());
    child.0.wait().unwrap();

    let mut other = TestProcess::spawn();
    let path = record(other.0.id(), "test", dir.path()).unwrap();
    let mut mismatched: ProcessRecord =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    mismatched.owner = departed_owner;
    mismatched.child.executable = "/private/not-the-running-process".into();
    std::fs::write(&path, serde_json::to_vec(&mismatched).unwrap()).unwrap();
    let report = reg.reap_orphans().await;
    assert_eq!(report.orphan_pids_killed, 0);
    assert_eq!(report.unresolved_records, 0);
    assert!(!path.exists(), "a disproved identity is a stale record");
    assert!(
        other.0.try_wait().unwrap().is_none(),
        "a mismatched live process must never be killed"
    );
}
