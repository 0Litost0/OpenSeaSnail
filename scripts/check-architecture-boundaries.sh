#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

fail=0

# ST-M6.6/M6.7: platform adapter layout must exist before the desktop shell
# grows more native capability implementations. OS-specific dependency entries
# belong under their target section, not the portable dependency block.
platform_root="apps/desktop/src-tauri/src/platform"
for required_platform_file in \
  "$platform_root/mod.rs" \
  "$platform_root/recording.rs" \
  "$platform_root/resource.rs" \
  "$platform_root/clipboard.rs" \
  "$platform_root/permission.rs" \
  "$platform_root/injection.rs" \
  "$platform_root/daemon.rs" \
  "$platform_root/macos/mod.rs"; do
  if [[ ! -f "$required_platform_file" ]]; then
    echo "missing desktop platform adapter: $required_platform_file" >&2
    fail=1
  fi
done
portable_desktop_dependencies="$(sed -n '/^\[dependencies\]/,/^\[target/p' apps/desktop/src-tauri/Cargo.toml)"
if grep -q '^[[:space:]]*core-graphics[[:space:]]*=' <<<"$portable_desktop_dependencies"; then
  echo "core-graphics must remain a macOS target dependency" >&2
  fail=1
fi

# desktop-core is the hard platform-independent boundary from the moment it exists.
if rg -n 'tauri|objc2|core-graphics|AVFoundation|AppKit|CoreGraphics|cpal|arboard|openapi|protobuf|seasnail-daemon|seasnail-runtime' \
  crates/desktop-core/Cargo.toml; then
  echo "desktop-core contains a forbidden platform/transport/runtime dependency" >&2
  fail=1
fi
if rg -n '^\s*(use|pub use|extern crate)\s+(tauri|objc2|core_graphics|cpal|arboard|seasnail_daemon|seasnail_runtime|seasnail_proto)' \
  crates/desktop-core/src --glob '*.rs'; then
  echo "desktop-core source imports a forbidden platform/transport/runtime crate" >&2
  fail=1
fi

# M6.7: the Tauri composition root may call semantic adapter functions, but it
# must not import native AppKit/Foundation/CoreGraphics types itself. Keeping
# those symbols in platform/macos makes the OS boundary reviewable and keeps a
# future non-macOS shell from copying native implementation into lib.rs.
if rg -n '^[[:space:]]*(use|pub use|extern crate).*?(objc2|core_graphics|NSWindow|NSWorkspace|NSScreen|NSURL|NSString|CGEvent|tauri_nspanel::tauri_panel)' \
  apps/desktop/src-tauri/src/lib.rs; then
  echo "Tauri composition root directly owns macOS native API types" >&2
  fail=1
fi

# M7.2: platform/daemon capability implementations must actually live behind
# the adapter boundary. Merely importing the adapter module while keeping the
# concrete recorder, clipboard/injection, HTTP client and supervisor in lib.rs
# leaves the old composition-root owner in place.
if rg -n '^[[:space:]]*(use|pub use|extern crate)[[:space:]].*(arboard|cpal|reqwest|prost|seasnail_proto)|^[[:space:]]*(arboard|cpal|reqwest|prost|seasnail_proto)::' \
  apps/desktop/src-tauri/src/lib.rs; then
  echo "Tauri composition root directly owns platform/transport dependencies" >&2
  fail=1
fi
if rg -n '\b(arboard|cpal|reqwest|prost|seasnail_proto)::|seasnail_daemon::resource' \
  apps/desktop/src-tauri/src/lib.rs; then
  echo "Tauri composition root contains platform/transport implementation calls" >&2
  fail=1
fi
if rg -n '^pub struct (RecordingController|TextInjector|DaemonClient|DaemonSupervisor)|^struct (RecordingTelemetry|CpalErrorConsumer|RecordingInner)' \
  apps/desktop/src-tauri/src/lib.rs; then
  echo "Tauri capability implementation owners remain in lib.rs" >&2
  fail=1
fi

# M6.1 起实时状态/reducer/generation 的唯一类型 owner 是 desktop-core；Tauri 只能
# 依赖或 re-export，不能重新声明一套会发生漂移的状态机。
if rg -n '^pub (struct|enum) (TaskGeneration|CancellationToken|RealtimeTaskPhase|RealtimeTaskFallback|RealtimeTaskSnapshot|RealtimeTaskEvent|WorkerToken)|^pub fn (reduce|worker_is_current|stable_failure_code)\(' \
  apps/desktop/src-tauri/src --glob '*.rs'; then
  echo "Tauri adapter redeclares desktop-core realtime state ownership" >&2
  fail=1
fi

# ST-M6.2: the active task lifecycle must be owned by desktop-core. Tauri may
# alias the coordinator and provide adapters, but it must not construct a
# second owner, expose a writable cancellation cell, or create a worker beside
# the coordinator. Shortcut and tray must both dispatch the same entry.
if ! rg -n '^pub struct ActiveRealtimeTask \{' crates/desktop-core/src/lib.rs >/dev/null; then
  echo "desktop-core does not declare the realtime active-task owner" >&2
  fail=1
