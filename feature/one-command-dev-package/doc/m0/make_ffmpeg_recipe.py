#!/usr/bin/env python3
"""Generate the isolated FFmpeg experiment recipe; never edit production scripts.

Run with an unused output path inside dist/dev-package/m0-20261011.
Build it with absolute --cache-dir/--output, MACOSX_DEPLOYMENT_TARGET=13.0,
GIT_CEILING_DIRECTORIES=<checkout root>, and an owned TMPDIR for disk sampling.
"""
from pathlib import Path
import shlex
import sys

ROOT = Path(__file__).resolve().parents[4]
target = Path(sys.argv[1]).resolve()
if not target.is_relative_to(ROOT / "dist/dev-package/m0-20261011") or target.exists():
    raise SystemExit("use an unused output path inside this M0 experiment directory")
source = (ROOT / "scripts/ffmpeg/build-macos-arm64.sh").read_text()
replacements = {
    'ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"':
        "ROOT_DIR=" + shlex.quote(str(ROOT)),
    '--host-cflags="-isysroot $(xcrun --show-sdk-path)"':
        '--host-cflags="-isysroot $(xcrun --show-sdk-path) -mmacosx-version-min=13.0"',
    '--host-ldflags="-isysroot $(xcrun --show-sdk-path)"':
        '--host-ldflags="-isysroot $(xcrun --show-sdk-path) -mmacosx-version-min=13.0" \\\n  --extra-cflags="-mmacosx-version-min=13.0" \\\n  --extra-ldflags="-mmacosx-version-min=13.0"',
    'cp "$ROOT_DIR/scripts/ffmpeg/build-macos-arm64.sh"': 'cp "${BASH_SOURCE[0]}"',
}
for old, new in replacements.items():
    if source.count(old) != 1:
        raise SystemExit("production recipe changed; review experiment replacements")
    source = source.replace(old, new)
target.write_text(source)
print(target)
