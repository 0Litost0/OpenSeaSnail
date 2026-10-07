#!/usr/bin/env bash
# 从 ST-M1.1 锁定的本地源码构建 SeaSnail 随包分发的 SenseVoice GGUF server。
#
# 此脚本刻意不 clone、fetch 或下载任何输入：调用方必须先提供干净且已锁定的
# SenseVoice 与 llama.cpp checkout。`FETCHCONTENT_SOURCE_DIR_LLAMA` 与 disconnected
# 配置共同阻止 CMake 在构建时访问网络或漂移到未锁定依赖。
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
LOCK_FILE="$ROOT_DIR/scripts/sensevoice/source-lock.json"
OUTPUT="$ROOT_DIR/third_party/sensevoice/macos-arm64/bundle"
SENSEVOICE_SRC=""
LLAMA_CPP_SRC=""
# 某些受限执行环境禁止 sysctl；此时退回单线程，调用方仍可通过 --jobs 覆盖。
JOBS="$(sysctl -n hw.ncpu 2>/dev/null || printf '1')"
BUILD_DIR=""

usage() {
  cat <<'EOF'
Usage: scripts/sensevoice/build-macos-arm64.sh \
  --sensevoice-src DIR --llama-cpp-src DIR [--output DIR] [--build-dir DIR] [--jobs N]

Builds the pinned arm64 sensevoice-server from local source checkouts only.
Both source directories must be clean Git worktrees at the revisions in
scripts/sensevoice/source-lock.json. The script never clones or downloads.
Use --build-dir to retain CMake intermediates and resume an interrupted build.
EOF
}

