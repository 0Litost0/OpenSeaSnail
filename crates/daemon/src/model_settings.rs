//! ST-M1.2：daemon 全局 component-keyed 模型设置存储。
//!
//! 持久化可选组件（punc/spk）的 `enabled` 开关到 `<DataDir>/models-settings.json`
//!（与 `bootstrap.json` 同根）。component-keyed：`Map<component_id, enabled>`，MVP 仅
//! `punc`，缺省条目视为 `false`（设计默认：punc/spk 不加载）。`downloaded` 由文件系统
//! 派生、`download_progress` 为下载期内存态——均不入此存储。
//!
//! 原子写（temp→fsync→chmod 0o600→rename）仿 [`crate::bootstrap::Bootstrap`]；缺失/损坏
//! 按默认（全 false）处理，不阻断启动。AppState 经 `data_dir` 构造，handler 经
//! `state.settings()` 读写（读在 sessions 注入 punc/spk，写在 `PUT /models/{component}`）。

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

const SETTINGS_FILE: &str = "models-settings.json";
const SETTINGS_TMP: &str = ".models-settings.json.tmp";

/// 落盘结构。加 component 不改 schema（未知条目缺省 false）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SettingsFile {
    /// component_id → enabled。未知 component 缺省 false（MVP 默认：punc/spk 不加载）。
    #[serde(default)]
    components: HashMap<String, bool>,
    #[serde(default)]
    backend: Option<String>,
}

/// 全局 component-keyed 模型设置。进程内持 `Mutex` 包内存态，写时 best-effort 持久化。
pub struct ModelSettings {
    dir: PathBuf,
    state: Mutex<SettingsFile>,
}

/// Immutable per-run feature snapshot.  A transcription never observes a
/// partially changed punc/spk configuration while it is in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSettingsSnapshot {
    pub punc: bool,
    pub spk: bool,
}

