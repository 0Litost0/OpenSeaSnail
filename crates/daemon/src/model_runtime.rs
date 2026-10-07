//! HTTP 无关的模型制品解析、校验与 runtime 构造 adapter.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use seasnail_runtime::{
    ArtifactIdentity, ArtifactRequirements, ArtifactVerificationCache, BackendRegistry,
    BackendResources, ModelRuntime, PreflightedFunAsrResources, PreflightedWhisperResources,
    RuntimeKind, VerifiedArtifact, VerifiedGgufResources, VerifiedSherpaResources,
};

use crate::application::{
    safe_artifact_key, ManifestEntry, ModelRuntimeBuildError, ModelRuntimeKind,
};
use crate::error::AppError;

/// Cross-check a verified v2 artifact against the embedded catalog. The caller
/// must authenticate and verify the artifact first; catalog size is the sum of
/// every manifest file, never a client-provided value or model-only estimate.
pub(crate) fn artifact_matches_catalog(entry: &ManifestEntry, artifact: &VerifiedArtifact) -> bool {
    entry.family.as_deref() == Some(artifact.identity.family.as_str())
        && entry.variant.as_deref() == Some(artifact.identity.variant.as_str())
        && entry.runtime.as_str() == artifact.identity.runtime
        && entry.id == artifact.identity.catalog_id
        && entry.artifact_manifest_sha256.as_deref() == Some(artifact.manifest_sha256.as_str())
        && entry.size_bytes == artifact.size_bytes
}

// ── 路径解析 ───────────────────────────────────────────────────────────────────

/// 解析 whisper 二进制 + 模型路径：`WHISPER_SERVER_PATH` + `WHISPER_MODEL_PATH` env。
/// 任一未设 / 空 → None（= not_installed；activate 据此 409 download-first）。
/// **env-only（dev）**：prod 应指向 app bundle `Resources/whisper-server` +
/// `Resources/<model>.bin`（M6 打包期落地；当前 env 探测便于 dev/test 上机）。
pub(crate) fn resolve_whisper_paths() -> Option<(PathBuf, PathBuf)> {
    let binary = std::env::var("WHISPER_SERVER_PATH")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)?;
    let model_path = std::env::var("WHISPER_MODEL_PATH")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)?;
    whisper_paths_are_complete(&binary, &model_path).then_some((binary, model_path))
}

pub(crate) fn whisper_paths_are_complete(
    binary: &std::path::Path,
    model: &std::path::Path,
) -> bool {
    is_executable_file(binary) && model.is_file()
}

pub(crate) fn is_executable_file(path: &std::path::Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path)
            .map(|metadata| metadata.mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    true
}

/// FunASR bundle 根目录：开发/测试可通过 `SEASNAIL_FUNASR_ROOT` 显式指定；发布版
/// 由守护进程可执行文件位置推导 `../Resources/funasr`。只接受完整 bundle，避免以
/// 系统 Python、pip 或模型仓库作为隐式后备。
pub(crate) fn resolve_funasr_root() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("SEASNAIL_FUNASR_ROOT").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(root));
    }
    let executable = std::env::current_exe().ok()?;
    let macos_dir = executable.parent()?;
    let contents_dir = macos_dir.parent()?;
    Some(contents_dir.join("Resources").join("funasr"))
}

/// 解析完整 FunASR bundle 路径。模型目录会再被 `FunAsrDriver::start` 逐项检查，
/// 此处提前检查以便 `GET /models` 精确显示 installed/not_installed。
pub(crate) fn resolve_funasr_paths() -> Option<(PathBuf, PathBuf, PathBuf)> {
    resolve_funasr_paths_at(&resolve_funasr_root()?)
}

pub(crate) fn resolve_funasr_paths_at(
    root: &std::path::Path,
) -> Option<(PathBuf, PathBuf, PathBuf)> {
    // 使用 SeaSnail 命名的解释器，让 macOS 活动监视器明确显示这是随应用启动的 FunASR sidecar。
    let python = root.join("python/bin/seasnail-funasr");
    let sidecar = root.join("sidecar.py");
    let models = root.join("models");
    // M2.3：仅检 asr+vad（最小常驻集）；punc/spk 可选，缺省不卡 `installed`
    //（punc 走用户目录 extra-root 或内置回落，spk 后置）。
    let complete = is_executable_file(&python)
        && sidecar.is_file()
        && ["asr", "vad"].iter().all(|name| models.join(name).is_dir());
    complete.then_some((python, sidecar, models))
}

/// 由 daemon 可执行文件位置推导发布包的 GGUF 根。
pub(crate) fn bundled_gguf_root() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let macos_dir = executable.parent()?;
    let contents_dir = macos_dir.parent()?;
    Some(contents_dir.join("Resources").join("gguf"))
}