fi
if rg -n 'ActiveRealtimeTask\s*\{' apps/desktop/src-tauri/src --glob '*.rs'; then
  echo "Tauri adapter constructs or redeclares ActiveRealtimeTask" >&2
  fail=1
fi
if rg -n 'as_atomic|cancellation_token\(\)' apps/desktop/src-tauri/src --glob '*.rs'; then
  echo "Tauri adapter can replace or independently create the realtime cancellation token" >&2
  fail=1
fi
coordinator_manage_count="$(rg -c 'app\.manage\(Arc::new\(RealtimeTaskCoordinator::new' apps/desktop/src-tauri/src/lib.rs || true)"
if [[ "$coordinator_manage_count" -ne 1 ]]; then
  echo "Tauri must install exactly one production RealtimeCoordinator" >&2
  fail=1
fi
shared_toggle_dispatch_count="$(rg -c 'dispatch_recording_toggle\(app\)' apps/desktop/src-tauri/src/lib.rs || true)"
if [[ "$shared_toggle_dispatch_count" -lt 2 ]]; then
  echo "shortcut and tray do not share the same realtime Coordinator entry" >&2
  fail=1
fi

# Application commands/results may not import transport or desktop types.
if rg -n '^\s*(use|pub use|extern crate)\s+(axum|http|multer|tauri)|::multipart' \
  crates/daemon/src/application --glob '*.rs'; then
  echo "daemon application layer contains a forbidden transport/desktop dependency" >&2
  fail=1
fi

# Session HTTP adapters must not regain the deleted runtime/storage pipeline.
if rg -n '^\s*use\s+(seasnail_runtime|seasnail_storage)|state\.(storage|registry|scheduler|normalizer|settings)\(\)|\b(PipelineJob|TranscribeReq|OpenAiSegments|JobGuard)\b' \
  crates/daemon/src/api/sessions.rs; then
  echo "sessions HTTP adapter regained runtime/storage orchestration" >&2
  fail=1
fi

# Model handlers call ModelService; the deleted switch/activate orchestration may not return.
if rg -n '^async fn (switch_active|activate_model)\(' crates/daemon/src/api/models.rs; then
  echo "models HTTP adapter regained runtime activation orchestration" >&2
  fail=1
fi
if rg -n 'BackendRegistry::build|BackendResources::|Verified(Sherpa|Gguf)Resources::verify|Preflighted(Whisper|FunAsr)Resources::preflight' \
  crates/daemon/src/api --glob '*.rs'; then
  echo "daemon HTTP adapters directly construct or verify runtime backends" >&2
  fail=1
fi
if rg -n -U 'pub\(crate\) fn[\s\S]{0,240}Result<[^\n]*(seasnail_storage|seasnail_proto)' \
  crates/daemon/src/application/services.rs; then
  echo "application service result leaks storage or protobuf types to adapters" >&2
  fail=1
fi

# M7.2: session history is refreshed by the native task snapshot bridge. A
# React refetch interval here would reintroduce the deleted daemon-polling
# production path (the local recording-status bridge has its own adapter).
if rg -n 'refetchInterval' apps/desktop/src/features/sessions --glob '*.{ts,tsx}'; then
  echo "React session UI must not poll daemon transcription status" >&2
  fail=1
fi