die() { printf '%s\n' "error: $*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --sensevoice-src) [[ $# -ge 2 ]] || die "--sensevoice-src requires a directory"; SENSEVOICE_SRC="$2"; shift 2 ;;
    --llama-cpp-src) [[ $# -ge 2 ]] || die "--llama-cpp-src requires a directory"; LLAMA_CPP_SRC="$2"; shift 2 ;;
    --output) [[ $# -ge 2 ]] || die "--output requires a directory"; OUTPUT="$2"; shift 2 ;;
    --build-dir) [[ $# -ge 2 ]] || die "--build-dir requires a directory"; BUILD_DIR="$2"; shift 2 ;;
    --jobs) [[ $# -ge 2 && "$2" =~ ^[1-9][0-9]*$ ]] || die "--jobs requires a positive integer"; JOBS="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done

[[ -f "$LOCK_FILE" ]] || die "missing source lock: $LOCK_FILE"
command -v jq >/dev/null 2>&1 || die "jq is required to read $LOCK_FILE"
command -v cmake >/dev/null 2>&1 || die "CMake 3.16 or newer is required"
command -v xcrun >/dev/null 2>&1 || die "Xcode Command Line Tools are required (install with: xcode-select --install)"
[[ "$(uname -s)" == Darwin && "$(uname -m)" == arm64 ]] || die "only macOS Apple Silicon is supported"
[[ -n "$SENSEVOICE_SRC" ]] || die "--sensevoice-src is required; this script never downloads source"
[[ -n "$LLAMA_CPP_SRC" ]] || die "--llama-cpp-src is required; this script never downloads source"

SENSEVOICE_REV="$(jq -er '.sensevoice.revision' "$LOCK_FILE")"
LLAMA_CPP_REV="$(jq -er '.llama_cpp.revision' "$LOCK_FILE")"
LLAMA_OVERRIDE="$(jq -er '.llama_cpp.cmake_source_override' "$LOCK_FILE")"
MIN_CMAKE="$(jq -er '.build.cmake_minimum_version' "$LOCK_FILE")"
TARGET="$(jq -er '.build.target' "$LOCK_FILE")"

version_at_least() {
  local have="$1" need="$2" have_major have_minor have_patch need_major need_minor need_patch
  IFS=. read -r have_major have_minor have_patch <<<"$have"
  IFS=. read -r need_major need_minor need_patch <<<"$need"
  have_minor="${have_minor:-0}"; have_patch="${have_patch:-0}"
  need_minor="${need_minor:-0}"; need_patch="${need_patch:-0}"
  ((10#$have_major > 10#$need_major)) || \
    ((10#$have_major == 10#$need_major && 10#$have_minor > 10#$need_minor)) || \
    ((10#$have_major == 10#$need_major && 10#$have_minor == 10#$need_minor && 10#$have_patch >= 10#$need_patch))
}

CMK_VERSION="$(cmake --version | awk 'NR == 1 { print $3 }')"
version_at_least "$CMK_VERSION" "$MIN_CMAKE" || die "CMake $MIN_CMAKE or newer is required (found $CMK_VERSION)"

verify_checkout() {
  local label="$1" dir="$2" expected="$3"
  [[ -d "$dir" ]] || die "$label source directory does not exist: $dir"
  git -C "$dir" rev-parse --is-inside-work-tree >/dev/null 2>&1 || die "$label is not a Git checkout: $dir"
  [[ "$(git -C "$dir" rev-parse HEAD)" == "$expected" ]] || \
    die "$label revision mismatch: expected $expected, found $(git -C "$dir" rev-parse HEAD)"
  [[ -z "$(git -C "$dir" status --porcelain)" ]] || die "$label checkout is dirty: $dir"
}

verify_checkout "SenseVoice" "$SENSEVOICE_SRC" "$SENSEVOICE_REV"
verify_checkout "llama.cpp" "$LLAMA_CPP_SRC" "$LLAMA_CPP_REV"

SOURCE_CMAKE="$SENSEVOICE_SRC/runtime/llama.cpp/CMakeLists.txt"
[[ -f "$SOURCE_CMAKE" ]] || die "SenseVoice checkout lacks runtime CMake file: $SOURCE_CMAKE"
[[ -f "$SENSEVOICE_SRC/runtime/llama.cpp/sensevoice-server/sensevoice-server.cpp" ]] || \
  die "SenseVoice checkout lacks sensevoice-server source"
rg -q "GIT_TAG[[:space:]]+$LLAMA_CPP_REV" "$SOURCE_CMAKE" || \
  die "SenseVoice source no longer declares the locked llama.cpp revision"

[[ ! -L "$OUTPUT" ]] || die "refusing symlink output directory: $OUTPUT"
[[ ! -e "$OUTPUT" || -z "$(find "$OUTPUT" -mindepth 1 -maxdepth 1 ! -name .gitkeep -print -quit)" ]] || \
  die "refusing to overwrite non-empty output: $OUTPUT"

OUTPUT_PARENT="$(dirname "$OUTPUT")"
mkdir -p "$OUTPUT_PARENT"
STAGE_PARENT="$(mktemp -d "$OUTPUT_PARENT/.sensevoice-server.XXXXXX")"
if [[ -n "$BUILD_DIR" ]]; then
  mkdir -p "$BUILD_DIR"
  KEEP_BUILD_DIR=1
else
  BUILD_DIR="$(mktemp -d "${TMPDIR:-/tmp}/seasnail-sensevoice-build.XXXXXX")"
  KEEP_BUILD_DIR=0
fi
STAGED_OUTPUT="$STAGE_PARENT/bundle"
cleanup() {
  rm -rf "$STAGE_PARENT"
  [[ "$KEEP_BUILD_DIR" -eq 1 ]] || rm -rf "$BUILD_DIR"
}
trap cleanup EXIT

printf '%s\n' "==> Configuring $TARGET from locked local sources"
cmake -S "$SENSEVOICE_SRC/runtime/llama.cpp" -B "$BUILD_DIR" \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_OSX_ARCHITECTURES=arm64 \
  -D"$LLAMA_OVERRIDE=$LLAMA_CPP_SRC" \
  -DFETCHCONTENT_FULLY_DISCONNECTED=ON \
  -DFETCHCONTENT_UPDATES_DISCONNECTED=ON

printf '%s\n' "==> Building $TARGET"
cmake --build "$BUILD_DIR" --target "$TARGET" --parallel "$JOBS"

SERVER="$BUILD_DIR/bin/$TARGET"
[[ -x "$SERVER" ]] || die "build did not produce executable server: $SERVER"
[[ "$(file -b "$SERVER")" == *"arm64"* ]] || die "built server is not arm64: $(file -b "$SERVER")"

# Candidate bundles may only depend on macOS system libraries at this stage. A
# non-system dylib requires an explicit packaging decision, not an accidental
# developer-machine dependency. Use a whitelist so @rpath and relative paths
# cannot silently pass this check.
if ! otool -L "$SERVER" | tail -n +2 | awk '
  $1 !~ /^\/System\/Library\// && $1 !~ /^\/usr\/lib\// { print $1; invalid = 1 }
  END { exit invalid }
'; then
  die "built server unexpectedly depends on a non-system dynamic library"
fi

mkdir -p "$STAGED_OUTPUT"
cp "$SERVER" "$STAGED_OUTPUT/sensevoice-server"
chmod +x "$STAGED_OUTPUT/sensevoice-server"
cp "$LOCK_FILE" "$STAGED_OUTPUT/source-lock.json"
{
  printf '%s\n' 'SeaSnail bundled SenseVoice GGUF server'
  printf 'SenseVoice revision: %s\n' "$SENSEVOICE_REV"
  printf 'llama.cpp revision: %s\n' "$LLAMA_CPP_REV"
  printf 'CMake: %s\n' "$CMK_VERSION"
  printf 'Architecture: arm64\n'
  printf 'Source acquisition: pre-fetched local checkouts; FetchContent disconnected\n'
} > "$STAGED_OUTPUT/BUILD-INFO.txt"

"$STAGED_OUTPUT/sensevoice-server" --help >/dev/null
 # Publish the complete bundle in one same-filesystem rename. The repository
 # placeholder is moved into the staged directory so a default empty output
 # directory can be replaced without deleting tracked state.
 if [[ -e "$OUTPUT" ]]; then
   [[ -f "$OUTPUT/.gitkeep" ]] && mv "$OUTPUT/.gitkeep" "$STAGED_OUTPUT/.gitkeep"
   rmdir "$OUTPUT" || die "output directory changed during build: $OUTPUT"
 fi
 mv "$STAGED_OUTPUT" "$OUTPUT"
printf 'Created locked SenseVoice server bundle: %s\n' "$OUTPUT/sensevoice-server"
