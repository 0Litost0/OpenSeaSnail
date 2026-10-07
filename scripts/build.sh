#!/usr/bin/env bash
# SeaSnail 统一开发构建入口。
#
# 默认构建 Sherpa ONNX 本地未签名 macOS Apple Silicon 开发包；不会覆盖已有产物。
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DESKTOP_DIR="$ROOT_DIR/apps/desktop"
# 当前已验证的完整运行包；名称同时反映 sidecar 在系统进程列表中的可识别名称。
FUNASR_ROOT="$ROOT_DIR/third_party/funasr/macos-arm64/bundle-process-name"
GGUF_ROOT="$ROOT_DIR/third_party/sensevoice/macos-arm64"
GGUF_VARIANT="q8"
ASR_BACKEND="sherpa_onnx"
ASR_MODEL="sensevoice-small"
ASR_VARIANT="int8"
ASR_ROOT="${SEASNAIL_SHERPA_ARTIFACT_ROOT:-$ROOT_DIR/third_party/sherpa/macos-arm64/artifact}"
SHERPA_ROOT="$ROOT_DIR/third_party/sherpa/macos-arm64"
SHERPA_CACHE="$SHERPA_ROOT/cache"
SHERPA_SOURCE="$SHERPA_ROOT/sources/sherpa-onnx"
ONNXRUNTIME_SOURCE="$SHERPA_ROOT/sources/onnxruntime"
SHERPA_BUILD_ROOT="$SHERPA_ROOT/build"
FFMPEG_ROOT="$ROOT_DIR/third_party/ffmpeg/macos-arm64/bundle"
OUTPUT="$ROOT_DIR/dist/SeaSnail.app"
BOOTSTRAP_LOCK=0

BUILD_FRONTEND=0
BUILD_RUST=0
BUILD_FUNASR=0
BUILD_GGUF=0
BUILD_FFMPEG=0
BUILD_APP=0
BUILD_RUNTIME=0
AUTO_PREPARE_RUNTIME=0
SLIM=0
DEBUG_BACKEND_SWITCHING=0
CAPSULE_SMOKE=0
OMIT_FFMPEG=0
FUNASR_ROOT_EXPLICIT=0
GGUF_ROOT_EXPLICIT=0
GGUF_VARIANT_EXPLICIT=0
ASR_ROOT_EXPLICIT=0
ASR_BACKEND_EXPLICIT=0

