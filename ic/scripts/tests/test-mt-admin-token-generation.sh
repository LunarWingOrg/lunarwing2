#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../mt-admin-lib/00-common.sh
source "$SCRIPT_DIR/../mt-admin-lib/00-common.sh"

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  exit 1
}

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

token_a="$(generate_token)"
token_b="$(generate_token)"
[[ "$token_a" =~ ^[0-9a-f]{64}$ ]] || fail "token is not 32 bytes of lowercase hex"
[[ "$token_a" != "0000000000000000000000000000000000000000000000000000000000000000" ]] \
  || fail "token is all zeroes"
[[ "$token_a" != "$token_b" ]] || fail "consecutive tokens are identical"
printf 'PASS: generate_token returns fresh 32-byte hex tokens\n'

mkdir -p "$fixture/bin"
for tool in date dd dirname tr; do
  ln -s "$(command -v "$tool")" "$fixture/bin/$tool"
done

missing_od_status=0
if missing_od_output="$(PATH="$fixture/bin" generate_token 2>&1)"; then
  fail "generate_token succeeded without od: $missing_od_output"
else
  missing_od_status=$?
fi
[[ "$missing_od_status" == "1" ]] || fail "missing od returned status $missing_od_status"
[[ "$missing_od_output" == "error: required command not found: od" ]] \
  || fail "missing od returned an unexpected error: $missing_od_output"
[[ "$missing_od_output" != *"replace-with-random-token-"* ]] \
  || fail "missing od emitted a predictable credential"
printf 'PASS: generate_token refuses to mint credentials without od\n'

dd() { :; }
invalid_status=0
if invalid_output="$(generate_token 2>&1)"; then
  fail "generate_token returned invalid random data"
else
  invalid_status=$?
fi
[[ "$invalid_status" == "1" ]] || fail "invalid random data returned status $invalid_status"
[[ "$invalid_output" == "error: failed to generate secure token" ]] \
  || fail "invalid random data returned an unexpected error: $invalid_output"
printf 'PASS: generate_token rejects invalid random data\n'

harness_script="$fixture/lunarwing-xmpp-test-env.sh"
sed '/^main "\$@"$/d' "$SCRIPT_DIR/../lunarwing-xmpp-test-env.sh" >"$harness_script"
harness_status=0
if harness_output="$(PATH="$fixture/bin" /bin/bash -c 'source "$1"; generate_token' _ "$harness_script" 2>&1)"; then
  fail "XMPP harness generate_token succeeded without od: $harness_output"
else
  harness_status=$?
fi
[[ "$harness_status" == "1" ]] || fail "XMPP harness missing od returned status $harness_status"
[[ "$harness_output" == "error: od is required" ]] \
  || fail "XMPP harness missing od returned an unexpected error: $harness_output"
[[ "$harness_output" != *"replace-with-random-token-"* ]] \
  || fail "XMPP harness emitted a predictable credential"
printf 'PASS: XMPP harness refuses to mint credentials without od\n'

printf 'ALL TESTS PASSED\n'
