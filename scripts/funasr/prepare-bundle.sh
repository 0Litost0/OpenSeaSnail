#!/usr/bin/env bash
# Dev convenience: prepare the FunASR runtime bundle from zero in one command.
#
# Runs the three preparation steps in order, each idempotent and resumable:
#   1. download-wheels-macos-arm64.sh   fetch Python archive + wheels, and
#                                       prepare a bootstrap interpreter with
#                                       modelscope installed.
#   2. download-models-macos-arm64.py   download the four default offline models.
#   3. build-macos-arm64.sh             assemble the offline bundle from cache.
#
# Remaining args are forwarded to build-macos-arm64.sh (e.g. --output DIR,
# --bootstrap-lock). For a release, run the steps individually and review the
# upstream model licenses; this script is for local development only.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT_DIR="$ROOT_DIR/scripts/funasr"
CACHE="$ROOT_DIR/third_party/funasr/macos-arm64/cache"
BOOT_PY="$CACHE/python-bootstrap/python/bin/python3"

printf '%s\n' '==> [1/3] download-wheels (Python archive + wheels + bootstrap interpreter)'
"$SCRIPT_DIR/download-wheels-macos-arm64.sh"

printf '%s\n' '==> [2/3] download-models (four default offline models to cache)'
"$BOOT_PY" "$SCRIPT_DIR/download-models-macos-arm64.py"

printf '%s\n' '==> [3/3] build-macos (assemble offline bundle)'
# Pass-through args forward to build-macos-arm64.sh (e.g. --slim produces a lean
# bundle: asr+vad only, punc/spk on-demand; --output, --bootstrap-lock likewise).
"$SCRIPT_DIR/build-macos-arm64.sh" "$@"
