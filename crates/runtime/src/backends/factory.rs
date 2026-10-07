//! Compile-time backend registry. The daemon supplies only locally resolved,
//! authenticated resources; descriptors construct but do not start drivers.

use super::{FunAsrDriver, SenseVoiceGgufDriver, SherpaOnnxDriver, WhisperDriver};
use crate::{
    verify_artifact, verify_legacy_gguf_artifact, ArtifactManifestError, ArtifactRequirements,
    GgufManifestError, ModelRuntime, RuntimeKind,
};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;

pub enum BackendResources {
    Whisper(PreflightedWhisperResources),
    FunAsr(PreflightedFunAsrResources),
    Gguf(VerifiedGgufResources),
    SherpaOnnx(VerifiedSherpaResources),
}

impl BackendResources {
    pub const fn runtime_kind(&self) -> RuntimeKind {
        match self {
            Self::Whisper(_) => RuntimeKind::Whisper,
            Self::FunAsr(_) => RuntimeKind::FunAsr,
            Self::Gguf(_) => RuntimeKind::Gguf,
            Self::SherpaOnnx(_) => RuntimeKind::SherpaOnnx,
        }
    }
}

/// Legacy backends have no catalog-authenticated artifact manifest. Raw paths
/// are accepted only by these preflight constructors and never by the factory.
pub struct PreflightedWhisperResources {
    id: String,
    binary: PathBuf,
    model: PathBuf,
}

impl PreflightedWhisperResources {
    pub fn preflight(
        id: String,
        binary: PathBuf,
        model: PathBuf,
    ) -> Result<Self, BackendRegistryError> {
        if !is_executable(&binary) || !model.is_file() {
            return Err(BackendRegistryError::NotInstalled(
                "whisper binary is not executable or model is missing".into(),
            ));
        }
        Ok(Self { id, binary, model })
    }
}

pub struct PreflightedFunAsrResources {
    id: String,
    python: PathBuf,
    sidecar: PathBuf,
    models_root: PathBuf,
    device: String,
    extra_root: Option<PathBuf>,
}

impl PreflightedFunAsrResources {
    #[allow(clippy::too_many_arguments)]
    pub fn preflight(
        id: String,
        python: PathBuf,
        sidecar: PathBuf,
        models_root: PathBuf,
        device: String,
        extra_root: Option<PathBuf>,
    ) -> Result<Self, BackendRegistryError> {
        if !is_executable(&python)
            || !sidecar.is_file()
            || !models_root.join("asr").is_dir()
            || !models_root.join("vad").is_dir()
        {
            return Err(BackendRegistryError::NotInstalled(
                "FunASR runtime resources are missing".into(),
            ));
        }
        Ok(Self {
            id,
            python,
            sidecar,
            models_root,
            device,
            extra_root,
        })
    }
}

/// GGUF resources verified against a catalog trust anchor. The historical
/// manifest has a fixed path schema; the catalog additionally authenticates
/// its bytes, identity, exact roles and total size.
pub struct VerifiedGgufResources {
    id: String,
    root: PathBuf,
    requirements: ArtifactRequirements,
    expected_size_bytes: u64,
}

impl VerifiedGgufResources {
    pub fn verify(
        id: String,
        root: PathBuf,
        requirements: ArtifactRequirements,
        expected_size_bytes: u64,
    ) -> Result<Self, BackendRegistryError> {
        let parsed =
            verify_gguf_against_requirements(&root, &id, &requirements, expected_size_bytes)?;
        Ok(Self {
            id,
            root: parsed.root,
            requirements,
            expected_size_bytes,
        })
    }
}

pub(super) fn verify_gguf_against_requirements(
    root: &std::path::Path,
    id: &str,
    requirements: &ArtifactRequirements,
    expected_size_bytes: u64,
) -> Result<crate::VerifiedArtifact, BackendRegistryError> {
    let artifact = verify_legacy_gguf_artifact(root, id)?;
    let exact_roles = artifact.files.len() == requirements.roles.len()
        && requirements.roles.iter().all(|(role, executable)| {
            artifact
                .files
                .get(role)
                .is_some_and(|file| file.executable == *executable)
        });
    if artifact.identity != requirements.identity
        || artifact.manifest_sha256 != requirements.manifest_sha256
        || artifact.size_bytes != expected_size_bytes
        || !exact_roles
    {
        return Err(BackendRegistryError::GgufCatalogMismatch {
            requested_id: id.to_owned(),
        });
    }
    Ok(artifact)
}

