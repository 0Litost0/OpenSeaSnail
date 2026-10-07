#!/usr/bin/env bash
# 显式预取 ST-M1.1 锁定的 SenseVoice 与 llama.cpp 源码到本地缓存。
# 构建脚本绝不调用本脚本；开发者可先准备缓存，再进行完全离线构建。
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
LOCK_FILE="$ROOT_DIR/scripts/sensevoice/source-lock.json"
CACHE_DIR="$ROOT_DIR/third_party/sensevoice/macos-arm64/cache/sources"
OFFLINE=0

usage() {
  cat <<'EOF'
Usage: scripts/sensevoice/prepare-sources-macos-arm64.sh [--cache-dir DIR] [--offline]

Explicitly populates the local source cache from the revisions locked in
scripts/sensevoice/source-lock.json. Existing checkouts are never overwritten.
Use --offline to validate an already prepared cache without network access.
EOF
}

die() { printf '%s\n' "error: $*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --cache-dir) [[ $# -ge 2 ]] || die "--cache-dir requires a directory"; CACHE_DIR="$2"; shift 2 ;;
    --offline) OFFLINE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done

[[ -f "$LOCK_FILE" ]] || die "missing source lock: $LOCK_FILE"
command -v jq >/dev/null 2>&1 || die "jq is required to read $LOCK_FILE"
command -v git >/dev/null 2>&1 || die "git is required to prepare source"

SENSEVOICE_REPO="$(jq -er '.sensevoice.repository' "$LOCK_FILE")"
SENSEVOICE_REV="$(jq -er '.sensevoice.revision' "$LOCK_FILE")"
LLAMA_CPP_REPO="$(jq -er '.llama_cpp.repository' "$LOCK_FILE")"
LLAMA_CPP_REV="$(jq -er '.llama_cpp.revision' "$LOCK_FILE")"
SENSEVOICE_DIR="$CACHE_DIR/SenseVoice"
LLAMA_CPP_DIR="$CACHE_DIR/llama.cpp"

verify_checkout() {
  local label="$1" dir="$2" expected="$3"
  [[ -d "$dir" ]] || return 1
  git -C "$dir" rev-parse --is-inside-work-tree >/dev/null 2>&1 || die "$label cache is not a Git checkout: $dir"
  # 网络中断可能仅留下 `git init` 创建的空仓库；这种无 HEAD 状态可安全续传。
  git -C "$dir" rev-parse --verify -q HEAD >/dev/null 2>&1 || return 1
  [[ "$(git -C "$dir" rev-parse HEAD)" == "$expected" ]] || \
    die "$label cache revision mismatch: expected $expected, found $(git -C "$dir" rev-parse HEAD)"
  [[ -z "$(git -C "$dir" status --porcelain)" ]] || die "$label cache is dirty: $dir"
  return 0
}

prepare_checkout() {
  local label="$1" repo="$2" revision="$3" dir="$4"
  if verify_checkout "$label" "$dir" "$revision"; then
    printf '%s\n' "==> Reusing locked $label cache: $dir"
    return
  fi
  if [[ -e "$dir" ]]; then
    git -C "$dir" rev-parse --is-inside-work-tree >/dev/null 2>&1 || \
      die "$label cache exists but is not a Git checkout; inspect or remove it manually: $dir"
    [[ -z "$(git -C "$dir" status --porcelain)" ]] || die "$label incomplete cache is dirty: $dir"
    [[ "$OFFLINE" -eq 0 ]] || die "$label cache is incomplete and --offline forbids download: $dir"
    printf '%s\n' "==> Resuming locked $label source fetch"
  else
    [[ "$OFFLINE" -eq 0 ]] || die "$label cache is missing and --offline forbids download: $dir"
    mkdir -p "$(dirname "$dir")"
    printf '%s\n' "==> Fetching locked $label source"
    # 不使用普通 clone：它会先枚举默认分支的完整历史，既慢又放大中断窗口。
    # 仅初始化空仓库并浅取锁定对象，随后 detached checkout。
    git init -q "$dir"
    git -C "$dir" remote add origin "$repo"
  fi
  git -C "$dir" fetch --depth=1 origin "$revision"
  git -C "$dir" checkout --detach FETCH_HEAD
  verify_checkout "$label" "$dir" "$revision" || die "failed to prepare $label"
}

prepare_checkout "SenseVoice" "$SENSEVOICE_REPO" "$SENSEVOICE_REV" "$SENSEVOICE_DIR"
prepare_checkout "llama.cpp" "$LLAMA_CPP_REPO" "$LLAMA_CPP_REV" "$LLAMA_CPP_DIR"

printf '%s\n' 'Prepared locked local sources:'
printf '  SenseVoice: %s\n' "$SENSEVOICE_DIR"
printf '  llama.cpp: %s\n' "$LLAMA_CPP_DIR"
printf '%s\n' 'Next step: run build-macos-arm64.sh with these two paths; it will remain offline.'
