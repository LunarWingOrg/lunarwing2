#!/bin/bash
# entrypoint.sh — LunarWing OpenCode 2 Worker startup script (root launcher)
#
# WI#400 (WI#449 migration): the container starts as root. The hub-handshake
# credential is moved out of the process environment into a root-only file
# before any long-lived process is spawned; the WebSocket bridge runs as root
# and reads it from that file, while the opencode serve process (which runs
# ALL task code) and every one-shot process run as the separate task uid
# (opencode2, 10003) via setpriv — so /proc/1/environ and the bridge's
# environ give task code EACCES. Task-side setup (the service.json reset,
# git config, SSH copies, config materialization) runs as the task uid in
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
OPENCODE2_TASK_UID="${OPENCODE2_TASK_UID:-10003}"
OPENCODE2_TASK_GID="${OPENCODE2_TASK_GID:-10003}"
# The tool-proxy relay reads these to hand per-task sockets to the task
# identity; keep the launcher and the bridge on the same identity even when
# the image ENV is absent (host-side runs).
export OPENCODE2_TASK_UID OPENCODE2_TASK_GID

# WI#400 LP-17: a zero (or non-numeric) id would make the setpriv drop a
# no-op while every caller keeps treating the split as active — refuse it.
if [[ ! "$OPENCODE2_TASK_UID" =~ ^[1-9][0-9]*$ || ! "$OPENCODE2_TASK_GID" =~ ^[1-9][0-9]*$ ]]; then
  die "OPENCODE2_TASK_UID/OPENCODE2_TASK_GID must be non-zero numeric ids (a zero id would make the privilege drop a no-op)"
fi

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
# --bounding-set=-all (run-170 LP-7) is belt and braces: the dropped task's
# bounding set is empty, so nothing it spawns can re-acquire a capability.
# It needs CAP_SETPCAP in the still-root setpriv, which the container cap
# trim carries.
SETPRIV_FLAGS=(--reuid="$OPENCODE2_TASK_UID" --regid="$OPENCODE2_TASK_GID" --clear-groups --ambient-caps=-all --inh-caps=-all --no-new-privs --bounding-set=-all)

# as_task CMD [ARGS...] — run a command as the task uid with the task home;
# direct exec when already unprivileged (host-side dev runs).
as_task() {
  if [ "$(id -u)" = "0" ]; then
    setpriv "${SETPRIV_FLAGS[@]}" env HOME=/home/opencode2 "$@"
  else
    env HOME="${HOME:-/home/opencode2}" "$@"
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

# ── task-side setup (service.json reset, git/SSH, config materialization) ────
WORKSPACE_ROOT="$WORKSPACE_ROOT" as_task /app/entrypoint-task-setup.sh

# opencode v2 only auto-discovers opencode.json/opencode.jsonc (project dirs,
# .opencode/, the global config dir) and never reads .opencode/opencode2.json,
# so without this the server ignores the lane config and falls back to its
# default provider. OPENCODE_CONFIG is the CLI's explicit config-file path; an
# operator-set value wins. Exported here so the serve process spawned below
# inherits it (the task setup materializes the file and runs the placeholder
# refusal against the effective path).
if [ -f /app/config/opencode2.json ]; then
  : "${OPENCODE_CONFIG:=$WORKSPACE_ROOT/.opencode/opencode2.json}"
  export OPENCODE_CONFIG
  log "OPENCODE_CONFIG=$OPENCODE_CONFIG"
fi

# ── always start the health server in the background (task uid: no secrets) ──
log "Starting health server on port $HEALTH_PORT"
export WS_STATE_FILE
export CODEX_MODE="$MODE"
export CODEX_VERSION="opencode2-worker-1.0.0"
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
        env HOME=/home/opencode2 -u AGENT_AUTH_TOKEN -u AGENT_AUTH_TOKEN_FILE \
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
        env HOME=/home/opencode2 -u AGENT_AUTH_TOKEN -u AGENT_AUTH_TOKEN_FILE \
        "$OPENCODE_BIN" acp "$@"
    fi
    exec env -u AGENT_AUTH_TOKEN -u AGENT_AUTH_TOKEN_FILE "$OPENCODE_BIN" acp "$@"
    ;;

  websocket)
    log "Mode: WebSocket — starting opencode headless server"

    # v2 serve mandates Basic auth: without an explicit password it mints a
    # random one (persisted in service.json) that the separate bridge process
    # can never learn — every bridge call then 401s, surfacing downstream as
    # SDK ClientError("UnsupportedContentType") at task ingestion (WI#167).
    # Serve ADOPTS OPENCODE_PASSWORD (or legacy OPENCODE_SERVER_PASSWORD); the
    # bridge reads OPENCODE_SERVER_PASSWORD for its Authorization header.
    # Export one shared per-boot secret so server and bridge always agree.
    : "${OPENCODE_PASSWORD:=${OPENCODE_SERVER_PASSWORD:-$(head -c 32 /dev/urandom | od -A n -t x1 | tr -d ' \n')}}"
    export OPENCODE_PASSWORD OPENCODE_SERVER_PASSWORD="$OPENCODE_PASSWORD"

    cd "$WORKSPACE_ROOT"
    # WI#373/WI#400: the serve process runs all task code (every bash tool
    # child inherits its env), so it runs as the task uid without the
    # bridge's hub-handshake credential or its file pointer. The bridge
    # (root) reads the credential from the root-only file; the server never
    # needs it. OPENCODE_PASSWORD stays — the server itself enforces it.
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