/// Catalog-anchored Sherpa resources. Verification happens before construction;
/// the driver retains the requirements and performs the same full check again
/// immediately before every start/restart.
pub struct VerifiedSherpaResources {
    id: String,
    root: PathBuf,
    requirements: ArtifactRequirements,
}

impl VerifiedSherpaResources {
    pub fn verify(
        id: String,
        root: PathBuf,
        requirements: ArtifactRequirements,
        expected_size_bytes: u64,
    ) -> Result<Self, BackendRegistryError> {
        let artifact = verify_artifact(&root, &requirements)?;
        if artifact.identity.runtime != RuntimeKind::SherpaOnnx.as_str()
            || artifact.identity.catalog_id != id
            || artifact.size_bytes != expected_size_bytes
        {
            return Err(BackendRegistryError::SherpaIdentityMismatch { requested_id: id });
        }
        Ok(Self {
            id,
            root: artifact.root,
            requirements,
        })
    }
}

#[derive(Debug, Error)]
pub enum BackendRegistryError {
    #[error("GGUF resource preflight failed: {0}")]
    Gguf(#[from] GgufManifestError),
    #[error("artifact verification failed: {0}")]
    Artifact(#[from] ArtifactManifestError),
    #[error(
        "GGUF manifest model_id {manifest_id:?} does not match requested model {requested_id:?}"
    )]
    GgufModelMismatch {
        manifest_id: String,
        requested_id: String,
    },
    #[error("GGUF artifact does not match catalog trust anchor for {requested_id:?}")]
    GgufCatalogMismatch { requested_id: String },
    #[error("Sherpa ONNX artifact identity does not match requested model {requested_id:?}")]
    SherpaIdentityMismatch { requested_id: String },
    #[error("backend resources are not installed: {0}")]
    NotInstalled(String),
    #[error("backend runtime {0:?} is not registered")]
    UnknownRuntime(RuntimeKind),
    #[error("backend runtime {0:?} is registered more than once")]
    DuplicateRuntime(RuntimeKind),
}

struct BackendDescriptor {
    kind: RuntimeKind,
    build: fn(BackendResources) -> Result<Arc<dyn ModelRuntime>, BackendRegistryError>,
}

const DESCRIPTORS: &[BackendDescriptor] = &[
    BackendDescriptor {
        kind: RuntimeKind::Whisper,
        build: build_whisper,
    },
    BackendDescriptor {
        kind: RuntimeKind::FunAsr,
        build: build_funasr,
    },
    BackendDescriptor {
        kind: RuntimeKind::Gguf,
        build: build_gguf,
    },
    BackendDescriptor {
        kind: RuntimeKind::SherpaOnnx,
        build: build_sherpa_onnx,
    },
];

pub struct BackendRegistry;

impl BackendRegistry {
    pub fn build(
        resources: BackendResources,
    ) -> Result<Arc<dyn ModelRuntime>, BackendRegistryError> {
        validate_descriptors(DESCRIPTORS)?;
        let kind = resources.runtime_kind();
        let descriptor = DESCRIPTORS
            .iter()
            .find(|descriptor| descriptor.kind == kind)
            .ok_or(BackendRegistryError::UnknownRuntime(kind))?;
        (descriptor.build)(resources)
    }

    pub fn registered_runtimes() -> impl Iterator<Item = RuntimeKind> {
        DESCRIPTORS.iter().map(|descriptor| descriptor.kind)
    }
}

fn validate_descriptors(descriptors: &[BackendDescriptor]) -> Result<(), BackendRegistryError> {
    let mut kinds = HashSet::new();
    for descriptor in descriptors {
        if !kinds.insert(descriptor.kind) {
            return Err(BackendRegistryError::DuplicateRuntime(descriptor.kind));
        }
    }
    Ok(())
}

fn build_whisper(
    resources: BackendResources,
) -> Result<Arc<dyn ModelRuntime>, BackendRegistryError> {
    let BackendResources::Whisper(resources) = resources else {
        unreachable!("registry descriptor mismatch")
    };
    Ok(Arc::new(WhisperDriver::new(
        &resources.id,
        resources.binary,
        resources.model,
    )))
}

