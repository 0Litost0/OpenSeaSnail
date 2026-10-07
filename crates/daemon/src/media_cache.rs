//! 明文剪贴板媒体缓存的安全删除（macOS/Unix）。
//!
//! 所有目录访问都相对一个已打开的目录 FD 完成，并使用 `O_NOFOLLOW`；因此路径在
//! 检查后被替换成符号链接也不会把删除操作带出 SeaSnail 数据根。

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

fn c_name(name: &str) -> io::Result<std::ffi::CString> {
    std::ffi::CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

fn open_dir_at(parent: &File, name: &str) -> io::Result<Option<File>> {
    let name = c_name(name)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd >= 0 {
        return Ok(Some(unsafe { File::from_raw_fd(fd) }));
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::NotFound {
        Ok(None)
    } else {
        Err(error)
    }
}

fn unlink_at(parent: &File, name: &str, flags: i32) -> io::Result<()> {
    let name = c_name(name)?;
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), flags) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn remove_tree_at(parent: &File, name: &str) -> io::Result<bool> {
    let Some(dir) = open_dir_at(parent, name)? else {
        return Ok(false);
    };
    // 空 capture 目录无需枚举；这也是最常见的中断写入恢复路径。
    if unlink_at(parent, name, libc::AT_REMOVEDIR).is_ok() {
        return Ok(true);
    }
    // `fdopendir/readdir` 枚举已打开的目录 FD；子项仍只通过 `openat/unlinkat` 操作。
    let duplicate = unsafe { libc::dup(dir.as_raw_fd()) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        unsafe { libc::close(duplicate) };
        return Err(io::Error::last_os_error());
    }
    loop {
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break;
        }
        let child = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_string_lossy();
        let child = child.as_ref();
        if child == "." || child == ".." {
            continue;
        }
        match open_dir_at(&dir, child) {
            Ok(Some(_)) => {
                let _ = remove_tree_at(&dir, child)?;
            }
            Ok(None) => {}
            Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
                unlink_at(&dir, child, 0)?;
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOTDIR) => {
                unlink_at(&dir, child, 0)?;
            }
            Err(error) => return Err(error),
        }
    }
    unsafe { libc::closedir(stream) };
    unlink_at(parent, name, libc::AT_REMOVEDIR)?;
    Ok(true)
}

