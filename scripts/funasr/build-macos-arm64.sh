#!/usr/bin/env bash
# Build SeaSnail's self-contained FunASR runtime for macOS Apple Silicon.
#
# The output is a relocatable directory intended for
# SeaSnail.app/Contents/Resources/funasr. It never reads a system Python and
# keeps all downloaded artifacts under a caller-selected cache directory.
#
# Usage:
#   scripts/funasr/build-macos-arm64.sh [--slim]
#
# A release build must use the committed requirements.lock. Bootstrap mode
# (--bootstrap-lock) resolves requirements.in once and emits a candidate lock;
# review and commit that lock before building a distributable artifact.
#
# --slim produces a lean bundle: only asr+vad are copied in (the minimal resident
# set for realtime transcription). punc/spk are omitted — the runtime downloads
# them on demand from ModelScope (see crates/daemon/src/downloader.rs), driven by
# models-manifest.json, which is *always* copied in full regardless of --slim so
# the downloader knows each component's model_id/revision/sha/size. Pass --slim
# to produce a shippable package that is ~1.1 GB lighter (no punc weights) and
# starts with punc/spk not loaded (design decision 5: 模型按需下载).

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT_DIR="$ROOT_DIR/scripts/funasr"
. "$SCRIPT_DIR/lib/python-standalone.sh"
# 默认输出与统一构建入口保持一致；该 bundle 包含带 seasnail-funasr 进程名的启动器。
OUTPUT="$ROOT_DIR/third_party/funasr/macos-arm64/bundle-process-name"
CACHE="$ROOT_DIR/third_party/funasr/macos-arm64/cache"
BOOTSTRAP_LOCK=0
SLIM=0

usage() {
  printf '%s\n' "Usage: $0 [--output DIR] [--cache DIR] [--slim] [--bootstrap-lock]"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --output) OUTPUT="$2"; shift 2 ;;
    --cache) CACHE="$2"; shift 2 ;;
    --bootstrap-lock) BOOTSTRAP_LOCK=1; shift ;;
    --slim) SLIM=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage; exit 2 ;;
  esac
done

[[ "$(uname -s)" == "Darwin" && "$(uname -m)" == "arm64" ]] || {
  printf '%s\n' 'This builder only supports macOS Apple Silicon (Darwin arm64).' >&2
  exit 1
}
if [[ -e "$OUTPUT" ]] && find "$OUTPUT" -mindepth 1 ! -name .gitkeep -print -quit | grep -q .; then
  printf 'Refusing to overwrite non-empty output: %s\n' "$OUTPUT" >&2
  exit 1
fi

LOCK_FILE="$SCRIPT_DIR/requirements.lock"
if [[ "$BOOTSTRAP_LOCK" -eq 0 && ! -f "$LOCK_FILE" ]]; then
  printf '%s\n' 'requirements.lock is missing; run once with --bootstrap-lock, review it, then commit it.' >&2
  exit 1
fi

mkdir -p "$CACHE"
ARCHIVE_PATH="$CACHE/$PYTHON_ARCHIVE"
ensure_python_archive "$CACHE"

WORK_DIR="$(mktemp -d "${TMPDIR:-/private/tmp}/seasnail-funasr.XXXXXX")"
cleanup() { rm -rf "$WORK_DIR"; }
trap cleanup EXIT

tar -xzf "$ARCHIVE_PATH" -C "$WORK_DIR"
PYTHON_HOME="$WORK_DIR/python"
PYTHON="$PYTHON_HOME/bin/python3"
[[ -x "$PYTHON" ]] || { printf '%s\n' 'Standalone Python archive did not contain bin/python3.' >&2; exit 1; }
# The archive hash was checked above. Remove only the propagated quarantine
# attribute from this temporary, verified interpreter before executing it.
if xattr -p com.apple.quarantine "$PYTHON" >/dev/null 2>&1; then
  xattr -dr com.apple.quarantine "$PYTHON_HOME"
fi

mkdir -p "$CACHE/wheels"
if [[ "$BOOTSTRAP_LOCK" -eq 1 ]]; then
  "$PYTHON" -m pip wheel --quiet --disable-pip-version-check --no-index \
    --find-links "$CACHE/wheels" --wheel-dir "$CACHE/wheels" \
    -r "$SCRIPT_DIR/requirements.in"
  "$PYTHON" -m pip install --quiet --disable-pip-version-check --no-index \
    --find-links "$CACHE/wheels" -r "$SCRIPT_DIR/requirements.in"
  CANDIDATE_LOCK="$CACHE/requirements.lock.candidate"
  # `ensurepip` supplies pip from a temporary wheel path. That path must not
  # leak into the committed lock file, and pip itself is a build tool rather
  # than a runtime dependency.
  "$PYTHON" -m pip freeze --all | sed '/^pip\([ =@]\)/d' | LC_ALL=C sort > "$CANDIDATE_LOCK"
  printf '%s\n' "Candidate lock written to $CANDIDATE_LOCK"
  printf '%s\n' 'Review it and copy it to scripts/funasr/requirements.lock before a release build.'
  LOCK_TO_INSTALL="$CANDIDATE_LOCK"
else
  "$PYTHON" -m pip install --quiet --disable-pip-version-check --no-index --find-links "$CACHE/wheels" -r "$LOCK_FILE"
  LOCK_TO_INSTALL="$LOCK_FILE"
