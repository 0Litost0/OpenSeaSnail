use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=SEASNAIL_MODELS_CATALOG");
    let default = PathBuf::from("resources/models.json");
    let source = env::var_os("SEASNAIL_MODELS_CATALOG")
        .map(PathBuf::from)
        .unwrap_or(default);
    println!("cargo:rerun-if-changed={}", source.display());
    let destination = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("models.json");
    let text = fs::read_to_string(&source).unwrap_or_else(|error| {
        panic!("failed to read model catalog {}: {error}", source.display())
    });
    validate_catalog(&text, &source);
    fs::write(&destination, text).unwrap_or_else(|error| {
        panic!(
            "failed to copy model catalog {} to {}: {error}",
            source.display(),
            destination.display()
        )
    });
}

fn validate_catalog(text: &str, source: &Path) {
    let root: serde_json::Value = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("invalid model catalog {}: {error}", source.display()));
    let models = root
        .get("models")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("model catalog {} has no models array", source.display()));
    let version = root
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_else(|| panic!("model catalog {} has no version", source.display()));
    assert!(
        matches!(version, 1 | 2) && !models.is_empty(),
        "model catalog {} has unsupported version or no models",
        source.display()
    );
    let mut ids = HashSet::new();
    let default_count = models
        .iter()
        .filter(|entry| {
            let id = entry
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_else(|| {
                    panic!("model catalog {} has entry without id", source.display())
                });
            assert!(
                ids.insert(id),
                "model catalog {} has duplicate id {id}",
                source.display()
            );
            entry.get("default").and_then(serde_json::Value::as_bool) == Some(true)
        })
        .count();
    assert_eq!(
        default_count,
        1,
        "model catalog {} must contain exactly one default",
        source.display()
    );

    if version == 2 {
        let mut keys = HashSet::new();
        let mut identities = HashSet::new();
        for entry in models {
            let id = required_string(entry, "id", source);
            let runtime = required_string(entry, "runtime", source);
            let family = required_string(entry, "family", source);
            let variant = required_string(entry, "variant", source);
            let artifact_key = required_string(entry, "artifact_key", source);
            let manifest_hash = required_string(entry, "artifact_manifest_sha256", source);
            let timestamp_capability = required_string(entry, "timestamp_capability", source);
            let expected_key = format!("{family}/{runtime}/{variant}");
            assert!(
                artifact_key == expected_key
                    && artifact_key.split('/').count() == 3
                    && !artifact_key.contains("..")
                    && !artifact_key.contains('\\')
                    && !artifact_key.starts_with('/'),
                "model catalog {} entry {id} has invalid artifact_key",
                source.display()
            );
            assert!(
                manifest_hash.len() == 64
                    && manifest_hash
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
                "model catalog {} entry {id} has invalid manifest hash",
                source.display()
            );
            assert!(
                matches!(timestamp_capability, "none" | "segment" | "token_start"),
                "model catalog {} entry {id} has invalid timestamp capability",
                source.display()
            );
            assert!(
                keys.insert(artifact_key) && identities.insert((family, runtime, variant)),
                "model catalog {} has duplicate artifact identity",
                source.display()
            );
            assert!(
                entry
                    .get("size_bytes")
                    .and_then(serde_json::Value::as_u64)
                    .is_some_and(|size| size > 0),
                "model catalog {} entry {id} has invalid size",
                source.display()
            );
        }
        let default = models
            .iter()
            .find(|entry| entry.get("default").and_then(serde_json::Value::as_bool) == Some(true))
            .expect("default count was checked above");
        assert!(
            default.get("bundled").and_then(serde_json::Value::as_bool) == Some(true),
            "model catalog {} default must be bundled",
            source.display()
        );
        return;
    }

    let entries: Vec<_> = models
        .iter()
        .filter(|entry| {
            entry.get("id").and_then(serde_json::Value::as_str) == Some("sensevoice-small")
        })
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "model catalog {} must contain one sensevoice-small entry",
        source.display()
    );
    let entry = entries[0];
    let valid_variant = matches!(
        entry.get("variant").and_then(serde_json::Value::as_str),
        Some("q8" | "f16" | "f32")
    );
    let valid_hash = entry
        .get("sha256")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        });
    assert!(
        entry.get("runtime").and_then(serde_json::Value::as_str) == Some("gguf")
            && entry.get("bundled").and_then(serde_json::Value::as_bool) == Some(true)
            && entry.get("default").and_then(serde_json::Value::as_bool) == Some(true)
            && entry
                .get("size_bytes")
                .and_then(serde_json::Value::as_u64)
                .is_some()
            && valid_variant
            && valid_hash,
        "model catalog {} has invalid SenseVoice GGUF metadata",
        source.display()
    );
}

fn required_string<'a>(entry: &'a serde_json::Value, field: &str, source: &Path) -> &'a str {
    entry
        .get(field)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "model catalog {} entry is missing {field}",
                source.display()
            )
        })
}
