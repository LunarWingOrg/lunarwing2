#!/usr/bin/env bash
# WI#731 harness: a dead tenant systemd user manager (`user@<uid>.service`
# failed) must be found once, before the first `systemctl --user` call, and
# reduced to one line that names the unit and the recovery command. start and
# restart recover it; stop, status and remove report it once instead of a
# "could not classify" line per unit.
#
# Stubs the system-bus `systemctl` and the user-bus `_systemctl_user_raw`, so
# no real manager, tenant account or user bus is touched.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"
# The composition root enables errexit; this harness inspects failures itself.
set +e

failures=0

pass() { printf 'PASS: %s\n' "$1"; }
fail() {
  printf 'FAIL: %s\n' "$1"
  failures=$((failures + 1))
}

assert_eq() {
  local label="$1" actual="$2" expected="$3"
  if [[ "$actual" == "$expected" ]]; then
    pass "$label"
  else
    fail "$label (expected '$expected', got '$actual')"
  fi
}

assert_contains() {
  local label="$1" output="$2" expected="$3"
  if [[ "$output" == *"$expected"* ]]; then
    pass "$label"
  else
    fail "$label (missing '$expected' in: $output)"
  fi
}

assert_not_contains() {
  local label="$1" output="$2" unexpected="$3"
  if [[ "$output" == *"$unexpected"* ]]; then
    fail "$label (found '$unexpected' in: $output)"
  else
    pass "$label"
  fi
}

count_lines() {
  if [[ -z "$1" ]]; then
    printf '0'
    return 0
  fi
  printf '%s\n' "$1" | wc -l
}

# Run a probe in this shell (not a command substitution) so the gate's
# per-process cache, notice flag and the stubbed manager state survive the
# call, exactly as they do in a real mt-admin invocation.
CAPTURED_OUT=""
CAPTURED_RC=0
capture() {
  local file
  file="$(mktemp)"
  "$@" >"$file" 2>&1
  CAPTURED_RC=$?
  CAPTURED_OUT="$(cat "$file")"
  rm -f "$file"
  return 0
}

# --- fixtures -------------------------------------------------------------

TENANT="spitfire"
UID_VALUE=1011
USER_MANAGER_STATE=failed   # `systemctl is-active user@<uid>.service`
BUS_UP=false                # whether the user bus answers
START_SUCCEEDS=true         # whether `systemctl start` brings the bus back
SYSTEMCTL_CALLS=()
RAW_CALLS=0
RAW_CALL_LOG=()

# The gate's bounded waits use `sleep`; their loop logic is what matters here.
sleep() { :; }

reset_gate() {
  _USER_MANAGER_STATE_CACHE=()
  _USER_MANAGER_NOTICE_SHOWN=()
  SYSTEMCTL_CALLS=()
  RAW_CALLS=0
  RAW_CALL_LOG=()
}

systemctl() {
  SYSTEMCTL_CALLS+=("$*")
  case "${1:-}" in
    is-active)
      case "$USER_MANAGER_STATE" in
        failed) printf 'failed\n'; return 3 ;;
        inactive) printf 'inactive\n'; return 3 ;;
        *) printf '%s\n' "$USER_MANAGER_STATE"; return 0 ;;
      esac
      ;;
    start)
      if [[ "$START_SUCCEEDS" == true ]]; then
        USER_MANAGER_STATE=active
        BUS_UP=true
        return 0
      fi
      return 1
      ;;
  esac
  return 0
}

id() {
  if [[ "${1:-}" == "-u" ]]; then
    printf '%s\n' "$UID_VALUE"
    return 0
  fi
  command id "$@"
}

loginctl() {
  # Every real mt-admin tenant is lingering; a non-lingering account is not
  # this failure mode and the probe must stay silent for it.
  if [[ "${1:-}" == show-user ]]; then
    printf '%s\n' "${LINGER_VALUE:-yes}"
    return 0
  fi
  return 1
}

_systemctl_user_raw() {
  local name="$1"
  shift
  RAW_CALLS=$((RAW_CALLS + 1))
  RAW_CALL_LOG+=("$*")
  if [[ "$BUS_UP" != true ]]; then
    printf 'Failed to connect to user scope bus via local transport: Connection refused\n' >&2
    return 1
  fi
  case "${1:-}" in
    is-system-running) printf 'running\n' ;;
    list-units) printf 'lunarwing-%s.service loaded active running test daemon\n' "$name" ;;
    *) printf 'active\n' ;;
  esac
  return 0
}

