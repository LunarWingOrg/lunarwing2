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
KIMI_CONFIG_SOURCE="${KIMI_CONFIG_SOURCE:-/app/config/config.toml}"

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
# KIMI_MODEL replaces default_model; when it does not match an existing
# [models] alias, the baked model entry is retargeted (alias + model id) so
# default_model stays resolvable — 0.37.2 fails startup otherwise.
# KIMI_API_KEY falls back to TENSORZERO_API_KEY for the TensorZero-backed
# default provider.
#
# WI#206: after overrides are applied, the fail-closed policy ([permission]
# dangerous_command_guard = true and the PreToolUse guard hook) is re-asserted
# and the guard-disabling env vars are scrubbed from the task environment, so
# a task (or a prompt-injected command) cannot lower its own approval policy.
export KIMI_API_KEY="${KIMI_API_KEY:-${TENSORZERO_API_KEY:-}}"
mkdir -p "$KIMI_CODE_HOME"

if [ ! -f "$KIMI_CONFIG_SOURCE" ]; then
  die "Kimi config source not found: $KIMI_CONFIG_SOURCE"
fi

if [ -f "$KIMI_CONFIG_SOURCE" ]; then
  python3 - "$KIMI_CONFIG_SOURCE" "$KIMI_CODE_HOME/config.toml" <<'PY'
import os, re, sys, tempfile, tomllib

source, target = sys.argv[1:]
with open(source, "r", encoding="utf-8") as config_file:
    text = config_file.read()

def toml_string(value):
    escapes = {'"': '\\"', '\\': '\\\\', '\b': '\\b', '\t': '\\t', '\n': '\\n', '\f': '\\f', '\r': '\\r'}
    return '"' + ''.join(escapes.get(char, f"\\u{ord(char):04x}" if ord(char) < 0x20 or ord(char) == 0x7f else char) for char in value) + '"'

def replace_setting(config, name, value, required):
    if not value:
        return config
    pattern = re.compile(rf'^(\s*{re.escape(name)}\s*=).*$' , re.MULTILINE)
    if pattern.search(config):
        return pattern.sub(lambda match: f'{match.group(1)} {toml_string(value)}', config, count=1)
    if required:
        raise ValueError(f"config missing {name}")
    return f'{name} = {toml_string(value)}\n{config}'

def retarget_model_entry(config, old_alias, new_model):
    # KIMI_MODEL does not match an existing [models] alias: rename the baked
    # model entry (table header + model id) so default_model stays resolvable.
    # kimi-code >= 0.37 fails startup when default_model is not a known alias.
    header = re.compile(rf'^\[models\.(?:"{re.escape(old_alias)}"|{re.escape(old_alias)})\]\s*$', re.MULTILINE)
    match = header.search(config)
    if not match:
        # Custom KIMI_CONFIG_SOURCE without a matching models table: leave it
        # untouched; whoever mounts it owns alias resolution.
        return config
    config = config[:match.start()] + f'[models.{toml_string(new_model)}]' + config[match.end():]
    body = re.compile(r'^(\s*model\s*=).*$', re.MULTILINE).search(config, match.start())
    if body:
        config = config[:body.start()] + f'{body.group(1)} {toml_string(new_model)}' + config[body.end():]
    return config

kimi_model = os.environ.get("KIMI_MODEL", "").strip()
old_alias = ""
if kimi_model:
    try:
        old_alias = tomllib.loads(text).get("default_model", "")
    except tomllib.TOMLDecodeError:
        old_alias = ""
text = replace_setting(text, "default_model", kimi_model, False)
if kimi_model and old_alias and old_alias != kimi_model:
    text = retarget_model_entry(text, old_alias, kimi_model)
# Whitespace-only URLs are unset (the placeholder guard must still fire).
# Normalize the model once above so alias retargeting uses the same value;
# API keys remain byte-for-byte as supplied.
text = replace_setting(text, "base_url", os.environ.get("KIMI_BASE_URL", "").strip(), True)
text = replace_setting(text, "api_key", os.environ.get("KIMI_API_KEY", ""), True)

# WI#206 fail-closed policy enforcement. Whatever the source config said
# (custom KIMI_CONFIG_SOURCE mounts included), the worker never runs with the
# dangerous-command guard disabled or without the PreToolUse guard hook:
# both are (re)injected when absent and verified present afterwards.
def policy_block(config):
    return "\n[permission]\ndangerous_command_guard = true\n"

