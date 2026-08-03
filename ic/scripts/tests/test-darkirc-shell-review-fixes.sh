#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d)"
holder_pid=""
trap '[[ -z "$holder_pid" ]] || kill "$holder_pid" 2>/dev/null || true; rm -rf "$fixture"' EXIT

fail() { printf 'FAIL: %s\n' "$1" >&2; exit 1; }
pass() { printf 'PASS: %s\n' "$1"; }

tenant_darkirc_enabled() { return 0; }
tenant_weechat_enabled() { return 1; }
tenant_proxy_enabled() { return 1; }
tenant_worker_enabled() { return 1; }
rc-update() { return 0; }

rc-service() {
  [[ "$1 $2" != "lunarwing-darkirc-fixture start" ]]
}
if (start_tenant_openrc fixture) >/dev/null 2>&1; then
  fail "OpenRC DarkIRC start failure was hidden"
else
  pass "OpenRC DarkIRC start failure propagates"
fi

rc-service() {
  [[ "$1 $2" != "lunarwing-darkirc-adapter-fixture stop" ]]
}
if (stop_tenant_openrc fixture) >/dev/null 2>&1; then
  fail "OpenRC DarkIRC stop failure was hidden"
else
  pass "OpenRC DarkIRC stop failure propagates"
fi

tenant_exists_in_registry() { return 0; }
tenant_state_dir() { printf '%s/state\n' "$fixture"; }
tenant_env_dir() { printf '%s/env\n' "$fixture"; }
tenant_run_dir() { printf '%s/run\n' "$fixture"; }
mkdir -p "$fixture/state/darkirc" "$fixture/env" "$fixture/run"
: >"$fixture/env/lunarwing.env"
DARKIRC_KEY_HELPER="$fixture/helper"
DARKIRC_COMPAT_FILE="$fixture/compat.json"
: >"$DARKIRC_KEY_HELPER"
: >"$DARKIRC_COMPAT_FILE"
darkirc_validate_root_executable() { [[ -f "$1" ]]; }
generate_darkirc_config() {
  printf 'generated\n' >"$fixture/state/darkirc/darkirc_config.toml"
}
retire_tenant_nanocode() { :; }
_owner_scope_needs_migration() { return 1; }
start_tenant_postgres() { :; }
start_tenant_vision() { :; }
with_tenant_lifecycle_lock() { shift; "$@"; }
_start_tenant_init_services() {
  [[ -f "$fixture/state/darkirc/darkirc_config.toml" ]]
}
tenant_home() { printf '%s\n' "$fixture"; }
warn_if_sshd_unreachable() { :; }
upload_tenant_darkirc_secret() { :; }
_ssh_ready_summary() { :; }
sudo() {
  if [[ "${1:-}" == -u ]]; then shift 2; fi
  command "$@"
}
start_tenant fixture >/dev/null
[[ -f "$fixture/state/darkirc/darkirc_config.toml" ]] \
  || fail "deferred DarkIRC config was not generated"
pass "deferred DarkIRC config generated before init start"

BUILD_LOCK="$fixture/build.lock"
tenant_darkirc_enabled() { return 1; }
upgrade_wrapper_child() {
  : >"$fixture/entered"
  while [[ ! -e "$fixture/release" ]]; do sleep 0.01; done
}
(upgrade_wrapper_boundary_locked fixture) &
holder_pid=$!
for _ in {1..500}; do
  [[ -e "$fixture/entered" ]] && break
  kill -0 "$holder_pid" 2>/dev/null || fail "upgrade boundary exited early"
  sleep 0.01
done
[[ -e "$fixture/entered" ]] || fail "upgrade boundary did not enter"
exec {contender_fd}>"${BUILD_LOCK}.fixture.upgrade"
if flock -x -n "$contender_fd"; then
  fail "second upgrade crossed wrapper transaction lock"
fi
pass "upgrade lock retained across wrapper transaction"
: >"$fixture/release"
wait "$holder_pid"
holder_pid=""

if (LUNARWING_MT_UPGRADE_BOUNDARY_TENANT=fixture \
    validate_upgrade_wrapper_boundary fixture) >/dev/null 2>&1; then
  fail "forged upgrade boundary marker bypassed lock validation"
else
  pass "upgrade boundary rejects marker without inherited lock"
fi

printf 'ALL TESTS PASSED\n'
