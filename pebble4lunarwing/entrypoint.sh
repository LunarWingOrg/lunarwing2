#!/usr/bin/env bash
set -euo pipefail

PEBBLE_MODE="${PEBBLE_MODE:-websocket}"

if [[ "${LUNARWING_ROOTFUL_WORKSPACE:-}" == 1 ]]; then
  [[ "${LUNARWING_WORKSPACE_ID:-}" =~ ^[0-9]+:[0-9]+$ ]] \
    || { echo '[entrypoint] rootful workspace identity is required' >&2; exit 1; }
  [[ "$(stat -c '%d:%i' -- "${WORKSPACE_ROOT:-/workspace}")" == "$LUNARWING_WORKSPACE_ID" ]] \
    || { echo '[entrypoint] workspace mount changed identity; refusing redirected workspace' >&2; exit 1; }
fi

if [[ "${LUNARWING_CONTAINER_ROLE:-}" == "split-bridge" ]]; then
  exec /usr/local/bin/entrypoint-split.sh "$@"
fi
if [[ "${LUNARWING_CONTAINER_ROLE:-}" == "split-task" ]]; then
  SPLIT_TASK_UID="${PEBBLE_TASK_UID:-10004}"
  [[ "$(id -u)" == "$SPLIT_TASK_UID" ]] || { echo "[entrypoint] split-task must run as uid $SPLIT_TASK_UID" >&2; exit 1; }
  if [[ -n "${AGENT_AUTH_TOKEN:-}" || -n "${AGENT_AUTH_TOKEN_FILE:-}" ]]; then
    echo "[entrypoint] hub credential variables must never reach the task container" >&2
    exit 1
  fi
  TASK_RUNNER_SOCKET="${PEBBLE_TASK_RUNNER_SOCKET:-/run/lunarwing-shared/runner/task-runner.sock}"
  export LUNARWING_TASK_RUNNER_REMOTE=1
  export LUNARWING_TASK_RUNNER_SOCKET="$TASK_RUNNER_SOCKET"
  export LUNARWING_TASK_RUNNER_STATE_FILE="${PEBBLE_TASK_RUNNER_STATE_FILE:-$(dirname "$TASK_RUNNER_SOCKET")/task-runner-state.json}"
  export LUNARWING_TOOL_PROXY_DIR="${LUNARWING_TOOL_PROXY_DIR:-/run/lunarwing-shared/tool-proxy}"
  export HOME=/home/pebbletask GOPATH=/home/pebbletask/go GOCACHE=/home/pebbletask/.cache/go-build
  export CARGO_HOME=/home/pebbletask/.cargo
  unset AGENT_AUTH_TOKEN AGENT_AUTH_TOKEN_FILE PEBBLE_TASK_UID PEBBLE_TASK_GID
  umask 0007
fi

# WI#400 LD-8: the bridge must run as container root (image USER 0:0). On
# rootless podman keep-id a USER-less image would land the whole entrypoint
# on the task uid — the same uid every task drops to — and the credential
# split would be a no-op (task-owned token file, readable bridge environ).
# Refuse that shape instead of running without the boundary. The explicit
# `inherit` pair is the one sanctioned non-root shape: the rootful
# tenant-uid launch (WI#422) runs the whole container as the tenant and
# mt-admin says so; there is no split on that lane by design.
if [ "$PEBBLE_MODE" = "websocket" ] && [ -n "${PEBBLE_TASK_UID:-}" ] && [ "${PEBBLE_TASK_UID}" != "inherit" ] && [ "$(id -u)" != "0" ]; then
  echo "[entrypoint] refusing to start: running as uid $(id -u); the bridge must be container root (image USER 0:0) so tasks can drop to ${PEBBLE_TASK_UID}" >&2
  exit 1
fi

