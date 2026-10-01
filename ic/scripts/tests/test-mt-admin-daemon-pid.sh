#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
set -euo pipefail

# Test harness for the WI#268 `daemon-pid` resolution path
# (tenant_daemon_pid in mt-admin-lib/04-tenant-lifecycle.sh).
#
# Covers the review findings from !1261 and !1276:
#  - the OpenRC child_pid record is validated AS WRITTEN: only surrounding
#    whitespace is stripped, so records mixing digits and non-digits
#    ("31337-not-a-number", "31337 4242") are rejected rather than collapsed
#    into the digits they contain (the old tr -cd '0-9' behavior)
#  - embedded NUL bytes are rejected before conversion to a Bash string
#  - a resolved PID must be alive, tenant-owned, and executing the tenant's
#    configured daemon binary, not a worker with a tenant path in its argv
#  - both the OpenRC (child_pid value record) and systemd (MainPID through
#    the tenant user manager) branches share these guarantees
#
# Fixture strategy: the association check reads real /proc state, so the
# "daemons" are real processes — copies of sleep(1) named `lunarwing` under a
# fixture tenant home — giving genuine /proc/<pid> directories and executables.
# Ownership is the test user's real uid; a shadowed stat() produces the
# foreign-owner case.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d)"
export RC_SVCDIR="$fixture/openrc-svc"
declare -a FIXTURE_PIDS=()

cleanup() {
  local p
  for p in "${FIXTURE_PIDS[@]}"; do
    kill "$p" 2>/dev/null || true
  done
  for p in "${FIXTURE_PIDS[@]}"; do
    wait "$p" 2>/dev/null || true
  done
  FIXTURE_PIDS=()
  rm -rf "$fixture"
}
trap cleanup EXIT

failures=0
pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }

# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"

# Stub AFTER sourcing: mt-admin's real registry/init helpers must not run.
TEST_INIT=openrc
TEST_UID="$(id -u)"
PROFILE=release
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
# the real /proc-backed ownership (which IS the test user). The foreign UID
# must differ from TEST_UID even when the CI container runs as root.
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

# Every spawn runs in the parent shell, never in a command substitution. This
# keeps both $! and the cleanup array in the shell that can terminate/reap them.
FIXTURE_PID=""
track_fixture_process() {
  FIXTURE_PID=$!
  FIXTURE_PIDS+=("$FIXTURE_PID")
  local expected="$1" actual attempts
  expected="$(realpath -e -- "$expected")"
  # Do not race the child's exec when the probe immediately inspects /proc.
  for ((attempts = 0; attempts < 100; attempts++)); do
    actual="$(readlink -- "/proc/$FIXTURE_PID/exe" 2>/dev/null || true)"
    [[ "$actual" == "$expected" ]] && return 0
    sleep 0.01
  done
  fail "fixture process did not execute its expected binary"
  return 1
}

spawn_fixture_daemon() {
  local name="$1" bin_dir
  bin_dir="$(tenant_repo "$name")/target/$PROFILE"
  mkdir -p "$bin_dir"
  cp -- "$(command -v sleep)" "$bin_dir/lunarwing"
  "$bin_dir/lunarwing" 300 >/dev/null 2>&1 &
  track_fixture_process "$bin_dir/lunarwing"
}

acorn_bin="$(tenant_repo acorn)/target/$PROFILE/lunarwing"

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
  elif [[ -n "$got" ]]; then
    fail "$label (failed probe still printed '$got')"
  else
    pass "$label"
  fi
}

# ── OpenRC branch ───────────────────────────────────────────────────────────

# 1. Clean record (no trailing newline, exactly as supervise-daemon writes
#    it) against a live, tenant-owned daemon resolves.
spawn_fixture_daemon acorn
acorn_pid="$FIXTURE_PID"
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

# Bash command substitution strips NUL bytes unless the byte stream is checked
# first. Leading, embedded, and trailing NULs must not yield the live PID.
for position in leading embedded trailing; do
  case "$position" in
    leading) printf '\0%s' "$acorn_pid" >"$svc_record" ;;
    embedded) printf '%s\0%s' "${acorn_pid:0:1}" "${acorn_pid:1}" >"$svc_record" ;;
    trailing) printf '%s\0' "$acorn_pid" >"$svc_record" ;;
  esac
  assert_unresolved "openrc: $position NUL rejected" acorn
