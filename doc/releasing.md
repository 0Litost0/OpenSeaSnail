# Public release checklist

This checklist is a release gate, not a record of approval. Publishing source, changing repository visibility, and uploading an App are separate operations. Prepare the following evidence for the exact release revision before publishing.

## Source release

- [ ] Review current files and **all Git history** for credentials, private paths, personal audio/text, logs, certificates, and generated artifacts. A pattern scan is only a preliminary check; rotate exposed credentials before any history cleanup.
- [ ] Review the Apache-2.0 source license, NOTICE, brand exclusions, and third-party/model evidence. Resolve all unknown license items.
- [ ] Verify README language parity, local links, documented commands, contribution policy, and issue/PR templates.
- [ ] Enable GitHub private vulnerability reporting and designate a private conduct-report contact. The policy files do not configure these settings.
- [ ] Enable PR checks and DCO enforcement; confirm repository history and intended public source are correct.
- [ ] Record actual build/test results and outstanding failures. Do not describe quick regression as full acceptance.
- [ ] Define a version/tag that matches the source, app metadata, model locks, and release notes.

## Binary release

- [ ] Complete the source checks above and build from the exact revision.
- [ ] Supply a verified Sherpa artifact matching the catalog, model sources and redistribution evidence, and required third-party notices (including fonts and transitive native dependencies).
- [ ] Supply FFmpeg's exact corresponding source/archive, patches if any, build configuration, and license materials for the distributed binary. Audit actual enabled features.
- [ ] Generate and review a release SBOM and dependency-license inventory. The notice index alone is insufficient.
- [ ] Resolve `SEASNAIL_DEV_FILE_KEYCHAIN` and file credential fallback; do not publish the default development package as a production build.
- [ ] Complete the intended signing/notarization/stapling process, or make an explicit, reviewed unsigned pre-release decision with accurate limitations. An unsigned label does not resolve credential fallback.
- [ ] Test on a fresh macOS user/session: onboarding, denied permissions and recovery, recording, context, clipboard fallback, auto-paste, logout/login, failure recovery, GUI responsiveness, and process cleanup.
- [ ] Re-run the full workspace tests for the release revision; preserve the native process-identity regression checks and resolve any failures before claiming full validation.
- [ ] Attach App archive, SHA-256 checksums, SBOM, required notices/corresponding-source assets, and install/known-issue notes. No models or App binaries in ordinary Git.

## Current preparation status

The repository now provides bilingual landing/development/install/privacy documentation, onboarding explanations and a context diagram, prerequisite and locked-input fetch tools, and contribution/release policy files. These files do not certify that the source history, all licenses, public artifact availability, native toolchain reproducibility, or the binary release gates have passed. See [development prerequisites](development.md) and [architecture](architecture.md).

The initial isolated 2026-10-07 build exposed directory-dependent native binaries. Embedded source paths and link-time RPATHs have been normalized, the reviewed native toolchain is now checked, and independent builds in two directories produced identical native outputs. The corrected default runtime preparation and full App packaging passed strict verification. The generated App is still an unsigned development build; the remaining source/binary gates above still apply.

### License preparation update — 2026-10-07

Copied shadcn components and both development skills now retain MIT attribution;
all 25 imported skill files were matched to fixed upstream Git blobs. The legacy
GGUF Apache-2.0 copy has been replaced by the complete standard text.

App packaging now collects project LICENSE/NOTICE, Geist OFL, Rust normal/build,
frontend production and SwiftProtobuf license materials offline. The collector
rejects missing texts, mismatched crate archives and changed pinned supplements.
MPL Rust crates and the two locked Eigen archives are included as source.
FFmpeg bundles now contain and verify corresponding source, actual build
configuration and build instructions, bound to the executable hash.

This does not complete the binary release gate. The default SenseVoiceSmall
weights use a custom upstream model agreement; the archive's MIT pointer covers
toolkit code. Official current/historical terms and attribution are retained in
`scripts/licenses`, but the conversion archive does not identify its original
weight revision. Resolve that provenance before public weight redistribution;
do not turn a successful development package into a model-license approval.
The three macOS system-voice WAVs have been replaced by procedural business
audio and attributed CC-BY-4.0 FLEURS speech. The PoC reuses the new English
fixture; current audio source evidence is in `tests/api_regression/assets`. This source snapshot omits historical feature records and
personal screenshots; review the exact files and any new Git history again
before publishing. See
[license preparation](../scripts/licenses/README.md) and
[model evidence](../scripts/licenses/MODEL-LICENSE.md).
