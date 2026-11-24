#!/usr/bin/env bash
# WI#882: `mt-admin status` must report whether something answers on each
# worker lane's published WSS port, not just whether its container is
# `.State.Running` and not just whether the port accepts a connection. A
# rootless published port is held by a forwarder for as long as the container
# runs: with nothing listening inside, the connect succeeds and the forwarder
# then drops the connection at once.
#
# Pins:
#  - a running container whose port refuses -> "dead (no listener on 127.0.0.1)"
#  - a running container whose port accepts and drops at once (end of file or
#    a reset) -> "dead (accepted on 127.0.0.1 and dropped at once, ...)"
#  - a running container whose port accepts and keeps the connection open
#    -> "listening"
#  - a lane with no allocated WSS slot -> "unknown (no WSS port allocated)"
#  - a present image line carries its revision (effective worker version)
#  - the "running (...)" prefix is preserved: lunarwing_mt_onboard's verifier
#    reads the first word of the line
#  - no lane connection is attempted for a stopped/absent container
#  - the tenant's config.toml is read through the tenant account, never by the
#    caller on the tenant-writable path; a link, a named pipe or a missing file
#    there reads as unreadable
# shellcheck disable=SC2034  # fixture globals are consumed by sourced helpers.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"

failures=0
pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }

# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"

# Three real listeners on 127.0.0.1, each in its own thread, and a closed port:
#  - live:  accepts and keeps every connection open, as a bridge does while it
#           waits for the WebSocket request;
#  - drop:  accepts and closes at once (what `rootlessport` does with nothing
#           behind the published port);
#  - reset: accepts and resets at once (what `pasta` does in the same case).
# The port files live in a private temporary directory, never in the
# directory the harness is started from.
FIXTURE_DIR="$(mktemp -d)"
python3 - "$FIXTURE_DIR" <<'PY' &
import os, socket, struct, sys, threading, time

def serve(name, mode):
    s = socket.socket()
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("127.0.0.1", 0))
    s.listen(64)
    tmp = os.path.join(sys.argv[1], name + ".tmp")
    with open(tmp, "w") as f:
        f.write(str(s.getsockname()[1]))
    os.rename(tmp, os.path.join(sys.argv[1], name + ".port"))
    held = []
    while True:
        conn, _ = s.accept()
        if mode == "hold":
            held.append(conn)
        elif mode == "reset":
            conn.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
            conn.close()
        else:
            conn.close()

for name, mode in (("live", "hold"), ("drop", "close"), ("reset", "reset")):
    threading.Thread(target=serve, args=(name, mode), daemon=True).start()
time.sleep(180)
PY
LISTENER_PID=$!
CONFIG_DIR=""
cleanup() {
  kill "$LISTENER_PID" 2>/dev/null || true
  wait "$LISTENER_PID" 2>/dev/null || true
  rm -rf "$FIXTURE_DIR"
  [[ -z "$CONFIG_DIR" ]] || rm -rf "$CONFIG_DIR"
}
trap cleanup EXIT

for _ in $(seq 1 100); do
  [[ -s "$FIXTURE_DIR/live.port" && -s "$FIXTURE_DIR/drop.port" && -s "$FIXTURE_DIR/reset.port" ]] && break
  sleep 0.1
done
LIVE_PORT="$(cat "$FIXTURE_DIR/live.port" 2>/dev/null || true)"
DROP_PORT="$(cat "$FIXTURE_DIR/drop.port" 2>/dev/null || true)"
RESET_PORT="$(cat "$FIXTURE_DIR/reset.port" 2>/dev/null || true)"
[[ -n "$LIVE_PORT" && -n "$DROP_PORT" && -n "$RESET_PORT" ]] \
  || { printf 'FAIL: could not start the fixture listeners\n'; exit 1; }

# A stand-in for `sudo`, first in PATH. The harness tenant `alice` is not a
# real account, so the real sudo cannot become it. The stand-in records every
# call and runs exactly one shape: the bounded tenant read
# (`sudo -n -u alice sh -c ...`). Every other call fails, as it did before.
mkdir -p "$FIXTURE_DIR/bin"
export LW_TEST_SUDO_LOG="$FIXTURE_DIR/sudo.log"
: >"$LW_TEST_SUDO_LOG"
cat >"$FIXTURE_DIR/bin/sudo" <<'FAKESUDO'
#!/bin/sh
printf '%s\n' "$*" >>"$LW_TEST_SUDO_LOG"
if [ "${1:-}" = "-n" ] && [ "${2:-}" = "-u" ] && [ "${3:-}" = "alice" ] && [ "${4:-}" = "sh" ] && [ "${5:-}" = "-c" ]; then
  shift 3
  exec "$@"