/// 选择 GGUF 资源根。环境覆盖仅存在于 debug 开发构建；生产包必须使用由可执行文件
/// 推导出的 App Resources 路径，不能被进程环境注入到任意自制 sidecar/manifest。
pub(crate) fn select_gguf_root(
    development_override: Option<PathBuf>,
    bundled_root: Option<PathBuf>,
    allow_development_override: bool,
) -> Option<PathBuf> {
    if allow_development_override {
        development_override.or(bundled_root)
    } else {
        bundled_root
    }
}

/// GGUF 资源根：debug 开发/测试可通过显式 `SEASNAIL_GGUF_ROOT` 指定；发布包由 daemon
/// 可执行文件位置推导 `../Resources/gguf`。不从用户目录、系统路径或网络下载推测资源。
pub(crate) fn resolve_gguf_root() -> Option<PathBuf> {
    let development_override = std::env::var_os("SEASNAIL_GGUF_ROOT")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    select_gguf_root(
        development_override,
        bundled_gguf_root(),
        cfg!(debug_assertions),
    )
}

pub(crate) fn gguf_entry_is_installed_at(entry: &ManifestEntry, root: &std::path::Path) -> bool {
    gguf_artifact_requirements(entry).is_some_and(|requirements| {
        VerifiedGgufResources::verify(
            entry.id.clone(),
            root.to_path_buf(),
            requirements,
            entry.size_bytes,
        )
        .is_ok()
    })
}

pub(crate) fn gguf_artifact_requirements(entry: &ManifestEntry) -> Option<ArtifactRequirements> {
    (entry.runtime == ModelRuntimeKind::Gguf).then_some(())?;
    Some(ArtifactRequirements {
        identity: ArtifactIdentity {
            runtime: RuntimeKind::Gguf.as_str().into(),
            catalog_id: entry.id.clone(),
            family: entry.family.clone()?,
            variant: entry.variant.clone()?,
            api_contract_version: 1,
        },
        manifest_sha256: entry.artifact_manifest_sha256.clone()?,
        roles: BTreeMap::from([
            ("model".into(), false),
            ("sidecar".into(), true),
            ("vad".into(), false),
        ]),
        architectures: BTreeMap::new(),
    })
}

pub(crate) fn bundled_asr_root() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let macos_dir = executable.parent()?;
    let contents_dir = macos_dir.parent()?;
    Some(contents_dir.join("Resources").join("asr"))
}

pub(crate) fn resolve_asr_root() -> Option<PathBuf> {
    let development_override = std::env::var_os("SEASNAIL_ASR_ROOT")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if cfg!(debug_assertions) {
        development_override.or_else(bundled_asr_root)
    } else {
        bundled_asr_root()
    }
}

pub(crate) fn sherpa_artifact_location(
    entry: &ManifestEntry,
) -> Option<(PathBuf, ArtifactRequirements)> {
    let artifact_key = entry.artifact_key.as_deref()?;
    if !safe_artifact_key(artifact_key) {
        return None;
    }
    let requirements = sherpa_artifact_requirements(entry)?;
    Some((resolve_asr_root()?.join(artifact_key), requirements))
}

pub(crate) fn sherpa_artifact_requirements(entry: &ManifestEntry) -> Option<ArtifactRequirements> {
    let family = entry.family.as_deref()?;
    let variant = entry.variant.as_deref()?;
    let manifest_sha256 = entry.artifact_manifest_sha256.as_deref()?;
    let roles = BTreeMap::from([
        ("model".into(), false),
        ("onnxruntime".into(), false),
        ("sherpa-onnx".into(), true),
        ("sidecar".into(), true),
        ("tokens".into(), false),
        ("vad".into(), false),
    ]);
    Some(ArtifactRequirements {
        identity: ArtifactIdentity {
            runtime: entry.runtime.as_str().into(),
            catalog_id: entry.id.clone(),
            family: family.into(),
            variant: variant.into(),
            api_contract_version: 1,
        },
        manifest_sha256: manifest_sha256.into(),
        roles,
        architectures: BTreeMap::from([
            ("onnxruntime".into(), "arm64".into()),
            ("sherpa-onnx".into(), "arm64".into()),
            ("sidecar".into(), "arm64".into()),
        ]),
    })
}

pub(crate) fn sherpa_artifact_cache() -> &'static ArtifactVerificationCache {
    static CACHE: OnceLock<ArtifactVerificationCache> = OnceLock::new();
    CACHE.get_or_init(ArtifactVerificationCache::new)
}

pub(crate) fn sherpa_entry_is_installed(entry: &ManifestEntry) -> bool {
    let Some(asr_root) = resolve_asr_root() else {
        return false;
    };
    sherpa_entry_is_installed_at(entry, &asr_root, sherpa_artifact_cache())
}

