//! 模型管理端点（ST-M3.10 基座）：`GET /models` / `POST /models/{id}`。
//!
//! 对应 `proto/openapi.yaml` `/models`。仅本地 UI（`sessions:read` / `sessions:write`）。
//!
//! **模型目录**：bundled 清单 `resources/models.json`（`include_str!` 编译期内嵌，
//! 单一事实源）枚举可用模型条目（id / runtime / size / languages / diarization /
//! bundled / default）。运行期 `ModelCatalog` 持解析后清单，handler 查/列。
//!
//! **状态语义**（`GET /models` 每条 `status`）：
//! - `active`：`registry.active_id() == id`（该条目为当前活跃 runtime）。
//! - `installed`：运行时所需的所有本地路径均存在 → 能立即 start。FunASR 优先读取
//!   `SEASNAIL_FUNASR_ROOT`（开发/测试），发布版则从
//!   `SeaSnail.app/Contents/Resources/funasr` 解析；绝不回退到系统 Python。
//! - `not_installed`：路径未就绪（dev 未设 env；非 bundled 模型未下载）。
//!
//! `downloading` 状态由 `download_progress` 反映（M4 按需下载已接通，punc）。
//!
//! **activate 流程**（`POST /models/{id} action=activate`）由 application
//! `ModelService` 独占：adapter 只解析 action、调用用例并映射 DTO/status。
//!
//! **幂等不重启**：已激活同 id → 202 不重启。崩溃 runtime 的恢复走 retry 链路
//!（[`retry_session`](crate::api::sessions) 先 health-check 再 stop+start），非
//! activate 职责——故幂等分支不 health-check（避免引入未测的 fallthrough 重启路径）。
//!
//! 多模型下载（punc 已接通 M4，spk 后置）/ DELETE /models / per-model is_installed
//! 中 `download_progress` 由后台下载任务写入、`GET /models` 轮询读出。

#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::{collections::HashMap, sync::Mutex};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::{AuthedCaller, HttpState};
#[cfg(test)]
use crate::application::{ModelCatalog, TimestampCapability};
use crate::application::{
    ModelComponentResult, ModelDiarization, ModelRuntimeKind, ModelStatusResult, ModelSummaryResult,
};
use crate::error::AppError;
#[cfg(test)]
use crate::model_runtime::*;
#[cfg(test)]
use crate::model_settings::ModelSettings;

#[cfg(test)]
use crate::downloader::{ManifestComponent, ManifestFile, ManifestRoot};

// ── DTO / 清单类型 ──────────────────────────────────────────────────────────────

/// 模型状态（OpenAPI `status` 枚举）。
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelStatus {
    NotInstalled,
    Installed,
    Active,
}

/// `GET /models` / `POST /models/{id}` 响应体（OpenAPI `Model` schema）。
#[derive(Debug, Clone, Serialize)]
pub struct ModelDto {
    pub id: String,
    pub runtime: ModelRuntimeKind,
    pub size_bytes: u64,
    pub languages: Vec<String>,
    pub diarization: ModelDiarization,
    pub status: ModelStatus,
    pub bundled: bool,
    pub default: bool,
    /// funasr 后端的可选组件（punc/spk）状态；无组件的运行时为空（序列化省略）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<ComponentDto>,
}

/// funasr 后端可选组件（punc/spk）状态（OpenAPI `Component` schema，M3.1）。
#[derive(Debug, Clone, Serialize)]
pub struct ComponentDto {
    pub id: String,
    /// 是否随 app 内置（builtin root 有该 component dir）。
    pub bundled: bool,
    /// 已下载（用户目录或内置有该 component → 可懒挂载）。
    pub downloaded: bool,
    /// 开关（daemon settings，懒加载：下条转写才挂/卸）。
    pub enabled: bool,
    /// UI 是否可操作（spk 后置→false"敬请期待"；punc→true）。UI 据此分支，不硬编码 id（M3 review M2）。
    pub toggleable: bool,
    /// 组件体积（M4.1 随包 models-manifest.json 提供权威值；M3.1 暂 0）。
    pub size_bytes: u64,
    /// 下载进度 0.0–1.0（M4 下载期内存态；M3.1 = None）。
    pub download_progress: Option<f32>,
}

impl From<ModelComponentResult> for ComponentDto {
    fn from(component: ModelComponentResult) -> Self {
        Self {
            id: component.id,
            bundled: component.bundled,
            downloaded: component.downloaded,
            enabled: component.enabled,
            toggleable: component.toggleable,
            size_bytes: component.size_bytes,
            download_progress: component.download_progress,
        }
    }
}

