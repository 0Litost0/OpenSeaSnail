//! HTTP-independent embedded model catalog and validation rules.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use super::ApplicationError;

/// Runtime kind declared by the embedded model catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModelRuntimeKind {
    #[serde(rename = "whisper")]
    Whisper,
    #[serde(rename = "funasr")]
    FunAsr,
    #[serde(rename = "gguf")]
    Gguf,
    #[serde(rename = "sherpa_onnx")]
    SherpaOnnx,
}

impl ModelRuntimeKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Whisper => "whisper",
            Self::FunAsr => "funasr",
            Self::Gguf => "gguf",
            Self::SherpaOnnx => "sherpa_onnx",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimestampCapability {
    None,
    Segment,
    TokenStart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelDiarization {
    Builtin,
    External,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelStatusResult {
    NotInstalled,
    Installed,
    Active,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelComponentResult {
    pub id: String,
    pub bundled: bool,
    pub downloaded: bool,
    pub enabled: bool,
    pub toggleable: bool,
    pub size_bytes: u64,
    pub download_progress: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelSummaryResult {
    pub id: String,
    pub runtime: ModelRuntimeKind,
    pub size_bytes: u64,
    pub languages: Vec<String>,
    pub diarization: ModelDiarization,
    pub status: ModelStatusResult,
    pub bundled: bool,
    pub default: bool,
    pub components: Vec<ModelComponentResult>,
}

/// A validated row from the embedded catalog. Fields stay crate-private so
/// transport adapters cannot manufacture entries from request data.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestEntry {
    pub(crate) id: String,
    pub(crate) runtime: ModelRuntimeKind,
    pub(crate) size_bytes: u64,
    pub(crate) languages: Vec<String>,
    pub(crate) diarization: ModelDiarization,
    pub(crate) bundled: bool,
    #[serde(default)]
    pub(crate) default: bool,
    #[serde(default)]
    pub(crate) family: Option<String>,
    #[serde(default)]
    pub(crate) variant: Option<String>,
    #[serde(default)]
    pub(crate) artifact_key: Option<String>,
    #[serde(default)]
    pub(crate) artifact_manifest_sha256: Option<String>,
    #[serde(default)]
    pub(crate) timestamp_capability: Option<TimestampCapability>,
    #[serde(default)]
    #[serde(rename = "sha256")]
    pub(crate) _legacy_sha256: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    models: Vec<ManifestEntry>,
}

#[derive(Debug, Clone)]
pub struct ModelCatalog {
    entries: Vec<ManifestEntry>,
}

impl ModelCatalog {
    pub fn from_manifest_str(source: &str) -> Result<Self, ApplicationError> {
        let manifest: Manifest = serde_json::from_str(source).map_err(|error| {
            ApplicationError::Internal(format!("invalid models manifest: {error}"))
        })?;
        validate_catalog(&manifest).map_err(|reason| {
            ApplicationError::Internal(format!("invalid models manifest: {reason}"))
        })?;
        Ok(Self {
            entries: manifest.models,
        })
    }

    pub(crate) fn get(&self, id: &str) -> Option<&ManifestEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    pub(crate) fn list(&self) -> &[ManifestEntry] {
        &self.entries
    }
}

fn validate_catalog(manifest: &Manifest) -> Result<(), String> {
    if !matches!(manifest.version, 1 | 2) || manifest.models.is_empty() {
        return Err("unsupported version or empty model list".into());
    }
    let mut ids = HashSet::new();
    let mut default_count = 0_usize;
    for entry in &manifest.models {
        if !valid_catalog_identifier(&entry.id)
            || entry.size_bytes == 0
            || entry.languages.is_empty()
            || !ids.insert(entry.id.as_str())
        {
            return Err("model IDs must be unique and entry metadata must be non-empty".into());
        }
        default_count += usize::from(entry.default);
    }
    if default_count != 1 {
        return Err("catalog must contain exactly one default model".into());
    }
    if !manifest
        .models
        .iter()
        .any(|entry| entry.default && entry.bundled)
    {
        return Err("catalog default model must be bundled".into());
    }
    if manifest.version == 1 {
        return Ok(());
    }

    let mut artifact_keys = HashSet::new();
    let mut identities = HashSet::new();
    for entry in &manifest.models {
        let family = entry
            .family
            .as_deref()
            .ok_or_else(|| format!("model {:?} is missing family", entry.id))?;
        let variant = entry
            .variant
            .as_deref()
            .ok_or_else(|| format!("model {:?} is missing variant", entry.id))?;
        let artifact_key = entry
            .artifact_key
            .as_deref()
            .ok_or_else(|| format!("model {:?} is missing artifact_key", entry.id))?;
        let manifest_sha256 = entry
            .artifact_manifest_sha256
            .as_deref()
            .ok_or_else(|| format!("model {:?} is missing manifest hash", entry.id))?;
        if !valid_catalog_identifier(family)
            || !valid_catalog_identifier(variant)
            || entry.timestamp_capability.is_none()
            || !valid_sha256(manifest_sha256)
        {
            return Err(format!("model {:?} has invalid v2 metadata", entry.id));
        }
        let expected_key = format!("{family}/{}/{variant}", entry.runtime.as_str());
        if artifact_key != expected_key || !safe_artifact_key(artifact_key) {
            return Err(format!(
                "model {:?} has an inconsistent artifact_key",
                entry.id
            ));
        }
        if !artifact_keys.insert(artifact_key)
            || !identities.insert((family, entry.runtime.as_str(), variant))
        {
            return Err(
                "artifact keys and family/runtime/variant identities must be unique".into(),
            );
        }
    }
    Ok(())
}

fn valid_catalog_identifier(value: &str) -> bool {
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

pub(crate) fn safe_artifact_key(value: &str) -> bool {
    !value.contains('\\')
        && !std::path::Path::new(value).is_absolute()
        && std::path::Path::new(value)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}
