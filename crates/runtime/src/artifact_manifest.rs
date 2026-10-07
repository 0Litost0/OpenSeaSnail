//! Backend-neutral artifact manifest parsing and verification.
//!
//! The catalog supplies the expected manifest SHA-256 and logical identity.
//! Verification authenticates the manifest bytes before parsing any path from
//! it, then verifies every declared file without following symbolic links.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use thiserror::Error;

pub const ARTIFACT_MANIFEST_FILE: &str = "artifact-manifest.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactIdentity {
    pub runtime: String,
    pub catalog_id: String,
    pub family: String,
    pub variant: String,
    pub api_contract_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRequirements {
    pub identity: ArtifactIdentity,
    pub manifest_sha256: String,
    /// Exact role set. The boolean says whether the corresponding file must
    /// have at least one executable bit on Unix.
    pub roles: BTreeMap<String, bool>,
    /// Native Mach-O roles and their required single architecture.
    pub architectures: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedArtifactFile {
    pub role: String,
    pub path: PathBuf,
    pub sha256: String,
    pub size_bytes: u64,
    pub executable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedArtifact {
    pub root: PathBuf,
    pub identity: ArtifactIdentity,
    pub manifest_sha256: String,
    pub source_revisions: BTreeMap<String, String>,
    pub files: BTreeMap<String, VerifiedArtifactFile>,
    pub size_bytes: u64,
}

/// 已通过 catalog 信任锚点和逐文件校验的 runtime 资源。
///
/// 该 newtype 用于阻止 factory 接收未经验证的裸目录；构造只能由
/// [`ArtifactResolver`] 完成。资源本身仍是不可变描述，启动前 backend 还会再次校验。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRuntimeResources(VerifiedArtifact);

impl VerifiedRuntimeResources {
    pub fn artifact(&self) -> &VerifiedArtifact {
        &self.0
    }
    pub fn root(&self) -> &Path {
        &self.0.root
    }
    pub fn file(&self, role: &str) -> Option<&Path> {
        self.0.file(role)
    }
    pub fn into_artifact(self) -> VerifiedArtifact {
        self.0
    }
}

/// 生产 catalog 的最小解析器：catalog 只保存可信要求，不接受 manifest 自报的身份。
#[derive(Debug, Clone, Default)]
pub struct ArtifactCatalog {
    entries: BTreeMap<String, ArtifactRequirements>,
}

impl ArtifactCatalog {
    pub fn new(entries: impl IntoIterator<Item = (String, ArtifactRequirements)>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
        }
    }
    pub fn requirements(&self, catalog_id: &str) -> Option<&ArtifactRequirements> {
        self.entries.get(catalog_id)
    }
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }
}

/// 从 catalog 条目解析并完整验证资源。没有“尝试安装/下载”或路径猜测。
#[derive(Debug, Clone)]
pub struct ArtifactResolver {
    catalog: ArtifactCatalog,
}

impl ArtifactResolver {
    pub fn new(catalog: ArtifactCatalog) -> Self {
        Self { catalog }
    }
    pub fn resolve(
        &self,
        catalog_id: &str,
        root: &Path,
    ) -> Result<VerifiedRuntimeResources, ArtifactManifestError> {
        let requirements = self.catalog.requirements(catalog_id).ok_or_else(|| {
            ArtifactManifestError::Invalid(format!("unknown artifact catalog id {catalog_id:?}"))
        })?;
        verify_artifact(root, requirements).map(VerifiedRuntimeResources)
    }
}

/// runtime factory 只接受已验证资源，禁止以裸路径构造生产 runtime。
pub trait ArtifactFactory<R> {
    type Error;
    fn build(&self, resources: VerifiedRuntimeResources) -> Result<R, Self::Error>;
}

/// Cache used only by read-only installed-state listing. Activation and every
/// sidecar (re)start must call [`verify_artifact`] directly.
#[derive(Debug, Default)]
pub struct ArtifactVerificationCache {
    entries: Mutex<BTreeMap<PathBuf, CachedArtifact>>,
}

