#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT

fixture_token='gateway-wait-fixture-token'
fixture_port='20042'
curl_calls=0
failures_before_success=0
failures=0

tenant_env_dir() { printf '%s/env' "$fixture_dir"; }
ports_get() {
  [[ "$2" == "gateway" ]] || return 1
  printf '%s' "$fixture_port"
}
sleep() { :; }
curl() {
  curl_calls=$((curl_calls + 1))
  printf '%s\n' "$*" >"$fixture_dir/curl-args"
  while [[ $# -gt 0 ]]; do
    if [[ "$1" == "-K" ]]; then
      cp "$2" "$fixture_dir/curl-config"
      shift 2
    else
      shift
    fi
  done
  ((curl_calls > failures_before_success))
}

assert() {
  local label="$1"
  shift
  if "$@"; then
    printf 'PASS: %s\n' "$label"
  else
    printf 'FAIL: %s\n' "$label"
    failures=$((failures + 1))
  fi
}

gateway_wait_fails() { ! _wait_tenant_gateway fixture; }

mkdir -p "$fixture_dir/env"
printf 'GATEWAY_AUTH_TOKEN=%s\n' "$fixture_token" >"$fixture_dir/env/lunarwing.env"

assert 'authenticated gateway probe succeeds' _wait_tenant_gateway fixture
assert 'gateway port and status route used' grep -q \
  "http://127.0.0.1:${fixture_port}/api/gateway/status" "$fixture_dir/curl-args"
assert 'token absent from argv' bash -c \
  '! grep -qF "$1" "$2"' _ "$fixture_token" "$fixture_dir/curl-args"
assert 'token sent through curl config' grep -qF \
  "Authorization: Bearer ${fixture_token}" "$fixture_dir/curl-config"

curl_calls=0
failures_before_success=1
assert 'transient failure is retried' _wait_tenant_gateway fixture
assert 'one failure causes two probes' test "$curl_calls" -eq 2

printf 'DARKIRC_ADAPTER_SECRET=not-a-gateway-token\n' >"$fixture_dir/env/lunarwing.env"
curl_calls=0
assert 'missing gateway token fails closed' gateway_wait_fails
if [[ "$curl_calls" -ne 0 ]]; then
  printf 'FAIL: missing token performs no request\n'
  failures=$((failures + 1))
else
  printf 'PASS: missing token performs no request\n'
fi

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all gateway wait tests passed\n'
