#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"

failures=0
RC_STATUS=0
RC_OUTPUT=""

fail() {
  printf 'FAIL: %s\n' "$1"
  failures=$((failures + 1))
}

assert_eq() {
  local label="$1" actual="$2" expected="$3"
  if [[ "$actual" == "$expected" ]]; then
    printf 'PASS: %s\n' "$label"
  else
    fail "$label (expected '$expected', got '$actual')"
  fi
}

assert_contains() {
  local label="$1" output="$2" expected="$3"
  if [[ "$output" == *"$expected"* ]]; then
    printf 'PASS: %s\n' "$label"
  else
    fail "$label (missing '$expected')"
  fi
}

assert_not_contains() {
  local label="$1" output="$2" unexpected="$3"
  if [[ "$output" == *"$unexpected"* ]]; then
    fail "$label (found '$unexpected')"
  else
    printf 'PASS: %s\n' "$label"
  fi
}

rc-service() {
  printf '%s\n' "$RC_OUTPUT"
  return "$RC_STATUS"
}

assert_openrc_state() {
  local label="$1" output="$2" status="$3" expected="$4"
  RC_OUTPUT="$output"
  RC_STATUS="$status"
  assert_eq "$label" "$(openrc_service_state lunarwing-test)" "$expected"
}

assert_openrc_state "standard started status" " * status: started" 0 "started"
assert_openrc_state "standard stopped status" " * status: stopped" 3 "stopped"
assert_openrc_state "standard crashed status" " * status: crashed" 1 "crashed"
assert_openrc_state "successful custom status" " * WeeChat (alice) session is up" 0 "started"
assert_openrc_state "stopped custom WeeChat status" " * WeeChat (alice) session is not running" 1 "stopped"
assert_openrc_state "unrecognized status failure" "service does not exist" 1 "unknown"

sanitize_name() { printf '%s' "$1"; }
tenant_exists_in_registry() { return 0; }
ports_get() { printf '10000\n'; }
ensure_container_runtime() {
  export CONTAINER_RT="podman"
  export MT_ROOTLESS="false"
}
_ctr() { return 1; }
_vision_container_healthy() { return 1; }
tenant_voice_enabled() { return 1; }
tenant_darkirc_enabled() { return 1; }
ensure_init_system() { export INIT_SYSTEM="openrc"; }
say() { printf '%s\n' "$*"; }
rc-service() {
  if [[ "$1" == "lunarwing-weechat-alice" ]]; then
    printf ' * WeeChat (alice) session is up\n'
    return 0
  fi
  printf ' * status: stopped\n'
  return 3
}

tenant_status="$(status_tenant alice)"
assert_contains "healthy custom WeeChat status is started" "$tenant_status" \
  "lunarwing-weechat-alice: started"
assert_not_contains "healthy custom WeeChat status is never unknown" "$tenant_status" \
  "lunarwing-weechat-alice: unknown"

# WI#271: per-tenant worker-image presence lines in status output.
tenant_worker_enabled() { [[ "$2" == pebble ]]; }
_ctr() {
  shift   # drop the tenant name
  if [[ "${1:-}" == "image" && "${2:-}" == "inspect" ]]; then
    # Only the pebble image is present in this tenant's store.
    if [[ "${3:-}" == "lunarwing-worker-pebble:latest" ]]; then return 0; fi
    return 1
  fi
  return 1
}
tenant_status="$(status_tenant alice)"
assert_contains "status lists worker images" "$tenant_status" \
  "Worker images:"
assert_contains "selected and present worker image reports present" "$tenant_status" \
  "lunarwing-worker-pebble:latest: present"
assert_contains "unselected worker image is not a false missing" "$tenant_status" \
  "lunarwing-worker-opencode:latest: not selected for tenant"

(( failures == 0 )) || exit 1
printf 'ALL TESTS PASSED\n'