# WI#400: move the hub-handshake credential out of the process environment
# before any long-lived process is spawned. It lands in a root-only file
# the bridge reads at startup (AGENT_AUTH_TOKEN_FILE); the bridge and every
# task spawned from here carry an environment without it. PID 1 execs into
# the bridge, so /proc/1/environ is the bridge's token-free environment,
# and the file is 0400 root while every task runs as PEBBLE_TASK_UID, so a
# task-side read of either is EACCES — see docs/ops/WORKER-CONTAINERS.md.
# With the `inherit` pair (the rootful tenant-uid launch, WI#422) there is
# no split and no root to own a file: the credential stays in the
# environment, the pre-WI#400 shape on that lane.
LUNARWING_TOKEN_DIR="${LUNARWING_TOKEN_DIR:-/run/lunarwing}"
if [ -n "${AGENT_AUTH_TOKEN:-}" ] && [ "${PEBBLE_TASK_UID:-}" = "inherit" ]; then
  echo "[entrypoint] PEBBLE_TASK_UID=inherit: no privilege split on this lane (uid $(id -u)); the hub credential stays in the environment"
elif [ -n "${AGENT_AUTH_TOKEN:-}" ]; then
  install -d -m 0700 "$LUNARWING_TOKEN_DIR"
  _token_tmp="$LUNARWING_TOKEN_DIR/.agent_auth_token.$$"
  ( umask 0177; printf '%s' "$AGENT_AUTH_TOKEN" > "$_token_tmp" )
  chmod 0400 "$_token_tmp"
  mv "$_token_tmp" "$LUNARWING_TOKEN_DIR/agent_auth_token"
  unset AGENT_AUTH_TOKEN
  export AGENT_AUTH_TOKEN_FILE="$LUNARWING_TOKEN_DIR/agent_auth_token"
fi

case "$PEBBLE_MODE" in
  websocket)
    if [[ "${LUNARWING_CONTAINER_ROLE:-}" == "split-task" ]]; then
      echo "[entrypoint] starting pebble4lunarwing task runner as uid $(id -u)"
      exec pebble4lunarwing task-runner "$TASK_RUNNER_SOCKET"
    fi
    if [[ -n "${PEBBLE_BRIDGE_UID:-}" ]]; then
      echo '[entrypoint] websocket images require LUNARWING_CONTAINER_ROLE=split-bridge or split-task' >&2
      exit 1
    fi
    echo "[entrypoint] starting pebble4lunarwing bridge (WebSocket mode)"
    exec pebble4lunarwing
    ;;
  cli)
    echo "[entrypoint] running pebble in CLI mode"
    # WI#373: the prompt runs untrusted agent code — never inherit the
    # bridge's hub-handshake credential (nor, WI#400, the credential-file
    # pointer). WI#400: run under the task uid, separate from the bridge.
    if [ "$(id -u)" = "0" ] && [ -n "${PEBBLE_TASK_UID:-}" ] && [ -n "${PEBBLE_TASK_GID:-}" ]; then
      # setpriv(1) preserves capabilities across the uid change unless the
      # ambient/inheritable sets are cleared — both flags are mandatory
      # (run-98 FY-1 prototype finding). --no-new-privs is mandatory too
      # (run-164 KW-2): without it a setuid or file-capability binary in the
      # image would let task code regain privilege.
      exec setpriv --reuid="$PEBBLE_TASK_UID" --regid="$PEBBLE_TASK_GID" --clear-groups \
        --ambient-caps=-all --inh-caps=-all --no-new-privs -- \
        env -u AGENT_AUTH_TOKEN -u AGENT_AUTH_TOKEN_FILE \
        pebble --permission-mode "${PEBBLE_PERMISSION_MODE:-danger-full-access}" \
                  --model "${PEBBLE_MODEL:-openai/gpt-5.2}" \
                  --output-format "${PEBBLE_OUTPUT_FORMAT:-text}" \
                  "$@"
    fi
    exec env -u AGENT_AUTH_TOKEN -u AGENT_AUTH_TOKEN_FILE \
      pebble --permission-mode "${PEBBLE_PERMISSION_MODE:-danger-full-access}" \
                --model "${PEBBLE_MODEL:-openai/gpt-5.2}" \
                --output-format "${PEBBLE_OUTPUT_FORMAT:-text}" \
                "$@"
    ;;
  *)
    echo "[entrypoint] unknown PEBBLE_MODE: $PEBBLE_MODE (expected websocket or cli)" >&2
    exit 1
    ;;
esac
