#!/usr/bin/env bash
# M6.2 one cold-run collector.  It records only process metadata and resource
# counters; it never persists API secrets, transcript text, source paths, or commands.
set -euo pipefail

usage() {
  echo "Usage: $0 --app APP --label slim|gguf-q8 --sample WAV --output-dir EMPTY_DIR" >&2
  exit 2
}

app=""; label=""; sample=""; output_dir=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --app) app="${2:-}"; shift 2 ;;
    --label) label="${2:-}"; shift 2 ;;
    --sample) sample="${2:-}"; shift 2 ;;
    --output-dir) output_dir="${2:-}"; shift 2 ;;
    *) usage ;;
  esac
done
[[ -x "$app/Contents/MacOS/SeaSnail" && -f "$sample" && -n "$label" && -n "$output_dir" && ! -e "$output_dir" ]] || usage
command -v curl >/dev/null && command -v jq >/dev/null && command -v pgrep >/dev/null && command -v ps >/dev/null || { echo "curl, jq, pgrep and ps are required" >&2; exit 1; }

mkdir -p "$output_dir"
data_dir="$(mktemp -d /private/tmp/seasnail-m62-data.XXXXXX)"
gui_log="$output_dir/gui.log"
process_csv="$output_dir/processes.csv"
total_csv="$output_dir/totals.csv"
printf 'relative_ms,scenario,pid,ppid,pgid,role,rss_kib,event\n' > "$process_csv"
printf 'relative_ms,scenario,total_rss_kib,pid_count\n' > "$total_csv"

now_ms() { perl -MTime::HiRes=time -e 'printf "%.0f\n", time * 1000'; }
start_ms="$(now_ms)"
gui_pid=""; sampler_pid=""
cleanup() {
  [[ -z "$sampler_pid" ]] || kill "$sampler_pid" 2>/dev/null || true
  [[ -z "$sampler_pid" ]] || wait "$sampler_pid" 2>/dev/null || true
  [[ -z "$gui_pid" ]] || kill "$gui_pid" 2>/dev/null || true
  [[ -z "$gui_pid" ]] || wait "$gui_pid" 2>/dev/null || true
}
trap cleanup EXIT

descendants() {
  local parent="$1" child
  while IFS= read -r child; do
    [[ -n "$child" ]] || continue
    printf '%s\n' "$child"
    descendants "$child"
  done < <(pgrep -P "$parent" 2>/dev/null || true)
}

role_for_pid() {
  local pid="$1" command
  # A helper may exit after descendant enumeration but before this lookup.
  command="$(ps -o comm= -p "$pid" 2>/dev/null | tr -d ' ' || true)"
  case "$command" in
    *seasnail-daemon) printf daemon ;;
    *sensevoice-server) printf sensevoice_server ;;
    *seasnail-funasr) printf funasr_sidecar ;;
    *SeaSnail) printf gui ;;
    *ffmpeg) printf ffmpeg_helper ;;
    *) printf helper ;;
  esac
}

sample_once() {
  local scenario="$1" t_ms=0 total=0 count=0 pid line pid_out ppid pgid rss role
  t_ms="$(( $(now_ms) - start_ms ))"
  while IFS= read -r pid; do
    line="$(ps -o pid=,ppid=,pgid=,rss= -p "$pid" 2>/dev/null || true)"
    [[ -n "$line" ]] || continue
    read -r pid_out ppid pgid rss <<< "$line"
    [[ "$rss" =~ ^[0-9]+$ ]] || continue
    role="$(role_for_pid "$pid_out")"
    printf '%s,%s,%s,%s,%s,%s,%s,sampled\n' "$t_ms" "$scenario" "$pid_out" "$ppid" "$pgid" "$role" "$rss" >> "$process_csv"
    total="$((total + rss))"; count="$((count + 1))"
  done < <({ printf '%s\n' "$gui_pid"; descendants "$gui_pid"; } | sort -nu)
  printf '%s,%s,%s,%s\n' "$t_ms" "$scenario" "$total" "$count" >> "$total_csv"
}

sample_window() {
  local scenario="$1" seconds="$2" interval="$3" deadline
  deadline="$((SECONDS + seconds))"
  while (( SECONDS < deadline )); do
    sample_once "$scenario"
    sleep "$interval"
  done
  sample_once "$scenario"
}

wait_for() {
  local seconds="$1" predicate="$2" deadline
  deadline="$((SECONDS + seconds))"
  while (( SECONDS < deadline )); do
    eval "$predicate" && return 0
    sleep 1
  done
  return 1
}

