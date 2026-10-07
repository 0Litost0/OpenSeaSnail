#!/usr/bin/env bash
# 显式准备一个已锁定的 ASR 变体和共享 FSMN-VAD；绝不下载其他 ASR 变体。
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
LOCK_FILE="$ROOT_DIR/scripts/sensevoice/artifact-lock.json"
VERIFY="$ROOT_DIR/scripts/sensevoice/verify-artifact.sh"
CACHE_DIR="$ROOT_DIR/third_party/sensevoice/macos-arm64/cache/models"
VARIANT="q8"
OFFLINE=0
DRY_RUN=0

usage() {
  cat <<'EOF'
Usage: scripts/sensevoice/prepare-models-macos-arm64.sh \
  [--variant q8|f16|f32] [--cache-dir DIR] [--offline] [--dry-run]

Prepares exactly one locked SenseVoice ASR variant plus fsmn-vad.gguf in a
gitignored local cache. Existing valid files are reused; invalid files are
never overwritten. --offline validates cache only; --dry-run prints the two
selected artifacts without downloading them.
EOF
}

die() { printf '%s\n' "error: $*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --variant) [[ $# -ge 2 ]] || die "--variant requires q8, f16, or f32"; VARIANT="$2"; shift 2 ;;
    --cache-dir) [[ $# -ge 2 ]] || die "--cache-dir requires a directory"; CACHE_DIR="$2"; shift 2 ;;
    --offline) OFFLINE=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done

[[ "$VARIANT" == q8 || "$VARIANT" == f16 || "$VARIANT" == f32 ]] || die "unsupported variant: $VARIANT"
[[ -f "$LOCK_FILE" && -x "$VERIFY" ]] || die "missing GGUF artifact lock or verifier"
command -v jq >/dev/null 2>&1 || die "jq is required to read $LOCK_FILE"
[[ "$OFFLINE" -eq 1 || "$DRY_RUN" -eq 1 ]] || command -v curl >/dev/null 2>&1 || die "curl is required to download artifacts"

artifact_field() { jq -er --arg id "$1" --arg field "$2" '.artifacts[] | select(.id == $id) | .[$field]' "$LOCK_FILE"; }
prepare_one() {
  local id="$1" destination="$2" url temporary
  url="$(artifact_field "$id" url)"
  if [[ "$DRY_RUN" -eq 1 ]]; then
    printf '%s\t%s\n' "$id" "$url"
    return
  fi
  [[ ! -L "$destination" ]] || die "$id destination must not be a symlink: $destination"
  if [[ -f "$destination" ]]; then
    "$VERIFY" "$id" "$destination" >/dev/null
    printf '%s\n' "==> Reusing verified $id: $destination"
    return
  fi
  [[ "$OFFLINE" -eq 0 ]] || die "$id is missing and --offline forbids download: $destination"
  mkdir -p "$(dirname "$destination")"
  temporary="$(mktemp "$(dirname "$destination")/.${id}.XXXXXX")"
  printf '%s\n' "==> Downloading locked $id artifact"
  if ! curl --fail --location --silent --show-error --output "$temporary" "$url"; then
    rm -f "$temporary"
    die "failed to download $id"
  fi
  if ! "$VERIFY" "$id" "$temporary" >/dev/null; then
    rm -f "$temporary"
    die "downloaded $id artifact failed verification"
  fi
  # BSD `ln -h` does not follow a destination symlink to a directory. It only
  # succeeds when the final destination path itself does not yet exist, so this
  # atomically publishes the verified file without overwriting a concurrent
  # producer or escaping through a symlink introduced after the check above.
  if ln -h "$temporary" "$destination" 2>/dev/null; then
    rm -f "$temporary"
    return
  fi
  rm -f "$temporary"
  [[ ! -L "$destination" ]] || die "$id destination became a symlink: $destination"
  [[ -f "$destination" ]] || die "$id destination appeared but is not a regular file: $destination"
  "$VERIFY" "$id" "$destination" >/dev/null
  printf '%s\n' "==> Reusing concurrently prepared $id: $destination"
}

prepare_one "$VARIANT" "$CACHE_DIR/$VARIANT/sensevoice.gguf"
prepare_one "fsmn-vad" "$CACHE_DIR/fsmn-vad.gguf"

if [[ "$DRY_RUN" -eq 0 ]]; then
  printf 'Prepared %s model cache: %s\n' "$VARIANT" "$CACHE_DIR"
fi