#[derive(Debug, Clone)]
struct CachedArtifact {
    requirements: ArtifactRequirements,
    artifact: VerifiedArtifact,
    fingerprint: ArtifactFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArtifactFingerprint {
    manifest: FileFingerprint,
    files: BTreeMap<String, FileFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileFingerprint {
    size_bytes: u64,
    modified_nanos: u128,
    #[cfg(unix)]
    changed_nanos: i128,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    mode: u32,
}

impl VerifiedArtifact {
    pub fn file(&self, role: &str) -> Option<&Path> {
        self.files.get(role).map(|file| file.path.as_path())
    }
}

impl ArtifactVerificationCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Verify for `GET /models`-style installed-state checks. A cached success
    /// is reused only while the trusted requirements and metadata fingerprint
    /// of the manifest and every verified file remain unchanged.
    pub fn verify_for_listing(
        &self,
        root: &Path,
        requirements: &ArtifactRequirements,
    ) -> Result<VerifiedArtifact, ArtifactManifestError> {
        if let Some(cached) = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(root)
            .cloned()
        {
            if cached.requirements == *requirements
                && fingerprint(&cached.artifact).is_some_and(|value| value == cached.fingerprint)
            {
                return Ok(cached.artifact);
            }
        }

        let artifact = verify_artifact(root, requirements)?;
        let fingerprint = fingerprint(&artifact).ok_or_else(|| {
            ArtifactManifestError::Invalid("artifact metadata changed during verification".into())
        })?;
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                artifact.root.clone(),
                CachedArtifact {
                    requirements: requirements.clone(),
                    artifact: artifact.clone(),
                    fingerprint,
                },
            );
        Ok(artifact)
    }

    pub fn invalidate(&self, root: &Path) {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(root);
    }

    pub fn clear(&self) {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }
}

#[derive(Debug, Error)]
pub enum ArtifactManifestError {
    #[error("artifact root is not a canonical non-symlink directory")]
    InvalidRoot,
    #[error("artifact manifest is not a regular non-symlink file")]
    InvalidManifestFile,
    #[error("artifact manifest I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("artifact manifest SHA-256 does not match the catalog trust anchor")]
    ManifestHashMismatch,
    #[error("artifact manifest JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("artifact manifest is invalid: {0}")]
    Invalid(String),
    #[error("artifact role {role:?} failed verification: {reason}")]
    Verification { role: String, reason: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    runtime: String,
    catalog_id: String,
    family: String,
    variant: String,
    api_contract_version: u32,
    source_revisions: BTreeMap<String, String>,
    files: Vec<ManifestFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    role: String,
    path: String,
    sha256: String,
    size_bytes: u64,
    executable: bool,
    #[serde(default)]
    architecture: Option<String>,
}

pub fn verify_artifact(
    root: &Path,
    requirements: &ArtifactRequirements,
) -> Result<VerifiedArtifact, ArtifactManifestError> {
    validate_requirements(requirements)?;
    let root = canonical_artifact_root(root)?;
    let manifest_path = root.join(ARTIFACT_MANIFEST_FILE);
    let manifest_metadata =
        fs::symlink_metadata(&manifest_path).map_err(ArtifactManifestError::Io)?;
    if manifest_metadata.file_type().is_symlink() || !manifest_metadata.is_file() {
        return Err(ArtifactManifestError::InvalidManifestFile);
    }

    let manifest_bytes = fs::read(&manifest_path)?;
    let manifest_sha256 = sha256_bytes(&manifest_bytes);
    if manifest_sha256 != requirements.manifest_sha256 {
        return Err(ArtifactManifestError::ManifestHashMismatch);
    }
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    validate_manifest_identity(&manifest, requirements)?;

    let declared_roles: BTreeSet<_> = manifest.files.iter().map(|file| &file.role).collect();
    if declared_roles.len() != manifest.files.len() {
        return Err(ArtifactManifestError::Invalid(
            "file roles must be unique".into(),
        ));
    }
    let expected_roles: BTreeSet<_> = requirements.roles.keys().collect();
    if declared_roles != expected_roles {
        return Err(ArtifactManifestError::Invalid(
            "file roles do not exactly match backend requirements".into(),
        ));
    }

    let mut declared_paths = BTreeSet::new();
    let mut verified_files = BTreeMap::new();
    let mut total_size = 0_u64;
    for entry in manifest.files {
        if !valid_identifier(&entry.role) || !safe_relative_path(&entry.path) {
            return Err(ArtifactManifestError::Invalid(format!(
                "role {:?} has an invalid role or relative path",
                entry.role
            )));
        }
        if !declared_paths.insert(entry.path.clone()) {
            return Err(ArtifactManifestError::Invalid(
                "file paths must be unique".into(),
            ));
        }
        if !valid_sha256(&entry.sha256) || entry.size_bytes == 0 {
            return Err(ArtifactManifestError::Invalid(format!(
                "role {:?} has an invalid hash or size",
                entry.role
            )));
        }
        let executable_required = requirements.roles[&entry.role];
        if entry.executable != executable_required {
            return Err(ArtifactManifestError::Invalid(format!(
                "role {:?} has an unexpected executable requirement",
                entry.role
            )));
        }
        let expected_architecture = requirements.architectures.get(&entry.role);
        if entry.architecture.as_ref() != expected_architecture {
            return Err(ArtifactManifestError::Invalid(format!(
                "role {:?} has an unexpected architecture declaration",
                entry.role
            )));
        }
        let path = root.join(&entry.path);
        verify_path_components(&root, Path::new(&entry.path), &entry.role)?;
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| ArtifactManifestError::Verification {
                role: entry.role.clone(),
                reason: error.kind().to_string(),
            })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(ArtifactManifestError::Verification {
                role: entry.role,
                reason: "not a regular non-symlink file".into(),
            });
        }
        if metadata.len() != entry.size_bytes {
            return Err(ArtifactManifestError::Verification {
                role: entry.role,
                reason: "size mismatch".into(),
            });
        }
        #[cfg(unix)]
        if executable_required {
            use std::os::unix::fs::MetadataExt;
            if metadata.mode() & 0o111 == 0 {
                return Err(ArtifactManifestError::Verification {
                    role: entry.role,
                    reason: "required executable bit is missing".into(),
                });
            }
        }
        let actual_sha256 = sha256_file(&path)?;
        if actual_sha256 != entry.sha256 {
            return Err(ArtifactManifestError::Verification {
                role: entry.role,
                reason: "SHA-256 mismatch".into(),
            });
        }
        if expected_architecture.is_some_and(|architecture| architecture == "arm64")
            && !is_thin_arm64_macho(&path)?
        {
            return Err(ArtifactManifestError::Verification {
                role: entry.role,
                reason: "native file is not a thin arm64 Mach-O".into(),
            });
        }
        total_size = total_size
            .checked_add(entry.size_bytes)
            .ok_or_else(|| ArtifactManifestError::Invalid("artifact size overflow".into()))?;
        verified_files.insert(
            entry.role.clone(),
            VerifiedArtifactFile {
                role: entry.role,
                path,
                sha256: entry.sha256,
                size_bytes: entry.size_bytes,
                executable: entry.executable,
            },
        );
    }

    Ok(VerifiedArtifact {
        root,
        identity: requirements.identity.clone(),
        manifest_sha256,
        source_revisions: manifest.source_revisions,
        files: verified_files,
        size_bytes: total_size,
    })
}

