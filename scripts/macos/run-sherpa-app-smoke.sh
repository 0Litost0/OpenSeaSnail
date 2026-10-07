#!/usr/bin/env bash
# M5.6: exercise an unsigned Sherpa-only candidate App through its daemon API.
set -euo pipefail
trap 'echo "smoke failed at line $LINENO: $BASH_COMMAND" >&2' ERR

usage() {
  echo "Usage: $0 --app /path/to/SeaSnail.app --sample /path/to/sample.wav [--silence /path/to/silence.wav] [--fault /path/to/invalid-audio] [--require-m6-cases]" >&2
  exit 2
}

app=""
sample=""
silence=""
fault=""
require_m6_cases=0
silent_reason="skipped"
fault_reason="skipped"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --app) app="${2:-}"; shift 2 ;;
    --sample) sample="${2:-}"; shift 2 ;;
    --silence) silence="${2:-}"; shift 2 ;;
    --fault) fault="${2:-}"; shift 2 ;;
    --require-m6-cases) require_m6_cases=1; shift ;;
    *) usage ;;
  esac
done

[[ -n "$app" && -x "$app/Contents/MacOS/seasnail-daemon" && -f "$sample" ]] || usage
[[ -z "$silence" || -f "$silence" ]] || usage
[[ -z "$fault" || -f "$fault" ]] || usage
[[ "$require_m6_cases" -eq 0 || ( -n "$silence" && -n "$fault" ) ]] || usage
command -v jq >/dev/null || { echo "jq is required" >&2; exit 1; }
command -v ps >/dev/null || { echo "ps is required" >&2; exit 1; }
app="$(cd "$app" && pwd)"
daemon_path="$app/Contents/MacOS/seasnail-daemon"
sidecar_path="$app/Contents/Resources/asr/sensevoice-small/sherpa_onnx/int8/bin/seasnail-sherpa-sidecar"

direct_child_process_ids() {
  local parent_pid="$1"
  local expected="$2"
  ps -axo pid=,ppid=,command= | awk -v parent_pid="$parent_pid" -v expected="$expected" \
    '$2 == parent_pid && $3 == expected { print $1 }'
}

full_text_or_fail() {
  jq -er '.transcript.full_text
    | select(type == "string" and (gsub("[[:space:]]"; "") | length > 0))'
}

is_live_process() {
  local pid="$1"
  local expected="$2"
  [[ -n "$pid" ]] && ps -p "$pid" -o command= | awk -v expected="$expected" \
    '$1 == expected { found = 1 } END { exit !found }'
}

data_dir="$(mktemp -d /private/tmp/seasnail-sherpa-app-smoke.XXXXXX)"
log_file="$data_dir/daemon.log"
fifo="$data_dir/stdin"
mkfifo "$fifo"
tail -f /dev/null > "$fifo" & feeder_pid=$!
SEASNAIL_DATA_DIR="$data_dir" SEASNAIL_DEV_FILE_KEYCHAIN=1 \
  "$app/Contents/MacOS/seasnail-daemon" < "$fifo" > "$log_file" 2>&1 &
daemon_pid=$!
sidecar_pids=""

cleanup() {
  local exit_code=$?
  # Capture only sidecars directly owned by this smoke daemon. Never collect
  # same-path processes from another App instance.
  for pid in $(direct_child_process_ids "$daemon_pid" "$sidecar_path"); do
    case " $sidecar_pids " in *" $pid "*) ;; *) sidecar_pids="$sidecar_pids $pid" ;; esac
  done
  # First exercise the daemon's normal shutdown, which should reap its sidecar.
  kill "$daemon_pid" "$feeder_pid" 2>/dev/null || true
  for _ in $(seq 1 10); do
    local live=0
    is_live_process "$daemon_pid" "$daemon_path" && live=1
    for pid in $sidecar_pids; do is_live_process "$pid" "$sidecar_path" && live=1; done
    [[ "$live" -eq 0 ]] && break
    sleep 1
  done
  local live=0
  is_live_process "$daemon_pid" "$daemon_path" && live=1
  for pid in $sidecar_pids; do is_live_process "$pid" "$sidecar_path" && live=1; done
  if [[ "$live" -ne 0 ]]; then
    # A forced cleanup is failure evidence, not a passing cleanup path.
    for pid in $sidecar_pids; do
      kill -CONT "$pid" 2>/dev/null || true
      kill -KILL "$pid" 2>/dev/null || true
    done
    kill -KILL "$daemon_pid" 2>/dev/null || true
    sleep 1
    live=0
    is_live_process "$daemon_pid" "$daemon_path" && live=1
    for pid in $sidecar_pids; do is_live_process "$pid" "$sidecar_path" && live=1; done
    exit_code=1
  fi
  wait "$daemon_pid" 2>/dev/null || true
  wait "$feeder_pid" 2>/dev/null || true
  if [[ "$live" -ne 0 ]]; then
    echo "smoke cleanup left a daemon or Sherpa sidecar process" >&2
  fi
  rm -rf "$data_dir"
  return "$exit_code"
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
  --data '{"username":"m5-sherpa-smoke","password":"m5-sherpa-smoke-password"}')"
token="$(printf '%s' "$setup" | jq -r '.secret')"
[[ -n "$token" && "$token" != null ]] || exit 1

