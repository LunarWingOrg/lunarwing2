#!/bin/bash
set -euo pipefail

log() { printf '[%s] %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }
die() { log "ERROR: $*" >&2; exit 1; }

[[ "$(id -u)" == "10005" ]] || die "task setup requires uid 10005"
[[ -z "${AGENT_AUTH_TOKEN:-}" && -z "${AGENT_AUTH_TOKEN_FILE:-}" ]] || die "hub credentials must not reach task setup"

export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export HOME=/home/lunardepth-home
export DSH_HOME=/home/lunardepth/.dsh
export WORKSPACE_ROOT=/workspace
export TMPDIR=/tmp TMP=/tmp TEMP=/tmp
export WS_NO_BUFFER_UTIL=1 WS_NO_UTF_8_VALIDATE=1
export DSH_TELEMETRY_DISABLED=1
export LUNARDEPTH_WS_URL=ws://127.0.0.1:19091/ws/agent
unset NODE_OPTIONS DSH_PERMISSION_MODE TENSORMOON_BASE_URL TENSORMOON_API_KEY DEEPSEEK_API_KEY DSH_TELEMETRY_OTLP_URL
unset LUNARWING_WORKER_HOST LUNARWING_WORKER_PORT LUNARWING_WORKSPACE_ROOT

APP_DIR="${LUNARWING_APP_DIR:-/app}"
TEMPLATE_ROOT="${LUNARDEPTH_TEMPLATE_ROOT:-/opt/lunardepth-templates}"
OVERLAY="${LUNARDEPTH_EFFECTIVE_OVERLAY:-/run/lunardepth-config/lunarwing-lane.patch.yml}"
BASE_URL="${LUNARDEPTH_BASE_URL:-}"
MODEL="${LUNARDEPTH_MODEL:-}"
ALLOW_INSECURE="${LUNARDEPTH_ALLOW_INSECURE_HTTP:-false}"
[[ "$ALLOW_INSECURE" == "true" || "$ALLOW_INSECURE" == "false" ]] || die "LUNARDEPTH_ALLOW_INSECURE_HTTP must be exactly true or false"
[[ -n "$BASE_URL" && -n "$MODEL" ]] || die "LUNARDEPTH_BASE_URL and LUNARDEPTH_MODEL are required"

verify_root_file() {
  local path="$1" mode="$2"
  [[ -f "$path" && ! -L "$path" ]] || die "required root-owned file is missing: $path"
  [[ "$(stat -c '%u:%a' "$path")" == "0:$mode" ]] || die "unexpected owner/mode on $path"
}

verify_effective_overlay() {
  local expected="${LUNARDEPTH_EFFECTIVE_OVERLAY_SHA256:-}" actual
  [[ -f "$OVERLAY" && ! -L "$OVERLAY" ]] || die "effective overlay must be a regular non-symlink file"
  [[ "$(stat -c '%a' "$OVERLAY")" == "444" ]] || die "effective overlay must be mode 0444"
  [[ "$expected" =~ ^[0-9a-f]{64}$ ]] || die "effective overlay SHA-256 launch contract is required"
  actual="$(sha256sum "$OVERLAY" | cut -d' ' -f1)"
  [[ "$actual" == "$expected" ]] || die "effective overlay digest differs from its launch contract"
}

verify_effective_overlay
verify_root_file "$APP_DIR/config/lunarwing-lane.patch.yml" 444
verify_root_file "$APP_DIR/config/home-cordis.patch.yml" 444
verify_root_file "$APP_DIR/config/home.env" 444
verify_root_file "$TEMPLATE_ROOT/profiles/lunarwing-worker/package.json" 444
verify_root_file "$TEMPLATE_ROOT/profiles/lunarwing-worker/cordis.patch.yml" 444
verify_root_file "$TEMPLATE_ROOT/profiles/lunarwing-worker/pnpm-workspace.yaml" 444
[[ "$(stat -c '%a' "$(dirname "$OVERLAY")")" == "555" ]] || die "overlay directory must be mode 0555"
[[ "$(stat -c '%u:%a' "$HOME")" == "0:555" ]] || die "harness HOME must be root-owned mode 0555"
[[ -z "$(find "$HOME" -mindepth 1 -maxdepth 1 -print -quit)" ]] || die "harness HOME must remain empty"

render_args=(
  --template "$APP_DIR/config/lunarwing-lane.patch.yml"
  --check "$OVERLAY"
  --base-url "$BASE_URL"
  --model "$MODEL"
)
if [[ "$ALLOW_INSECURE" == "true" ]]; then
  render_args+=(--allow-insecure-http)
fi
/usr/bin/python3 "$APP_DIR/scripts/render_overlay.py" "${render_args[@]}" || die "effective overlay validation failed"

if [[ -n "${LUNARDEPTH_API_KEY_FILE:-}" ]]; then
  [[ -f "$LUNARDEPTH_API_KEY_FILE" && ! -L "$LUNARDEPTH_API_KEY_FILE" ]] || die "model key must be a regular non-symlink file"
  [[ "$(stat -c '%u:%a' "$LUNARDEPTH_API_KEY_FILE")" == "10005:400" ]] || die "model key file must be task-owned mode 0400"
  LUNARDEPTH_API_KEY="$(<"$LUNARDEPTH_API_KEY_FILE")"
fi
if [[ -z "${LUNARDEPTH_API_KEY:-}" ]]; then
  [[ "${LUNARDEPTH_ALLOW_PLACEHOLDER_KEY:-false}" == "true" ]] \
    || die "LUNARDEPTH_API_KEY is required unless the explicit unauthenticated-gateway option is enabled"
  export LUNARDEPTH_API_KEY=lunarwing-unauthenticated-placeholder
fi
export LUNARDEPTH_API_KEY
unset LUNARDEPTH_API_KEY_FILE

install -d -m 0700 "$DSH_HOME"
for path in "$DSH_HOME"/* "$DSH_HOME"/.[!.]* "$DSH_HOME"/..?*; do
  [[ -e "$path" || -L "$path" ]] || continue
  name="$(basename "$path")"
  case "$name" in
    sessions|storages|attachments)
      [[ -d "$path" && ! -L "$path" ]] || die "session data path must be a real directory: $name"
      [[ "$(stat -c '%u' "$path")" == "10005" ]] || die "session data path has the wrong owner: $name"
      chmod 0700 "$path"
      ;;
    *) rm -rf -- "$path" ;;
  esac
done
for name in sessions storages attachments; do
  install -d -m 0700 "$DSH_HOME/$name"
done

install -d -m 0700 "$DSH_HOME/profiles"
cp -a "$TEMPLATE_ROOT/profiles/lunarwing-worker" "$DSH_HOME/profiles/lunarwing-worker"
chmod -R u=rwX,go= "$DSH_HOME/profiles"
install -m 0644 "$APP_DIR/config/home-cordis.patch.yml" "$DSH_HOME/cordis.patch.yml"
install -m 0644 "$APP_DIR/config/home.env" "$DSH_HOME/.env"

STATE_DIR=/run/lunardepth-state
install -d -m 0700 "$STATE_DIR"
TOKEN_FILE="$STATE_DIR/local-auth-token"
rm -f -- "$TOKEN_FILE"
/usr/bin/python3 - "$TOKEN_FILE" <<'PY'
import os
import secrets
import sys

path = sys.argv[1]
flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
if hasattr(os, "O_NOFOLLOW"):
    flags |= os.O_NOFOLLOW
fd = os.open(path, flags, 0o400)
with os.fdopen(fd, "w", encoding="utf-8") as handle:
    handle.write(secrets.token_hex(32))
PY
[[ ! -L "$TOKEN_FILE" && "$(stat -c '%u:%a' "$TOKEN_FILE")" == "10005:400" ]] || die "local token file validation failed"
LUNARDEPTH_LOCAL_AUTH_TOKEN="$(<"$TOKEN_FILE")"
export LUNARDEPTH_LOCAL_AUTH_TOKEN
unset LUNARDEPTH_LOCAL_AUTH_TOKEN_FILE

mkdir -p /run/lunarwing-shared/runner
rm -f /run/lunarwing-shared/runner/composition-state.json \
  /run/lunarwing-shared/runner/sandbox-state.json \
  /run/lunarwing-shared/runner/lunardepth-ready.json \
  /run/lunarwing-shared/runner/task-runner-state.json

log "LunarDepth task configuration regenerated"
