#!/usr/bin/env bash
set -euo pipefail

# Test harness for the WI#268 `daemon-pid` resolution path
# (tenant_daemon_pid in mt-admin-lib/04-tenant-lifecycle.sh).
#
# Covers the review findings from the rejected !1261:
#  - the OpenRC child_pid record is validated AS WRITTEN: only surrounding
#    whitespace is stripped, so records mixing digits and non-digits
#    ("31337-not-a-number", "31337 4242") are rejected rather than collapsed
#    into the digits they contain (the old tr -cd '0-9' behavior)
#  - a resolved PID must be associated with the tenant: alive, owned by the
#    tenant user, and carrying the tenant home path + /lunarwing in its
#    command line — so a stale-but-reused PID of an unrelated process fails
#    closed instead of passing an audit as this tenant's daemon
#  - both the OpenRC (child_pid value record) and systemd (MainPID through
#    the tenant user manager) branches share these guarantees
#
# Fixture strategy: the association check reads real /proc state, so the
# "daemons" are real processes — copies of sleep(1) named `lunarwing` under a
# fixture tenant home — giving genuine /proc/<pid> directories and cmdlines.
# Ownership is the test user's real uid; a shadowed stat() produces the
# foreign-owner case.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d)"
export RC_SVCDIR="$fixture/openrc-svc"
declare -a FIXTURE_PIDS=()

cleanup() {
  local p
  for p in "${FIXTURE_PIDS[@]+\"${FIXTURE_PIDS[@]}\"}"; do
    kill "$p" 2>/dev/null || true
  done
  rm -rf "$fixture"
}
trap cleanup EXIT

failures=0
pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }

# shellcheck source=../lunarwing-mt-admin.sh
# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"

# Stub AFTER sourcing: mt-admin's real registry/init helpers must not run.
TEST_INIT=openrc
TEST_UID="$(id -u)"
sanitize_name() { printf '%s' "$1"; }
tenant_exists_in_registry() { return 0; }
ensure_init_system() { export INIT_SYSTEM="$TEST_INIT"; }
tenant_home() { printf '%s/home/%s' "$fixture" "$1"; }

# The fixture tenants are not real accounts; `id -u` must answer with the
# test user's uid for the ownership comparison.
real_id() { command id "$@"; }
id() {
  if [[ "${1:-}" == "-u" && "$#" -eq 2 ]]; then
    printf '%s' "$TEST_UID"
    return 0
  fi
  real_id "$@"
}

# Only the foreign-ownership case needs stat shadowed; everything else uses
# the real /proc-backed ownership (which IS the test user).
declare -A PID_OWNERS=()
real_stat() { command stat "$@"; }
stat() {
  local args=("$@") i path=""
  for i in "${!args[@]}"; do
    if [[ "${args[$i]}" == "-c" ]]; then
      path="${args[$((i + 2))]:-}"
      break
    fi
  done
  if [[ "$path" =~ ^/proc/([0-9]+)$ && -n "${PID_OWNERS[${BASH_REMATCH[1]}]:-}" ]]; then
    printf '%s\n' "${PID_OWNERS[${BASH_REMATCH[1]}]}"
    return 0
  fi
  real_stat "$@"
}

# Spawn a fixture "daemon": a real sleep(1) binary named `lunarwing` under a
# fixture home, so /proc/<pid>/cmdline genuinely carries the tenant path. The
# child's fds are detached from the caller's command-substitution pipe, or the
# substitution would block on the open pipe until the daemon exits.
spawn_fixture_daemon() {
  local home_dir="$1" bin_dir pid
  bin_dir="$home_dir/ic/target/release"
  mkdir -p "$bin_dir"
  cp -- "$(command -v sleep)" "$bin_dir/lunarwing"
  "$bin_dir/lunarwing" 300 >/dev/null 2>&1 &
  pid=$!
  FIXTURE_PIDS+=("$pid")
  printf '%s' "$pid"
}

acorn_bin="$fixture/home/acorn/lunarwing/ic/target/release/lunarwing"
mkdir -p "$(dirname "$acorn_bin")"
cp -- "$(command -v sleep)" "$acorn_bin"

svc_record="$RC_SVCDIR/options/lunarwing-acorn/child_pid"
mkdir -p "$(dirname "$svc_record")"

assert_resolves() {
  local label="$1" tenant="$2" expected="$3" got
  if got="$(tenant_daemon_pid "$tenant")"; then
    if [[ "$got" == "$expected" ]]; then
      pass "$label"
    else
      fail "$label (expected '$expected', got '$got')"
    fi
  else
    fail "$label (probe failed; got '$got')"
  fi
}

assert_unresolved() {
  local label="$1" tenant="$2" got
  if got="$(tenant_daemon_pid "$tenant" 2>/dev/null)"; then
    fail "$label (unexpectedly resolved to '$got')"
  else
    pass "$label"
  fi
}