impl From<ModelSummaryResult> for ModelDto {
    fn from(model: ModelSummaryResult) -> Self {
        Self {
            id: model.id,
            runtime: model.runtime,
            size_bytes: model.size_bytes,
            languages: model.languages,
            diarization: model.diarization,
            status: match model.status {
                ModelStatusResult::NotInstalled => ModelStatus::NotInstalled,
                ModelStatusResult::Installed => ModelStatus::Installed,
                ModelStatusResult::Active => ModelStatus::Active,
            },
            bundled: model.bundled,
            default: model.default,
            components: model.components.into_iter().map(Into::into).collect(),
        }
    }
}

// ── 组件状态（M3.1）─────────────────────────────────────────────────────────────

/// funasr 后端的可选组件（punc/spk）状态。`size_bytes` 取随包 manifest 逐文件求和
/// （M4.1 权威值）；`downloaded` 对 extra（用户下载）按 manifest 全文件 size 校验，消
/// partial 目录的 `is_dir()` 误判（与 `put_component` 的 `is_component_downloaded`、
/// 下载触发的 `component_complete` 三处统一）；spk 后置列出但 enabled=false。
#[cfg(test)]
fn funasr_components(
    builtin: Option<&std::path::Path>,
    extra: &std::path::Path,
    settings: &ModelSettings,
    progress: &Arc<Mutex<HashMap<String, f32>>>,
) -> Vec<ComponentDto> {
    let builtin_models = builtin.map(|b| b.join("models"));
    // 一次解析随包 manifest（builtin/models/models-manifest.json），供 size_bytes 与
    // downloaded 的逐文件校验复用；无 bundle/无 manifest/解析失败 → None，回落旧行为。
    let manifest: Option<ManifestRoot> = builtin_models.as_ref().and_then(|m| {
        std::fs::read_to_string(m.join("models-manifest.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    });
    ["punc", "spk"]
        .iter()
        .map(|&id| {
            let bundled = builtin_models
                .as_ref()
                .map(|m| m.join(id).is_dir())
                .unwrap_or(false);
            let comp = manifest
                .as_ref()
                .and_then(|m| m.models.iter().find(|c| c.role == id));
            // downloaded：内置 bundled（随包完整）或 extra 按 manifest 逐文件 size 校验；
            // 无 manifest（如测试 temp bundle）→ 回落 is_dir() 保旧行为。
            let downloaded = if bundled {
                true
            } else if let Some(c) = comp {
                component_complete(c, &extra.join(id))
            } else {
                extra.join(id).is_dir()
            };
            ComponentDto {
                id: id.into(),
                bundled,
                downloaded,
                enabled: settings.is_enabled(id),
                toggleable: id == "punc", // spk 后置，UI 不可操作（"敬请期待"）
                size_bytes: comp
                    .map(|c| c.files.iter().map(|f| f.size_bytes).sum())
                    .unwrap_or(0),
                download_progress: progress.lock().unwrap().get(id).copied(),
            }
        })
        .collect()
}

/// 组件是否已完整下载到 `dest_dir`：manifest 全文件存在且 size 匹配。
/// 防 partial 目录误判（download_file 失败后组件目录已建但文件不全，R1）。
#[cfg(test)]
fn component_complete(comp: &ManifestComponent, dest_dir: &std::path::Path) -> bool {
    comp.files.iter().all(|f| {
        std::fs::metadata(dest_dir.join(&f.path))
            .map(|m| m.len() == f.size_bytes)
            .unwrap_or(false)
    })
}

/// `POST /models/{id}` 请求体：`{action: "activate"}`。
/// 用 String + match（非 serde 枚举）：未知 action → 409（openapi 无 400/422，
/// 未知动作作「与当前支持冲突」映射 409，message 明示）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelActionReq {
    action: String,
}

// ── handler ────────────────────────────────────────────────────────────────────

/// `GET /models` — 列全部模型及当前状态（`sessions:read`）。
async fn get_models(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
) -> Result<Json<Vec<ModelDto>>, AppError> {
    let models = state
        .model_service()
        .list(&caller)
        .await
        .map_err(AppError::from)?;
    Ok(Json(models.into_iter().map(Into::into).collect()))
}

/// `POST /models/{id}` — activate 模型（`sessions:write`）。
async fn model_action(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path(id): Path<String>,
    Json(req): Json<ModelActionReq>,
) -> Result<(StatusCode, Json<ModelDto>), AppError> {
    match req.action.as_str() {
        "activate" => {
            let model = state
                .model_service()
                .activate(&caller, &id)
                .await
                .map_err(AppError::from)?;
            Ok((StatusCode::ACCEPTED, Json(model.into())))
        }
        other => Err(AppError::Conflict(format!("unsupported action: {other}"))),
    }
}

/// `PUT /models/{component}` 请求体（M3.2）。
#[derive(Debug, Deserialize)]
struct ComponentEnableReq {
    enabled: bool,
}

/// `PUT /models/{component} {enabled}` — 开/关可选组件（M3.2，懒：仅持久化，下条转写才挂/卸）。
/// `{component}`∈{punc,spk}（未知→404）；spk 后置→422（mount 未接，UI 应禁用）；punc 未下载
/// 即开启→422。注：activate 用 409"download first"（M3.10 既有约定 + 复用为 in-flight 冲突），
/// 此处用 422（设计要求；not-downloaded 是语义前置条件，非冲突）——有意 split（M3 review M1）。
/// `set_enabled` 同步 fsync under `std::Mutex`（M1.2 review L2）→ `spawn_blocking` 不阻 executor；
/// downloaded 检查入 closure 内消 TOCTOU（M3 review L2）。
async fn put_component(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path(component): Path<String>,
    Json(req): Json<ComponentEnableReq>,
) -> Result<StatusCode, AppError> {
    state
        .model_service()
        .set_component_enabled(&caller, component, req.enabled)
        .await
        .map_err(AppError::from)?;
    Ok(StatusCode::ACCEPTED)
}

/// `POST /models/{component}/download` — 触发后台按需下载（M4：punc）。懒加载配套：下载完成
/// 后再开开关、首条 punc=true 转写即懒挂载（sidecar 按请求重解析 punc 路径）。`{component}`∈
/// {punc,spk}（未知→404）；spk 后置→422；已下载→202 幂等；进行中→409；无 bundle/manifest→503。
/// 进度写入进程内 `download_progress` map，经 GET /models 轮询（download_progress ∈ [0,1)）。
async fn download_component_handler(
    State(state): State<HttpState>,
    AuthedCaller(caller): AuthedCaller,
    Path(component): Path<String>,
) -> Result<StatusCode, AppError> {
    state
        .model_service()
        .download_component(&caller, &component)
        .map_err(AppError::from)?;
    Ok(StatusCode::ACCEPTED)
}

/// models 资源子路由。
pub fn routes() -> Router<HttpState> {
    Router::new()
        .route("/models", get(get_models))
        .route("/models/:id", post(model_action).put(put_component))
        .route(
            "/models/:component/download",
            post(download_component_handler),
        )
}

#[cfg(test)]
mod tests {
    //! HTTP adapter 的 catalog/artifact/factory characterization；模型切换与补偿由
    //! application `ModelService` fault-injection tests 覆盖。
    use super::*;
    use std::path::PathBuf;

    use seasnail_runtime::{ArtifactIdentity, ArtifactVerificationCache, VerifiedArtifact};
    use sha2::{Digest, Sha256};

    fn sha(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn gguf_fixture_with_server(server: &[u8]) -> tempfile::TempDir {
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        let files = [
            ("sensevoice-server", server),
            ("sensevoice.gguf", b"model".as_slice()),
            ("fsmn-vad.gguf", b"vad".as_slice()),
        ];
        for (name, bytes) in files {
            fs::write(dir.path().join(name), bytes).unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                dir.path().join("sensevoice-server"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let manifest_files: Vec<_> = files
            .iter()
            .map(|(path, bytes)| {
                serde_json::json!({
                    "path": path,
                    "sha256": sha(bytes),
                    "size_bytes": bytes.len(),
                })
            })
            .collect();
        fs::write(
            dir.path().join("runtime-manifest.json"),
            serde_json::json!({
                "schema_version": 1,
                "runtime": "gguf",
                "model_id": "sensevoice-small",
                "variant": "q8",
                "model_file": "sensevoice.gguf",
                "vad_file": "fsmn-vad.gguf",
                "source_revision": "6991744856587fa44379e8b5dcc432debffeb1be",
                "api_contract_version": 1,
                "files": manifest_files,
            })
            .to_string(),
        )
        .unwrap();
        dir
    }

    fn gguf_fixture() -> tempfile::TempDir {
        gguf_fixture_with_server(b"server")
    }

    fn fixture_catalog(root: &std::path::Path) -> ModelCatalog {
        let manifest = std::fs::read(root.join("runtime-manifest.json")).unwrap();
        let size_bytes = ["sensevoice-server", "sensevoice.gguf", "fsmn-vad.gguf"]
            .iter()
            .map(|name| std::fs::metadata(root.join(name)).unwrap().len())
            .sum::<u64>();
        ModelCatalog::from_manifest_str(
            &serde_json::json!({
                "version": 2,
                "models": [{
                    "id": "sensevoice-small-gguf-q8",
                    "runtime": "gguf",
                    "family": "sensevoice-small",
                    "variant": "q8",
                    "size_bytes": size_bytes,
                    "artifact_key": "sensevoice-small/gguf/q8",
                    "artifact_manifest_sha256": sha(&manifest),
                    "timestamp_capability": "token_start",
                    "languages": ["zh"],
                    "diarization": "none",
                    "bundled": true,
                    "default": true,
                }]
            })
            .to_string(),
        )
        .expect("fixture catalog parses")
    }

    fn v2_catalog(
        models: serde_json::Value,
    ) -> Result<ModelCatalog, crate::application::ApplicationError> {
        ModelCatalog::from_manifest_str(
            &serde_json::json!({"version": 2, "models": models}).to_string(),
        )
    }

    fn v2_entry(id: &str, variant: &str, default: bool) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "runtime": "gguf",
            "family": "sensevoice-small",
            "variant": variant,
            "size_bytes": 11,
            "artifact_key": format!("sensevoice-small/gguf/{variant}"),
            "artifact_manifest_sha256": "a".repeat(64),
            "timestamp_capability": "token_start",
            "languages": ["zh"],
            "diarization": "none",
            "bundled": true,
            "default": default
        })
    }

    fn sherpa_fixture() -> (tempfile::TempDir, ModelCatalog) {
        let asr_root = tempfile::tempdir().unwrap();
        let artifact_key = "sensevoice-small/sherpa_onnx/int8";
        let artifact_root = asr_root.path().join(artifact_key);
        std::fs::create_dir_all(artifact_root.join("lib")).unwrap();
        let files = [
            ("model", "model.int8.onnx", b"model".as_slice(), false),
            (
                "onnxruntime",
                "lib/libonnxruntime.dylib",
                b"\xcf\xfa\xed\xfe\x0c\x00\x00\x01ort".as_slice(),
                false,
            ),
            (
                "sherpa-onnx",
                "lib/libsherpa-onnx-c-api.dylib",
                b"\xcf\xfa\xed\xfe\x0c\x00\x00\x01sherpa".as_slice(),
                true,
            ),
            (
                "sidecar",
                "seasnail-sherpa-sidecar",
                b"\xcf\xfa\xed\xfe\x0c\x00\x00\x01sidecar".as_slice(),
                true,
            ),
            ("tokens", "tokens.txt", b"tokens".as_slice(), false),
            ("vad", "silero_vad.onnx", b"vad".as_slice(), false),
        ];
        for &(_, path, bytes, executable) in &files {
            let path = artifact_root.join(path);
            std::fs::write(&path, bytes).unwrap();
            #[cfg(unix)]
            if executable {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let manifest_files: Vec<_> = files
            .iter()
            .map(|(role, path, bytes, executable)| {
                serde_json::json!({
                    "role": role,
                    "path": path,
                    "sha256": sha(bytes),
                    "size_bytes": bytes.len(),
                    "executable": executable,
                    "architecture": matches!(*role, "onnxruntime" | "sherpa-onnx" | "sidecar").then_some("arm64")
                })
            })
            .collect();
        let manifest = serde_json::json!({
            "schema_version": 1,
            "runtime": "sherpa_onnx",
            "catalog_id": "sensevoice-small-sherpa-int8",
            "family": "sensevoice-small",
            "variant": "int8",
            "api_contract_version": 1,
            "source_revisions": {
                "sherpa-onnx": "0123456789abcdef0123456789abcdef01234567",
                "onnxruntime": "89abcdef0123456789abcdef0123456789abcdef"
            },
            "files": manifest_files
        })
        .to_string();
        std::fs::write(artifact_root.join("artifact-manifest.json"), &manifest).unwrap();
        let size_bytes: usize = files.iter().map(|(_, _, bytes, _)| bytes.len()).sum();
        let catalog = ModelCatalog::from_manifest_str(
            &serde_json::json!({
                "version": 2,
                "models": [{
                    "id": "sensevoice-small-sherpa-int8",
                    "runtime": "sherpa_onnx",
                    "family": "sensevoice-small",
                    "variant": "int8",
                    "size_bytes": size_bytes,
                    "artifact_key": artifact_key,
                    "artifact_manifest_sha256": sha(manifest.as_bytes()),
                    "timestamp_capability": "token_start",
                    "languages": ["zh", "en", "mixed"],
                    "diarization": "none",
                    "bundled": true,
                    "default": true
                }]
            })
            .to_string(),
        )
        .unwrap();
        (asr_root, catalog)
    }

    #[test]
    fn v2_catalog_accepts_unique_complete_entries() {
        let catalog = v2_catalog(serde_json::json!([
            v2_entry("sensevoice-small-gguf-q8", "q8", true),
            v2_entry("sensevoice-small-gguf-f16", "f16", false)
        ]))
        .unwrap();
        let entry = catalog.get("sensevoice-small-gguf-q8").unwrap();
        assert_eq!(entry.family.as_deref(), Some("sensevoice-small"));
        assert_eq!(
            entry.timestamp_capability,
            Some(TimestampCapability::TokenStart)
        );
    }

    #[test]
    fn v2_catalog_rejects_duplicate_ids_keys_identities_and_defaults() {
        let duplicate_id =
            serde_json::json!([v2_entry("same", "q8", true), v2_entry("same", "f16", false)]);
        assert!(v2_catalog(duplicate_id).is_err());

        let mut duplicate_key_second = v2_entry("second", "f16", false);
        duplicate_key_second["artifact_key"] = serde_json::json!("sensevoice-small/gguf/q8");
        assert!(v2_catalog(serde_json::json!([
            v2_entry("first", "q8", true),
            duplicate_key_second
        ]))
        .is_err());

        let duplicate_identity = serde_json::json!([
            v2_entry("first", "q8", true),
            v2_entry("second", "q8", false)
        ]);
        assert!(v2_catalog(duplicate_identity).is_err());

        assert!(v2_catalog(serde_json::json!([
            v2_entry("first", "q8", true),
            v2_entry("second", "f16", true)
        ]))
        .is_err());
        assert!(v2_catalog(serde_json::json!([
            v2_entry("first", "q8", false),
            v2_entry("second", "f16", false)
        ]))
        .is_err());
    }

    #[test]
    fn v2_catalog_rejects_missing_or_inconsistent_cross_fields() {
        for field in [
            "family",
            "variant",
            "artifact_key",
            "artifact_manifest_sha256",
            "timestamp_capability",
        ] {
            let mut entry = v2_entry("candidate", "q8", true);
            entry.as_object_mut().unwrap().remove(field);
            assert!(v2_catalog(serde_json::json!([entry])).is_err(), "{field}");
        }

        let mut absolute = v2_entry("candidate", "q8", true);
        absolute["artifact_key"] = serde_json::json!("/absolute/q8");
        assert!(v2_catalog(serde_json::json!([absolute])).is_err());

        let mut parent = v2_entry("candidate", "q8", true);
        parent["artifact_key"] = serde_json::json!("sensevoice-small/gguf/../q8");
        assert!(v2_catalog(serde_json::json!([parent])).is_err());

        let mut wrong_hash = v2_entry("candidate", "q8", true);
        wrong_hash["artifact_manifest_sha256"] = serde_json::json!("A".repeat(64));
        assert!(v2_catalog(serde_json::json!([wrong_hash])).is_err());
    }

    #[test]
    fn verified_artifact_must_match_catalog_identity_hash_and_total_size() {
        let catalog = v2_catalog(serde_json::json!([v2_entry(
            "sensevoice-small-gguf-q8",
            "q8",
            true
        )]))
        .unwrap();
        let entry = catalog.get("sensevoice-small-gguf-q8").unwrap();
        let mut artifact = VerifiedArtifact {
            root: PathBuf::from("/verified/artifact"),
            identity: ArtifactIdentity {
                runtime: "gguf".into(),
                catalog_id: entry.id.clone(),
                family: "sensevoice-small".into(),
                variant: "q8".into(),
                api_contract_version: 1,
            },
            manifest_sha256: "a".repeat(64),
            source_revisions: Default::default(),
            files: Default::default(),
            size_bytes: 11,
        };
        assert!(artifact_matches_catalog(entry, &artifact));
        artifact.size_bytes += 1;
        assert!(!artifact_matches_catalog(entry, &artifact));
        artifact.size_bytes = 11;
        artifact.identity.catalog_id = "artifact-key-is-not-an-id".into();
        assert!(!artifact_matches_catalog(entry, &artifact));
    }

    #[test]
    fn sherpa_listing_uses_cache_and_activation_performs_full_verification() {
        let (asr_root, catalog) = sherpa_fixture();
        let entry = catalog.get("sensevoice-small-sherpa-int8").unwrap();
        let cache = ArtifactVerificationCache::new();
        let asr_root_path = asr_root.path().canonicalize().unwrap();
        assert!(sherpa_entry_is_installed_at(entry, &asr_root_path, &cache));

        let artifact_root = asr_root_path.join(entry.artifact_key.as_deref().unwrap());
        let requirements = sherpa_artifact_requirements(entry).unwrap();
        let runtime =
            build_sherpa_runtime_verified(entry, artifact_root.clone(), requirements.clone())
                .unwrap();
        assert_eq!(runtime.id(), entry.id);
        assert_eq!(
            runtime.runtime_kind(),
            seasnail_runtime::RuntimeKind::SherpaOnnx
        );

        // Same-size in-place mutation must invalidate listing and full activation.
        std::fs::write(artifact_root.join("model.int8.onnx"), b"wrong").unwrap();
        assert!(!sherpa_entry_is_installed_at(entry, &asr_root_path, &cache));
        assert!(build_sherpa_runtime_verified(entry, artifact_root, requirements).is_err());
    }

    #[test]
    fn duplicate_daemon_backend_descriptors_are_rejected() {
        let duplicate = [
            ModelBackendDescriptor {
                kind: ModelRuntimeKind::Whisper,
                installed: whisper_is_installed,
                build: build_whisper_runtime,
            },
            ModelBackendDescriptor {
                kind: ModelRuntimeKind::Whisper,
                installed: whisper_is_installed,
                build: build_whisper_runtime,
            },
        ];
        assert!(validate_backend_descriptors(&duplicate).is_err());
    }

    #[test]
    fn legacy_whisper_and_funasr_installed_checks_require_complete_resources() {
        let directory = tempfile::tempdir().unwrap();
        let whisper = directory.path().join("whisper-server");
        let whisper_model = directory.path().join("model.bin");
        std::fs::write(&whisper, b"binary").unwrap();
        std::fs::write(&whisper_model, b"model").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&whisper, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(whisper_paths_are_complete(&whisper, &whisper_model));
        std::fs::remove_file(&whisper_model).unwrap();
        assert!(!whisper_paths_are_complete(&whisper, &whisper_model));

        let funasr = directory.path().join("funasr");
        std::fs::create_dir_all(funasr.join("python/bin")).unwrap();
        std::fs::create_dir_all(funasr.join("models/asr")).unwrap();
        std::fs::create_dir_all(funasr.join("models/vad")).unwrap();
        std::fs::write(funasr.join("python/bin/seasnail-funasr"), b"python").unwrap();
        std::fs::write(funasr.join("sidecar.py"), b"sidecar").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                funasr.join("python/bin/seasnail-funasr"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        assert!(resolve_funasr_paths_at(&funasr).is_some());
        std::fs::remove_dir_all(funasr.join("models/vad")).unwrap();
        assert!(resolve_funasr_paths_at(&funasr).is_none());
    }

    #[test]
    fn activation_request_rejects_artifact_key_and_client_size() {
        assert!(serde_json::from_str::<ModelActionReq>(r#"{"action":"activate"}"#).is_ok());
        assert!(serde_json::from_str::<ModelActionReq>(
            r#"{"action":"activate","artifact_key":"x"}"#
        )
        .is_err());
        assert!(
            serde_json::from_str::<ModelActionReq>(r#"{"action":"activate","size_bytes":1}"#)
                .is_err()
        );
    }

    #[test]
    fn production_catalog_contains_only_the_anchored_sherpa_default() {
        let catalog = ModelCatalog::from_manifest_str(include_str!("../../resources/models.json"))
            .expect("bundled model catalog must parse");
        assert_eq!(catalog.list().len(), 1);
        let sherpa = catalog
            .get("sensevoice-small-sherpa-int8")
            .expect("Sherpa entry");
        assert!(sherpa.bundled && sherpa.default);
        assert_eq!(sherpa.runtime, ModelRuntimeKind::SherpaOnnx);
    }

    #[test]
    fn gguf_installed_requires_a_complete_verified_manifest() {
        let dir = gguf_fixture();
        let root = dir.path().canonicalize().unwrap();
        let catalog = fixture_catalog(&root);
        let entry = catalog.get("sensevoice-small-gguf-q8").expect("GGUF entry");
        assert!(
            gguf_entry_is_installed_at(entry, &root),
            "complete fixture is installed"
        );
        std::fs::write(root.join("sensevoice.gguf"), b"other").unwrap();
        assert!(
            !gguf_entry_is_installed_at(entry, &root),
            "hash mismatch is not installed"
        );
    }

    #[test]
    fn gguf_root_override_is_development_only() {
        let dev = PathBuf::from("/explicit-development-root");
        let bundled = PathBuf::from("/signed-app/Contents/Resources/gguf");
        assert_eq!(
            select_gguf_root(Some(dev.clone()), Some(bundled.clone()), true),
            Some(dev.clone()),
            "debug builds accept an explicit development root"
        );
        assert_eq!(
            select_gguf_root(Some(dev), Some(bundled.clone()), false),
            Some(bundled),
            "production ignores environment overrides"
        );
        assert_eq!(
            select_gguf_root(Some(PathBuf::from("/dev")), None, false),
            None,
            "production never falls back to an injected path"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn gguf_factory_builds_a_startable_verified_runtime() {
        let server = br##"#!/usr/bin/env perl
use IO::Socket::INET;
my $s = IO::Socket::INET->new(LocalAddr=>'127.0.0.1',LocalPort=>$ARGV[-1],Proto=>'tcp',Listen=>8,ReuseAddr=>1) or die $!;
while (my $c=$s->accept) { <$c>; print $c "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"; close $c; }
"##;
        let dir = gguf_fixture_with_server(server);
        let root = dir.path().canonicalize().unwrap();
        let catalog = fixture_catalog(&root);
        let entry = catalog.get("sensevoice-small-gguf-q8").expect("GGUF entry");
        let gguf = build_gguf_runtime(entry, root).expect("verified fixture builds GGUF runtime");
        gguf.start(0).await.expect("GGUF runtime starts");
        assert!(gguf.health().await, "GGUF server health is ready");
        gguf.stop().await.unwrap();
    }

    /// M3.1：funasr_components 反映 bundled/downloaded/enabled（builtin/extra/settings）。
    #[test]
    fn funasr_components_reflects_bundled_downloaded_enabled() {
        use crate::model_settings::ModelSettings;
        let dir = tempfile::tempdir().unwrap();
        // builtin root（funasr 根）含 models/punc；extra（用户 models 目录）含 punc；spk 均无。
        let builtin = dir.path().join("builtin");
        std::fs::create_dir_all(builtin.join("models/punc")).unwrap();
        let extra = dir.path().join("extra");
        std::fs::create_dir_all(extra.join("punc")).unwrap();
        let settings = ModelSettings::new(dir.path().join("s"));
        settings.set_enabled("punc", true).unwrap();
        let progress = Arc::new(Mutex::new(HashMap::<String, f32>::new()));
        let cs = funasr_components(Some(&builtin), &extra, &settings, &progress);
        assert_eq!(cs.len(), 2, "punc + spk");
        let punc = cs.iter().find(|c| c.id == "punc").unwrap();
        assert!(punc.bundled, "punc bundled（builtin 有）");
        assert!(punc.downloaded, "punc downloaded（builtin 或 extra 有）");
        assert!(punc.enabled, "punc enabled（settings 设了）");
        assert!(punc.toggleable, "punc toggleable");
        assert!(punc.download_progress.is_none(), "无下载中 → progress None");
        // 下载中：progress map 有 punc 条目 → download_progress=Some(frac)。
        let progress_mid = Arc::new(Mutex::new({
            let mut m = HashMap::<String, f32>::new();
            m.insert("punc".into(), 0.42);
            m
        }));
        let cs2 = funasr_components(Some(&builtin), &extra, &settings, &progress_mid);
        let punc2 = cs2.iter().find(|c| c.id == "punc").unwrap();
        assert_eq!(
            punc2.download_progress,
            Some(0.42),
            "下载中 → progress Some(frac)"
        );
        let spk = cs.iter().find(|c| c.id == "spk").unwrap();
        assert!(
            !spk.bundled && !spk.downloaded && !spk.enabled,
            "spk 均无/关"
        );
        assert!(!spk.toggleable, "spk 后置 toggleable=false");
    }

    /// M4.1：funasr_components 接线 manifest 权威 size_bytes，且 downloaded 走 manifest
    /// 逐文件 size 校验（消 partial 目录的 is_dir 误判）。无 manifest → 回落 is_dir。
    #[test]
    fn funasr_components_uses_manifest_size_and_verified_downloaded() {
        use crate::model_settings::ModelSettings;
        let dir = tempfile::tempdir().unwrap();
        // builtin = 含 manifest 的随包根；manifest 列 punc 两文件（size 3 + 4 = 7）。
        let builtin = dir.path().join("builtin");
        let models = builtin.join("models");
        std::fs::create_dir_all(models.join("punc")).unwrap();
        let manifest = serde_json::json!({
            "format": 1,
            "models": [
                {
                    "role": "punc",
                    "model_id": "iic/punc",
                    "revision": "master",
                    "files": [
                        {"path": "a.bin", "size_bytes": 3, "sha256": "x"},
                        {"path": "sub/b.bin", "size_bytes": 4, "sha256": "y"},
                    ],
                },
            ],
        });
        std::fs::write(models.join("models-manifest.json"), manifest.to_string()).unwrap();
        let extra = dir.path().join("extra");
        std::fs::create_dir_all(extra.join("punc").join("sub")).unwrap();
        std::fs::write(extra.join("punc").join("a.bin"), b"abc").unwrap();
        // 缺 sub/b.bin → partial。
        let settings = ModelSettings::new(dir.path().join("s"));
        let progress = Arc::new(Mutex::new(HashMap::<String, f32>::new()));
        let cs = funasr_components(Some(&builtin), &extra, &settings, &progress);
        let punc = cs.iter().find(|c| c.id == "punc").unwrap();
        assert_eq!(punc.size_bytes, 7, "size_bytes 取 manifest 逐文件求和");
        // builtin/punc 存在 → bundled=true → downloaded=true（随包完整）；extra partial 不影响。
        assert!(punc.bundled && punc.downloaded, "bundled 即 downloaded");

        // 现去掉内置 punc（仅 extra），验 extra 走 manifest 逐文件校验。
        std::fs::remove_dir_all(models.join("punc")).unwrap();
        let cs2 = funasr_components(Some(&builtin), &extra, &settings, &progress);
        let punc2 = cs2.iter().find(|c| c.id == "punc").unwrap();
        assert!(!punc2.bundled, "内置无 punc → bundled=false");
        assert!(
            !punc2.downloaded,
            "extra 缺 sub/b.bin → partial → downloaded=false（消 is_dir 误判）"
        );
        assert_eq!(
            punc2.size_bytes, 7,
            "size_bytes 仍由 manifest 给（与是否下载无关）"
        );

        // 补齐 sub/b.bin → downloaded=true。
        std::fs::write(extra.join("punc").join("sub").join("b.bin"), b"abcd").unwrap();
        let cs3 = funasr_components(Some(&builtin), &extra, &settings, &progress);
        let punc3 = cs3.iter().find(|c| c.id == "punc").unwrap();
        assert!(punc3.downloaded, "extra 全文件齐 → downloaded=true");
    }

    /// M4：component_complete 按 manifest 全文件 size 校验——完整 true、缺文件/size 不符 false。
    #[test]
    fn component_complete_checks_all_files() {
        let dir = tempfile::tempdir().unwrap();
        let comp = ManifestComponent {
            role: "punc".into(),
            model_id: "iic/test".into(),
            revision: "master".into(),
            files: vec![
                ManifestFile {
                    path: "a.bin".into(),
                    size_bytes: 3,
                    sha256: "x".into(),
                },
                ManifestFile {
                    path: "sub/b.bin".into(),
                    size_bytes: 4,
                    sha256: "y".into(),
                },
            ],
        };
        assert!(!component_complete(&comp, dir.path()), "无文件 → 不完整");
        std::fs::write(dir.path().join("a.bin"), b"abc").unwrap();
        assert!(!component_complete(&comp, dir.path()), "缺 b → 不完整");
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/b.bin"), b"abcd").unwrap();
        assert!(
            component_complete(&comp, dir.path()),
            "全文件 size 对 → 完整"
        );
        std::fs::write(dir.path().join("sub/b.bin"), b"abcde").unwrap();
        assert!(
            !component_complete(&comp, dir.path()),
            "b size 不符 → 不完整"
        );
    }
}
