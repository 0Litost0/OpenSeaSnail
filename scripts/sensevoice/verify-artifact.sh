#!/usr/bin/env bash
# 校验 artifact-lock.json 中任一模型/VAD 工件的精确大小与 SHA-256。
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
LOCK_FILE="$ROOT_DIR/scripts/sensevoice/artifact-lock.json"

usage() { printf '%s\n' "Usage: $0 <q8|f16|f32|fsmn-vad> <file>"; }
die() { printf '%s\n' "error: $*" >&2; exit 1; }

[[ $# -eq 2 ]] || { usage >&2; exit 2; }
command -v jq >/dev/null 2>&1 || die "jq is required to read $LOCK_FILE"
command -v shasum >/dev/null 2>&1 || die "shasum is required"
[[ -f "$LOCK_FILE" ]] || die "missing artifact lock: $LOCK_FILE"

ID="$1"
FILE="$2"
[[ -f "$FILE" ]] || die "artifact file does not exist: $FILE"
ENTRY="$(jq -cer --arg id "$ID" '.artifacts[] | select(.id == $id)' "$LOCK_FILE")" || \
  die "unknown locked artifact: $ID"
EXPECTED_SIZE="$(jq -er '.size_bytes' <<<"$ENTRY")"
EXPECTED_SHA="$(jq -er '.sha256' <<<"$ENTRY")"
ACTUAL_SIZE="$(stat -f '%z' "$FILE")"
[[ "$ACTUAL_SIZE" == "$EXPECTED_SIZE" ]] || \
  die "size mismatch for $ID: expected $EXPECTED_SIZE bytes, got $ACTUAL_SIZE"
ACTUAL_SHA="$(shasum -a 256 "$FILE" | awk '{print $1}')"
[[ "$ACTUAL_SHA" == "$EXPECTED_SHA" ]] || \
  die "SHA-256 mismatch for $ID: expected $EXPECTED_SHA, got $ACTUAL_SHA"
printf 'Verified %s: %s (%s bytes, SHA-256 %s)\n' "$ID" "$FILE" "$ACTUAL_SIZE" "$ACTUAL_SHA"