usage() {
  cat <<'EOF'
Usage: scripts/build.sh <target> [options]

Targets (one or more):
  --frontend              构建 React/Vite 前端到 apps/desktop/dist/
  --rust                  构建整个 Rust workspace 的 release 二进制与库
  --prepare-runtime       准备 Sherpa artifact；已有制品则复用，否则从本地锁定输入构建
  --build-runtime         `--prepare-runtime` 的显式别名，供维护者构建运行时
  --funasr                从已准备的本地缓存构建 FunASR bundle（兼容路径）
  --ffmpeg                构建内置、LGPL-only 的 Apple Silicon ffmpeg
  --app                   构建前端、Rust 并组装未签名 Sherpa SeaSnail.app
  --package               准备 Sherpa/ffmpeg、构建前端和 Rust，并组装未签名 App
  --all                   构建前端、Rust workspace，并组装未签名 Sherpa SeaSnail.app

Options:
  --output APP_PATH       App 输出路径（默认 dist/SeaSnail.app）
  --asr-backend BACKEND   ASR backend：sherpa_onnx（默认）、funasr 或 gguf
  --asr-model MODEL       ASR 逻辑模型（Sherpa 默认 sensevoice-small）
  --asr-variant VARIANT   ASR artifact 变体（Sherpa 默认 int8）
  --asr-root DIR          已准备的 ASR artifact 根目录
  --sherpa-cache DIR      Sherpa 锁定归档缓存（默认 third_party/sherpa/macos-arm64/cache）
  --sherpa-source DIR     Sherpa ONNX 源码 checkout（默认 third_party/sherpa/macos-arm64/sources/sherpa-onnx）
  --onnxruntime-source DIR ONNX Runtime 源码 checkout（默认 third_party/sherpa/macos-arm64/sources/onnxruntime）
  --runtime-build-root DIR Sherpa 中间构建目录（默认 third_party/sherpa/macos-arm64/build）
  --funasr-root DIR       兼容参数：打包时使用的完整 FunASR bundle 路径
  --gguf                  将 --app/--all 的运行时资源形态切换为 SenseVoice GGUF
  --gguf-root DIR         打包时使用的已准备 GGUF 构建根（含 bundle/ 与 cache/models/）
  --gguf-variant VARIANT  GGUF 模型变体：q8（默认）、f16 或 f32
  --ffmpeg-root DIR       打包时使用的已构建 ffmpeg bundle（默认 third_party/ffmpeg/macos-arm64/bundle）
  --omit-ffmpeg           仅构建实时转录 smoke 包；不打包 FFmpeg，文件导入不可用
  --bootstrap-lock        仅与 --funasr 联用，生成待审核依赖锁候选文件
  --slim                  仅与 --funasr 联用：重建为精简 FunASR bundle（仅内置
                          asr+vad，punc/spk 不打包，运行时按需从 ModelScope 下载）。
                          不加则内置全部四个模型（开发默认）。注意 --all 不含
                          --funasr，故 --all --slim（不带 --funasr）不重建 bundle、
                          --slim 不生效，仍复用既有 bundle-process-name（默认全量）。
  --debug-backend-switching
                          构建仅供内部迁移验证的 App：显示调试后端切换，允许在
                          GGUF/FunASR 间显式选择并在重启后生效。不得用于 release。
  --capsule-smoke         构建隔离胶囊真机冒烟包：循环中英文 fixture，并禁用生产录音
                          快捷键、状态栏录音入口和 CPAL 错误消费线程。不得用于 release。
  -h, --help              显示帮助

Notes:
  --package 会优先复用已有 artifact；artifact 不存在时，会使用仓库内的
  cache/source/build 目录执行本地锁定构建。源码和 cache 缺失时会给出明确提示。
  --app/--all 不会自动准备 Sherpa artifact，需要 --asr-root 指向已准备 artifact
  （或设置 SEASNAIL_SHERPA_ARTIFACT_ROOT）。
  --funasr-root/--gguf-root 仅用于兼容候选构建。
  --ffmpeg（以及 --all）首次会从 ffmpeg.org 下载已锁定校验和的源码并本地编译。
  --funasr 只使用本地 cache，且不会覆盖非空输出目录。完整准备过程见
  third_party/funasr/macos-arm64/README.md。
EOF
}

die() { printf '%s\n' "error: $*" >&2; exit 1; }