pub(crate) fn sherpa_entry_is_installed_at(
    entry: &ManifestEntry,
    asr_root: &std::path::Path,
    cache: &ArtifactVerificationCache,
) -> bool {
    let Some(requirements) = sherpa_artifact_requirements(entry) else {
        return false;
    };
    let Some(artifact_key) = entry.artifact_key.as_deref() else {
        return false;
    };
    if !safe_artifact_key(artifact_key) {
        return false;
    }
    let root = asr_root.join(artifact_key);
    cache
        .verify_for_listing(&root, &requirements)
        .ok()
        .is_some_and(|artifact| artifact_matches_catalog(entry, &artifact))
}

pub(crate) struct ModelBackendDescriptor {
    pub(crate) kind: ModelRuntimeKind,
    pub(crate) installed: fn(&ManifestEntry) -> bool,
    pub(crate) build:
        fn(&ManifestEntry, &std::path::Path) -> Result<Arc<dyn ModelRuntime>, AppError>,
}

pub(crate) const MODEL_BACKENDS: &[ModelBackendDescriptor] = &[
    ModelBackendDescriptor {
        kind: ModelRuntimeKind::Whisper,
        installed: whisper_is_installed,
        build: build_whisper_runtime,
    },
    ModelBackendDescriptor {
        kind: ModelRuntimeKind::FunAsr,
        installed: funasr_is_installed,
        build: build_funasr_runtime,
    },
    ModelBackendDescriptor {
        kind: ModelRuntimeKind::Gguf,
        installed: gguf_is_installed,
        build: build_gguf_runtime_from_catalog,
    },
    ModelBackendDescriptor {
        kind: ModelRuntimeKind::SherpaOnnx,
        installed: sherpa_entry_is_installed,
        build: build_sherpa_runtime,
    },
];

pub(crate) fn validate_backend_descriptors(
    descriptors: &[ModelBackendDescriptor],
) -> Result<(), AppError> {
    let mut kinds = HashSet::new();
    for descriptor in descriptors {
        if !kinds.insert(descriptor.kind) {
            return Err(AppError::Internal(format!(
                "duplicate model backend descriptor: {}",
                descriptor.kind.as_str()
            )));
        }
    }
    Ok(())
}

pub(crate) fn backend_descriptor(
    kind: ModelRuntimeKind,
) -> Option<&'static ModelBackendDescriptor> {
    MODEL_BACKENDS
        .iter()
        .find(|descriptor| descriptor.kind == kind)
}

pub(crate) fn whisper_is_installed(_: &ManifestEntry) -> bool {
    resolve_whisper_paths().is_some()
}
pub(crate) fn funasr_is_installed(_: &ManifestEntry) -> bool {
    resolve_funasr_paths().is_some()
}
pub(crate) fn gguf_is_installed(entry: &ManifestEntry) -> bool {
    resolve_gguf_root().is_some_and(|root| gguf_entry_is_installed_at(entry, &root))
}

/// 条目是否「installed」（所有运行时本地路径可解析）。
pub(crate) fn is_installed(entry: &ManifestEntry) -> bool {
    validate_backend_descriptors(MODEL_BACKENDS).is_ok()
        && backend_descriptor(entry.runtime).is_some_and(|descriptor| (descriptor.installed)(entry))
}

/// 内部迁移包的显式后端选择开关。它是构建 feature，须由打包脚本同时传给 daemon 和
/// WebView；不能用 `debug_assertions`，因为未签名开发 App 仍以优化 release profile 构建。
pub(crate) const fn backend_switching_enabled() -> bool {
    cfg!(feature = "debug-backend-switching")
}

/// 由清单条目解析本地资源并统一交给 BackendRegistry 构造 runtime。所有路径必须已由
/// bundle 提供，路径未就绪 → 409；不允许系统 Python 或在线下载后备。
pub(crate) fn build_runtime(
    entry: &ManifestEntry,
    extra_root: &std::path::Path,
) -> Result<Arc<dyn ModelRuntime>, AppError> {
    validate_backend_descriptors(MODEL_BACKENDS)?;
    let descriptor = backend_descriptor(entry.runtime).ok_or_else(|| {
        AppError::Conflict(format!(
            "unsupported model runtime: {}",
            entry.runtime.as_str()
        ))
    })?;
    (descriptor.build)(entry, extra_root)
}

