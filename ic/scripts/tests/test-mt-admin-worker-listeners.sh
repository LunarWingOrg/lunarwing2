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
# Run-90 FC-2/FC-6 hardening:
#  - the expected-port exemption (health / bridge WS) is TCP LISTEN only — a
#    UDP bind on those ports is an exposure, and every line carries its
#    protocol label;
#  - IPv6 fixtures: [::] on an unexpected port is flagged, ::1 is skipped;
#  - the rootful reader must read tcp+tcp6+udp+udp6 (header-count parity
#    against the same four files read directly);
#  - the rootless reader is exercised through a PATH sudo shim, asserting the
#    delegated command still cats all four tables.
#
# _ctr/_worker_listeners_read are stubbed from fixture tables; the REAL
# _worker_listeners_read is additionally exercised against a live process.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d)"
FIXTURE_PID=""
cleanup() {
  if [[ -n "$FIXTURE_PID" ]]; then kill "$FIXTURE_PID" 2>/dev/null || true; wait "$FIXTURE_PID" 2>/dev/null || true; fi
  rm -rf "$fixture" "${ROOTLESS_BIN:-}"
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
      absent-podman) printf 'Error: no such object: "%s"\n' "$container" >&2; return 125 ;;
      absent-docker) printf 'Error: No such object: %s\n' "$container" >&2; return 1 ;;
      absent-daemon) printf 'Error response from daemon: No such container: %s\n' "$container" >&2; return 1 ;;
      wrong-container) printf 'Error: no such container %s-other\n' "$container" >&2; return 1 ;;
      absence-success) printf 'Error: no such container %s\n' "$container" >&2; return 0 ;;
      missing-user) printf "id: 'acorn': no such user\n" >&2; return 1 ;;
      missing-runtime) printf 'Error: stat /run/user/1234: no such file or directory\n' >&2; return 1 ;;
      stopped) printf 'false %d\n' "$NEXT_PID" ;;
      inspect-error) printf 'sudo: connection refused\n' >&2; return 1 ;;
      state-on-error) printf 'true %d\n' "$NEXT_PID"; return 1 ;;
      warn-running:*) printf 'WARN[0000] runtime warning\n' >&2; printf 'true %s\n' "${FIXTURE_STATE[$lane]#warn-running:}" ;;
      running:*) printf 'true %s\n' "${FIXTURE_STATE[$lane]#running:}" ;;
    esac
    return 0
  fi
  if [[ "$fmt" == *PortBindings* ]]; then
    if [[ "${FIXTURE_BINDINGS[$lane]:-}" == inspect-error ]]; then
      printf 'runtime: connection refused\n' >&2
      return 1
    fi
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
expect_finding "rogue non-loopback UDP listener flagged" "unexpected non-loopback listener: 0.0.0.0:5353/udp"

# FC-2: the expected-port exemption is TCP LISTEN only — a UDP bind on the
# health or bridge-WS port is not published anywhere and stays an exposure,
# with the protocol labelled on the line.
reset_fixtures
assign_running opencode2
FIXTURE_LISTENERS[opencode2]="$(listeners_with_wss opencode2)
 5: 00000000:20FB 00000000:0000 07 00
 6: 00000000:4E2D 00000000:0000 07 00"
mark_absent pebble
mark_absent opencode
mark_absent kimi
run_audit
expect_finding "UDP bind on the health port is an exposure" "unexpected non-loopback listener: 0.0.0.0:8443/udp"
expect_finding "UDP bind on the bridge-WS port is an exposure" "unexpected non-loopback listener: 0.0.0.0:20013/udp"

# FC-6: IPv6 rows — [::] on an unexpected port is flagged, ::1 is skipped.
reset_fixtures
assign_running kimi
FIXTURE_LISTENERS[kimi]="  sl  local_address rem_address   st tx_queue
 0: 00000000000000000000000000000000:1267 00000000000000000000000000000000:0000 0A 00
 1: 00000000000000000000000001000000:1267 00000000000000000000000000000000:0000 0A 00"
mark_absent pebble
mark_absent opencode
mark_absent opencode2
run_audit
expect_finding "non-loopback IPv6 [::] listener flagged" "unexpected non-loopback listener: :::4711/tcp"

reset_fixtures
assign_running kimi
FIXTURE_LISTENERS[kimi]="  sl  local_address rem_address   st tx_queue
 0: 00000000000000000000000001000000:1267 00000000000000000000000000000000:0000 0A 00"
mark_absent pebble
mark_absent opencode
mark_absent opencode2
run_audit
if [[ "$AUDIT_RC" -eq 0 && "$AUDIT_OUT" == *"worker listener audit: clean"* ]]; then
  pass "IPv6 ::1 listener is skipped"
else
  fail "IPv6 ::1 listener is skipped (rc=$AUDIT_RC, output: $AUDIT_OUT)"
fi

# 10. An unreadable netns is a finding, never a silent skip.
reset_fixtures
assign_running pebble
FIXTURE_LISTENERS[pebble]=""
mark_absent opencode
mark_absent opencode2
mark_absent kimi
run_audit
expect_finding "unreadable netns is a finding" "could not read /proc/"

# FC-1: absence is a non-zero inspect with a runtime diagnostic naming this
# exact container. Docker capitalisation and Podman quoting both occur live.
for shape in absent-podman absent-docker absent-daemon; do
  reset_fixtures
  FIXTURE_STATE[pebble]="$shape"
  run_audit
  if [[ "$AUDIT_RC" -eq 0 && "$AUDIT_OUT" == *"pebble: not present"* ]]; then
    pass "$shape is recognised as absent"
  else
    fail "$shape is recognised as absent (rc=$AUDIT_RC, output: $AUDIT_OUT)"
  fi
