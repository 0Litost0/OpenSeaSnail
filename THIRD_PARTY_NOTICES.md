# Third-party software and models

SeaSnail's source license does not replace third-party licenses. This index identifies existing evidence and the inventories that must accompany a public binary. It is not a claim that the current development App has completed a distribution audit.

| Component | Version/source authority | License/evidence |
| --- | --- | --- |
| Sherpa ONNX and native dependencies | `scripts/sherpa/source-lock.json` | [Runtime notices](scripts/sherpa/licenses/THIRD-PARTY-NOTICES.md); verified source licenses are copied into the artifact |
| ONNX Runtime | Same source lock, including commit and dependency manifest | MIT and upstream `ThirdPartyNotices.txt`, checked during runtime preparation |
| SenseVoice int8 model and tokens | `scripts/sherpa/artifact-lock.json` | Custom FunASR model agreement; [model attribution and provenance limits](scripts/licenses/MODEL-LICENSE.md). The archive's MIT pointer covers toolkit code, not weights |
| Silero VAD | Same artifact lock | [MIT text](scripts/sherpa/licenses/SILERO-VAD-MIT.txt) |
| FFmpeg | `scripts/ffmpeg/build-macos-arm64.sh` | LGPL build, source archive and build metadata in the prepared bundle; see [build notes](third_party/ffmpeg/macos-arm64/README.md) |
| Rust crates | `Cargo.lock` | [Offline notice collector](scripts/licenses/README.md), including version-specific supplements and MPL crate source archives; separately bundled native dependencies retain their own inventory |
| JavaScript packages and Geist font | `apps/desktop/pnpm-lock.yaml` | Production dependency texts collected during App packaging; [Geist OFL-1.1](scripts/licenses/upstream/GEIST-OFL-1.1.txt) |
| SwiftProtobuf in the native paste monitor | `apps/desktop/native/post-paste-monitor/Package.resolved` | Apache-2.0 with upstream Runtime Library Exception; full text and exact revision collected during App packaging |
| Copied shadcn UI components and development skills | [Verified source records](scripts/licenses/VENDORED-SOURCES.md) | [MIT license and copyright](scripts/licenses/upstream/SHADCN-MIT.txt); SeaSnail adaptations do not remove upstream attribution |
| Legacy ASR paths | `scripts/funasr`, `scripts/sensevoice` | Their respective lock files and license directories; audit if included in a distribution |
| Regression audio fixtures | `tests/api_regression/assets/manifest.json`, `audio-provenance.json` and `pipeline-audio.json` | FLEURS speech retains CC-BY-4.0 with full terms, attribution and conversion/source records; procedural business audio is Apache-2.0. See [audio notices](tests/api_regression/assets/AUDIO-LICENSE.md). PoC reuses the licensed English fixture |

Do not publish a binary until its complete dependency inventory, required notices, model redistribution evidence, and any corresponding-source obligations have been verified. Preserve upstream notices and modification records. See the [release checklist](doc/releasing.md).

App packages include SeaSnail's LICENSE/NOTICE, copied-source attribution,
Geist's full license, model evidence and the generated dependency inventory in
`Contents/Resources/licenses/application/`. The collector refuses missing or
changed pinned materials. Its `dependency-inventory.json` describes a conservative
Rust normal/build and frontend production graph; it is not a complete native
runtime SBOM or a model redistribution approval.
