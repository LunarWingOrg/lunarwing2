#!/usr/bin/env bash
# WI#402: single-node scripts must not bake developer-LAN LLM endpoint defaults.
# Pins the fail-loud behavior of:
#   - ic/scripts/setup-instance.sh         (--llm-base-url required)
#   - ic/scripts/lunarwing-xmpp-test-env.sh (init/bring-up requires
#     LUNARWING_TEST_TENSORZERO_URL or TENSORZERO_URL, or an existing
#     proxy.env value; render-systemd/render-launchd/start-proxy guard sites
#     pinned incl. the valueless-existing-proxy.env path; doctor reports
#     NOT CONFIGURED instead of dying)
#   - ic/scripts/lunarwing_toolcall_diag.py (--host required; the historical
#     dev-LAN gateway only via an explicit --default)
# No docker, network, or cargo needed — argument/write-path behavior only.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IC_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
SETUP_INSTANCE="$IC_DIR/scripts/setup-instance.sh"
TEST_ENV="$IC_DIR/scripts/lunarwing-xmpp-test-env.sh"
DIAG="$IC_DIR/scripts/lunarwing_toolcall_diag.py"

WORK="$(mktemp -d)"
trap 'if [[ -f "$WORK/env-g/run/proxy.pid" ]]; then kill "$(cat "$WORK/env-g/run/proxy.pid")" 2>/dev/null; fi; rm -rf "$WORK"' EXIT

failures=0

pass() {
  echo "  PASS: $1"
}

fail() {
  echo "  FAIL: $1"
  echo "        $2"
  failures=$((failures + 1))
}

assert_exit() { # <label> <expected-rc> <actual-rc>
  if [[ "$2" == "$3" ]]; then
    pass "$1"
  else
    fail "$1" "expected rc=$2, got rc=$3"
  fi
}

assert_contains() { # <label> <haystack> <needle>
  if [[ "$2" == *"$3"* ]]; then
    pass "$1"
  else
    fail "$1" "expected to contain [$3], got: ${2:0:200}"
  fi
}

assert_absent() { # <label> <path>
  if [[ ! -e "$2" ]]; then
    pass "$1"
  else
    fail "$1" "expected $2 to not exist"
  fi
}

assert_file_contains() { # <label> <path> <needle>
  if [[ -f "$2" ]] && grep -qF "$3" "$2"; then
    pass "$1"
  else
    fail "$1" "expected $2 to contain [$3]"
  fi
}

echo "=== setup-instance.sh: --llm-base-url is required (WI#402) ==="

out="$(LUNARWING_ONBOARD_LEGACY_OK=1 bash "$SETUP_INSTANCE" \
  --base-dir "$WORK/si-a" --database libsql 2>&1)"
rc=$?
assert_exit "setup-instance dies without --llm-base-url" "1" "$rc"
assert_contains "error names --llm-base-url" "$out" "--llm-base-url is required"
assert_absent "no config.toml written on refusal" "$WORK/si-a/config.toml"

out="$(LUNARWING_ONBOARD_LEGACY_OK=1 bash "$SETUP_INSTANCE" \
  --base-dir "$WORK/si-b" --database libsql \
  --llm-base-url 'http://127.0.0.1:3002/v1' 2>&1)"
rc=$?
assert_exit "setup-instance succeeds with --llm-base-url" "0" "$rc"
assert_file_contains "config.toml carries the given endpoint" \
  "$WORK/si-b/config.toml" 'openai_compatible_base_url = "http://127.0.0.1:3002/v1"'

if grep -qF '192.168.1.157' "$SETUP_INSTANCE"; then
  fail "setup-instance.sh source pin" "dev-LAN literal still present"
else
  pass "setup-instance.sh carries no dev-LAN literal"
fi

echo "=== lunarwing-xmpp-test-env.sh: explicit TensorZero URL at bring-up (WI#402) ==="

ROOT_A="$WORK/env-a"
out="$(env -u LUNARWING_TEST_TENSORZERO_URL -u TENSORZERO_URL \
  LUNARWING_TEST_ROOT="$ROOT_A" bash "$TEST_ENV" init 2>&1)"
