#!/usr/bin/env bash
# 验证 sensevoice-server 的启动失败、health 超时、HTTP 非 2xx 和主动停止均能回收子进程。
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SERVER="$ROOT_DIR/third_party/sensevoice/macos-arm64/bundle/sensevoice-server"
VERIFY="$ROOT_DIR/scripts/sensevoice/verify-artifact.sh"
MODEL=""
VAD=""
TIMEOUT=5

usage() {
  cat <<'EOF'
Usage: scripts/sensevoice/run-server-failure-smoke.sh --model Q8_GGUF --vad VAD_GGUF \
  [--server PATH] [--timeout SECONDS]

Runs controlled local failure cases and a live server request. All children are
waited for before exit; the live server is bound only to a random 127.0.0.1 port.
EOF
}

die() { printf '%s\n' "error: $*" >&2; exit 1; }
now_ms() { perl -MTime::HiRes=time -e 'printf "%.0f\n", time * 1000'; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --server) [[ $# -ge 2 ]] || die "--server requires a path"; SERVER="$2"; shift 2 ;;
    --model) [[ $# -ge 2 ]] || die "--model requires a path"; MODEL="$2"; shift 2 ;;
    --vad) [[ $# -ge 2 ]] || die "--vad requires a path"; VAD="$2"; shift 2 ;;
    --timeout) [[ $# -ge 2 && "$2" =~ ^[1-9][0-9]*$ ]] || die "--timeout requires positive seconds"; TIMEOUT="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done

[[ -x "$SERVER" ]] || die "server is not executable: $SERVER"
[[ -n "$MODEL" && -n "$VAD" ]] || die "--model and --vad are required"
command -v curl >/dev/null 2>&1 || die "curl is required"
command -v jot >/dev/null 2>&1 || die "jot is required"
command -v lsof >/dev/null 2>&1 || die "lsof is required"
command -v perl >/dev/null 2>&1 || die "perl is required"
"$VERIFY" q8 "$MODEL" >/dev/null
"$VERIFY" fsmn-vad "$VAD" >/dev/null

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/seasnail-sensevoice-failure.XXXXXX")"
PID=""
terminate_group() {
  local group_pid="$1"
  kill -TERM -- "-$group_pid" 2>/dev/null || true
  local deadline=$((SECONDS + 3))
  while pgrep -g "$group_pid" >/dev/null 2>&1 && (( SECONDS < deadline )); do sleep 0.05; done
  if pgrep -g "$group_pid" >/dev/null 2>&1; then kill -KILL -- "-$group_pid" 2>/dev/null || true; fi
  wait "$group_pid" 2>/dev/null || true
  if pgrep -g "$group_pid" >/dev/null 2>&1; then die "process group still exists after cleanup: $group_pid"; fi
}
cleanup() {
  [[ -z "$PID" ]] || terminate_group "$PID"
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT

start_group() {
  # New session => PID is also the process-group ID, so cleanup covers server
  # descendants rather than only the shell leader.
  perl -MPOSIX=setsid -e 'setsid() == -1 and die "setsid: $!"; exec @ARGV' "$@" &
  PID="$!"
}
assert_group_reaped() {
  local group_pid="$1" label="$2"
  if pgrep -g "$group_pid" >/dev/null 2>&1; then die "$label process group was not reaped: pgid=$group_pid"; fi
}

# A command that exits immediately exercises startup failure without depending
# on a malformed model file or on a particular server diagnostic string.
EXITING_SERVER="$WORK_DIR/exiting-server"
printf '%s\n' '#!/usr/bin/env bash' 'exit 42' >"$EXITING_SERVER"
chmod 700 "$EXITING_SERVER"
start_group "$EXITING_SERVER" -m "$MODEL" -vad "$VAD" 127.0.0.1 1 >"$WORK_DIR/exiting.out" 2>"$WORK_DIR/exiting.err"
if wait "$PID"; then die "controlled startup failure unexpectedly succeeded"; fi
assert_group_reaped "$PID" "startup failure"
PID=""

# This binds and accepts health connections but never responds, exercising the
# curl response deadline instead of merely a connection-refused path.
HANGING_SERVER="$WORK_DIR/hanging-server"
printf '%s\n' '#!/usr/bin/env bash' 'port="${!#}"' 'exec perl -MIO::Socket::INET -e '\''my $s = IO::Socket::INET->new(LocalAddr => "127.0.0.1", LocalPort => shift, Proto => "tcp", Listen => 1, ReuseAddr => 1) or die $!; my $c = $s->accept or die $!; sleep 60; '\'' "$port"' >"$HANGING_SERVER"
chmod 700 "$HANGING_SERVER"
port="$(jot -r 1 49152 65535)"
start_group "$HANGING_SERVER" -m "$MODEL" -vad "$VAD" 127.0.0.1 "$port" >"$WORK_DIR/hanging.out" 2>"$WORK_DIR/hanging.err"
deadline=$((SECONDS + 5))
until lsof -nP -a -p "$PID" -iTCP:"$port" -sTCP:LISTEN 2>/dev/null | tail -n +2 | rg -qx ".* TCP 127\\.0\\.0\\.1:$port \\(LISTEN\\)"; do
  if ! kill -0 "$PID" 2>/dev/null; then sed -n '1,120p' "$WORK_DIR/hanging.err" >&2 || true; die "controlled response-timeout server exited before bind"; fi
  ((SECONDS < deadline)) || die "controlled response-timeout server did not bind"
  sleep 0.05
done
timeout_started="$(now_ms)"
if curl --fail --silent --show-error --connect-timeout 1 --max-time "$TIMEOUT" "http://127.0.0.1:$port/health" >"$WORK_DIR/health-timeout.json" 2>/dev/null; then
  die "controlled response-timeout server unexpectedly answered"
fi
timeout_elapsed="$(( $(now_ms) - timeout_started ))"
(( timeout_elapsed >= TIMEOUT * 800 )) || die "health request did not run to its timeout: ${timeout_elapsed}ms"
terminate_group "$PID"
assert_group_reaped "$PID" "health timeout"
PID=""

ready=0
for attempt in 1 2 3 4 5; do
  port="$(jot -r 1 49152 65535)"
  start_group "$SERVER" -m "$MODEL" -vad "$VAD" --max-audio-seconds 3600 127.0.0.1 "$port" >"$WORK_DIR/live.out" 2>"$WORK_DIR/live.err"
  deadline=$((SECONDS + TIMEOUT + 25))
  until curl --fail --silent --show-error --connect-timeout 1 --max-time 1 "http://127.0.0.1:$port/health" >"$WORK_DIR/health.json" 2>/dev/null; do
    if ! kill -0 "$PID" 2>/dev/null; then
      wait "$PID" 2>/dev/null || true
      PID=""
      break
    fi
    ((SECONDS < deadline)) || die "live server health check timed out"
    sleep 0.1
  done
  [[ -n "$PID" ]] || continue
  listeners="$(lsof -nP -a -p "$PID" -iTCP:"$port" -sTCP:LISTEN 2>/dev/null | tail -n +2 | sed '/^$/d' || true)"
  listener_count="$(printf '%s\n' "$listeners" | sed '/^$/d' | wc -l | tr -d ' ')"
  if [[ "$listener_count" == 1 ]] && printf '%s\n' "$listeners" | rg -qx ".* TCP 127\\.0\\.0\\.1:$port \\(LISTEN\\)"; then ready=1; break; fi
  terminate_group "$PID"
  PID=""
done
[[ "$ready" -eq 1 ]] || die "live server did not bind an exclusive loopback listener"

status="$(curl --silent --show-error --output "$WORK_DIR/non-2xx.json" --write-out '%{http_code}' \
  --connect-timeout 1 --max-time 5 "http://127.0.0.1:$port/does-not-exist" || true)"
[[ "$status" =~ ^4[0-9][0-9]$ ]] || die "expected HTTP 4xx for invalid endpoint, got: ${status:-curl failure}"

started_ms="$(now_ms)"
terminate_group "$PID"
stopped_ms="$(now_ms)"
assert_group_reaped "$PID" "active stop"
PID=""
printf 'Server failure smoke passed: startup failure, health timeout, HTTP %s, stop/reap in %sms\n' \
  "$status" "$((stopped_ms - started_ms))"
