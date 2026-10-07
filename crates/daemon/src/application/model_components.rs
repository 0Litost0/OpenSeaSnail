//! Local optional-model component state and download adapter used by ModelService.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::downloader::{
    download_component, modelscope_url, ManifestComponent, ManifestFile, ManifestRoot, ProgressFn,
};
use crate::model_settings::ModelSettings;

use super::{ApplicationError, ModelComponentResult};

pub(super) fn resolve_funasr_root() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("SEASNAIL_FUNASR_ROOT").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(root));
    }
    let executable = std::env::current_exe().ok()?;
    let macos_dir = executable.parent()?;
    let contents_dir = macos_dir.parent()?;
    Some(contents_dir.join("Resources").join("funasr"))
}

fn load_manifest_component(component: &str) -> Option<ManifestComponent> {
    let root = resolve_funasr_root()?;
    let source = std::fs::read_to_string(root.join("models/models-manifest.json")).ok()?;
    let manifest: ManifestRoot = serde_json::from_str(&source).ok()?;
    manifest
        .models
        .into_iter()
        .find(|item| item.role == component)
}

pub(super) fn component_complete(component: &ManifestComponent, destination: &Path) -> bool {
    component.files.iter().all(|file| {
        std::fs::metadata(destination.join(&file.path))
            .map(|metadata| metadata.len() == file.size_bytes)
            .unwrap_or(false)
    })
}

fn is_component_downloaded(builtin: Option<&Path>, extra: &Path, component: &str) -> bool {
    if builtin.is_some_and(|root| root.join("models").join(component).is_dir()) {
        return true;
    }
    load_manifest_component(component)
        .map(|manifest| component_complete(&manifest, &extra.join(component)))
        .unwrap_or_else(|| extra.join(component).is_dir())
}

pub(super) fn states(
    settings: &ModelSettings,
    progress: &Arc<Mutex<HashMap<String, f32>>>,
) -> Vec<ModelComponentResult> {
    let builtin = resolve_funasr_root();
    let builtin_models = builtin.as_ref().map(|root| root.join("models"));
    let extra = settings.dir().join("models");
    let manifest: Option<ManifestRoot> = builtin_models.as_ref().and_then(|models| {
        std::fs::read_to_string(models.join("models-manifest.json"))
            .ok()
            .and_then(|source| serde_json::from_str(&source).ok())
    });
    ["punc", "spk"]
        .into_iter()
        .map(|id| {
            let bundled = builtin_models
                .as_ref()
                .is_some_and(|models| models.join(id).is_dir());
            let component = manifest
                .as_ref()
                .and_then(|manifest| manifest.models.iter().find(|item| item.role == id));
            let downloaded = if bundled {
                true
            } else if let Some(component) = component {
                component_complete(component, &extra.join(id))
            } else {
                extra.join(id).is_dir()
            };
            ModelComponentResult {
                id: id.into(),
                bundled,
                downloaded,
                enabled: settings.is_enabled(id),
                toggleable: id == "punc",
                size_bytes: component
                    .map(|item| item.files.iter().map(|file| file.size_bytes).sum())
                    .unwrap_or(0),
                download_progress: progress
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .get(id)
                    .copied(),
            }
        })
        .collect()
}

pub(super) fn set_enabled(
    settings: &ModelSettings,
    component: &str,
    enabled: bool,
) -> Result<(), ApplicationError> {
    if !matches!(component, "punc" | "spk") {
        return Err(ApplicationError::NotFound(format!(
            "unknown component: {component}"
        )));
    }
    if component == "spk" {
        return Err(ApplicationError::UnprocessableEntity(
            "spk not yet supported; coming soon".into(),
        ));
    }
    let builtin = resolve_funasr_root();
    let extra = settings.dir().join("models");
    if enabled && !is_component_downloaded(builtin.as_deref(), &extra, component) {
        return Err(ApplicationError::UnprocessableEntity(format!(
            "component {component} not downloaded; download first"
        )));
    }
    settings
        .set_enabled(component, enabled)
        .map_err(|error| ApplicationError::Internal(format!("set_enabled persist: {error}")))
}

struct ProgressGuard {
    progress: Arc<Mutex<HashMap<String, f32>>>,
    component: String,
}

impl Drop for ProgressGuard {
    fn drop(&mut self) {
        if let Ok(mut progress) = self.progress.lock() {
            progress.remove(&self.component);
        }
    }
}

pub(super) fn start_download(
    settings: &ModelSettings,
    progress: &Arc<Mutex<HashMap<String, f32>>>,
    component: &str,
) -> Result<(), ApplicationError> {
    if !matches!(component, "punc" | "spk") {
        return Err(ApplicationError::NotFound(format!(
            "unknown component: {component}"
        )));
    }
    if component == "spk" {
        return Err(ApplicationError::UnprocessableEntity(
            "spk not yet supported; coming soon".into(),
        ));
    }
    if progress
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains_key(component)
    {
        return Err(ApplicationError::Conflict(format!(
            "download already in progress: {component}"
        )));
    }
    let root = resolve_funasr_root().ok_or_else(|| {
        ApplicationError::ServiceUnavailable(
            "FunASR bundle not available; install FunASR first".into(),
        )
    })?;
    let source =
        std::fs::read_to_string(root.join("models/models-manifest.json")).map_err(|error| {
            ApplicationError::ServiceUnavailable(format!("read models-manifest: {error}"))
        })?;
    let manifest: ManifestRoot = serde_json::from_str(&source)
        .map_err(|error| ApplicationError::Internal(format!("parse models-manifest: {error}")))?;
    let manifest_component = manifest
        .models
        .into_iter()
        .find(|item| item.role == component)
        .ok_or_else(|| {
            ApplicationError::NotFound(format!("component {component} not in manifest"))
        })?;
    let destination = settings.dir().join("models").join(component);
    if component_complete(&manifest_component, &destination) {
        return Ok(());
    }
    {
        let mut entries = progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if entries.contains_key(component) {
            return Err(ApplicationError::Conflict(format!(
                "download already in progress: {component}"
            )));
        }
        entries.insert(component.into(), 0.0);
    }

    let progress = Arc::clone(progress);
    let component_id = component.to_owned();
    tokio::spawn(async move {
        let _guard = ProgressGuard {
            progress: Arc::clone(&progress),
            component: component_id.clone(),
        };
        let model_id = manifest_component.model_id.clone();
        let revision = manifest_component.revision.clone();
        let url_for = move |file: &ManifestFile| modelscope_url(&model_id, &revision, &file.path);
        let progress_fn: ProgressFn = {
            let progress = Arc::clone(&progress);
            let component_id = component_id.clone();
            Arc::new(move |done, total| {
                let fraction = if total > 0 {
                    (done as f32 / total as f32).min(1.0)
                } else {
                    0.0
                };
                if let Ok(mut entries) = progress.lock() {
                    entries.insert(component_id.clone(), fraction);
                }
            })
        };
        match download_component(&manifest_component, &destination, progress_fn, url_for).await {
            Ok(()) => tracing::info!(component = %component_id, "model download completed"),
            Err(error) => {
                tracing::error!(component = %component_id, error = %error, "model download failed")
            }
        }
    });
    Ok(())
}
