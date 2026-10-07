//! Manual/CI real-chain gate for the locked macOS arm64 Sherpa artifact.

use seasnail_runtime::contract::TranscribeReq;
use seasnail_runtime::{
    verify_artifact, ArtifactIdentity, ArtifactRequirements, BackendRegistry, BackendResources,
    VerifiedSherpaResources,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[ignore = "requires SEASNAIL_SHERPA_REAL_ROOT and SEASNAIL_SHERPA_REAL_WAV"]
#[tokio::test]
async fn locked_artifact_runs_inherited_fd_driver_chain() {
    let Some(root) = std::env::var_os("SEASNAIL_SHERPA_REAL_ROOT").map(PathBuf::from) else {
        return;
    };
    let Some(wav) = std::env::var_os("SEASNAIL_SHERPA_REAL_WAV").map(PathBuf::from) else {
        return;
    };
    let requirements = ArtifactRequirements {
        identity: ArtifactIdentity {
            runtime: "sherpa_onnx".into(),
            catalog_id: "sensevoice-small-sherpa-int8".into(),
            family: "sensevoice-small".into(),
            variant: "int8".into(),
            api_contract_version: 1,
        },
        manifest_sha256: "c2e2548be277f00de73601c12bfe17c4b414bfaa75f46d0c1d4bd4018fc32e73".into(),
        roles: BTreeMap::from([
            ("model".into(), false),
            ("onnxruntime".into(), false),
            ("sherpa-onnx".into(), true),
            ("sidecar".into(), true),
            ("tokens".into(), false),
            ("vad".into(), false),
        ]),
        architectures: BTreeMap::from([
            ("onnxruntime".into(), "arm64".into()),
            ("sherpa-onnx".into(), "arm64".into()),
            ("sidecar".into(), "arm64".into()),
        ]),
    };
    let root = root.canonicalize().unwrap();
    let expected_size_bytes = verify_artifact(&root, &requirements).unwrap().size_bytes;
    let resources = VerifiedSherpaResources::verify(
        "sensevoice-small-sherpa-int8".into(),
        root,
        requirements,
        expected_size_bytes,
    )
    .unwrap();
    let driver = BackendRegistry::build(BackendResources::SherpaOnnx(resources)).unwrap();
    driver.start(0).await.unwrap();
    assert!(driver.health().await);
    let result = driver
        .transcribe(TranscribeReq {
            wav,
            language: None,
            prompt: None,
            punc: None,
            spk: None,
        })
        .await
        .unwrap();
    assert!(!result.text.is_empty());
    assert!(!result.segments.is_empty());
    assert!(!result.words.is_empty(), "M4 must expose exact token words");
    assert_eq!(
        result
            .words
            .iter()
            .map(|word| word.text.as_str())
            .collect::<String>(),
        result.text,
        "real locked sample must reconstruct the authoritative text exactly"
    );
    assert!(result
        .segments
        .windows(2)
        .all(|pair| pair[0].end <= pair[1].start));
    driver.stop().await.unwrap();
    assert!(!driver.health().await);
}
