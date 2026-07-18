#!/usr/bin/env bash
# Failing-first (TDD) regression test for upload_tenant_darkirc_secret().
#
# Sources lunarwing-mt-admin.sh (its BASH_SOURCE guard skips main()/require_root
# when sourced) and drives upload_tenant_darkirc_secret() against a fixture
# tenant env file. All registry/env/curl interactions are stubbed so the test
# runs without a live tenant, root, or the ports registry.
#
# Contract under test (the function we are implementing):
#   - guard: no-op (return 0) when tenant_darkirc_enabled is false
#   - guard: no-op (return 0) when DARKIRC_ADAPTER_SECRET is empty/missing
#   - guard: no-op (return 1, warning) when GATEWAY_AUTH_TOKEN is missing
#   - happy path: POST to http://127.0.0.1:<port>/api/extensions/darkirc/setup
#       with Content-Type: application/json and Authorization: Bearer <token>,
#       body {"secrets":{"darkirc_adapter_secret":<value>},"fields":{}} on stdin
#   - success detection: semantic (response.success == true)
#   - failure handling: warn + continue (return 1), NEVER echo response body
#   - idempotency: re-running is safe (function is stateless, no staged file)
#   - secret safety: the fixture secret value must NEVER appear in test output
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$SCRIPT_DIR/../lunarwing-mt-admin.sh"

# shellcheck source=../lunarwing-mt-admin.sh
source "$ADMIN_SCRIPT"

# ---- Fixtures ---------------------------------------------------------------
# The fixture secret/token must NEVER appear in test stdout/stderr. We embed the
# fixture secret inside the FAILURE response body to prove the function never
# echoes response content (a regression test against leaking credentials).
FIXTURE_SECRET="fixt-7Gx_mZ9qLpwRk2.s3cret"
FIXTURE_TOKEN="fixt-token-9K2mP7vqLwRn"
FIXTURE_PORT="20042"

# ---- Harness state ----------------------------------------------------------
stub_dir=""        # temp dir holding captured curl calls + fixture env
failures=0
CURL_RESPONSE=""    # canned response the curl stub returns
CURL_CURL_RV=0      # canned exit status for the curl stub

# ---- Helpers ----------------------------------------------------------------
# 7 helper functions.
tenant_darkirc_enabled() { [[ "${DARKIRC_ENABLED:-false}" == "true" ]]; }

tenant_env_dir() { printf '%s/env' "$stub_dir"; }

ports_get() { printf '%s' "$FIXTURE_PORT"; printf '%s' "$2" >"${stub_dir}/ports_get_last_key_called.txt"; }

_wait_tenant_gateway() { return 0; }

say() { printf '%s\n' "$*" >>"${stub_dir}/say.log"; }

