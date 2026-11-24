#!/usr/bin/env bash
# WI#882: `mt-admin status` must report whether each worker lane's WSS port is
# actually accepting loopback connections, not just whether its container is
# `.State.Running`. Kageho's 2.2.1.0 upgrade finished with both configured
# workers refusing connections while `status` printed "running" for each, and
# the only listener audit on the host was the WI#357 exposure scan.
#
# Pins:
#  - a running container whose port refuses -> "dead (no listener on 127.0.0.1)"
#  - a running container whose port accepts -> "listening"
#  - a lane with no allocated WSS slot -> "unknown (no WSS port allocated)"
#  - a present image line carries its revision (effective worker version)
#  - the "running (...)" prefix is preserved: lunarwing_mt_onboard's verifier
#    reads the first word of the line
#  - no lane connection is attempted for a stopped/absent container
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"

failures=0
pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }

# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"

# A real listener so the "listening" branch is exercised against a live socket,
# and a genuinely closed one for the dead branch. 127.0.0.1 only.
python3 - "$PWD" <<'PY' &
import socket, sys, time
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", 0))
s.listen(8)
open(sys.argv[1] + "/.readiness-port", "w").write(str(s.getsockname()[1]))
time.sleep(60)
PY
LISTENER_PID=$!
cleanup() { kill "$LISTENER_PID" 2>/dev/null || true; wait "$LISTENER_PID" 2>/dev/null || true; rm -f "$PWD/.readiness-port"; }
trap cleanup EXIT

for _ in $(seq 1 50); do [[ -s "$PWD/.readiness-port" ]] && break; sleep 0.1; done
LIVE_PORT="$(cat "$PWD/.readiness-port" 2>/dev/null || true)"
[[ -n "$LIVE_PORT" ]] || { printf 'FAIL: could not start fixture listener\n'; exit 1; }

# A port with nothing bound: bind ephemeral, read it, close it.
DEAD_PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')"

sanitize_name() { printf '%s' "$1"; }
tenant_exists_in_registry() { return 0; }
ensure_container_runtime() { CONTAINER_RT=podman; MT_ROOTLESS=false; }
ensure_init_system() { INIT_SYSTEM=systemd; }
tenant_darkirc_enabled() { return 1; }
tenant_voice_enabled() { return 1; }
tenant_vision_external() { return 1; }
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

# Per-lane fixture state: running lanes get a real port; opencode2 has no slot.
ports_get() {  # <tenant> <slot>
  case "$2" in
    pebble_wss) printf '%s\n' "$LIVE_PORT" ;;
    opencode_wss) printf '%s\n' "$DEAD_PORT" ;;
    kimi_wss) printf '%s\n' "$DEAD_PORT" ;;
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

assert_contains "a live listener is reported listening" \
  "Pebble worker: running (lunarwing-pebble-alice, WSS port $LIVE_PORT: listening)"
assert_contains "a refused port is reported dead, naming the loopback address" \
  "OpenCode worker: running (lunarwing-opencode-alice, WSS port $DEAD_PORT: dead (no listener on 127.0.0.1))"
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

# A lane whose container is not running must not cost a loopback dial: the
# "stopped"/"not created" branches never call _port_in_use.
dial_count=0
_real_port_in_use="$(declare -f _port_in_use)"
eval "_counting_port_in_use${_real_port_in_use#_port_in_use}"
_port_in_use() { dial_count=$((dial_count + 1)); _counting_port_in_use "$@"; }
LANE_STATE=([pebble]=false [opencode]="" [opencode2]="" [lunardepth]="" [kimi]="")
status_tenant alice >/dev/null
if [[ "$dial_count" -eq 0 ]]; then
  pass "no loopback dial for a stopped or absent lane"
else
  fail "no loopback dial for a stopped or absent lane (dialed $dial_count)"
fi
eval "$_real_port_in_use"

(( failures == 0 )) || exit 1
printf 'ALL TESTS PASSED\n'
