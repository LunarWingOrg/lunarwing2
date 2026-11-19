#!/usr/bin/env bash
# WI#769 harness: a tenant whose systemd user manager is dead must stop with
# one deliberate line about the worker egress namespace keeper and no Podman
# bus errors. Rootless Podman cannot remove the pod without the user bus, so
# `stop-tenant` keeps it on purpose (exit 0) and the next start validates it
# before use; a teardown (remove-tenant) refuses with one line rather than
# spray Podman's errors and claim the tenant removed while its keeper lives.
# A live (or unknown) manager keeps today's calls; rootful runtimes and OpenRC
# hosts have no user manager on this path and keep today's calls too.
#
# Stubs the manager state and the container runtime, so no tenant account,
# user bus or Podman store is touched. The podman stand-in refuses `pod rm`
# with the bus errors measured on 2026-11-18 (WI#758 note 19233).
# shellcheck disable=SC2034  # runtime state is consumed by the sourced library
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
TENANT_UID=1009
KEEPER="lw-egress-keeper-$TENANT"
MANAGER_STATE=failed
POD_EXISTS_RC=0
POD_RM_RC=0
CTR_LOG_FILE="$(mktemp)"
STATE_LOG_FILE="$(mktemp)"
trap 'rm -f -- "$CTR_LOG_FILE" "$STATE_LOG_FILE"' EXIT
ctr_calls() { cat "$CTR_LOG_FILE"; }
ctr_call_count() { wc -l <"$CTR_LOG_FILE" | tr -d ' '; }

reset_runtime() {
  : >"$CTR_LOG_FILE"
  : >"$STATE_LOG_FILE"
  CONTAINER_RT=podman
  MT_ROOTLESS=true
  INIT_SYSTEM=systemd
  MANAGER_STATE=failed
  POD_EXISTS_RC=0
  POD_RM_RC=0
}

# The gate's answer, without a probe: each case sets the state it needs.
_user_manager_state() { printf '%s\n' "$MANAGER_STATE"; }

id() {
  if [[ "${1:-}" == -u && "${2:-}" == "$TENANT" ]]; then
    printf '%s\n' "$TENANT_UID"
    return 0
  fi
  command id "$@"
}

ensure_init_system() { :; }
ensure_container_runtime() { :; }
worker_egress_enabled() { return 0; }
tenant_exists_in_registry() { return 0; }
tenant_rootless_account_gone() { return 1; }
_worker_egress_remove_state() { printf 'state-removed\n' >>"$STATE_LOG_FILE"; }

# The stand-in for rootless Podman. `pod rm` refuses with the measured bus
# errors when POD_RM_RC is non-zero; every call is recorded so a case can
# assert that a dead manager never reaches it.
_ctr() {
  local name="$1"
  shift
  printf '%s\n' "$name $*" >>"$CTR_LOG_FILE"
  case "$1 $2" in
    'pod exists') return "${POD_EXISTS_RC:-0}" ;;
    'pod inspect') printf 'worker-egress-keeper\n' ;;
    'pod rm')
      if [[ "${POD_RM_RC:-0}" -ne 0 ]]; then
        printf 'ERRO[0000] failed to move the rootless netns pasta process to the systemd user.slice: dial unix /run/user/%s/bus: connect: connection refused\n' \
          "$TENANT_UID" >&2
        printf 'Error: removing pod %s: cgroup: dial unix /run/user/%s/bus: connect: connection refused\n' \
          "$KEEPER" "$TENANT_UID" >&2
        return 1
      fi
      ;;
    *) return 0 ;;
  esac
}

# --- 1. dead manager: a stop keeps the keeper, one line, exit 0 -------------

reset_runtime
MANAGER_STATE=failed
POD_RM_RC=1
capture worker_egress_stop_keeper "$TENANT" keep
out="$CAPTURED_OUT"
assert_eq "dead manager: stop keeper exits 0" "$CAPTURED_RC" "0"
assert_eq "dead manager: stop keeper prints exactly one line" "$(count_lines "$out")" "1"
assert_contains "dead manager: the line says the keeper was kept" "$out" \
  "kept namespace keeper $KEEPER on purpose"
assert_contains "dead manager: the line names the manager unit" "$out" "user@1009.service"
assert_contains "dead manager: the line names the recovery command" "$out" \
  "systemctl start user@1009.service"
assert_not_contains "dead manager: no raw Podman bus error" "$out" "connection refused"
assert_not_contains "dead manager: no Podman removal error" "$out" "Error: removing pod"
assert_not_contains "dead manager: no release failure line" "$out" "cannot release"
assert_eq "dead manager: podman is never called" "$(ctr_call_count)" "0"

# --- 2. dead manager: a teardown refuses in one line, touching nothing -----

reset_runtime
MANAGER_STATE=inactive
POD_RM_RC=1
capture worker_egress_stop_keeper "$TENANT"
out="$CAPTURED_OUT"
assert_eq "dead manager: release-mode keeper step fails" "$CAPTURED_RC" "1"
assert_eq "dead manager: release-mode prints exactly one line" "$(count_lines "$out")" "1"
assert_contains "dead manager: release-mode says it cannot release" "$out" \
  "cannot release namespace keeper $KEEPER"
