# SeaSnail

**Local voice dictation for your Mac.** Speak, bring in clipboard context, and paste the result into the app where you write.

English · [简体中文](README.zh-CN.md)

SeaSnail runs speech recognition on your device using a bundled Sherpa ONNX SenseVoice int8 runtime. It is an early-stage, open-source desktop application for **macOS 13+ on Apple Silicon**.

## What you can do

- Start and stop recording with **⌘ ⇧ Space**; customize the shortcut in Settings.
- Automatically paste transcriptions into the focused app, with clipboard/history fallback.
- Copy text, links, files, or images while recording and include that context in the result.
- Browse encrypted local session history and manage local accounts.
- Maintain a personal dictionary, with optional learning from corrections and your edits.
- Optionally clean up dictation using a provider you configure; manage prompts and inspect results.
- Use scoped integration tokens, the local OpenAPI API, and data export workflows.

![Clipboard context: record speech, copy a link while recording, then receive a combined result.](doc/assets/clipboard-context.svg)

Clipboard context is captured during recording when enabled. Text and links are included in the result; files and images are represented by local paths. You can change capture in Settings → Privacy & permissions.

## Privacy

Audio, transcripts, and clipboard context are stored locally with encryption. A local account protects data on this device; it does not require an online registration. Keep your password safe: you need it to log back in after logging out.

**Optional AI cleanup is off by default.** Enabling it sends transcript text and dictionary spelling hints to your configured provider. Actual clipboard context is replaced with opaque placeholders before the request and restored locally afterward. See [privacy and permissions](doc/privacy.md).

## Install and try

See the repository’s [Releases](https://github.com/0Litost0/SeaSnail/releases) for available builds. Use the platform, signature status, and checksum information on the specific release; this README does not imply that a public binary is already available. You can also [build from source](doc/development.md).

1. Open SeaSnail and create a local account.
2. Read each permission explanation before allowing access. Microphone access enables recording; Accessibility enables automatic pasting and related edit learning. Both can be configured later.
3. Review the shortcut and clipboard context guide.
4. Open a text editor, place the cursor, press **⌘ ⇧ Space**, say a sentence, and press it again.

The current build scripts produce **unsigned development apps**, with a development-only file Keychain fallback. Public distribution requires a separate release review; see the [release checklist](doc/releasing.md). Read the [installation guide](doc/install.md) before running an unsigned build.

## Develop

Install Xcode Command Line Tools, Rust stable, pnpm 9+, Python 3, and a supported Node.js version: 22.x ≥22.22.2, 24.x ≥24.15, or 26+. Full runtime builds also need CMake and jq.

```sh
git clone https://github.com/0Litost0/SeaSnail.git
cd SeaSnail
python3 scripts/doctor.py --frontend-only
pnpm --dir apps/desktop install --frozen-lockfile
pnpm --dir apps/desktop generate:openapi
pnpm --dir apps/desktop typecheck
pnpm --dir apps/desktop test
pnpm --dir apps/desktop build
```

`pnpm --dir apps/desktop dev` starts the UI only. Recording, global shortcuts, permissions, and daemon IPC require the native app. For runtime preparation, Rust checks, packaging, and troubleshooting, follow the [development guide](doc/development.md) ([中文](doc/development.zh-CN.md)). See the [release checklist](doc/releasing.md) for validation requirements and remaining release gates.

## Architecture

```text
React UI → restricted Tauri IPC → native platform adapters
                                     ↕ desktop-core coordinator
                               local Rust daemon
                                     ↓
                        application services → runtime → ASR sidecar
                                     ↓
                        SQLCipher + encrypted files + Keychain
```

The workspace separates desktop coordination, business services, runtime adapters, cryptography, storage, and protocol contracts. OpenAPI types are generated from [proto/openapi.yaml](proto/openapi.yaml); transcript and cleanup artifacts use protobuf. See [architecture design](doc/architecture.md) and [account lifecycle](doc/account-login-lifecycle.md).

<a id="测试与-agent-开发约定"></a>

## Testing and agent conventions

Read the relevant requirement, design, and roadmap before editing. Run checks for the affected scope, fix failures, and report actual results. Do not weaken assertions, skip required cases, or claim that quick regression is full acceptance.

| Change | Required checks |
| --- | --- |
| Documentation | Check links and commands; `git diff --check` |
| Frontend | `pnpm --dir apps/desktop typecheck`, `test`, and `build` |
| Rust | `cargo check --workspace`, `cargo test --workspace`; API regression when behavior changes |
| OpenAPI | Regenerate types; `pnpm --dir apps/desktop lint:openapi`; affected frontend/Rust/API checks |
| API behavior | API typecheck, contract checks, affected cases, then `quick` |
| Test framework, real ASR, or full acceptance | Relevant infrastructure checks and `acceptance` |

Run `bash scripts/check-architecture-boundaries.sh` when changing architecture. The API suite uses Playwright’s API mode without a browser: 17 quick cases, 22 required acceptance cases, and a separate optional provider evaluation. Report run IDs, scope, exit status, flaky results, teardown, and report paths. See the [API test guide](tests/api_regression/README.md) and [detailed development conventions](doc/development.zh-CN.md#测试与-agent-开发约定).

## Contribute and report issues

Read [CONTRIBUTING](CONTRIBUTING.md), the [code of conduct](CODE_OF_CONDUCT.md), and [security reporting policy](SECURITY.md). Bug reports should include platform, app version, reproduction steps, and redacted diagnostics. Never post credentials, recordings, transcripts, or clipboard contents.

## License and attribution

SeaSnail-owned source code is licensed under [Apache-2.0](LICENSE). Third-party software, fonts, and models retain their own licenses; see [NOTICE](NOTICE) and [third-party notices](THIRD_PARTY_NOTICES.md). The SeaSnail name, logos, and app icons are covered separately by the [brand policy](BRAND.md).

The default SenseVoiceSmall weights have a separate custom FunASR model
agreement. The toolkit's MIT license does not cover those weights; see the
[model terms, attribution and provenance record](scripts/licenses/MODEL-LICENSE.md).
