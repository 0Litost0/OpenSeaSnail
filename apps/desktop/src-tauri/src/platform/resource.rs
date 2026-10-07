//! Validated context-resource and link opening adapter.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(crate) struct ResolvedResource {
    pub(crate) path: String,
    pub(crate) kind: String,
    pub(crate) size: u64,
    pub(crate) modified_unix_ms: Option<u128>,
    pub(crate) device: Option<u64>,
    pub(crate) inode: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResolvedLink {
    pub(crate) url: String,
}

pub(crate) fn validate_resolved_resource_identity(
    resource: &ResolvedResource,
    metadata: &std::fs::Metadata,
) -> Result<(), String> {
    if !metadata.file_type().is_file() || metadata.len() != resource.size {
        return Err("resource_changed".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if resource.device != Some(metadata.dev()) || resource.inode != Some(metadata.ino()) {
            return Err("resource_changed".into());
        }
    }
    if let Some(expected) = resource.modified_unix_ms {
        let actual = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis());
        if actual != Some(expected) {
            return Err("resource_changed".into());
        }
    }
    if resource.kind != "file" && resource.kind != "image" {
        return Err("resource_unsafe".into());
    }
    Ok(())
}

pub(crate) fn open_validated_resource<F>(
    resource: &ResolvedResource,
    path: &std::path::Path,
    opener: F,
) -> Result<bool, String>
where
    F: FnOnce(&str, bool) -> Result<bool, String>,
{
    let raw = path.to_str().ok_or_else(|| "resource_unsafe".to_string())?;
    let (canonical, metadata) =
        seasnail_daemon::resource::validate_openable_file(raw).map_err(|error| match error {
            seasnail_daemon::resource::ResourceError::Unavailable => "resource_unavailable",
            seasnail_daemon::resource::ResourceError::Unsafe
            | seasnail_daemon::resource::ResourceError::BudgetExceeded => "resource_unsafe",
        })?;
    if canonical != path {
        return Err("resource_changed".into());
    }
    validate_resolved_resource_identity(resource, &metadata)?;
    opener(canonical.to_string_lossy().as_ref(), true)
}

pub(crate) fn validate_http_link(url: &str) -> Result<(), String> {
    let authority = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| "unsupported_scheme".to_string())?;
    if authority.is_empty() || authority.starts_with('/') {
        return Err("unsupported_scheme".into());
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| "unsupported_scheme".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("unsupported_scheme".into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn open_with_default_application(target: &str, is_file: bool) -> Result<bool, String> {
    crate::platform::macos::open_with_default_application(target, is_file)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn open_with_default_application(_target: &str, _is_file: bool) -> Result<bool, String> {
    Err("unsupported_platform".into())
}
