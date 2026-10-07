//! SenseVoice GGUF runtime manifest 的离线解析与完整性预检。
//!
//! 只接受 App/开发者明确提供的资源根；绝不下载、跟随 manifest 路径或猜测其他 backend。

use crate::{ArtifactIdentity, RuntimeKind, VerifiedArtifact, VerifiedArtifactFile};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use thiserror::Error;

const MANIFEST: &str = "runtime-manifest.json";
const SERVER: &str = "sensevoice-server";
const MODEL: &str = "sensevoice.gguf";
const VAD: &str = "fsmn-vad.gguf";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GgufResources {
    pub root: PathBuf,
    pub model_id: String,
    pub variant: GgufVariant,
    pub model_size_bytes: u64,
    pub model_sha256: String,
    pub manifest_sha256: String,
    pub source_revision: String,
    pub server_sha256: String,
    pub server_size_bytes: u64,
    pub vad_sha256: String,
    pub vad_size_bytes: u64,
    pub server: PathBuf,
    pub model: PathBuf,
    pub vad: PathBuf,
}

/// 旧版 `runtime-manifest.json` 的预检结果。它明确不是 catalog-signed
/// `VerifiedRuntimeResources`，避免 legacy 路径宣称具备新 manifest 的完整性保证。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreflightedLegacyResources(pub GgufResources);

impl PreflightedLegacyResources {
    pub(crate) fn into_resources(self) -> GgufResources {
        self.0
    }
}

pub(crate) struct LegacyArtifactResolver;

impl LegacyArtifactResolver {
    pub(crate) fn preflight(root: &Path) -> Result<PreflightedLegacyResources, GgufManifestError> {
        verify_gguf_resources(root).map(PreflightedLegacyResources)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GgufVariant {
    Q8,
    F16,
    F32,
}

impl GgufVariant {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Q8 => "q8",
            Self::F16 => "f16",
            Self::F32 => "f32",
        }
    }

    fn parse(value: &str) -> Result<Self, GgufManifestError> {
        match value {
            "q8" => Ok(Self::Q8),
            "f16" => Ok(Self::F16),
            "f32" => Ok(Self::F32),
            _ => Err(GgufManifestError::Invalid(format!(
                "unsupported GGUF variant {value:?}"
            ))),
        }
    }
}

