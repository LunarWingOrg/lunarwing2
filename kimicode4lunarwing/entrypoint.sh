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

# WI#454/ME-1: the entrypoint runs as container root, and every command it
# resolves would otherwise walk an inherited PATH whose first entries are
# directories the task uid owns — a planted file there runs as root. Pin
# the root PATH to the image's root-owned directories for the whole
# launcher (the drop below re-points the task env at the task toolchain).
export PATH=/usr/local/cargo/bin:/usr/local/go/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

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

# WI#400 LP-17: a zero (or non-numeric) id would make the setpriv drop a
# no-op while every caller keeps treating the split as active — refuse it.
if [[ ! "$KIMI_TASK_UID" =~ ^[1-9][0-9]*$ || ! "$KIMI_TASK_GID" =~ ^[1-9][0-9]*$ ]]; then
  die "KIMI_TASK_UID/KIMI_TASK_GID must be non-zero numeric ids (a zero id would make the privilege drop a no-op)"
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
SETPRIV_FLAGS=(--reuid="$KIMI_TASK_UID" --regid="$KIMI_TASK_GID" --clear-groups --ambient-caps=-all --inh-caps=-all --no-new-privs)

# WI#454/ME-1: the task's toolchain directories are handed over at the drop,
# NOT baked into the image PATH (root resolves through that PATH too, and the
# task owns these directories).
TASK_PATH="/home/kimi/.cargo/bin:/usr/local/cargo/bin:/usr/local/go/bin:/home/kimi/go/bin:$PATH"