done
printf '\0' >"$svc_record"
assert_unresolved "openrc: NUL-only record rejected" acorn

# 5. Alphabetic-only record is rejected.
printf 'not-a-pid' >"$svc_record"
assert_unresolved "openrc: alphabetic record rejected" acorn

# 6. Missing record is unresolved (not gate-off evidence).
rm -f "$svc_record"
assert_unresolved "openrc: missing record unresolved" acorn

# 7. Record pointing at a dead PID is unresolved.
spawn_fixture_daemon dead
dead_pid="$FIXTURE_PID"
kill "$dead_pid" 2>/dev/null || true
wait "$dead_pid" 2>/dev/null || true
unset 'FIXTURE_PIDS[-1]'
printf '%s' "$dead_pid" >"$svc_record"
assert_unresolved "openrc: dead pid rejected" acorn

# 8. Live tenant-owned PID whose cmdline is NOT the tenant daemon (a plain
#    sleep) is rejected — stale-but-reused PID defense.
printf '%s' "$acorn_pid" >"$svc_record"
plain_sleep_pid=""
( exec -a "worker" "$(command -v sleep)" 300 ) >/dev/null 2>&1 & plain_sleep_pid=$!
track_fixture_process "$(command -v sleep)"
printf '%s' "$plain_sleep_pid" >"$svc_record"
assert_unresolved "openrc: reused pid with unrelated cmdline rejected" acorn

# Same UID, real Python executable, and the tenant's home path in an argument:
# all old command-line substring checks pass, but this is not the daemon.
python3 -c 'import time; time.sleep(300)' "$(tenant_lw_root acorn)/context/profile.json" >/dev/null 2>&1 &
track_fixture_process "$(command -v python3)"
worker_pid="$FIXTURE_PID"
printf '%s' "$worker_pid" >"$svc_record"
assert_unresolved "openrc: same-user worker with tenant-path argument rejected" acorn

# argv[0] itself is forgeable; require the actual executable, not its spelling.
( exec -a "$acorn_bin" "$(command -v sleep)" 300 ) >/dev/null 2>&1 &
track_fixture_process "$(command -v sleep)"
spoofed_pid="$FIXTURE_PID"
printf '%s' "$spoofed_pid" >"$svc_record"
assert_unresolved "openrc: spoofed daemon argv[0] rejected" acorn

# Even a binary named lunarwing under the same tenant home is not sufficient.
worker_bin="$(tenant_lw_root acorn)/workers/lunarwing"
mkdir -p "$(dirname "$worker_bin")"
cp -- "$(command -v sleep)" "$worker_bin"
"$worker_bin" 300 >/dev/null 2>&1 &
track_fixture_process "$worker_bin"
wrong_path_pid="$FIXTURE_PID"
printf '%s' "$wrong_path_pid" >"$svc_record"
assert_unresolved "openrc: same-name binary outside launch path rejected" acorn

# 9. Foreign-owned PID (another user's live process) is rejected.
foreign_pid="$acorn_pid"
PID_OWNERS[$foreign_pid]=$((TEST_UID + 1))
printf '%s' "$foreign_pid" >"$svc_record"
assert_unresolved "openrc: foreign-owned pid rejected" acorn
unset 'PID_OWNERS['"$foreign_pid"']'

# 10. Tenant-shaped daemon under a DIFFERENT tenant's home is rejected.
spawn_fixture_daemon other
other_pid="$FIXTURE_PID"
printf '%s' "$other_pid" >"$svc_record"
assert_unresolved "openrc: other tenant's daemon rejected" acorn

# ── systemd branch ──────────────────────────────────────────────────────────

