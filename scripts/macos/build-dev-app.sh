#!/usr/bin/env bash
# 构建未签名 macOS Apple Silicon 开发版 SeaSnail.app（M6.0/M6.1）。
#
# 该脚本只建立资源布局与可启动性，不执行 codesign/notarization；发行签名另行完成。
# Sherpa/legacy ASR 与可选的 LGPL-only ffmpeg bundle 均由项目内可复现脚本预先准备。
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ARTIFACT_LOCK="$ROOT_DIR/scripts/sensevoice/artifact-lock.json"
SOURCE_LOCK="$ROOT_DIR/scripts/sensevoice/source-lock.json"
VERIFY_ARTIFACT="$ROOT_DIR/scripts/sensevoice/verify-artifact.sh"
MODELS_CATALOG_TEMPLATE="$ROOT_DIR/crates/daemon/resources/models.json"
OUTPUT="$ROOT_DIR/dist/SeaSnail.app"
FFMPEG=""
OMIT_FFMPEG=0
# 与 scripts/build.sh 保持一致，避免直接调用本脚本时落到旧的不完整 bundle 目录。
FUNASR="${SEASNAIL_FUNASR_BUNDLE:-$ROOT_DIR/third_party/funasr/macos-arm64/bundle-process-name}"
GGUF_ROOT="$ROOT_DIR/third_party/sensevoice/macos-arm64"
GGUF_VARIANT="q8"
ASR_ROOT=""
ASR_BACKEND=""
ASR_MODEL="sensevoice-small"
ASR_VARIANT="int8"
ASR_ENABLED=0
SKIP_RUST_BUILD=0
DEBUG_BACKEND_SWITCHING=0
CAPSULE_SMOKE=0
FUNASR_ROOT_EXPLICIT=0
GGUF_VARIANT_EXPLICIT=0

usage() { printf '%s\n' "Usage: $0 [--output APP_PATH] [--asr-backend sherpa_onnx --asr-root ARTIFACT | --funasr-root DIR | --gguf-root DIR --gguf-variant q8|f16|f32] [--ffmpeg PATH | --omit-ffmpeg] [--skip-rust-build] [--debug-backend-switching] [--capsule-smoke]"; }

ensure_cargo() {
  if command -v cargo >/dev/null 2>&1; then
    return
  fi
  local rustup_bin="${HOME:-}/.cargo/bin"
  if [[ -n "${HOME:-}" && -x "$rustup_bin/cargo" ]]; then
    export PATH="$rustup_bin:$PATH"
    return
  fi
  printf '%s\n' 'error: cargo was not found. Install Rust with rustup (https://rustup.rs), then restart the terminal or run: source "$HOME/.cargo/env"' >&2
  exit 1
}

