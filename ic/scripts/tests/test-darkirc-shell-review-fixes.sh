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
# EQ-2: stops now target installed init scripts only, so the fixture must
# install the DarkIRC units for the failing stop to be exercised at all.
export OPENRC_INITD_DIR="$fixture/init.d"
mkdir -p "$OPENRC_INITD_DIR"
printf '# fixture\n' >"$OPENRC_INITD_DIR/lunarwing-darkirc-fixture"
printf '# fixture\n' >"$OPENRC_INITD_DIR/lunarwing-darkirc-adapter-fixture"
if (stop_tenant_openrc fixture) >/dev/null 2>&1; then
  fail "OpenRC DarkIRC stop failure was hidden"
else
  pass "OpenRC DarkIRC stop failure propagates"
fi

tenant_exists_in_registry() { return 0; }
tenant_state_dir() { printf '%s/state\n' "$fixture"; }
tenant_env_dir() { printf '%s/env\n' "$fixture"; }
tenant_run_dir() { printf '%s/run\n' "$fixture"; }
tenant_home() { printf '%s\n' "$fixture"; }
ensure_init_system() { :; }
mkdir -p "$fixture/state/darkirc" "$fixture/env" "$fixture/run"
: >"$fixture/env/lunarwing.env"

DARKIRC_RELEASE_DAEMON="$fixture/releases/current/darkirc"
systemd_unit="$fixture/.config/systemd/user/lunarwing-darkirc-fixture.service"
mkdir -p "${systemd_unit%/*}"
printf '[Service]\nExecStart=%s --config %s/state/darkirc/darkirc_config.toml\n' \
  "$DARKIRC_RELEASE_DAEMON" "$fixture" >"$systemd_unit"
systemd_drop_ins=""
_systemctl_user() {
  shift
  if [[ "$1" == daemon-reload ]]; then
    return 0
  elif [[ "$*" == *FragmentPath* ]]; then
    printf '%s\n' "$systemd_unit"
  elif [[ "$*" == *DropInPaths* ]]; then
    printf '%s\n' "$systemd_drop_ins"
  else
    return 1
  fi
}
INIT_SYSTEM=systemd
darkirc_service_uses_release_selector fixture \
  || fail "selector-backed systemd unit was rejected"
systemd_drop_ins="$fixture/override.conf"
if darkirc_service_uses_release_selector fixture; then
  fail "systemd unit with command drop-in was accepted"
fi
systemd_drop_ins=""
printf 'ExecStart=/usr/local/bin/darkirc --config stale\n' >>"$systemd_unit"
if darkirc_service_uses_release_selector fixture; then
  fail "systemd unit with second stale command was accepted"
fi

OPENRC_INITD_DIR="$fixture/openrc"
mkdir -p "$OPENRC_INITD_DIR"
printf 'darkirc_command="%s"\n' "$DARKIRC_RELEASE_DAEMON" \
  >"$OPENRC_INITD_DIR/lunarwing-darkirc-fixture"
# Consumed by sourced mt-admin service validator.
# shellcheck disable=SC2034
INIT_SYSTEM=openrc
darkirc_service_uses_release_selector fixture \
  || fail "selector-backed OpenRC unit was rejected"
printf 'darkirc_command="/usr/local/bin/darkirc"\n' \
  >>"$OPENRC_INITD_DIR/lunarwing-darkirc-fixture"
if darkirc_service_uses_release_selector fixture; then
  fail "OpenRC unit with second stale command was accepted"
fi
pass "stale DarkIRC service commands rejected"

darkirc_validate_root_executable() { [[ -f "$1" ]]; }
darkirc_load_compatibility() { return 0; }
darkirc_service_uses_release_selector() { return 0; }
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