pub(crate) fn build_runtime_for_service(
    entry: &ManifestEntry,
    extra_root: &std::path::Path,
) -> Result<Arc<dyn ModelRuntime>, ModelRuntimeBuildError> {
    build_runtime(entry, extra_root).map_err(|error| match error {
        AppError::Conflict(message)
        | AppError::ResourceUnavailable(message)
        | AppError::ServiceUnavailable(message) => ModelRuntimeBuildError::Unavailable(message),
        AppError::ResourceUnsafe(message) | AppError::UnsupportedScheme(message) => {
            ModelRuntimeBuildError::InvalidArtifact(message)
        }
        AppError::Internal(message) => ModelRuntimeBuildError::Internal(message),
        other => {
            ModelRuntimeBuildError::Internal(format!("unexpected runtime build error: {other:?}"))
        }
    })
}

pub(crate) fn build_whisper_runtime(
    entry: &ManifestEntry,
    _: &std::path::Path,
) -> Result<Arc<dyn ModelRuntime>, AppError> {
    let (binary, model) = resolve_whisper_paths().ok_or_else(|| {
        AppError::Conflict(format!("model {} not installed; download first", entry.id))
    })?;
    let resources = PreflightedWhisperResources::preflight(entry.id.clone(), binary, model)
        .map_err(|error| {
            AppError::Conflict(format!("model {} not installed: {error}", entry.id))
        })?;
    BackendRegistry::build(BackendResources::Whisper(resources))
        .map_err(|error| AppError::Conflict(format!("model {} not installed: {error}", entry.id)))
}

pub(crate) fn build_funasr_runtime(
    entry: &ManifestEntry,
    extra_root: &std::path::Path,
) -> Result<Arc<dyn ModelRuntime>, AppError> {
    let (python, sidecar, models_root) = resolve_funasr_paths().ok_or_else(|| {
        AppError::Conflict(format!("model {} not installed; download first", entry.id))
    })?;
    let resources = PreflightedFunAsrResources::preflight(
        entry.id.clone(),
        python,
        sidecar,
        models_root,
        "mps".into(),
        Some(extra_root.to_path_buf()),
    )
    .map_err(|error| AppError::Conflict(format!("model {} not installed: {error}", entry.id)))?;
    BackendRegistry::build(BackendResources::FunAsr(resources))
        .map_err(|error| AppError::Conflict(format!("model {} not installed: {error}", entry.id)))
}

pub(crate) fn build_gguf_runtime_from_catalog(
    entry: &ManifestEntry,
    _: &std::path::Path,
) -> Result<Arc<dyn ModelRuntime>, AppError> {
    build_gguf_runtime(
        entry,
        resolve_gguf_root().ok_or_else(|| {
            AppError::Conflict(format!(
                "model {} not installed; GGUF root unavailable",
                entry.id
            ))
        })?,
    )
}

pub(crate) fn build_sherpa_runtime(
    entry: &ManifestEntry,
    _: &std::path::Path,
) -> Result<Arc<dyn ModelRuntime>, AppError> {
    let (root, requirements) = sherpa_artifact_location(entry).ok_or_else(|| {
        AppError::Conflict(format!(
            "model {} has incomplete Sherpa artifact metadata",
            entry.id
        ))
    })?;
    build_sherpa_runtime_verified(entry, root, requirements)
}

pub(crate) fn build_sherpa_runtime_verified(
    entry: &ManifestEntry,
    root: PathBuf,
    requirements: ArtifactRequirements,
) -> Result<Arc<dyn ModelRuntime>, AppError> {
    // Registry construction always performs a full rehash; it deliberately
    // bypasses the listing-only metadata cache and checks the catalog total.
    let resources =
        VerifiedSherpaResources::verify(entry.id.clone(), root, requirements, entry.size_bytes)
            .map_err(|error| {
                AppError::Conflict(format!("model {} not installed: {error}", entry.id))
            })?;
    BackendRegistry::build(BackendResources::SherpaOnnx(resources))
        .map_err(|error| AppError::Conflict(format!("model {} not installed: {error}", entry.id)))
}

pub(crate) fn build_gguf_runtime(
    entry: &ManifestEntry,
    root: PathBuf,
) -> Result<Arc<dyn ModelRuntime>, AppError> {
    if !gguf_entry_is_installed_at(entry, &root) {
        return Err(AppError::Conflict(format!(
            "model {} not installed: GGUF runtime manifest does not match the embedded model catalog",
            entry.id
        )));
    }
    let requirements = gguf_artifact_requirements(entry).ok_or_else(|| {
        AppError::Conflict(format!(
            "model {} has no trusted GGUF artifact metadata",
            entry.id
        ))
    })?;
    let resources =
        VerifiedGgufResources::verify(entry.id.clone(), root, requirements, entry.size_bytes)
            .map_err(|error| {
                AppError::Conflict(format!("model {} not installed: {error}", entry.id))
            })?;
    BackendRegistry::build(BackendResources::Gguf(resources))
        .map_err(|error| AppError::Conflict(format!("model {} not installed: {error}", entry.id)))
}
