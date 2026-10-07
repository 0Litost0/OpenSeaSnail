//! flock 单例锁：守护进程同数据根目录只允许一个活跃实例。
//!
//! 锁文件 = `<data_dir>/daemon.lock`；非阻塞 acquire，已占用即「另一实例在运行」。
//! flock 在进程退出（含崩溃）时由内核释放，无需显式清理（与 bootstrap 的 temp+rename 不同）。
//! 对应设计文档「守护进程模块」单例约束与路线图 ST-M1.3。

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

use fs2::FileExt;

const LOCK_FILE: &str = "daemon.lock";

/// 持有锁直到 Drop；Drop / 进程退出释放 flock。
#[derive(Debug)]
pub struct SingletonLock {
    _file: File,
}

/// 单例锁获取失败。
#[derive(Debug)]
pub enum AcquireError {
    /// 另一守护进程实例已在运行。
    AlreadyRunning,
    /// I/O 错误（建目录 / 开锁文件失败）。
    Io(io::Error),
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning => write!(f, "另一守护进程实例已在运行"),
            Self::Io(e) => write!(f, "单例锁 I/O 错误: {e}"),
        }
    }
}

impl std::error::Error for AcquireError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::AlreadyRunning => None,
        }
    }
}

impl SingletonLock {
    /// 在 `<dir>/daemon.lock` 上 acquire 非阻塞排他锁。
    /// 已被占用 → `AcquireError::AlreadyRunning`。
    pub fn acquire(dir: &Path) -> Result<Self, AcquireError> {
        if !dir.exists() {
            std::fs::create_dir_all(dir).map_err(AcquireError::Io)?;
        }
        let path = dir.join(LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .map_err(AcquireError::Io)?;
        // 非阻塞排他锁；竞争时 fs2 返回 lock_contended_error()（WouldBlock 族）。
        let contended_kind = fs2::lock_contended_error().kind();
        if let Err(e) = file.try_lock_exclusive() {
            return Err(
                if e.kind() == contended_kind || e.kind() == io::ErrorKind::WouldBlock {
                    AcquireError::AlreadyRunning
                } else {
                    AcquireError::Io(e)
                },
            );
        }
        Ok(Self { _file: file })
    }

    /// 锁文件路径（诊断 / 测试用）。
    pub fn path(dir: &Path) -> std::path::PathBuf {
        dir.join(LOCK_FILE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn acquire_then_second_is_already_running() {
        // flock 按 open file description 计；同进程二次 open + try_lock 仍竞争。
        let dir = tmpdir();
        let _first = SingletonLock::acquire(dir.path()).expect("首次 acquire");
        match SingletonLock::acquire(dir.path()) {
            Err(AcquireError::AlreadyRunning) => {}
            other => panic!("期望 AlreadyRunning，实际 {other:?}"),
        }
    }

    #[test]
    fn lock_releases_on_drop() {
        let dir = tmpdir();
        {
            let _l = SingletonLock::acquire(dir.path()).expect("acquire");
        } // Drop 释放
        let _l2 = SingletonLock::acquire(dir.path()).expect("Drop 后应可重新获取");
    }

    #[test]
    fn acquire_creates_missing_dir() {
        let dir = tmpdir();
        let sub = dir.path().join("nested");
        let _l = SingletonLock::acquire(&sub).expect("acquire 建目录");
        assert!(sub.exists(), "缺失目录应被创建");
    }

    #[test]
    fn lock_file_persists_and_reuse_across_acquire() {
        // 锁文件是空文件，跨重启复用；flock 在 fd 上而非文件内容。
        let dir = tmpdir();
        let p = SingletonLock::path(dir.path());
        {
            let _l = SingletonLock::acquire(dir.path()).unwrap();
            assert!(p.exists());
        }
        let _l2 = SingletonLock::acquire(dir.path()).unwrap(); // 复用同文件
        assert!(p.exists());
    }
}