# M7.3: the migration compatibility surface is closed. AppState is a
# composition input/lifecycle owner, not a handler-facing service locator.
app_state_file="crates/daemon/src/api/mod.rs"
app_state_impl="$(awk '
  /^impl AppState[[:space:]]*\{/ { in_impl=1 }
  in_impl {
    line=$0
    opens=gsub(/\{/, "", line)
    closes=gsub(/\}/, "", line)
    print
    depth += opens - closes
    if (depth == 0) exit
  }
' "$app_state_file")"
if rg -n '^\s*pub(?:\(crate\))? fn (storage|registry|scheduler|normalizer|catalog|settings|download_progress|thumbnail_slots|thumbnail_session_slots)\(' <<<"$app_state_impl"; then
  echo "legacy AppState bottom-level accessor remains after M7.3" >&2
  fail=1
fi

# M7.3: deleted owners and adapters must not quietly return as production
# modules. The old realtime pipeline is retained only as cfg(test) regression
# coverage and is therefore not listed here.
for deleted_file in \
  "apps/desktop/src-tauri/src/realtime_task.rs" \
  "apps/desktop/src-tauri/src/realtime_submission.rs" \
  "apps/desktop/src-tauri/src/realtime_polling.rs" \
  "crates/runtime/src/scheduler.rs"; do
  if [[ -e "$deleted_file" ]]; then
    echo "deleted compatibility owner still exists: $deleted_file" >&2
    fail=1
  fi
done

if rg -n '\bTranscribeScheduler\b|\bJobGuard\b|\bOccupied\b' \
  apps/desktop/src-tauri/src crates/runtime/src --glob '*.rs'; then
  echo "legacy runtime/desktop compatibility symbols remain" >&2
  fail=1
fi

# M7.1: the registry/gate names are canonical. Do not retain source-compatible
# aliases or a public gate reset that can bypass RAII ownership in production.
if rg -n '^\s*pub\s+(type\s+BackendFactory(?:Error)?|fn\s+force_clear\s*\()' \
  crates/runtime/src --glob '*.rs'; then
  echo "runtime compatibility alias or public gate reset remains" >&2
  fail=1
fi
if rg -n '^pub use backends::\{funasr, whisper\};' crates/runtime/src/lib.rs; then
  echo "top-level legacy backend module re-export remains" >&2
  fail=1
fi

# No HTTP adapter may recover raw identity/storage state after authentication.
if rg -n '\.auth\(\)|app_state\(' crates/daemon/src/api --glob '*.rs'; then
  echo "HTTP API exposes a raw Auth/AppState bypass" >&2
  fail=1
fi

if rg -n 'State<AppState>|Router<AppState>' crates/daemon/src/api --glob '*.rs'; then
  echo "API routes must use the unique HttpState composition root" >&2
  fail=1
fi

if rg -n 'state\.storage\(\)' crates/daemon/src/api --glob '*.rs'; then
  echo "authenticated API data access must use caller-bound storage" >&2
  fail=1
fi

if rg -n '^\s*(auth_service|account_service|token_service|lease_tracker|repository_factory):' \
  crates/daemon/src/api/mod.rs; then
  echo "AppState must not own a second identity service/repository graph" >&2
  fail=1
fi

if [[ "$(rg -c 'ApplicationServices::new' crates/daemon/src/api/mod.rs || true)" -ne 1 ]]; then
  echo "HttpState must construct exactly one ApplicationServices graph" >&2
  fail=1
fi

# Identity services must use the caller-bound repository rather than consulting
# the mutable active-account database after authentication.
if rg -n 'open_active_db|TokenRow|->\s*Result<.*(AccountError\b|IssuedToken\b)' \
  crates/daemon/src/application/services.rs; then
  echo "application identity API leaks or reopens mutable infrastructure state" >&2
  fail=1
fi
if rg -n '^\s*pub fn crypto\(' crates/daemon/src/application --glob '*.rs'; then
  echo "application public API must not expose Crypto" >&2
  fail=1
fi
if rg -n '^\s*pub(?:\(crate\))? fn (auth|crypto|app_state)\(' crates/daemon/src/api/mod.rs crates/daemon/src/account/storage.rs; then
  echo "raw Auth/Crypto/AppState accessors are forbidden" >&2
  fail=1
fi

if rg -n '^\s*pub fn (new\(app: AppState|delete_account\()' crates/daemon/src/api/mod.rs crates/daemon/src/account/{auth,crypto}.rs; then
  echo "composition/deletion bypass became public" >&2
  fail=1
fi

if sed -n '/pub struct CallerContext {/,/^}/p' crates/daemon/src/application/mod.rs | rg -n '^\s{2,}pub\s+[a-z_]'; then
  echo "CallerContext identity fields must remain private" >&2
  fail=1
fi

# Runtime reservations must remain opaque outside the runtime crate. Callers
# may execute work through the reservation, but cannot detach its lease or
# recover the underlying runtime and bypass RAII cleanup.
if rg -n '^\s*pub\s+fn\s+(runtime|into_parts)\(' crates/runtime/src/reservation.rs; then
  echo "runtime reservation exposes its runtime or lease" >&2
  fail=1
fi

# Backend construction is a typed trust boundary. The registry accepts only
# preflighted/verified resource values, never struct variants carrying paths.
if rg -n 'BackendResources::(Whisper|FunAsr|Gguf|SherpaOnnx)\s*\{' \
  crates/runtime/src crates/daemon/src --glob '*.rs'; then
  echo "backend factory accepts raw struct-form resources" >&2
  fail=1
fi

# The public facade must not leak backend protocol request/response shapes.
facade_public_api="$(rg '^\s*pub\s+(struct|enum|trait|fn|async fn)|^\s*pub\s+[a-z_]+:' crates/runtime/src/facade.rs || true)"
if rg -n 'TranscribeReq|OpenAiSegments' <<<"$facade_public_api"; then
  echo "runtime facade public API leaks backend protocol types" >&2
  fail=1
fi
if sed -n '/^pub trait TranscriptionEngine/,/^}/p; /^pub trait RuntimeAdmin/,/^}/p' \
  crates/runtime/src/facade.rs | rg -n 'TranscribeReq|OpenAiSegments'; then
  echo "runtime facade public trait leaks backend protocol types" >&2
  fail=1
fi

# Catalog-verified drivers may only be constructed through typed resources and
# BackendRegistry; otherwise a caller can bypass the verified-resource proof.
if rg -n '^\s*pub fn new\(' \
  crates/runtime/src/backends/{sensevoice_gguf,sherpa_onnx}.rs; then
  echo "verified backend driver exposes a raw public constructor" >&2
  fail=1
fi

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "architecture boundary checks passed"
