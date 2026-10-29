#!/bin/bash
set -euo pipefail

log() { printf '[%s] %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }
die() { log "ERROR: $*" >&2; exit 1; }

export PATH=/usr/local/cargo/bin:/usr/local/go/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

ROLE="${LUNARWING_CONTAINER_ROLE:?LUNARWING_CONTAINER_ROLE must be split-bridge or split-task}"
APP_DIR="${LUNARWING_APP_DIR:-/app}"
TASK_UID="${KIMI_TASK_UID:-10002}"
TASK_GID="${KIMI_TASK_GID:-10002}"
BRIDGE_UID="${KIMI_BRIDGE_UID:?KIMI_BRIDGE_UID is required for the split launcher}"
WORKSPACE_ROOT="${WORKSPACE_ROOT:-/workspace}"
if [[ "${LUNARWING_ROOTFUL_WORKSPACE:-}" == 1 ]]; then
  [[ "${LUNARWING_WORKSPACE_ID:-}" =~ ^[0-9]+:[0-9]+$ ]] || die "rootful workspace identity is required"
  [[ "$(stat -c '%d:%i' -- "$WORKSPACE_ROOT")" == "$LUNARWING_WORKSPACE_ID" ]] \
    || die "workspace mount changed identity; refusing redirected workspace"
fi
RUNNER_SOCKET="${KIMI_TASK_RUNNER_SOCKET:-/run/lunarwing-shared/runner/task-runner.sock}"
RUNNER_STATE_FILE="${KIMI_TASK_RUNNER_STATE_FILE:-$(dirname "$RUNNER_SOCKET")/task-runner-state.json}"

if [[ ! "$TASK_UID" =~ ^[1-9][0-9]*$ || ! "$TASK_GID" =~ ^[1-9][0-9]*$ || ! "$BRIDGE_UID" =~ ^[1-9][0-9]*$ ]]; then
  die "split bridge/task ids must be non-zero numeric ids"
fi
if [[ "$TASK_UID" == "$BRIDGE_UID" ]]; then
  die "split bridge and task uids must differ"
fi

umask 0007
export WORKSPACE_ROOT
export LUNARWING_TASK_RUNNER_REMOTE=1
export LUNARWING_TASK_RUNNER_SOCKET="$RUNNER_SOCKET"
export LUNARWING_TASK_RUNNER_STATE_FILE="$RUNNER_STATE_FILE"
export LUNARWING_TOOL_PROXY_DIR="${LUNARWING_TOOL_PROXY_DIR:-/run/lunarwing-shared/tool-proxy}"

start_health() {
  local health_port="${HEALTH_PORT:-8443}"
  export WS_STATE_FILE="${WS_STATE_FILE:-/tmp/lunarwing_ws_state.json}"
  export KIMI_MODE=websocket
  export KIMI_VERSION="kimi-worker-1.0.0"
  /usr/bin/python3 "$APP_DIR"/health_server.py --port "$health_port" &
  HEALTH_PID=$!
}

stop_bridge() {
  [ -z "${BRIDGE_PID:-}" ] || kill "$BRIDGE_PID" 2>/dev/null || true
  [ -z "${HEALTH_PID:-}" ] || kill "$HEALTH_PID" 2>/dev/null || true
  [ -z "${BRIDGE_PID:-}" ] || wait "$BRIDGE_PID" 2>/dev/null || true
  [ -z "${HEALTH_PID:-}" ] || wait "$HEALTH_PID" 2>/dev/null || true
}

run_bridge() {
  [[ "$(id -u)" == "$BRIDGE_UID" ]] || die "split-bridge must run as uid $BRIDGE_UID, not $(id -u)"
  export HOME=/home/kimi-bridge
  local token_dir="${LUNARWING_TOKEN_DIR:-${HOME}/.local/state/lunarwing}"
  if [ -n "${AGENT_AUTH_TOKEN:-}" ]; then
    install -d -m 0700 "$token_dir"
    local token_tmp="$token_dir/.agent_auth_token.$$"
    (umask 0177; printf '%s' "$AGENT_AUTH_TOKEN" >"$token_tmp")
    chmod 0400 "$token_tmp"
    mv "$token_tmp" "$token_dir/agent_auth_token"
    unset AGENT_AUTH_TOKEN
    export AGENT_AUTH_TOKEN_FILE="$token_dir/agent_auth_token"
  fi
  [[ -n "${AGENT_AUTH_TOKEN_FILE:-}" ]] || die "split-bridge requires the hub credential"

  unset KIMI_TASK_UID KIMI_TASK_GID
  export LUNARWING_TASK_UID="$TASK_UID"
  export LUNARWING_TASK_GID="$TASK_GID"
  export KIMI_MODE=websocket
  export WS_ROLE="${WS_ROLE:-server}"

  local attempt=0
  while [[ ! -S "$RUNNER_SOCKET" ]]; do
    attempt=$((attempt + 1))
    [[ "$attempt" -lt 30 ]] || die "task runner did not create $RUNNER_SOCKET"
    sleep 1
  done
  start_health
  trap stop_bridge EXIT INT TERM
  cd "$APP_DIR"
  node "$APP_DIR"/scripts/lunarwing_bridge.js &
  BRIDGE_PID=$!
  log "Kimi non-root bridge started as uid $(id -u)"
  wait "$BRIDGE_PID"
}

run_task() {
  [[ "$(id -u)" == "$TASK_UID" ]] || die "split-task must run as uid $TASK_UID, not $(id -u)"
  if [[ -n "${AGENT_AUTH_TOKEN:-}" || -n "${AGENT_AUTH_TOKEN_FILE:-}" ]]; then
    die "hub credential variables must never reach the task container"
  fi
  unset AGENT_AUTH_TOKEN AGENT_AUTH_TOKEN_FILE KIMI_TASK_UID KIMI_TASK_GID
  export HOME=/home/kimi
  export KIMI_CODE_HOME="${KIMI_CODE_HOME:-/home/kimi/.kimi-code}"
  KIMI_CODE_HOME="$KIMI_CODE_HOME" "$APP_DIR"/entrypoint-task-setup.sh
  export KIMI_API_KEY="${KIMI_API_KEY:-${TENSORZERO_API_KEY:-}}"
  log "Kimi task runner started as uid $(id -u)"
  exec node "$APP_DIR"/scripts/task_runner.js "$RUNNER_SOCKET"
}

case "$ROLE" in
  split-bridge) run_bridge ;;
  split-task) run_task ;;
  *) die "unknown LUNARWING_CONTAINER_ROLE '$ROLE'" ;;
esac
