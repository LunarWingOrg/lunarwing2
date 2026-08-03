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
boundary_pid=""
lifecycle_writer_pid=""
writer_pid=""
cleanup() {
  local pid
  for pid in "$boundary_pid" "$lifecycle_writer_pid" "$writer_pid"; do
    [[ -z "$pid" ]] || kill "$pid" 2>/dev/null || true
  done
  rm -rf "$TEST_ROOT"
}
trap cleanup EXIT

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

# shellcheck disable=SC2034  # Consumed by sourced mt-admin functions.
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
# shellcheck disable=SC2034  # Consumed by sourced mt-admin functions.
DARKIRC_WRITER_LOCK_HELD=false
darkirc_scope_id() { printf '00112233445566778899aabbccddeeff\n'; }
darkirc_writer_lock() {
  lock_active=true
  DARKIRC_WRITER_LOCK_HELD=true
}
darkirc_writer_unlock() {
  lock_active=false
  # shellcheck disable=SC2034  # Updates sourced mt-admin lock state.
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

# Real flock regression with bounded waits. Child validates inherited descriptors
# using production helper. Lifecycle-path and direct writer-lock contenders must
# both remain blocked until seal child exits and holder descriptors close.
LIFECYCLE_LOCK="$TEST_ROOT/fixture.lifecycle.lock"
WRITER_LOCK="$TEST_ROOT/fixture.writer.lock"
BOUNDARY_ENTERED="$TEST_ROOT/boundary-entered"
BOUNDARY_RELEASE="$TEST_ROOT/boundary-release"
LIFECYCLE_ATTEMPTED="$TEST_ROOT/lifecycle-attempted"
LIFECYCLE_COMPLETED="$TEST_ROOT/lifecycle-completed"
WRITER_ATTEMPTED="$TEST_ROOT/writer-attempted"
WRITER_COMPLETED="$TEST_ROOT/writer-completed"

wait_for_file() {
  local path="$1" pid="$2" label="$3"
  for _ in {1..500}; do
    [[ -e "$path" ]] && return 0
    kill -0 "$pid" 2>/dev/null || {
      printf '%s exited before signaling\n' "$label" >&2
      return 1
    }
    sleep 0.01
  done
  printf 'timed out waiting for %s\n' "$label" >&2
  return 1
}

exec {unlocked_fd}>"$LIFECYCLE_LOCK"
if inherited_lock_fd_is_held "$unlocked_fd" "$LIFECYCLE_LOCK"; then
  printf 'unlocked lifecycle descriptor passed held-state validation\n' >&2
  exit 1
fi
exec {wrong_fd}>"$TEST_ROOT/wrong.lock"
flock -x "$wrong_fd"
if inherited_lock_fd_is_held "$wrong_fd" "$LIFECYCLE_LOCK"; then
  printf 'wrong-inode lifecycle descriptor passed validation\n' >&2
  exit 1
fi
exec {locked_fd}>"$LIFECYCLE_LOCK"
flock -x "$locked_fd"
exec {same_inode_unlocked_fd}>"$LIFECYCLE_LOCK"
if inherited_lock_fd_is_held "$same_inode_unlocked_fd" "$LIFECYCLE_LOCK"; then
  printf 'unlocked same-inode descriptor passed while different FD held lock\n' >&2
  exit 1
fi
inherited_lock_fd_is_held "$locked_fd" "$LIFECYCLE_LOCK" || {
  printf 'owning same-inode descriptor failed held-state validation\n' >&2
  exit 1
}
exec {unlocked_fd}>&-
flock -u "$wrong_fd"
exec {wrong_fd}>&-
exec {same_inode_unlocked_fd}>&-
flock -u "$locked_fd"
exec {locked_fd}>&-

(
  exec {lifecycle_fd}>"$LIFECYCLE_LOCK"
  flock -x "$lifecycle_fd"
  exec {writer_fd}>"$WRITER_LOCK"
  flock -x "$writer_fd"
  bash -c '
    set -euo pipefail
    source "$1"
    inherited_lock_fd_is_held "$2" "$3"
    inherited_lock_fd_is_held "$4" "$5"
    : >"$6"
    for _attempt in {1..500}; do
      [[ -e "$7" ]] && exit 0
      sleep 0.01
    done
    printf "seal child timed out waiting for release\n" >&2
    exit 1
  ' _ "$ADMIN_SCRIPT" "$lifecycle_fd" "$LIFECYCLE_LOCK" \
    "$writer_fd" "$WRITER_LOCK" "$BOUNDARY_ENTERED" "$BOUNDARY_RELEASE"
) &
boundary_pid=$!
wait_for_file "$BOUNDARY_ENTERED" "$boundary_pid" "seal child"

(
  : >"$LIFECYCLE_ATTEMPTED"
  exec {fd}>"$LIFECYCLE_LOCK"
  flock -x -w 5 "$fd"
  exec {dark_fd}>"$WRITER_LOCK"
  flock -x -w 5 "$dark_fd"
  : >"$LIFECYCLE_COMPLETED"
) &
lifecycle_writer_pid=$!
(
  : >"$WRITER_ATTEMPTED"
  exec {fd}>"$WRITER_LOCK"
  flock -x -w 5 "$fd"
  : >"$WRITER_COMPLETED"
) &
writer_pid=$!
wait_for_file "$LIFECYCLE_ATTEMPTED" "$lifecycle_writer_pid" "lifecycle writer"
wait_for_file "$WRITER_ATTEMPTED" "$writer_pid" "DarkIRC writer"
[[ ! -e "$LIFECYCLE_COMPLETED" && ! -e "$WRITER_COMPLETED" ]] || {
  printf 'concurrent writer crossed retained export boundary\n' >&2
  exit 1
}

: >"$BOUNDARY_RELEASE"
wait "$boundary_pid"
wait_for_file "$LIFECYCLE_COMPLETED" "$lifecycle_writer_pid" "released lifecycle writer"
wait_for_file "$WRITER_COMPLETED" "$writer_pid" "released DarkIRC writer"
wait "$lifecycle_writer_pid"
wait "$writer_pid"

printf 'ALL TESTS PASSED\n'
