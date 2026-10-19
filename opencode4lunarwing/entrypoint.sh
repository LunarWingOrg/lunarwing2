#!/bin/bash
# entrypoint.sh — LunarWing OpenCode Worker startup script (root launcher)
#
# WI#400: the container starts as root. The hub-handshake credential is
# moved out of the process environment into a root-only file before any
# long-lived process is spawned; the WebSocket bridge runs as root and
# reads it from that file, while the opencode serve process (which runs
# ALL task code) and every one-shot process run as the separate task uid
# (opencode, 10001) via setpriv — so /proc/1/environ and the bridge's
# environ give task code EACCES. Task-side setup (git config, SSH copies,
# config materialization) runs as the task uid in
# entrypoint-task-setup.sh. PID 1 (tini) keeps the original container env
# (/proc/1/environ is captured at exec time), but it runs as root, so a
# task-uid read is EACCES — see docs/ops/WORKER-CONTAINERS.md.
set -euo pipefail

log()  { printf '[%s] %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }
die()  { log "ERROR: $*" >&2; exit 1; }

MODE="${OPENCODE_MODE:-websocket}"
HEALTH_PORT="${HEALTH_PORT:-8443}"
OPENCODE_SERVE_PORT="${OPENCODE_SERVE_PORT:-4096}"
OPENCODE_SERVE_HOST="${OPENCODE_SERVE_HOST:-127.0.0.1}"
WS_ROLE="${WS_ROLE:-server}"
WS_STATE_FILE="${WS_STATE_FILE:-/tmp/lunarwing_ws_state.json}"
OPENCODE_BIN="${OPENCODE_BIN:-/usr/local/bin/opencode}"
WORKSPACE_ROOT="${WORKSPACE_ROOT:-/workspace}"
OPENCODE_TASK_UID="${OPENCODE_TASK_UID:-10001}"
OPENCODE_TASK_GID="${OPENCODE_TASK_GID:-10001}"
# The tool-proxy relay reads these to hand per-task sockets to the task
# identity; keep the launcher and the bridge on the same identity even when
# the image ENV is absent (host-side runs).
export OPENCODE_TASK_UID OPENCODE_TASK_GID

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
SETPRIV_FLAGS=(--reuid="$OPENCODE_TASK_UID" --regid="$OPENCODE_TASK_GID" --clear-groups --ambient-caps=-all --inh-caps=-all --no-new-privs)

# as_task CMD [ARGS...] — run a command as the task uid with the task home;
# direct exec when already unprivileged (host-side dev runs).
as_task() {
  if [ "$(id -u)" = "0" ]; then
    setpriv "${SETPRIV_FLAGS[@]}" env HOME=/home/opencode "$@"
  else
    env HOME="${HOME:-/home/opencode}" "$@"
  fi
}

# ── parse CLI args ─────────────────────────────────────────────────────────────
while [ $# -gt 0 ]; do
  case "$1" in
    --mode)       MODE="$2";              shift 2 ;;
    --port)       HEALTH_PORT="$2";       shift 2 ;;
    --serve-port) OPENCODE_SERVE_PORT="$2"; shift 2 ;;
    --)           shift; break ;;
    *)            break ;;
  esac
done

# ── task-side setup (git/SSH, config materialization, MCP integrations) ──────
WORKSPACE_ROOT="$WORKSPACE_ROOT" as_task /app/entrypoint-task-setup.sh

# ── always start the health server in the background (task uid: no secrets) ──
log "Starting health server on port $HEALTH_PORT"
export WS_STATE_FILE
export CODEX_MODE="$MODE"
export CODEX_VERSION="opencode-worker-1.0.0"
as_task python3 /app/health_server.py --port "$HEALTH_PORT" &
HEALTH_PID=$!

sleep 1