rc=$?
assert_exit "init dies with no knob on a fresh root" "1" "$rc"
assert_contains "error names LUNARWING_TEST_TENSORZERO_URL" "$out" "LUNARWING_TEST_TENSORZERO_URL"
assert_absent "no proxy.env written on refusal" "$ROOT_A/env/proxy.env"
assert_absent "no lunarwing.env written on refusal" "$ROOT_A/env/lunarwing.env"

out="$(env -u TENSORZERO_URL \
  LUNARWING_TEST_ROOT="$ROOT_A" LUNARWING_TEST_TENSORZERO_URL="http://tz.test:3000" \
  bash "$TEST_ENV" init 2>&1)"
rc=$?
assert_exit "init succeeds with LUNARWING_TEST_TENSORZERO_URL" "0" "$rc"
assert_file_contains "proxy.env bakes the given URL" "$ROOT_A/env/proxy.env" \
  "TENSORZERO_URL=http://tz.test:3000"
assert_file_contains "lunarwing.env still points at the local proxy" \
  "$ROOT_A/env/lunarwing.env" "LLM_BASE_URL=http://127.0.0.1:3002/v1"

out="$(env -u LUNARWING_TEST_TENSORZERO_URL -u TENSORZERO_URL \
  LUNARWING_TEST_ROOT="$ROOT_A" bash "$TEST_ENV" init 2>&1)"
rc=$?
assert_exit "re-init without knob reuses the baked proxy.env value" "0" "$rc"

ROOT_B="$WORK/env-b"
out="$(env -u LUNARWING_TEST_TENSORZERO_URL \
  LUNARWING_TEST_ROOT="$ROOT_B" TENSORZERO_URL="http://fallback.test:3000" \
  bash "$TEST_ENV" init 2>&1)"
rc=$?
assert_exit "plain TENSORZERO_URL is honored as a fallback" "0" "$rc"
assert_file_contains "proxy.env bakes the fallback URL" "$ROOT_B/env/proxy.env" \
  "TENSORZERO_URL=http://fallback.test:3000"

ROOT_C="$WORK/env-c"
out="$(env -u LUNARWING_TEST_TENSORZERO_URL -u TENSORZERO_URL \
  LUNARWING_TEST_ROOT="$ROOT_C" bash "$TEST_ENV" doctor 2>&1)"
rc=$?
assert_exit "doctor does not die without the knob" "0" "$rc"
assert_contains "doctor reports the gateway as not configured" "$out" "NOT CONFIGURED"

echo "=== lunarwing-xmpp-test-env.sh: bring-up guard sites pinned (WI#402 IH-2) ==="

# render-systemd on a fresh root with no knob must die, and ensure_env must
# not leave a valueless proxy.env behind — the proxy.env-absence assertion
# kills a write_proxy_env_if_missing guard removal (the render's own require
# would still die, but only after the bad file exists).
ROOT_D="$WORK/env-d"
out="$(env -u LUNARWING_TEST_TENSORZERO_URL -u TENSORZERO_URL \
  LUNARWING_TEST_ROOT="$ROOT_D" bash "$TEST_ENV" render-systemd 2>&1)"
rc=$?
assert_exit "render-systemd dies with no knob on a fresh root" "1" "$rc"
assert_contains "render-systemd refusal names the knob" "$out" "LUNARWING_TEST_TENSORZERO_URL"
assert_absent "render-systemd leaves no valueless proxy.env" "$ROOT_D/env/proxy.env"
assert_absent "render-systemd writes no proxy unit on refusal" \
  "$ROOT_D/systemd/lunarwing-proxy-test.service"

# render-launchd: same bring-up guard shape on the macOS lane.
ROOT_E="$WORK/env-e"
out="$(env -u LUNARWING_TEST_TENSORZERO_URL -u TENSORZERO_URL \
  LUNARWING_TEST_ROOT="$ROOT_E" bash "$TEST_ENV" render-launchd 2>&1)"
