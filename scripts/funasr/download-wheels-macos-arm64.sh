#!/usr/bin/env bash
# Download all binary Python wheels required by SeaSnail's macOS arm64 FunASR
# bundle, and install them into a persistent bootstrap interpreter so it can
# run scripts that depend on modelscope/funasr (notably download-models).
# Run this from any directory; files are kept inside the repository.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT_DIR="$ROOT_DIR/scripts/funasr"
. "$SCRIPT_DIR/lib/python-standalone.sh"

CACHE_DIR="$ROOT_DIR/third_party/funasr/macos-arm64/cache"
ARCHIVE_PATH="$CACHE_DIR/$PYTHON_ARCHIVE"
BOOTSTRAP_DIR="$CACHE_DIR/python-bootstrap"
WHEEL_DIR="$CACHE_DIR/wheels"
REQUIREMENTS="$ROOT_DIR/scripts/funasr/requirements.in"

[[ "$(uname -s)" == "Darwin" && "$(uname -m)" == "arm64" ]] || {
  printf '%s\n' 'This downloader only supports macOS Apple Silicon (Darwin arm64).' >&2
  exit 1
}
# Fetch the pinned standalone Python archive into cache/ if missing and verify it.
ensure_python_archive "$CACHE_DIR"

if [[ ! -x "$BOOTSTRAP_DIR/python/bin/python3" ]]; then
  mkdir -p "$BOOTSTRAP_DIR"
  tar -xzf "$ARCHIVE_PATH" -C "$BOOTSTRAP_DIR"
fi

PYTHON="$BOOTSTRAP_DIR/python/bin/python3"
# GitHub-downloaded archives propagate Gatekeeper's quarantine attribute to the
# extracted interpreter. The archive SHA-256 has already been verified above, so
# clear that attribute only from this isolated, repository-local runtime.
if xattr -p com.apple.quarantine "$PYTHON" >/dev/null 2>&1; then
  xattr -dr com.apple.quarantine "$BOOTSTRAP_DIR/python"
fi
mkdir -p "$WHEEL_DIR"
"$PYTHON" -m ensurepip --upgrade
"$PYTHON" -m pip wheel --quiet --disable-pip-version-check \
  --wheel-dir "$WHEEL_DIR" -r "$REQUIREMENTS"

# Install the wheels into the bootstrap interpreter so it can run scripts that
# depend on modelscope/funasr (notably download-models-macos-arm64.py). Mirrors
# build-macos-arm64.sh's own install step; --no-index keeps it offline.
"$PYTHON" -m pip install --quiet --disable-pip-version-check --no-index \
  --find-links "$WHEEL_DIR" -r "$REQUIREMENTS"

wheel_count="$(find "$WHEEL_DIR" -maxdepth 1 -name '*.whl' -type f | wc -l | tr -d ' ')"
printf 'Downloaded %s wheels to %s\n' "$wheel_count" "$WHEEL_DIR"
printf 'Bootstrap interpreter ready: %s\n' "$PYTHON"
printf 'Download models with: %s scripts/funasr/download-models-macos-arm64.py\n' "$PYTHON"
