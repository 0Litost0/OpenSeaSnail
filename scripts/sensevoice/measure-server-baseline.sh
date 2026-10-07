#!/usr/bin/env bash
# 记录 M1 原生 sidecar 的空闲、首次和连续转写 RSS/耗时基线（不含 GUI/daemon）。
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SERVER="$ROOT_DIR/third_party/sensevoice/macos-arm64/bundle/sensevoice-server"
VERIFY="$ROOT_DIR/scripts/sensevoice/verify-artifact.sh"
MODEL=""
VAD=""
SAMPLE=""
OUTPUT=""
TIMEOUT=45

usage() {
  cat <<'EOF'
Usage: scripts/sensevoice/measure-server-baseline.sh --model Q8_GGUF --vad VAD_GGUF --sample PCM_WAV --output RESULT.json
  [--server PATH] [--timeout SECONDS]

Measures the local server only. RSS is sampled from `ps` in KiB; request peak
is the largest sample while each multipart POST is in flight.
EOF
}

die() { printf '%s\n' "error: $*" >&2; exit 1; }
now_ms() { perl -MTime::HiRes=time -e 'printf "%.0f\n", time * 1000'; }
rss_kib() { ps -o rss= -p "$1" | tr -d ' ' ; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --server) [[ $# -ge 2 ]] || die "--server requires a path"; SERVER="$2"; shift 2 ;;
    --model) [[ $# -ge 2 ]] || die "--model requires a path"; MODEL="$2"; shift 2 ;;
    --vad) [[ $# -ge 2 ]] || die "--vad requires a path"; VAD="$2"; shift 2 ;;
    --sample) [[ $# -ge 2 ]] || die "--sample requires a path"; SAMPLE="$2"; shift 2 ;;
    --output) [[ $# -ge 2 ]] || die "--output requires a path"; OUTPUT="$2"; shift 2 ;;
    --timeout) [[ $# -ge 2 && "$2" =~ ^[1-9][0-9]*$ ]] || die "--timeout requires positive seconds"; TIMEOUT="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done

[[ -x "$SERVER" && -f "$SAMPLE" ]] || die "server or sample is missing"
[[ -n "$MODEL" && -n "$VAD" && -n "$OUTPUT" ]] || die "--model, --vad and --output are required"
[[ ! -e "$OUTPUT" ]] || die "refusing to overwrite output: $OUTPUT"
command -v curl >/dev/null 2>&1 || die "curl is required"
command -v jq >/dev/null 2>&1 || die "jq is required"
command -v jot >/dev/null 2>&1 || die "jot is required"
command -v perl >/dev/null 2>&1 || die "perl is required"
"$VERIFY" q8 "$MODEL" >/dev/null
"$VERIFY" fsmn-vad "$VAD" >/dev/null

OUTPUT_DIR="$(dirname "$OUTPUT")"
[[ -d "$OUTPUT_DIR" ]] || die "output parent directory does not exist: $OUTPUT_DIR"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/seasnail-sensevoice-baseline.XXXXXX")"
PID=""
TEMPORARY_OUTPUT=""
cleanup() {
  if [[ -n "$PID" ]] && kill -0 "$PID" 2>/dev/null; then kill "$PID" 2>/dev/null || true; fi
  [[ -z "$PID" ]] || wait "$PID" 2>/dev/null || true
  [[ -z "$TEMPORARY_OUTPUT" ]] || rm -f "$TEMPORARY_OUTPUT"
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT

port="$(jot -r 1 49152 65535)"
started_ms="$(now_ms)"
"$SERVER" -m "$MODEL" -vad "$VAD" --max-audio-seconds 3600 127.0.0.1 "$port" >"$WORK_DIR/server.out" 2>"$WORK_DIR/server.err" &
PID="$!"
deadline=$((SECONDS + TIMEOUT))
until curl --fail --silent --show-error --connect-timeout 1 --max-time 1 "http://127.0.0.1:$port/health" >"$WORK_DIR/health.json" 2>/dev/null; do
  if ! kill -0 "$PID" 2>/dev/null; then sed -n '1,120p' "$WORK_DIR/server.err" >&2 || true; die "server exited before health became ready"; fi
  ((SECONDS < deadline)) || die "server health check timed out after ${TIMEOUT}s"
  sleep 0.1
done
ready_ms="$(now_ms)"
idle_rss_kib="$(rss_kib "$PID")"

measure_request() {
  local label="$1" result started finished request_pid peak current
  result="$WORK_DIR/$label.json"
  peak="$(rss_kib "$PID")"
  started="$(now_ms)"
  curl --fail --silent --show-error --connect-timeout 2 --max-time "$TIMEOUT" \
    -F "file=@$SAMPLE;type=audio/wav" -F 'model=sensevoice-small' -F 'response_format=verbose_json' \
    "http://127.0.0.1:$port/v1/audio/transcriptions" >"$result" &
  request_pid="$!"
  while kill -0 "$request_pid" 2>/dev/null; do
    current="$(rss_kib "$PID" 2>/dev/null || true)"
    [[ "$current" =~ ^[0-9]+$ ]] && (( current > peak )) && peak="$current"
    sleep 0.05
  done
  wait "$request_pid"
  current="$(rss_kib "$PID" 2>/dev/null || true)"
  [[ "$current" =~ ^[0-9]+$ ]] && (( current > peak )) && peak="$current"
  finished="$(now_ms)"
  jq -e '.text | type == "string" and length > 0' "$result" >/dev/null || die "$label returned no text"
  printf '%s %s %s\n' "$((finished - started))" "$peak" "$(jq '.segments | length' "$result")"
}

read -r first_ms first_peak_kib first_segments < <(measure_request first)
read -r second_ms second_peak_kib second_segments < <(measure_request consecutive)
server_sha256="$(shasum -a 256 "$SERVER" | awk '{print $1}')"
model_sha256="$(shasum -a 256 "$MODEL" | awk '{print $1}')"
vad_sha256="$(shasum -a 256 "$VAD" | awk '{print $1}')"
sample_sha256="$(shasum -a 256 "$SAMPLE" | awk '{print $1}')"

TEMPORARY_OUTPUT="$(mktemp "$OUTPUT_DIR/.seasnail-baseline.XXXXXX")"
jq -n \
  --arg captured_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg macos "$(sw_vers -productVersion)" \
  --arg build "$(sw_vers -buildVersion)" \
  --arg machine "$(uname -m)" \
  --arg server "$SERVER" --arg model "$MODEL" --arg vad "$VAD" --arg sample "$SAMPLE" \
  --arg server_sha256 "$server_sha256" --arg model_sha256 "$model_sha256" --arg vad_sha256 "$vad_sha256" --arg sample_sha256 "$sample_sha256" \
  --argjson startup_ms "$((ready_ms - started_ms))" --argjson idle_rss_kib "$idle_rss_kib" \
  --argjson first_ms "$first_ms" --argjson first_peak_kib "$first_peak_kib" --argjson first_segments "$first_segments" \
  --argjson second_ms "$second_ms" --argjson second_peak_kib "$second_peak_kib" --argjson second_segments "$second_segments" \
  '{captured_at:$captured_at,scope:"sensevoice-server sidecar only; RSS from macOS ps in KiB, sampled every 50ms during requests (plus after completion); no GUI or daemon",machine:{macos:$macos,build:$build,architecture:$machine},command:{server:$server,arguments:["-m",$model,"-vad",$vad,"--max-audio-seconds","3600","127.0.0.1","<random-port>"],sample:$sample},artifacts:{server_sha256:$server_sha256,model_sha256:$model_sha256,vad_sha256:$vad_sha256,sample_sha256:$sample_sha256},sampling_interval_ms:50,startup_to_health_ms:$startup_ms,idle_rss_kib:$idle_rss_kib,first_transcription:{elapsed_ms:$first_ms,peak_rss_kib:$first_peak_kib,segments:$first_segments},consecutive_transcription:{elapsed_ms:$second_ms,peak_rss_kib:$second_peak_kib,segments:$second_segments}}' >"$TEMPORARY_OUTPUT"
if ! ln -h "$TEMPORARY_OUTPUT" "$OUTPUT" 2>/dev/null; then die "output appeared during measurement; refusing to overwrite: $OUTPUT"; fi
rm -f "$TEMPORARY_OUTPUT"
TEMPORARY_OUTPUT=""
printf 'Server baseline recorded: %s\n' "$OUTPUT"
