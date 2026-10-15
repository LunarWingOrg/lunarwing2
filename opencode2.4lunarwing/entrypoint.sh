#!/bin/bash
# entrypoint.sh — LunarWing OpenCode 2 Worker startup script
set -euo pipefail

log()  { printf '[%s] %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }
die()  { log "ERROR: $*" >&2; exit 1; }

MODE="${OPENCODE_MODE:-websocket}"
HEALTH_PORT="${HEALTH_PORT:-8443}"
OPENCODE_SERVE_PORT="${OPENCODE_SERVE_PORT:-4096}"
OPENCODE_SERVE_HOST="${OPENCODE_SERVE_HOST:-127.0.0.1}"
WS_ROLE="${WS_ROLE:-server}"
WS_STATE_FILE="${WS_STATE_FILE:-/tmp/lunarwing_ws_state.json}"
FILE_UMASK="${FILE_UMASK:-0002}"
OPENCODE_BIN="${OPENCODE_BIN:-/usr/local/bin/opencode}"
WORKSPACE_ROOT="${WORKSPACE_ROOT:-/workspace}"

umask "$FILE_UMASK"

# WI#357 / FB-1: server-backed CLI commands otherwise spawn the real binary
# directly in service mode, bypassing the argv wrapper. Reset the shared
# service default on every boot so they use private, command-scoped servers.
# This is a naive-use guard: the agent can still edit its live service config.
# FB-7: upstream resolves the service config as $OPENCODE_CONFIG_DIR when set,
# otherwise ${XDG_CONFIG_HOME:-~/.config}/opencode — honour the same order so
# the reset always lands where upstream reads it.
SERVICE_CONFIG_DIR="${OPENCODE_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/opencode}"
mkdir -p "$SERVICE_CONFIG_DIR"
# FB-6: the agent can plant a FIFO, directory or symlink at the reset path; a
# plain redirect would hang on the FIFO, crash on the directory, or overwrite
# the symlink's target. Remove whatever sits there before writing.
rm -rf -- "$SERVICE_CONFIG_DIR/service.json"
printf '%s\n' '{"disabled": true}' > "$SERVICE_CONFIG_DIR/service.json"

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
if [ -d /home/opencode2/.host-ssh ] && [ "$(ls -A /home/opencode2/.host-ssh 2>/dev/null)" ]; then
  mkdir -p /home/opencode2/.ssh
  cp -a /home/opencode2/.host-ssh/* /home/opencode2/.ssh/ 2>/dev/null || true
  chmod 700 /home/opencode2/.ssh 2>/dev/null || true
  chmod 600 /home/opencode2/.ssh/id_* 2>/dev/null || true
  chmod 644 /home/opencode2/.ssh/*.pub 2>/dev/null || true
  chmod 644 /home/opencode2/.ssh/known_hosts 2>/dev/null || true
  log "SSH keys copied from host mount"
  if [ ! -f /home/opencode2/.ssh/config ]; then
    printf 'Host *\n  StrictHostKeyChecking accept-new\n  UserKnownHostsFile /home/opencode2/.ssh/known_hosts\n' \
      > /home/opencode2/.ssh/config
    chmod 600 /home/opencode2/.ssh/config
  fi
fi

# Copy host .gitconfig if mounted
if [ -f /home/opencode2/.host-gitconfig ] && [ -s /home/opencode2/.host-gitconfig ]; then
  cp /home/opencode2/.host-gitconfig /home/opencode2/.gitconfig 2>/dev/null || true
  log "Host .gitconfig copied into container"
fi

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

# ── config setup ──────────────────────────────────────────────────────────────
if { [ -n "${LW2STUDYMCP_STATE_HOME:-}" ] || [ -n "${LW2STUDYMCP_ALLOW_WRITE:-}" ]; } \
    && [ -z "${LW2STUDYMCP_HOME:-}" ]; then
  die "LW2STUDYMCP_HOME is required when Study Notes state or write mode is configured"
fi
case "${LW2STUDYMCP_ALLOW_WRITE:-0}" in
  1|true|TRUE|yes|YES|on|ON) die "Study Notes writes are not exposed inside OpenCode workers" ;;
  0|false|FALSE|no|NO|off|OFF|"") ;;
  *) die "LW2STUDYMCP_ALLOW_WRITE must be a boolean" ;;
esac

if [ -f /app/config/opencode2.json ]; then
  mkdir -p "$WORKSPACE_ROOT/.opencode"
  if [ -n "${OPENCODE2_MODEL:-}" ] || [ -n "${OPENCODE2_BASE_URL:-}" ] \
      || [ -n "${PASEO_URL:-}" ] || [ -n "${PASEO_TOKEN:-}" ] \
      || [ -n "${LW2STUDYMCP_HOME:-}" ] \
      || [ -n "${LW2STUDYMCP_STATE_HOME:-}" ]; then
    rm -f "$WORKSPACE_ROOT/.opencode/opencode2.json"
    cp /app/config/opencode2.json "$WORKSPACE_ROOT/.opencode/opencode2.json"
    # cp preserves the baked 0444 mode; only the runtime copy must be writable.
    chmod u+w "$WORKSPACE_ROOT/.opencode/opencode2.json"
    config_materialized=true
  else
    ln -sfn /app/config/opencode2.json "$WORKSPACE_ROOT/.opencode/opencode2.json"
    config_materialized=false
  fi
  if [ -n "${OPENCODE2_MODEL:-}" ] || [ -n "${OPENCODE2_BASE_URL:-}" ]; then
    python3 - "$WORKSPACE_ROOT/.opencode/opencode2.json" <<'PY'
import json, os, sys
with open("/app/config/opencode2.json") as f:
    cfg = json.load(f)
model = os.environ.get("OPENCODE2_MODEL", "").strip()
base = os.environ.get("OPENCODE2_BASE_URL", "").strip()
if model:
    # v2 model selection is "<providerID>/<model-id>" (ConfigModel.Selection
    # Short form, packages/schema/src/config/model.ts). A bare function name
    # like "tensorzero::function_name::FrontierCODE" has no slash, so build
    # the reference against the first provider in the config (the TensorZero
    # provider) and register the model in that provider's models map so
    # opencode can resolve it.
    if "/" not in model:
        providers = cfg.get("providers", {})
        if providers:
            provider_id = next(iter(providers))
            cfg["model"] = f"{provider_id}/{model}"
            providers[provider_id].setdefault("models", {})[model] = {"name": model}
        else:
            cfg["model"] = model
    else:
        cfg["model"] = model
if base:
    # v2 provider request overlays: baseURL lives under settings (the v1
    # options.baseURL field migrates to settings.baseURL per
    # packages/core/src/v1/config/migrate.ts migrateStandardProvider).
    for prov in cfg.get("providers", {}).values():
        prov.setdefault("settings", {})["baseURL"] = base
with open(sys.argv[1], "w") as f:
    json.dump(cfg, f, indent=2)
PY
    log "Config generated with overrides (model=${OPENCODE2_MODEL:-<default>}, baseURL=${OPENCODE2_BASE_URL:-<default>})"
  elif [ "$config_materialized" = true ]; then
    log "Config copied: /app/config/opencode2.json -> $WORKSPACE_ROOT/.opencode/opencode2.json"
  else
    log "Config linked: /app/config/opencode2.json -> $WORKSPACE_ROOT/.opencode/opencode2.json"
  fi
  # opencode v2 only auto-discovers opencode.json/opencode.jsonc (project dirs,
  # .opencode/, the global config dir) and never reads .opencode/opencode2.json,
  # so without this the server ignores the lane config and falls back to its
  # default provider. OPENCODE_CONFIG is the CLI's explicit config-file path; an
  # operator-set value wins.
  : "${OPENCODE_CONFIG:=$WORKSPACE_ROOT/.opencode/opencode2.json}"
  export OPENCODE_CONFIG
  log "OPENCODE_CONFIG=$OPENCODE_CONFIG"
  # WI#330: the baked config ships a placeholder baseURL on the .invalid TLD
  # (guaranteed NXDOMAIN). mt-admin always renders OPENCODE2_BASE_URL —
  # defaulting to the tenant's own LLM endpoint — so a placeholder that
  # survives to the EFFECTIVE runtime config (an operator-set OPENCODE_CONFIG
  # wins, so that file is what gets checked) means a hand-rolled deployment
  # forgot the override. Fail loudly at startup, not at the first LLM request.
  if grep -q 'set-opencode2-base-url\.invalid' "$OPENCODE_CONFIG" 2>/dev/null; then
    die "OPENCODE2_BASE_URL is unset and the baked config is a placeholder; set it (mt-admin: configure-opencode2 <tenant> --base-url <url>) to a container-reachable OpenAI-compatible endpoint"
  fi
fi

# ── optional Paseo MCP integration ────────────────────────────────────────────
if [ -n "${PASEO_URL:-}" ] && [ -n "${PASEO_TOKEN:-}" ] && [ -f "$WORKSPACE_ROOT/.opencode/opencode2.json" ]; then
  python3 - "$WORKSPACE_ROOT/.opencode/opencode2.json" <<'PY'
import json, os, sys
path = sys.argv[1]
with open(path) as f:
    cfg = json.load(f)
mcp = cfg.setdefault("mcp", {})
mcp["paseo"] = {
    "type": "remote",
    "url": os.environ["PASEO_URL"].rstrip("/") + "/mcp/agents",
    "headers": {"Authorization": "Bearer " + os.environ["PASEO_TOKEN"]},
}
with open(path, "w") as f:
    json.dump(cfg, f, indent=2)
PY
  log "Paseo MCP entry injected into opencode config"
fi

# ── optional LunarWing Study Notes MCP integration ────────────────────────────
if [ -n "${LW2STUDYMCP_HOME:-}" ] && [ -f "$WORKSPACE_ROOT/.opencode/opencode2.json" ]; then
  study_args=(
    --config "$WORKSPACE_ROOT/.opencode/opencode2.json"
    --home "$LW2STUDYMCP_HOME"
  )
  if [ -n "${LW2STUDYMCP_STATE_HOME:-}" ]; then
    study_args+=(--state-home "$LW2STUDYMCP_STATE_HOME")
  fi
  python3 /app/scripts/study_notes_config.py "${study_args[@]}"
  log "Study Notes MCP and skill path injected into opencode config"
fi

# ── always start the health server in the background ──────────────────────────
log "Starting health server on port $HEALTH_PORT"
export WS_STATE_FILE
export CODEX_MODE="$MODE"
export CODEX_VERSION="opencode2-worker-1.0.0"
python3 /app/health_server.py --port "$HEALTH_PORT" &
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
    # WI#373: the prompt runs untrusted agent code — never inherit the
    # bridge's hub-handshake credential.
    exec env -u AGENT_AUTH_TOKEN "$OPENCODE_BIN" run "$@"
    ;;

  acp)
    log "Mode: ACP — launching opencode ACP bridge"
    cd "$WORKSPACE_ROOT"
    # WI#373: same one-shot task boundary as cli mode.
    exec env -u AGENT_AUTH_TOKEN "$OPENCODE_BIN" acp "$@"
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
    # WI#373: the serve process runs all task code (every bash tool child
    # inherits its env), so strip the bridge's hub-handshake credential from
    # the server env. The bridge keeps its own copy for the WebSocket
    # handshake; the server never needs it. OPENCODE_PASSWORD stays — the
    # server itself enforces it.
    env -u AGENT_AUTH_TOKEN "$OPENCODE_BIN" serve \
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

    exec bun run /app/scripts/lunarwing_bridge.ts
    ;;

  *)
    die "Unknown mode '$MODE'. Use --mode websocket, --mode cli, or --mode acp"
    ;;

esac