#[derive(Debug, Error)]
pub enum GgufManifestError {
    #[error("GGUF manifest I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("GGUF manifest JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid GGUF manifest: {0}")]
    Invalid(String),
    #[error("GGUF resource {name} failed verification: {reason}")]
    Verification { name: &'static str, reason: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    runtime: String,
    model_id: String,
    variant: String,
    model_file: String,
    vad_file: String,
    source_revision: String,
    api_contract_version: u32,
    files: Vec<ManifestFile>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    path: String,
    sha256: String,
    size_bytes: u64,
}

/// 解析 `root/runtime-manifest.json`，验证 manifest 中三项规范资源的大小、SHA-256
/// 及 server 可执行权限，并返回 canonical 的固定路径。
///
/// 发布资源由 App 签名和只读 bundle 保证不可变；开发资源根禁止符号链接。调用方在
/// 启动进程前必须重新调用本函数，避免把一次预检误当成可变开发目录的永久授权。
pub fn verify_gguf_resources(root: &Path) -> Result<GgufResources, GgufManifestError> {
    if !root.is_absolute() {
        return Err(GgufManifestError::Invalid(
            "GGUF root must be an absolute canonical path".into(),
        ));
    }
    let root_meta = fs::symlink_metadata(root)?;
    if root_meta.file_type().is_symlink() || !root_meta.is_dir() {
        return Err(GgufManifestError::Invalid(
            "GGUF root must be a non-symlink directory".into(),
        ));
    }
    let canonical_root = root.canonicalize()?;
    if canonical_root != root {
        return Err(GgufManifestError::Invalid(
            "GGUF root and all of its ancestors must be canonical (no symlinks)".into(),
        ));
    }
    let root = canonical_root;
    let manifest_path = root.join(MANIFEST);
    let manifest_meta = fs::symlink_metadata(&manifest_path)?;
    if manifest_meta.file_type().is_symlink() || !manifest_meta.is_file() {
        return Err(GgufManifestError::Invalid(
            "runtime-manifest.json must be a regular file".into(),
        ));
    }
    let manifest_bytes = fs::read(&manifest_path)?;
    let manifest_sha256 = format!("{:x}", Sha256::digest(&manifest_bytes));
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    if manifest.schema_version != 1 || manifest.api_contract_version != 1 {
        return Err(GgufManifestError::Invalid(
            "unsupported manifest or API contract version".into(),
        ));
    }
    if manifest
        .runtime
        .parse::<RuntimeKind>()
        .map_err(|e| GgufManifestError::Invalid(e.to_string()))?
        != RuntimeKind::Gguf
    {
        return Err(GgufManifestError::Invalid(
            "runtime must be \"gguf\"".into(),
        ));
    }
    if manifest.model_id != "sensevoice-small"
        || manifest.model_file != MODEL
        || manifest.vad_file != VAD
        || !is_revision(&manifest.source_revision)
    {
        return Err(GgufManifestError::Invalid(
            "unexpected SenseVoice GGUF identity or canonical file names".into(),
        ));
    }
    let variant = GgufVariant::parse(&manifest.variant)?;
    let server_entry = manifest
        .files
        .iter()
        .find(|file| file.path == SERVER)
        .ok_or_else(|| GgufManifestError::Invalid(format!("files missing {SERVER}")))?;
    let server_sha256 = server_entry.sha256.clone();
    let server_size_bytes = server_entry.size_bytes;
    let server = verify_file(&root, &manifest.files, SERVER, true)?;
    let model_entry = manifest
        .files
        .iter()
        .find(|file| file.path == MODEL)
        .ok_or_else(|| GgufManifestError::Invalid(format!("files missing {MODEL}")))?;
    let model_size_bytes = model_entry.size_bytes;
    let model_sha256 = model_entry.sha256.clone();
    let model = verify_file(&root, &manifest.files, MODEL, false)?;
    let vad_entry = manifest
        .files
        .iter()
        .find(|file| file.path == VAD)
        .ok_or_else(|| GgufManifestError::Invalid(format!("files missing {VAD}")))?;
    let vad_sha256 = vad_entry.sha256.clone();
    let vad_size_bytes = vad_entry.size_bytes;
    let vad = verify_file(&root, &manifest.files, VAD, false)?;
    if manifest.files.len() != 3 {
        return Err(GgufManifestError::Invalid(
            "files must contain exactly server, model and VAD".into(),
        ));
    }
    Ok(GgufResources {
        root,
        model_id: manifest.model_id,
        variant,
        model_size_bytes,
        model_sha256,
        manifest_sha256,
        source_revision: manifest.source_revision,
        server_sha256,
        server_size_bytes,
        vad_sha256,
        vad_size_bytes,
        server,
        model,
        vad,
    })
}

/// Compatibility adapter for the historical GGUF `runtime-manifest.json`.
///
/// The old manifest has already been fully checked by [`verify_gguf_resources`].
/// This function maps its historical `model_id` to the catalog ID supplied by
/// the daemon and exposes the result through the backend-neutral artifact type.
/// New artifacts must use `artifact-manifest.json` and a catalog trust anchor.
pub fn verify_legacy_gguf_artifact(
    root: &Path,
    catalog_id: &str,
) -> Result<VerifiedArtifact, GgufManifestError> {
    if catalog_id.is_empty()
        || !catalog_id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
    {
        return Err(GgufManifestError::Invalid(
            "legacy catalog ID is invalid".into(),
        ));
    }
    let resources = LegacyArtifactResolver::preflight(root)?.into_resources();
    let files = BTreeMap::from([
        (
            "sidecar".into(),
            VerifiedArtifactFile {
                role: "sidecar".into(),
                path: resources.server.clone(),
                sha256: resources.server_sha256.clone(),
                size_bytes: resources.server_size_bytes,
                executable: true,
            },
        ),
        (
            "model".into(),
            VerifiedArtifactFile {
                role: "model".into(),
                path: resources.model.clone(),
                sha256: resources.model_sha256.clone(),
                size_bytes: resources.model_size_bytes,
                executable: false,
            },
        ),
        (
            "vad".into(),
            VerifiedArtifactFile {
                role: "vad".into(),
                path: resources.vad.clone(),
                sha256: resources.vad_sha256.clone(),
                size_bytes: resources.vad_size_bytes,
                executable: false,
            },
        ),
    ]);
    Ok(VerifiedArtifact {
        root: resources.root,
        identity: ArtifactIdentity {
            runtime: "gguf".into(),
            catalog_id: catalog_id.into(),
            family: resources.model_id,
            variant: resources.variant.as_str().into(),
            api_contract_version: 1,
        },
        manifest_sha256: resources.manifest_sha256,
        source_revisions: BTreeMap::from([("sensevoice_gguf".into(), resources.source_revision)]),
        size_bytes: files.values().map(|file| file.size_bytes).sum(),
        files,
    })
}

fn verify_file(
    root: &Path,
    files: &[ManifestFile],
    name: &'static str,
    executable: bool,
) -> Result<PathBuf, GgufManifestError> {
    let entry = files
        .iter()
        .find(|file| file.path == name)
        .ok_or_else(|| GgufManifestError::Invalid(format!("files missing {name}")))?;
    if files.iter().filter(|file| file.path == name).count() != 1 || !valid_hex(&entry.sha256) {
        return Err(GgufManifestError::Invalid(format!(
            "invalid entry for {name}"
        )));
    }
    let path = root.join(name);
    let meta = fs::symlink_metadata(&path)?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(GgufManifestError::Verification {
            name,
            reason: "not a regular file".into(),
        });
    }
    if meta.len() != entry.size_bytes {
        return Err(GgufManifestError::Verification {
            name,
            reason: format!("size {} != {}", meta.len(), entry.size_bytes),
        });
    }
    #[cfg(unix)]
    if executable && (std::os::unix::fs::MetadataExt::mode(&meta) & 0o111 == 0) {
        return Err(GgufManifestError::Verification {
            name,
            reason: "not executable".into(),
        });
    }
    let mut file = fs::File::open(&path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    let actual = format!("{:x}", hash.finalize());
    if actual != entry.sha256 {
        return Err(GgufManifestError::Verification {
            name,
            reason: "SHA-256 mismatch".into(),
        });
    }
    Ok(path)
}
fn valid_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
fn is_revision(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sha(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let entries = [
            (SERVER, b"server".as_slice()),
            (MODEL, b"model".as_slice()),
            (VAD, b"vad".as_slice()),
        ];
        for (name, bytes) in entries {
            fs::write(dir.path().join(name), bytes).unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir.path().join(SERVER), fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let files: Vec<_> = entries.iter().map(|(name, bytes)| serde_json::json!({"path":name,"sha256":sha(bytes),"size_bytes":bytes.len()})).collect();
        fs::write(dir.path().join(MANIFEST), serde_json::json!({"schema_version":1,"runtime":"gguf","model_id":"sensevoice-small","variant":"q8","model_file":MODEL,"vad_file":VAD,"source_revision":"6991744856587fa44379e8b5dcc432debffeb1be","api_contract_version":1,"files":files}).to_string()).unwrap();
        dir
    }
    fn canonical(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().canonicalize().unwrap()
    }
    #[test]
    fn accepts_complete_fixed_layout() {
        let dir = fixture();
        let got = verify_gguf_resources(&canonical(&dir)).unwrap();
        assert_eq!(got.variant, GgufVariant::Q8);
    }
    #[test]
    fn wraps_legacy_manifest_as_generic_artifact() {
        let dir = fixture();
        let got = verify_legacy_gguf_artifact(&canonical(&dir), "sensevoice-small").unwrap();
        assert_eq!(got.identity.catalog_id, "sensevoice-small");
        assert_eq!(got.identity.family, "sensevoice-small");
        assert_eq!(got.identity.variant, "q8");
        assert_eq!(got.size_bytes, 14);
        assert!(got.file("sidecar").is_some());
    }
    #[test]
    fn rejects_missing_file() {
        let dir = fixture();
        fs::remove_file(dir.path().join(VAD)).unwrap();
        assert!(verify_gguf_resources(&canonical(&dir)).is_err());
    }
    #[test]
    fn rejects_size_mismatch() {
        let dir = fixture();
        fs::write(dir.path().join(MODEL), b"bad-model").unwrap();
        assert!(matches!(
            verify_gguf_resources(&canonical(&dir)),
            Err(GgufManifestError::Verification { .. })
        ));
    }
    #[test]
    fn rejects_same_size_hash_mismatch() {
        let dir = fixture();
        fs::write(dir.path().join(MODEL), b"xxxxx").unwrap();
        assert!(matches!(
            verify_gguf_resources(&canonical(&dir)),
            Err(GgufManifestError::Verification { .. })
        ));
    }
    #[cfg(unix)]
    #[test]
    fn rejects_non_executable_server() {
        use std::os::unix::fs::PermissionsExt;
        let dir = fixture();
        fs::set_permissions(dir.path().join(SERVER), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            verify_gguf_resources(&canonical(&dir)),
            Err(GgufManifestError::Verification { .. })
        ));
    }
    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_root_and_resource() {
        use std::os::unix::fs::symlink;
        let dir = fixture();
        let parent = tempfile::tempdir().unwrap();
        let root_link = parent.path().join("root");
        symlink(dir.path(), &root_link).unwrap();
        assert!(verify_gguf_resources(&root_link).is_err());
        fs::remove_file(dir.path().join(MODEL)).unwrap();
        symlink("/dev/null", dir.path().join(MODEL)).unwrap();
        assert!(verify_gguf_resources(&canonical(&dir)).is_err());
    }
}