/// 删除某账户下单个 UUID capture 目录。不存在返回 false；任何符号链接组件都会失败。
pub fn remove_capture(data_root: &Path, account_id: &str, capture_id: &str) -> io::Result<bool> {
    if uuid::Uuid::parse_str(account_id).is_err() || uuid::Uuid::parse_str(capture_id).is_err() {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(data_root)?;
    let Some(cache) = open_dir_at(&root, "cache")? else {
        return Ok(false);
    };
    let Some(clipboard) = open_dir_at(&cache, "clipboard-context")? else {
        return Ok(false);
    };
    let Some(account) = open_dir_at(&clipboard, account_id)? else {
        return Ok(false);
    };
    remove_tree_at(&account, capture_id)
}

/// 删除某账户的整个媒体缓存根（同样使用固定父目录 FD）。
pub fn remove_account(data_root: &Path, account_id: &str) -> io::Result<bool> {
    if uuid::Uuid::parse_str(account_id).is_err() {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(data_root)?;
    let Some(cache) = open_dir_at(&root, "cache")? else {
        return Ok(false);
    };
    let Some(clipboard) = open_dir_at(&cache, "clipboard-context")? else {
        return Ok(false);
    };
    remove_tree_at(&clipboard, account_id)
}

fn valid_date_bucket(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
}

/// 删除加密数据树中的一个 session 目录。每级目录均以已打开 FD + O_NOFOLLOW 寻址。
pub fn remove_session_data(
    data_root: &Path,
    account_id: &str,
    date: &str,
    session_id: &str,
) -> io::Result<bool> {
    if uuid::Uuid::parse_str(account_id).is_err()
        || uuid::Uuid::parse_str(session_id).is_err()
        || !valid_date_bucket(date)
    {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(data_root)?;
    let Some(data) = open_dir_at(&root, "data")? else {
        return Ok(false);
    };
    let Some(account) = open_dir_at(&data, account_id)? else {
        return Ok(false);
    };
    let Some(date_dir) = open_dir_at(&account, date)? else {
        return Ok(false);
    };
    remove_tree_at(&date_dir, session_id)
}

/// 以 fd-relative/no-follow 方式检查 session 目录中的固定普通文件。
///
/// 返回值只包含调用方给出的文件名；任何祖先目录或候选文件是符号链接时均失败。
pub fn enumerate_session_files(
    data_root: &Path,
    account_id: &str,
    date: &str,
    session_id: &str,
    names: &[&str],
) -> io::Result<Vec<String>> {
    if uuid::Uuid::parse_str(account_id).is_err()
        || uuid::Uuid::parse_str(session_id).is_err()
        || !valid_date_bucket(date)
    {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(data_root)?;
    let Some(data) = open_dir_at(&root, "data")? else {
        return Ok(Vec::new());
    };
    let Some(account) = open_dir_at(&data, account_id)? else {
        return Ok(Vec::new());
    };
    let Some(date_dir) = open_dir_at(&account, date)? else {
        return Ok(Vec::new());
    };
    let Some(session) = open_dir_at(&date_dir, session_id)? else {
        return Ok(Vec::new());
    };

    let mut present = Vec::new();
    for name in names {
        if name.is_empty() || *name == "." || *name == ".." || name.contains('/') {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let c_name = c_name(name)?;
        let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::fstatat(
                session.as_raw_fd(),
                c_name.as_ptr(),
                &mut metadata,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                continue;
            }
            return Err(error);
        }
        if metadata.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "session artifact is not a regular file",
            ));
        }
        present.push((*name).to_owned());
    }
    Ok(present)
}

/// 删除整个账户加密数据目录，阻止 `data/<account_id>` 被符号链接替换后越界删除。
pub fn remove_account_data(data_root: &Path, account_id: &str) -> io::Result<bool> {
    if uuid::Uuid::parse_str(account_id).is_err() {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(data_root)?;
    let Some(data) = open_dir_at(&root, "data")? else {
        return Ok(false);
    };
    remove_tree_at(&data, account_id)
}

#[cfg(test)]
mod tests {
    use super::{
        enumerate_session_files, remove_account_data, remove_capture, remove_session_data,
    };

    #[cfg(unix)]
    #[test]
    fn capture_symlink_is_not_followed_during_delete() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let account = uuid::Uuid::new_v4().to_string();
        let capture = uuid::Uuid::new_v4().to_string();
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("must-survive"), b"safe").unwrap();
        let account_dir = temp.path().join("cache/clipboard-context").join(&account);
        std::fs::create_dir_all(&account_dir).unwrap();
        symlink(&outside, account_dir.join(&capture)).unwrap();

        assert!(remove_capture(temp.path(), &account, &capture).is_err());
        assert!(outside.join("must-survive").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn session_and_account_data_symlinks_are_not_followed() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let account = uuid::Uuid::new_v4().to_string();
        let session = uuid::Uuid::new_v4().to_string();
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("must-survive"), b"safe").unwrap();

        let account_dir = temp.path().join("data").join(&account);
        std::fs::create_dir_all(account_dir.join("2026-09-07")).unwrap();
        symlink(&outside, account_dir.join("2026-09-07").join(&session)).unwrap();
        assert!(remove_session_data(temp.path(), &account, "2026-09-07", &session).is_err());
        assert!(outside.join("must-survive").is_file());

        std::fs::remove_dir_all(temp.path().join("data")).unwrap();
        std::fs::create_dir_all(temp.path().join("data")).unwrap();
        symlink(&outside, temp.path().join("data").join(&account)).unwrap();
        assert!(remove_account_data(temp.path(), &account).is_err());
        assert!(outside.join("must-survive").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn session_enumeration_rejects_symlinked_ancestor_and_file() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let account = uuid::Uuid::new_v4().to_string();
        let session = uuid::Uuid::new_v4().to_string();
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(outside.join(&session)).unwrap();
        std::fs::write(outside.join(&session).join("cleanup.pb.enc"), b"outside").unwrap();
        std::fs::create_dir_all(temp.path().join("data").join(&account)).unwrap();
        symlink(
            &outside,
            temp.path().join("data").join(&account).join("2026-09-07"),
        )
        .unwrap();

        assert!(enumerate_session_files(
            temp.path(),
            &account,
            "2026-09-07",
            &session,
            &["cleanup.pb.enc"],
        )
        .is_err());

        std::fs::remove_file(temp.path().join("data").join(&account).join("2026-09-07")).unwrap();
        let session_dir = temp
            .path()
            .join("data")
            .join(&account)
            .join("2026-09-07")
            .join(&session);
        std::fs::create_dir_all(&session_dir).unwrap();
        symlink(
            outside.join(&session).join("cleanup.pb.enc"),
            session_dir.join("cleanup.pb.enc"),
        )
        .unwrap();
        assert!(enumerate_session_files(
            temp.path(),
            &account,
            "2026-09-07",
            &session,
            &["cleanup.pb.enc"],
        )
        .is_err());
    }
}
