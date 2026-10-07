# Develop SeaSnail on macOS

[简体中文](development.zh-CN.md)

Run commands from the repository root. Native builds require macOS 13+ and Apple Silicon. This guide distinguishes UI development from a full desktop build; a browser-only UI cannot exercise native IPC, audio, permissions, or global shortcuts.

## 1. Tools and source

Install Xcode Command Line Tools (`xcode-select --install`), Rust stable via rustup, Node.js 22.x ≥22.22.2 / 24.x ≥24.15 / 26+, pnpm 9+, Python 3.11+, jq, and CMake. Python 3.11 supplies the TOML reader used for App license collection. Full native builds require the exact reviewed compiler/SDK/CMake versions in [Sherpa inputs](../scripts/sherpa/BUILD-INPUTS.md); UI/Rust-only development does not. If cargo is missing after rustup installation, run `source "$HOME/.cargo/env"`.

```sh
git clone https://github.com/0Litost0/SeaSnail.git
cd SeaSnail
python3 scripts/doctor.py
```

Use your Node version manager to select a supported Node release. The repository pins 22.22.2 in `.nvmrc` and `.node-version`; with nvm, run `nvm install` then `nvm use`. `doctor.py` is read-only and reports missing tools or unsupported versions before you install dependencies. `--frontend-only` checks the smaller UI toolchain.

## 2. Build and test the UI

```sh
pnpm --dir apps/desktop install --frozen-lockfile
pnpm --dir apps/desktop generate:openapi
pnpm --dir apps/desktop lint:openapi
pnpm --dir apps/desktop typecheck
pnpm --dir apps/desktop test
pnpm --dir apps/desktop build
```

Generate the UI before compiling the Tauri crate: its `frontendDist` points to `apps/desktop/dist`. OpenAPI TypeScript is generated from `proto/openapi.yaml`; never edit it by hand. Protobuf compilation uses Rust tooling and does not require a system `protoc`.

For UI iteration, run `pnpm --dir apps/desktop dev`. Native behavior requires a packaged app. `cargo run` by itself is not the complete resource preparation and packaging workflow.

## 3. Rust validation

```sh
cargo check --workspace --locked
cargo test --workspace --locked
bash scripts/check-architecture-boundaries.sh
```

Read [testing conventions](../README.md#testing-and-agent-conventions). Some real-runtime tests require explicit runtime fixtures; ignored/manual tests do not establish real-device acceptance. Account data, model artifacts, and OS permissions must be separately isolated when testing multiple apps.

## 4. Prepare the bundled runtime

Two paths exist:

**Reuse an authenticated artifact:** If a maintainer provides a verified artifact matching this revision's embedded catalog, unpack it outside ordinary Git and pass its absolute path as `--asr-root`. Do not download arbitrary model bundles or bypass verification. There is no assumed public artifact download URL in this guide.

**Build locked upstream inputs:** Preview all downloads, then fetch them. This step uses the network, can download substantial source archives/models, and clones ONNX Runtime and its locked submodules. Existing matching inputs are verified and reused; mismatched or dirty inputs are refused rather than replaced. Source retrieval uses shallow pinned-tag clones, checks the full commit, and omits ONNX Runtime’s large `onnxruntime/test/testdata` directory because unit tests are disabled in the locked native build. Required build sources and locked submodules remain unchanged. Git steps time out after five minutes rather than wait indefinitely; retrying reuses verified archives.

```sh
python3 scripts/sherpa/fetch-inputs.py --list
python3 scripts/sherpa/fetch-inputs.py
scripts/build.sh --package --output dist/SeaSnail-dev.app
```

The fetch script prepares the default `third_party/sherpa/macos-arm64/{cache,sources}` directories and invokes the existing lock verifier. Build scripts then prepare the sidecar, native libraries, models, and licenses. `--package` itself does not download missing Sherpa inputs. For custom roots, use `fetch-inputs.py --root /absolute/path`, followed by the matching `--sherpa-cache`, `--sherpa-source`, and `--onnxruntime-source` build arguments.

**Native reproducibility:** The prepared artifact must match a fixed manifest, including compiled native binary hashes. The build now checks the reviewed toolchain and normalizes embedded source/build paths. If a native size/hash differs, preserve the failure and report it; do not edit hashes or weaken validation to make a local build pass. See the [reviewed build inputs](../scripts/sherpa/BUILD-INPUTS.md) for the pinned toolchain and reproducibility requirements.

A failed preparation can leave a partial artifact directory. That directory is not verified: move it aside before retrying, and never distribute it. The preparation script publishes `artifact-manifest.json` only after all checks pass.

For explicit source build stages and lock evidence, see [macOS build tutorial](build_for_MacOS_tutorial.md) (Chinese) and [BUILD-INPUTS.md](../scripts/sherpa/BUILD-INPUTS.md) (English).

## 5. Package and run

When an artifact already exists:

```sh
scripts/build.sh --all --asr-root /absolute/path/to/verified-artifact --output dist/SeaSnail-dev.app
open dist/SeaSnail-dev.app
```

`--all` prepares FFmpeg and builds the frontend/Rust app. `--app` rebuilds UI, GUI, and daemon but requires an existing FFmpeg bundle. `--package` prepares/reuses Sherpa and FFmpeg as well. Use a new output path each time; existing App outputs are not overwritten. Real-time-only smoke packages may use `--omit-ffmpeg`; they do not support file import.

These scripts produce unsigned development apps with `SEASNAIL_DEV_FILE_KEYCHAIN`. See [installation](install.md), [privacy](privacy.md), and [release requirements](releasing.md). System permission dialogs, real Keychain, automatic pasting, and window responsiveness require a real macOS session.

Use `SEASNAIL_DATA_DIR` for separate test data. Default logs are under `~/Library/Application Support/SeaSnail/logs/`. Never publish recordings, transcript text, clipboard context, or credentials with diagnostics.

## 6. API regression

API regression is a separate pnpm project and uses Playwright's API mode without browsers. It requires its own dependencies, a Rust host, Python `jsonschema`, and the fixed Sherpa/FFmpeg resources. See [execution instructions](../tests/api_regression/README.md).

```sh
pnpm --dir tests/api_regression install --frozen-lockfile
pnpm --dir tests/api_regression typecheck
pnpm --dir tests/api_regression check:contracts
pnpm --dir tests/api_regression api-test quick
```

Quick covers 17 cases. Full `acceptance` requires all 22 required cases actually passing; skipped, incomplete, or flaky results are not full acceptance. Live provider evaluation is separate and explicitly opt-in.
