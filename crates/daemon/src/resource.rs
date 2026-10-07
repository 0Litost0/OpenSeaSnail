//! 剪贴板资源解析与打开前的基础安全校验。

use std::path::{Path, PathBuf};

pub const THUMBNAIL_MAX_DIM: u32 = 320;
pub const THUMBNAIL_MAX_OUTPUT_BYTES: usize = 512 * 1024;
pub const THUMBNAIL_MAX_INPUT_BYTES: u64 = 32 * 1024 * 1024;
pub const THUMBNAIL_MAX_PIXELS: u64 = 16_000_000;
pub const THUMBNAIL_MAX_DECODE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceError {
    Unavailable,
    Unsafe,
    BudgetExceeded,
}

/// 校验资源是可由默认应用打开的普通文件。
///
/// 不接受目录、任意符号链接、App bundle、脚本和带执行位的文件；同时要求路径
/// 已规范化，避免父级符号链接在检查后把访问带出原始位置。
pub fn validate_openable_file(raw: &str) -> Result<(PathBuf, std::fs::Metadata), ResourceError> {
    let path = Path::new(raw);
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(ResourceError::Unsafe);
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|_| ResourceError::Unavailable)?;
    if !metadata.file_type().is_file() {
        return Err(ResourceError::Unsafe);
    }
    let mut parent = path.parent();
    while let Some(component) = parent {
        let info = std::fs::symlink_metadata(component).map_err(|_| ResourceError::Unavailable)?;
        if info.file_type().is_symlink() || !info.is_dir() {
            return Err(ResourceError::Unsafe);
        }
        parent = component.parent();
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| ResourceError::Unavailable)?;
    validate_file_policy(path, &metadata)?;
    Ok((canonical, metadata))
}

fn validate_file_policy(path: &Path, metadata: &std::fs::Metadata) -> Result<(), ResourceError> {
    if path.components().any(|component| {
        let name = component.as_os_str().to_string_lossy().to_ascii_lowercase();
        name.ends_with(".app")
            || name.ends_with(".command")
            || name.ends_with(".sh")
            || name.ends_with(".bash")
            || name.ends_with(".zsh")
            || name.ends_with(".js")
            || name.ends_with(".py")
            || name.ends_with(".scpt")
    }) {
        return Err(ResourceError::Unsafe);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 != 0 {
            return Err(ResourceError::Unsafe);
        }
    }
    Ok(())
}

/// 私有缓存资源的额外边界：固定在 `cache/clipboard-context/...` 根下，且每一级
/// 已存在路径都必须是目录而非符号链接。
pub fn validate_private_cache_file(
    raw: &str,
    cache_root: &Path,
) -> Result<(PathBuf, std::fs::Metadata), ResourceError> {
    // Do not canonicalize the candidate before walking it: canonicalization would
    // erase a parent symlink and make the component-by-component check ineffective.
    let path = Path::new(raw);
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(ResourceError::Unsafe);
    }
    let lexical_root = cache_root.to_path_buf();
    let root = cache_root
        .canonicalize()
        .map_err(|_| ResourceError::Unavailable)?;
    if !path.starts_with(&lexical_root) {
        return Err(ResourceError::Unsafe);
    }
    let relative = path
        .strip_prefix(&lexical_root)
        .map_err(|_| ResourceError::Unsafe)?;
    let mut current = lexical_root;
    let components: Vec<_> = relative.components().collect();
    for (position, component) in components.iter().enumerate() {
        current.push(component.as_os_str());
        let info = std::fs::symlink_metadata(&current).map_err(|_| ResourceError::Unavailable)?;
        if info.file_type().is_symlink() {
            return Err(ResourceError::Unsafe);
        }
        if position + 1 < components.len() && !info.is_dir() {
            return Err(ResourceError::Unsafe);
        }
    }
    // Reuse the ordinary file policy only after the raw path's parents have been
    // verified. It is safe for this second pass to canonicalize the path.
    let metadata = std::fs::symlink_metadata(path).map_err(|_| ResourceError::Unavailable)?;
    if !metadata.file_type().is_file() {
        return Err(ResourceError::Unsafe);
    }
    validate_file_policy(path, &metadata)?;
    let canonical = path
        .canonicalize()
        .map_err(|_| ResourceError::Unavailable)?;
    if !canonical.starts_with(&root) {
        return Err(ResourceError::Unsafe);
    }
    Ok((canonical, metadata))
}

