#!/usr/bin/env bash
set -euo pipefail

# Test harness for the `fix-podman-userns` repair verb (WI#142).
# Verifies:
#  - a healthy podman probe is a no-op (no stop/migrate/start calls)
#  - the userns-join failure signature triggers the full repair sequence:
#    stop tenant -> pause.pid cleanup -> `podman system migrate` as the
#    tenant -> start tenant -> verify probe
#  - pause.pid hygiene keeps a live pause process owned by the tenant and
#    removes stale (dead pid) and foreign-owned (reused pid) files
#  - failures that are NOT the userns-join signature never stop the tenant
#    and fail loudly instead
#  - a failing `podman system migrate` warns but the repair still completes
#    when the final probe recovers
#
# Stubbing notes: podman calls run inside `$( ... )` subshells, so every piece
# of stub state that must survive them lives in files, not shell variables.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"

export LUNARWING_SERVICE_MANAGER=systemd

# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
failures=0

pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }
assert_eq() {
  if [[ "$2" == "$3" ]]; then pass "$1"; else fail "$1 (want [$2], got [$3])"; fi
}

# ── Stubbed world ────────────────────────────────────────────────────────────

tenant_exists_in_registry() { return 0; }

# The real lifecycle lock creates a root-owned 0700 dir; tests run
# unprivileged, so re-enter the lock wrapper trivially (it only serializes
# real lifecycle operations).
with_tenant_lifecycle_lock() { shift; "$@"; }

ensure_container_runtime() { :; }
# shellcheck disable=SC2034  # Consumed by sourced mt-admin functions.
CONTAINER_RT="podman"
# shellcheck disable=SC2034  # Consumed by sourced mt-admin functions.
MT_ROOTLESS="true"

# Repair-step recording: file-based so records survive `die` subshells.
ORDER="$fixture/order"
: >"$ORDER"
note_step() { printf '%s\n' "$1" >>"$ORDER"; }

# Podman probe scripting: file-based failure counter (subshell-safe).
PS_FAILS_FILE="$fixture/ps_fails"
set_ps_fails() { printf '%s\n' "${1:-0}" >"$PS_FAILS_FILE"; }
pop_ps_fail() {
  local n
  n="$(cat "$PS_FAILS_FILE")"
  if [[ "$n" -gt 0 ]]; then
    printf '%s\n' "$((n - 1))" >"$PS_FAILS_FILE"
    return 0
  fi
  return 1
}

_ctr() { shift; _ctr_behave "$@"; }
_ctr_behave() { return 0; }

stop_tenant() { note_step STOP; }
start_tenant() { note_step START; }
# Capture the real hygiene implementation under a distinct name before the
# orchestration stub replaces it; cases 4-7 exercise the real one against a
# fixture runtime dir.
_clean_stale_pause_pid_real="$(declare -f clean_stale_pause_pid \
  | sed 's/^clean_stale_pause_pid[[:space:]]*(/_clean_stale_pause_pid_real(/')"
eval "$_clean_stale_pause_pid_real"
clean_stale_pause_pid() { note_step CLEAN_PAUSE; }

# `id`, `kill`, and `stat` are shadowed where the REAL clean_stale_pause_pid
# is exercised (cases 4-7), so it can run against a fixture runtime dir
# without /run/user or /proc access.
TEST_UID=20101
real_id() { command id "$@"; }
real_kill() { command kill "$@"; }
real_stat() { command stat "$@"; }
id() {
  if [[ "${1:-}" == "-u" ]]; then
    printf '%s' "$TEST_UID"
    return 0
  fi
  real_id "$@"
}
declare -a ALIVE_PIDS=()
kill() {
  local target="${2:-}" p
  [[ -n "$target" ]] || return 1
  for p in ${ALIVE_PIDS[@]+"${ALIVE_PIDS[@]}"}; do
    [[ "$p" == "$target" ]] && return 0
  done
  return 1
}
declare -A PID_OWNERS=()
stat() {
  local args=("$@") i path=""
  for i in "${!args[@]}"; do
    if [[ "${args[$i]}" == "-c" ]]; then
      path="${args[$((i + 2))]:-}"
      break
    fi
  done
  if [[ "$path" =~ ^/proc/([0-9]+)$ ]]; then
    printf '%s\n' "${PID_OWNERS[${BASH_REMATCH[1]}]:--1}"
    return 0
  fi
  real_stat "$@"
}

pause_dir="$fixture/run"
mkdir -p "$pause_dir/libpod/tmp"

# ── 1. Healthy tenant: no-op ─────────────────────────────────────────────────

_ctr_behave() { return 0; }  # podman ps succeeds
: >"$ORDER"
if repair_tenant_podman_userns acorn >/dev/null 2>&1; then
  pass "healthy tenant: repair succeeds"
else
  fail "healthy tenant: repair failed"
fi
if [[ ! -s "$ORDER" ]]; then
  pass "healthy tenant: no stop/migrate/start cycle"
else
  fail "healthy tenant: unexpected actions: $(tr '\n' ' ' <"$ORDER")"
fi

# ── 2. Join-signature failure: full repair sequence ──────────────────────────