tenant_rootless_account_gone() { return 1; }
tenant_darkirc_enabled() { return 1; }
tenant_weechat_enabled() { return 1; }
tenant_proxy_enabled() { return 1; }

# --- 1. report gate: one line, names the unit and the command --------------

reset_gate
USER_MANAGER_STATE=failed
BUS_UP=false
capture _tenant_user_manager_gate "$TENANT" report
out="$CAPTURED_OUT"
assert_eq "report gate exits non-zero for a dead manager" "$CAPTURED_RC" "1"
assert_eq "report gate prints exactly one line" "$(count_lines "$out")" "1"
assert_contains "report line names the manager unit" "$out" "user@1011.service"
assert_contains "report line names the recovery command" "$out" "systemctl start user@1011.service"
assert_not_contains "report line hides the raw bus error" "$out" "Failed to connect"

capture _tenant_user_manager_gate "$TENANT" report
assert_eq "report gate says it once per process" "$(count_lines "$CAPTURED_OUT")" "0"

# --- 2. _systemctl_user refuses a dead bus without a connection flood ------

reset_gate
USER_MANAGER_STATE=failed
BUS_UP=false
capture _systemctl_user "$TENANT" is-active lunarwing-spitfire.service
out="$CAPTURED_OUT"
assert_eq "systemctl wrapper fails when the manager is dead" "$CAPTURED_RC" "1"
assert_eq "systemctl wrapper prints one line, not one per unit" "$(count_lines "$out")" "1"
assert_contains "systemctl wrapper line names the manager" "$out" "user@1011.service"
assert_not_contains "systemctl wrapper never leaks the bus error" "$out" "Failed to connect"
assert_eq "systemctl wrapper does not run the raw user-bus call" "$RAW_CALLS" "0"

# --- 3. stop-tenant path: one line, no per-unit classification -------------

reset_gate
USER_MANAGER_STATE=failed
BUS_UP=false
capture stop_tenant_systemd "$TENANT"
out="$CAPTURED_OUT"
assert_eq "stop skips unit classification with a dead manager" "$CAPTURED_RC" "0"
assert_eq "stop prints exactly one line" "$(count_lines "$out")" "1"
assert_contains "stop line names the manager" "$out" "user@1011.service"
assert_not_contains "stop prints no could-not-classify flood" "$out" "could not classify"
assert_not_contains "stop prints no raw bus error" "$out" "Failed to connect"

# --- 4. recover gate: starts the unit, waits for the bus, settles ----------

reset_gate
USER_MANAGER_STATE=failed
BUS_UP=false
START_SUCCEEDS=true
capture _tenant_user_manager_gate "$TENANT" recover
out="$CAPTURED_OUT"
assert_eq "recover gate succeeds when the manager comes back" "$CAPTURED_RC" "0"
assert_eq "recover gate prints exactly one line" "$(count_lines "$out")" "1"
assert_contains "recover line names the unit" "$out" "user@1011.service"
assert_contains "recover line names the command it ran" "$out" "systemctl start user@1011.service"
assert_contains "recover gate starts the tenant manager" "${SYSTEMCTL_CALLS[*]}" "start --no-block user@1011.service"

capture _systemctl_user "$TENANT" is-active lunarwing-spitfire.service
out="$CAPTURED_OUT"
assert_eq "a recovered manager is usable in the same process" "$CAPTURED_RC" "0"
assert_not_contains "a recovered manager prints nothing more" "$out" "user@1011.service"

# --- 5. recover gate: failure names the unit and the command ---------------

reset_gate
USER_MANAGER_STATE=failed
BUS_UP=false
START_SUCCEEDS=false
capture _tenant_user_manager_gate "$TENANT" recover
out="$CAPTURED_OUT"
assert_eq "recover gate fails when the manager stays down" "$CAPTURED_RC" "1"
assert_eq "failed recovery prints exactly one line" "$(count_lines "$out")" "1"
assert_contains "failed recovery names the unit" "$out" "user@1011.service"
assert_contains "failed recovery names the command" "$out" "systemctl start user@1011.service"
assert_not_contains "failed recovery hides the raw bus error" "$out" "Failed to connect"

