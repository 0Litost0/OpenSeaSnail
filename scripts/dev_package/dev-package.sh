#!/bin/bash
# Bash 3.2 bootstrap: help and missing-Python diagnostics require no Node/jq.
set -eu
TOOL_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT_DIR=$(CDPATH= cd -- "$TOOL_DIR/../.." && pwd)
if [ "$#" -eq 1 ] && { [ "$1" = --help ] || [ "$1" = -h ]; }; then
  cat <<'EOF'
Usage: scripts/dev_package/dev-package.sh [COMMAND] [OPTIONS]
Commands: check, build, test, package, version, status, runs
No arguments means package. Use COMMAND --help for the command contract.
--json: one final JSON object on stdout; progress/events on stderr.
--full: build/package only. --suite unit|integration|all: test only.
version show|check|increment|bump patch|minor|major|set X.Y.Z
--dry-run: version increment/bump/set only, with no writes.
status RUN_ID | status --current; runs [--operation OP] [--since UTC] [--limit N]
Python >=3.11 is required; tools are checked and never installed.
MVP milestones are implemented incrementally; unavailable operations fail explicitly.
EOF
  exit 0
fi
json_escape() {
  local value="$1"
  value=${value//\\/\\\\}; value=${value//\"/\\\"}
  value=${value//$'\n'/\\n}; value=${value//$'\r'/\\r}; value=${value//$'\t'/\\t}
  local code control escaped
  for ((code=1; code<32; code++)); do
    case "$code" in 9|10|13) continue ;; esac
    printf -v control "\\$(printf '%03o' "$code")"
    printf -v escaped '\\u%04x' "$code"
    value=${value//"$control"/"$escaped"}
  done
  printf '%s' "$value"
}
PYTHON=$(command -v python3 || true)
if [ -z "$PYTHON" ] || ! "$PYTHON" -c 'import sys; sys.exit(0 if sys.version_info >= (3,11) else 1)' >/dev/null 2>&1; then
  actual=missing
  if [ -n "$PYTHON" ]; then actual=$("$PYTHON" --version 2>&1 || true); fi
  printf 'Python >=3.11 required; actual: %s. Select an existing supported interpreter and retry. No tools were installed.\n' "$actual" >&2
  for arg in "$@"; do
    if [ "$arg" = --json ]; then
      printf '{"schema_version":1,"operation":"bootstrap","run_id":null,"status":"blocked","stage":"bootstrap","started_at":null,"finished_at":null,"source":{},"environment":{},"checks":[],"artifacts":{},"log_path":null,"errors":[{"code":"python_unavailable","stage":"bootstrap","reason":"Python >=3.11 required","actual":"%s","required":"3.11+","path":null,"retry_policy":"after_user_fix","required_action":"Select an existing supported Python; retry","subprocess_exit_code":null}],"validation_scope":[],"requested_scope":{},"test_summary":null,"changes":null,"ready_for_task":false,"prepared":false}\n' "$(json_escape "$actual")"
      break
    fi
  done
  exit 3
fi
# Avoid bytecode writes on every read-only command; locate this checkout, not CWD.
export PYTHONDONTWRITEBYTECODE=1
cd "$ROOT_DIR"
exec "$PYTHON" -B -m scripts.dev_package "$@"
