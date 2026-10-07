#!/usr/bin/env bash
# 验证锁定的 Q8 + VAD server 仅在随机 loopback 端口启动并通过 /health。
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SERVER="$ROOT_DIR/third_party/sensevoice/macos-arm64/bundle/sensevoice-server"
VERIFY="$ROOT_DIR/scripts/sensevoice/verify-artifact.sh"
MODEL=""
VAD=""
PORT=""
TIMEOUT=30
RANDOM_PORT_ATTEMPTS=5

usage() {
  cat <<'EOF'
Usage: scripts/sensevoice/run-server-smoke.sh --model Q8_GGUF --vad VAD_GGUF \
  [--server PATH] [--port PORT] [--timeout SECONDS]

Starts the server only on 127.0.0.1, polls GET /health, checks the listener,
then terminates the process. The model and VAD must match artifact-lock.json.
EOF
}

die() { printf '%s\n' "error: $*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --server) [[ $# -ge 2 ]] || die "--server requires a path"; SERVER="$2"; shift 2 ;;
    --model) [[ $# -ge 2 ]] || die "--model requires a path"; MODEL="$2"; shift 2 ;;
    --vad) [[ $# -ge 2 ]] || die "--vad requires a path"; VAD="$2"; shift 2 ;;
    --port) [[ $# -ge 2 && "$2" =~ ^[1-9][0-9]*$ && "$2" -le 65535 ]] || die "--port requires 1..65535"; PORT="$2"; shift 2 ;;
    --timeout) [[ $# -ge 2 && "$2" =~ ^[1-9][0-9]*$ ]] || die "--timeout requires positive seconds"; TIMEOUT="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done

[[ -x "$SERVER" ]] || die "server is not executable: $SERVER"
[[ -n "$MODEL" && -n "$VAD" ]] || die "--model and --vad are required"
command -v curl >/dev/null 2>&1 || die "curl is required for health probing"
command -v lsof >/dev/null 2>&1 || die "lsof is required for listener verification"
"$VERIFY" q8 "$MODEL" >/dev/null
"$VERIFY" fsmn-vad "$VAD" >/dev/null

EXPLICIT_PORT=0
if [[ -n "$PORT" ]]; then
  EXPLICIT_PORT=1
else
  command -v jot >/dev/null 2>&1 || die "jot is required to choose a random port"
fi

LOG_DIR="$(mktemp -d "${TMPDIR:-/tmp}/seasnail-sensevoice-smoke.XXXXXX")"
PID=""
cleanup() {
  if [[ -n "$PID" ]] && kill -0 "$PID" 2>/dev/null; then
    kill "$PID" 2>/dev/null || true
  fi
  [[ -z "$PID" ]] || wait "$PID" 2>/dev/null || true
  rm -rf "$LOG_DIR"
}
trap cleanup EXIT

for ((attempt = 1; attempt <= RANDOM_PORT_ATTEMPTS; attempt++)); do
  if [[ "$EXPLICIT_PORT" -eq 0 ]]; then
    PORT="$(jot -r 1 49152 65535)"
  fi
  : >"$LOG_DIR/stdout.log"
  : >"$LOG_DIR/stderr.log"
  "$SERVER" -m "$MODEL" -vad "$VAD" --max-audio-seconds 3600 127.0.0.1 "$PORT" >"$LOG_DIR/stdout.log" 2>"$LOG_DIR/stderr.log" &
  PID="$!"
  deadline=$((SECONDS + TIMEOUT))
  ready=0
  until curl --fail --silent --show-error --connect-timeout 1 --max-time 1 \
    "http://127.0.0.1:$PORT/health" >"$LOG_DIR/health.json" 2>/dev/null; do
    if ! kill -0 "$PID" 2>/dev/null; then
      wait "$PID" 2>/dev/null || true
      PID=""
      if [[ "$EXPLICIT_PORT" -eq 0 ]] && rg -q 'failed to bind' "$LOG_DIR/stderr.log"; then
        break
      fi
      sed -n '1,120p' "$LOG_DIR/stderr.log" >&2 || true
      die "server exited before /health became ready"
    fi
    ((SECONDS < deadline)) || die "server health check timed out after ${TIMEOUT}s"
    sleep 0.1
  done
  [[ -n "$PID" ]] || continue
  ready=1
  # `lsof` must report exactly one listener for this PID+port and it must be
  # the IPv4 loopback endpoint. Wildcard, LAN and IPv6 listeners are rejected.
  listeners="$(lsof -nP -a -p "$PID" -iTCP:"$PORT" -sTCP:LISTEN 2>/dev/null | tail -n +2 | sed '/^$/d' || true)"
  listener_count="$(printf '%s\n' "$listeners" | sed '/^$/d' | wc -l | tr -d ' ')"
  [[ "$listener_count" == 1 ]] && \
    printf '%s\n' "$listeners" | rg -qx ".* TCP 127\\.0\\.0\\.1:$PORT \\(LISTEN\\)" || \
    die "server is not listening exclusively on 127.0.0.1:$PORT"
  printf 'Server health smoke passed: pid=%s endpoint=http://127.0.0.1:%s/health\n' "$PID" "$PORT"
  break
done

[[ "$ready" -eq 1 ]] || die "server could not bind a random loopback port after ${RANDOM_PORT_ATTEMPTS} attempts"