ensure_cargo() {
  if command -v cargo >/dev/null 2>&1; then
    return
  fi

  local rustup_bin="${HOME:-}/.cargo/bin"
  if [[ -n "${HOME:-}" && -x "$rustup_bin/cargo" ]]; then
    export PATH="$rustup_bin:$PATH"
    return
  fi

  die "cargo was not found. Install Rust with rustup (https://rustup.rs), then restart the terminal or run: source \"$HOME/.cargo/env\""
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --frontend) BUILD_FRONTEND=1 ;;
    --rust) BUILD_RUST=1 ;;
    --funasr) [[ "$ASR_BACKEND_EXPLICIT" -eq 0 || "$ASR_BACKEND" == funasr ]] || die "--funasr conflicts with --asr-backend $ASR_BACKEND"; BUILD_FUNASR=1; ASR_BACKEND="funasr" ;;
    --gguf) [[ "$ASR_BACKEND_EXPLICIT" -eq 0 || "$ASR_BACKEND" == gguf ]] || die "--gguf conflicts with --asr-backend $ASR_BACKEND"; BUILD_GGUF=1; ASR_BACKEND="gguf" ;;
    --ffmpeg) BUILD_FFMPEG=1 ;;
    --app) BUILD_FRONTEND=1; BUILD_APP=1 ;;
    --package) BUILD_FRONTEND=1; BUILD_RUST=1; BUILD_FFMPEG=1; BUILD_RUNTIME=1; BUILD_APP=1; AUTO_PREPARE_RUNTIME=1 ;;
    --prepare-runtime|--build-runtime) BUILD_RUNTIME=1 ;;
    --all) BUILD_FRONTEND=1; BUILD_RUST=1; BUILD_FFMPEG=1; BUILD_APP=1 ;;
    --output)
      [[ $# -ge 2 ]] || die "--output requires an app path"
      OUTPUT="$2"; shift ;;
    --asr-backend)
      [[ $# -ge 2 ]] || die "--asr-backend requires a backend"
      [[ "$BUILD_FUNASR" -eq 0 || "$2" == funasr ]] || die "--asr-backend $2 conflicts with --funasr"
      [[ "$BUILD_GGUF" -eq 0 || "$2" == gguf ]] || die "--asr-backend $2 conflicts with --gguf"
      [[ "$FUNASR_ROOT_EXPLICIT" -eq 0 || "$2" == funasr ]] || die "--asr-backend $2 conflicts with --funasr-root"
      [[ "$GGUF_ROOT_EXPLICIT" -eq 0 || "$2" == gguf ]] || die "--asr-backend $2 conflicts with --gguf-root"
      ASR_BACKEND="$2"; ASR_BACKEND_EXPLICIT=1; shift ;;
    --asr-model)
      [[ $# -ge 2 ]] || die "--asr-model requires a model"
      ASR_MODEL="$2"; shift ;;
    --asr-variant)
      [[ $# -ge 2 ]] || die "--asr-variant requires a variant"
      ASR_VARIANT="$2"; shift ;;
    --asr-root)
      [[ $# -ge 2 ]] || die "--asr-root requires a directory"
      ASR_ROOT="$2"; ASR_ROOT_EXPLICIT=1; shift ;;
    --sherpa-cache)
      [[ $# -ge 2 ]] || die "--sherpa-cache requires a directory"
      SHERPA_CACHE="$2"; shift ;;
    --sherpa-source)
      [[ $# -ge 2 ]] || die "--sherpa-source requires a directory"
      SHERPA_SOURCE="$2"; shift ;;
    --onnxruntime-source)
      [[ $# -ge 2 ]] || die "--onnxruntime-source requires a directory"
      ONNXRUNTIME_SOURCE="$2"; shift ;;
    --runtime-build-root)
      [[ $# -ge 2 ]] || die "--runtime-build-root requires a directory"
      SHERPA_BUILD_ROOT="$2"; shift ;;
    --funasr-root)
      [[ $# -ge 2 ]] || die "--funasr-root requires a directory"
      [[ "$ASR_BACKEND_EXPLICIT" -eq 0 || "$ASR_BACKEND" == funasr ]] || die "--funasr-root conflicts with --asr-backend $ASR_BACKEND"
      FUNASR_ROOT="$2"; FUNASR_ROOT_EXPLICIT=1; ASR_BACKEND="funasr"; shift ;;
    --gguf-root)
      [[ $# -ge 2 ]] || die "--gguf-root requires a directory"
      [[ "$ASR_BACKEND_EXPLICIT" -eq 0 || "$ASR_BACKEND" == gguf ]] || die "--gguf-root conflicts with --asr-backend $ASR_BACKEND"
      GGUF_ROOT="$2"; GGUF_ROOT_EXPLICIT=1; shift ;;
    --gguf-variant)
      [[ $# -ge 2 ]] || die "--gguf-variant requires q8, f16, or f32"
      GGUF_VARIANT="$2"; GGUF_VARIANT_EXPLICIT=1; shift ;;
    --ffmpeg-root)
      [[ $# -ge 2 ]] || die "--ffmpeg-root requires a directory"
      FFMPEG_ROOT="$2"; shift ;;
    --omit-ffmpeg) OMIT_FFMPEG=1 ;;
    --bootstrap-lock) BOOTSTRAP_LOCK=1 ;;
    --slim) SLIM=1 ;;
    --debug-backend-switching) DEBUG_BACKEND_SWITCHING=1 ;;
    --capsule-smoke) CAPSULE_SMOKE=1 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
  shift
done

[[ "$BUILD_FRONTEND" -eq 1 || "$BUILD_RUST" -eq 1 || "$BUILD_RUNTIME" -eq 1 || "$BUILD_FUNASR" -eq 1 || "$BUILD_FFMPEG" -eq 1 || "$BUILD_APP" -eq 1 ]] || {
  usage >&2
  die "select at least one target"
}
[[ "$BOOTSTRAP_LOCK" -eq 0 || "$BUILD_FUNASR" -eq 1 ]] || die "--bootstrap-lock can only be used with --funasr"
[[ "$BUILD_GGUF" -eq 0 || "$BUILD_FUNASR" -eq 0 ]] || die "--gguf and --funasr cannot be combined in one candidate build"
[[ "$BUILD_GGUF" -eq 0 || "$SLIM" -eq 0 ]] || die "--slim only applies to --funasr and cannot be combined with --gguf"
[[ "$OMIT_FFMPEG" -eq 0 || "$BUILD_FFMPEG" -eq 0 ]] || die "--omit-ffmpeg cannot be combined with --package or --all"
[[ "$GGUF_VARIANT" == q8 || "$GGUF_VARIANT" == f16 || "$GGUF_VARIANT" == f32 ]] || die "unsupported --gguf-variant: $GGUF_VARIANT"
[[ "$FUNASR_ROOT_EXPLICIT" -eq 0 || "$GGUF_ROOT_EXPLICIT" -eq 0 ]] || die "--funasr-root and --gguf-root cannot be combined in one candidate build"
[[ "$GGUF_ROOT_EXPLICIT" -eq 0 || "$BUILD_GGUF" -eq 1 ]] || die "--gguf-root requires --gguf"
[[ "$FUNASR_ROOT_EXPLICIT" -eq 0 || "$BUILD_GGUF" -eq 0 ]] || die "--funasr-root cannot be used with --gguf"
[[ "$GGUF_VARIANT_EXPLICIT" -eq 0 || "$BUILD_GGUF" -eq 1 ]] || die "--gguf-variant requires --gguf"
[[ "$BUILD_FUNASR" -eq 0 || "$ASR_BACKEND" == funasr ]] || die "--funasr cannot be combined with a non-FunASR --asr-backend"
[[ "$BUILD_GGUF" -eq 0 || "$ASR_BACKEND" == gguf ]] || die "--gguf cannot be combined with a non-GGUF --asr-backend"
[[ "$ASR_ROOT_EXPLICIT" -eq 0 || "$FUNASR_ROOT_EXPLICIT" -eq 0 ]] || die "--asr-root and --funasr-root cannot be combined"
[[ "$ASR_ROOT_EXPLICIT" -eq 0 || "$GGUF_ROOT_EXPLICIT" -eq 0 ]] || die "--asr-root and --gguf-root cannot be combined"
[[ "$BUILD_GGUF" -eq 0 || "$BUILD_APP" -eq 1 ]] || die "--gguf is only valid when building an App candidate (--app or --all)"
[[ "$DEBUG_BACKEND_SWITCHING" -eq 0 || "$BUILD_GGUF" -eq 0 ]] || die "--debug-backend-switching cannot build a GGUF-only candidate; it requires the separate dual-runtime migration package"
[[ "$ASR_BACKEND" == sherpa_onnx || "$ASR_BACKEND" == funasr || "$ASR_BACKEND" == gguf ]] || die "unsupported --asr-backend: $ASR_BACKEND"
if [[ "$ASR_BACKEND" == sherpa_onnx ]]; then
  [[ "$ASR_MODEL" == sensevoice-small && "$ASR_VARIANT" == int8 ]] || die "Sherpa candidate requires sensevoice-small/int8"
  [[ "$BUILD_APP" -eq 0 || "$AUTO_PREPARE_RUNTIME" -eq 1 || -d "$ASR_ROOT" ]] || die "Sherpa artifact root not found: $ASR_ROOT; run --prepare-runtime or use --package"
fi
[[ "$ASR_BACKEND" != funasr || "$FUNASR_ROOT_EXPLICIT" -eq 1 || "$BUILD_FUNASR" -eq 1 ]] || die "FunASR backend requires --funasr-root or --funasr"

build_frontend() {
  printf '%s\n' '[前端] 构建 React/Vite 前端'
  if [[ "$DEBUG_BACKEND_SWITCHING" -eq 1 && "$CAPSULE_SMOKE" -eq 1 ]]; then
    VITE_DEBUG_BACKEND_SWITCHING=true VITE_CAPSULE_SMOKE=true pnpm --dir "$DESKTOP_DIR" build
  elif [[ "$DEBUG_BACKEND_SWITCHING" -eq 1 ]]; then
    VITE_DEBUG_BACKEND_SWITCHING=true pnpm --dir "$DESKTOP_DIR" build
  elif [[ "$CAPSULE_SMOKE" -eq 1 ]]; then
    VITE_CAPSULE_SMOKE=true pnpm --dir "$DESKTOP_DIR" build
  else
    pnpm --dir "$DESKTOP_DIR" build
  fi
}

build_rust() {
  printf '%s\n' '[Rust] 构建 workspace（release）'
  ensure_cargo
  "$ROOT_DIR/scripts/macos/build-post-paste-monitor.sh"
  if [[ "$DEBUG_BACKEND_SWITCHING" -eq 1 ]]; then
    (cd "$ROOT_DIR" && cargo build --release --workspace --features seasnail-daemon/debug-backend-switching)
  else
    (cd "$ROOT_DIR" && cargo build --release --workspace)
  fi
}

build_funasr() {
  printf '%s\n' '[兼容] 从本地缓存构建 FunASR bundle'
  local args=(--output "$FUNASR_ROOT")
  if [[ "$BOOTSTRAP_LOCK" -eq 1 ]]; then
    args+=(--bootstrap-lock)
  fi
  if [[ "$SLIM" -eq 1 ]]; then
    args+=(--slim)
  fi
  "$ROOT_DIR/scripts/funasr/build-macos-arm64.sh" "${args[@]}"
}

build_ffmpeg() {
  if [[ -x "$FFMPEG_ROOT/ffmpeg" ]]; then
    [[ -f "$FFMPEG_ROOT/BUILD-INFO.txt" && -f "$FFMPEG_ROOT/licenses/FFMPEG-LGPL-2.1-or-later.txt" ]] || \
      die "existing ffmpeg bundle is incomplete: $FFMPEG_ROOT"
    printf '%s\n' '[ffmpeg] 复用已存在的 LGPL bundle'
    return
  fi
  printf '%s\n' '[ffmpeg] 构建内置 LGPL bundle'
  "$ROOT_DIR/scripts/ffmpeg/build-macos-arm64.sh" --output "$FFMPEG_ROOT"
}

build_runtime() {
  if [[ -f "$ASR_ROOT/artifact-manifest.json" ]]; then
    cmp -s "$ASR_ROOT/artifact-manifest.json" "$ROOT_DIR/scripts/sherpa/artifact-manifest.macos-arm64.json" || {
      die "existing Sherpa artifact uses a different manifest: $ASR_ROOT; move it aside and rebuild the reviewed artifact (it will not be overwritten)"
    }
    printf '%s\n' '[Sherpa] 复用已存在的 artifact'
    return
  fi
  [[ "$ASR_BACKEND" == sherpa_onnx ]] || die "--prepare-runtime only supports the Sherpa backend"
  [[ -d "$SHERPA_CACHE" ]] || die "Sherpa cache not found: $SHERPA_CACHE; prepare locked inputs or pass --sherpa-cache"
  [[ -d "$SHERPA_SOURCE" ]] || die "Sherpa source not found: $SHERPA_SOURCE; pass --sherpa-source"
  [[ -d "$ONNXRUNTIME_SOURCE" ]] || die "ONNX Runtime source not found: $ONNXRUNTIME_SOURCE; pass --onnxruntime-source"
  [[ "$ASR_ROOT" == "$ROOT_DIR/third_party/sherpa/macos-arm64/artifact" ]] || {
    die "automatic runtime preparation requires the default artifact path; use --asr-root with an already prepared artifact"
  }
  local probe_root="$SHERPA_BUILD_ROOT/probe"
  printf '%s\n' '[Sherpa] 构建 native install（仓库内中间目录）'
  "$ROOT_DIR/scripts/sherpa/build-probe-macos-arm64.sh" \
    --source "$SHERPA_SOURCE" \
    --onnxruntime-source "$ONNXRUNTIME_SOURCE" \
    --cache "$SHERPA_CACHE" \
    --output "$probe_root"
  printf '%s\n' '[Sherpa] 组装并校验 artifact（仓库内制品目录）'
  "$ROOT_DIR/scripts/sherpa/prepare-macos-arm64.sh" \
    --source "$SHERPA_SOURCE" \
    --onnxruntime-source "$ONNXRUNTIME_SOURCE" \
    --cache "$SHERPA_CACHE" \
    --native-install "$probe_root/install" \
    --output "$ASR_ROOT"
}

package_app() {
  printf '%s\n' '[App] 组装未签名 macOS 开发包'
  local args=(--output "$OUTPUT")
  if [[ "$OMIT_FFMPEG" -eq 1 ]]; then
    args+=(--omit-ffmpeg)
  else
    args+=(--ffmpeg "$FFMPEG_ROOT/ffmpeg")
  fi
  if [[ "$ASR_BACKEND" == sherpa_onnx ]]; then
    args+=(--asr-backend "$ASR_BACKEND" --asr-model "$ASR_MODEL" --asr-variant "$ASR_VARIANT" --asr-root "$ASR_ROOT")
  elif [[ "$ASR_BACKEND" == gguf ]]; then
    args+=(--gguf-root "$GGUF_ROOT" --gguf-variant "$GGUF_VARIANT")
  else
    args+=(--funasr-root "$FUNASR_ROOT")
  fi
  # debug feature 必须由组装脚本亲自编译，避免 `--skip-rust-build` 复用普通 release
  # 二进制而打出「前端可切换、daemon 拒绝切换」的不一致包。
  if [[ "$BUILD_RUST" -eq 1 && "$DEBUG_BACKEND_SWITCHING" -eq 0 && "$ASR_BACKEND" == funasr ]]; then
    args+=(--skip-rust-build)
  fi
  if [[ "$DEBUG_BACKEND_SWITCHING" -eq 1 ]]; then
    args+=(--debug-backend-switching)
  fi
  if [[ "$CAPSULE_SMOKE" -eq 1 ]]; then
    args+=(--capsule-smoke)
  fi
  "$ROOT_DIR/scripts/macos/build-dev-app.sh" "${args[@]}"
}

if [[ "$BUILD_RUNTIME" -eq 1 ]]; then build_runtime; fi
if [[ "$BUILD_FRONTEND" -eq 1 ]]; then build_frontend; fi
if [[ "$BUILD_RUST" -eq 1 ]]; then build_rust; fi
if [[ "$BUILD_FUNASR" -eq 1 ]]; then build_funasr; fi
if [[ "$BUILD_FFMPEG" -eq 1 ]]; then build_ffmpeg; fi
if [[ "$BUILD_APP" -eq 1 ]]; then
  if [[ "$BUILD_FRONTEND" -eq 0 ]]; then build_frontend; fi
  package_app
fi

printf '%s\n' '==> Build completed'
