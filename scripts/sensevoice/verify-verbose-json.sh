#!/usr/bin/env bash
# 校验 sensevoice-server verbose_json 的最小 SeaSnail 契约。
set -euo pipefail

usage() { printf '%s\n' "Usage: $0 --input RESPONSE.json [--min-segments N] [--min-gap-ms N]"; }
die() { printf '%s\n' "error: $*" >&2; exit 1; }

INPUT=""
MIN_SEGMENTS=1
MIN_GAP_MS=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --input) [[ $# -ge 2 ]] || die "--input requires a JSON file"; INPUT="$2"; shift 2 ;;
    --min-segments) [[ $# -ge 2 && "$2" =~ ^[1-9][0-9]*$ ]] || die "--min-segments requires a positive integer"; MIN_SEGMENTS="$2"; shift 2 ;;
    --min-gap-ms) [[ $# -ge 2 && "$2" =~ ^[0-9]+$ ]] || die "--min-gap-ms requires a non-negative integer"; MIN_GAP_MS="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done
[[ -n "$INPUT" && -f "$INPUT" ]] || die "--input must be an existing JSON file"
command -v jq >/dev/null 2>&1 || die "jq is required"

jq -e --argjson min "$MIN_SEGMENTS" --argjson min_gap "$MIN_GAP_MS" '
  . as $root |
  (.text | type == "string" and length > 0) and
  (.duration | type == "number" and . >= 0) and
  (.segments | type == "array" and length >= $min) and
  (all(.segments[];
    (.id | type == "number") and
    (.start | type == "number") and (.end | type == "number") and (.start >= 0 and .end >= .start and .end <= $root.duration) and
    (.start_ms | type == "number") and (.end_ms | type == "number") and (.start_ms >= 0 and .end_ms >= .start_ms and .end_ms <= ($root.duration * 1000)) and
    (((.start * 1000 - .start_ms) | abs) <= 1) and (((.end * 1000 - .end_ms) | abs) <= 1) and
    (.text | type == "string"))) and
  (all(range(1; ($root.segments | length));
    $root.segments[.-1].end <= $root.segments[.].start and
    $root.segments[.-1].end_ms + $min_gap <= $root.segments[.].start_ms))
' "$INPUT" >/dev/null || die "invalid verbose_json segment contract"
printf 'Verified verbose_json contract: %s (at least %s segment(s))\n' "$INPUT" "$MIN_SEGMENTS"