def hook_block():
    return ("\n[[hooks]]\n"
            "event = \"PreToolUse\"\n"
            "matcher = \"Bash\"\n"
            "command = \"node /app/hooks/headless_command_guard.mjs\"\n"
            "timeout = 10\n")

parsed = tomllib.loads(text)
if "permission" not in parsed or not isinstance(parsed.get("permission"), dict):
    text += policy_block(text)
elif parsed["permission"].get("dangerous_command_guard") is not True:
    if "dangerous_command_guard" in parsed["permission"]:
        text = re.sub(
            r"^\s*dangerous_command_guard\s*=.*$",
            "dangerous_command_guard = true",
            text,
            count=1,
            flags=re.MULTILINE,
        )
    else:
        text = text.replace("[permission]", "[permission]\ndangerous_command_guard = true", 1)
parsed = tomllib.loads(text)
hooks = parsed.get("hooks")
guard_hook_present = isinstance(hooks, list) and any(
    isinstance(hook, dict)
    and hook.get("event") == "PreToolUse"
    and hook.get("matcher") == "Bash"
    and "headless_command_guard.mjs" in str(hook.get("command", ""))
    for hook in hooks
)
if not guard_hook_present:
    text += hook_block()
tomllib.loads(text)

fd, temporary = tempfile.mkstemp(prefix=".config.toml.", dir=os.path.dirname(target))
try:
    os.fchmod(fd, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as config_file:
        config_file.write(text)
        config_file.flush()
        os.fsync(config_file.fileno())
    os.replace(temporary, target)
    os.chmod(target, 0o600)
except BaseException:
    try:
        os.close(fd)
    except OSError:
        pass
    try:
        os.unlink(temporary)
    except FileNotFoundError:
        pass
    raise
PY
  if [ -n "${KIMI_MODEL:-}" ] || [ -n "${KIMI_BASE_URL:-}" ] || [ -n "${KIMI_API_KEY:-}" ]; then
    log "Config generated with overrides (model=${KIMI_MODEL:-<default>}, base_url=${KIMI_BASE_URL:-<default>}, api_key=${KIMI_API_KEY:+set})"
  else
    log "Config copied: $KIMI_CONFIG_SOURCE -> $KIMI_CODE_HOME/config.toml"
  fi
fi

# WI#330: the baked config ships a placeholder base_url on the .invalid TLD
# (guaranteed NXDOMAIN). mt-admin always renders KIMI_BASE_URL — defaulting to
# the tenant's own LLM endpoint — so a placeholder that survives to the
# runtime config means a hand-rolled deployment forgot the override. Fail
# loudly at startup instead of at the first LLM request.
if grep -q 'set-kimi-base-url\.invalid' "$KIMI_CODE_HOME/config.toml" 2>/dev/null; then
  die "KIMI_BASE_URL is unset and the baked config is a placeholder; set it (mt-admin: configure-kimi <tenant> --base-url <url>) to a container-reachable OpenAI-compatible endpoint"
fi

# WI#324 worker-side tool bridge: register the thin stdio MCP server that
# relays MCP tools/list + tools/call into tool_list/tool_call frames on the
# hub WebSocket. User-level mcp.json is trusted (no workspace trust prompt);
# written only when absent so an operator can override or disable the entry
# ("enabled": false) without the entrypoint clobbering it. The shim
# advertises zero tools unless the bridge injects the per-task proxy env, and
# the hub stays deny-by-default without a tool_allowlist grant.
if [ ! -f "$KIMI_CODE_HOME/mcp.json" ]; then
  umask 0177
  printf '%s\n' '{"mcpServers":{"lunarwing":{"command":"node","args":["/app/scripts/lunarwing_mcp_server.js"]}}}' \
    > "$KIMI_CODE_HOME/mcp.json"
  umask "$FILE_UMASK"
  log "Registered lunarwing MCP tool-proxy server in $KIMI_CODE_HOME/mcp.json"
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
    # WI#373: the prompt runs untrusted agent code — never inherit the
    # bridge's hub-handshake credential.
    exec env -u AGENT_AUTH_TOKEN kimi -p "$*" --output-format stream-json
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
