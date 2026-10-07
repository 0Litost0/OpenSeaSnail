#!/bin/bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
PACKAGE_DIR="$ROOT_DIR/apps/desktop/native/post-paste-monitor"
OUTPUT="${1:-$ROOT_DIR/target/release/seasnail-post-paste-monitor}"
PROTO="$ROOT_DIR/proto/seasnail/native/v1/post_paste_monitor.proto"
GENERATED="$PACKAGE_DIR/Sources/seasnail-post-paste-monitor/PostPasteMonitor.pb.swift"
CACHE_DIR="$ROOT_DIR/target/swift-cache"

# 用 BSD grep（macOS 自带）而非 rg：出包链路不依赖可能未安装的 ripgrep。
grep -qF 'oneof payload' "$PROTO" || {
  echo 'post_paste_monitor.proto is missing the expected oneof payload' >&2
  exit 1
}
PROTO_SHA="$(shasum -a 256 "$PROTO" | awk '{print $1}')"
grep -qF "proto-sha256: $PROTO_SHA" "$GENERATED" || {
  echo 'Swift protobuf source is stale; regenerate it from post_paste_monitor.proto' >&2
  exit 1
}
mkdir -p "$CACHE_DIR/clang" "$CACHE_DIR/swiftpm"
CLANG_MODULE_CACHE_PATH="$CACHE_DIR/clang" \
SWIFTPM_MODULECACHE_OVERRIDE="$CACHE_DIR/swiftpm" \
swift build --package-path "$PACKAGE_DIR" -c release
mkdir -p "$(dirname "$OUTPUT")"
cp "$PACKAGE_DIR/.build/release/seasnail-post-paste-monitor" "$OUTPUT"
chmod 755 "$OUTPUT"
"$OUTPUT" --self-test
if [[ "${SEASNAIL_SKIP_POST_PASTE_PROTOCOL_SMOKE:-0}" != 1 ]]; then
  cargo run --quiet --manifest-path "$ROOT_DIR/Cargo.toml" \
    -p seasnail-proto --example post_paste_monitor_smoke -- "$OUTPUT"
fi
echo "Built $OUTPUT"