fi
exit 1
FAKESUDO
chmod 0755 "$FIXTURE_DIR/bin/sudo"
PATH="$FIXTURE_DIR/bin:$PATH"

# A port with nothing bound: bind ephemeral, read it, close it.
DEAD_PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')"

sanitize_name() { printf '%s' "$1"; }
tenant_exists_in_registry() { return 0; }
# A config with pebble+opencode configured (the two lanes kageho had), so the
# registry-vs-config block can be asserted in both directions.
CONFIG_DIR="$(mktemp -d)"
cat >"$CONFIG_DIR/config.toml" <<'TOML'
[[sandbox.external_workers]]
name = "pebble"
url = "ws://127.0.0.1:20011/ws/agent"

[[sandbox.external_workers]]
name = "opencode"
url = "ws://127.0.0.1:20012/ws/agent"

[[sandbox.external_workers]]
name = "lunardepth"
url = "ws://127.0.0.1:20015/ws/agent"
TOML
ensure_container_runtime() { CONTAINER_RT=podman; MT_ROOTLESS=false; }
ensure_init_system() { INIT_SYSTEM=systemd; }
tenant_darkirc_enabled() { return 1; }
tenant_voice_enabled() { return 1; }
tenant_vision_external() { return 1; }
tenant_state_dir() { printf '%s' "$CONFIG_DIR"; }
tenant_worker_enabled() { [[ " pebble opencode opencode2 kimi " == *" $2 "* ]]; }
_worker_image_refs() {
  printf '%s\n' \
    "pebble lunarwing-worker-pebble:latest" \
    "opencode lunarwing-worker-opencode:latest" \
    "opencode2 lunarwing-worker-opencode2:latest" \
    "lunardepth lunarwing-worker-lunardepth:latest" \
    "kimi lunarwing-worker-kimi:latest"
}
say() { printf '%s\n' "$*"; }

# Per-lane fixture state: pebble answers, opencode's port refuses, kimi's port
# accepts and drops, opencode2 has no slot.
ports_get() {  # <tenant> <slot>
  case "$2" in
    pebble_wss) printf '%s\n' "$LIVE_PORT" ;;
    opencode_wss) printf '%s\n' "$DEAD_PORT" ;;
    kimi_wss) printf '%s\n' "$DROP_PORT" ;;
    opencode2_wss) return 1 ;;
    *) return 1 ;;
  esac
}
declare -A LANE_STATE=([pebble]=true [opencode]=true [opencode2]=true [lunardepth]="" [kimi]=true)
_ctr() {  # <tenant> [container] <verb> ... — mirrors the real typed wrapper
  local name="$1"; shift
  if [[ "${1:-}" == container ]]; then shift; fi
  case "${1:-}" in
    inspect)
      # _ctr_inspect <name> -f <fmt> <container>  |  _ctr_inspect <name> <container>
      local fmt="" container="" lane
      if [[ "${2:-}" == "-f" ]]; then fmt="${3:-}"; container="${4:-}"; else container="${2:-}"; fi
      [[ "$fmt" == *State.Running* || -z "$fmt" ]] || return 1
      lane="${container#lunarwing-}"; lane="${lane%%-*}"
      local state="${LANE_STATE[$lane]:-}"
      [[ -n "$state" ]] || return 1
      printf '%s\n' "$state"
      ;;
    image)
      # _ctr <name> image inspect [--format <fmt>] <image>
      local image="${*: -1}"
      [[ "$image" == "lunarwing-worker-pebble:latest" ]] || return 1
      printf '%s\n' "0123456789abcdef0123456789abcdef01234567"
      ;;
    *) return 1 ;;
  esac
}

# The probe itself, against each kind of port. The two dropped cases get a
# long hold: the probe returns as soon as the drop arrives, so the bound only
# matters on a stalled host, where it keeps the assertion from flaking.
probe_rc() { local rc=0; _worker_port_answers "$1" || rc=$?; printf '%s' "$rc"; }
[[ "$(WORKER_PORT_HOLD_SECS=0.3 probe_rc "$LIVE_PORT")" == 0 ]] \
  && pass "the probe answers 0 for a port that keeps the connection open" \
  || fail "the probe answers 0 for a port that keeps the connection open"
[[ "$(probe_rc "$DEAD_PORT")" == 1 ]] \
  && pass "the probe answers 1 for a refused connection" \
  || fail "the probe answers 1 for a refused connection"