SEASNAIL_DATA_DIR="$data_dir" SEASNAIL_DEV_FILE_KEYCHAIN=1 "$app/Contents/MacOS/SeaSnail" > "$gui_log" 2>&1 &
gui_pid="$!"
wait_for 60 "[[ -s '$data_dir/bootstrap.json' ]]" || { tail -n 120 "$gui_log" >&2; exit 1; }
bootstrap_ms="$(( $(now_ms) - start_ms ))"
port="$(jq -r '.port' "$data_dir/bootstrap.json")"
setup="$(curl --fail --silent --show-error -X POST "http://127.0.0.1:$port/api/v1/auth/setup" -H 'Content-Type: application/json' --data '{"username":"m62","password":"m62-measurement-password"}')"
token="$(printf '%s' "$setup" | jq -r '.secret')"
[[ -n "$token" && "$token" != null ]] || exit 1

model_status=""
for _ in $(seq 1 180); do
  models="$(curl --fail --silent --show-error "http://127.0.0.1:$port/api/v1/models" -H "Authorization: Bearer $token")"
  model_status="$(printf '%s' "$models" | jq -r '.[] | select(.default == true) | .status' | head -n 1)"
  [[ "$model_status" == active ]] && break
  sleep 1
done
[[ "$model_status" == active ]] || { printf '%s' "$models" | jq 'map({id,status,runtime})' >&2; exit 1; }
active_ms="$(( $(now_ms) - start_ms ))"
sample_window idle 30 1

run_session() {
  local scenario="$1" created session_id session_status="" sampler_start sampler_end elapsed
  sampler_start="$(now_ms)"
  sample_window "$scenario" 180 0.05 & sampler_pid="$!"
  created="$(curl --fail --silent --show-error -X POST "http://127.0.0.1:$port/api/v1/sessions" -H "Authorization: Bearer $token" -F "audio=@$sample;type=audio/wav" -F source=imported -F language=zh)"
  session_id="$(printf '%s' "$created" | jq -r '.id')"
  [[ -n "$session_id" && "$session_id" != null ]] || exit 1
  for _ in $(seq 1 180); do
    session="$(curl --fail --silent --show-error "http://127.0.0.1:$port/api/v1/sessions/$session_id" -H "Authorization: Bearer $token")"
    session_status="$(printf '%s' "$session" | jq -r '.status')"
    [[ "$session_status" == completed || "$session_status" == failed ]] && break
    sleep 0.1
  done
  kill "$sampler_pid" 2>/dev/null || true; wait "$sampler_pid" 2>/dev/null || true; sampler_pid=""
  sampler_end="$(now_ms)"; elapsed="$((sampler_end - sampler_start))"
  [[ "$session_status" == completed && -n "$(printf '%s' "$session" | jq -r '.transcript // empty')" ]] || { printf '%s' "$session" | jq '{status,failure_reason}' >&2; exit 1; }
  printf '%s,%s,%s\n' "$scenario" "$elapsed" "$(printf '%s' "$session" | jq -r '.segments | length')" >> "$output_dir/sessions.csv"
}

printf 'scenario,elapsed_ms,segment_count\n' > "$output_dir/sessions.csv"
run_session first
sleep 2
run_session consecutive_2
sleep 2
run_session consecutive_3
sample_window after 30 1

app_bytes="$(du -sk "$app" | awk '{print $1 * 1024}')"
jq -n --arg label "$label" --arg app_sha256 "$(shasum -a 256 "$app/Contents/MacOS/SeaSnail" | awk '{print $1}')" --arg sample_sha256 "$(shasum -a 256 "$sample" | awk '{print $1}')" --argjson app_bytes "$app_bytes" --argjson launch_to_bootstrap_ms "$bootstrap_ms" --argjson launch_to_active_ms "$active_ms" --arg macos "$(sw_vers -productVersion)" --arg build "$(sw_vers -buildVersion)" --arg arch "$(uname -m)" '{label:$label,app_bytes:$app_bytes,app_executable_sha256:$app_sha256,sample_sha256:$sample_sha256,machine:{macos:$macos,build:$build,architecture:$arch},launch_to_bootstrap_ms:$launch_to_bootstrap_ms,launch_to_active_ms:$launch_to_active_ms,measurement:{rss_sampling_ms:{idle:1000,first:50,consecutive:50,after:1000},footprint:"not collected by this RSS collector; see M6.1 protocol"}}' > "$output_dir/metadata.json"
printf 'M6.2 cold run recorded: %s\n' "$output_dir"
