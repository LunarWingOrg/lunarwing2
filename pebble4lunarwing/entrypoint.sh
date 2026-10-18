#!/usr/bin/env bash
set -euo pipefail

PEBBLE_MODE="${PEBBLE_MODE:-websocket}"

# WI#400: move the hub-handshake credential out of the process environment
# before any long-lived process is spawned. It lands in a root-only file
# the bridge reads at startup (AGENT_AUTH_TOKEN_FILE); the bridge and every
# task spawned from here carry an environment without it. PID 1 keeps the
# original container env (/proc/1/environ is captured at exec time), but
# PID 1 runs as root while every task runs as PEBBLE_TASK_UID, so a
# task-side read is EACCES — see docs/ops/WORKER-CONTAINERS.md.
LUNARWING_TOKEN_DIR="${LUNARWING_TOKEN_DIR:-/run/lunarwing}"
if [ -n "${AGENT_AUTH_TOKEN:-}" ]; then
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
      # (run-98 FY-1 prototype finding).
      exec setpriv --reuid="$PEBBLE_TASK_UID" --regid="$PEBBLE_TASK_GID" --clear-groups \
        --ambient-caps=-all --inh-caps=-all -- \
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
