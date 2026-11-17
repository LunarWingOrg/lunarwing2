#!/usr/bin/env bash
# WI#730: `LUNARWING_SERVICE_MANAGER` must not override the host it runs on.
#
# Pins both acceptance directions (openrc on a systemd host, systemd on an
# OpenRC host): detection refuses before anything is written, and add-tenant
# never reaches the account/registry/other mutators. Also pins the forcing
# switch (refused while the host has tenants under the manager it runs) and the
# health pipeline's manager-of-record (always the probe, never a forced value).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"

# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

failures=0
pass() { printf 'PASS: %s\n' "$1"; }
fail() {
  printf 'FAIL: %s\n' "$1"
  failures=$((failures + 1))
}

assert_eq() {  # <label> <actual> <expected>
  if [[ "$2" == "$3" ]]; then
    pass "$1"
  else
    fail "$1 (expected '$3', got '$2')"
  fi
}

assert_nonzero() {  # <label> <status>
  if [[ "$2" != "0" ]]; then
    pass "$1"
  else
    fail "$1 (exited 0)"
  fi
}

assert_contains() {  # <label> <output> <needle>
  if [[ "$2" == *"$3"* ]]; then
    pass "$1"
  else
    fail "$1 (missing '$3' in: $2)"
  fi
}

# Point the probe at fixture artifacts. Production reads /run; this is the same
# seam the sourced-command harnesses use, and keeps the test deterministic on
# systemd and OpenRC hosts alike.
simulate_host() {  # <openrc|systemd|none>
  rm -rf "$fixture/host-markers"
  mkdir -p "$fixture/host-markers"
  OPENRC_HOST_MARKER="$fixture/host-markers/openrc"
  SYSTEMD_HOST_MARKER="$fixture/host-markers/systemd"
  case "$1" in
    openrc)  : >"$OPENRC_HOST_MARKER" ;;
    systemd) mkdir -p "$SYSTEMD_HOST_MARKER" ;;
    none)    : ;;
  esac
}

printf '=== override vs host probe ===\n'

simulate_host systemd
rc=0
out="$(LUNARWING_SERVICE_MANAGER=openrc detect_init_system 2>&1)" || rc=$?
assert_nonzero "openrc override on a systemd host is refused" "$rc"
assert_contains "refusal names what was asked for" "$out" "LUNARWING_SERVICE_MANAGER=openrc"
assert_contains "refusal names what was found" "$out" "this host runs systemd"
assert_contains "refusal names the probe artifact" "$out" "$SYSTEMD_HOST_MARKER"

simulate_host openrc
rc=0
out="$(LUNARWING_SERVICE_MANAGER=systemd detect_init_system 2>&1)" || rc=$?
assert_nonzero "systemd override on an OpenRC host is refused" "$rc"
assert_contains "refusal names what was asked for" "$out" "LUNARWING_SERVICE_MANAGER=systemd"
assert_contains "refusal names what was found" "$out" "this host runs openrc"
assert_contains "refusal names the probe artifact" "$out" "$OPENRC_HOST_MARKER"

simulate_host systemd
assert_eq "matching override is accepted" "$(LUNARWING_SERVICE_MANAGER=systemd detect_init_system)" "systemd"
assert_eq "matching systemd-user alias is normalized" "$(LUNARWING_SERVICE_MANAGER=systemd-user detect_init_system)" "systemd"

# A chroot or image build has no PID-1 marker; that is not a contradiction.
simulate_host none
rc=0
out="$(LUNARWING_SERVICE_MANAGER=openrc detect_init_system 2>"$fixture/none.err")" || rc=$?
assert_eq "override accepted when no host marker exists" "$rc" "0"
assert_eq "override returned without a probe" "$out" "openrc"
assert_contains "missing-probe warning names the artifacts" "$(<"$fixture/none.err")" "no host init marker"

printf '=== add-tenant stops before any write ===\n'

# Any of these would mean add-tenant got past the preflight. They are stubs, so
# no account, registry or /etc file can be touched by this test.
mutators="$fixture/mutators"
useradd() { printf 'useradd\n' >>"$mutators"; }
ports_registry_init() { printf 'registry\n' >>"$mutators"; }
ports_allocate() { printf 'allocate\n' >>"$mutators"; }
create_tenant_user() { printf 'create-user\n' >>"$mutators"; }

for direction in "systemd:openrc" "openrc:systemd"; do
  host="${direction%%:*}"
  asked="${direction##*:}"
  simulate_host "$host"
  PORTS_REGISTRY="$fixture/ports-$asked.json"
  rc=0
  (
    simulate_host "$host"
    LUNARWING_SERVICE_MANAGER="$asked" add_tenant probe
  ) >"$fixture/add-$asked.log" 2>&1 || rc=$?
  assert_nonzero "add-tenant refuses a $asked override on a $host host" "$rc"
  assert_contains "add-tenant refusal names the ask" "$(<"$fixture/add-$asked.log")" "LUNARWING_SERVICE_MANAGER=$asked"
  assert_contains "add-tenant refusal names the host" "$(<"$fixture/add-$asked.log")" "this host runs $host"
  if [[ -e "$mutators" ]]; then
    fail "add-tenant reached a mutator ($(tr '\n' ' ' <"$mutators")) before refusing"
  else
    pass "add-tenant reached no account/registry mutator before refusing"
  fi
  if [[ -e "$PORTS_REGISTRY" ]]; then
    fail "add-tenant created a registry entry before refusing"
  else
    pass "add-tenant left the registry untouched"
  fi
done

printf '=== forcing switch ===\n'

simulate_host systemd
printf '{"version":18,"tenants":{"alice":{"base":10000}}}\n' >"$fixture/ports-tenanted.json"
mkdir -p "$fixture/home/alice/.config/systemd/user"
: >"$fixture/home/alice/.config/systemd/user/lunarwing-alice.service"
PORTS_REGISTRY="$fixture/ports-tenanted.json"
rc=0
out="$(
  # shellcheck disable=SC2329  # called by _registered_tenants_under_manager below
  tenant_home() { printf '%s/home/%s' "$fixture" "$1"; }
  LUNARWING_SERVICE_MANAGER=openrc LUNARWING_SERVICE_MANAGER_FORCE=1 detect_init_system 2>&1
)" || rc=$?
assert_nonzero "force is refused while systemd tenants are registered" "$rc"
assert_contains "force refusal names the host manager" "$out" "tenants registered under systemd"

printf '{"version":18,"tenants":{}}\n' >"$fixture/ports-empty.json"
PORTS_REGISTRY="$fixture/ports-empty.json"
rc=0
out="$(LUNARWING_SERVICE_MANAGER=openrc LUNARWING_SERVICE_MANAGER_FORCE=1 detect_init_system 2>"$fixture/force.err")" || rc=$?
assert_eq "force is allowed when the host has no tenants" "$rc" "0"
assert_eq "force returns the asked-for manager" "$out" "openrc"
assert_contains "force warns and names the host manager" "$(<"$fixture/force.err")" "forcing openrc on a systemd host"

printf '=== health pipeline manager of record ===\n'

simulate_host systemd
# shellcheck disable=SC2034  # read by health_service_manager_of_record
INIT_SYSTEM="openrc"
assert_eq "forced init never becomes the health manager" "$(health_service_manager_of_record)" "systemd"
simulate_host none
assert_eq "chroot keeps the configured manager" "$(health_service_manager_of_record)" "openrc"

(( failures == 0 )) || {
  printf '%d assertion(s) failed\n' "$failures" >&2
  exit 1
}
printf 'ALL TESTS PASSED\n'
