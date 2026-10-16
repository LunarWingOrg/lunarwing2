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

# HF-2 (WI#393): the safe-unit guard runs on every OpenRC service operation,
# so one status pass re-checks every installed proxy unit once per service —
# the !1628 per-line-regex check cost ~50x a presence grep and pushed a
# 5-tenant `mt-admin status` past the onboarding verifier's 30 s budget. The
# contract is now one fresh body render + one byte comparison per unit, with
# verdicts cached per process by stat signature. Pin it with a 20-unit fleet:
# a cold pass renders each unit body exactly once, a warm pass renders none,
# and both passes together stay well under the verifier budget. The call
# counts are the deterministic pins; the wall budget only bounds the
# per-check fork storm.
(
  fixture="$(mktemp -d)"
  trap 'rm -rf "$fixture"' EXIT
  export OPENRC_INITD_DIR="$fixture/init.d"
  export OPENRC_CONFD_DIR="$fixture/conf.d"
  export OPENRC_PROXY_BACKUP_DIR="$fixture/backup-units"
  export PORTS_REGISTRY="$fixture/ports.json"
  mkdir -p "$OPENRC_INITD_DIR" "$OPENRC_CONFD_DIR"
  printf '{"tenants":{}}' >"$PORTS_REGISTRY"
  tenant_home() { printf '%s/%s' "$fixture" "$1"; }
  install_openrc_env_exec() { :; }
  tenants=()
  for i in $(seq 0 19); do tenants+=("t$(printf '%02d' "$i")"); done
  ports_get() { local idx="${1#t}"; printf '%d\n' "$((23100 + 10#$idx))"; }
  for t in "${tenants[@]}"; do
    render_proxy_openrc_unit "$t" || exit 1
  done
  # Count expected-body renders through a wrapper (file append survives the
  # checker's process-substitution subshells).
  body_calls="$fixture/body-calls"
  : >"$body_calls"
  eval "_hf2_orig_body$(declare -f _openrc_proxy_unit_body | sed '1s/^_openrc_proxy_unit_body//')"
  _openrc_proxy_unit_body() { printf 'x\n' >>"$body_calls"; _hf2_orig_body "$@"; }
  start="$EPOCHREALTIME"
  status_tenant t00 >/dev/null || exit 1
  cold_calls="$(wc -l <"$body_calls")"
  : >"$body_calls"
  status_tenant t00 >/dev/null || exit 1
  end="$EPOCHREALTIME"
  warm_calls="$(wc -l <"$body_calls")"
  [[ "$cold_calls" == 20 ]] || {
    printf 'FAIL: cold 20-unit status rendered %s expected bodies, want 20 (verdict cache broken)\n' "$cold_calls"
    exit 1
  }
  [[ "$warm_calls" == 0 ]] || {
    printf 'FAIL: warm 20-unit status rendered %s expected bodies, want 0 (verdict cache broken)\n' "$warm_calls"
    exit 1
  }
  elapsed_ms="$(awk -v s="$start" -v e="$end" 'BEGIN{printf "%d", (e - s) * 1000}')"
  [[ "$elapsed_ms" -lt 15000 ]] || {
    printf 'FAIL: two 20-unit fleet status passes took %s ms (budget 15000)\n' "$elapsed_ms"
    exit 1
  }
  printf 'PASS: HF-2 20-unit fleet status cold=20 renders warm=0 renders, %s ms for both passes\n' "$elapsed_ms"
)

(( failures == 0 )) || exit 1
printf 'ALL TESTS PASSED\n'
