#!/usr/bin/env bash
# Shared constants + helper for the pinned standalone Python archive.
# Sourced by download-wheels-macos-arm64.sh and build-macos-arm64.sh so the
# pinned URL / SHA-256 / archive name live in exactly one place.
#
# This file only fetches and verifies the archive. Extraction and quarantine
# handling stay in the callers, because they extract to different destinations
# (download-wheels: persistent cache/python-bootstrap; build-macos: a temp dir).

PYTHON_URL="https://github.com/astral-sh/python-build-standalone/releases/download/20260814/cpython-3.11.16%2B20260814-aarch64-apple-darwin-install_only.tar.gz"
PYTHON_SHA256="fcba9f3f676c83e07225e38116649f0c6eb94cb4fcc166632cf92769462b6e39"
PYTHON_ARCHIVE="cpython-3.11.16+20260814-aarch64-apple-darwin-install_only.tar.gz"

# ensure_python_archive <cache_dir>
# Download the pinned standalone Python archive into <cache_dir> if missing, then
# verify its SHA-256. Aborts on mismatch. Idempotent: skips download if present.
ensure_python_archive() {
  local cache_dir="$1"
  local archive_path="$cache_dir/$PYTHON_ARCHIVE"
  if [[ ! -f "$archive_path" ]]; then
    curl --fail --location --retry 3 --output "$archive_path.part" "$PYTHON_URL"
    mv "$archive_path.part" "$archive_path"
  fi
  local actual_sha256
  actual_sha256="$(shasum -a 256 "$archive_path" | awk '{print $1}')"
  [[ "$actual_sha256" == "$PYTHON_SHA256" ]] || {
    printf 'Python archive SHA-256 mismatch: expected %s, got %s\n' \
      "$PYTHON_SHA256" "$actual_sha256" >&2
    return 1
  }
}
