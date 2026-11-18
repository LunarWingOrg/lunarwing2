#!/usr/bin/env bash
# WI#730/WI#757: `LUNARWING_SERVICE_MANAGER` must not override the host it runs on.
#
# Pins both acceptance directions (openrc on a systemd host, systemd on an
# OpenRC host): detection refuses before anything is written, and add-tenant
# never reaches the account/registry/other mutators. WI#757 adds the dispatcher
# gate: every command, including render-units and stop-tenant under the real
# lifecycle lock, stops at the same one line before its first write; a forced
# run and a marker-less run each print their warning once in the command's
# output. Also pins the forcing switch (refused while the registry holds any
# tenant) and the health pipeline's manager-of-record (the probe, never a
# forced value).
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

  # The same refusal at the CLI boundary: main dispatch reaches add_tenant and
  # exits non-zero. Root and lock are stubbed so this exercises the command,
  # not the host's privilege state.
  rc=0
  (
    simulate_host "$host"
    # shellcheck disable=SC2329  # stubs invoked indirectly by main
    require_root() { :; }
    # shellcheck disable=SC2329  # stub invoked indirectly by main
    openrc_dispatch_preflight() { return 0; }
    # shellcheck disable=SC2329  # stub invoked indirectly by main
    with_tenant_lifecycle_lock() { shift; "$@"; }
    LUNARWING_SERVICE_MANAGER="$asked" main add-tenant probe --llm-base-url http://fixture.invalid
  ) >"$fixture/cli-$asked.log" 2>&1 || rc=$?
  assert_nonzero "add-tenant command exits non-zero for a $asked override on $host" "$rc"
  assert_contains "command refusal names the ask" "$(<"$fixture/cli-$asked.log")" "LUNARWING_SERVICE_MANAGER=$asked"
  assert_contains "command refusal names the host" "$(<"$fixture/cli-$asked.log")" "this host runs $host"
done

printf '=== forcing switch ===\n'

simulate_host systemd
# No unit file for alice: a failed add-tenant leaves a registry entry without
# one, and that shape must not unlock the forcing switch (WI#757 AFI-6).
printf '{"version":18,"tenants":{"alice":{"base":10000}}}\n' >"$fixture/ports-tenanted.json"
PORTS_REGISTRY="$fixture/ports-tenanted.json"
rc=0
out="$(LUNARWING_SERVICE_MANAGER=openrc LUNARWING_SERVICE_MANAGER_FORCE=1 detect_init_system 2>&1)" || rc=$?
assert_nonzero "force is refused while a registry entry exists under the host manager" "$rc"
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

printf '=== lifecycle commands stop before the lock ===\n'

# AFI-2 measured render-units and stop-tenant writing files after printing the
# refusal. Both run here through the real dispatcher preflight and, for
# stop-tenant, through the real with_tenant_lifecycle_lock — which turns errexit
# off around its command, so a gate inside the handler could not stop it. The
# handler stubs record any reach past the gate; the lock directory records
# whether the lock was ever taken.
STOP_MUTATORS=""
render_tenant_units() { printf 'render\n' >>"$STOP_MUTATORS"; }
stop_tenant() { printf 'stop\n' >>"$STOP_MUTATORS"; }
# shellcheck disable=SC2329  # called indirectly by the dispatcher
ports_registry_init() { :; }

for command in render-units stop-tenant; do
  for direction in "systemd:openrc" "openrc:systemd"; do
    host="${direction%%:*}"
    asked="${direction##*:}"
    STOP_MUTATORS="$fixture/mutators-$command-$asked"
    rm -f "$STOP_MUTATORS"
    lock_dir="$fixture/locks-$command-$asked"
    rm -rf "$lock_dir"
    mkdir -p "$lock_dir"
    rc=0
    (
      simulate_host "$host"
      # shellcheck disable=SC2329  # stub invoked indirectly by main
      require_root() { :; }
      # shellcheck disable=SC2034  # read by with_tenant_lifecycle_lock
      TENANT_LIFECYCLE_LOCK_DIR="$lock_dir"
      LUNARWING_SERVICE_MANAGER="$asked" main "$command" probe
    ) >"$fixture/$command-$asked.log" 2>&1 || rc=$?
    assert_nonzero "$command refuses a $asked override on a $host host" "$rc"
    assert_contains "$command refusal names the ask" "$(<"$fixture/$command-$asked.log")" "LUNARWING_SERVICE_MANAGER=$asked"
    assert_contains "$command refusal names the host" "$(<"$fixture/$command-$asked.log")" "this host runs $host"
    assert_eq "$command prints the refusal once" \
      "$(grep -c 'refusing to write' "$fixture/$command-$asked.log" || true)" "1"
    if [[ -e "$STOP_MUTATORS" ]]; then
      fail "$command reached its handler ($(tr '\n' ' ' <"$STOP_MUTATORS")) before refusing"
    else
      pass "$command reached no handler before refusing"
    fi
    if [[ -n "$(ls -A "$lock_dir")" ]]; then
      fail "$command took the lifecycle lock before refusing"
    else
      pass "$command never took the lifecycle lock"
    fi
  done
done

printf '=== warnings appear once in a command output ===\n'

# AFI-3: the dispatcher's first detection used to run with stderr discarded and
# the result cached, so neither warning was ever shown. list-tenants is a real
# command that writes nothing; the gate's detection is its only run.
PORTS_REGISTRY="$fixture/ports-empty.json"
rc=0
(
  simulate_host systemd
  LUNARWING_SERVICE_MANAGER=openrc LUNARWING_SERVICE_MANAGER_FORCE=1 main list-tenants
) >"$fixture/forced.log" 2>&1 || rc=$?
assert_eq "a forced command exits 0" "$rc" "0"
assert_eq "a forced command prints its warning once" \
  "$(grep -c 'forcing openrc on a systemd host' "$fixture/forced.log" || true)" "1"

rc=0
(
  simulate_host none
  LUNARWING_SERVICE_MANAGER=openrc main list-tenants
) >"$fixture/no-marker.log" 2>&1 || rc=$?
assert_eq "a marker-less command exits 0" "$rc" "0"
assert_eq "a marker-less command prints its warning once" \
  "$(grep -c 'no host init marker' "$fixture/no-marker.log" || true)" "1"

(( failures == 0 )) || {
  printf '%d assertion(s) failed\n' "$failures" >&2
  exit 1
}
printf 'ALL TESTS PASSED\n'