fn validate_requirements(requirements: &ArtifactRequirements) -> Result<(), ArtifactManifestError> {
    let identity = &requirements.identity;
    if !valid_identifier(&identity.runtime)
        || !valid_identifier(&identity.catalog_id)
        || !valid_identifier(&identity.family)
        || !valid_identifier(&identity.variant)
        || identity.api_contract_version == 0
        || !valid_sha256(&requirements.manifest_sha256)
        || requirements.roles.is_empty()
        || requirements
            .roles
            .keys()
            .any(|role| !valid_identifier(role))
    {
        return Err(ArtifactManifestError::Invalid(
            "catalog artifact requirements are invalid".into(),
        ));
    }
    if requirements
        .architectures
        .iter()
        .any(|(role, architecture)| {
            !requirements.roles.contains_key(role) || architecture != "arm64"
        })
    {
        return Err(ArtifactManifestError::Invalid(
            "artifact architecture requirements are invalid".into(),
        ));
    }
    Ok(())
}

fn validate_manifest_identity(
    manifest: &Manifest,
    requirements: &ArtifactRequirements,
) -> Result<(), ArtifactManifestError> {
    let expected = &requirements.identity;
    if manifest.schema_version != 1
        || manifest.runtime != expected.runtime
        || manifest.catalog_id != expected.catalog_id
        || manifest.family != expected.family
        || manifest.variant != expected.variant
        || manifest.api_contract_version != expected.api_contract_version
    {
        return Err(ArtifactManifestError::Invalid(
            "manifest identity does not match the catalog".into(),
        ));
    }
    if manifest.source_revisions.is_empty()
        || manifest
            .source_revisions
            .iter()
            .any(|(name, revision)| !valid_identifier(name) || !valid_git_revision(revision))
    {
        return Err(ArtifactManifestError::Invalid(
            "source revisions are missing or invalid".into(),
        ));
    }
    Ok(())
}

