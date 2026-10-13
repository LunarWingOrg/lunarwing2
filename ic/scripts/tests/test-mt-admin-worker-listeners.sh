#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
set -euo pipefail

# Harness for the WI#357 control-9 `worker-listeners` exposure audit
# (worker_listeners in mt-admin-lib/09-worker-config.sh), covering the
# run-79 EI-6 hardening:
#  - a Docker-shaped all-interfaces publish ("HostIp":"") is a FINDING,
#    never "nothing to check"; "0.0.0.0" (podman shape) likewise
#  - an inspect FAILURE is a finding, never a silent "not running — skipped";
#    only a genuinely absent container is skipped
#  - listeners are read host-side via _worker_listeners_read (no in-container
#    `cat` a root-in-container agent could forge)
#  - UDP listeners (state 07) are audited like TCP LISTEN (0A)
#
# _ctr/_worker_listeners_read are stubbed from fixture tables; the REAL
# _worker_listeners_read is additionally exercised against a live process.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d)"
FIXTURE_PID=""
cleanup() {
  if [[ -n "$FIXTURE_PID" ]]; then kill "$FIXTURE_PID" 2>/dev/null || true; wait "$FIXTURE_PID" 2>/dev/null || true; fi
  rm -rf "$fixture"
}
trap cleanup EXIT

failures=0
pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }

# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"

# Stub AFTER sourcing: no real registry, runtime, or container is touched.
sanitize_name() { printf '%s' "$1"; }
tenant_exists_in_registry() { return 0; }
ensure_container_runtime() { return 0; }
ports_get() {  # <tenant> <slot>
  case "$2" in
    pebble_wss) printf '20011' ;;
    opencode_wss) printf '20012' ;;
    opencode2_wss) printf '20013' ;;
    kimi_wss) printf '20014' ;;
    *) return 1 ;;
  esac
}

# Fixture tables: lane -> state / port bindings / netns dump.
declare -A FIXTURE_STATE FIXTURE_BINDINGS FIXTURE_LISTENERS
declare -A PID_LANE
NEXT_PID=41000

_ctr() {  # <tenant> inspect -f <format> <container>  →  $1 $2 $3 $4 $5
  local fmt="$4" container="$5" lane
  lane="${container#lunarwing-}"
  lane="${lane%%-*}"
  if [[ "$fmt" == *State.Running* ]]; then
    case "${FIXTURE_STATE[$lane]:-absent}" in
      absent) printf 'Error: no such container %s\n' "$container" >&2; return 1 ;;
      stopped) printf 'false %d\n' "$NEXT_PID" ;;
      inspect-error) printf 'sudo: connection refused\n' >&2; return 1 ;;
      running:*) printf 'true %s\n' "${FIXTURE_STATE[$lane]#running:}" ;;
    esac
    return 0
  fi
  if [[ "$fmt" == *PortBindings* ]]; then
    printf '%s' "${FIXTURE_BINDINGS[$lane]:-null}"
    return 0
  fi
  return 1
}

# Keep the real host-side reader available under a new name before stubbing.
eval "$(declare -f _worker_listeners_read | sed 's/_worker_listeners_read/_real_worker_listeners_read/')"

_worker_listeners_read() {  # <tenant> <pid> — fixture netns dump by lane
  local pid="$2"
  printf '%s' "${FIXTURE_LISTENERS[${PID_LANE[$pid]:-unknown}]:-}"
}

# /proc/net/tcp-shaped rows. Columns: sl local_address rem_address st ...
# 0A = TCP LISTEN, 07 = UDP UNCONN. 8443 = 0x20FB; wss ports 20011..20014
# = 0x4E2B..0x4E2E; 4711 = 0x1267; 5353 = 0x14E9.
CLEAN_BINDINGS='{"8443/tcp":[{"HostIp":"127.0.0.1","HostPort":"8443"}]}'
HDR="  sl  local_address rem_address   st tx_queue"
lane_wss_hex() {
  case "$1" in
    pebble) printf '4E2B' ;;
    opencode) printf '4E2C' ;;
    opencode2) printf '4E2D' ;;
    kimi) printf '4E2E' ;;
  esac
}
listeners_with_wss() {  # <lane> — loopback bind + expected health/wss binds
  printf '%s\n 0: 0100007F:0BCD 00000000:0000 0A 00\n 1: 00000000:20FB 00000000:0000 0A 00\n 2: 00000000:%s 00000000:0000 0A 00\n' \
    "$HDR" "$(lane_wss_hex "$1")"
}
rogue_tcp_listeners="$(listeners_with_wss opencode2)
 3: 00000000:1267 00000000:0000 0A 00"
rogue_udp_listeners="$(listeners_with_wss opencode2)
 4: 00000000:14E9 00000000:0000 07 00"

reset_fixtures() {
  FIXTURE_STATE=()
  FIXTURE_BINDINGS=()
  FIXTURE_LISTENERS=()
  PID_LANE=()
}

assign_running() {  # <lane> — running with clean listeners/bindings
  local lane="$1"
  NEXT_PID=$((NEXT_PID + 1))
  FIXTURE_STATE[$lane]="running:$NEXT_PID"
  PID_LANE[$NEXT_PID]="$lane"
  FIXTURE_BINDINGS[$lane]="$CLEAN_BINDINGS"
  FIXTURE_LISTENERS[$lane]="$(listeners_with_wss "$lane")"
}

mark_absent() {
  FIXTURE_STATE[$1]=absent
}

run_audit() {  # sets AUDIT_OUT / AUDIT_RC
  AUDIT_OUT="$(worker_listeners acorn 2>&1)" && AUDIT_RC=0 || AUDIT_RC=$?
}

