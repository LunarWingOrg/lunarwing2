#!/usr/bin/env bash
# The migration wrappers must use mt-admin's init-agnostic writer boundary.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$SCRIPT_DIR/../lunarwing-mt-admin.sh"

source "$ADMIN_SCRIPT"

TENANT="fixture"
INIT_SYSTEM="systemd"
tenant_exists_in_registry() { return 0; }
tenant_darkirc_enabled() { return 0; }
tenant_proxy_enabled() { return 1; }
tenant_worker_enabled() { return 1; }
ensure_init_system() { :; }
SYSTEMD_CALLS=()
_systemctl_user() {
  SYSTEMD_CALLS+=("$*")
  if [[ "${2:-}" == is-active ]]; then
    printf 'inactive\n'
    return 3
  fi
  return 1
}

stop_tenant_writers "$TENANT"
printf '%s\n' "${SYSTEMD_CALLS[@]}" | grep -Fq 'stop lunarwing-fixture.service'
printf '%s\n' "${SYSTEMD_CALLS[@]}" | grep -Fq 'stop xmpp-bridge-fixture.service'
if printf '%s\n' "${SYSTEMD_CALLS[@]}" | grep -Fq 'lunarwing-pg-fixture'; then
  printf 'writer stop must not stop PostgreSQL\n' >&2
  exit 1
fi

writer_rc=0
if tenant_writers_active "$TENANT"; then
  printf 'inactive writer probe reported active\n' >&2
  exit 1
else
  writer_rc=$?
fi
[[ "$writer_rc" -eq 1 ]] || {
  printf 'inactive writer probe did not return the inactive result\n' >&2
  exit 1
}

# The public dispatcher must preserve tenant_writers_active's exit status.  A
# stopped writer set is a normal export precondition and must report "stopped"
# with exit 1 rather than being misclassified as an indeterminate probe.
PORTS_INIT_CALLED=false
ports_registry_init() { PORTS_INIT_CALLED=true; }
require_root() { :; }
stopped_output=""
stopped_rc=0
stopped_output="$(main writers-active "$TENANT" 2>&1)" || stopped_rc=$?
[[ "$stopped_rc" -eq 1 ]] || {
  printf 'writers-active stopped dispatch returned rc=%s (output=%s)\n' "$stopped_rc" "$stopped_output" >&2
  exit 1
}
[[ "$stopped_output" == *stopped* ]] || {
  printf 'writers-active stopped dispatch omitted stopped status: %s\n' "$stopped_output" >&2
  exit 1
}

# A deactivating systemd unit is still an unsafe writer state for export.  It
# must be reported active, not treated as quiesced.
_systemctl_user() {
  SYSTEMD_CALLS+=("$*")
  if [[ "${2:-}" == is-active ]]; then
    printf 'deactivating\n'
    return 3
  fi
  return 1
}
deactivating_rc=0
if tenant_writers_active "$TENANT"; then
  deactivating_rc=0
else
  deactivating_rc=$?
fi
[[ "$deactivating_rc" -eq 0 ]] || {
  printf 'deactivating writer probe was treated as quiesced (rc=%s)\n' "$deactivating_rc" >&2
  exit 1
}

LOCK_ROOT="$(mktemp -d)"
chmod 0750 "$LOCK_ROOT"
DARKIRC_WRITER_LOCK_ROOT="$LOCK_ROOT"
darkirc_scope_id() { printf '00112233445566778899aabbccddeeff\n'; }
require_cmd() { :; }
darkirc_writer_lock "$TENANT"
[[ "$(stat -c '%a' "$LOCK_ROOT")" == 700 ]] || {
  printf 'root-owned DarkIRC lock directory was not mode 0700\n' >&2
  exit 1
}
[[ "$(stat -c '%a' "$LOCK_ROOT/darkirc-00112233445566778899aabbccddeeff.lock")" == 600 ]] || {
  printf 'root-owned DarkIRC lock file was not mode 0600\n' >&2
  exit 1
}
darkirc_writer_unlock

# An intermediate state-path symlink must be rejected before any privileged
# mkdir/chown/open can touch the symlink target.
symlink_root="$(mktemp -d)"
mkdir -p "$symlink_root/tenant/lunarwing" "$symlink_root/foreign-state"
ln -s "$symlink_root/foreign-state" "$symlink_root/tenant/lunarwing/state"
tenant_state_dir() { printf '%s\n' "$symlink_root/tenant/lunarwing/state"; }
if ( darkirc_prepare_tenant_dirs "$TENANT" ) >/dev/null 2>&1; then
  printf 'intermediate state symlink was accepted by directory preparation\n' >&2
  exit 1
fi
[[ ! -e "$symlink_root/foreign-state/darkirc" ]] || {
  printf 'writer lock mutated an intermediate symlink target\n' >&2
  exit 1
}
rm -rf "$symlink_root"

ENSURE_SCOPE_CALLED=false
ensure_darkirc_scope_id() { ENSURE_SCOPE_CALLED=true; return 1; }
darkirc_scope_id() { printf '00112233445566778899aabbccddeeff\n'; }
run_darkirc_key_helper() { :; }
tenant_darkirc_enabled() { return 0; }
darkirc_contact_helper list "$TENANT"
[[ "$ENSURE_SCOPE_CALLED" == false ]] || {
  printf 'read-only DarkIRC inspection mutated scope state\n' >&2
  exit 1
}

PORTS_INIT_CALLED=false
READONLY_REGISTRY_CHECKED=false
ports_registry_init() { PORTS_INIT_CALLED=true; }
ports_registry_require_readonly() { READONLY_REGISTRY_CHECKED=true; }
require_root() { :; }
main darkirc-contact list "$TENANT"
[[ "$PORTS_INIT_CALLED" == false && "$READONLY_REGISTRY_CHECKED" == true ]] || {
  printf 'read-only DarkIRC dispatch initialized or migrated the registry\n' >&2
  exit 1
}

# Recovery must be reachable through the supported mt-admin surface so an
# unfinished journal can be settled without invoking the helper directly.
RECOVER_CALLED=false
ports_registry_init() { :; }
tenant_exists_in_registry() { return 0; }
tenant_darkirc_enabled() { return 0; }
darkirc_contact_helper() {
  [[ "${1:-}" == recover && "${2:-}" == "$TENANT" ]] || return 1
  RECOVER_CALLED=true
}
main darkirc-contact recover "$TENANT"
[[ "$RECOVER_CALLED" == true ]] || {
  printf 'DarkIRC recovery was not dispatched through mt-admin\n' >&2
  exit 1
}
printf 'ALL TESTS PASSED\n'
