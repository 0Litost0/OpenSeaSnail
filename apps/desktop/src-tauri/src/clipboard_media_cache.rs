//! 录音期剪贴板图片的本地明文缓存。
//!
//! 图片本体要交给本机 Agent 读取，不能放进加密会话文件；但其目录层级和权限仍必须
//! 受 SeaSnail 控制。此模块只接受已解码的 RGBA 像素，因而可由 collector 的后台
//! 线程调用，绝不能从 cpal 音频回调调用。

#[cfg(not(unix))]
use std::fs;
#[cfg(not(unix))]
use std::fs::DirBuilder;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use uuid::Uuid;

const MAX_IMAGE_PIXELS: u64 = 40_000_000;
const MAX_IMAGE_BYTES: usize = 160 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum MediaCacheError {
    #[error("截图尺寸无效")]
    InvalidDimensions,
    #[error("截图超过系统安全限制")]
    ImageTooLarge,
    #[error("缓存路径包含不安全的符号链接或非目录项: {0}")]
    UnsafePath(PathBuf),
    #[error("缓存目标已存在: {0}")]
    AlreadyExists(PathBuf),
    #[error("写入 PNG 失败: {0}")]
    Io(#[from] io::Error),
    #[error("编码 PNG 失败: {0}")]
    Png(#[from] png::EncodingError),
}

/// 明文图片缓存的路径编排器。`data_dir` 是 SeaSnail 的受控应用数据根。
#[derive(Clone)]
pub struct MediaCache {
    data_dir: PathBuf,
}

impl MediaCache {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            data_dir: data_dir.as_ref().to_path_buf(),
        }
    }

    /// 将 RGBA8 图片安全地写成 PNG，返回可交给本机 Agent 的绝对路径。
    pub fn write_rgba_png(
        &self,
        account_id: &str,
        capture_id: &str,
        observed_at: DateTime<Utc>,
        event_id: &str,
        width: usize,
        height: usize,
        rgba: &[u8],
    ) -> Result<PathBuf, MediaCacheError> {
        validate_id(account_id)?;
        validate_id(capture_id)?;
        validate_id(event_id)?;
        validate_rgba(width, height, rgba)?;

        let date = observed_at.format("%Y-%m-%d").to_string();
        #[cfg(unix)]
        {
            return self
                .write_rgba_png_unix(account_id, capture_id, &date, event_id, width, height, rgba);
        }

        #[cfg(not(unix))]
        {
            let target_dir = self.private_image_directory(account_id, capture_id, &date)?;

            let final_path = target_dir.join(format!("{event_id}.png"));
            if fs::symlink_metadata(&final_path).is_ok() {
                return Err(MediaCacheError::AlreadyExists(final_path));
            }

            let tmp_path = target_dir.join(format!(".{event_id}.png.tmp"));
            let write_result = write_png(&tmp_path, width, height, rgba);
            if let Err(error) = write_result {
                let _ = fs::remove_file(&tmp_path);
                return Err(error);
            }

            // `hard_link` 是同一目录内的原子发布：若攻击者在检查后抢先创建最终文件，
            // 它会失败而不会覆盖目标（`rename` 在 Unix 上会覆盖现有文件）。
            match fs::hard_link(&tmp_path, &final_path) {
                Ok(()) => {
                    fs::remove_file(&tmp_path)?;
                    sync_directory(&target_dir)?;
                    Ok(final_path)
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let _ = fs::remove_file(&tmp_path);
                    Err(MediaCacheError::AlreadyExists(final_path))
                }
                Err(error) => {
                    let _ = fs::remove_file(&tmp_path);
                    Err(MediaCacheError::Io(error))
                }
            }
        }
    }

    #[cfg(unix)]
    fn write_rgba_png_unix(
        &self,
        account_id: &str,
        capture_id: &str,
        date: &str,
        event_id: &str,
        width: usize,
        height: usize,
        rgba: &[u8],
    ) -> Result<PathBuf, MediaCacheError> {
        let components = [
            "cache",
            "clipboard-context",
            account_id,
            capture_id,
            date,
            "image",
        ];
        let dir = open_private_dir_chain(&self.data_dir, &components)?;
        let temp = format!(".{event_id}.png.tmp");
        let final_name = format!("{event_id}.png");
        let file = open_new_file_at(&dir, &temp)?;
        if let Err(error) = write_png_file(file, width, height, rgba) {
            let _ = unlink_at(&dir, &temp);
            return Err(error);
        }
        match link_at(&dir, &temp, &final_name) {
            Ok(()) => {
                unlink_at(&dir, &temp)?;
                dir.sync_all()?;
                Ok(self
                    .data_dir
                    .join("cache")
                    .join("clipboard-context")
                    .join(account_id)
                    .join(capture_id)
                    .join(date)
                    .join("image")
                    .join(final_name))
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let _ = unlink_at(&dir, &temp);
                Err(MediaCacheError::AlreadyExists(
                    self.data_dir
                        .join("cache")
                        .join("clipboard-context")
                        .join(account_id)
                        .join(capture_id)
                        .join(date)
                        .join("image")
                        .join(final_name),
                ))
            }
            Err(error) => {
                let _ = unlink_at(&dir, &temp);
                Err(MediaCacheError::Io(error))
            }
        }
    }

    #[cfg(not(unix))]
    fn private_image_directory(
        &self,
        account_id: &str,
        capture_id: &str,
        date: &str,
    ) -> Result<PathBuf, MediaCacheError> {
        // 仅管理应用数据目录以下的专属子树。不得对 `/private`、用户目录或应用数据根
        // 本身重设权限；这样既避免越权，又能逐层拒绝 cache 子树中的符号链接。
        let mut current = self.data_dir.clone();
        for segment in [
            "cache",
            "clipboard-context",
            account_id,
            capture_id,
            date,
            "image",
        ] {
            current.push(segment);
            ensure_private_directory_component(&current)?;
        }
        Ok(current)
    }
}