expect_finding() {  # <label> <needle>
  if [[ "$AUDIT_RC" -ne 0 && "$AUDIT_OUT" == *"$2"* ]]; then
    pass "$1"
  else
    fail "$1 (rc=$AUDIT_RC, output: $AUDIT_OUT)"
  fi
}

# 1. Clean tenant: every lane healthy → rc 0, clean verdict.
reset_fixtures
assign_running pebble
assign_running opencode
assign_running opencode2
assign_running kimi
run_audit
if [[ "$AUDIT_RC" -eq 0 && "$AUDIT_OUT" == *"worker listener audit: clean"* ]]; then
  pass "clean tenant audits clean"
else
  fail "clean tenant audits clean (rc=$AUDIT_RC, output: $AUDIT_OUT)"
fi

# 2. Docker all-interfaces publish is a finding (run-79 EI-6 core case):
#    the same mapping carries an empty HostIp for 9090 and loopback 8443.
reset_fixtures
assign_running pebble
FIXTURE_BINDINGS[pebble]='{"9090/tcp":[{"HostIp":"","HostPort":"9090"}],"8443/tcp":[{"HostIp":"127.0.0.1","HostPort":"8443"}]}'
mark_absent opencode
mark_absent opencode2
mark_absent kimi
run_audit
expect_finding "docker empty HostIp publish is a finding" "empty HostIp — Docker all-interfaces publish"

# 3. Missing HostIp key (null) is likewise all-interfaces.
reset_fixtures
assign_running pebble
FIXTURE_BINDINGS[pebble]='{"9090/tcp":[{"HostPort":"9090"}]}'
mark_absent opencode
mark_absent opencode2
mark_absent kimi
run_audit
expect_finding "missing HostIp publish is a finding" "0.0.0.0 (empty HostIp"

# 4. Podman-shape 0.0.0.0 HostIp is a finding.
reset_fixtures
assign_running kimi
FIXTURE_BINDINGS[kimi]='{"8443/tcp":[{"HostIp":"0.0.0.0","HostPort":"8443"}]}'
mark_absent pebble
mark_absent opencode
mark_absent opencode2
run_audit
expect_finding "podman 0.0.0.0 HostIp publish is a finding" "HostIp 0.0.0.0"

# 5. Inspect failure is a finding, never a silent skip.
reset_fixtures
FIXTURE_STATE[pebble]=inspect-error
mark_absent opencode
mark_absent opencode2
mark_absent kimi
run_audit
expect_finding "inspect failure is a finding" "INSPECT FAILED"

# 6. Absent container is skipped (not a finding) — alone, verdict is clean.
reset_fixtures
mark_absent pebble
mark_absent opencode
mark_absent opencode2
mark_absent kimi
run_audit
if [[ "$AUDIT_RC" -eq 0 && "$AUDIT_OUT" == *"not present"* && "$AUDIT_OUT" == *"worker listener audit: clean"* ]]; then
  pass "absent containers are skipped, verdict clean"
else
  fail "absent containers are skipped, verdict clean (rc=$AUDIT_RC, output: $AUDIT_OUT)"
fi

# 7. Stopped container is skipped (present, not running).
reset_fixtures
FIXTURE_STATE[pebble]=stopped
mark_absent opencode
mark_absent opencode2
mark_absent kimi
run_audit
if [[ "$AUDIT_RC" -eq 0 && "$AUDIT_OUT" == *"not running"* && "$AUDIT_OUT" == *"worker listener audit: clean"* ]]; then
  pass "stopped container is skipped"
else
  fail "stopped container is skipped (rc=$AUDIT_RC, output: $AUDIT_OUT)"
fi

# 8. Unexpected non-loopback TCP listener is an exposure finding.
reset_fixtures
assign_running opencode2
FIXTURE_LISTENERS[opencode2]="$rogue_tcp_listeners"
mark_absent pebble
mark_absent opencode
mark_absent kimi
run_audit
expect_finding "rogue non-loopback TCP listener flagged" "unexpected non-loopback listener: 0.0.0.0:4711"

# 9. UDP listener (state 07) is audited too.
reset_fixtures
assign_running opencode2
FIXTURE_LISTENERS[opencode2]="$rogue_udp_listeners"
mark_absent pebble
mark_absent opencode
mark_absent kimi
run_audit
expect_finding "rogue non-loopback UDP listener flagged" "unexpected non-loopback listener: 0.0.0.0:5353"

# 10. An unreadable netns is a finding, never a silent skip.
reset_fixtures
assign_running pebble
FIXTURE_LISTENERS[pebble]=""
mark_absent opencode
mark_absent opencode2
mark_absent kimi
run_audit
expect_finding "unreadable netns is a finding" "could not read /proc/"

# 11. The REAL _worker_listeners_read (rootful branch) reads a live
# process's /proc net tables host-side — the same read the audit performs
# on container pids. Covers tcp/tcp6/udp/udp6 concatenation.
# The aliased real reader branches on this global (rootful → direct cat).
# shellcheck disable=SC2034
MT_ROOTLESS=false
( sleep 60 ) >/dev/null 2>&1 & FIXTURE_PID=$!
real_read="$(_real_worker_listeners_read acorn "$FIXTURE_PID")"
if [[ "$real_read" == *"local_address"* && "$(printf '%s' "$real_read" | grep -c 'local_address')" -ge 2 ]]; then
  pass "real _worker_listeners_read reads tcp+udp tables host-side"
else
  fail "real _worker_listeners_read reads tcp+udp tables host-side (got: ${real_read:0:120})"
fi

(( failures == 0 )) || exit 1
printf 'ALL TESTS PASSED\n'