# --- 6. start-tenant path: no daemon-reload against a dead bus -------------

reset_gate
USER_MANAGER_STATE=failed
BUS_UP=false
START_SUCCEEDS=false
capture start_tenant_systemd "$TENANT"
out="$CAPTURED_OUT"
assert_eq "start fails when the manager cannot be recovered" "$CAPTURED_RC" "1"
assert_eq "start prints exactly one line" "$(count_lines "$out")" "1"
assert_not_contains "start prints no raw bus error" "$out" "Failed to connect"
assert_not_contains "start never issues daemon-reload on a dead bus" "${RAW_CALL_LOG[*]}" "daemon-reload"

# --- 7. status: one line in place of a line per unit -----------------------

reset_gate
USER_MANAGER_STATE=failed
BUS_UP=false
tenant_exists_in_registry() { return 0; }
ports_get() { printf '10000\n'; }
ensure_container_runtime() {
  # shellcheck disable=SC2034  # Consumed by the sourced mt-admin functions.
  CONTAINER_RT=podman
  # shellcheck disable=SC2034  # Consumed by the sourced mt-admin functions.
  MT_ROOTLESS=false
}
ensure_init_system() {
  # shellcheck disable=SC2034  # Consumed by the sourced mt-admin functions.
  INIT_SYSTEM=systemd
}
podman_supports_quadlet() { return 1; }
_ctr_inspect() { return 1; }
_ctr() { return 1; }
_worker_image_refs() { :; }
tenant_worker_enabled() { return 1; }
tenant_vision_external() { return 1; }
_vision_container_healthy() { return 1; }
tenant_voice_enabled() { return 1; }
_voice_container_healthy() { return 1; }

status_out="$(mktemp)"
status_err="$(mktemp)"
status_tenant "$TENANT" >"$status_out" 2>"$status_err"
status_body="$(cat "$status_out")"
status_error="$(cat "$status_err")"
rm -f "$status_out" "$status_err"
assert_eq "status prints the one line on stderr" "$(count_lines "$status_error")" "1"
assert_contains "status line names the manager" "$status_error" "user@1011.service"
assert_contains "status still prints the tenant header" "$status_body" "=== Tenant: $TENANT ==="
assert_not_contains "status prints no per-unit state for a dead bus" "$status_body" "lunarwing-spitfire.service:"
assert_not_contains "status prints no raw bus error" "$status_body$status_error" "Failed to connect"

# --- 8. writer checks skip the unit half on a dead manager -----------------

tenant_writer_services() { printf 'lunarwing-spitfire.service\n'; }
detect_container_runtime() { return 1; }

reset_gate
USER_MANAGER_STATE=failed
BUS_UP=false
capture tenant_writers_active "$TENANT"
assert_eq "dead manager: writer check finds no unit writers" "$CAPTURED_RC" "1"
assert_eq "dead manager: writer check still says the one line once" "$(count_lines "$CAPTURED_OUT")" "1"

reset_gate
USER_MANAGER_STATE=active
BUS_UP=true
capture tenant_writers_active "$TENANT"
assert_eq "healthy manager: an active writer unit is still detected" "$CAPTURED_RC" "0"

# --- 9. a non-lingering account is never declared dead --------------------

reset_gate
USER_MANAGER_STATE=inactive
BUS_UP=false
LINGER_VALUE=no
capture _tenant_user_manager_gate "$TENANT" report
assert_eq "non-lingering tenant passes the gate" "$CAPTURED_RC" "0"
assert_eq "non-lingering tenant prints nothing" "$CAPTURED_OUT" ""
LINGER_VALUE=yes

# --- 10. healthy manager is unchanged -------------------------------------

reset_gate
USER_MANAGER_STATE=active
BUS_UP=true
capture _tenant_user_manager_gate "$TENANT" report
out="$CAPTURED_OUT"
assert_eq "healthy manager passes the gate" "$CAPTURED_RC" "0"
assert_eq "healthy manager prints nothing" "$(count_lines "$out")" "0"

(( failures == 0 )) || exit 1
printf 'ALL TESTS PASSED\n'