rc=$?
assert_exit "render-launchd dies with no knob on a fresh root" "1" "$rc"
assert_absent "render-launchd leaves no valueless proxy.env" "$ROOT_E/env/proxy.env"

# render-systemd with an existing but valueless proxy.env (legacy/hand-edited
# root): ensure_env skips the write, so only the render's own require refuses.
ROOT_F="$WORK/env-f"
mkdir -p "$ROOT_F/env"
printf 'PROXY_PORT=3002\nPROXY_BIND=127.0.0.1\n' >"$ROOT_F/env/proxy.env"
out="$(env -u LUNARWING_TEST_TENSORZERO_URL -u TENSORZERO_URL \
  LUNARWING_TEST_ROOT="$ROOT_F" bash "$TEST_ENV" render-systemd 2>&1)"
rc=$?
assert_exit "render-systemd dies on a valueless existing proxy.env" "1" "$rc"
assert_contains "valueless-proxy.env render refusal names the knob" "$out" "LUNARWING_TEST_TENSORZERO_URL"
assert_absent "no proxy unit on the valueless-proxy.env refusal" \
  "$ROOT_F/systemd/lunarwing-proxy-test.service"

# start-proxy with an existing but valueless proxy.env: ensure_env skips the
# write, so only start_proxy's own require refuses (pins the M5 guard site).
ROOT_G="$WORK/env-g"
mkdir -p "$ROOT_G/env"
printf 'PROXY_PORT=13992\nPROXY_BIND=127.0.0.1\n' >"$ROOT_G/env/proxy.env"
out="$(env -u LUNARWING_TEST_TENSORZERO_URL -u TENSORZERO_URL \
  LUNARWING_TEST_ROOT="$ROOT_G" LUNARWING_TEST_PROXY_PORT=13992 \
  bash "$TEST_ENV" start-proxy 2>&1)"
rc=$?
assert_exit "start-proxy dies on a valueless existing proxy.env" "1" "$rc"
assert_contains "start-proxy refusal names the knob" "$out" "LUNARWING_TEST_TENSORZERO_URL"
assert_absent "start-proxy leaves no pid file on refusal" "$ROOT_G/run/proxy.pid"

# render-launchd over a valueless existing proxy.env: only render_launchd's
# own require can refuse (ensure_env skips the proxy.env write).
ROOT_H="$WORK/env-h"
mkdir -p "$ROOT_H/env"
{ echo "PROXY_PORT=3002"; echo "PROXY_BIND=127.0.0.1"; } >"$ROOT_H/env/proxy.env"
out="$(env -u LUNARWING_TEST_TENSORZERO_URL -u TENSORZERO_URL LUNARWING_TEST_ROOT="$ROOT_H" bash "$TEST_ENV" render-launchd 2>&1)"
rc=$?
assert_exit "render-launchd dies on a valueless existing proxy.env" "1" "$rc"
if compgen -G "$ROOT_H/launchd/"*.plist >/dev/null; then
  fail "no launchd plists on the valueless-proxy.env refusal" "plist written under $ROOT_H/launchd"
else
  pass "no launchd plists on the valueless-proxy.env refusal"
fi

# Direct-gateway mode (ENABLE_PROXY=false) bakes the URL into the daemon's
# LLM_BASE_URL, and lunarwing.env is never rewritten once it exists. With a
# valueless existing proxy.env only that writer's own require can refuse.
ROOT_I="$WORK/env-i"
mkdir -p "$ROOT_I/env"
{ echo "PROXY_PORT=3002"; echo "PROXY_BIND=127.0.0.1"; } >"$ROOT_I/env/proxy.env"
out="$(env -u LUNARWING_TEST_TENSORZERO_URL -u TENSORZERO_URL LUNARWING_TEST_ROOT="$ROOT_I" LUNARWING_TEST_ENABLE_PROXY=false bash "$TEST_ENV" render-systemd 2>&1)"
rc=$?
assert_exit "direct-gateway render-systemd dies with no knob" "1" "$rc"
assert_absent "direct-gateway refusal bakes no lunarwing.env" "$ROOT_I/env/lunarwing.env"
ROOT_J="$WORK/env-j"
out="$(env -u TENSORZERO_URL LUNARWING_TEST_ROOT="$ROOT_J" LUNARWING_TEST_ENABLE_PROXY=false LUNARWING_TEST_TENSORZERO_URL="http://direct.test:3000" bash "$TEST_ENV" init 2>&1)"
rc=$?
assert_exit "direct-gateway init succeeds with the knob" "0" "$rc"
assert_file_contains "direct-gateway lunarwing.env bakes the given URL" "$ROOT_J/env/lunarwing.env" "LLM_BASE_URL=http://direct.test:3000/openai/v1"

