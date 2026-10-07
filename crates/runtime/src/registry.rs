//! 活跃 runtime 注册表（ST-M3.1）。
//!
//! 活跃唯一、切换原子（`tokio::sync::Mutex<Option<Arc<dyn ModelRuntime>>>`）。
//! 切换链路（M3.10 activate）：active.stop() → clear() → new.start() → register()。

use crate::ModelRuntime;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct SidecarRegistry {
    active: Mutex<Option<Arc<dyn ModelRuntime>>>,
    orphan_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct ReapReport {
    pub orphan_pids_killed: u32,
    pub unresolved_records: u32,
}

impl SidecarRegistry {
    pub fn new() -> Self {
        Self::with_orphan_dir(None)
    }

    /// Production registry with a private, account-independent sidecar pid directory.
    pub fn new_with_orphan_dir(path: PathBuf) -> Self {
        Self::with_orphan_dir(Some(path))
    }

    fn with_orphan_dir(orphan_dir: Option<PathBuf>) -> Self {
        Self {
            active: Mutex::new(None),
            orphan_dir,
        }
    }

    /// 注册活跃 runtime。调用方应先 stop 旧的（切换链路）。
    pub async fn register(&self, rt: Arc<dyn ModelRuntime>) {
        *self.active.lock().await = Some(rt);
    }

    /// 取活跃 runtime（clone Arc）。None=无活跃。
    pub async fn active(&self) -> Option<Arc<dyn ModelRuntime>> {
        self.active.lock().await.clone()
    }

    /// 取活跃 runtime 的稳定 id（`ModelRuntime::id`）。None=无活跃。
    /// `GET /models` 据此标记某条目 `status=active`，`POST /models/{id} action=activate`
    /// 据此判幂等（`active_id==id` 则已激活，直接 202）。**只读 id**——不像 `active()`
    /// 克隆 Arc，避免给仅判等的调用方泄露运行时句柄（activate 链路除外，仍走 `active()`）。
    pub async fn active_id(&self) -> Option<String> {
        self.active
            .lock()
            .await
            .as_ref()
            .map(|r| r.id().to_string())
    }

    /// 清活跃（stop 后调）。幂等。
    pub async fn clear(&self) {
        *self.active.lock().await = None;
    }

    /// Reap only identities whose original owner has exited. Unknown records are retained.
    pub async fn reap_orphans(&self) -> ReapReport {
        self.reap(false).await
    }

    pub async fn reap_shutdown_children(&self) -> ReapReport {
        self.reap(true).await
    }

    async fn reap(&self, shutdown: bool) -> ReapReport {
        use crate::process_identity::{inspect, ProcessRecord};
        let Some(dir) = self.orphan_dir.as_deref() else {
            return ReapReport::default();
        };
        let mut report = ReapReport::default();
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return report,
            Err(_) => {
                report.unresolved_records += 1;
                return report;
            }
        };
        for entry in entries {
            let Ok(entry) = entry else {
                report.unresolved_records += 1;
                continue;
            };
            let path = entry.path();
            if path.extension().and_then(|v| v.to_str()) != Some("pid") {
                continue;
            }
            if path.is_symlink() {
                report.unresolved_records += 1;
                continue;
            }
            let record = fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<ProcessRecord>(&bytes).ok());
            let Some(record) =
                record.filter(|r| r.version == 1 && r.child.pid > 1 && r.child.pid != r.owner.pid)
            else {
                report.unresolved_records += 1;
                continue;
            };
            match inspect(record.child.pid) {
                Ok(Some(current)) if current == record.child => {}
                Ok(_) => {
                    if fs::remove_file(&path).is_err() {
                        report.unresolved_records += 1
                    }
                    continue;
                }
                Err(_) => {
                    report.unresolved_records += 1;
                    continue;
                }
            }
            match inspect(record.owner.pid) {
                Ok(Some(owner))
                    if owner == record.owner && !(shutdown && owner.pid == std::process::id()) =>
                {
                    report.unresolved_records += 1;
                    continue;
                }
                Err(_) => {
                    report.unresolved_records += 1;
                    continue;
                }
                _ => {}
            }
            let mut exited = false;
            for signal in [libc::SIGTERM, libc::SIGKILL] {
                // Recheck native birth before every signal; a reused PID is never killed.
                match inspect(record.child.pid) {
                    Ok(Some(current)) if current == record.child => unsafe {
                        libc::kill(current.pid as i32, signal);
                    },
                    Ok(_) => {
                        exited = true;
                        break;
                    }
                    Err(_) => break,
                }
                let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
                while tokio::time::Instant::now() < deadline {
                    match inspect(record.child.pid) {
                        Ok(Some(current)) if current == record.child => {}
                        Ok(_) => {
                            exited = true;
                            break;
                        }
                        Err(_) => break,
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                if exited {
                    break;
                }
            }
            if exited {
                report.orphan_pids_killed += 1;
                if fs::remove_file(path).is_err() {
                    report.unresolved_records += 1
                }
            } else {
                report.unresolved_records += 1
            }
        }
        report
    }
}

impl Default for SidecarRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::RuntimeKind;
    use crate::mock::{CannedResponse, MockRuntime};

    #[tokio::test]
    async fn native_records_protect_live_owner_and_reap_owned_shutdown_child() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("20")
            .spawn()
            .unwrap();
        let path = crate::process_identity::record(child.id(), "test", dir.path()).unwrap();
        let registry = SidecarRegistry::new_with_orphan_dir(dir.path().into());
        let startup = registry.reap_orphans().await;
        assert_eq!(startup.orphan_pids_killed, 0);
        assert_eq!(startup.unresolved_records, 1);
        assert!(child.try_wait().unwrap().is_none());
        let shutdown = registry.reap_shutdown_children().await;
        assert_eq!(shutdown.orphan_pids_killed, 1);
        assert_eq!(shutdown.unresolved_records, 0);
        child.wait().unwrap();
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn mismatched_birth_record_does_not_kill_same_executable() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("20")
            .spawn()
            .unwrap();
        let path = crate::process_identity::record(child.id(), "test", dir.path()).unwrap();
        let mut record: crate::process_identity::ProcessRecord =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        record.child.birth = "other-start".into();
        fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        let registry = SidecarRegistry::new_with_orphan_dir(dir.path().into());
        assert_eq!(registry.reap_orphans().await.orphan_pids_killed, 0);
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    /// `active_id` 跟踪当前注册的 runtime id；register 覆写（不停旧）；clear→None。
    /// 不需 start（active_id 只读 id()），故无端口/进程依赖，快且确定。
    #[tokio::test]
    async fn active_id_tracks_registered_runtime() {
        let reg = SidecarRegistry::new();
        assert_eq!(reg.active_id().await, None, "空 registry → None");

        let rt = Arc::new(MockRuntime::new(
            "whisper-tiny",
            RuntimeKind::Whisper,
            CannedResponse::default(),
        )) as Arc<dyn ModelRuntime>;
        reg.register(rt).await;
        assert_eq!(reg.active_id().await.as_deref(), Some("whisper-tiny"));

        // 覆写注册第二条：active_id 跟最新（register 不停旧；activate 链路显式 stop）。
        let rt2 = Arc::new(MockRuntime::new(
            "whisper-base",
            RuntimeKind::Whisper,
            CannedResponse::default(),
        )) as Arc<dyn ModelRuntime>;
        reg.register(rt2).await;
        assert_eq!(reg.active_id().await.as_deref(), Some("whisper-base"));

        reg.clear().await;
        assert_eq!(reg.active_id().await, None, "clear → None");
    }
}
