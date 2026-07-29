#!/bin/bash
# entrypoint.sh — LunarWing Kimi Code Worker startup script
set -euo pipefail

log()  { printf '[%s] %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }
die()  { log "ERROR: $*" >&2; exit 1; }

MODE="${KIMI_MODE:-websocket}"
HEALTH_PORT="${HEALTH_PORT:-8443}"
WS_STATE_FILE="${WS_STATE_FILE:-/tmp/lunarwing_ws_state.json}"
FILE_UMASK="${FILE_UMASK:-0002}"
WORKSPACE_ROOT="${WORKSPACE_ROOT:-/workspace}"
KIMI_CODE_HOME="${KIMI_CODE_HOME:-/home/kimi/.kimi-code}"

umask "$FILE_UMASK"

# ── git / SSH credential setup ────────────────────────────────────────────────
if [ -n "${GIT_AUTHOR_NAME:-}" ] && ! git config --global user.name >/dev/null 2>&1; then
  git config --global user.name "$GIT_AUTHOR_NAME"
  log "Git user.name set to: $GIT_AUTHOR_NAME"
fi
if [ -n "${GIT_AUTHOR_EMAIL:-}" ] && ! git config --global user.email >/dev/null 2>&1; then
  git config --global user.email "$GIT_AUTHOR_EMAIL"
  log "Git user.email set to: $GIT_AUTHOR_EMAIL"
fi

if [ -n "${GITHUB_TOKEN:-}" ]; then
  git config --global credential.https://github.com.helper \
    '!f() { echo "protocol=https"; echo "host=github.com"; echo "username=x-access-token"; echo "password=${GITHUB_TOKEN}"; }; f'
  log "GitHub HTTPS credential helper configured"
elif [ -n "${GH_TOKEN:-}" ]; then
  git config --global credential.https://github.com.helper \
    '!f() { echo "protocol=https"; echo "host=github.com"; echo "username=x-access-token"; echo "password=${GH_TOKEN}"; }; f'
  log "GitHub HTTPS credential helper configured (via GH_TOKEN)"
fi

# Copy host SSH keys into writable ~/.ssh if mounted (read-only mount needs copy)
if [ -d /home/kimi/.host-ssh ] && [ "$(ls -A /home/kimi/.host-ssh 2>/dev/null)" ]; then
  mkdir -p /home/kimi/.ssh
  cp -a /home/kimi/.host-ssh/* /home/kimi/.ssh/ 2>/dev/null || true
  chmod 700 /home/kimi/.ssh 2>/dev/null || true
  chmod 600 /home/kimi/.ssh/id_* 2>/dev/null || true
  chmod 644 /home/kimi/.ssh/*.pub 2>/dev/null || true
  chmod 644 /home/kimi/.ssh/known_hosts 2>/dev/null || true
  log "SSH keys copied from host mount"
  if [ ! -f /home/kimi/.ssh/config ]; then
    printf 'Host *\n  StrictHostKeyChecking accept-new\n  UserKnownHostsFile /home/kimi/.ssh/known_hosts\n' \
      > /home/kimi/.ssh/config
    chmod 600 /home/kimi/.ssh/config
  fi
fi

# Copy host .gitconfig if mounted
if [ -f /home/kimi/.host-gitconfig ] && [ -s /home/kimi/.host-gitconfig ]; then
  cp /home/kimi/.host-gitconfig /home/kimi/.gitconfig 2>/dev/null || true
  log "Host .gitconfig copied into container"
fi

# ── parse CLI args ─────────────────────────────────────────────────────────────
while [ $# -gt 0 ]; do
  case "$1" in
    --mode)       MODE="$2";              shift 2 ;;
    --port)       HEALTH_PORT="$2";       shift 2 ;;
    --)           shift; break ;;
    *)            break ;;
  esac
done

# ── config setup ──────────────────────────────────────────────────────────────
# Materialize the Kimi Code config (config.toml) from the baked default unless
# a per-tenant KIMI_MODEL/KIMI_BASE_URL/KIMI_API_KEY override is present. Kimi
# Code CLI reads its config from $KIMI_CODE_HOME/config.toml.
mkdir -p "$KIMI_CODE_HOME"

if [ -f /app/config/config.toml ]; then
  if [ -n "${KIMI_MODEL:-}" ] || [ -n "${KIMI_BASE_URL:-}" ] || [ -n "${KIMI_API_KEY:-}" ]; then
    cp /app/config/config.toml "$KIMI_CODE_HOME/config.toml"
    if [ -n "${KIMI_MODEL:-}" ]; then
      # Override default_model in the config.toml.
      python3 - "$KIMI_CODE_HOME/config.toml" <<'PY'
import os, re, sys
path = sys.argv[1]
with open(path, "r") as f:
    text = f.read()
model = os.environ.get("KIMI_MODEL", "").strip()
if model:
    if re.search(r'^default_model\s*=', text, re.MULTILINE):
        text = re.sub(r'^default_model\s*=.*$', f'default_model = "{model}"', text, count=1, flags=re.MULTILINE)
    else:
        text = f'default_model = "{model}"\n' + text
with open(path, "w") as f:
    f.write(text)
PY
    fi
    if [ -n "${KIMI_BASE_URL:-}" ]; then
      # Override provider base_url in config.toml.
      python3 - "$KIMI_CODE_HOME/config.toml" <<'PY'
import os, re, sys
path = sys.argv[1]
with open(path, "r") as f:
    text = f.read()
base = os.environ.get("KIMI_BASE_URL", "").strip()
if base:
    text = re.sub(r'base_url\s*=\s*"[^"]*"', f'base_url = "{base}"', text, count=1)
with open(path, "w") as f:
    f.write(text)
PY
    fi
    if [ -n "${KIMI_API_KEY:-}" ]; then
      # Inject the API key (baked default ships api_key = "").
      python3 - "$KIMI_CODE_HOME/config.toml" <<'PY'
import os, re, sys
path = sys.argv[1]
with open(path, "r") as f:
    text = f.read()
key = os.environ.get("KIMI_API_KEY", "").strip()
if key:
    text = re.sub(r'api_key\s*=\s*"[^"]*"', f'api_key = "{key}"', text, count=1)
with open(path, "w") as f:
    f.write(text)
PY
    fi
    log "Config generated with overrides (model=${KIMI_MODEL:-<default>}, base_url=${KIMI_BASE_URL:-<default>}, api_key=${KIMI_API_KEY:+set})"
  else
    cp /app/config/config.toml "$KIMI_CODE_HOME/config.toml"
    log "Config copied: /app/config/config.toml -> $KIMI_CODE_HOME/config.toml"
  fi
fi

# ── always start the health server in the background ──────────────────────────
log "Starting health server on port $HEALTH_PORT"
export WS_STATE_FILE
export KIMI_MODE="$MODE"
export KIMI_VERSION="kimi-worker-1.0.0"
python3 /app/health_server.py --port "$HEALTH_PORT" &
HEALTH_PID=$!

sleep 1

cleanup() {
  log "Shutting down..."
  kill "$BRIDGE_PID" 2>/dev/null || true
  kill "$HEALTH_PID" 2>/dev/null || true
  wait "$BRIDGE_PID" 2>/dev/null || true
  wait "$HEALTH_PID" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# ── mode dispatch ──────────────────────────────────────────────────────────────
case "$MODE" in

  cli)
    log "Mode: CLI — running kimi with args: $*"
    cd "$WORKSPACE_ROOT"
    exec kimi "$@"
    ;;

  websocket)
    log "Mode: WebSocket — starting bridge"

    cd "$WORKSPACE_ROOT"
    export KIMI_CODE_HOME
    export WS_STATE_FILE
    export WORKSPACE_ROOT
    export KIMI_MODE

    node /app/scripts/lunarwing_bridge.js &
    BRIDGE_PID=$!

    log "Kimi Code worker bridge started (pid $BRIDGE_PID)"
    wait "$BRIDGE_PID"
    ;;

  *)
    die "Unknown mode '$MODE'. Use --mode websocket or --mode cli"
    ;;

esac
