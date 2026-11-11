#!/bin/bash
set -euo pipefail

log() { printf '[%s] %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }
die() { log "ERROR: $*" >&2; exit 1; }

export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
ROLE="${LUNARWING_CONTAINER_ROLE:?LUNARWING_CONTAINER_ROLE must be split-bridge or split-task}"
APP_DIR="${LUNARWING_APP_DIR:-/app}"
WORKSPACE_ROOT="${WORKSPACE_ROOT:-/workspace}"
RUNNER_SOCKET="${LUNARDEPTH_RUNNER_SOCKET:-/run/lunarwing-shared/runner/task-runner.sock}"
RUNNER_STATE_FILE="${LUNARDEPTH_RUNNER_STATE_FILE:-$(dirname "$RUNNER_SOCKET")/task-runner-state.json}"
if [[ "${LUNARWING_ROOTFUL_WORKSPACE:-}" == "1" ]]; then
  [[ "${LUNARWING_WORKSPACE_ID:-}" =~ ^[0-9]+:[0-9]+$ ]] || die "rootful workspace identity is required"
  [[ "$(stat -c '%d:%i' -- "$WORKSPACE_ROOT")" == "$LUNARWING_WORKSPACE_ID" ]] \
    || die "workspace mount changed identity; refusing redirected workspace"
fi
umask 0027
export WORKSPACE_ROOT LUNARDEPTH_RUNNER_SOCKET="$RUNNER_SOCKET" LUNARDEPTH_RUNNER_STATE_FILE="$RUNNER_STATE_FILE"

stop_task() {
  [[ -z "${HARNESS_PID:-}" ]] || kill "$HARNESS_PID" 2>/dev/null || true
  [[ -z "${RUNNER_PID:-}" ]] || kill "$RUNNER_PID" 2>/dev/null || true
  [[ -z "${HARNESS_PID:-}" ]] || wait "$HARNESS_PID" 2>/dev/null || true
  [[ -z "${RUNNER_PID:-}" ]] || wait "$RUNNER_PID" 2>/dev/null || true
}

run_task() {
  [[ "$(id -u)" == "10005" && "$(id -g)" == "10005" ]] || die "split-task requires 10005:10005"
  [[ -z "${AGENT_AUTH_TOKEN:-}" && -z "${AGENT_AUTH_TOKEN_FILE:-}" ]] || die "hub credentials must not reach the task"
  unset AGENT_AUTH_TOKEN AGENT_AUTH_TOKEN_FILE LUNARDEPTH_TASK_UID LUNARDEPTH_TASK_GID
  "$APP_DIR/entrypoint-task-setup.sh"
  cd /run/lunardepth-launch
  /usr/local/bin/node "$APP_DIR/scripts/effective_config_assert.js" gate
  /usr/local/bin/node "$APP_DIR/scripts/sandbox_smoke.js"

  local launcher="${LUNARDEPTH_CLOSURE_ROOT:-/opt/lunardepth}/node_modules/@lunardepth/dsh/lib/bin.js"
  local overlay="${LUNARDEPTH_EFFECTIVE_OVERLAY:-/run/lunardepth-config/lunarwing-lane.patch.yml}"
  export LUNARDEPTH_BOOT_LOG=/run/lunardepth-state/boot.log
  /usr/local/bin/node "$launcher" --profile lunarwing-worker --patch "$overlay" >"$LUNARDEPTH_BOOT_LOG" 2>&1 &
  HARNESS_PID=$!
  /usr/local/bin/node "$APP_DIR/scripts/lunardepth_task_runner.js" "$RUNNER_SOCKET" &
  RUNNER_PID=$!
  trap stop_task EXIT INT TERM

  local attempt=0
  until curl -fsS --max-time 2 http://127.0.0.1:19091/ready >/dev/null 2>&1; do
    attempt=$((attempt + 1))
    [[ "$attempt" -lt 90 ]] || die "LunarDepth did not become ready"
    kill -0 "$HARNESS_PID" 2>/dev/null || die "LunarDepth exited before readiness"
    kill -0 "$RUNNER_PID" 2>/dev/null || die "task runner exited before readiness"
    sleep 1
  done
  /usr/local/bin/node "$APP_DIR/scripts/effective_config_assert.js" post-ready
  log "LunarDepth task harness and runner are ready"
  wait -n "$HARNESS_PID" "$RUNNER_PID" || true
  die "LunarDepth task role stopped unexpectedly"
}

stop_bridge() {
  [[ -z "${BRIDGE_PID:-}" ]] || kill "$BRIDGE_PID" 2>/dev/null || true
  [[ -z "${HEALTH_PID:-}" ]] || kill "$HEALTH_PID" 2>/dev/null || true
  [[ -z "${BRIDGE_PID:-}" ]] || wait "$BRIDGE_PID" 2>/dev/null || true
  [[ -z "${HEALTH_PID:-}" ]] || wait "$HEALTH_PID" 2>/dev/null || true
}

run_bridge() {
  [[ "$(id -u)" == "10105" && "$(id -g)" == "10005" ]] || die "split-bridge requires 10105:10005"
  [[ -z "${LUNARDEPTH_API_KEY:-}" && -z "${LUNARDEPTH_API_KEY_FILE:-}" ]] || die "model credential must not reach the bridge"
  [[ -z "${LUNARDEPTH_LOCAL_AUTH_TOKEN:-}" && -z "${LUNARDEPTH_LOCAL_AUTH_TOKEN_FILE:-}" ]] || die "local credential must not reach the bridge"
  export HOME=/home/lunardepth-bridge
  local token_dir="$HOME/.local/state/lunarwing"
  if [[ -n "${AGENT_AUTH_TOKEN:-}" ]]; then
    install -d -m 0700 "$token_dir"
    local token_tmp="$token_dir/.agent_auth_token.$$"
    (umask 0177; printf '%s' "$AGENT_AUTH_TOKEN" >"$token_tmp")
    chmod 0400 "$token_tmp"
    mv "$token_tmp" "$token_dir/agent_auth_token"
    unset AGENT_AUTH_TOKEN
    export AGENT_AUTH_TOKEN_FILE="$token_dir/agent_auth_token"
  fi
  [[ -n "${AGENT_AUTH_TOKEN_FILE:-}" ]] || die "split-bridge requires the hub credential"
  export LUNARDEPTH_TASK_UID=10005 LUNARDEPTH_TASK_GID=10005
  export WS_STATE_FILE="${WS_STATE_FILE:-/tmp/lunardepth_ws_state.json}"
  export LUNARDEPTH_MODE=websocket LUNARDEPTH_VERSION=lunardepth-worker-1.0.0

  local attempt=0
  while [[ ! -S "$RUNNER_SOCKET" ]]; do
    attempt=$((attempt + 1))
    [[ "$attempt" -lt 90 ]] || die "task runner did not create its socket"
    sleep 1
  done
  /usr/bin/python3 "$APP_DIR/health_server.py" --port "${HEALTH_PORT:-8443}" &
  HEALTH_PID=$!
  /usr/local/bin/node "$APP_DIR/scripts/lunarwing_bridge.js" &
  BRIDGE_PID=$!
  trap stop_bridge EXIT INT TERM
  log "LunarDepth non-root bridge started"
  wait -n "$BRIDGE_PID" "$HEALTH_PID" || true
  die "LunarDepth bridge role stopped unexpectedly"
}

case "$ROLE" in
  split-task) run_task ;;
  split-bridge) run_bridge ;;
  *) die "unknown LUNARWING_CONTAINER_ROLE '$ROLE'" ;;
esac
