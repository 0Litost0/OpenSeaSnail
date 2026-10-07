//! Bootstrap 文件：守护进程写 port（+ version），GUI / CLI 据此发现运行中的守护进程。
//!
//! 原子写（temp + rename，mode 0o600），token 不落盘；缺失或损坏按 stale 处理。
//! 对应设计文档「守护进程模块」`Bootstrap` trait 与路线图 ST-M1.2。

use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// bootstrap 落盘内容。仅 port + version，绝不存 token。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapInfo {
    pub version: u32,
    pub port: u16,
}

/// bootstrap 格式版本。一经发布不可变（语义同 HKDF info 串，改即破坏发现兼容）。
const BOOTSTRAP_VERSION: u32 = 1;
const BOOTSTRAP_FILE: &str = "bootstrap.json";
const BOOTSTRAP_TMP: &str = ".bootstrap.json.tmp";

/// bootstrap 读写器，绑定一个基目录。
/// 默认 `$SEASNAIL_DATA_DIR` 或 `~/Library/Application Support/SeaSnail/`；测试可注入临时目录。
/// 与 per-account 数据（`.../SeaSnail/data/`）同根，发现类文件（bootstrap / daemon.lock）亦落此。
pub struct Bootstrap {
    dir: PathBuf,
}

impl Bootstrap {
    /// 默认基目录：`$SEASNAIL_DATA_DIR` 优先，否则 `$HOME/Library/Application Support/SeaSnail`。
    pub fn default_dir() -> io::Result<PathBuf> {
        if let Some(d) = std::env::var_os("SEASNAIL_DATA_DIR") {
            return Ok(PathBuf::from(d));
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME 未设置"))?;
        Ok(home
            .join("Library")
            .join("Application Support")
            .join("SeaSnail"))
    }

    /// 用默认基目录构造。
    pub fn new_default() -> io::Result<Self> {
        Ok(Self {
            dir: Self::default_dir()?,
        })
    }

    /// 用指定基目录构造（测试注入）。
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// bootstrap 文件最终路径。
    pub fn path(&self) -> PathBuf {
        self.dir.join(BOOTSTRAP_FILE)
    }

    /// 临时文件路径（写崩溃残留线索）。
    pub fn tmp_path(&self) -> PathBuf {
        self.dir.join(BOOTSTRAP_TMP)
    }

    /// 原子写：temp → fsync → chmod 0o600 → rename。
    /// 写入中途崩溃：要么旧文件不变、要么新文件就位，不留半截。
    pub fn write(&self, port: u16) -> io::Result<()> {
        // 确保基目录存在；新建则收紧到 0o700。
        let created = !self.dir.exists();
        if created {
            std::fs::create_dir_all(&self.dir)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // best-effort 收紧目录权限（已存在目录亦对齐）。
            let _ = std::fs::set_permissions(&self.dir, PermissionsExt::from_mode(0o700));
        }

        let info = BootstrapInfo {
            version: BOOTSTRAP_VERSION,
            port,
        };
        let bytes =
            serde_json::to_vec(&info).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        let final_path = self.path();
        let tmp_path = self.tmp_path();

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
        // rename 原子替换：要么旧、要么新，无半截。
        std::fs::rename(&tmp_path, &final_path)?;
        Ok(())
    }

    /// 读 bootstrap；缺失或损坏返回 None（按 stale 处理）。
    pub fn read(&self) -> Option<BootstrapInfo> {
        let bytes = std::fs::read(self.path()).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// 连一次 health，连不上当 stale 返回 false。对应 `Bootstrap::validate`。
    pub async fn validate(&self) -> bool {
        let Some(info) = self.read() else {
            return false;
        };
        health_probe(info.port).await
    }

    /// 清理 bootstrap（守护进程退出时调）；不存在视为成功。
    pub fn clear(&self) -> io::Result<()> {
        match std::fs::remove_file(self.path()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// 单次健康探测：GET / 返回 200 即存活。供 `validate` 与测试复用。
pub async fn health_probe(port: u16) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
        Ok(s) => s,
        Err(_) => return false,
    };
    let req = b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
    if s.write_all(req).await.is_err() {
        return false;
    }
    let mut buf = [0u8; 128];
    match s.read(&mut buf).await {
        Ok(n) if n > 0 => buf.starts_with(b"HTTP/1.1 200") || buf.starts_with(b"HTTP/1.0 200"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn bs_at(dir: &tempfile::TempDir) -> Bootstrap {
        Bootstrap::new(dir.path().to_path_buf())
    }

    #[test]
    fn write_then_read_roundtrip() {
        let dir = tmpdir();
        let bs = bs_at(&dir);
        bs.write(54321).unwrap();
        let info = bs.read().expect("应能读回");
        assert_eq!(info.port, 54321);
        assert_eq!(info.version, 1);
    }

    #[test]
    fn read_returns_none_when_missing() {
        let dir = tmpdir();
        let bs = bs_at(&dir);
        assert!(bs.read().is_none());
    }

    #[test]
    fn read_returns_none_when_corrupt() {
        let dir = tmpdir();
        let bs = bs_at(&dir);
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(bs.path(), b"not json").unwrap();
        assert!(bs.read().is_none(), "损坏的 bootstrap 应按 stale 处理");
    }

    #[test]
    fn no_tmp_lingering_after_write() {
        let dir = tmpdir();
        let bs = bs_at(&dir);
        bs.write(11111).unwrap();
        assert!(bs.path().exists());
        assert!(!bs.tmp_path().exists(), "temp 残留应被 rename 清掉");
    }

    #[cfg(unix)]
    #[test]
    fn file_mode_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmpdir();
        let bs = bs_at(&dir);
        bs.write(22222).unwrap();
        let mode = std::fs::metadata(bs.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "bootstrap 文件权限应为 0o600");
    }

    #[cfg(unix)]
    #[test]
    fn dir_mode_is_0700_when_created() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmpdir();
        let sub = dir.path().join("nested");
        let bs = Bootstrap::new(sub.clone());
        bs.write(33333).unwrap();
        let mode = std::fs::metadata(&sub).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "新建基目录权限应为 0o700");
    }

    #[test]
    fn clear_removes_file() {
        let dir = tmpdir();
        let bs = bs_at(&dir);
        bs.write(44444).unwrap();
        assert!(bs.path().exists());
        bs.clear().unwrap();
        assert!(!bs.path().exists());
    }

    #[test]
    fn clear_when_missing_is_ok() {
        let dir = tmpdir();
        let bs = bs_at(&dir);
        assert!(bs.clear().is_ok(), "清理不存在的 bootstrap 应视为成功");
    }

    #[tokio::test]
    async fn validate_false_for_stale_port() {
        // 写一个无人监听的端口 → validate 应判 stale。
        let dir = tmpdir();
        let bs = bs_at(&dir);
        bs.write(1).unwrap(); // 1 号特权端口，测试进程无监听
        assert!(!bs.validate().await, "无人监听的端口应判 stale");
    }

    #[tokio::test]
    async fn validate_false_when_missing() {
        let dir = tmpdir();
        let bs = bs_at(&dir);
        assert!(!bs.validate().await, "无 bootstrap 应判 stale");
    }
}
