#!/usr/bin/env bash
set -euo pipefail

log() { printf '[%s] %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }
die() { log "ERROR: $*" >&2; exit 1; }

[[ "${LUNARWING_CONTAINER_ROLE:-}" == "split-bridge" ]] || die "this launcher only accepts split-bridge"
TASK_UID="${PEBBLE_TASK_UID:-10004}"
TASK_GID="${PEBBLE_TASK_GID:-10004}"
BRIDGE_UID="${PEBBLE_BRIDGE_UID:?PEBBLE_BRIDGE_UID is required for the split launcher}"
RUNNER_SOCKET="${PEBBLE_TASK_RUNNER_SOCKET:-/run/lunarwing-shared/runner/task-runner.sock}"
RUNNER_STATE_FILE="${PEBBLE_TASK_RUNNER_STATE_FILE:-$(dirname "$RUNNER_SOCKET")/task-runner-state.json}"
BRIDGE_BIN="${LUNARWING_BRIDGE_BIN:-/usr/local/bin/pebble4lunarwing}"

if [[ ! "$TASK_UID" =~ ^[1-9][0-9]*$ || ! "$TASK_GID" =~ ^[1-9][0-9]*$ || ! "$BRIDGE_UID" =~ ^[1-9][0-9]*$ ]]; then
  die "split bridge/task ids must be non-zero numeric ids"
fi
[[ "$TASK_UID" != "$BRIDGE_UID" ]] || die "split bridge and task uids must differ"
[[ "$(id -u)" == "$BRIDGE_UID" ]] || die "split-bridge must run as uid $BRIDGE_UID, not $(id -u)"

umask 0007
export LUNARWING_TASK_RUNNER_REMOTE=1
export LUNARWING_TASK_RUNNER_SOCKET="$RUNNER_SOCKET"
export LUNARWING_TASK_RUNNER_STATE_FILE="$RUNNER_STATE_FILE"
export LUNARWING_TOOL_PROXY_DIR="${LUNARWING_TOOL_PROXY_DIR:-/run/lunarwing-shared/tool-proxy}"
export LUNARWING_TASK_UID="$TASK_UID" LUNARWING_TASK_GID="$TASK_GID"
export PEBBLE_MODE=websocket
unset PEBBLE_TASK_UID PEBBLE_TASK_GID

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
  [[ "$attempt" -lt 30 ]] || die "task runner did not create $RUNNER_SOCKET"
  sleep 1
done

log "Pebble non-root bridge started as uid $(id -u)"
exec "$BRIDGE_BIN"