/// Decode a PNG image under bounded input/geometry budgets and emit a PNG thumbnail.
pub fn thumbnail_png(path: &Path) -> Result<(Vec<u8>, u32, u32), ResourceError> {
    let metadata = std::fs::metadata(path).map_err(|_| ResourceError::Unavailable)?;
    if metadata.len() > THUMBNAIL_MAX_INPUT_BYTES {
        tracing::warn!(
            reason = "input_bytes",
            actual = metadata.len(),
            limit = THUMBNAIL_MAX_INPUT_BYTES,
            "thumbnail budget exceeded"
        );
        return Err(ResourceError::BudgetExceeded);
    }
    let bytes = std::fs::read(path).map_err(|_| ResourceError::Unavailable)?;
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|_| ResourceError::Unsafe)?;
    let header = reader.info();
    let pixels = u64::from(header.width).saturating_mul(u64::from(header.height));
    if header.width == 0 || header.height == 0 {
        return Err(ResourceError::Unsafe);
    }
    if pixels > THUMBNAIL_MAX_PIXELS {
        tracing::warn!(
            reason = "pixels",
            width = header.width,
            height = header.height,
            actual = pixels,
            limit = THUMBNAIL_MAX_PIXELS,
            "thumbnail budget exceeded"
        );
        return Err(ResourceError::BudgetExceeded);
    }
    let decode_bytes = reader.output_buffer_size().ok_or_else(|| {
        tracing::warn!(reason = "decode_size_overflow", "thumbnail budget exceeded");
        ResourceError::BudgetExceeded
    })?;
    if decode_bytes > THUMBNAIL_MAX_DECODE_BYTES {
        tracing::warn!(
            reason = "decode_bytes",
            actual = decode_bytes,
            limit = THUMBNAIL_MAX_DECODE_BYTES,
            "thumbnail budget exceeded"
        );
        return Err(ResourceError::BudgetExceeded);
    }
    let mut input = vec![0; decode_bytes];
    let info = reader
        .next_frame(&mut input)
        .map_err(|_| ResourceError::Unsafe)?;
    let scale = (THUMBNAIL_MAX_DIM as f32 / info.width as f32)
        .min(THUMBNAIL_MAX_DIM as f32 / info.height as f32)
        .min(1.0);
    let width = ((info.width as f32 * scale).round() as u32).max(1);
    let height = ((info.height as f32 * scale).round() as u32).max(1);
    let source = &input[..info.buffer_size()];
    let pixel_count = usize::try_from(u64::from(info.width) * u64::from(info.height))
        .map_err(|_| ResourceError::BudgetExceeded)?;
    let mut rgba = Vec::with_capacity(
        pixel_count
            .checked_mul(4)
            .ok_or(ResourceError::BudgetExceeded)?,
    );
    match info.color_type {
        png::ColorType::Rgba => rgba.extend_from_slice(source),
        png::ColorType::Rgb => {
            for pixel in source.chunks_exact(3) {
                rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
            }
        }
        png::ColorType::Grayscale => {
            for value in source {
                rgba.extend_from_slice(&[*value, *value, *value, 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for pixel in source.chunks_exact(2) {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
            }
        }
        png::ColorType::Indexed => return Err(ResourceError::Unsafe),
    }
    let mut scaled = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let sx = x * info.width / width;
            let sy = y * info.height / height;
            let src = ((sy * info.width + sx) * 4) as usize;
            let dst = ((y * width + x) * 4) as usize;
            scaled[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    let mut output = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut output, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .map_err(|_| ResourceError::Unavailable)?
            .write_image_data(&scaled)
            .map_err(|_| ResourceError::Unavailable)?;
    }
    if output.len() > THUMBNAIL_MAX_OUTPUT_BYTES {
        tracing::warn!(
            reason = "output_bytes",
            actual = output.len(),
            limit = THUMBNAIL_MAX_OUTPUT_BYTES,
            "thumbnail budget exceeded"
        );
        return Err(ResourceError::BudgetExceeded);
    }
    Ok((output, width, height))
}

#[cfg(test)]
mod tests {
    use super::{validate_openable_file, validate_private_cache_file, ResourceError};

    #[test]
    fn accepts_regular_non_executable_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("note.txt");
        std::fs::write(&file, b"ok").unwrap();
        let file = file.canonicalize().unwrap();
        assert!(validate_openable_file(file.to_str().unwrap()).is_ok());
    }

    #[test]
    fn rejects_directory_script_and_parent_escape() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("run.sh");
        std::fs::write(&script, b"echo no").unwrap();
        assert!(matches!(
            validate_openable_file(dir.path().to_str().unwrap()),
            Err(ResourceError::Unsafe)
        ));
        assert!(matches!(
            validate_openable_file(script.to_str().unwrap()),
            Err(ResourceError::Unsafe)
        ));
        assert!(matches!(
            validate_openable_file(&format!("{}/../x", dir.path().display())),
            Err(ResourceError::Unsafe)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_and_executable_file() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        let link = dir.path().join("link.txt");
        std::fs::write(&target, b"ok").unwrap();
        symlink(&target, &link).unwrap();
        assert!(matches!(
            validate_openable_file(link.to_str().unwrap()),
            Err(ResourceError::Unsafe)
        ));
        let real_parent = dir.path().join("real-parent");
        let linked_parent = dir.path().join("linked-parent");
        std::fs::create_dir(&real_parent).unwrap();
        let nested = real_parent.join("note.txt");
        std::fs::write(&nested, b"ok").unwrap();
        symlink(&real_parent, &linked_parent).unwrap();
        assert!(matches!(
            validate_openable_file(linked_parent.join("note.txt").to_str().unwrap()),
            Err(ResourceError::Unsafe)
        ));
        let executable = dir.path().join("run.bin");
        std::fs::write(&executable, b"x").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(
            validate_openable_file(executable.to_str().unwrap()),
            Err(ResourceError::Unsafe)
        ));
    }

    #[test]
    fn private_cache_rejects_outside_and_accepts_nested_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cache");
        let nested = root.join("acct/capture/date/image");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("1.png");
        std::fs::write(&file, b"png").unwrap();
        assert!(validate_private_cache_file(file.to_str().unwrap(), &root).is_ok());
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, b"no").unwrap();
        assert!(matches!(
            validate_private_cache_file(outside.to_str().unwrap(), &root),
            Err(ResourceError::Unsafe)
        ));
    }

    #[test]
    fn thumbnail_is_bounded_and_scaled() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("image.png");
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 640, 320);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&vec![255; 640 * 320 * 4]).unwrap();
        }
        std::fs::write(&file, bytes).unwrap();
        let (thumb, width, height) = super::thumbnail_png(&file).unwrap();
        assert_eq!((width, height), (320, 160));
        assert!(thumb.len() <= super::THUMBNAIL_MAX_OUTPUT_BYTES);
    }

    #[test]
    fn thumbnail_accepts_rgb_png() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rgb.png");
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 2, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[255, 0, 0, 0, 255, 0])
                .unwrap();
        }
        std::fs::write(&file, bytes).unwrap();
        assert!(super::thumbnail_png(&file).is_ok());
    }

    #[test]
    fn oversized_thumbnail_input_reports_budget_exceeded() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("large.png");
        let output = std::fs::File::create(&file).unwrap();
        output
            .set_len(super::THUMBNAIL_MAX_INPUT_BYTES + 1)
            .unwrap();
        assert!(matches!(
            super::thumbnail_png(&file),
            Err(ResourceError::BudgetExceeded)
        ));
    }
}