fi

mkdir -p "$OUTPUT"
cp -R "$PYTHON_HOME" "$OUTPUT/python"
# macOS 的活动监视器以最终解释器二进制名显示此 sidecar。重命名真实二进制、
# 再保留 python3/python3.11 兼容链接，既显示 SeaSnail 归属，也不破坏包内 shebang。
mv "$OUTPUT/python/bin/python3.11" "$OUTPUT/python/bin/seasnail-funasr"
ln -sf seasnail-funasr "$OUTPUT/python/bin/python3"
ln -sf seasnail-funasr "$OUTPUT/python/bin/python3.11"
MODELS_DIR="$CACHE/models"
[[ -f "$MODELS_DIR/models-manifest.json" ]] || {
  printf '%s\n' "Missing model manifest: $MODELS_DIR/models-manifest.json. Run download-models-macos-arm64.py first." >&2
  exit 1
}
# asr+vad 是最小常驻集，始终必需（sidecar 无法无之启动）。punc/spk 是可选组件：
# --slim 时不在打包期拷入（运行时按需下载，见 downloader.rs）；非 slim 时全量内置。
REQUIRED_MODELS=("asr" "vad")
OPTIONAL_MODELS=("punc" "spk")
if [[ "$SLIM" -eq 1 ]]; then
  BUNDLED_MODELS=("${REQUIRED_MODELS[@]}")
else
  BUNDLED_MODELS=("asr" "vad" "punc" "spk")
fi
for model_dir in "${REQUIRED_MODELS[@]}"; do
  [[ -d "$MODELS_DIR/$model_dir" ]] || {
    printf 'Missing required local model directory: %s\n' "$MODELS_DIR/$model_dir" >&2
    exit 1
  }
done
if [[ "$SLIM" -eq 0 ]]; then
  for model_dir in "${OPTIONAL_MODELS[@]}"; do
    [[ -d "$MODELS_DIR/$model_dir" ]] || {
      printf 'Missing local model directory (non-slim build needs all four): %s\n' "$MODELS_DIR/$model_dir" >&2
      exit 1
    }
  done
fi
if find "$MODELS_DIR" -type f -name '*.incomplete' -print -quit | grep -q .; then
  printf '%s\n' 'FunASR model cache contains an incomplete download; resume model download before building.' >&2
  exit 1
fi
# 拷贝模型：先建目录，逐个拷入 bundled 组件；manifest 文件全量拷入（slim 时仍含
# punc/spk 条目——downloader 据此运行时从 ModelScope 拉取被 strip 的组件）。
mkdir -p "$OUTPUT/models"
cp "$MODELS_DIR/models-manifest.json" "$OUTPUT/models/models-manifest.json"
for model_dir in "${BUNDLED_MODELS[@]}"; do
  cp -R "$MODELS_DIR/$model_dir" "$OUTPUT/models/$model_dir"
done
if [[ "$SLIM" -eq 1 ]]; then
  printf 'Slim bundle: asr+vad bundled; punc/spk omitted (on-demand download via manifest).\n'
fi
mkdir -p "$OUTPUT/licenses"
cp "$SCRIPT_DIR/THIRD_PARTY_NOTICES.md" "$OUTPUT/licenses/THIRD_PARTY_NOTICES.md"
cp "$LOCK_TO_INSTALL" "$OUTPUT/requirements.lock"
cp "$SCRIPT_DIR/sidecar.py" "$OUTPUT/sidecar.py"

# Do not create .pyc files after the checksum manifest is emitted. This is a
# smoke test of the bundled interpreter, independent from system Python.
PYTHONDONTWRITEBYTECODE=1 "$OUTPUT/python/bin/python3" -c 'import importlib.metadata, funasr, torch; print("FunASR", importlib.metadata.version("funasr")); print("Torch", torch.__version__); print("MPS available", torch.backends.mps.is_available())'

(
  cd "$OUTPUT"
  find python models sidecar.py -type f -print0 | LC_ALL=C sort -z | xargs -0 shasum -a 256
) > "$OUTPUT/python-files.sha256"

find "$CACHE/wheels" -maxdepth 1 -type f -name '*.whl' -print0 \
  | LC_ALL=C sort -z | xargs -0 shasum -a 256 > "$OUTPUT/wheel-files.sha256"

# `slim` 标记供出包审计/量化区分精简包（asr+vad only）与全量包。runtime 不读此字段
#（统一走 punc/spk 可选 + 双根 + 缺省降级），纯构建期元数据。
printf '{\n  "bundle_format": 1,\n  "platform": "macos-arm64",\n  "slim": %s,\n  "python": {\n    "version": "3.11.16",\n    "release": "20260814",\n    "archive": "%s",\n    "sha256": "%s"\n  },\n  "requirements_lock": "requirements.lock",\n  "file_hashes": "python-files.sha256",\n  "wheel_file_hashes": "wheel-files.sha256",\n  "model_assets": "models/models-manifest.json"\n}\n' \
  "$([ "$SLIM" -eq 1 ] && echo true || echo false)" "$PYTHON_ARCHIVE" "$PYTHON_SHA256" > "$OUTPUT/bundle-manifest.json"

printf 'Created FunASR runtime bundle: %s\n' "$OUTPUT"
