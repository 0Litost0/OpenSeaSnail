#!/bin/bash
# Compatibility entry; the build implementation has one source.
set -eu
ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
exec /bin/bash "$ROOT_DIR/scripts/dev_package/phases/build-ffmpeg-macos-arm64.sh" "$@"