fn canonical_artifact_root(root: &Path) -> Result<PathBuf, ArtifactManifestError> {
    if !root.is_absolute() {
        return Err(ArtifactManifestError::InvalidRoot);
    }
    let metadata = fs::symlink_metadata(root).map_err(ArtifactManifestError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ArtifactManifestError::InvalidRoot);
    }
    let canonical = root.canonicalize()?;
    if canonical != root {
        return Err(ArtifactManifestError::InvalidRoot);
    }
    Ok(canonical)
}

fn safe_relative_path(value: &str) -> bool {
    if value.is_empty() || value.contains('\\') {
        return false;
    }
    let path = Path::new(value);
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// Reject symlinks in every component, not only at the final file. This keeps
/// `lib/runtime.dylib` inside the authenticated artifact root even when an
/// attacker replaces `lib` with a link after installation.
fn verify_path_components(
    root: &Path,
    relative: &Path,
    role: &str,
) -> Result<(), ArtifactManifestError> {
    let components: Vec<_> = relative.components().collect();
    let mut current = root.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(ArtifactManifestError::Verification {
                role: role.into(),
                reason: "path contains a non-normal component".into(),
            });
        };
        current.push(name);
        let metadata = fs::symlink_metadata(&current).map_err(|error| {
            ArtifactManifestError::Verification {
                role: role.into(),
                reason: error.kind().to_string(),
            }
        })?;
        if metadata.file_type().is_symlink() {
            return Err(ArtifactManifestError::Verification {
                role: role.into(),
                reason: "path contains a symbolic link".into(),
            });
        }
        let final_component = index + 1 == components.len();
        if (!final_component && !metadata.is_dir()) || (final_component && !metadata.is_file()) {
            return Err(ArtifactManifestError::Verification {
                role: role.into(),
                reason: "path component has an unexpected file type".into(),
            });
        }
    }
    Ok(())
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn valid_git_revision(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn sha256_file(path: &Path) -> Result<String, ArtifactManifestError> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn is_thin_arm64_macho(path: &Path) -> Result<bool, ArtifactManifestError> {
    let mut file = fs::File::open(path)?;
    let mut header = [0_u8; 8];
    if file.read_exact(&mut header).is_err() {
        return Ok(false);
    }
    let magic = u32::from_le_bytes(header[0..4].try_into().unwrap());
    let cpu_type = u32::from_le_bytes(header[4..8].try_into().unwrap());
    Ok(matches!(magic, 0xfeed_face | 0xfeed_facf) && cpu_type == 0x0100_000c)
}

fn fingerprint(artifact: &VerifiedArtifact) -> Option<ArtifactFingerprint> {
    let manifest = file_fingerprint(&artifact.root.join(ARTIFACT_MANIFEST_FILE))?;
    let files = artifact
        .files
        .iter()
        .map(|(role, file)| Some((role.clone(), file_fingerprint(&file.path)?)))
        .collect::<Option<BTreeMap<_, _>>>()?;
    Some(ArtifactFingerprint { manifest, files })
}

fn file_fingerprint(path: &Path) -> Option<FileFingerprint> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    let modified_nanos = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(FileFingerprint {
            size_bytes: metadata.len(),
            modified_nanos,
            changed_nanos: i128::from(metadata.ctime()) * 1_000_000_000
                + i128::from(metadata.ctime_nsec()),
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
        })
    }
    #[cfg(not(unix))]
    {
        Some(FileFingerprint {
            size_bytes: metadata.len(),
            modified_nanos,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

    fn write_fixture() -> (tempfile::TempDir, ArtifactRequirements) {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("lib")).unwrap();
        fs::write(directory.path().join("model.bin"), b"model").unwrap();
        fs::write(directory.path().join("lib/runtime.dylib"), b"native").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                directory.path().join("lib/runtime.dylib"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let manifest = json!({
            "schema_version": 1,
            "runtime": "sherpa_onnx",
            "catalog_id": "sensevoice-small-sherpa-int8",
            "family": "sensevoice-small",
            "variant": "int8",
            "api_contract_version": 1,
            "source_revisions": {"sherpa_onnx": REVISION},
            "files": [
                {"role":"model", "path":"model.bin", "sha256":sha256_bytes(b"model"), "size_bytes":5, "executable":false},
                {"role":"runtime", "path":"lib/runtime.dylib", "sha256":sha256_bytes(b"native"), "size_bytes":6, "executable":true}
            ]
        });
        let bytes = serde_json::to_vec(&manifest).unwrap();
        fs::write(directory.path().join(ARTIFACT_MANIFEST_FILE), &bytes).unwrap();
        let requirements = ArtifactRequirements {
            identity: ArtifactIdentity {
                runtime: "sherpa_onnx".into(),
                catalog_id: "sensevoice-small-sherpa-int8".into(),
                family: "sensevoice-small".into(),
                variant: "int8".into(),
                api_contract_version: 1,
            },
            manifest_sha256: sha256_bytes(&bytes),
            roles: BTreeMap::from([("model".into(), false), ("runtime".into(), true)]),
            architectures: BTreeMap::new(),
        };
        (directory, requirements)
    }

    fn rewrite_manifest(
        directory: &tempfile::TempDir,
        requirements: &mut ArtifactRequirements,
        mutate: impl FnOnce(&mut Value),
    ) {
        let path = directory.path().join(ARTIFACT_MANIFEST_FILE);
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        mutate(&mut value);
        let bytes = serde_json::to_vec(&value).unwrap();
        fs::write(path, &bytes).unwrap();
        requirements.manifest_sha256 = sha256_bytes(&bytes);
    }

    fn root(directory: &tempfile::TempDir) -> PathBuf {
        directory.path().canonicalize().unwrap()
    }

    #[test]
    fn verifies_trusted_complete_artifact() {
        let (directory, requirements) = write_fixture();
        let artifact = verify_artifact(&root(&directory), &requirements).unwrap();
        assert_eq!(artifact.size_bytes, 11);
        assert_eq!(
            artifact.file("model"),
            Some(root(&directory).join("model.bin").as_path())
        );
    }

    #[test]
    fn authenticates_manifest_before_parsing_paths() {
        let (directory, mut requirements) = write_fixture();
        requirements.manifest_sha256 = "0".repeat(64);
        fs::write(
            directory.path().join(ARTIFACT_MANIFEST_FILE),
            br#"{"files":[{"path":"../../escape"}]}"#,
        )
        .unwrap();
        assert!(matches!(
            verify_artifact(&root(&directory), &requirements),
            Err(ArtifactManifestError::ManifestHashMismatch)
        ));
    }

    #[test]
    fn rejects_unknown_fields_and_identity_drift() {
        let (directory, mut requirements) = write_fixture();
        rewrite_manifest(&directory, &mut requirements, |value| {
            value["unexpected"] = json!(true);
        });
        assert!(matches!(
            verify_artifact(&root(&directory), &requirements),
            Err(ArtifactManifestError::Json(_))
        ));

        let (directory, mut requirements) = write_fixture();
        rewrite_manifest(&directory, &mut requirements, |value| {
            value["variant"] = json!("fp32");
        });
        assert!(matches!(
            verify_artifact(&root(&directory), &requirements),
            Err(ArtifactManifestError::Invalid(_))
        ));
    }

    #[test]
    fn rejects_missing_extra_or_duplicate_roles_and_paths() {
        let (directory, mut requirements) = write_fixture();
        rewrite_manifest(&directory, &mut requirements, |value| {
            value["files"].as_array_mut().unwrap().pop();
        });
        assert!(verify_artifact(&root(&directory), &requirements).is_err());

        let (directory, mut requirements) = write_fixture();
        rewrite_manifest(&directory, &mut requirements, |value| {
            value["files"].as_array_mut().unwrap().push(json!({
                "role":"extra", "path":"extra", "sha256":"0".repeat(64),
                "size_bytes":1, "executable":false
            }));
        });
        assert!(verify_artifact(&root(&directory), &requirements).is_err());

        let (directory, mut requirements) = write_fixture();
        rewrite_manifest(&directory, &mut requirements, |value| {
            value["files"][1]["role"] = json!("model");
            value["files"][1]["path"] = json!("model.bin");
        });
        assert!(verify_artifact(&root(&directory), &requirements).is_err());
    }

    #[test]
    fn rejects_absolute_parent_and_backslash_paths() {
        for unsafe_path in ["/tmp/model", "../model", "nested/../model", "nested\\model"] {
            let (directory, mut requirements) = write_fixture();
            rewrite_manifest(&directory, &mut requirements, |value| {
                value["files"][0]["path"] = json!(unsafe_path);
            });
            assert!(verify_artifact(&root(&directory), &requirements).is_err());
        }
    }

    #[test]
    fn rejects_size_hash_permission_and_symlink_failures() {
        let (directory, requirements) = write_fixture();
        fs::write(directory.path().join("model.bin"), b"wrong").unwrap();
        assert!(matches!(
            verify_artifact(&root(&directory), &requirements),
            Err(ArtifactManifestError::Verification { .. })
        ));

        let (directory, requirements) = write_fixture();
        fs::write(directory.path().join("model.bin"), b"xxxxx").unwrap();
        assert!(matches!(
            verify_artifact(&root(&directory), &requirements),
            Err(ArtifactManifestError::Verification { .. })
        ));

        #[cfg(unix)]
        {
            use std::os::unix::fs::{symlink, PermissionsExt};
            let (directory, requirements) = write_fixture();
            fs::set_permissions(
                directory.path().join("lib/runtime.dylib"),
                fs::Permissions::from_mode(0o644),
            )
            .unwrap();
            assert!(verify_artifact(&root(&directory), &requirements).is_err());

            let (directory, requirements) = write_fixture();
            fs::remove_file(directory.path().join("model.bin")).unwrap();
            symlink("/dev/null", directory.path().join("model.bin")).unwrap();
            assert!(verify_artifact(&root(&directory), &requirements).is_err());

            let (directory, requirements) = write_fixture();
            let outside = tempfile::tempdir().unwrap();
            fs::write(outside.path().join("runtime.dylib"), b"native").unwrap();
            fs::set_permissions(
                outside.path().join("runtime.dylib"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
            fs::remove_dir_all(directory.path().join("lib")).unwrap();
            symlink(outside.path(), directory.path().join("lib")).unwrap();
            assert!(matches!(
                verify_artifact(&root(&directory), &requirements),
                Err(ArtifactManifestError::Verification { .. })
            ));
        }
    }

    #[test]
    fn rejects_native_file_with_wrong_architecture() {
        let (directory, mut requirements) = write_fixture();
        requirements
            .architectures
            .insert("runtime".into(), "arm64".into());
        rewrite_manifest(&directory, &mut requirements, |value| {
            value["files"][1]["architecture"] = json!("arm64");
        });
        assert!(matches!(
            verify_artifact(&root(&directory), &requirements),
            Err(ArtifactManifestError::Verification { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_root_and_manifest() {
        use std::os::unix::fs::symlink;
        let (directory, requirements) = write_fixture();
        let parent = tempfile::tempdir().unwrap();
        let link = parent.path().join("artifact");
        symlink(directory.path(), &link).unwrap();
        assert!(matches!(
            verify_artifact(&link, &requirements),
            Err(ArtifactManifestError::InvalidRoot)
        ));

        let (directory, requirements) = write_fixture();
        let manifest = directory.path().join(ARTIFACT_MANIFEST_FILE);
        fs::remove_file(&manifest).unwrap();
        symlink("/dev/null", manifest).unwrap();
        assert!(matches!(
            verify_artifact(&root(&directory), &requirements),
            Err(ArtifactManifestError::InvalidManifestFile)
        ));
    }

    #[test]
    fn listing_cache_invalidates_when_file_metadata_changes() {
        let (directory, requirements) = write_fixture();
        let root = root(&directory);
        let cache = ArtifactVerificationCache::new();
        assert!(cache.verify_for_listing(&root, &requirements).is_ok());

        let replacement = directory.path().join("replacement");
        fs::write(&replacement, b"xxxxx").unwrap();
        fs::rename(replacement, directory.path().join("model.bin")).unwrap();
        assert!(matches!(
            cache.verify_for_listing(&root, &requirements),
            Err(ArtifactManifestError::Verification { .. })
        ));
    }

    #[test]
    fn listing_cache_invalidates_when_requirements_change() {
        let (directory, requirements) = write_fixture();
        let root = root(&directory);
        let cache = ArtifactVerificationCache::new();
        assert!(cache.verify_for_listing(&root, &requirements).is_ok());
        let mut changed = requirements.clone();
        changed.identity.variant = "fp32".into();
        assert!(matches!(
            cache.verify_for_listing(&root, &changed),
            Err(ArtifactManifestError::Invalid(_))
        ));
    }
}
