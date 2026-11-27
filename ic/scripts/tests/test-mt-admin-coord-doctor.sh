#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

umask 077
fixture="$(mktemp -d)"
listener_pid=""
cleanup() {
  if [[ -n "$listener_pid" ]]; then
    kill "$listener_pid" 2>/dev/null || true
    wait "$listener_pid" 2>/dev/null || true
  fi
  rm -rf -- "$fixture"
}
trap cleanup EXIT

python3 - "$fixture" <<'PY' &
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
import sys

fixture = Path(sys.argv[1])

class StatusEndpoint(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path != "/v1/status":
            self.send_error(404)
            return
        if self.headers.get("Authorization") != "Bearer fixture-only":
            self.send_error(401)
            return
        body = (fixture / "status").read_bytes()
        if not body:
            self.send_error(503)
            return
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass

with HTTPServer(("127.0.0.1", 0), StatusEndpoint) as server:
    (fixture / "port").write_text(f"{server.server_port}\n")
    server.serve_forever()
PY
listener_pid=$!
for ((attempt = 0; attempt < 100; attempt++)); do
  [[ -s "$fixture/port" ]] && break
  sleep 0.05
done
[[ -s "$fixture/port" ]] || { printf 'FAIL: status fixture did not start\n'; exit 1; }
read -r fixture_port <"$fixture/port"
export no_proxy=127.0.0.1 NO_PROXY=127.0.0.1

fixture_tenant="$(id -un)"
ports_registry_init() { :; }
tenant_exists_in_registry() { [[ "$1" == "$fixture_tenant" ]]; }
tenant_env_dir() { printf '%s\n' "$fixture"; }
ports_get() { [[ "$2" == bridge ]] && printf '%s\n' "$fixture_port"; }

alpha='alpha@conference.example.test'
beta='beta@conference.example.test'
printf '%s\n' 'COORD_ENABLED=true' "COORD_CHANNELS=$alpha,$beta" >"$fixture/lunarwing.env"

set_static_allowlist() {
  printf '%s\n' 'XMPP_BRIDGE_TOKEN=fixture-only' "$1" >"$fixture/xmpp-bridge.env"
}

check_status() {
  local label="$1" body="$2" expected_rc="$3" rc=0 output fragment
  shift 3
  printf '%s' "$body" >"$fixture/status"
  output="$(coord_doctor "$fixture_tenant" 2>&1)" || rc=$?
  if [[ "$output" == *fixture-only* ]]; then
    printf 'FAIL: %s (credential printed)\n' "$label" >&2
    exit 1
  fi
  if [[ "$rc" -ne "$expected_rc" ]]; then
    printf 'FAIL: %s (exit %s, expected %s)\n%s\n' "$label" "$rc" "$expected_rc" "$output" >&2
    exit 1
  fi
  for fragment in "$@"; do
    if [[ "$fragment" == !* ]]; then
      [[ "$output" != *"${fragment:1}"* ]] && continue
    elif [[ "$output" == *"$fragment"* ]]; then
      continue
    fi
    printf 'FAIL: %s (output assertion: %s)\n%s\n' "$label" "$fragment" "$output" >&2
    exit 1
  done
  printf 'PASS: %s\n' "$label"
}

set_static_allowlist 'XMPP_ALLOW_ROOMS_JSON=["stale@conference.example.test"]'
check_status 'empty presence fails for each configured room' \
  "{\"configured_rooms\":[\"$alpha\",\"$beta\"],\"rooms_with_presence\":[]}" 1 \
  "[FAIL] room '$alpha' is configured but shows no presence" \
  "[FAIL] room '$beta' is configured but shows no presence" 'failed=2 warnings=0'
check_status 'partial presence identifies the joined and missing rooms' \
  "{\"configured_rooms\":[\"$alpha\",\"$beta\"],\"rooms_with_presence\":[\"$beta\"]}" 1 \
  "[PASS] room '$beta' shows presence" \
  "[FAIL] room '$alpha' is configured but shows no presence" 'failed=1 warnings=0'
check_status 'reordered presence passes and live configuration overrides stale env' \
  "{\"configured_rooms\":[\"$alpha\",\"$beta\"],\"rooms_with_presence\":[\"$beta\",\"$alpha\"]}" 0 \
  "[PASS] coordination room '$alpha' is on the bridge allowlist" \
  "[PASS] coordination room '$beta' is on the bridge allowlist" \
  "[PASS] room '$alpha' shows presence" "[PASS] room '$beta' shows presence" \
  '!static fallback' 'failed=0 warnings=0'
check_status 'missing presence field warns without overriding live configuration' \
  "{\"configured_rooms\":[\"$alpha\",\"$beta\"]}" 0 \
  '[WARN] bridge /v1/status has no usable rooms_with_presence list' \
  '!static fallback' '!shows presence' 'failed=0 warnings=1'

set_static_allowlist "XMPP_ALLOW_ROOMS=$alpha,$beta"
check_status 'unavailable status falls back to static config, not presence' '' 0 \
  '[WARN] bridge /v1/status unreachable' 'Bridge config (static fallback, not live)' \
  "[PASS] coordination room '$alpha' is on the bridge allowlist" \
  "[PASS] coordination room '$beta' is on the bridge allowlist" \
  '!shows presence' 'failed=0 warnings=1'
check_status 'explicitly empty live allowlist overrides configured env' \
  '{"configured_rooms":[],"rooms_with_presence":[]}' 1 \
  '[FAIL] no MUC room configured on the bridge' '!static fallback' 'failed=3 warnings=0'

set_static_allowlist "XMPP_ALLOW_ROOMS_JSON=[\"$alpha\",\"$beta\"]"
check_status 'invalid status falls back without asserting presence' 'not JSON' 0 \
  '[WARN] bridge /v1/status has no usable configured_rooms list' \
  'Bridge config (static fallback, not live)' '!shows presence' 'failed=0 warnings=1'
