#!/usr/bin/env bash
# Regression test: rewriting lunarwing.env must preserve the DarkIRC adapter secret.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

source "$ADMIN_SCRIPT"

TENANT="fixture"
ENV_DIR="$TMP_ROOT/env"
STATE_DIR="$TMP_ROOT/state"
RUN_DIR="$TMP_ROOT/run"
mkdir -p "$ENV_DIR" "$STATE_DIR" "$RUN_DIR"

tenant_env_dir() { printf '%s\n' "$ENV_DIR"; }
tenant_state_dir() { printf '%s\n' "$STATE_DIR"; }
tenant_run_dir() { printf '%s\n' "$RUN_DIR"; }
tenant_proxy_enabled() { return 1; }
tenant_darkirc_enabled() { return 0; }
tenant_pg_password() { printf 'pg-password\n'; }
generate_token() { printf 'generated-token\n'; }
ports_get() {
  case "$2" in
    gateway) printf '21000\n' ;;
    http) printf '21001\n' ;;
    bridge) printf '21002\n' ;;
    postgres) printf '21003\n' ;;
    proxy) printf '21004\n' ;;
    weechat) printf '21005\n' ;;
    weechat_adapter) printf '21006\n' ;;
    orchestrator) printf '21007\n' ;;
    nanocode_wss) printf '21008\n' ;;
    pebble_wss) printf '21009\n' ;;
    opencode_wss) printf '21010\n' ;;
    *) return 1 ;;
  esac
}
chown() { :; }
say() { :; }

cat >"$ENV_DIR/lunarwing.env" <<'ENV'
DARKIRC_ADAPTER_SECRET=stable-adapter-secret
ENV

write_tenant_lunarwing_env "$TENANT"

grep -qx 'DARKIRC_ADAPTER_SECRET=stable-adapter-secret' "$ENV_DIR/lunarwing.env"

# A tenant-controlled final symlink must never turn the root-run adapter writer
# into an arbitrary-file overwrite primitive.
sentinel="$TMP_ROOT/adapter-sentinel"
printf 'SENTINEL\n' >"$sentinel"
ln -s "$sentinel" "$ENV_DIR/darkirc-adapter.env"
tenant_darkirc_enabled() { return 0; }
ports_get() {
  case "$2" in
    darkirc_adapter) printf '21011\n' ;;
    darkirc_irc) printf '21012\n' ;;
    *) return 1 ;;
  esac
}
DARKIRC_WRITER_LOCK_HELD=true
if ( write_tenant_darkirc_adapter_env "$TENANT" ) >/dev/null 2>&1; then
  printf 'adapter writer accepted a final symlink\n' >&2
  exit 1
fi
[[ "$(cat "$sentinel")" == 'SENTINEL' ]] || {
  printf 'adapter writer modified a symlink target\n' >&2
  exit 1
}
printf 'ALL TESTS PASSED\n'