_systemctl_user() {
  [[ "$*" == "acorn show lunarwing-acorn.service -p MainPID --value" ]] || return 1
  if [[ -n "${SYSTEMD_PID_RECORD:-}" ]]; then
    cat -- "$SYSTEMD_PID_RECORD"
  else
    printf '%s' "${SYSTEMD_MAINPID:-}"
  fi
  return "${SYSTEMD_STATUS:-0}"
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

PID_OWNERS[$acorn_pid]=$((TEST_UID + 1))
SYSTEMD_MAINPID="$acorn_pid"
assert_unresolved "systemd: foreign-owned pid rejected" acorn
unset 'PID_OWNERS['"$acorn_pid"']'

SYSTEMD_MAINPID="$other_pid"
assert_unresolved "systemd: other tenant's daemon rejected" acorn

SYSTEMD_MAINPID="$worker_pid"
assert_unresolved "systemd: same-user worker with tenant-path argument rejected" acorn

SYSTEMD_MAINPID="$spoofed_pid"
assert_unresolved "systemd: spoofed daemon argv[0] rejected" acorn

SYSTEMD_MAINPID="$wrong_path_pid"
assert_unresolved "systemd: same-name binary outside launch path rejected" acorn

SYSTEMD_PID_RECORD="$svc_record"
for position in leading embedded trailing; do
  case "$position" in
    leading) printf '\0%s' "$acorn_pid" >"$svc_record" ;;
    embedded) printf '%s\0%s' "${acorn_pid:0:1}" "${acorn_pid:1}" >"$svc_record" ;;
    trailing) printf '%s\0' "$acorn_pid" >"$svc_record" ;;
  esac
  assert_unresolved "systemd: $position NUL rejected" acorn
done
printf '\0' >"$svc_record"
assert_unresolved "systemd: NUL-only MainPID rejected" acorn
unset SYSTEMD_PID_RECORD

SYSTEMD_MAINPID="$acorn_pid"
SYSTEMD_STATUS=1
assert_unresolved "systemd: failed MainPID capture with numeric output rejected" acorn
unset SYSTEMD_STATUS

# The shared launch contract uses the configured profile, not a guessed path
# derived from the process's arguments. No compilation is needed for fixtures.
PROFILE=audit-fixture
spawn_fixture_daemon acorn
profile_pid="$FIXTURE_PID"
SYSTEMD_MAINPID="$profile_pid"
assert_resolves "systemd: configured profile executable resolves" acorn "$profile_pid"
SYSTEMD_MAINPID="$acorn_pid"
assert_unresolved "systemd: executable from a different profile rejected" acorn

TEST_INIT=openrc
printf '%s' "$profile_pid" >"$svc_record"
assert_resolves "openrc: configured profile executable resolves" acorn "$profile_pid"
printf '%s' "$acorn_pid" >"$svc_record"
assert_unresolved "openrc: executable from a different profile rejected" acorn

# Canonicalize a genuine symlinked launch path, not arbitrary argv spellings.
PROFILE=linked-fixture
linked_bin="$(tenant_repo acorn)/target/$PROFILE/lunarwing"
native_bin="$fixture/native-images/lunarwing"
mkdir -p "$(dirname "$linked_bin")" "$(dirname "$native_bin")"
cp -- "$(command -v sleep)" "$native_bin"
ln -s "$native_bin" "$linked_bin"
"$linked_bin" 300 >/dev/null 2>&1 &
track_fixture_process "$native_bin"
linked_pid="$FIXTURE_PID"
printf '%s' "$linked_pid" >"$svc_record"
assert_resolves "openrc: canonical symlinked launch executable resolves" acorn "$linked_pid"
TEST_INIT=systemd
SYSTEMD_MAINPID="$linked_pid"
assert_resolves "systemd: canonical symlinked launch executable resolves" acorn "$linked_pid"

# Exercise the EXIT-trap cleanup in the parent and verify every fixture was
# both terminated and reaped, rather than trusting an empty subshell array.
spawned_pids=("${FIXTURE_PIDS[@]}")
cleanup
for pid in "${spawned_pids[@]}"; do
  if [[ -d "/proc/$pid" ]]; then
    fail "fixture process $pid survived cleanup"
  fi
done
pass "fixture children terminated and reaped"

(( failures == 0 )) || exit 1
printf 'ALL TESTS PASSED\n'