done

for shape in missing-user missing-runtime wrong-container absence-success state-on-error; do
  reset_fixtures
  FIXTURE_STATE[pebble]="$shape"
  run_audit
  expect_finding "$shape cannot turn an inspect failure into clean" "INSPECT FAILED"
done

reset_fixtures
assign_running pebble
FIXTURE_STATE[pebble]="warn-${FIXTURE_STATE[pebble]}"
run_audit
if [[ "$AUDIT_RC" -eq 0 && "$AUDIT_OUT" == *"pebble: lunarwing-pebble-acorn pid"* ]]; then
  pass "warning before valid state still audits running container"
else
  fail "warning before valid state (rc=$AUDIT_RC, output: $AUDIT_OUT)"
fi
# Prove the warning did not skip the running container's mapping audit.
FIXTURE_BINDINGS[pebble]='{"9090/tcp":[{"HostIp":"0.0.0.0","HostPort":"9090"}]}'
run_audit
expect_finding "warning-prefixed state still checks mappings" "HostIp 0.0.0.0"

reset_fixtures
assign_running pebble
FIXTURE_BINDINGS[pebble]=inspect-error
run_audit
expect_finding "PortBindings inspect failure is a finding" "INSPECT FAILED: could not read PortBindings"

FIXTURE_BINDINGS[pebble]='invalid-json'
run_audit
expect_finding "PortBindings parse failure is a finding" "could not parse PortBindings"

# 11. The REAL _worker_listeners_read (rootful branch) reads a live
# process's /proc net tables host-side — the same read the audit performs
# on container pids. FC-6: the read must cover every table the reader is
# supposed to cat — compare the header count against the same four files
# read directly, so a reader that drops udp/udp6 cannot pass.
# The aliased real reader branches on this global (rootful → direct cat).
# shellcheck disable=SC2034
MT_ROOTLESS=false
( sleep 60 ) >/dev/null 2>&1 & FIXTURE_PID=$!
real_read="$(_real_worker_listeners_read acorn "$FIXTURE_PID")"
expected_headers="$(cat "/proc/$FIXTURE_PID/net/tcp" "/proc/$FIXTURE_PID/net/tcp6" \
    "/proc/$FIXTURE_PID/net/udp" "/proc/$FIXTURE_PID/net/udp6" 2>/dev/null | grep -c 'local_address' || true)"
got_headers="$(printf '%s' "$real_read" | grep -c 'local_address' || true)"
if [[ "$got_headers" -eq "$expected_headers" && "$got_headers" -ge 2 ]]; then
  pass "real _worker_listeners_read reads tcp+tcp6+udp+udp6 host-side ($got_headers tables)"
else
  fail "real _worker_listeners_read reads tcp+tcp6+udp+udp6 host-side (got $got_headers, expected $expected_headers)"
fi

# 12. FC-6: the ROOTLESS branch (MT_ROOTLESS=true) must delegate through
# sudo to the tenant and still read all four tables. A sudo shim on PATH
# records the delegated command and runs it directly; dropping udp/udp6
# from the rootless cat list, or rewriting it to /proc/self/net/tcp, fails.
# The shims live under the repo tree: /tmp is noexec on some dev hosts.
rootless_bin="$(mktemp -d "$SCRIPT_DIR/.worker-listeners-rootless.XXXXXX")"
ROOTLESS_BIN="$rootless_bin"
SUDO_SHIM_LOG="$fixture/sudo-shim.log"
export SUDO_SHIM_LOG
: > "$SUDO_SHIM_LOG"
real_id="$(command -v id)"
real_getent="$(command -v getent)"
cat > "$rootless_bin/sudo" <<SH
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "\$SUDO_SHIM_LOG"
shift 2
exec "\$@"
SH
cat > "$rootless_bin/id" <<SH
#!/usr/bin/env bash
if [[ \$# -eq 2 && "\$1" == "-u" && "\$2" == "acorn" ]]; then printf '1042\n'; exit 0; fi
exec "$real_id" "\$@"
SH
cat > "$rootless_bin/getent" <<SH
#!/usr/bin/env bash
if [[ \$# -eq 2 && "\$1" == "passwd" && "\$2" == "acorn" ]]; then
  printf 'acorn:x:1042:1042::/home/acorn:/bin/bash\n'; exit 0
fi
exec "$real_getent" "\$@"
SH
chmod +x "$rootless_bin/sudo" "$rootless_bin/id" "$rootless_bin/getent"
# shellcheck disable=SC2034
MT_ROOTLESS=true
rootless_read="$(PATH="$rootless_bin:$PATH" _real_worker_listeners_read acorn "$FIXTURE_PID")"
# shellcheck disable=SC2034
MT_ROOTLESS=false
rootless_got="$(printf '%s' "$rootless_read" | grep -c 'local_address' || true)"
delegated="$(cat "$SUDO_SHIM_LOG")"
if [[ "$rootless_got" -eq "$expected_headers" && "$rootless_got" -ge 2
      && "$delegated" == *"/proc/\$1/net/tcp"* && "$delegated" == *"/proc/\$1/net/tcp6"*
      && "$delegated" == *"/proc/\$1/net/udp"* && "$delegated" == *"/proc/\$1/net/udp6"* ]]; then
  pass "rootless reader delegates via sudo and reads all four net tables"
else
  fail "rootless reader delegates via sudo and reads all four net tables (got $rootless_got headers; delegated: $delegated)"
fi
rm -rf "$rootless_bin"

(( failures == 0 )) || exit 1
printf 'ALL TESTS PASSED\n'