impl ModelSettings {
    /// 用指定 data_dir 构造并 best-effort 读已有设置；缺失/损坏 → 默认（全 false）。
    /// 不失败：读盘错误降级为空设置，不阻断启动（与 bootstrap 同语义）。
    pub fn new(dir: PathBuf) -> Self {
        let state = match std::fs::read(dir.join(SETTINGS_FILE)) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => SettingsFile::default(),
        };
        Self {
            dir,
            state: Mutex::new(state),
        }
    }

    /// 文件最终路径。
    pub fn path(&self) -> PathBuf {
        self.dir.join(SETTINGS_FILE)
    }

    /// 设置文件所在 data_dir（构造时传入的 `<DataDir>`）。M2.3 用作 models extra-root
    /// 派生源：`dir().join("models")` = 用户下载模型目录（`~/Library/.../models/`）。
    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// 组件是否启用。未知 component（缺省条目）返回 false。
    pub fn is_enabled(&self, component: &str) -> bool {
        self.state
            .lock()
            .expect("model settings mutex poisoned")
            .components
            .get(component)
            .copied()
            .unwrap_or(false)
    }

    pub fn snapshot(&self) -> ModelSettingsSnapshot {
        let guard = self.state.lock().expect("model settings mutex poisoned");
        ModelSettingsSnapshot {
            punc: guard.components.get("punc").copied().unwrap_or(false),
            spk: guard.components.get("spk").copied().unwrap_or(false),
        }
    }

    /// 设组件开关并 best-effort 持久化。**持锁跨落盘**：payload 小、开关低频，串行化
    /// 写者避免"克隆快照出锁再 persist"的乱序——否则并发 set 各自持旧快照 rename，后
    /// rename 的旧值会覆盖盘上更新值（重启丢值）。内存态随 insert 立即更新；落盘失败
    /// 返回 `io::Error`（组件开关保留既有 best-effort 内存语义）。后端选择由
    /// `set_backend` 额外保证失败时回滚内存，以便模型激活补偿后内存/磁盘一致。
    pub fn set_enabled(&self, component: &str, enabled: bool) -> io::Result<()> {
        let mut guard = self.state.lock().expect("model settings mutex poisoned");
        guard.components.insert(component.to_string(), enabled);
        self.persist(&guard)
    }

    pub fn backend(&self) -> Option<String> {
        self.state
            .lock()
            .expect("model settings mutex poisoned")
            .backend
            .clone()
    }

    pub fn set_backend(&self, backend: &str) -> io::Result<()> {
        let mut guard = self.state.lock().expect("model settings mutex poisoned");
        let previous = guard.backend.clone();
        guard.backend = Some(backend.to_owned());
        if let Err(error) = self.persist(&guard) {
            guard.backend = previous;
            return Err(error);
        }
        Ok(())
    }

    /// 原子写：确保基目录 → temp→fsync→chmod 0o600→rename（仿 `bootstrap.rs`）。
    fn persist(&self, s: &SettingsFile) -> io::Result<()> {
        if !self.dir.exists() {
            std::fs::create_dir_all(&self.dir)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.dir, PermissionsExt::from_mode(0o700));
        }
        let bytes = serde_json::to_vec_pretty(s)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let final_path = self.path();
        let tmp_path = self.dir.join(SETTINGS_TMP);
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp_path)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp_path, PermissionsExt::from_mode(0o600))?;
        }
        std::fs::rename(&tmp_path, &final_path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_at(dir: &tempfile::TempDir) -> ModelSettings {
        ModelSettings::new(dir.path().to_path_buf())
    }

    #[test]
    fn defaults_all_false_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let s = store_at(&dir);
        assert!(!s.is_enabled("punc"));
        assert!(!s.is_enabled("spk"));
        assert!(!s.is_enabled("unknown"));
    }

    #[test]
    fn set_and_reread_across_instances() {
        // 跨实例（模拟重启）读回 enabled——ST-M1.2 验收。
        let dir = tempfile::tempdir().unwrap();
        let s = store_at(&dir);
        s.set_enabled("punc", true).unwrap();
        assert!(s.is_enabled("punc"));

        let s2 = store_at(&dir); // 新实例从盘读
        assert!(s2.is_enabled("punc"), "重启后应读回 enabled=true");
        assert!(!s2.is_enabled("spk"), "未设的仍 false");
    }

    #[test]
    fn set_false_disables_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let s = store_at(&dir);
        s.set_enabled("punc", true).unwrap();
        s.set_enabled("punc", false).unwrap();
        assert!(!s.is_enabled("punc"));
        let s2 = store_at(&dir);
        assert!(!s2.is_enabled("punc"), "关闭应落盘并可读回");
    }

    #[test]
    fn backend_selection_persists_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        let settings = store_at(&dir);
        assert_eq!(
            settings.backend(),
            None,
            "missing setting uses the release default"
        );
        settings.set_backend("funasr-default").unwrap();
        assert_eq!(settings.backend().as_deref(), Some("funasr-default"));
        let reloaded = store_at(&dir);
        assert_eq!(reloaded.backend().as_deref(), Some("funasr-default"));
    }

    #[test]
    fn corrupt_file_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join(SETTINGS_FILE), b"not json").unwrap();
        let s = store_at(&dir);
        assert!(!s.is_enabled("punc"), "损坏文件按默认（全 false）处理");
    }

    #[test]
    fn adding_component_needs_no_schema_change() {
        // 旧文件（只 punc）读回后设 spk，punc 不丢——加 component 不改结构。
        let dir = tempfile::tempdir().unwrap();
        let s = store_at(&dir);
        s.set_enabled("punc", true).unwrap();
        let s2 = store_at(&dir);
        s2.set_enabled("spk", true).unwrap();
        assert!(s2.is_enabled("punc"), "punc 仍在");
        assert!(s2.is_enabled("spk"));
        let s3 = store_at(&dir);
        assert!(s3.is_enabled("punc") && s3.is_enabled("spk"));
    }

    #[cfg(unix)]
    #[test]
    fn file_mode_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let s = store_at(&dir);
        s.set_enabled("punc", true).unwrap();
        let mode = std::fs::metadata(s.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "settings 文件权限应为 0o600");
    }

    #[cfg(unix)]
    #[test]
    fn dir_mode_is_0700_when_created() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("nested");
        let s = ModelSettings::new(sub.clone());
        s.set_enabled("punc", true).unwrap();
        let mode = std::fs::metadata(&sub).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "新建基目录权限应为 0o700");
    }
}