# as_task CMD [ARGS...] — run a command as the task uid with the task home;
# direct exec when already unprivileged (host-side dev runs).
as_task() {
  if [ "$(id -u)" = "0" ]; then
    setpriv "${SETPRIV_FLAGS[@]}" env HOME=/home/kimi PATH="$TASK_PATH" "$@"
  else
    env HOME="${HOME:-/home/kimi}" PATH="$TASK_PATH" "$@"
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

# NC-7: compute runner paths early so the health server (started next) can
# see them. The runner socket dir is only created when the credential handoff
# is active (websocket mode below), but the health server needs the env vars
# at its start to include runner liveness in /ready.
RUNNER_DIR="${KIMI_TASK_RUNNER_DIR:-/run/lunarwing-runner}"
RUNNER_SOCKET="${KIMI_TASK_RUNNER_SOCKET:-$RUNNER_DIR/task-runner.sock}"
RUNNER_STATE_FILE="${KIMI_TASK_RUNNER_STATE_FILE:-$RUNNER_DIR/task-runner-state.json}"
if [ -n "${AGENT_AUTH_TOKEN_FILE:-}" ] && [ "${KIMI_TASK_RUNNER:-}" != "skip" ]; then
  export LUNARWING_TASK_RUNNER_SOCKET="$RUNNER_SOCKET"
  export LUNARWING_TASK_RUNNER_STATE_FILE="$RUNNER_STATE_FILE"
fi

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
        env -u AGENT_AUTH_TOKEN -u AGENT_AUTH_TOKEN_FILE HOME=/home/kimi PATH="$TASK_PATH" \
        kimi -p "$*" --output-format stream-json
    fi
    exec env -u AGENT_AUTH_TOKEN -u AGENT_AUTH_TOKEN_FILE HOME="${HOME:-/home/kimi}" PATH="$TASK_PATH" \
      kimi -p "$*" --output-format stream-json
    ;;

  websocket)
    log "Mode: WebSocket — starting bridge"

    # WI#454: start the task-runner daemon as the task uid BEFORE the bridge.
    # The runner is the task-container half of the two-container non-root
    # split: it executes every task under the task uid and never sees the
    # hub-handshake credential (the token file is 0400 root-only and the
    # token var was unset above). The bridge (still root in this image) then
    # delegates each task to it over the socket instead of spawning the CLI
    # itself. KIMI_TASK_RUNNER=skip (host-side dev runs without a runner)
    # keeps the bridge on local execution. The socket directory is SEPARATE
    # from LUNARWING_TOKEN_DIR: the token directory must stay root-only
    # 0700 (WI#400), and the runner must own its own directory to bind the
    # socket — root hands it over 0700 task-uid below.
    # NC-9: the PID file lives in the root-only LUNARWING_TOKEN_DIR, not the
    # task-uid-owned RUNNER_DIR — the task uid can rewrite files there, and
    # cleanup() runs kill as root on whatever it reads.
    RUNNER_PID_FILE="${KIMI_TASK_RUNNER_PID_FILE:-$LUNARWING_TOKEN_DIR/task-runner.pid}"
    if [ -n "${AGENT_AUTH_TOKEN_FILE:-}" ] && [ "${KIMI_TASK_RUNNER:-}" != "skip" ]; then
      install -d -m 0700 "$RUNNER_DIR" 2>/dev/null || true
      if [ "$(id -u)" = "0" ]; then
        chmod 0700 "$RUNNER_DIR" 2>/dev/null || true
        chown "$KIMI_TASK_UID:$KIMI_TASK_GID" "$RUNNER_DIR" 2>/dev/null || true
      fi
      # The runner runs as the task uid, so HOME and PATH must be the task's
      # own (as_task's exact env shape) — and the credential vars must never
      # be present in what it inherits. KIMI_TASK_UID/KIMI_TASK_GID are
      # UNSET for the runner: it already IS the task identity, and a second
      # setpriv drop inside executeTask would look for setpriv on the task
      # PATH and fail (or drop to the wrong identity). The identity stays
      # pinned for the bridge process itself.
      as_task env -u AGENT_AUTH_TOKEN -u AGENT_AUTH_TOKEN_FILE \
        -u KIMI_TASK_UID -u KIMI_TASK_GID \
        WORKSPACE_ROOT="$WORKSPACE_ROOT" \
        node /app/scripts/task_runner.js "$RUNNER_SOCKET" &
      RUNNER_PID=$!
      log "Task runner started as task uid on $RUNNER_SOCKET (pid $RUNNER_PID)"
      # NB-17 (!1861 run 195): the runner runs as the task uid, so any task
      # can kill it — after which every later task fails with a connection
      # error while /ready stays true. Supervise it: a dead runner is
      # restarted (up to 5 rapid-restart attempts, then the supervisor
      # signals this launcher, whose cleanup trap stops the bridge, the
      # runner and the health server so tini exits the container and the
      # orchestrator's restart policy takes over). The supervisor owns the
      # runner PID from here on: it records the LIVE pid in $RUNNER_PID_FILE
      # so the parent's cleanup() kills the current runner, not a stale
      # first-generation pid.
      (
        _attempts=0
        _current="$RUNNER_PID"
        while :; do
          # Watch the current runner generation.
          while kill -0 "$_current" 2>/dev/null; do sleep 2; done
          _attempts=$((_attempts + 1))
          if [ "$_attempts" -gt 5 ]; then
            log "Task runner failed 5 restart attempts — stopping the container for orchestrator restart"
            # NI-3 (!1868 run 198): this subshell forked BEFORE $BRIDGE_PID
            # existed, so it holds an empty copy — killing $BRIDGE_PID here
            # was a no-op and `exit 1` only ended the subshell, leaving a
            # root bridge accepting tasks it could never complete. Signal
            # the launcher itself instead: `$$` in a subshell is still the
            # parent's pid, so this runs the cleanup trap, which stops the
            # bridge, the runner and the health server.
            kill -TERM "$$" 2>/dev/null || true
            exit 1
          fi
          log "Task runner exited — restarting (attempt $_attempts/5)"
          as_task env -u AGENT_AUTH_TOKEN -u AGENT_AUTH_TOKEN_FILE \
            -u KIMI_TASK_UID -u KIMI_TASK_GID \
            WORKSPACE_ROOT="$WORKSPACE_ROOT" \
            node /app/scripts/task_runner.js "$RUNNER_SOCKET" &
          _current=$!
          echo "$_current" > "$RUNNER_PID_FILE"
          # Give the new runner a 30 s grace window; if it survives, reset
          # the attempt counter (NC-9: without this, the 6th runner death
          # over the container's lifetime — however far apart — exits).
          sleep 30
          if kill -0 "$_current" 2>/dev/null; then
            _attempts=0
          fi
        done
      ) &
      RUNNER_SUPERVISOR_PID=$!
      echo "$RUNNER_PID" > "$RUNNER_PID_FILE"
      # LUNARWING_TASK_RUNNER_SOCKET was already exported before the health
      # server started (NC-7); the bridge reads it from the inherited env.
      cleanup() {
        log "Shutting down..."
        [ -z "${RUNNER_SUPERVISOR_PID:-}" ] || kill "$RUNNER_SUPERVISOR_PID" 2>/dev/null || true
        [ -z "${BRIDGE_PID:-}" ] || kill "$BRIDGE_PID" 2>/dev/null || true
        # The supervisor may have restarted the runner: the LIVE pid lives in
        # the pid file (NB-17), not in the spawn-time $RUNNER_PID.
        RUNNER_LIVE_PID="$(cat "$RUNNER_PID_FILE" 2>/dev/null || echo "${RUNNER_PID:-}")"
        [ -z "$RUNNER_LIVE_PID" ] || kill "$RUNNER_LIVE_PID" 2>/dev/null || true
        kill "$HEALTH_PID" 2>/dev/null || true
        [ -z "${BRIDGE_PID:-}" ] || wait "$BRIDGE_PID" 2>/dev/null || true
        [ -z "$RUNNER_LIVE_PID" ] || wait "$RUNNER_LIVE_PID" 2>/dev/null || true
        wait "$HEALTH_PID" 2>/dev/null || true
      }
    fi

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