# Precedence: LUNARWING_TEST_TENSORZERO_URL, then TENSORZERO_URL, then the
# baked proxy.env value (ROOT_A baked http://tz.test:3000 above).
out="$(LUNARWING_TEST_ROOT="$ROOT_A" LUNARWING_TEST_TENSORZERO_URL="http://first.test:3000" TENSORZERO_URL="http://second.test:3000" bash "$TEST_ENV" render-systemd 2>&1)"
rc=$?
assert_exit "render-systemd on an initialized root with both knobs" "0" "$rc"
assert_file_contains "LUNARWING_TEST_TENSORZERO_URL wins over TENSORZERO_URL and proxy.env" "$ROOT_A/systemd/lunarwing-proxy-test.service" "tensorzero http://first.test:3000"
out="$(env -u LUNARWING_TEST_TENSORZERO_URL LUNARWING_TEST_ROOT="$ROOT_A" TENSORZERO_URL="http://second.test:3000" bash "$TEST_ENV" render-systemd 2>&1)"
rc=$?
assert_exit "render-systemd on an initialized root with TENSORZERO_URL" "0" "$rc"
assert_file_contains "TENSORZERO_URL wins over the baked proxy.env value" "$ROOT_A/systemd/lunarwing-proxy-test.service" "tensorzero http://second.test:3000"

if grep -qF '192.168.1.157' "$TEST_ENV"; then
  fail "lunarwing-xmpp-test-env.sh source pin" "dev-LAN literal still present"
else
  pass "lunarwing-xmpp-test-env.sh carries no dev-LAN literal"
fi

echo "=== lunarwing_toolcall_diag.py: --host required, literal behind --default (WI#402) ==="

out="$(python3 "$DIAG" 2>&1)"
rc=$?
assert_exit "diag dies without --host" "2" "$rc"
assert_contains "error names --host" "$out" "--host is required"

out="$(timeout 20 python3 "$DIAG" --host x --default 2>&1)"
rc=$?
assert_exit "diag rejects --host with --default" "2" "$rc"
assert_contains "error names the conflict" "$out" "mutually exclusive"

python3 "$DIAG" --help >/dev/null 2>&1
rc=$?
assert_exit "diag --help exits 0" "0" "$rc"

out="$(DIAG_PATH="$DIAG" python3 - <<'PY'
import importlib.util
import os

spec = importlib.util.spec_from_file_location("diag", os.environ["DIAG_PATH"])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

a = m.parse_args(["--default"])
assert a.default is True and a.port == m.GATEWAY_PORT
assert m.DEFAULT_GATEWAY_HOST == "192.168.1.157"

a = m.parse_args(["--host", "10.0.0.9", "--port", "1234"])
assert a.host == "10.0.0.9" and a.port == 1234 and a.default is False
print("parse pins ok")
PY
)"
rc=$?
assert_exit "diag parse layer resolves --default and --host/--port" "0" "$rc"
assert_contains "parse pin output" "$out" "parse pins ok"

echo
if [[ "$failures" -gt 0 ]]; then
  echo "SINGLE-NODE ENDPOINT DEFAULTS: $failures FAILURES"
  exit 1
fi
echo "SINGLE-NODE ENDPOINT DEFAULTS: ALL CHECKS PASSED"
