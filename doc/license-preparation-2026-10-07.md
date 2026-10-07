# License preparation — 2026-10-07

This records completed preparation and its limits, not a public-release approval.

## Completed changes

- Retained shadcn's MIT copyright and full text for copied UI components and both
  imported skills. All 25 skill files, including icons, match the official Git
  blobs at their recorded revisions. See `scripts/licenses/VENDORED-SOURCES.json`.
- Replaced the abbreviated legacy Apache-2.0 copy with the complete root text.
- Recorded the actual custom FunASR model agreement separately from toolkit MIT;
  retained current and 2024 official model cards/terms with hashes and revisions.
  Re-fetched all four locked GGUF/VAD model cards: they declare Apache-2.0 for
  those specific distributions. The default ONNX weights are not relabeled.
- Added offline, version-specific notice collection to App packaging. Missing
  license texts, changed pinned materials and mismatched crate archives fail
  collection. No placeholder license is generated for an unknown dependency.
- Included full Geist OFL and SwiftProtobuf runtime-exception text, project
  LICENSE/NOTICE, dependency attribution and an inventory. The current conservative
  inventory contains 398 Rust normal/build dependencies, 90 frontend production
  dependencies and one Swift dependency. It is not a complete native SBOM.
- Included six MPL Rust crate archives and both locked Eigen source archives.
- FFmpeg now carries its exact source archive, actual build configuration,
  build script/instructions and source/binary hash manifest. App packaging verifies
  and includes these materials. A rebuilt FFmpeg matched the previous executable
  byte for byte; native ASR/model bytes and strict artifact baselines were not
  changed for this license work.

## Validation

- Development-setup tests: 15 passed, including rejection of unsupported Python
  before packaging, a crate with missing
  license text, a tampered crate, a substituted FFmpeg source pin and a mismatched
  FFmpeg executable/source manifest.
- API contracts/assets checks passed; no ASR acceptance rerun is claimed.
- Two independent offline license collections produced identical 352-file
  manifests; every file's size/hash and inventory completeness were verified.
- Real unsigned development App packaging passed. Its collected license files
  matched the independent collection exactly. FFmpeg was rebuilt from the locked
  pristine source and its corresponding-source package passed verification.
- New checked-in license materials passed a redacted secret scan; no secret
  findings. Generated license text/metadata contained no project-owner home path.
- Shell syntax and whitespace checks passed.

The application test package is a development artifact, not authorization to
publish model weights or a signed/notarized release. These checks do not inspect
unrelated ignored application data, remote GitHub settings or remote refs.

## Remaining decisions before public distribution

1. The default ONNX conversion archive does not pin its original weight revision.
   Its 71-byte MIT pointer describes the toolkit. Resolve that provenance with
   the distributor or reproduce the conversion from an explicitly licensed,
   pinned weight snapshot before public binary redistribution. The complete
   custom agreement is recorded; its restrictions have not been waived.
2. Follow-up fixture replacement removes the three macOS system-voice WAVs.
   Procedural business audio and CC-BY-4.0 FLEURS speech now have separate
   provenance and tests; see `tests/api_regression/assets/AUDIO-LICENSE.md`.
   Validation of the new corpus is recorded separately from the historical checks
   above.
3. This source snapshot removes historical feature records and the known personal
   screenshots. Review all remaining files and the eventual initial commit before
   publication; a new repository alone is not privacy clearance.
4. Finish the intended release dependency/SBOM review, macOS credential/signing
   and fresh-user acceptance checks in `releasing.md`.

The validation above was acquired in the development repository before source
snapshot cleanup. It is not validation of a new public commit. No repository or
release was published.
