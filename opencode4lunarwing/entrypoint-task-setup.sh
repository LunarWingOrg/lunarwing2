#!/bin/bash
# entrypoint-task-setup.sh — LunarWing OpenCode Worker task-side setup
#
# WI#400: the container entrypoint (root) runs this script as the TASK uid
# (opencode, 10001) before starting the serve process and the root bridge.
# Everything here writes task-owned state — ~/.gitconfig, ~/.ssh,
# $WORKSPACE_ROOT/.opencode/opencode.json — so it must never run as root
# (root-owned files would be unreadable/unwritable to the task-uid serve
# process they configure, and subuid-owned on the host side).
set -euo pipefail

log()  { printf '[%s] %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }
die()  { log "ERROR: $*" >&2; exit 1; }

[ "$(id -u)" != "0" ] || die "entrypoint-task-setup.sh runs as the task uid only (WI#400)"

FILE_UMASK="${FILE_UMASK:-0002}"
WORKSPACE_ROOT="${WORKSPACE_ROOT:-/workspace}"

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
if [ -d /home/opencode/.host-ssh ] && [ "$(ls -A /home/opencode/.host-ssh 2>/dev/null)" ]; then
  mkdir -p /home/opencode/.ssh
  cp -a /home/opencode/.host-ssh/* /home/opencode/.ssh/ 2>/dev/null || true
  chmod 700 /home/opencode/.ssh 2>/dev/null || true
  chmod 600 /home/opencode/.ssh/id_* 2>/dev/null || true
  chmod 644 /home/opencode/.ssh/*.pub 2>/dev/null || true
  chmod 644 /home/opencode/.ssh/known_hosts 2>/dev/null || true
  log "SSH keys copied from host mount"
  if [ ! -f /home/opencode/.ssh/config ]; then
    printf 'Host *\n  StrictHostKeyChecking accept-new\n  UserKnownHostsFile /home/opencode/.ssh/known_hosts\n' \
      > /home/opencode/.ssh/config
    chmod 600 /home/opencode/.ssh/config
  fi
fi

# Copy host .gitconfig if mounted
if [ -f /home/opencode/.host-gitconfig ] && [ -s /home/opencode/.host-gitconfig ]; then
  cp /home/opencode/.host-gitconfig /home/opencode/.gitconfig 2>/dev/null || true
  log "Host .gitconfig copied into container"
fi

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

if [ -f /app/config/opencode.json ]; then
  mkdir -p "$WORKSPACE_ROOT/.opencode"
  if [ -n "${OPENCODE_MODEL:-}" ] || [ -n "${OPENCODE_BASE_URL:-}" ] \
      || [ -n "${PASEO_URL:-}" ] || [ -n "${PASEO_TOKEN:-}" ] \
      || [ -n "${LW2STUDYMCP_HOME:-}" ] \
      || [ -n "${LW2STUDYMCP_STATE_HOME:-}" ]; then
    rm -f "$WORKSPACE_ROOT/.opencode/opencode.json"
    cp /app/config/opencode.json "$WORKSPACE_ROOT/.opencode/opencode.json"
    config_materialized=true
  else
    ln -sfn /app/config/opencode.json "$WORKSPACE_ROOT/.opencode/opencode.json"
    config_materialized=false
  fi
  if [ -n "${OPENCODE_MODEL:-}" ] || [ -n "${OPENCODE_BASE_URL:-}" ]; then
    python3 - "$WORKSPACE_ROOT/.opencode/opencode.json" <<'PY'
import json, os, sys
with open("/app/config/opencode.json") as f:
    cfg = json.load(f)
model = os.environ.get("OPENCODE_MODEL", "").strip()
base = os.environ.get("OPENCODE_BASE_URL", "").strip()
if model:
    # opencode resolves a model reference as "<provider>/<model-id>", splitting on
    # the first "/" (packages/core/src/model.ts:parse). A bare function name like
    # "tensorzero::function_name::FrontierCODE" has no slash, so opencode treats the
    # whole string as the provider and the model id as empty, producing a malformed
    # request ("tensorzero::function_name::FrontierCODE/") that the backend rejects.
    # Accept the bare function name and build the provider/model reference against
    # the first provider in the config (the TensorZero provider). Also register the
    # model in that provider's models map so opencode can resolve it.
    if "/" not in model:
        providers = cfg.get("provider", {})
        if providers:
            provider_id = next(iter(providers))
            cfg["model"] = f"{provider_id}/{model}"
            providers[provider_id].setdefault("models", {})[model] = {"name": model}
        else:
            cfg["model"] = model
    else:
        cfg["model"] = model
if base:
    for prov in cfg.get("provider", {}).values():
        prov.setdefault("options", {})["baseURL"] = base
with open(sys.argv[1], "w") as f:
    json.dump(cfg, f, indent=2)
PY
    log "Config generated with overrides (model=${OPENCODE_MODEL:-<default>}, baseURL=${OPENCODE_BASE_URL:-<default>})"
  elif [ "$config_materialized" = true ]; then
    log "Config copied: /app/config/opencode.json -> $WORKSPACE_ROOT/.opencode/opencode.json"
  else
    log "Config linked: /app/config/opencode.json -> $WORKSPACE_ROOT/.opencode/opencode.json"
  fi
  # WI#330: the baked config ships a placeholder baseURL on the .invalid TLD
  # (guaranteed NXDOMAIN). mt-admin always renders OPENCODE_BASE_URL —
  # defaulting to the tenant's own LLM endpoint — so a placeholder that
  # survives to the runtime config means a hand-rolled deployment forgot the
  # override. Fail loudly at startup instead of at the first LLM request.
  if grep -q 'set-opencode-base-url\.invalid' "$WORKSPACE_ROOT/.opencode/opencode.json" 2>/dev/null; then
    die "OPENCODE_BASE_URL is unset and the baked config is a placeholder; set it (mt-admin: configure-opencode <tenant> --base-url <url>) to a container-reachable OpenAI-compatible endpoint"
  fi
fi

# ── optional Paseo MCP integration ────────────────────────────────────────────
if [ -n "${PASEO_URL:-}" ] && [ -n "${PASEO_TOKEN:-}" ] && [ -f "$WORKSPACE_ROOT/.opencode/opencode.json" ]; then
  python3 - "$WORKSPACE_ROOT/.opencode/opencode.json" <<'PY'
import json, os, sys
path = sys.argv[1]
with open(path) as f:
    cfg = json.load(f)
mcp = cfg.setdefault("mcp", {})
mcp["paseo"] = {
    "type": "remote",
    "url": os.environ["PASEO_URL"].rstrip("/") + "/mcp/agents",
    "enabled": True,
    "oauth": False,
    "headers": {"Authorization": "Bearer " + os.environ["PASEO_TOKEN"]},
}
with open(path, "w") as f:
    json.dump(cfg, f, indent=2)
PY
  log "Paseo MCP entry injected into opencode config"
fi

# ── optional LunarWing Study Notes MCP integration ────────────────────────────
if [ -n "${LW2STUDYMCP_HOME:-}" ] && [ -f "$WORKSPACE_ROOT/.opencode/opencode.json" ]; then
  study_args=(
    --config "$WORKSPACE_ROOT/.opencode/opencode.json"
    --home "$LW2STUDYMCP_HOME"
  )
  if [ -n "${LW2STUDYMCP_STATE_HOME:-}" ]; then
    study_args+=(--state-home "$LW2STUDYMCP_STATE_HOME")
  fi
  python3 /app/scripts/study_notes_config.py "${study_args[@]}"
  log "Study Notes MCP and skill path injected into opencode config"
fi
