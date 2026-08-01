#!/usr/bin/env bash
# Root orchestration must delegate secret env reads and writes to tenant identity.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT
source "$ADMIN_SCRIPT"

TENANT=fixture
INIT_SYSTEM=systemd
ENV_DIR="$TMP_ROOT/env"
mkdir -p "$ENV_DIR"
chmod 0700 "$ENV_DIR"
say() { :; }
tenant_exists_in_registry() { return 0; }
tenant_darkirc_enabled() { return 0; }
ensure_init_system() { :; }
_systemctl_user() { return 0; }
tenant_env_dir() { printf '%s\n' "$ENV_DIR"; }
ports_get() {
  case "$2" in
    darkirc_adapter) printf '21010\n' ;;
    darkirc_irc) printf '21011\n' ;;
    orchestrator) printf '21012\n' ;;
    *) return 1 ;;
  esac
}

SUDO_LOG="$TMP_ROOT/sudo.log"
sudo() {
  printf '%s\n' "$*" >>"$SUDO_LOG"
  [[ "${1:-}" == -u ]] && shift 2
  command "$@"
}

printf 'DARKIRC_ADAPTER_SECRET=stable-secret\n' >"$ENV_DIR/lunarwing.env"
chmod 0600 "$ENV_DIR/lunarwing.env"
DARKIRC_WRITER_LOCK_HELD=true
write_tenant_darkirc_adapter_env "$TENANT" >/dev/null
grep -Fq 'DARKIRC_ADAPTER_SECRET' "$SUDO_LOG"
grep -qx 'ADAPTER_SECRET=stable-secret' "$ENV_DIR/darkirc-adapter.env"
[[ "$(stat -c '%a' "$ENV_DIR/darkirc-adapter.env")" == 600 ]]

: >"$SUDO_LOG"
curl() {
  local arg
  for arg in "$@"; do [[ "$arg" != *stable-secret* ]]; done
  cat >/dev/null
  printf '{"status":"ok","irc_connected":true}\n'
}
darkirc_health_strict "$TENANT" 1 true
grep -Fq 'DARKIRC_ADAPTER_SECRET' "$SUDO_LOG"

tenant_darkirc_enabled() { return 1; }
_owner_scope_needs_migration() { return 1; }
ensure_external_worker_config() { :; }
ports_get() {
  case "$2" in
    orchestrator) printf '21012\n' ;;
    nanocode_wss) printf '21013\n' ;;
    pebble_wss) printf '21014\n' ;;
    opencode_wss) printf '21015\n' ;;
    weechat_adapter) printf '21016\n' ;;
    weechat) printf '21017\n' ;;
    coord_admin) printf '21018\n' ;;
    *) return 1 ;;
  esac
}
printf 'BASE_VALUE=preserved\n' >"$ENV_DIR/lunarwing.env"
chmod 0600 "$ENV_DIR/lunarwing.env"
patch_tenant_env "$TENANT" >/dev/null
patch_tenant_env "$TENANT" >/dev/null
grep -qx 'BASE_VALUE=preserved' "$ENV_DIR/lunarwing.env"
[[ "$(grep -c '^LUNARWING_OWNER_ID=fixture$' "$ENV_DIR/lunarwing.env")" -eq 1 ]]
[[ "$(grep -c '^ORCHESTRATOR_PORT=21012$' "$ENV_DIR/lunarwing.env")" -eq 1 ]]

ln "$ENV_DIR/lunarwing.env" "$TMP_ROOT/hardlink"
if ( patch_tenant_env "$TENANT" ) >/dev/null 2>&1; then
  printf 'patch-env accepted multiply-linked file\n' >&2
  exit 1
fi
rm -f "$TMP_ROOT/hardlink"
chmod 0644 "$ENV_DIR/lunarwing.env"
if ( patch_tenant_env "$TENANT" ) >/dev/null 2>&1; then
  printf 'patch-env accepted permissive file\n' >&2
  exit 1
fi

printf 'ALL TESTS PASSED\n'