[[ "$(WORKER_PORT_HOLD_SECS=5 probe_rc "$DROP_PORT")" == 2 ]] \
  && pass "the probe answers 2 for a port that accepts and closes at once" \
  || fail "the probe answers 2 for a port that accepts and closes at once"
# A reset can reach the client before its connect() returns; the kernel then
# fails the connect itself, so a resetting port reads as refused (1) or as
# dropped (2) depending on scheduling. Both are dead; only "answers" is wrong.
reset_rc="$(WORKER_PORT_HOLD_SECS=5 probe_rc "$RESET_PORT")"
if [[ "$reset_rc" == 1 || "$reset_rc" == 2 ]]; then
  pass "the probe never answers 0 for a port that accepts and resets at once"
else
  fail "the probe never answers 0 for a port that accepts and resets at once (got $reset_rc)"
fi
reset_verdict="$(WORKER_PORT_HOLD_SECS=5 worker_wss_readiness "$RESET_PORT")"
if [[ "$reset_verdict" == "dead ("* ]]; then
  pass "a reset port reads dead, not listening"
else
  fail "a reset port reads dead, not listening (got '$reset_verdict')"
fi

out="$(status_tenant alice)"

assert_contains() {
  local label="$1" expected="$2"
  if [[ "$out" == *"$expected"* ]]; then pass "$label"; else
    fail "$label (missing '$expected')"; printf '%s\n' "$out" >&2
  fi
}
assert_not_contains() {
  local label="$1" unexpected="$2"
  if [[ "$out" == *"$unexpected"* ]]; then fail "$label (found '$unexpected')"; else pass "$label"; fi
}

assert_contains "a port that keeps the connection open is reported listening" \
  "Pebble worker: running (lunarwing-pebble-alice, WSS port $LIVE_PORT: listening)"
assert_contains "a refused port is reported dead, naming the loopback address" \
  "OpenCode worker: running (lunarwing-opencode-alice, WSS port $DEAD_PORT: dead (no listener on 127.0.0.1))"
assert_contains "a port that accepts and drops at once is reported dead, not listening" \
  "Kimi worker: running (lunarwing-kimi-alice, WSS port $DROP_PORT: dead (accepted on 127.0.0.1 and dropped at once, nothing answers behind the published port))"
assert_not_contains "a port that accepts and drops is never called listening" \
  "WSS port $DROP_PORT: listening"
assert_contains "a lane without an allocated WSS slot says so" \
  "OpenCode 2 worker: running (lunarwing-opencode2-alice, WSS port none: unknown (no WSS port allocated))"
assert_contains "an absent container is not created" "LunarDepth worker: not created"

# The verifier in lunarwing_mt_onboard reads the first word after the label.
first_word="${out#*Pebble worker: }"
if [[ "${first_word%% *}" == "running" ]]; then
  pass "the running (...) prefix the onboarding verifier parses is preserved"
else
  fail "the running (...) prefix the onboarding verifier parses is preserved (got '${first_word%% *}')"
fi

assert_contains "a present worker image reports its revision" \
  "lunarwing-worker-pebble:latest: present (revision 0123456789abcdef0123456789abcdef01234567)"
assert_contains "an absent worker image still reports missing" \
  "lunarwing-worker-opencode:latest: missing"
assert_contains "an unselected lane is not a false missing" \
  "lunarwing-worker-lunardepth:latest: not selected for tenant"

# WI#882: registry selection vs the daemon's effective config.toml.
assert_contains "a lane in both registry and config is confirmed" \
  "Worker config: pebble enabled in registry and configured for the daemon"
assert_contains "a lane enabled but missing from config is a finding" \
  "Worker config: opencode2 enabled in registry but MISSING from $CONFIG_DIR/config.toml"
assert_contains "a lane disabled in registry but still configured is a finding" \
  "Worker config: lunardepth disabled in registry but still configured in $CONFIG_DIR/config.toml"

# WI#882 (review AMJ-2): the config sits in the tenant's own state directory,
# so the caller must not open that path itself. The read above went through
# the tenant account: the stand-in sudo saw the bounded read for this path.
if grep -qF -- "-n -u alice sh -c" "$LW_TEST_SUDO_LOG" \
  && grep -F -- "-n -u alice sh -c" "$LW_TEST_SUDO_LOG" | grep -qF -- "$CONFIG_DIR/config.toml"; then
  pass "the tenant config is read through the tenant account"
else
  fail "the tenant config is read through the tenant account (sudo log: $(tr '\n' '|' <"$LW_TEST_SUDO_LOG"))"
fi