_ctr_behave() {
  case "${1:-}" in
    ps)
      if pop_ps_fail; then
        printf 'Error: cannot re-exec process to join the existing user namespace: try podman system migrate\n' >&2
        return 1
      fi
      return 0
      ;;
    system)
      note_step MIGRATE
      return 0
      ;;
  esac
  return 0
}
set_ps_fails 1  # initial probe fails; the post-restart verification probe passes

if repair_tenant_podman_userns acorn >/dev/null 2>&1; then
  pass "join failure: repair succeeds"
else
  fail "join failure: repair failed"
fi
assert_eq "join failure: stop -> cleanup -> migrate -> start order" \
  "$(printf 'STOP\nCLEAN_PAUSE\nMIGRATE\nSTART')" "$(cat "$ORDER")"

# ── 3. Non-matching failure: never stop the tenant, fail loudly ──────────────

_ctr_behave() {
  if [[ "${1:-}" == "ps" ]]; then
    printf 'Error: uninitialized storage: db corruption\n' >&2
    return 1
  fi
  return 0
}
: >"$ORDER"
# repair_tenant calls die() (exit) on this path, so run it in a subshell.
if ( repair_tenant_podman_userns acorn ) >/dev/null 2>&1; then
  fail "non-join failure: repair unexpectedly succeeded"
else
  pass "non-join failure: repair fails loudly"
fi
# The cheap pause.pid cleanup is expected; the stop/migrate/start cycle is not.
assert_eq "non-join failure: only pause.pid cleanup ran" "CLEAN_PAUSE" "$(cat "$ORDER")"

# ── 4-7: real clean_stale_pause_pid branches against a fixture dir ───────────

_ctr_behave() { return 0; }

# 4. Live pause process owned by the tenant is kept.
printf '31337\n' >"$pause_dir/libpod/tmp/pause.pid"
ALIVE_PIDS=(31337)
PID_OWNERS[31337]=$TEST_UID
_clean_stale_pause_pid_real acorn "$TEST_UID" "$pause_dir"
if [[ -f "$pause_dir/libpod/tmp/pause.pid" ]]; then
  pass "pause.pid hygiene: live tenant-owned pause process kept"
else
  fail "pause.pid hygiene: live tenant-owned pause process removed"
fi

# 5. Dead-pid pause file is removed.
printf '4190002\n' >"$pause_dir/libpod/tmp/pause.pid"
ALIVE_PIDS=()
_clean_stale_pause_pid_real acorn "$TEST_UID" "$pause_dir"
if [[ ! -f "$pause_dir/libpod/tmp/pause.pid" ]]; then
  pass "pause.pid hygiene: dead pid removed"
else
  fail "pause.pid hygiene: dead pid kept"
fi

# 6. Foreign-owned (reused) pid is removed.
printf '31338\n' >"$pause_dir/libpod/tmp/pause.pid"
ALIVE_PIDS=(31338)
PID_OWNERS[31338]=4242  # another user's process recycled the pid
_clean_stale_pause_pid_real acorn "$TEST_UID" "$pause_dir"
if [[ ! -f "$pause_dir/libpod/tmp/pause.pid" ]]; then
  pass "pause.pid hygiene: foreign-owned pid removed"
else
  fail "pause.pid hygiene: foreign-owned pid kept"
fi

# 7. Missing pause.pid is a no-op.
ALIVE_PIDS=()
if _clean_stale_pause_pid_real acorn "$TEST_UID" "$pause_dir" >/dev/null 2>&1; then
  pass "pause.pid hygiene: absent file is a no-op"
else
  fail "pause.pid hygiene: absent file errored"
fi

# ── 8. Failing system migrate warns but repair completes ─────────────────────

clean_stale_pause_pid() { note_step CLEAN_PAUSE; }  # re-stub for orchestration
_ctr_behave() {
  case "${1:-}" in
    ps)
      if pop_ps_fail; then
        printf 'Error: cannot re-exec process to join the existing user namespace\n' >&2
        return 1
      fi
      return 0
      ;;
    system)
      return 1  # migrate blows up
      ;;
  esac
  return 0
}
set_ps_fails 1
: >"$ORDER"
if repair_tenant_podman_userns acorn >/dev/null 2>&1; then
  pass "failing migrate: repair completes when final probe recovers"
else
  fail "failing migrate: repair aborted"
fi
if grep -q '^START$' "$ORDER"; then
  pass "failing migrate: tenant still started"
else
  fail "failing migrate: tenant not started"
fi

# ── 9. The classifier matches all known signatures ───────────────────────────

if _podman_userns_join_failure "Error: cannot re-exec process to join the existing user namespace"; then
  pass "classifier: bare join error matches"
else
  fail "classifier: bare join error not matched"
fi
if _podman_userns_join_failure "failed to reexec: Permission denied"; then
  pass "classifier: reexec permission denied matches"
else
  fail "classifier: reexec permission denied not matched"
fi
if _podman_userns_join_failure 'fatal error, invalid internal status ... Try running "podman system migrate" ...'; then
  pass "classifier: system-migrate wrapper matches"
else
  fail "classifier: system-migrate wrapper not matched"
fi
if ! _podman_userns_join_failure "Error: no route to registry"; then
  pass "classifier: unrelated errors rejected"
else
  fail "classifier: unrelated error matched"
fi

echo
if [[ "$failures" -eq 0 ]]; then
  echo "ALL TESTS PASSED"
else
  echo "$failures TEST(S) FAILED"
  exit 1
fi