fn build_funasr(
    resources: BackendResources,
) -> Result<Arc<dyn ModelRuntime>, BackendRegistryError> {
    let BackendResources::FunAsr(resources) = resources else {
        unreachable!("registry descriptor mismatch")
    };
    Ok(Arc::new(FunAsrDriver::new(
        &resources.id,
        resources.python,
        resources.sidecar,
        resources.models_root,
        resources.device,
        resources.extra_root,
    )))
}

fn build_gguf(resources: BackendResources) -> Result<Arc<dyn ModelRuntime>, BackendRegistryError> {
    let BackendResources::Gguf(resources) = resources else {
        unreachable!("registry descriptor mismatch")
    };
    Ok(Arc::new(SenseVoiceGgufDriver::new_verified(
        &resources.id,
        resources.root,
        resources.requirements,
        resources.expected_size_bytes,
    )))
}

fn build_sherpa_onnx(
    resources: BackendResources,
) -> Result<Arc<dyn ModelRuntime>, BackendRegistryError> {
    let BackendResources::SherpaOnnx(resources) = resources else {
        unreachable!("registry descriptor mismatch")
    };
    Ok(Arc::new(SherpaOnnxDriver::new(
        &resources.id,
        resources.root,
        resources.requirements,
    )))
}

fn is_executable(path: &std::path::Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(path)
            .map(|meta| meta.mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ArtifactIdentity;
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn sha(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn gguf_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let entries = [
            ("sensevoice-server", b"server".as_slice()),
            ("sensevoice.gguf", b"model".as_slice()),
            ("fsmn-vad.gguf", b"vad".as_slice()),
        ];
        for (name, bytes) in entries {
            fs::write(dir.path().join(name), bytes).unwrap();
        }
        #[cfg(unix)]
        fs::set_permissions(
            dir.path().join("sensevoice-server"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let files: Vec<_> = entries.iter().map(|(name, bytes)| serde_json::json!({"path":name,"sha256":sha(bytes),"size_bytes":bytes.len()})).collect();
        fs::write(dir.path().join("runtime-manifest.json"), serde_json::json!({"schema_version":1,"runtime":"gguf","model_id":"sensevoice-small","variant":"q8","model_file":"sensevoice.gguf","vad_file":"fsmn-vad.gguf","source_revision":"6991744856587fa44379e8b5dcc432debffeb1be","api_contract_version":1,"files":files}).to_string()).unwrap();
        dir
    }

    fn gguf_requirements(root: &std::path::Path, id: &str) -> (ArtifactRequirements, u64) {
        let artifact = verify_legacy_gguf_artifact(root, id).unwrap();
        let requirements = ArtifactRequirements {
            identity: artifact.identity.clone(),
            manifest_sha256: artifact.manifest_sha256.clone(),
            roles: artifact
                .files
                .iter()
                .map(|(role, file)| (role.clone(), file.executable))
                .collect(),
            architectures: BTreeMap::new(),
        };
        (requirements, artifact.size_bytes)
    }

    fn sherpa_artifact(id: &str) -> (tempfile::TempDir, ArtifactRequirements) {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("model.onnx"), b"model").unwrap();
        let manifest = serde_json::json!({
            "schema_version": 1,
            "runtime": "sherpa_onnx",
            "catalog_id": id,
            "family": "sensevoice-small",
            "variant": "int8",
            "api_contract_version": 1,
            "source_revisions": {"sherpa-onnx":"0123456789abcdef0123456789abcdef01234567"},
            "files": [{"role":"model","path":"model.onnx","sha256":sha(b"model"),"size_bytes":5,"executable":false}]
        })
        .to_string();
        fs::write(directory.path().join("artifact-manifest.json"), &manifest).unwrap();
        let requirements = ArtifactRequirements {
            identity: ArtifactIdentity {
                runtime: "sherpa_onnx".into(),
                catalog_id: id.into(),
                family: "sensevoice-small".into(),
                variant: "int8".into(),
                api_contract_version: 1,
            },
            manifest_sha256: sha(manifest.as_bytes()),
            roles: BTreeMap::from([("model".into(), false)]),
            architectures: BTreeMap::new(),
        };
        (directory, requirements)
    }

    #[test]
    fn registry_constructs_all_four_backends_without_starting() {
        let root = gguf_root();
        let root_path = root.path().canonicalize().unwrap();
        let (requirements, expected_size_bytes) = gguf_requirements(&root_path, "sensevoice-small");
        let gguf_resources = VerifiedGgufResources::verify(
            "sensevoice-small".into(),
            root_path,
            requirements,
            expected_size_bytes,
        )
        .unwrap();
        let gguf = BackendRegistry::build(BackendResources::Gguf(gguf_resources)).unwrap();
        assert_eq!(gguf.runtime_kind(), RuntimeKind::Gguf);
        let tmp = tempfile::tempdir().unwrap();
        let whisper_bin = tmp.path().join("whisper");
        let whisper_model = tmp.path().join("model");
        let python = tmp.path().join("python");
        let sidecar = tmp.path().join("sidecar.py");
        let models = tmp.path().join("models");
        fs::write(&whisper_bin, b"").unwrap();
        fs::write(&whisper_model, b"").unwrap();
        fs::write(&python, b"").unwrap();
        fs::write(&sidecar, b"").unwrap();
        fs::create_dir(&models).unwrap();
        fs::create_dir(models.join("asr")).unwrap();
        fs::create_dir(models.join("vad")).unwrap();
        #[cfg(unix)]
        {
            fs::set_permissions(&whisper_bin, fs::Permissions::from_mode(0o755)).unwrap();
            fs::set_permissions(&python, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let whisper =
            PreflightedWhisperResources::preflight("w".into(), whisper_bin, whisper_model).unwrap();
        assert_eq!(
            BackendRegistry::build(BackendResources::Whisper(whisper))
                .unwrap()
                .runtime_kind(),
            RuntimeKind::Whisper
        );
        let funasr = PreflightedFunAsrResources::preflight(
            "f".into(),
            python,
            sidecar,
            models,
            "cpu".into(),
            None,
        )
        .unwrap();
        assert_eq!(
            BackendRegistry::build(BackendResources::FunAsr(funasr))
                .unwrap()
                .runtime_kind(),
            RuntimeKind::FunAsr
        );
        let (sherpa_root, sherpa_requirements) = sherpa_artifact("sensevoice-small-sherpa-int8");
        let sherpa = VerifiedSherpaResources::verify(
            "sensevoice-small-sherpa-int8".into(),
            sherpa_root.path().canonicalize().unwrap(),
            sherpa_requirements,
            5,
        )
        .unwrap();
        assert_eq!(
            BackendRegistry::build(BackendResources::SherpaOnnx(sherpa))
                .unwrap()
                .runtime_kind(),
            RuntimeKind::SherpaOnnx
        );
    }

    #[test]
    fn duplicate_runtime_descriptors_are_rejected() {
        let duplicate = [
            BackendDescriptor {
                kind: RuntimeKind::Whisper,
                build: build_whisper,
            },
            BackendDescriptor {
                kind: RuntimeKind::Whisper,
                build: build_whisper,
            },
        ];
        assert!(matches!(
            validate_descriptors(&duplicate),
            Err(BackendRegistryError::DuplicateRuntime(RuntimeKind::Whisper))
        ));
    }

    #[test]
    fn unavailable_resources_fail_without_constructing_a_backend() {
        assert!(matches!(
            PreflightedWhisperResources::preflight(
                "w".into(),
                PathBuf::from("/missing/bin"),
                PathBuf::from("/missing/model")
            ),
            Err(BackendRegistryError::NotInstalled(_))
        ));
    }

    #[test]
    fn gguf_model_id_mismatch_is_rejected_before_driver_creation() {
        let root = gguf_root();
        let root_path = root.path().canonicalize().unwrap();
        let (requirements, expected_size_bytes) = gguf_requirements(&root_path, "sensevoice-small");
        let result = VerifiedGgufResources::verify(
            "wrong".into(),
            root_path,
            requirements,
            expected_size_bytes,
        );
        assert!(matches!(
            result,
            Err(BackendRegistryError::GgufCatalogMismatch { .. })
        ));
    }

    #[test]
    fn gguf_catalog_manifest_hash_mismatch_is_rejected_before_driver_creation() {
        let root = gguf_root();
        let root_path = root.path().canonicalize().unwrap();
        let (mut requirements, expected_size_bytes) =
            gguf_requirements(&root_path, "sensevoice-small");
        requirements.manifest_sha256 = "0".repeat(64);

        let result = VerifiedGgufResources::verify(
            "sensevoice-small".into(),
            root_path,
            requirements,
            expected_size_bytes,
        );

        assert!(matches!(
            result,
            Err(BackendRegistryError::GgufCatalogMismatch { .. })
        ));
    }
}