# A link at the config path is refused, even when it points at a file with
# worker entries: the report says unreadable and calls no lane configured.
REAL_CONFIG_DIR="$CONFIG_DIR"
LINK_DIR="$FIXTURE_DIR/linked-state"
mkdir -p "$LINK_DIR"
ln -s "$REAL_CONFIG_DIR/config.toml" "$LINK_DIR/config.toml"
CONFIG_DIR="$LINK_DIR"
out="$(worker_config_consistency alice || true)"
assert_contains "a link at the config path reads as unreadable" \
  "Worker config: $LINK_DIR/config.toml is missing, is not a regular file, or is unreadable as tenant 'alice'"
assert_not_contains "no lane is called configured through a link" "configured for the daemon"
assert_not_contains "no lane is called missing through a link" "MISSING from"

# A named pipe there must neither block the report nor be read.
PIPE_DIR="$FIXTURE_DIR/pipe-state"
mkdir -p "$PIPE_DIR"
mkfifo "$PIPE_DIR/config.toml"
CONFIG_DIR="$PIPE_DIR"
# Run under a watchdog: a reader that opens the pipe would wait for ever, and
# the harness must then fail, not hang.
( worker_config_consistency alice >"$FIXTURE_DIR/pipe.out" 2>&1 || true ) &
pipe_pid=$!
pipe_done=false
for _ in $(seq 1 80); do
  if ! kill -0 "$pipe_pid" 2>/dev/null; then pipe_done=true; break; fi
  sleep 0.1
done
if [[ "$pipe_done" == true ]]; then
  pass "a named pipe at the config path does not block the report"
else
  fail "a named pipe at the config path does not block the report (still reading after 8s)"
  # Release the blocked reader: a writer that opens and closes ends its read.
  timeout 2 bash -c ': >"$1"' _ "$PIPE_DIR/config.toml" 2>/dev/null || true
  kill "$pipe_pid" 2>/dev/null || true
fi
wait "$pipe_pid" 2>/dev/null || true
out="$(cat "$FIXTURE_DIR/pipe.out" 2>/dev/null || true)"
assert_contains "a named pipe at the config path reads as unreadable" \
  "Worker config: $PIPE_DIR/config.toml is missing, is not a regular file, or is unreadable as tenant 'alice'"

# A missing file is unreadable too, not "consistent".
CONFIG_DIR="$FIXTURE_DIR/no-such-state"
out="$(worker_config_consistency alice || true)"
assert_contains "a missing config reads as unreadable" \
  "Worker config: $FIXTURE_DIR/no-such-state/config.toml is missing, is not a regular file, or is unreadable as tenant 'alice'"

# When the caller is the tenant, no privilege drop is needed or attempted.
CONFIG_DIR="$REAL_CONFIG_DIR"
: >"$LW_TEST_SUDO_LOG"
out="$(worker_config_consistency "$(id -un)" || true)"
assert_contains "the tenant itself reads its own config" \
  "Worker config: pebble enabled in registry and configured for the daemon"
if [[ ! -s "$LW_TEST_SUDO_LOG" ]]; then
  pass "no sudo call when the caller is the tenant"
else
  fail "no sudo call when the caller is the tenant ($(tr '\n' '|' <"$LW_TEST_SUDO_LOG"))"
fi

# A lane whose container is not running must not cost a loopback dial: the
# "stopped"/"not created" branches never call the probe. The status line calls
# the probe inside a command substitution, so the count is kept in a file: a
# shell variable would be incremented in the subshell and lost.
_real_probe="$(declare -f _worker_port_answers)"
eval "_counting_probe${_real_probe#_worker_port_answers}"
_worker_port_answers() { printf 'dial\n' >>"$FIXTURE_DIR/dials"; _counting_probe "$@"; }
dial_count() { if [[ -f "$FIXTURE_DIR/dials" ]]; then wc -l <"$FIXTURE_DIR/dials"; else printf '0\n'; fi; }
LANE_STATE=([pebble]=true [opencode]="" [opencode2]="" [lunardepth]="" [kimi]="")
WORKER_PORT_HOLD_SECS=0.3 status_tenant alice >/dev/null
if [[ "$(dial_count)" -eq 1 ]]; then
  pass "one running lane with a port costs exactly one dial (the counter works)"
else
  fail "one running lane with a port costs exactly one dial (dialed $(dial_count))"
fi
rm -f "$FIXTURE_DIR/dials"
LANE_STATE=([pebble]=false [opencode]="" [opencode2]="" [lunardepth]="" [kimi]="")
status_tenant alice >/dev/null
if [[ "$(dial_count)" -eq 0 ]]; then
  pass "no loopback dial for a stopped or absent lane"
