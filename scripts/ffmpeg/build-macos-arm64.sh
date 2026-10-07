#!/usr/bin/env bash
# 构建 SeaSnail 随 app 分发的 macOS Apple Silicon ffmpeg。
#
# 仅启用本地音频归一化需要的内建解码/重采样/WAV 输出，不启用任何外部库或 GPL 组件。
# 产物静态链接 FFmpeg 自身库；仅依赖 macOS 系统 dylib，运行时不需要 Homebrew。
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VERSION="9.0.1"
TAG="n${VERSION}"
ARCHIVE="FFmpeg-${TAG}.tar.gz"
URL="https://codeload.github.com/FFmpeg/FFmpeg/tar.gz/refs/tags/${TAG}"
SHA256="195d54bebe1a27f84d77f4b989d193466f305b355da92292766a69f16880b18a"
CACHE_DIR="$ROOT_DIR/third_party/ffmpeg/macos-arm64/cache"
OUTPUT="$ROOT_DIR/third_party/ffmpeg/macos-arm64/bundle"

usage() {
  cat <<'EOF'
Usage: scripts/ffmpeg/build-macos-arm64.sh [--output DIR] [--cache-dir DIR]

Downloads the pinned FFmpeg GitHub tag once, verifies SHA-256, then builds
an LGPL-only, dependency-free arm64 ffmpeg for SeaSnail's audio normalizer.
EOF
}

