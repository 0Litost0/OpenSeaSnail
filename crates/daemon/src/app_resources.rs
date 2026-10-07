//! macOS `.app` 内资源定位（M6.0）。
//!
//! 开发时 daemon 从 `target/*` 启动，仍允许调用方通过环境变量显式指定资源；发布或
//! 开发版 `.app` 则只从 `SeaSnail.app/Contents/Resources` 取资源，避免意外回退到
//! 用户机器上的同名工具。

use std::path::{Path, PathBuf};

/// 由 app 内可执行文件路径推导 `Contents/Resources`。
pub fn resources_dir_from_executable(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent()?;
    if macos.file_name()? != "MacOS" {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }
    Some(contents.join("Resources"))
}

/// 当前 daemon 位于 `.app/Contents/MacOS/` 时，返回其中某个资源的绝对路径。
pub fn bundled_resource(name: &str) -> Option<PathBuf> {
    resources_dir_from_executable(&std::env::current_exe().ok()?).map(|dir| dir.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_resources_only_for_app_layout() {
        assert_eq!(
            resources_dir_from_executable(Path::new(
                "/tmp/SeaSnail.app/Contents/MacOS/seasnail-daemon"
            )),
            Some(PathBuf::from("/tmp/SeaSnail.app/Contents/Resources"))
        );
        assert!(
            resources_dir_from_executable(Path::new("/tmp/target/debug/seasnail-daemon")).is_none()
        );
    }
}