run_cargo_with_env() {
  if ((${#build_env[@]} > 0)); then
    env "${build_env[@]}" cargo "$@"
  else
    cargo "$@"
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --output) [[ $# -ge 2 ]] || { usage >&2; exit 2; }; OUTPUT="$2"; shift 2 ;;
    --funasr-root) [[ $# -ge 2 ]] || { usage >&2; exit 2; }; FUNASR="$2"; FUNASR_ROOT_EXPLICIT=1; shift 2 ;;
    --gguf-root) [[ $# -ge 2 ]] || { usage >&2; exit 2; }; GGUF_ROOT="$2"; GGUF_ENABLED=1; shift 2 ;;
    --gguf-variant) [[ $# -ge 2 ]] || { usage >&2; exit 2; }; GGUF_VARIANT="$2"; GGUF_VARIANT_EXPLICIT=1; shift 2 ;;
    --asr-root) [[ $# -ge 2 ]] || { usage >&2; exit 2; }; ASR_ROOT="$2"; ASR_ENABLED=1; shift 2 ;;
    --asr-backend) [[ $# -ge 2 ]] || { usage >&2; exit 2; }; ASR_BACKEND="$2"; shift 2 ;;
    --asr-model) [[ $# -ge 2 ]] || { usage >&2; exit 2; }; ASR_MODEL="$2"; shift 2 ;;
    --asr-variant) [[ $# -ge 2 ]] || { usage >&2; exit 2; }; ASR_VARIANT="$2"; shift 2 ;;
    --ffmpeg) [[ $# -ge 2 ]] || { usage >&2; exit 2; }; FFMPEG="$2"; shift 2 ;;
    --omit-ffmpeg) OMIT_FFMPEG=1; shift ;;
    --skip-rust-build) SKIP_RUST_BUILD=1; shift ;;
    --debug-backend-switching) DEBUG_BACKEND_SWITCHING=1; shift ;;
    --capsule-smoke) CAPSULE_SMOKE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done

[[ "$OMIT_FFMPEG" -eq 0 || -z "$FFMPEG" ]] || {
  printf '%s\n' 'error: --omit-ffmpeg cannot be combined with --ffmpeg' >&2; exit 1;
}

GGUF_ENABLED="${GGUF_ENABLED:-0}"

[[ "$(uname -s)" == Darwin && "$(uname -m)" == arm64 ]] || {
  printf '%s\n' 'This development bundle only supports macOS Apple Silicon.' >&2; exit 1;
}
[[ "$DEBUG_BACKEND_SWITCHING" -eq 0 || "$SKIP_RUST_BUILD" -eq 0 ]] || {
  printf '%s\n' 'error: --debug-backend-switching cannot be combined with --skip-rust-build; the daemon must be compiled with the matching feature.' >&2; exit 1;
}
[[ "$CAPSULE_SMOKE" -eq 0 || "$SKIP_RUST_BUILD" -eq 0 ]] || {
  printf '%s\n' 'error: --capsule-smoke cannot be combined with --skip-rust-build; the GUI must be compiled with production recording entries disabled.' >&2; exit 1;
}
[[ "$GGUF_ENABLED" -eq 0 || "$SKIP_RUST_BUILD" -eq 0 ]] || {
  printf '%s\n' 'error: --skip-rust-build cannot be combined with GGUF; the daemon must embed the selected model catalog' >&2; exit 1;
}
[[ "$ASR_ENABLED" -eq 0 || "$GGUF_ENABLED" -eq 0 ]] || {
  printf '%s\n' 'error: --asr-root and --gguf-root cannot be combined' >&2; exit 1;
}
[[ "$ASR_ENABLED" -eq 0 || "$FUNASR_ROOT_EXPLICIT" -eq 0 ]] || {
  printf '%s\n' 'error: --asr-root and --funasr-root cannot be combined' >&2; exit 1;
}
if [[ "$ASR_ENABLED" -eq 1 ]]; then
  [[ "$ASR_BACKEND" == sherpa_onnx ]] || {
    printf '%s\n' 'error: --asr-root currently requires --asr-backend sherpa_onnx' >&2; exit 1;
  }
  [[ "$ASR_MODEL" == sensevoice-small && "$ASR_VARIANT" == int8 ]] || {
    printf '%s\n' 'error: the bundled Sherpa candidate requires sensevoice-small/int8' >&2; exit 1;
  }
  [[ -d "$ASR_ROOT" && -f "$ASR_ROOT/artifact-manifest.json" ]] || {
    printf '%s\n' "Missing Sherpa artifact root: $ASR_ROOT" >&2; exit 1;
  }
fi
if [[ -n "$ASR_BACKEND" && "$ASR_ENABLED" -eq 0 ]]; then
  case "$ASR_BACKEND" in
    sherpa_onnx) printf '%s\n' 'error: --asr-backend sherpa_onnx requires --asr-root' >&2; exit 1 ;;
    funasr) [[ "$FUNASR_ROOT_EXPLICIT" -eq 1 ]] || { printf '%s\n' 'error: --asr-backend funasr requires --funasr-root' >&2; exit 1; } ;;
    gguf) [[ "$GGUF_ENABLED" -eq 1 ]] || { printf '%s\n' 'error: --asr-backend gguf requires --gguf-root' >&2; exit 1; } ;;
    *) printf '%s\n' "error: unsupported --asr-backend: $ASR_BACKEND" >&2; exit 1 ;;
  esac
fi
[[ "$GGUF_VARIANT" == q8 || "$GGUF_VARIANT" == f16 || "$GGUF_VARIANT" == f32 ]] || {
  printf '%s\n' "Unsupported GGUF variant: $GGUF_VARIANT" >&2; exit 1;
}
[[ "$FUNASR_ROOT_EXPLICIT" -eq 0 || "$GGUF_ENABLED" -eq 0 ]] || {
  printf '%s\n' 'error: --funasr-root and --gguf-root cannot be combined in one candidate build' >&2; exit 1;
}
[[ "$GGUF_VARIANT_EXPLICIT" -eq 0 || "$GGUF_ENABLED" -eq 1 ]] || {
  printf '%s\n' 'error: --gguf-variant requires --gguf-root' >&2; exit 1;
}
if [[ "$ASR_ENABLED" -eq 1 ]]; then
  ASR_MANIFEST_SHA="$(shasum -a 256 "$ASR_ROOT/artifact-manifest.json" | awk '{print $1}')"
  ASR_SIZE="$(jq -er '[.files[].size_bytes] | add' "$ASR_ROOT/artifact-manifest.json")"
  jq -e --arg sha "$ASR_MANIFEST_SHA" --argjson size "$ASR_SIZE" \
    '([.models[] | select(.id == "sensevoice-small-sherpa-int8")]) as $m
     | ($m | length == 1) and ($m[0].runtime == "sherpa_onnx")
       and ($m[0].default == true) and ($m[0].artifact_manifest_sha256 == $sha)
       and ($m[0].size_bytes == $size)' "$MODELS_CATALOG_TEMPLATE" >/dev/null || {
    printf '%s\n' 'Sherpa artifact manifest does not match the embedded catalog' >&2; exit 1;
  }
elif [[ "$GGUF_ENABLED" -eq 1 ]]; then
  [[ "$DEBUG_BACKEND_SWITCHING" -eq 0 ]] || {
    printf '%s\n' 'error: debug backend switching requires a separate dual-runtime migration package' >&2; exit 1;
  }
  GGUF_SERVER="$GGUF_ROOT/bundle/sensevoice-server"
  GGUF_BUILD_INFO="$GGUF_ROOT/bundle/BUILD-INFO.txt"
  GGUF_SOURCE_LOCK="$GGUF_ROOT/bundle/source-lock.json"
  GGUF_MODEL="$GGUF_ROOT/cache/models/$GGUF_VARIANT/sensevoice.gguf"
  GGUF_VAD="$GGUF_ROOT/cache/models/fsmn-vad.gguf"
  for resource in "$GGUF_SERVER" "$GGUF_BUILD_INFO" "$GGUF_SOURCE_LOCK" "$GGUF_MODEL" "$GGUF_VAD"; do
    [[ -f "$resource" && ! -L "$resource" ]] || {
      printf '%s\n' "Missing regular GGUF resource: $resource" >&2; exit 1;
    }
  done
  [[ -x "$GGUF_SERVER" ]] || { printf '%s\n' "GGUF server is not executable: $GGUF_SERVER" >&2; exit 1; }
  [[ -f "$ARTIFACT_LOCK" && -f "$SOURCE_LOCK" && -x "$VERIFY_ARTIFACT" ]] || {
    printf '%s\n' 'Missing GGUF build locks or artifact verifier' >&2; exit 1;
  }
  cmp -s "$GGUF_SOURCE_LOCK" "$SOURCE_LOCK" || {
    printf '%s\n' 'GGUF server source lock does not match the repository lock' >&2; exit 1;
  }
  grep -qxF 'Architecture: arm64' "$GGUF_BUILD_INFO" || {
    printf '%s\n' 'GGUF server build metadata does not declare arm64' >&2; exit 1;
  }
  SOURCE_REVISION="$(jq -er '.sensevoice.revision' "$SOURCE_LOCK")"
  grep -qxF "SenseVoice revision: $SOURCE_REVISION" "$GGUF_BUILD_INFO" || {
    printf '%s\n' 'GGUF server build metadata does not match the locked SenseVoice revision' >&2; exit 1;
  }
  "$VERIFY_ARTIFACT" "$GGUF_VARIANT" "$GGUF_MODEL" >/dev/null
  "$VERIFY_ARTIFACT" fsmn-vad "$GGUF_VAD" >/dev/null
  MODEL_SIZE="$(jq -er --arg id "$GGUF_VARIANT" '.artifacts[] | select(.id == $id) | .size_bytes' "$ARTIFACT_LOCK")"
  MODEL_SHA="$(jq -er --arg id "$GGUF_VARIANT" '.artifacts[] | select(.id == $id) | .sha256' "$ARTIFACT_LOCK")"
  VAD_SIZE="$(jq -er '.artifacts[] | select(.id == "fsmn-vad") | .size_bytes' "$ARTIFACT_LOCK")"
  VAD_SHA="$(jq -er '.artifacts[] | select(.id == "fsmn-vad") | .sha256' "$ARTIFACT_LOCK")"
else
  [[ -x "$FUNASR/python/bin/seasnail-funasr" && -f "$FUNASR/sidecar.py" ]] || {
    printf '%s\n' "Missing complete FunASR bundle: $FUNASR" >&2; exit 1;
  }
  # asr+vad 是最小常驻集，始终必需；punc/spk 可选（slim bundle 不含，运行时按需从
  # ModelScope 下载，见 build-macos-arm64.sh --slim 与 downloader.rs）。缺 punc/spk
  # 不阻断打包，与 sidecar resolve_funasr_paths 只要求 asr+vad 的契约一致。
  for component in asr vad; do
    [[ -d "$FUNASR/models/$component" ]] || {
      printf '%s\n' "Missing required FunASR model component in bundle: $component" >&2; exit 1;
    }
  done
  for component in punc spk; do
    [[ -d "$FUNASR/models/$component" ]] || \
      printf '%s\n' "Note: optional FunASR component not in bundle (slim/on-demand): $component" >&2
  done
fi
[[ ! -e "$OUTPUT" ]] || { printf '%s\n' "Refusing to overwrite existing output: $OUTPUT" >&2; exit 1; }
if [[ "$OMIT_FFMPEG" -eq 0 ]]; then
  [[ -n "$FFMPEG" && -x "$FFMPEG" ]] || { printf '%s\n' "Missing bundled ffmpeg. Run scripts/ffmpeg/build-macos-arm64.sh first, then pass --ffmpeg, or use --omit-ffmpeg for realtime-only smoke." >&2; exit 1; }
fi
FRONTEND_SMOKE=0
if [[ -d "$ROOT_DIR/apps/desktop/dist/assets" ]] && \
  grep -qrF --include='*.js' 'data-capsule-smoke' "$ROOT_DIR/apps/desktop/dist/assets"; then
  FRONTEND_SMOKE=1
fi
[[ "$FRONTEND_SMOKE" -eq "$CAPSULE_SMOKE" ]] || {
  printf '%s\n' 'error: frontend dist and --capsule-smoke disagree; build through scripts/build.sh so the fixture UI and disabled native entries use the same mode.' >&2; exit 1;
}

OUTPUT_PARENT="$(dirname "$OUTPUT")"
mkdir -p "$OUTPUT_PARENT"
# `SEASNAIL_MODELS_CATALOG` 会由 Cargo 的 build script 在 crate 工作目录读取，
# 因此暂存目录必须是绝对路径；否则 `--output dist/...` 会找不到临时 catalog。
OUTPUT_PARENT="$(cd "$OUTPUT_PARENT" && pwd)"
STAGE_PARENT="$(mktemp -d "$OUTPUT_PARENT/.seasnail-app.XXXXXX")"
STAGED_APP="$STAGE_PARENT/SeaSnail.app"
cleanup() { rm -rf "$STAGE_PARENT"; }
trap cleanup EXIT

if [[ "$ASR_ENABLED" -eq 1 ]]; then
  mkdir -p "$STAGED_APP/Contents/Resources/asr/sensevoice-small/sherpa_onnx/int8"
  mkdir -p "$STAGED_APP/Contents/Resources/licenses"
  ASR_STAGE="$STAGED_APP/Contents/Resources/asr/sensevoice-small/sherpa_onnx/int8"
  verify_license_tree() {
    local root=$1
    [[ -f "$root/licenses/license-manifest.json" && ! -L "$root/licenses/license-manifest.json" ]] || {
      printf '%s\n' "missing Sherpa license manifest: $root" >&2; exit 1;
    }
    while IFS= read -r row; do
      local relative path expected_size expected_sha expected_mode
      relative=$(jq -r '.path' <<<"$row")
      path="$root/$relative"
      expected_size=$(jq -r '.size_bytes' <<<"$row")
      expected_sha=$(jq -r '.sha256' <<<"$row")
      expected_mode=$(jq -r '.mode' <<<"$row")
      [[ -f "$path" && ! -L "$path" ]] || { printf '%s\n' "missing license: $path" >&2; exit 1; }
      [[ "$(stat -f '%z' "$path")" == "$expected_size" ]] || { printf '%s\n' "license size mismatch: $path" >&2; exit 1; }
      [[ "$(shasum -a 256 "$path" | awk '{print $1}')" == "$expected_sha" ]] || { printf '%s\n' "license SHA-256 mismatch: $path" >&2; exit 1; }
      [[ "$(stat -f '%Lp' "$path")" == "$expected_mode" ]] || { printf '%s\n' "license mode mismatch: $path" >&2; exit 1; }
    done < <(jq -c '((if type == "array" then . else .files end)[])' "$root/licenses/license-manifest.json")
    while IFS= read -r path; do
      local relative=${path#"$root/"}
      [[ "$relative" == licenses/license-manifest.json ]] && continue
      jq -e --arg path "$relative" '((if type == "array" then . else .files end) | map(.path) | index($path) != null)' "$root/licenses/license-manifest.json" >/dev/null || {
        printf '%s\n' "unlisted license file: $path" >&2; exit 1;
      }
    done < <(find "$root/licenses" -type f -not -name license-manifest.json -print)
  }
  verify_sherpa_tree() {
    local root=$1
    while IFS= read -r relative; do
      local expected_size expected_sha path
      expected_size=$(jq -er --arg path "$relative" '.files[] | select(.path == $path) | .size_bytes' "$root/artifact-manifest.json")
      expected_sha=$(jq -er --arg path "$relative" '.files[] | select(.path == $path) | .sha256' "$root/artifact-manifest.json")
      path="$root/$relative"
      [[ -f "$path" && ! -L "$path" ]] || { printf '%s\n' "missing manifest file: $path" >&2; exit 1; }
      [[ "$(stat -f '%z' "$path")" == "$expected_size" ]] || { printf '%s\n' "manifest size mismatch: $path" >&2; exit 1; }
      [[ "$(shasum -a 256 "$path" | awk '{print $1}')" == "$expected_sha" ]] || { printf '%s\n' "manifest SHA-256 mismatch: $path" >&2; exit 1; }
    done < <(jq -r '.files[].path' "$root/artifact-manifest.json")
    while IFS= read -r path; do
      local relative=${path#"$root/"}
      case "$relative" in
        artifact-manifest.json|licenses/*) continue ;;
      esac
      jq -e --arg path "$relative" '[.files[].path] | index($path) != null' "$root/artifact-manifest.json" >/dev/null || {
        printf '%s\n' "unlisted Sherpa artifact file: $path" >&2; exit 1;
      }
    done < <(find "$root" -type f -not -path "$root/artifact-manifest.json" -print)
  }
  verify_sherpa_tree "$ASR_ROOT"
  ditto "$ASR_ROOT" "$ASR_STAGE"
  verify_license_tree "$ASR_ROOT"
  while IFS= read -r relative; do
    mkdir -p "$STAGED_APP/Contents/Resources/$(dirname "$relative")"
    cp -p "$ASR_ROOT/$relative" "$STAGED_APP/Contents/Resources/$relative"
  done < <(jq -r '((if type == "array" then . else .files end)[] | .path)' "$ASR_ROOT/licenses/license-manifest.json")
  cp -p "$ASR_ROOT/licenses/license-manifest.json" "$STAGED_APP/Contents/Resources/licenses/license-manifest.json"
elif [[ "$GGUF_ENABLED" -eq 1 ]]; then
  [[ -f "$MODELS_CATALOG_TEMPLATE" ]] || { printf '%s\n' 'missing daemon model catalog template' >&2; exit 1; }
  GGUF_CATALOG="$STAGE_PARENT/models.json"
  jq \
    --arg variant "$GGUF_VARIANT" --arg sha256 "$MODEL_SHA" --argjson size_bytes "$MODEL_SIZE" \
    'if ([.models[] | select(.id == "sensevoice-small")] | length) != 1 then error("missing sensevoice-small catalog entry") else . end
     | (.models[] | select(.id == "sensevoice-small")) |= (.runtime = "gguf" | .bundled = true | .default = true | .variant = $variant | .sha256 = $sha256 | .size_bytes = $size_bytes)
     | if ([.models[] | select(.default == true)] | length) == 1 then . else error("candidate catalog must have one default model") end' \
    "$MODELS_CATALOG_TEMPLATE" > "$GGUF_CATALOG"
fi

if [[ "$SKIP_RUST_BUILD" -eq 0 ]]; then
  ensure_cargo
  "$ROOT_DIR/scripts/macos/build-post-paste-monitor.sh"
  build_env=()
  if [[ "$CAPSULE_SMOKE" -eq 1 ]]; then
    build_env+=(SEASNAIL_CAPSULE_SMOKE=1)
  fi
  if [[ "$DEBUG_BACKEND_SWITCHING" -eq 1 ]]; then
    (cd "$ROOT_DIR" && run_cargo_with_env build --release -p seasnail-daemon -p seasnail-desktop --features seasnail-daemon/debug-backend-switching)
  elif [[ "$GGUF_ENABLED" -eq 1 ]]; then
    build_env+=(SEASNAIL_MODELS_CATALOG="$GGUF_CATALOG")
    (cd "$ROOT_DIR" && run_cargo_with_env build --release -p seasnail-daemon -p seasnail-desktop)
  else
    (cd "$ROOT_DIR" && run_cargo_with_env build --release -p seasnail-daemon -p seasnail-desktop)
  fi
fi
mkdir -p "$STAGED_APP/Contents/MacOS" "$STAGED_APP/Contents/Resources/licenses"
cp "$ROOT_DIR/packaging/macos/Info.plist" "$STAGED_APP/Contents/Info.plist"
if [[ "$CAPSULE_SMOKE" -eq 1 ]]; then
  # Keep the isolated fixture app distinct from an installed SeaSnail instance.
  /usr/libexec/PlistBuddy -c 'Set :CFBundleIdentifier com.seasnail.app.capsule-smoke' "$STAGED_APP/Contents/Info.plist"
  /usr/libexec/PlistBuddy -c 'Set :CFBundleDisplayName SeaSnail Capsule Smoke' "$STAGED_APP/Contents/Info.plist"
  /usr/libexec/PlistBuddy -c 'Set :CFBundleName SeaSnail Capsule Smoke' "$STAGED_APP/Contents/Info.plist"
fi
cp "$ROOT_DIR/apps/desktop/src-tauri/icons/SeaSnail.icns" "$STAGED_APP/Contents/Resources/SeaSnail.icns"
cp "$ROOT_DIR/target/release/seasnail-desktop" "$STAGED_APP/Contents/MacOS/SeaSnail"
cp "$ROOT_DIR/target/release/seasnail-daemon" "$STAGED_APP/Contents/MacOS/seasnail-daemon"
cp "$ROOT_DIR/target/release/seasnail-post-paste-monitor" "$STAGED_APP/Contents/MacOS/seasnail-post-paste-monitor"
chmod +x "$STAGED_APP/Contents/MacOS/SeaSnail" "$STAGED_APP/Contents/MacOS/seasnail-daemon" "$STAGED_APP/Contents/MacOS/seasnail-post-paste-monitor"
if [[ "$ASR_ENABLED" -eq 1 ]]; then
  verify_license_tree "$ASR_STAGE"
  verify_sherpa_tree "$ASR_STAGE"
  [[ -x "$STAGED_APP/Contents/Resources/asr/sensevoice-small/sherpa_onnx/int8/bin/seasnail-sherpa-sidecar" ]] || {
    printf '%s\n' 'missing Sherpa sidecar' >&2; exit 1;
  }
  [[ "$(find "$STAGED_APP/Contents/Resources/asr" "$STAGED_APP/Contents/Resources/licenses" -type l -print -quit)" == "" ]] || {
    printf '%s\n' 'Sherpa artifact contains symbolic links' >&2; exit 1;
  }
  native_queue=( \
    "$STAGED_APP/Contents/Resources/asr/sensevoice-small/sherpa_onnx/int8/bin/seasnail-sherpa-sidecar" \
    "$STAGED_APP/Contents/Resources/asr/sensevoice-small/sherpa_onnx/int8/lib/libsherpa-onnx-c-api.dylib" \
    "$STAGED_APP/Contents/Resources/asr/sensevoice-small/sherpa_onnx/int8/lib/libonnxruntime.1.dylib"
  )
  native_seen='|'
  while [[ "${#native_queue[@]}" -gt 0 ]]; do
    native="${native_queue[0]}"
    native_queue=("${native_queue[@]:1}")
    [[ "$native_seen" == *"|$native|"* ]] && continue
    native_seen+="$native|"
    [[ "$(lipo -archs "$native")" == "arm64" ]] || {
      printf '%s\n' "Sherpa native file is not thin arm64: $native" >&2; exit 1;
    }
    while IFS= read -r dependency; do
      case "$dependency" in
        /usr/lib/*|/System/Library/*) ;;
        @rpath/*)
          resolved="$STAGED_APP/Contents/Resources/asr/sensevoice-small/sherpa_onnx/int8/lib/${dependency#@rpath/}"
          [[ -f "$resolved" ]] || {
            printf '%s\n' "unresolved Sherpa @rpath dependency: $dependency" >&2; exit 1;
          }
          native_queue+=("$resolved")
          ;;
        @loader_path/*)
          resolved="$(dirname "$native")/${dependency#@loader_path/}"
          [[ -f "$resolved" ]] || {
            printf '%s\n' "unresolved Sherpa @loader_path dependency: $dependency" >&2; exit 1;
          }
          native_queue+=("$resolved")
          ;;
        *)
          printf '%s\n' "non-system absolute or unsupported Sherpa dependency: $dependency" >&2; exit 1
          ;;
      esac
    done < <(otool -L "$native" | sed -n '2,$p' | awk '{print $1}')
  done
  [[ ! -e "$STAGED_APP/Contents/Resources/funasr" && ! -e "$STAGED_APP/Contents/Resources/gguf" ]] || {
    printf '%s\n' 'Sherpa candidate must not include legacy ASR resources' >&2; exit 1;
  }
elif [[ "$GGUF_ENABLED" -eq 1 ]]; then
  mkdir -p "$STAGED_APP/Contents/Resources/gguf"
  GGUF_STAGE="$STAGED_APP/Contents/Resources/gguf"
  cp -p "$GGUF_SERVER" "$GGUF_STAGE/sensevoice-server"
  cp -p "$GGUF_MODEL" "$GGUF_STAGE/sensevoice.gguf"
  cp -p "$GGUF_VAD" "$GGUF_STAGE/fsmn-vad.gguf"
  chmod +x "$GGUF_STAGE/sensevoice-server"
  SERVER_SIZE="$(stat -f '%z' "$GGUF_STAGE/sensevoice-server")"
  SERVER_SHA="$(shasum -a 256 "$GGUF_STAGE/sensevoice-server" | awk '{print $1}')"
  jq -n \
    --arg variant "$GGUF_VARIANT" \
    --arg source_revision "$SOURCE_REVISION" \
    --arg server_sha "$SERVER_SHA" --argjson server_size "$SERVER_SIZE" \
    --arg model_sha "$MODEL_SHA" --argjson model_size "$MODEL_SIZE" \
    --arg vad_sha "$VAD_SHA" --argjson vad_size "$VAD_SIZE" \
    '{schema_version:1,runtime:"gguf",model_id:"sensevoice-small",variant:$variant,model_file:"sensevoice.gguf",vad_file:"fsmn-vad.gguf",source_revision:$source_revision,api_contract_version:1,files:[{path:"sensevoice-server",sha256:$server_sha,size_bytes:$server_size},{path:"sensevoice.gguf",sha256:$model_sha,size_bytes:$model_size},{path:"fsmn-vad.gguf",sha256:$vad_sha,size_bytes:$vad_size}]}' \
    > "$GGUF_STAGE/runtime-manifest.json"
  for notice in SENSEVOICE-NOTICE.md SENSEVOICE-MIT.txt LLAMA_CPP-NOTICE.md LLAMA_CPP-MIT.txt GGUF-MODEL-LICENSE.md APACHE-2.0.txt; do
    cp "$ROOT_DIR/scripts/sensevoice/licenses/$notice" "$STAGED_APP/Contents/Resources/licenses/$notice"
  done
else
  ditto "$FUNASR" "$STAGED_APP/Contents/Resources/funasr"
  cp "$ROOT_DIR/scripts/funasr/THIRD_PARTY_NOTICES.md" "$STAGED_APP/Contents/Resources/licenses/THIRD_PARTY_NOTICES.md"
fi
# 此标记仅由本未签名开发打包脚本写入。Tauri 检出后才会让 daemon 使用普通文件
# Keychain，以绕开 Data Protection Keychain 对签名 entitlement 的要求；正式发行包
# 绝不能携带此文件。
printf '%s\n' 'UNSIGNED DEVELOPMENT BUILD ONLY: use file Keychain, never distribute.' \
  > "$STAGED_APP/Contents/Resources/SEASNAIL_DEV_FILE_KEYCHAIN"
if [[ "$CAPSULE_SMOKE" -eq 1 ]]; then
  printf '%s\n' 'ISOLATED CAPSULE FIXTURE SMOKE: production recording entries disabled.' \
    > "$STAGED_APP/Contents/Resources/SEASNAIL_CAPSULE_SMOKE"
fi
if [[ "$OMIT_FFMPEG" -eq 0 ]]; then
  cp "$FFMPEG" "$STAGED_APP/Contents/Resources/ffmpeg"
  chmod +x "$STAGED_APP/Contents/Resources/ffmpeg"
  FFMPEG_BUNDLE_DIR="$(dirname "$FFMPEG")"
  [[ -f "$FFMPEG_BUNDLE_DIR/BUILD-INFO.txt" && -f "$FFMPEG_BUNDLE_DIR/licenses/FFMPEG-LGPL-2.1-or-later.txt" ]] || {
    printf '%s\n' "ffmpeg bundle lacks required LGPL distribution materials: $FFMPEG_BUNDLE_DIR" >&2; exit 1;
  }
  cp "$FFMPEG_BUNDLE_DIR/BUILD-INFO.txt" "$STAGED_APP/Contents/Resources/licenses/FFMPEG-BUILD-INFO.txt"
  cp "$FFMPEG_BUNDLE_DIR/licenses/FFMPEG-LGPL-2.1-or-later.txt" "$STAGED_APP/Contents/Resources/licenses/FFMPEG-LGPL-2.1-or-later.txt"
  python3 "$ROOT_DIR/scripts/licenses/verify-ffmpeg-source.py" "$FFMPEG_BUNDLE_DIR"
  mkdir -p "$STAGED_APP/Contents/Resources/licenses/ffmpeg"
  ditto "$FFMPEG_BUNDLE_DIR/corresponding-source" \
    "$STAGED_APP/Contents/Resources/licenses/ffmpeg/corresponding-source"
  cp "$FFMPEG_BUNDLE_DIR/source-manifest.json" \
    "$STAGED_APP/Contents/Resources/licenses/ffmpeg/source-manifest.json"
fi
[[ -x "$STAGED_APP/Contents/MacOS/SeaSnail" ]] || { printf '%s\n' 'missing Tauri GUI executable' >&2; exit 1; }
[[ -x "$STAGED_APP/Contents/MacOS/seasnail-daemon" ]] || { printf '%s\n' 'missing daemon executable' >&2; exit 1; }
[[ -x "$STAGED_APP/Contents/MacOS/seasnail-post-paste-monitor" ]] || { printf '%s\n' 'missing post-paste monitor helper' >&2; exit 1; }
[[ -f "$STAGED_APP/Contents/Resources/SeaSnail.icns" ]] || { printf '%s\n' 'missing SeaSnail app icon' >&2; exit 1; }
if [[ "$GGUF_ENABLED" -eq 1 ]]; then
  [[ -x "$STAGED_APP/Contents/Resources/gguf/sensevoice-server" ]] || { printf '%s\n' 'missing GGUF server' >&2; exit 1; }
  jq -e '(.runtime == "gguf") and (.model_id == "sensevoice-small") and (.files | length == 3)' \
    "$STAGED_APP/Contents/Resources/gguf/runtime-manifest.json" >/dev/null || {
      printf '%s\n' 'invalid GGUF runtime manifest' >&2; exit 1;
    }
  jq -e --slurpfile runtime "$STAGED_APP/Contents/Resources/gguf/runtime-manifest.json" '
    ([.models[] | select(.id == "sensevoice-small")]) as $models
    | ($models | length == 1)
      and ($models[0].runtime == $runtime[0].runtime)
      and ($models[0].default == true)
      and ($models[0].variant == $runtime[0].variant)
      and ($models[0].size_bytes == ($runtime[0].files[] | select(.path == "sensevoice.gguf") | .size_bytes))
      and ($models[0].sha256 == ($runtime[0].files[] | select(.path == "sensevoice.gguf") | .sha256))
  ' "$GGUF_CATALOG" >/dev/null || {
    printf '%s\n' 'daemon model catalog and GGUF runtime manifest disagree' >&2; exit 1;
  }
  [[ ! -e "$STAGED_APP/Contents/Resources/funasr" ]] || { printf '%s\n' 'GGUF candidate must not include FunASR resources' >&2; exit 1; }
  [[ "$(find "$STAGED_APP/Contents/Resources/gguf" -mindepth 1 -maxdepth 1 -print | sed 's#.*/##' | sort | tr '\n' ' ')" == "fsmn-vad.gguf runtime-manifest.json sensevoice-server sensevoice.gguf " ]] || {
    printf '%s\n' 'GGUF resource directory contains unexpected files or weights' >&2; exit 1;
  }
  for resource in sensevoice-server sensevoice.gguf fsmn-vad.gguf runtime-manifest.json; do
    [[ -f "$STAGED_APP/Contents/Resources/gguf/$resource" && ! -L "$STAGED_APP/Contents/Resources/gguf/$resource" ]] || {
      printf '%s\n' "invalid GGUF resource layout: $resource" >&2; exit 1;
    }
  done
  for notice in SENSEVOICE-NOTICE.md SENSEVOICE-MIT.txt LLAMA_CPP-NOTICE.md LLAMA_CPP-MIT.txt GGUF-MODEL-LICENSE.md APACHE-2.0.txt; do
    [[ -f "$STAGED_APP/Contents/Resources/licenses/$notice" ]] || { printf '%s\n' "missing GGUF license material: $notice" >&2; exit 1; }
  done
fi
license_args=(--output "$STAGED_APP/Contents/Resources/licenses/application")
if [[ "$ASR_ENABLED" -eq 1 ]]; then
  license_args+=(--native-cache "${SEASNAIL_SHERPA_CACHE:-$ROOT_DIR/third_party/sherpa/macos-arm64/cache}")
fi
python3 "$ROOT_DIR/scripts/licenses/collect.py" "${license_args[@]}"

plutil -lint "$STAGED_APP/Contents/Info.plist" >/dev/null || {
  printf '%s\n' 'invalid Info.plist' >&2; exit 1;
}
mv "$STAGED_APP" "$OUTPUT"
printf 'Created unsigned development app: %s\n' "$OUTPUT"
