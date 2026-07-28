#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"

failures=0
CALLS=()

fail() {
  printf 'FAIL: %s\n' "$1"
  failures=$((failures + 1))
}

assert_call() {
  local expected="$1"
  local call
  for call in "${CALLS[@]}"; do
    [[ "$call" == "$expected" ]] && { printf 'PASS: %s\n' "$expected"; return; }
  done
  fail "missing call: $expected"
}

assert_before() {
  local first="$1" second="$2" first_index=-1 second_index=-1 index
  for index in "${!CALLS[@]}"; do
    [[ "${CALLS[$index]}" == "$first" ]] && first_index="$index"
    [[ "${CALLS[$index]}" == "$second" ]] && second_index="$index"
  done
  if [[ "$first_index" -ge 0 && "$second_index" -gt "$first_index" ]]; then
    printf 'PASS: %s before %s\n' "$first" "$second"
  else
    fail "expected '$first' before '$second'"
  fi
}

sanitize_name() { printf '%s' "$1"; }
tenant_exists_in_registry() { return 0; }
tenant_worker_enabled() { return 0; }
ensure_container_runtime() { CONTAINER_RT="podman"; MT_ROOTLESS="true"; }
ensure_init_system() { :; }
podman_supports_quadlet() { return 0; }
say() { :; }
_systemctl_user() { CALLS+=("systemctl:$*"); }
rc-service() { CALLS+=("rc:$*"); }
_ctr() {
  CALLS+=("ctr:$*")
  return 0
}
start_tenant_opencode() { CALLS+=("start-opencode:$1"); }

INIT_SYSTEM="systemd"
CALLS=()
stop_worker alice opencode
assert_call "systemctl:alice stop lunarwing-opencode-alice.service"

CALLS=()
kill_worker alice opencode
assert_call "systemctl:alice stop --no-block lunarwing-opencode-alice.service"
assert_call "ctr:alice stop --time 0 lunarwing-opencode-alice"
assert_before \
  "systemctl:alice stop --no-block lunarwing-opencode-alice.service" \
  "ctr:alice stop --time 0 lunarwing-opencode-alice"

INIT_SYSTEM="openrc"
CALLS=()
stop_worker alice opencode
assert_call "rc:lunarwing-opencode-alice-sup stop"
assert_call "ctr:alice stop lunarwing-opencode-alice"
assert_before "rc:lunarwing-opencode-alice-sup stop" "ctr:alice stop lunarwing-opencode-alice"

CALLS=()
start_worker alice opencode
assert_call "start-opencode:alice"

if (stop_worker alice arbitrary >/dev/null 2>&1); then
  fail "unknown worker accepted"
else
  printf 'PASS: unknown worker rejected\n'
fi

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all worker lifecycle tests passed\n'
