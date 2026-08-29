#!/usr/bin/env bash
set -euo pipefail

TEST_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$TEST_SCRIPT_DIR/../lunarwing-mt-admin.sh"

fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT

fixture_token='ssh-api-fixture-token'
fixture_key='fixture-private-key-material'
fixture_port='20042'
upload_response='{"success":true}'
upload_status=0
failures=0

tenant_env_dir() { printf '%s/env' "$fixture_dir"; }
ports_get() {
  [[ "$2" == "http" ]] || return 1
  printf '%s' "$fixture_port"
}
generate_token() { printf '%s\n' "$fixture_token"; }
sleep() { :; }
say() { printf '%s\n' "$*" >>"$fixture_dir/messages.log"; }

curl() {
  local args="$*" config_fd="" arg
  if [[ "$args" == *"/agent/status"* ]]; then
    printf '%s\n' "$args" >"$fixture_dir/readiness.args"
  else
    printf '%s\n' "$args" >"$fixture_dir/upload.args"
    cat >"$fixture_dir/upload.body"
  fi
  while [[ $# -gt 0 ]]; do
    arg="$1"
    shift
    if [[ "$arg" == "-K" && $# -gt 0 ]]; then
      config_fd="$1"
      shift
    fi
  done
  if [[ -n "$config_fd" ]]; then
    cat "$config_fd" >"$fixture_dir/curl.auth"
  fi
  if [[ "$args" == *"/agent/status"* ]]; then
    return 0
  fi
  printf '%s' "$upload_response"
  return "$upload_status"
}

assert_eq() {
  local label="$1" actual="$2" expected="$3"
  if [[ "$actual" == "$expected" ]]; then
    printf 'PASS: %s\n' "$label"
  else
    printf 'FAIL: %s (expected %q, got %q)\n' "$label" "$expected" "$actual"
    failures=$((failures + 1))
  fi
}

assert_file_exists() {
  local label="$1" path="$2"
  if [[ -f "$path" ]]; then
    printf 'PASS: %s\n' "$label"
  else
    printf 'FAIL: %s (missing %s)\n' "$label" "$path"
    failures=$((failures + 1))
  fi
}

assert_file_absent() {
  local label="$1" path="$2"
  if [[ ! -e "$path" ]]; then
    printf 'PASS: %s\n' "$label"
  else
    printf 'FAIL: %s (unexpected %s)\n' "$label" "$path"
    failures=$((failures + 1))
  fi
}

assert_not_contains() {
  local label="$1" path="$2" value="$3"
  if [[ -f "$path" ]] && grep -qF "$value" "$path"; then
    printf 'FAIL: %s\n' "$label"
    failures=$((failures + 1))
  else
    printf 'PASS: %s\n' "$label"
  fi
}

env_path="$fixture_dir/env/lunarwing.env"
staged_key="$fixture_dir/env/ssh_key_staged"
mkdir -p "$fixture_dir/env"

printf 'GATEWAY_AUTH_TOKEN=preserved\n' >"$env_path"
chmod 0640 "$env_path"
ensure_ssh_api_token fixture
assert_eq 'missing token is generated' "$(_read_env_value "$env_path" SSH_API_TOKEN)" "$fixture_token"
assert_eq 'env mode is preserved' "$(stat -c '%a' "$env_path")" '640'
assert_eq 'token has one definition' "$(grep -c '^SSH_API_TOKEN=' "$env_path")" '1'

printf 'SSH_API_TOKEN=existing-token\n' >>"$env_path"
ensure_ssh_api_token fixture
assert_eq 'existing final token is preserved' "$(_read_env_value "$env_path" SSH_API_TOKEN)" 'existing-token'
assert_eq 'existing token is not duplicated' "$(grep -c '^SSH_API_TOKEN=' "$env_path")" '2'

_env_set "$env_path" SSH_API_TOKEN "$fixture_token"
printf '%s\n' "$fixture_key" >"$staged_key"
chmod 0600 "$staged_key"
upload_response='{"success":true}'
upload_status=0
upload_tenant_ssh_key fixture
assert_file_absent 'successful upload consumes staged key' "$staged_key"
assert_eq 'uploaded payload contains staged key' "$(jq -r .key_data "$fixture_dir/upload.body")" "$fixture_key"
assert_not_contains 'token absent from readiness argv' "$fixture_dir/readiness.args" "$fixture_token"
assert_not_contains 'token absent from upload argv' "$fixture_dir/upload.args" "$fixture_token"
assert_not_contains 'private key absent from upload argv' "$fixture_dir/upload.args" "$fixture_key"
assert_eq 'token sent through curl config' \
  "$(grep -cF "Authorization: Bearer $fixture_token" "$fixture_dir/curl.auth")" '1'

lifecycle_source="$TEST_SCRIPT_DIR/../mt-admin-lib/04-tenant-lifecycle.sh"
backfill_line="$(grep -n 'ensure_ssh_api_token "$name"' "$lifecycle_source" | head -1 | cut -d: -f1)"
start_line="$(grep -n 'with_tenant_lifecycle_lock.*_start_tenant_init_services' "$lifecycle_source" | head -1 | cut -d: -f1)"
restart_line="$(grep -n 'if \[\[ "$ssh_token_backfilled" == "true" \]\]' "$lifecycle_source" | head -1 | cut -d: -f1)"
upload_line="$(grep -n 'upload_tenant_ssh_key "$name"' "$lifecycle_source" | head -1 | cut -d: -f1)"
assert_eq 'token backfill happens before daemon start' "$((backfill_line < start_line))" '1'
assert_eq 'backfilled token restart happens before key upload' "$((restart_line < upload_line))" '1'

printf '%s\n' "$fixture_key" >"$staged_key"
upload_response='transport reflected fixture-private-key-material'
upload_status=22
rv=0
upload_tenant_ssh_key fixture || rv=$?
assert_eq 'transport failure returns nonzero' "$rv" '1'
assert_file_exists 'transport failure retains staged key' "$staged_key"
assert_not_contains 'transport response is not logged' "$fixture_dir/messages.log" "$fixture_key"

upload_response='{"success":false,"error":"fixture-private-key-material"}'
upload_status=0
rv=0
upload_tenant_ssh_key fixture || rv=$?
assert_eq 'semantic failure returns nonzero' "$rv" '1'
assert_file_exists 'semantic failure retains staged key' "$staged_key"
assert_not_contains 'semantic response is not logged' "$fixture_dir/messages.log" "$fixture_key"

_env_set "$env_path" SSH_API_TOKEN ''
rm -f "$fixture_dir/readiness.args" "$fixture_dir/upload.args"
rv=0
upload_tenant_ssh_key fixture || rv=$?
assert_eq 'missing token fails closed' "$rv" '1'
assert_file_absent 'missing token performs no readiness request' "$fixture_dir/readiness.args"
assert_file_absent 'missing token performs no upload request' "$fixture_dir/upload.args"
assert_file_exists 'missing token retains staged key' "$staged_key"

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all SSH key upload tests passed\n'
