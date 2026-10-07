#!/usr/bin/env bash
# M5.5: exercise an unsigned GGUF-only candidate App through its public daemon API.
# The script deliberately prints no bootstrap token or transcript content.
set -euo pipefail

usage() {
  echo "Usage: $0 --app /path/to/SeaSnail.app [--sample path/to/sample.wav]" >&2
  exit 2
}

app=""
sample="third_party/sensevoice/macos-arm64/cache/sources/SenseVoice/runtime/llama.cpp/tests/sample.wav"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --app) app="${2:-}"; shift 2 ;;
    --sample) sample="${2:-}"; shift 2 ;;
    *) usage ;;
  esac
done

[[ -n "$app" && -x "$app/Contents/MacOS/seasnail-daemon" && -f "$sample" ]] || usage

data_dir="$(mktemp -d /private/tmp/seasnail-gguf-app-smoke.XXXXXX)"
log_file="$data_dir/daemon.log"
fifo="$data_dir/stdin"
mkfifo "$fifo"
tail -f /dev/null > "$fifo" & feeder_pid=$!
SEASNAIL_DATA_DIR="$data_dir" SEASNAIL_DEV_FILE_KEYCHAIN=1 \
  "$app/Contents/MacOS/seasnail-daemon" < "$fifo" > "$log_file" 2>&1 &
daemon_pid=$!
cleanup() {
  kill "$daemon_pid" "$feeder_pid" 2>/dev/null || true
  wait "$daemon_pid" 2>/dev/null || true
  wait "$feeder_pid" 2>/dev/null || true
  rm -f "$fifo"
}
trap cleanup EXIT

for _ in $(seq 1 60); do
  [[ -s "$data_dir/bootstrap.json" ]] && break
  sleep 1
done
[[ -s "$data_dir/bootstrap.json" ]] || { tail -n 100 "$log_file" >&2; exit 1; }
port="$(jq -r '.port' "$data_dir/bootstrap.json")"
setup="$(curl --fail --silent --show-error -X POST "http://127.0.0.1:$port/api/v1/auth/setup" \
  -H 'Content-Type: application/json' \
  --data '{"username":"m5-smoke","password":"m5-smoke-password"}')"
token="$(printf '%s' "$setup" | jq -r '.secret')"
[[ -n "$token" && "$token" != null ]] || exit 1

model_status=""
for _ in $(seq 1 90); do
  models="$(curl --fail --silent --show-error "http://127.0.0.1:$port/api/v1/models" \
    -H "Authorization: Bearer $token")"
  model_status="$(printf '%s' "$models" | jq -r '.[] | select(.id == "sensevoice-small") | .status')"
  [[ "$model_status" == active ]] && break
  sleep 1
done
[[ "$model_status" == active ]] || { printf '%s' "$models" | jq '.[] | select(.id == "sensevoice-small") | del(.last_error)' >&2; exit 1; }

created="$(curl --fail --silent --show-error -X POST "http://127.0.0.1:$port/api/v1/sessions" \
  -H "Authorization: Bearer $token" \
  -F "audio=@$sample;type=audio/wav" -F source=imported -F language=zh)"
session_id="$(printf '%s' "$created" | jq -r '.id')"
[[ -n "$session_id" && "$session_id" != null ]] || exit 1

session_status=""
for _ in $(seq 1 120); do
  session="$(curl --fail --silent --show-error "http://127.0.0.1:$port/api/v1/sessions/$session_id" \
    -H "Authorization: Bearer $token")"
  session_status="$(printf '%s' "$session" | jq -r '.status')"
  [[ "$session_status" == completed || "$session_status" == failed ]] && break
  sleep 1
done
[[ "$session_status" == completed ]] || { printf '%s' "$session" | jq '{id, status, failure_reason}' >&2; exit 1; }
transcript="$(printf '%s' "$session" | jq -r '.transcript // empty')"
[[ -n "$transcript" ]] || exit 1

printf 'M5.5 GGUF App smoke passed\napp_size_bytes=%s\nmodel_status=%s\nsession_status=%s\ntranscript_chars=%s\nlogs=%s\n' \
  "$(du -sk "$app" | awk '{print $1 * 1024}')" "$model_status" "$session_status" "${#transcript}" "$log_file"