model_status=""
for _ in $(seq 1 90); do
  models="$(curl --fail --silent --show-error "http://127.0.0.1:$port/api/v1/models" \
    -H "Authorization: Bearer $token")"
  model_status="$(printf '%s' "$models" | jq -r '.[] | select(.id == "sensevoice-small-sherpa-int8") | .status')"
  [[ "$model_status" == active ]] && break
  sleep 1
done
[[ "$model_status" == active ]] || { printf '%s' "$models" | jq '.[] | select(.id == "sensevoice-small-sherpa-int8") | del(.last_error)' >&2; exit 1; }
jq -e 'map(select(.id == "sensevoice-small-sherpa-int8")) as $m
  | ($m | length == 1) and $m[0].status == "active"
    and $m[0].runtime == "sherpa_onnx"' <<<"$models" >/dev/null || {
  echo "active Sherpa catalog entry is incomplete or inconsistent" >&2; exit 1;
}
sidecar_pids="$(direct_child_process_ids "$daemon_pid" "$sidecar_path")"
[[ "$(wc -w <<<"$sidecar_pids" | tr -d ' ')" == 1 ]] || {
  echo "expected exactly one Sherpa sidecar owned by candidate daemon" >&2; exit 1;
}

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
jq -e '.source == "imported" and .model == "sensevoice-small-sherpa-int8"' <<<"$session" >/dev/null || exit 1
transcript="$(printf '%s' "$session" | full_text_or_fail)" || {
  printf '%s' "$session" | jq '{id, status, transcript}' >&2; exit 1;
}

# The fixed WAV is a deterministic stand-in for a short realtime capture.
realtime="$(curl --fail --silent --show-error -X POST "http://127.0.0.1:$port/api/v1/sessions" \
  -H "Authorization: Bearer $token" \
  -F "audio=@$sample;type=audio/wav" -F source=realtime -F language=zh)"
realtime_id="$(printf '%s' "$realtime" | jq -r '.id')"
[[ -n "$realtime_id" && "$realtime_id" != null ]] || exit 1
for _ in $(seq 1 120); do
  realtime_session="$(curl --fail --silent --show-error "http://127.0.0.1:$port/api/v1/sessions/$realtime_id" \
    -H "Authorization: Bearer $token")"
  realtime_status="$(printf '%s' "$realtime_session" | jq -r '.status')"
  [[ "$realtime_status" == completed || "$realtime_status" == failed ]] && break
  sleep 1
done
[[ "$realtime_status" == completed ]] || { printf '%s' "$realtime_session" | jq '{id, status, failure_reason}' >&2; exit 1; }
jq -e '.source == "realtime" and .model == "sensevoice-small-sherpa-int8"' <<<"$realtime_session" >/dev/null || exit 1
realtime_transcript="$(printf '%s' "$realtime_session" | full_text_or_fail)" || exit 1

if [[ -n "$silence" ]]; then
  silent_created="$(curl --fail --silent --show-error -X POST "http://127.0.0.1:$port/api/v1/sessions" \
    -H "Authorization: Bearer $token" \
    -F "audio=@$silence;type=audio/wav" -F source=realtime -F language=zh)"
  silent_id="$(jq -er '.id' <<<"$silent_created")"
  silent_status=""
  for _ in $(seq 1 120); do
    silent_session="$(curl --fail --silent --show-error "http://127.0.0.1:$port/api/v1/sessions/$silent_id" \
      -H "Authorization: Bearer $token")"
    silent_status="$(jq -r '.status' <<<"$silent_session")"
    [[ "$silent_status" == completed || "$silent_status" == failed ]] && break
    sleep 1
  done
  jq -e '.source == "realtime" and .status == "failed" and .failure_reason == "no_speech" and .transcript == null' \
    <<<"$silent_session" >/dev/null || {
    jq '{id, source, status, failure_reason, transcript}' <<<"$silent_session" >&2; exit 1;
  }
  silent_reason="$(jq -r '.failure_reason' <<<"$silent_session")"
fi

if [[ -n "$fault" ]]; then
  fault_created="$(curl --fail --silent --show-error -X POST "http://127.0.0.1:$port/api/v1/sessions" \
    -H "Authorization: Bearer $token" \
    -F "audio=@$fault;type=audio/wav" -F source=realtime -F language=zh)"
  fault_id="$(jq -er '.id' <<<"$fault_created")"
  fault_status=""
  for _ in $(seq 1 120); do
    fault_session="$(curl --fail --silent --show-error "http://127.0.0.1:$port/api/v1/sessions/$fault_id" \
      -H "Authorization: Bearer $token")"
    fault_status="$(jq -r '.status' <<<"$fault_session")"
    [[ "$fault_status" == completed || "$fault_status" == failed ]] && break
    sleep 1
  done
  jq -e '.status == "failed" and (.failure_reason | type == "string" and . != "no_speech") and .transcript == null' \
    <<<"$fault_session" >/dev/null || {
    jq '{id, status, failure_reason, transcript}' <<<"$fault_session" >&2; exit 1;
  }
  fault_reason="$(jq -r '.failure_reason' <<<"$fault_session")"
fi

printf 'Sherpa App smoke passed\nm6_cases_required=%s\napp_size_bytes=%s\nmodel_status=%s\nsession_status=%s\ntranscript_chars=%s\nsilence_failure_reason=%s\nfault_failure_reason=%s\n' \
  "$require_m6_cases" \
  "$(du -sk "$app" | awk '{print $1 * 1024}')" "$model_status" "$session_status" "${#transcript}" \
  "$silent_reason" "$fault_reason"