else
  fail "no loopback dial for a stopped or absent lane (dialed $(dial_count))"
fi
eval "$_real_probe"

# WI#882: the strict gate (`worker-readiness`) must fail on a dead enabled
# lane, pass when every enabled lane answers, and skip an intentionally
# stopped OpenRC lane rather than calling it dead.
LANE_STATE=([pebble]=true [opencode]=true [opencode2]=true [lunardepth]="" [kimi]=true)
tenant_worker_desired_running() { return 0; }
INIT_SYSTEM=systemd
if check_tenant_worker_readiness alice >/dev/null 2>&1; then
  fail "the strict gate fails while a selected lane's port is dead"
else
  pass "the strict gate fails while a selected lane's port is dead"
fi

# Only the accept-and-drop lane is unhealthy: the gate must still fail, and
# name that lane. This is the rootless case a plain connect cannot see.
ports_get() {  # <tenant> <slot>
  case "$2" in
    pebble_wss|opencode_wss) printf '%s\n' "$LIVE_PORT" ;;
    kimi_wss) printf '%s\n' "$DROP_PORT" ;;
    *) return 1 ;;
  esac
}
gate_rc=0
gate_err="$(check_tenant_worker_readiness alice 2>&1 >/dev/null)" || gate_rc=$?
if [[ "$gate_rc" -ne 0 && "$gate_err" == *"kimi worker for alice is not listening on 127.0.0.1:$DROP_PORT"* ]]; then
  pass "the strict gate fails for a lane whose port accepts and drops, and names it"
else
  fail "the strict gate fails for a lane whose port accepts and drops, and names it (rc=$gate_rc: $gate_err)"
fi
if [[ "$gate_err" != *"pebble worker"* && "$gate_err" != *"opencode worker"* ]]; then
  pass "the lanes that keep the connection open are not reported"
else
  fail "the lanes that keep the connection open are not reported ($gate_err)"
fi

# The remaining cases probe only ports that keep the connection open or have
# no slot, so a shorter hold keeps the harness quick without making it flaky.
WORKER_PORT_HOLD_SECS=0.3

# Every selected lane on the live port: the gate passes.
ports_get() {  # <tenant> <slot>
  case "$2" in
    pebble_wss|opencode_wss|kimi_wss) printf '%s\n' "$LIVE_PORT" ;;
    opencode2_wss) return 1 ;;
    *) return 1 ;;
  esac
}
if check_tenant_worker_readiness alice >/dev/null 2>&1; then
  pass "the strict gate passes when every selected lane listens"
else
  fail "the strict gate passes when every selected lane listens"
fi

# OpenRC: an intentionally stopped lane is not a dead lane.
INIT_SYSTEM=openrc
tenant_worker_desired_running() { [[ "$2" != kimi ]]; }
ports_get() {  # <tenant> <slot>
  case "$2" in
    pebble_wss|opencode_wss) printf '%s\n' "$LIVE_PORT" ;;
    *) return 1 ;;
  esac
}
if check_tenant_worker_readiness alice >/dev/null 2>&1; then
  pass "an intentionally stopped OpenRC lane is not reported dead"
else
  fail "an intentionally stopped OpenRC lane is not reported dead"
fi

# An invalid hold falls back to the default instead of failing the probe.
WORKER_PORT_HOLD_SECS=not-a-number
[[ "$(probe_rc "$DEAD_PORT")" == 1 ]] \
  && pass "an invalid hold does not break the probe" \
  || fail "an invalid hold does not break the probe"
WORKER_PORT_HOLD_SECS=1

# The wait is a bounded shared window, and a wait of 0 costs no sleep at all.
unset LUNARWING_MT_WORKER_READY_WAIT_SECS
[[ "$(_worker_ready_wait_secs)" == "0" ]] \
  && pass "the readiness wait defaults to no polling" \
  || fail "the readiness wait defaults to no polling"
LUNARWING_MT_WORKER_READY_WAIT_SECS=not-a-number
[[ "$(_worker_ready_wait_secs)" == "0" ]] \
  && pass "an invalid readiness wait falls back to no polling" \
  || fail "an invalid readiness wait falls back to no polling"
LUNARWING_MT_WORKER_READY_WAIT_SECS=3
[[ "$(_worker_ready_wait_secs)" == "3" ]] \
  && pass "an explicit readiness wait is honoured" \
  || fail "an explicit readiness wait is honoured"

(( failures == 0 )) || exit 1
printf 'ALL TESTS PASSED\n'
