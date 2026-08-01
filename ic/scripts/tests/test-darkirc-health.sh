#!/usr/bin/env bash
# Strict DarkIRC health checks both init services and authenticated adapter state.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT
source "$ADMIN_SCRIPT"

TENANT=fixture
INIT_SYSTEM=systemd
tenant_exists_in_registry() { return 0; }
tenant_darkirc_enabled() { return 0; }
ensure_init_system() { :; }
_systemctl_user() { return 0; }
ports_get() { [[ "$2" == darkirc_adapter ]] && printf '21010\n'; }
tenant_env_dir() { printf '%s\n' "$TMP_ROOT"; }
sudo() { [[ "${1:-}" == -u ]] && shift 2; command "$@"; }
curl() {
  local arg
  for arg in "$@"; do [[ "$arg" != *stable-secret* ]]; done
  cat >/dev/null
  printf '{"status":"ok","irc_connected":true}\n'
}
printf 'DARKIRC_ADAPTER_SECRET=stable-secret\n' >"$TMP_ROOT/lunarwing.env"
chmod 0600 "$TMP_ROOT/lunarwing.env"
darkirc_health_strict "$TENANT" 1 true

INIT_SYSTEM=openrc
OPENRC_INITD_DIR="$TMP_ROOT/init.d"
mkdir -p "$OPENRC_INITD_DIR"
printf '#!/bin/sh\n' >"$OPENRC_INITD_DIR/lunarwing-fixture"
chmod +x "$OPENRC_INITD_DIR/lunarwing-fixture"
tenant_writer_services() { printf 'lunarwing-%s\n' "$TENANT"; }
ensure_container_runtime() { :; }
_ctr() { [[ "${2:-}" == ps ]]; }
rc-service() { printf 'stopped\n'; return 3; }
writer_rc=0
tenant_writers_active "$TENANT" || writer_rc=$?
[[ "$writer_rc" -eq 1 ]]

rc-service() { return 0; }
darkirc_health_strict "$TENANT" 1 true

printf 'ALL TESTS PASSED\n'
