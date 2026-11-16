#!/bin/bash
set -euo pipefail

log() { printf '[%s] %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }
die() { log "ERROR: $*" >&2; exit 1; }

export PATH=/usr/local/cargo/bin:/usr/local/go/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/local/bun-node-fallback-bin

[[ "${LUNARWING_CONTAINER_ROLE:-}" == "split-bridge" ]] || die "this launcher only accepts split-bridge"
APP_DIR="${LUNARWING_APP_DIR:-/app}"
TASK_UID="${OPENCODE2_TASK_UID:-10003}"
TASK_GID="${OPENCODE2_TASK_GID:-10003}"
BRIDGE_UID="${OPENCODE2_BRIDGE_UID:?OPENCODE2_BRIDGE_UID is required for the split launcher}"
WORKSPACE_ROOT="${WORKSPACE_ROOT:-/workspace}"
RUNNER_SOCKET="${OPENCODE2_TASK_RUNNER_SOCKET:-/run/lunarwing-shared/runner/task-runner.sock}"
RUNNER_STATE_FILE="${OPENCODE2_TASK_RUNNER_STATE_FILE:-$(dirname "$RUNNER_SOCKET")/task-runner-state.json}"

if [[ ! "$TASK_UID" =~ ^[1-9][0-9]*$ || ! "$TASK_GID" =~ ^[1-9][0-9]*$ || ! "$BRIDGE_UID" =~ ^[1-9][0-9]*$ ]]; then
  die "split bridge/task ids must be non-zero numeric ids"
fi
[[ "$TASK_UID" != "$BRIDGE_UID" ]] || die "split bridge and task uids must differ"
[[ "$(id -u)" == "$BRIDGE_UID" ]] || die "split-bridge must run as uid $BRIDGE_UID, not $(id -u)"

umask 0007
export WORKSPACE_ROOT
export LUNARWING_TASK_RUNNER_REMOTE=1
export LUNARWING_TASK_RUNNER_SOCKET="$RUNNER_SOCKET"
export LUNARWING_TASK_RUNNER_STATE_FILE="$RUNNER_STATE_FILE"
export LUNARWING_TOOL_PROXY_DIR="${LUNARWING_TOOL_PROXY_DIR:-/run/lunarwing-shared/tool-proxy}"
export LUNARWING_TASK_UID="$TASK_UID" LUNARWING_TASK_GID="$TASK_GID"
export OPENCODE_MODE=websocket WS_ROLE="${WS_ROLE:-server}"

export HOME=/home/opencode2-bridge
TOKEN_DIR="${LUNARWING_TOKEN_DIR:-${HOME}/.local/state/lunarwing}"
if [ -n "${AGENT_AUTH_TOKEN:-}" ]; then
  install -d -m 0700 "$TOKEN_DIR"
  TOKEN_TMP="$TOKEN_DIR/.agent_auth_token.$$"
  (umask 0177; printf '%s' "$AGENT_AUTH_TOKEN" >"$TOKEN_TMP")
  chmod 0400 "$TOKEN_TMP"
  mv "$TOKEN_TMP" "$TOKEN_DIR/agent_auth_token"
  unset AGENT_AUTH_TOKEN
  export AGENT_AUTH_TOKEN_FILE="$TOKEN_DIR/agent_auth_token"
fi
[[ -n "${AGENT_AUTH_TOKEN_FILE:-}" ]] || die "split-bridge requires the hub credential"

attempt=0
while [[ ! -S "$RUNNER_SOCKET" ]]; do
  attempt=$((attempt + 1))
  [[ "$attempt" -lt 120 ]] || die "task runner did not create $RUNNER_SOCKET"
  sleep 1
done

export WS_STATE_FILE="${WS_STATE_FILE:-/tmp/lunarwing_ws_state.json}"
export CODEX_MODE=websocket
export CODEX_VERSION="opencode2-worker-1.0.0"
/usr/bin/python3 "$APP_DIR/health_server.py" --port "${HEALTH_PORT:-8443}" &
HEALTH_PID=$!
cleanup() {
  [ -z "${BRIDGE_PID:-}" ] || kill "$BRIDGE_PID" 2>/dev/null || true
  kill "$HEALTH_PID" 2>/dev/null || true
  [ -z "${BRIDGE_PID:-}" ] || wait "$BRIDGE_PID" 2>/dev/null || true
  wait "$HEALTH_PID" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

cd "$APP_DIR"
bun run "$APP_DIR/scripts/lunarwing_bridge.ts" &
BRIDGE_PID=$!
log "OpenCode 2 non-root bridge started as uid $(id -u)"
wait "$BRIDGE_PID"
