#!/usr/bin/env bash
# DarkIRC migration and upgrade operations must retain init-agnostic writer locks.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$SCRIPT_DIR/../lunarwing-mt-admin.sh"

# shellcheck source=../lunarwing-mt-admin.sh
source "$ADMIN_SCRIPT"

TENANT=fixture
INIT_SYSTEM=systemd
TEST_ROOT="$(mktemp -d)"
SYSTEMD_CALLS="$TEST_ROOT/systemd.calls"
OPENRC_CALLS="$TEST_ROOT/openrc.calls"
trap 'rm -rf "$TEST_ROOT"' EXIT

tenant_exists_in_registry() { [[ "$1" == "$TENANT" ]]; }
tenant_darkirc_enabled() { return 0; }
tenant_proxy_enabled() { return 1; }
tenant_worker_enabled() { return 1; }
ensure_init_system() { :; }
ensure_container_runtime() { :; }
_ctr() {
  case "${2:-}" in
    ps) return 0 ;;
    *) return 1 ;;
  esac
}
stop_worker() { :; }
stop_tenant_vision() { :; }
_systemctl_user() {
  printf '%s\n' "$*" >>"$SYSTEMD_CALLS"
  if [[ "${2:-}" == is-active ]]; then
    printf 'inactive\n'
    return 3
  fi
  return 0
}

stop_tenant_writers "$TENANT"
forwarded="$(cat "$SYSTEMD_CALLS")"
for unit in \
  "lunarwing-${TENANT}.service" \
  "xmpp-bridge-${TENANT}.service" \
  "lunarwing-darkirc-${TENANT}.service" \
  "lunarwing-darkirc-adapter-${TENANT}.service"; do
  grep -Fq -- "$unit" <<<"$forwarded" || {
    printf 'systemd writer boundary omitted %s\n' "$unit" >&2
    exit 1
  }
done
if grep -Fq -- "lunarwing-pg-${TENANT}" <<<"$forwarded"; then
  printf 'writer stop must leave PostgreSQL running\n' >&2
  exit 1
fi

writer_rc=0
tenant_writers_active "$TENANT" || writer_rc=$?
[[ "$writer_rc" == 1 ]] || {
  printf 'inactive systemd writers returned %s\n' "$writer_rc" >&2
  exit 1
}

_systemctl_user() {
  if [[ "${2:-}" == is-active ]]; then
    printf 'deactivating\n'
    return 3
  fi
  return 0
}
writer_rc=0
tenant_writers_active "$TENANT" || writer_rc=$?
[[ "$writer_rc" == 0 ]] || {
  printf 'deactivating systemd unit was treated as quiesced\n' >&2
  exit 1
}

INIT_SYSTEM=openrc
OPENRC_INITD_DIR="$TEST_ROOT/init.d"
mkdir -p "$OPENRC_INITD_DIR"
while IFS= read -r service; do
  printf '#!/bin/sh\n' >"$OPENRC_INITD_DIR/$service"
  chmod +x "$OPENRC_INITD_DIR/$service"
done < <(tenant_writer_services "$TENANT")
rc-service() {
  printf '%s\n' "$*" >>"$OPENRC_CALLS"
  printf 'stopped\n'
  return 3
}
writer_rc=0
tenant_writers_active "$TENANT" || writer_rc=$?
[[ "$writer_rc" == 1 ]] || {
  printf 'stopped OpenRC writers returned %s\n' "$writer_rc" >&2
  exit 1
}

lock_active=false
readiness_checked=false
stop_checked=false
DARKIRC_WRITER_LOCK_HELD=false
darkirc_scope_id() { printf '00112233445566778899aabbccddeeff\n'; }
darkirc_writer_lock() {
  lock_active=true
  DARKIRC_WRITER_LOCK_HELD=true
}
darkirc_writer_unlock() {
  lock_active=false
  DARKIRC_WRITER_LOCK_HELD=false
}
run_darkirc_key_helper() {
  [[ "$lock_active" == true && "$2" == migration-ready ]] || return 1
  readiness_checked=true
}
stop_tenant() {
  [[ "$lock_active" == true ]] || return 1
  stop_checked=true
}
stop_tenant_for_upgrade "$TENANT"
[[ "$readiness_checked" == true && "$stop_checked" == true && "$lock_active" == false ]] || {
  printf 'upgrade readiness lock was not retained through tenant stop\n' >&2
  exit 1
}

printf 'ALL TESTS PASSED\n'