# ── OpenRC branch ───────────────────────────────────────────────────────────

# 1. Clean record (no trailing newline, exactly as supervise-daemon writes
#    it) against a live, tenant-owned daemon resolves.
acorn_pid="$(spawn_fixture_daemon "$fixture/home/acorn/lunarwing")"
printf '%s' "$acorn_pid" >"$svc_record"
assert_resolves "openrc: clean record resolves" acorn "$acorn_pid"

# 2. Trailing newline / surrounding whitespace is tolerated.
printf '\n %s \n' "$acorn_pid" >"$svc_record"
assert_resolves "openrc: surrounding whitespace tolerated" acorn "$acorn_pid"

# 3. Digits mixed with non-digits are REJECTED as-written (review P2).
printf '%s-not-a-number' "$acorn_pid" >"$svc_record"
assert_unresolved "openrc: digits+letters record rejected" acorn

# 4. Multi-value (space-separated) record is rejected, not collapsed.
printf '%s 4242' "$acorn_pid" >"$svc_record"
assert_unresolved "openrc: multi-value record rejected" acorn

# 5. Alphabetic-only record is rejected.
printf 'not-a-pid' >"$svc_record"
assert_unresolved "openrc: alphabetic record rejected" acorn

# 6. Missing record is unresolved (not gate-off evidence).
rm -f "$svc_record"
assert_unresolved "openrc: missing record unresolved" acorn

# 7. Record pointing at a dead PID is unresolved.
dead_bin="$fixture/home/dead/lunarwing/ic/target/release/lunarwing"
mkdir -p "$(dirname "$dead_bin")"
cp -- "$(command -v sleep)" "$dead_bin"
"$dead_bin" 300 >/dev/null 2>&1 & dead_pid=$!
FIXTURE_PIDS+=("$dead_pid")
kill "$dead_pid" 2>/dev/null || true
# Bounded poll instead of `wait`: with an EXIT trap armed and other background
# jobs alive, bash's wait can defer indefinitely for the trap to run.
for _ in $(seq 1 50); do
  [[ -d "/proc/$dead_pid" ]] || break
  sleep 0.1
done
printf '%s' "$dead_pid" >"$svc_record"
assert_unresolved "openrc: dead pid rejected" acorn

# 8. Live tenant-owned PID whose cmdline is NOT the tenant daemon (a plain
#    sleep) is rejected — stale-but-reused PID defense.
printf '%s' "$acorn_pid" >"$svc_record"
plain_sleep_pid=""
( exec -a "worker" "$(command -v sleep)" 300 ) >/dev/null 2>&1 & plain_sleep_pid=$!
FIXTURE_PIDS+=("$plain_sleep_pid")
printf '%s' "$plain_sleep_pid" >"$svc_record"
assert_unresolved "openrc: reused pid with unrelated cmdline rejected" acorn

# 9. Foreign-owned PID (another user's live process) is rejected.
foreign_pid="$(spawn_fixture_daemon "$fixture/home/foreign/lunarwing")"
PID_OWNERS[$foreign_pid]=0
printf '%s' "$foreign_pid" >"$svc_record"
assert_unresolved "openrc: foreign-owned pid rejected" acorn
unset 'PID_OWNERS['"$foreign_pid"']'

# 10. Tenant-shaped daemon under a DIFFERENT tenant's home is rejected.
other_pid="$(spawn_fixture_daemon "$fixture/home/other/lunarwing")"
printf '%s' "$other_pid" >"$svc_record"
assert_unresolved "openrc: other tenant's daemon rejected" acorn

# ── systemd branch ──────────────────────────────────────────────────────────

_systemctl_user() {
  printf '%s' "${SYSTEMD_MAINPID:-}"
}

TEST_INIT=systemd

SYSTEMD_MAINPID="$acorn_pid"
assert_resolves "systemd: MainPID resolves with association checks" acorn "$acorn_pid"

SYSTEMD_MAINPID="$acorn_pid-not-a-number"
assert_unresolved "systemd: malformed MainPID rejected" acorn

SYSTEMD_MAINPID="0"
assert_unresolved "systemd: zero MainPID rejected" acorn

SYSTEMD_MAINPID=""
assert_unresolved "systemd: empty MainPID unresolved" acorn

SYSTEMD_MAINPID="  $acorn_pid  "
assert_resolves "systemd: surrounding whitespace tolerated" acorn "$acorn_pid"

PID_OWNERS[$acorn_pid]=0
SYSTEMD_MAINPID="$acorn_pid"
assert_unresolved "systemd: foreign-owned pid rejected" acorn
unset 'PID_OWNERS['"$acorn_pid"']'

SYSTEMD_MAINPID="$other_pid"
assert_unresolved "systemd: other tenant's daemon rejected" acorn

(( failures == 0 )) || exit 1
printf 'ALL TESTS PASSED\n'