assert_contains "dead manager: release-mode names the recovery command" "$out" \
  "recover with: systemctl start user@1009.service"
assert_not_contains "dead manager: release-mode has no raw Podman bus error" "$out" "connection refused"
assert_eq "dead manager: release-mode never calls podman" "$(ctr_call_count)" "0"

# --- 3. live manager: the same calls as today ------------------------------

reset_runtime
MANAGER_STATE=ok
capture worker_egress_stop_keeper "$TENANT" keep
out="$CAPTURED_OUT"
calls="$(ctr_calls)"
expected="$(printf '%s\n' \
  "$TENANT pod exists $KEEPER" \
  "$TENANT pod inspect --format {{index .Labels \"lunarwing.purpose\"}} $KEEPER" \
  "$TENANT pod rm -f $KEEPER")"
assert_eq "live manager: stop keeper exits 0" "$CAPTURED_RC" "0"
assert_eq "live manager: stop keeper prints nothing" "$out" ""
assert_eq "live manager: the keeper is probed, inspected and removed as today" "$calls" "$expected"

# An inconclusive probe must not invent a dead manager: same calls as today.
reset_runtime
MANAGER_STATE=unknown
capture worker_egress_stop_keeper "$TENANT" keep
out="$CAPTURED_OUT"
assert_eq "unknown manager: stop keeper exits 0" "$CAPTURED_RC" "0"
assert_eq "unknown manager: the keeper is still released" "$(ctr_call_count)" "3"
assert_contains "unknown manager: the keeper pod is removed" "$(ctr_calls)" \
  "$TENANT pod rm -f $KEEPER"

# A live manager with no keeper stops after the one existence probe.
reset_runtime
MANAGER_STATE=ok
POD_EXISTS_RC=1
capture worker_egress_stop_keeper "$TENANT" keep
assert_eq "live manager: an absent keeper exits 0" "$CAPTURED_RC" "0"
assert_eq "live manager: an absent keeper is probed once" "$(ctr_call_count)" "1"

# --- 4. rootful runtimes and OpenRC keep today's calls ---------------------

reset_runtime
MANAGER_STATE=failed
MT_ROOTLESS=false
capture worker_egress_stop_keeper "$TENANT" keep
assert_eq "rootful runtime: stop keeper exits 0" "$CAPTURED_RC" "0"
assert_eq "rootful runtime: no output" "$CAPTURED_OUT" ""
assert_eq "rootful runtime: no podman calls" "$(ctr_call_count)" "0"

reset_runtime
MANAGER_STATE=failed
INIT_SYSTEM=openrc
capture worker_egress_stop_keeper "$TENANT" keep
assert_eq "OpenRC: stop keeper exits 0" "$CAPTURED_RC" "0"
assert_eq "OpenRC: the keeper is released as today" "$(ctr_call_count)" "3"

# --- 5. stop-tenant end to end with a dead manager -------------------------

reset_runtime
MANAGER_STATE=failed
POD_RM_RC=1
stop_tenant_init_services() { :; }
stop_tenant_vision() { :; }
stop_tenant_voice() { :; }
stop_tenant_pebble() { :; }
stop_tenant_opencode() { :; }
stop_tenant_kimi() { :; }
stop_tenant_opencode2() { :; }
stop_tenant_lunardepth() { :; }
stop_tenant_postgres() { :; }
await_writer_quiescence() { :; }
retire_tenant_nanocode() { :; }
capture stop_tenant "$TENANT"
out="$CAPTURED_OUT"
assert_eq "stop-tenant: exits 0 with a dead manager" "$CAPTURED_RC" "0"
assert_contains "stop-tenant: says the keeper was kept" "$out" \
  "kept namespace keeper $KEEPER on purpose"
assert_not_contains "stop-tenant: no raw Podman bus error" "$out" "connection refused"
assert_not_contains "stop-tenant: no Podman removal error" "$out" "Error: removing pod"
assert_not_contains "stop-tenant: no keeper warning" "$out" "WARNING: could not release"
assert_eq "stop-tenant: podman is never called" "$(ctr_call_count)" "0"

# --- 6. remove-tenant's teardown refuses; keeper and state untouched --------

reset_runtime
MANAGER_STATE=dead
POD_RM_RC=1
capture worker_egress_teardown "$TENANT"
out="$CAPTURED_OUT"
assert_eq "teardown: refuses with a dead manager" "$CAPTURED_RC" "1"
assert_eq "teardown: prints exactly one line" "$(count_lines "$out")" "1"
assert_contains "teardown: names the tenant" "$out" "cannot tear down the egress state of $TENANT"
assert_contains "teardown: states the keeper and network are unchanged" "$out" \
  "keeper, network and rendered state are unchanged"
assert_contains "teardown: names the recovery command" "$out" \
  "recover with: systemctl start user@1009.service"
assert_not_contains "teardown: no raw Podman bus error" "$out" "connection refused"
assert_eq "teardown: never calls podman" "$(ctr_call_count)" "0"
assert_not_contains "teardown: the rendered state is untouched" "$(cat "$STATE_LOG_FILE")" "state-removed"

(( failures == 0 )) || exit 1
printf 'ALL TESTS PASSED\n'
