#!/usr/bin/env bash
# Migration wrappers use mt-admin init abstraction and preserve writer-state semantics.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT
source "$ADMIN_SCRIPT"

TENANT=fixture
INIT_SYSTEM=systemd
tenant_exists_in_registry() { return 0; }
ensure_init_system() { :; }
ensure_container_runtime() { :; }
stop_worker() { :; }
stop_tenant_vision() { :; }
CTR_MODE=empty
_ctr() {
  if [[ "${2:-}" == ps ]]; then
    [[ "$CTR_MODE" == active ]] && printf 'lunarwing-kimi-fixture\n'
    return 0
  fi
  if [[ "${2:-}" == inspect && "$CTR_MODE" == active ]]; then
    printf 'true\n'
    return 0
  fi
  return 1
}
_systemctl_user() {
  if [[ "${2:-}" == is-active ]]; then
    printf 'inactive\n'
    return 3
  fi
  return 0
}

stop_tenant_writers "$TENANT" >/dev/null
calls="$(tenant_writer_services "$TENANT")"
grep -Fq 'lunarwing-fixture' <<<"$calls"
grep -Fq 'xmpp-bridge-fixture' <<<"$calls"
if grep -Fq 'lunarwing-pg-fixture' <<<"$calls"; then
  printf 'writer stop touched PostgreSQL\n' >&2
  exit 1
fi

writer_rc=0
tenant_writers_active "$TENANT" || writer_rc=$?
[[ "$writer_rc" -eq 1 ]]

_systemctl_user() {
  [[ "${2:-}" == is-active ]] && { printf 'deactivating\n'; return 3; }
  return 0
}
tenant_writers_active "$TENANT"

_systemctl_user() { printf 'inactive\n'; return 3; }
CTR_MODE=active
tenant_writers_active "$TENANT"
CTR_MODE=empty

CONTAINER_RT=podman
MT_ROOTLESS=false
_ctr_tenant_store() {
  if [[ "${2:-}" == ps ]]; then printf 'lunarwing-vision-fixture\n'; return 0; fi
  if [[ "${2:-}" == inspect ]]; then printf 'true\n'; return 0; fi
  return 1
}
tenant_writers_active "$TENANT"
CONTAINER_RT=""
MT_ROOTLESS=""

ports_registry_init() { : >"$TMP_ROOT/ports-init"; }
ports_registry_require_readonly() { : >"$TMP_ROOT/readonly-check"; }
require_root() { :; }
with_tenant_lifecycle_lock() { shift; "$@"; }
tenant_writers_active() { return 1; }
stopped_rc=0
stopped_output="$(main writers-active "$TENANT" 2>&1)" || stopped_rc=$?
[[ "$stopped_rc" -eq 1 && "$stopped_output" == *stopped* ]]
[[ ! -e "$TMP_ROOT/ports-init" && -e "$TMP_ROOT/readonly-check" ]]

tenant_writers_active() { return 2; }
unknown_rc=0
unknown_output="$(main writers-active "$TENANT" 2>&1)" || unknown_rc=$?
[[ "$unknown_rc" -eq 2 && "$unknown_output" == *'could not determine'* ]]

printf 'ALL TESTS PASSED\n'