curl() {
  local this_n arg_fd
  this_n="$(ls "${stub_dir}"/curl_body.*.txt 2>/dev/null | wc -l)"
  this_n=$((this_n + 1))
  printf '%s\n' "$*" >"${stub_dir}/curl_args.${this_n}.txt"
  cat >"${stub_dir}/curl_body.${this_n}.txt"
  arg_fd=""
  while [[ $# -gt 0 ]]; do
    if [[ "$1" == "-K" ]]; then
      arg_fd="$2"
      shift 2
      continue
    fi
    shift
  done
  if [[ -n "$arg_fd" ]] && [[ -e "$arg_fd" ]]; then
    cat "$arg_fd" >"${stub_dir}/curl_auth.${this_n}.txt" 2>/dev/null || true
  fi
  printf '%s' "${CURL_RESPONSE}"
  return "${CURL_CURL_RV}"
}

curl_call_count() {
  ls "${stub_dir}"/curl_body.*.txt 2>/dev/null | wc -l
}

reset_curl_recordings() {
  rm -f "${stub_dir}"/curl_args.*.txt "${stub_dir}"/curl_body.*.txt "${stub_dir}"/curl_auth.*.txt "${stub_dir}"/ports_get_last_key_called.txt
}

# Writes a tenant lunarwing.env with the requested DarkIRC secret + token.
write_fixture_env() {
  local secret="$1" token="$2"
  mkdir -p "${stub_dir}/env"
  {
    printf 'GATEWAY_AUTH_TOKEN=%s\n' "$token"
    printf 'DARKIRC_ADAPTER_SECRET=%s\n' "$secret"
    printf 'DARKIRC_ADAPTER_URL=http://127.0.0.1:20010\n'
  } >"${stub_dir}/env/lunarwing.env"
  chmod 0600 "${stub_dir}/env/lunarwing.env"
}

# ---- Assert helpers ---------------------------------------------------------
# 4 assert functions.
assert_eq() { # <label> <actual> <expected>
  if [[ "$2" == "$3" ]]; then
    echo "  PASS: $1"
  else
    echo "  FAIL: $1"
    echo "        expected: [$3]"
    echo "        actual:   [$2]"
    failures=$((failures + 1))
  fi
}

assert_contains() { # <label> <haystack> <needle>
  if [[ "$2" == *"$3"* ]]; then
    echo "  PASS: $1"
  else
    echo "  FAIL: $1 (needle not found)"
    echo "        haystack: [$2]"
    echo "        needle:   [$3]"
    failures=$((failures + 1))
  fi
}

assert_not_contains() { # <label> <haystack> <needle>
  if [[ "$2" == *"$3"* ]]; then
    echo "  FAIL: $1 (needle unexpectedly found)"
    echo "        haystack: [$2]"
    echo "        needle:   [$3]"
    failures=$((failures + 1))
  else
    echo "  PASS: $1"
  fi
}

assert_json_path() { # <label> <json_file> <jq_filter> <expected>
  local got
  got="$(jq -r "$3" "$2")"
  assert_eq "$1" "$got" "$4"
}

assert_url_path() { # <label> <args_file> <expected_path>
  local args path
  args="$(cat "$2")"
  # curl args look like: -sf -X POST http://127.0.0.1:PORT/api/extensions/darkirc/setup ...
  path="$(printf '%s' "$args" | grep -oE 'https?://[^ ]+' | head -1)"
  path="${path#http://127.0.0.1:${FIXTURE_PORT}}"
  assert_eq "$1" "$path" "$3"
}

# ---- Test setup -------------------------------------------------------------
stub_dir="$(mktemp -d)"
# shellcheck disable=SC2064
trap 'rm -rf "$stub_dir"' EXIT

echo "=== upload_tenant_darkirc_secret() regression tests ==="

# The function must exist. In the RED phase it is undefined and this is the
# single failing check; in GREEN it is defined and the behavioral tests run.
if ! declare -f upload_tenant_darkirc_secret >/dev/null 2>&1; then
  echo "  FAIL: upload_tenant_darkirc_secret is not defined (RED — implement it)"
  failures=$((failures + 1))
  echo ""
  echo "RESULT: $failures failure(s)"
  exit "$failures"
fi

# --- Test 1: guard — DarkIRC disabled => no POST, returns 0 ---------------
echo ""
echo "--- Test 1: guard (DarkIRC disabled) ---"
DARKIRC_ENABLED=false
reset_curl_recordings
CURL_RESPONSE='{"success":true}'
write_fixture_env "$FIXTURE_SECRET" "$FIXTURE_TOKEN"
rv=0
upload_tenant_darkirc_secret "darktest" || rv=$?
assert_eq "disabled returns 0" "$rv" "0"
assert_eq "disabled performs no POST" "$(curl_call_count)" "0"

# --- Test 2: guard — secret empty => no POST, returns 0 --------------------
echo ""
echo "--- Test 2: guard (empty secret) ---"
DARKIRC_ENABLED=true
reset_curl_recordings
CURL_RESPONSE='{"success":true}'
write_fixture_env "" "$FIXTURE_TOKEN"
rv=0
upload_tenant_darkirc_secret "darktest" || rv=$?
assert_eq "empty secret returns 0" "$rv" "0"
assert_eq "empty secret performs no POST" "$(curl_call_count)" "0"

# --- Test 3: guard — token missing => warning, returns 1, no POST ----------
echo ""
echo "--- Test 3: guard (missing token) ---"
DARKIRC_ENABLED=true
reset_curl_recordings
CURL_RESPONSE='{"success":true}'
write_fixture_env "$FIXTURE_SECRET" ""
rv=0
upload_tenant_darkirc_secret "darktest" || rv=$?
assert_eq "missing token returns 1" "$rv" "1"
assert_eq "missing token performs no POST" "$(curl_call_count)" "0"
assert_contains "missing token warns" "$(cat "${stub_dir}/say.log")" "GATEWAY_AUTH_TOKEN"

# --- Test 4: happy path — payload shape, headers, URL, method --------------
echo ""
echo "--- Test 4: happy path (payload + headers + URL) ---"
DARKIRC_ENABLED=true
reset_curl_recordings
CURL_RESPONSE='{"success":true,"message":"DarkIRC adapter configured","activated":true}'
write_fixture_env "$FIXTURE_SECRET" "$FIXTURE_TOKEN"
rv=0
upload_tenant_darkirc_secret "darktest" || rv=$?
assert_eq "happy path returns 0" "$rv" "0"
assert_eq "happy path performs exactly one POST" "$(curl_call_count)" "1"

# Body shape: {"secrets":{"darkirc_adapter_secret":<value>},"fields":{}}
body_file="${stub_dir}/curl_body.$(curl_call_count).txt"
assert_json_path "body has secrets.darkirc_adapter_secret (string, non-empty)" \
  "$body_file" '.secrets.darkirc_adapter_secret | if type=="string" and length>0 then "ok" else "bad" end' "ok"
assert_json_path "body.fields is empty object" \
  "$body_file" '.fields | if .=={} then "empty" else "non-empty" end' "empty"
# Exact secret value is transmitted, but never echoed by the test.
transmitted_secret="$(jq -r '.secrets.darkirc_adapter_secret' "$body_file")"
if [[ "$transmitted_secret" == "$FIXTURE_SECRET" ]]; then
  echo "  PASS: transmitted secret matches fixture (value redacted in output)"
else
  echo "  FAIL: transmitted secret does not match fixture (value redacted)"
  failures=$((failures + 1))
fi

# Headers + method + URL.
args_file="${stub_dir}/curl_args.$(curl_call_count).txt"
args="$(cat "$args_file")"
assert_contains "uses POST method" "$args" "-X POST"
assert_contains "sets Content-Type: application/json" "$args" "Content-Type: application/json"
assert_not_contains "argv contains no fixture token" "$args" "$FIXTURE_TOKEN"
assert_not_contains "argv contains no fixture secret" "$args" "$FIXTURE_SECRET"

auth_file="${stub_dir}/curl_auth.$(curl_call_count).txt"
if [[ -s "$auth_file" ]]; then
  auth_content="$(cat "$auth_file")"
  if [[ "$auth_content" == *"$FIXTURE_TOKEN"* ]]; then
    echo "  PASS: Authorization header carries fixture token (value redacted)"
  else
    echo "  FAIL: Authorization header does not carry fixture token (value redacted)"
    failures=$((failures + 1))
  fi
  assert_contains "auth FD sets Bearer scheme" "$auth_content" "Authorization: Bearer"
else
  echo "  FAIL: no curl auth config captured via -K /dev/fd/N"
  failures=$((failures + 1))
fi
assert_url_path "posts to /api/extensions/darkirc/setup" "$args_file" "/api/extensions/darkirc/setup"

last_key="$(cat "${stub_dir}/ports_get_last_key_called.txt" 2>/dev/null || true)"
assert_eq "requests the gateway port (not http)" "$last_key" "gateway"

# --- Test 5: failure handling — warn + continue, never echo response -------
echo ""
echo "--- Test 5: failure handling (no credential leak) ---"
DARKIRC_ENABLED=true
reset_curl_recordings
# Response body deliberately embeds the fixture secret to prove the function
# never echoes response content (which could expose credentials).
CURL_RESPONSE="{\"success\":false,\"message\":\"adapter rejected secret ${FIXTURE_SECRET}\"}"
write_fixture_env "$FIXTURE_SECRET" "$FIXTURE_TOKEN"
say_file_before="$(cat "${stub_dir}/say.log")"
rv=0
upload_tenant_darkirc_secret "darktest" || rv=$?
assert_eq "failure returns 1" "$rv" "1"
say_file_after="$(cat "${stub_dir}/say.log")"
new_say="${say_file_after#"${say_file_before}"}"
assert_contains "failure emits a warning" "$new_say" "WARNING"
# The fixture secret must not appear anywhere the function wrote.
if [[ "$new_say" == *"$FIXTURE_SECRET"* ]]; then
  echo "  FAIL: function leaked the fixture secret in its warning output"
  failures=$((failures + 1))
else
  echo "  PASS: function did not echo the fixture secret on failure"
fi

# --- Test 6: idempotency — re-running is safe (stateless) ------------------
echo ""
echo "--- Test 6: idempotency (re-run) ---"
DARKIRC_ENABLED=true
reset_curl_recordings
CURL_RESPONSE='{"success":true,"activated":true}'
write_fixture_env "$FIXTURE_SECRET" "$FIXTURE_TOKEN"
rv_first=0
upload_tenant_darkirc_secret "darktest" || rv_first=$?
rv_second=0
upload_tenant_darkirc_secret "darktest" || rv_second=$?
assert_eq "first run returns 0" "$rv_first" "0"
assert_eq "second run returns 0" "$rv_second" "0"
assert_eq "two runs => two idempotent POSTs" "$(curl_call_count)" "2"

# ---- Global no-leak gate ----------------------------------------------------
# The fixture secret and token must never appear in test stdout/stderr. We
# cannot introspect our own stdout, but we can assert the captured say.log and
# the curl stub recordings never leaked into anything the test prints. The
# strongest signal: none of the assert helpers ever printed the secret above.
echo ""
echo "=== RESULT: $failures failure(s) ==="
exit "$failures"