die() { printf '%s\n' "error: $*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --output) [[ $# -ge 2 ]] || die "--output requires a directory"; OUTPUT="$2"; shift 2 ;;
    --cache-dir) [[ $# -ge 2 ]] || die "--cache-dir requires a directory"; CACHE_DIR="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done

[[ "$(uname -s)" == Darwin && "$(uname -m)" == arm64 ]] || die "only macOS Apple Silicon is supported"
command -v xcrun >/dev/null 2>&1 || die "Xcode Command Line Tools are required (install with: xcode-select --install)"
[[ ! -e "$OUTPUT" || -z "$(find "$OUTPUT" -mindepth 1 -maxdepth 1 ! -name .gitkeep -print -quit)" ]] || die "refusing to overwrite non-empty output: $OUTPUT"

mkdir -p "$CACHE_DIR"
ARCHIVE_PATH="$CACHE_DIR/$ARCHIVE"
if [[ ! -f "$ARCHIVE_PATH" ]]; then
  printf '%s\n' "==> Downloading FFmpeg ${VERSION} source"
  curl --fail --silent --show-error --location --output "$ARCHIVE_PATH.partial" "$URL"
  mv "$ARCHIVE_PATH.partial" "$ARCHIVE_PATH"
fi
ACTUAL_SHA="$(shasum -a 256 "$ARCHIVE_PATH" | awk '{print $1}')"
[[ "$ACTUAL_SHA" == "$SHA256" ]] || die "SHA-256 mismatch for $ARCHIVE_PATH (got $ACTUAL_SHA)"

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/seasnail-ffmpeg.XXXXXX")"
cleanup() { rm -rf "$WORK_DIR"; }
trap cleanup EXIT
tar -xzf "$ARCHIVE_PATH" -C "$WORK_DIR"
SOURCE_DIR="$WORK_DIR/FFmpeg-$TAG"

printf '%s\n' "==> Configuring LGPL-only static ffmpeg ${VERSION}"
cd "$SOURCE_DIR"
./configure \
  --arch=arm64 \
  --cc="$(xcrun --find clang)" \
  --sysroot="$(xcrun --show-sdk-path)" \
  --host-cc="$(xcrun --find clang)" \
  --host-cflags="-isysroot $(xcrun --show-sdk-path)" \
  --host-ldflags="-isysroot $(xcrun --show-sdk-path)" \
  --disable-shared \
  --enable-static \
  --disable-autodetect \
  --disable-debug \
  --disable-doc \
  --disable-network \
  --disable-programs \
  --enable-ffmpeg \
  --disable-everything \
  --enable-avcodec \
  --enable-avformat \
  --enable-avutil \
  --enable-swresample \
  --enable-protocol=file \
  --enable-demuxer=wav,mp3,mov,flac,ogg,matroska \
  --enable-muxer=wav \
  --enable-decoder=aac,flac,mp3,opus,vorbis,pcm_s16le,pcm_s16be,pcm_s24le,pcm_s24be,pcm_s32le,pcm_s32be,pcm_f32le,pcm_f32be,pcm_f64le,pcm_f64be \
  --enable-encoder=pcm_s16le \
  --enable-parser=aac,mpegaudio,opus,vorbis,flac \
  --enable-filter=aresample
make -j"$(sysctl -n hw.ncpu)" ffmpeg

mkdir -p "$OUTPUT/licenses"
cp ffmpeg "$OUTPUT/ffmpeg"
chmod +x "$OUTPUT/ffmpeg"
cp COPYING.LGPLv2.1 "$OUTPUT/licenses/FFMPEG-LGPL-2.1-or-later.txt"
mkdir -p "$OUTPUT/corresponding-source"
cp "$ARCHIVE_PATH" "$OUTPUT/corresponding-source/$ARCHIVE"
cp "$ROOT_DIR/scripts/ffmpeg/build-macos-arm64.sh" "$OUTPUT/corresponding-source/build-macos-arm64.sh"
"$OUTPUT/ffmpeg" -buildconf > "$OUTPUT/corresponding-source/BUILD-CONFIGURATION.txt" 2>&1
cat > "$OUTPUT/corresponding-source/README.txt" <<EOF
FFmpeg ${VERSION} corresponding source for the bundled command-line executable.
Copyright belongs to the FFmpeg contributors; see COPYING.LGPLv2.1 in the archive.

Archive: $ARCHIVE
Source SHA-256: $SHA256
Upstream source: $URL
Source modifications: none.

Extract the archive. In its source root, run ./configure using the flags recorded
in BUILD-CONFIGURATION.txt, then run make -jN ffmpeg. Xcode Command Line Tools
and a matching macOS SDK are required. The complete SeaSnail build script is
included for reference; it normally runs from scripts/ffmpeg in the source repo.
FFmpeg's libraries are linked into its standalone executable; SeaSnail invokes
that executable as a separate process. No external libraries, GPL or nonfree
components are enabled by this build.
EOF
python3 - "$OUTPUT" "$ARCHIVE" "$SHA256" <<'PY'
import hashlib
import json
from pathlib import Path
import sys
root = Path(sys.argv[1])
files = []
for path in sorted((root / "corresponding-source").iterdir()):
    files.append({"path": path.relative_to(root).as_posix(),
                  "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                  "size_bytes": path.stat().st_size})
(root / "source-manifest.json").write_text(json.dumps({
    "schema_version": 1, "binary_sha256": hashlib.sha256((root / "ffmpeg").read_bytes()).hexdigest(),
    "archive": sys.argv[2], "archive_sha256": sys.argv[3], "modifications": "none", "files": files,
}, indent=2) + "\n")
PY
cat > "$OUTPUT/BUILD-INFO.txt" <<EOF
SeaSnail bundled ffmpeg
Version: $VERSION
Source: $URL
Source SHA-256: $SHA256
License: LGPL-2.1-or-later
Configuration: static, no external libraries, audio normalization only
EOF
"$OUTPUT/ffmpeg" -version | head -n 2
if otool -L "$OUTPUT/ffmpeg" | tail -n +2 | awk '{print $1}' | grep -Eq '^/(opt|usr/local|Users)/'; then
  die "built ffmpeg unexpectedly depends on a non-system dynamic library"
fi
"$OUTPUT/ffmpeg" -L 2>&1 | grep -q 'Lesser General Public' || die "built ffmpeg is not LGPL licensed"
printf 'Created bundled ffmpeg: %s\n' "$OUTPUT/ffmpeg"
