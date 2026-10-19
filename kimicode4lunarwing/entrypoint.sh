#!/bin/bash
# entrypoint.sh — LunarWing Kimi Code Worker startup script (root launcher)
#
# WI#400: the container starts as root. The hub-handshake credential is
# moved out of the process environment into a root-only file before any
# long-lived process is spawned; the WebSocket bridge (which must be able
# to drop privileges per task) runs as root and reads it from that file,
# while every task process runs as the separate task uid (kimi, 10002) via
# setpriv — so /proc/1/environ and the bridge's environ give task code
# EACCES. Task-side setup (git config, SSH copies, config materialization)
# runs as the task uid in entrypoint-task-setup.sh so task-owned files
# keep the right ownership. PID 1 (tini) keeps the original container env
# (/proc/1/environ is captured at exec time), but it runs as root, so a
# task-uid read is EACCES — see docs/ops/WORKER-CONTAINERS.md.
set -euo pipefail

log()  { printf '[%s] %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }
die()  { log "ERROR: $*" >&2; exit 1; }

MODE="${KIMI_MODE:-websocket}"
HEALTH_PORT="${HEALTH_PORT:-8443}"
WS_STATE_FILE="${WS_STATE_FILE:-/tmp/lunarwing_ws_state.json}"
WORKSPACE_ROOT="${WORKSPACE_ROOT:-/workspace}"
KIMI_CODE_HOME="${KIMI_CODE_HOME:-/home/kimi/.kimi-code}"
KIMI_TASK_UID="${KIMI_TASK_UID:-10002}"
KIMI_TASK_GID="${KIMI_TASK_GID:-10002}"
# The bridge's executor reads these to wrap every task spawn (WI#400); keep
# the launcher and the bridge on the same identity even when the image ENV
# is absent (host-side runs).
export KIMI_TASK_UID KIMI_TASK_GID

# ── WI#400 credential handoff (before anything long-lived is spawned) ────────
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

# setpriv flags: the cap-clearing pair is mandatory — setpriv(1) preserves
# capabilities across the uid change otherwise, and the task could regain
# the bridge uid (run-98 FY-1 prototype finding). --no-new-privs is
# mandatory too (run-164 KW-2): without it a setuid or file-capability
# binary in the image would let task code regain privilege.
SETPRIV_FLAGS=(--reuid="$KIMI_TASK_UID" --regid="$KIMI_TASK_GID" --clear-groups --ambient-caps=-all --inh-caps=-all --no-new-privs)

# as_task CMD [ARGS...] — run a command as the task uid with the task home;
# direct exec when already unprivileged (host-side dev runs).
as_task() {
  if [ "$(id -u)" = "0" ]; then
    setpriv "${SETPRIV_FLAGS[@]}" env HOME=/home/kimi "$@"
  else
    env HOME="${HOME:-/home/kimi}" "$@"
  fi
}

# ── parse CLI args ─────────────────────────────────────────────────────────────
while [ $# -gt 0 ]; do
  case "$1" in
    --mode)       MODE="$2";              shift 2 ;;
    --port)       HEALTH_PORT="$2";       shift 2 ;;
    --)           shift; break ;;
    *)            break ;;
  esac
done

# ── task-side setup (config materialization, git/SSH, MCP registration) ──────
KIMI_CODE_HOME="$KIMI_CODE_HOME" as_task /app/entrypoint-task-setup.sh

# The bridge's per-task env must carry the LLM key fallback (deliberately
# NOT stripped, WI#373); the setup process's export dies with it.
export KIMI_API_KEY="${KIMI_API_KEY:-${TENSORZERO_API_KEY:-}}"

# ── always start the health server in the background (task uid: no secrets) ──
log "Starting health server on port $HEALTH_PORT"
export WS_STATE_FILE
export KIMI_MODE="$MODE"
export KIMI_VERSION="kimi-worker-1.0.0"
as_task python3 /app/health_server.py --port "$HEALTH_PORT" &
HEALTH_PID=$!

sleep 1

cleanup() {
  log "Shutting down..."
  [ -z "${BRIDGE_PID:-}" ] || kill "$BRIDGE_PID" 2>/dev/null || true
  kill "$HEALTH_PID" 2>/dev/null || true
  [ -z "${BRIDGE_PID:-}" ] || wait "$BRIDGE_PID" 2>/dev/null || true
  wait "$HEALTH_PID" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# ── mode dispatch ──────────────────────────────────────────────────────────────
case "$MODE" in

  cli)
    [ $# -gt 0 ] || die "CLI mode requires prompt arguments"
    log "Mode: CLI — running kimi in print/stream-json mode"
    cd "$WORKSPACE_ROOT"
    # WI#206: never let an inherited environment disable the dangerous-command
    # guard — print mode (-p) runs the auto policy where the guard is skipped
    # by upstream, and the PreToolUse hook enforces fail-closed denials. The
    # mode reminder stays off: it is prompt noise for a fixed-mode worker.
    export KIMI_CODE_PERMISSION_MODE_REMINDER="${KIMI_CODE_PERMISSION_MODE_REMINDER:-0}"
    # WI#411: kimi-code 2.2.0's KIMI_CODE_TRUST_WORKSPACE auto-spawns a repo's
    # .mcp.json servers before any model turn, outside the hook — never inherit it.
    unset KIMI_CODE_DANGEROUS_COMMAND_GUARD KIMI_CODE_TRUST_WORKSPACE
    # WI#373/WI#400: the prompt runs untrusted agent code — never inherit the
    # bridge's hub-handshake credential or its file pointer; run as the task uid.
    if [ "$(id -u)" = "0" ]; then
      exec setpriv "${SETPRIV_FLAGS[@]}" \
        env HOME=/home/kimi -u AGENT_AUTH_TOKEN -u AGENT_AUTH_TOKEN_FILE \
        kimi -p "$*" --output-format stream-json
    fi
    exec env -u AGENT_AUTH_TOKEN -u AGENT_AUTH_TOKEN_FILE \
      kimi -p "$*" --output-format stream-json
    ;;

  websocket)
    log "Mode: WebSocket — starting bridge"

    cd "$WORKSPACE_ROOT"
    export KIMI_CODE_HOME
    export WS_STATE_FILE
    export WORKSPACE_ROOT
    export KIMI_MODE
    # WI#206: never let an inherited environment disable the dangerous-command
    # guard — print mode (-p) runs the auto policy where the guard is skipped
    # by upstream, and the PreToolUse hook enforces fail-closed denials. The
    # mode reminder stays off: it is prompt noise for a fixed-mode worker.
    export KIMI_CODE_PERMISSION_MODE_REMINDER="${KIMI_CODE_PERMISSION_MODE_REMINDER:-0}"
    # WI#411: kimi-code 2.2.0's KIMI_CODE_TRUST_WORKSPACE auto-spawns a repo's
    # .mcp.json servers before any model turn, outside the hook — never inherit it.
    unset KIMI_CODE_DANGEROUS_COMMAND_GUARD KIMI_CODE_TRUST_WORKSPACE

    node /app/scripts/lunarwing_bridge.js &
    BRIDGE_PID=$!

    log "Kimi Code worker bridge started (pid $BRIDGE_PID)"
    wait "$BRIDGE_PID"
    ;;

  *)
    die "Unknown mode '$MODE'. Use --mode websocket or --mode cli"
    ;;

esac
