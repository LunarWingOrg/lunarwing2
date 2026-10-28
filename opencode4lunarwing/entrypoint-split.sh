#!/bin/bash
set -euo pipefail

log() { printf '[%s] %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }
die() { log "ERROR: $*" >&2; exit 1; }

export PATH=/usr/local/cargo/bin:/usr/local/go/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/local/bun-node-fallback-bin

ROLE="${LUNARWING_CONTAINER_ROLE:?LUNARWING_CONTAINER_ROLE must be split-bridge or split-task}"
APP_DIR="${LUNARWING_APP_DIR:-/app}"
TASK_UID="${OPENCODE_TASK_UID:-10001}"
TASK_GID="${OPENCODE_TASK_GID:-10001}"
BRIDGE_UID="${OPENCODE_BRIDGE_UID:?OPENCODE_BRIDGE_UID is required for the split launcher}"
WORKSPACE_ROOT="${WORKSPACE_ROOT:-/workspace}"
RUNNER_SOCKET="${OPENCODE_TASK_RUNNER_SOCKET:-/run/lunarwing-shared/runner/task-runner.sock}"
RUNNER_STATE_FILE="${OPENCODE_TASK_RUNNER_STATE_FILE:-$(dirname "$RUNNER_SOCKET")/task-runner-state.json}"
OPENCODE_BIN="${OPENCODE_BIN:-/usr/local/bin/opencode}"
OPENCODE_SERVE_PORT="${OPENCODE_SERVE_PORT:-4096}"
OPENCODE_SERVE_HOST="${OPENCODE_SERVE_HOST:-127.0.0.1}"

if [[ ! "$TASK_UID" =~ ^[1-9][0-9]*$ || ! "$TASK_GID" =~ ^[1-9][0-9]*$ || ! "$BRIDGE_UID" =~ ^[1-9][0-9]*$ ]]; then
  die "split bridge/task ids must be non-zero numeric ids"
fi
[[ "$TASK_UID" != "$BRIDGE_UID" ]] || die "split bridge and task uids must differ"

umask 0007
export WORKSPACE_ROOT OPENCODE_SERVE_PORT OPENCODE_SERVE_HOST
export LUNARWING_TASK_RUNNER_REMOTE=1
export LUNARWING_TASK_RUNNER_SOCKET="$RUNNER_SOCKET"
export LUNARWING_TASK_RUNNER_STATE_FILE="$RUNNER_STATE_FILE"
export LUNARWING_TOOL_PROXY_DIR="${LUNARWING_TOOL_PROXY_DIR:-/run/lunarwing-shared/tool-proxy}"

start_health() {
  local health_port="${HEALTH_PORT:-8443}"
  export WS_STATE_FILE="${WS_STATE_FILE:-/tmp/lunarwing_ws_state.json}"
  export CODEX_MODE=websocket
  export CODEX_VERSION="opencode-worker-1.0.0"
  /usr/bin/python3 "$APP_DIR/health_server.py" --port "$health_port" &
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
  unset OPENCODE_TASK_UID OPENCODE_TASK_GID
  export LUNARWING_TASK_UID="$TASK_UID" LUNARWING_TASK_GID="$TASK_GID"
  export OPENCODE_MODE=websocket WS_ROLE="${WS_ROLE:-server}"

  local attempt=0
  while [[ ! -S "$RUNNER_SOCKET" ]]; do
    attempt=$((attempt + 1))
    [[ "$attempt" -lt 30 ]] || die "task runner did not create $RUNNER_SOCKET"
    sleep 1
  done
  start_health
  trap stop_bridge EXIT INT TERM
  cd "$APP_DIR"
  bun run "$APP_DIR/scripts/lunarwing_bridge.ts" &
  BRIDGE_PID=$!
  log "OpenCode non-root bridge started as uid $(id -u)"
  wait "$BRIDGE_PID"
}

wait_for_serve() {
  local attempt=0
  until /usr/bin/curl -s --max-time 2 -o /dev/null "http://$OPENCODE_SERVE_HOST:$OPENCODE_SERVE_PORT/" 2>/dev/null; do
    attempt=$((attempt + 1))
    [[ "$attempt" -lt 30 ]] || die "opencode server failed to start"
    kill -0 "$OPENCODE_PID" 2>/dev/null || die "opencode server exited unexpectedly"
    sleep 1
  done
}

run_task() {
  [[ "$(id -u)" == "$TASK_UID" ]] || die "split-task must run as uid $TASK_UID, not $(id -u)"
  if [[ -n "${AGENT_AUTH_TOKEN:-}" || -n "${AGENT_AUTH_TOKEN_FILE:-}" ]]; then
    die "hub credential variables must never reach the task container"
  fi
  unset AGENT_AUTH_TOKEN AGENT_AUTH_TOKEN_FILE OPENCODE_TASK_UID OPENCODE_TASK_GID
  export HOME=/home/opencode
  WORKSPACE_ROOT="$WORKSPACE_ROOT" "$APP_DIR/entrypoint-task-setup.sh"
  cd "$WORKSPACE_ROOT"
  "$OPENCODE_BIN" serve --port "$OPENCODE_SERVE_PORT" --hostname "$OPENCODE_SERVE_HOST" &
  OPENCODE_PID=$!
  wait_for_serve
  log "OpenCode task runner started as uid $(id -u)"
  exec bun run "$APP_DIR/scripts/task_runner.ts"
}

case "$ROLE" in
  split-bridge) run_bridge ;;
  split-task) run_task ;;
  *) die "unknown LUNARWING_CONTAINER_ROLE '$ROLE'" ;;
esac