#[cfg(unix)]
fn c_name(value: &str) -> Result<std::ffi::CString, MediaCacheError> {
    std::ffi::CString::new(value).map_err(|_| MediaCacheError::UnsafePath(PathBuf::from(value)))
}

#[cfg(unix)]
fn open_private_dir_chain(root: &Path, components: &[&str]) -> Result<File, MediaCacheError> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::OpenOptionsExt;
    let mut current = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(root)?;
    for component in components {
        let name = c_name(component)?;
        let mut fd = unsafe {
            libc::openat(
                current.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 && io::Error::last_os_error().kind() == io::ErrorKind::NotFound {
            let created = unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o700) };
            if created != 0 && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists {
                return Err(MediaCacheError::Io(io::Error::last_os_error()));
            }
            fd = unsafe {
                libc::openat(
                    current.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
        }
        if fd < 0 {
            let error = io::Error::last_os_error();
            return if matches!(error.raw_os_error(), Some(code) if code == libc::ELOOP || code == libc::ENOTDIR)
            {
                Err(MediaCacheError::UnsafePath(root.to_path_buf()))
            } else {
                Err(MediaCacheError::Io(error))
            };
        }
        current = unsafe { File::from_raw_fd(fd) };
        if unsafe { libc::fchmod(current.as_raw_fd(), 0o700) } != 0 {
            return Err(MediaCacheError::Io(io::Error::last_os_error()));
        }
    }
    Ok(current)
}

#[cfg(unix)]
fn open_new_file_at(dir: &File, name: &str) -> Result<File, MediaCacheError> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = c_name(name)?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if fd < 0 {
        return Err(MediaCacheError::Io(io::Error::last_os_error()));
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn unlink_at(dir: &File, name: &str) -> Result<(), io::Error> {
    use std::os::fd::AsRawFd;
    let name =
        std::ffi::CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    if unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn link_at(dir: &File, old: &str, new: &str) -> Result<(), io::Error> {
    use std::os::fd::AsRawFd;
    let old =
        std::ffi::CString::new(old).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let new =
        std::ffi::CString::new(new).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    if unsafe {
        libc::linkat(
            dir.as_raw_fd(),
            old.as_ptr(),
            dir.as_raw_fd(),
            new.as_ptr(),
            0,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn write_png_file(
    file: File,
    width: usize,
    height: usize,
    rgba: &[u8],
) -> Result<(), MediaCacheError> {
    let mut writer = BufWriter::new(file);
    let mut encoder = png::Encoder::new(&mut writer, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(rgba)?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(())
}

fn validate_id(value: &str) -> Result<(), MediaCacheError> {
    if Uuid::parse_str(value).is_ok() {
        Ok(())
    } else {
        Err(MediaCacheError::UnsafePath(PathBuf::from(value)))
    }
}

fn validate_rgba(width: usize, height: usize, rgba: &[u8]) -> Result<(), MediaCacheError> {
    let pixels = width
        .checked_mul(height)
        .ok_or(MediaCacheError::InvalidDimensions)?;
    let bytes = pixels
        .checked_mul(4)
        .ok_or(MediaCacheError::InvalidDimensions)?;
    if width == 0 || height == 0 || pixels as u64 > MAX_IMAGE_PIXELS || bytes > MAX_IMAGE_BYTES {
        return Err(MediaCacheError::ImageTooLarge);
    }
    if rgba.len() != bytes {
        return Err(MediaCacheError::InvalidDimensions);
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_directory_component(path: &Path) -> Result<(), MediaCacheError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(MediaCacheError::UnsafePath(path.to_path_buf()))
        }
        Ok(_) => set_private_permissions(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_private_directory(path)?;
            Ok(())
        }
        Err(error) => Err(MediaCacheError::Io(error)),
    }
}

#[cfg(not(unix))]
fn write_png(path: &Path, width: usize, height: usize, rgba: &[u8]) -> Result<(), MediaCacheError> {
    let file = create_private_file(path)?;
    let mut writer = BufWriter::new(file);
    {
        let mut encoder = png::Encoder::new(&mut writer, width as u32, height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut png_writer = encoder.write_header()?;
        png_writer.write_image_data(rgba)?;
    }
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn create_private_directory(path: &Path) -> Result<(), MediaCacheError> {
    fs::create_dir(path)?;
    set_private_permissions(path)
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> Result<File, MediaCacheError> {
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    set_private_permissions(path)?;
    Ok(file)
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<(), MediaCacheError> {
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), MediaCacheError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn ids() -> (String, String, String) {
        (
            Uuid::new_v4().to_string(),
            Uuid::new_v4().to_string(),
            Uuid::new_v4().to_string(),
        )
    }

    #[test]
    fn writes_private_png_in_the_expected_cache_layout() {
        let temp = tempfile::tempdir().unwrap();
        let cache = MediaCache::new(temp.path());
        let (account, capture, event) = ids();
        let path = cache
            .write_rgba_png(
                &account,
                &capture,
                DateTime::from_timestamp(1_723_801_600, 0).unwrap(),
                &event,
                1,
                1,
                &[12, 34, 56, 255],
            )
            .unwrap();

        assert!(path.ends_with(format!("{account}/{capture}/2024-08-16/image/{event}.png")));
        assert_eq!(&fs::read(&path).unwrap()[..8], b"\x89PNG\r\n\x1a\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn tightens_permissions_on_existing_cache_directories() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let cache_root = temp.path().join("cache/clipboard-context");
        fs::create_dir_all(&cache_root).unwrap();
        fs::set_permissions(&cache_root, fs::Permissions::from_mode(0o755)).unwrap();
        let (account, capture, event) = ids();
        MediaCache::new(temp.path())
            .write_rgba_png(
                &account,
                &capture,
                Utc::now(),
                &event,
                1,
                1,
                &[0, 0, 0, 255],
            )
            .unwrap();
        assert_eq!(
            fs::metadata(cache_root).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlink_in_the_cache_path() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), temp.path().join("cache")).unwrap();
        let cache = MediaCache::new(temp.path());
        let (account, capture, event) = ids();

        let error = cache
            .write_rgba_png(
                &account,
                &capture,
                Utc::now(),
                &event,
                1,
                1,
                &[0, 0, 0, 255],
            )
            .unwrap_err();
        assert!(matches!(error, MediaCacheError::UnsafePath(_)));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlink_at_the_account_component() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (account, capture, event) = ids();
        let parent = temp.path().join("cache").join("clipboard-context");
        fs::create_dir_all(&parent).unwrap();
        symlink(outside.path(), parent.join(&account)).unwrap();

        let error = MediaCache::new(temp.path())
            .write_rgba_png(
                &account,
                &capture,
                Utc::now(),
                &event,
                1,
                1,
                &[0, 0, 0, 255],
            )
            .unwrap_err();
        assert!(matches!(error, MediaCacheError::UnsafePath(_)));
    }
}