cleanup() {
  log "Shutting down..."
  kill "$OPENCODE_PID" 2>/dev/null || true
  kill "$HEALTH_PID" 2>/dev/null || true
  wait "$OPENCODE_PID" 2>/dev/null || true
  wait "$HEALTH_PID" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# ── mode dispatch ──────────────────────────────────────────────────────────────
case "$MODE" in

  cli)
    log "Mode: CLI — running opencode with args: $*"
    cd "$WORKSPACE_ROOT"
    # WI#373/WI#400: the prompt runs untrusted agent code — never inherit the
    # bridge's hub-handshake credential or its file pointer; run as the task uid.
    if [ "$(id -u)" = "0" ]; then
      exec setpriv "${SETPRIV_FLAGS[@]}" \
        env HOME=/home/opencode -u AGENT_AUTH_TOKEN -u AGENT_AUTH_TOKEN_FILE \
        "$OPENCODE_BIN" run "$@"
    fi
    exec env -u AGENT_AUTH_TOKEN -u AGENT_AUTH_TOKEN_FILE "$OPENCODE_BIN" run "$@"
    ;;

  acp)
    log "Mode: ACP — launching opencode ACP bridge"
    cd "$WORKSPACE_ROOT"
    # WI#373: same one-shot task boundary as cli mode.
    if [ "$(id -u)" = "0" ]; then
      exec setpriv "${SETPRIV_FLAGS[@]}" \
        env HOME=/home/opencode -u AGENT_AUTH_TOKEN -u AGENT_AUTH_TOKEN_FILE \
        "$OPENCODE_BIN" acp "$@"
    fi
    exec env -u AGENT_AUTH_TOKEN -u AGENT_AUTH_TOKEN_FILE "$OPENCODE_BIN" acp "$@"
    ;;

  websocket)
    log "Mode: WebSocket — starting opencode headless server"

    cd "$WORKSPACE_ROOT"
    # WI#373/WI#400: the serve process runs all task code (every bash tool
    # child inherits its env), so it runs as the task uid without the
    # bridge's hub-handshake credential or its file pointer. The bridge
    # (root) reads the credential from the root-only file; the server never
    # needs it.
    as_task env -u AGENT_AUTH_TOKEN -u AGENT_AUTH_TOKEN_FILE "$OPENCODE_BIN" serve \
      --port "$OPENCODE_SERVE_PORT" \
      --hostname "$OPENCODE_SERVE_HOST" &
    OPENCODE_PID=$!

    log "Waiting for opencode server on port $OPENCODE_SERVE_PORT..."
    RETRIES=0
    MAX_RETRIES=30
    # Ready = the server speaks HTTP on this port. Use `curl -s` (not `-sf`)
    # because some OpenCode releases return 404 on `/`; any HTTP response means
    # ready, while a refused connection keeps the loop waiting.
    until curl -s --max-time 2 -o /dev/null "http://$OPENCODE_SERVE_HOST:$OPENCODE_SERVE_PORT/" 2>/dev/null; do
      RETRIES=$((RETRIES + 1))
      if [ $RETRIES -ge $MAX_RETRIES ]; then
        die "opencode server failed to start after ${MAX_RETRIES}s"
      fi
      if ! kill -0 "$OPENCODE_PID" 2>/dev/null; then
        die "opencode server process exited unexpectedly"
      fi
      sleep 1
    done
    log "opencode server ready on http://$OPENCODE_SERVE_HOST:$OPENCODE_SERVE_PORT"

    log "Starting LunarWing bridge — role: $WS_ROLE"
    export OPENCODE_SERVE_PORT
    export OPENCODE_SERVE_HOST
    export WS_ROLE
    export WS_STATE_FILE
    export WORKSPACE_ROOT
    # WI#400 LD-5: the bridge runs as root (HOME=/root) while serve runs with
    # HOME=/home/opencode, so the serve-log enrichment (WI#374) must not fall
    # back to the bridge's own homedir — point it at the serve process's log.
    export OPENCODE_SERVE_LOG="${OPENCODE_SERVE_LOG-${XDG_DATA_HOME:-/home/opencode/.local/share}/opencode/log/opencode.log}"

    # WI#400 (run-170 LP-1): the bridge is container root, and bun loads
    # bunfig.toml preloads and .env from its working directory. Never start
    # it from the task-writable workspace: a task-planted bunfig.toml would
    # run as root at the next container start and read the hub credential.
    # /app is root-owned (LD-2); the bridge takes WORKSPACE_ROOT from env.
    cd /app
    exec bun run /app/scripts/lunarwing_bridge.ts
    ;;

  *)
    die "Unknown mode '$MODE'. Use --mode websocket, --mode cli, or --mode acp"
    ;;

esac
