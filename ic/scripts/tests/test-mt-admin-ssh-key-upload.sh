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
# WI#423 (run-166 LF-4): the upload now reads SSH_API_TOKEN through the
# tenant-side per-key reader, which drops privileges with sudo -u <tenant>.
# The fixture tenant is not a real account (and CI runs no-new-privileges), so
# map the drop onto the invoking user — the fixture env dir is 0700 and the env
# 0600, both owned by that user, satisfying every tenant-side check.
sudo() {
  if [[ "${1:-}" == "-u" ]]; then shift 2; fi
  command "$@"
}

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
chmod 700 "$fixture_dir/env"

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

# WI#423: the upload's tenant-side reader demands the production shape
# (0600 env inside the 0700 dir) before the upload section runs.
chmod 0600 "$env_path"
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

# WI#423 (run-166 LF-4): the upload bearer must come from the tenant-side
# reader — a lunarwing.env symlinked at a foreign env used to be read as root,
# handing the OTHER file's SSH_API_TOKEN to this tenant's loopback gateway as
# the bearer. The tenant-side reader refuses the symlinked path; the upload
# fails closed, nothing is sent, and the staged key waits for the next pass.
lf4_victim="$fixture_dir/lf4-victim.env"
printf 'SSH_API_TOKEN=foreign-tenant-bearer\n' >"$lf4_victim"
chmod 0600 "$lf4_victim"
lf4_victim_before="$(cat "$lf4_victim")"
mv "$env_path" "$fixture_dir/lf4-real.env"
ln -s "$lf4_victim" "$env_path"
rm -f "$fixture_dir/readiness.args" "$fixture_dir/upload.args" "$fixture_dir/messages.log"
printf '%s\n' "$fixture_key" >"$staged_key"
rv=0
upload_tenant_ssh_key fixture || rv=$?
assert_eq 'symlinked env fails closed' "$rv" '1'
if grep -q 'could not safely read SSH_API_TOKEN' "$fixture_dir/messages.log" 2>/dev/null; then
  printf 'PASS: symlink refusal names the unreadable credential\n'
else
  printf 'FAIL: symlink refusal does not name the unreadable credential\n'
  failures=$((failures + 1))
fi
assert_file_absent 'symlinked env performs no readiness request' "$fixture_dir/readiness.args"
assert_file_absent 'symlinked env performs no upload request' "$fixture_dir/upload.args"
assert_file_exists 'symlinked env retains staged key' "$staged_key"
assert_eq 'symlink refusal leaves the foreign env untouched' "$(cat "$lf4_victim")" "$lf4_victim_before"
rm -f "$env_path"
mv "$fixture_dir/lf4-real.env" "$env_path"

# WI#423 (run-171 LS-2): the staged key itself used to be read by root
# (`jq --rawfile` on the tenant-writable path). A symlink planted at
# env/ssh_key_staged would hand any root-readable file to the tenant's own
# daemon. The key is now snapshotted AS THE TENANT: what the tenant cannot
# read is refused, nothing is sent, and the root-private snapshot never
# outlives the call. The tenant-side refusal is modelled by a tenant_cat that
# denies reads through a symlink (the invoking user may be root here).
ls2_orig_tenant_cat="$(declare -f tenant_cat)"
ls2_tenant_cat() {
  if [[ -L "$2" ]]; then
    printf 'cat: %s: Permission denied\n' "$2" >&2
    return 1
  fi
  cat -- "$2"
}
tenant_cat() { ls2_tenant_cat "$@"; }
ls2_tmp="$fixture_dir/ls2-tmp"
mkdir -p "$ls2_tmp"
ls2_victim="$fixture_dir/ls2-root-only.key"
ls2_secret='root-only-secret-material'
_env_set "$env_path" SSH_API_TOKEN "$fixture_token"
printf '%s\n' "$ls2_secret" >"$ls2_victim"
chmod 0600 "$ls2_victim"
rm -f "$staged_key" "$fixture_dir/readiness.args" "$fixture_dir/upload.args" "$fixture_dir/upload.body" "$fixture_dir/messages.log"
ln -s "$ls2_victim" "$staged_key"
upload_response='{"success":true}'
upload_status=0
rv=0
TMPDIR="$ls2_tmp" upload_tenant_ssh_key fixture || rv=$?
assert_eq 'symlinked staged key fails closed' "$rv" '1'
if grep -q 'could not safely read the staged SSH key' "$fixture_dir/messages.log" 2>/dev/null; then
  printf 'PASS: staged-key refusal names the unreadable key\n'
else
  printf 'FAIL: staged-key refusal does not name the unreadable key\n'
  failures=$((failures + 1))
fi
assert_file_absent 'symlinked staged key performs no readiness request' "$fixture_dir/readiness.args"
assert_file_absent 'symlinked staged key performs no upload request' "$fixture_dir/upload.args"
assert_not_contains 'root-only file never reaches the upload body' "$fixture_dir/upload.body" "$ls2_secret"
if [[ -L "$staged_key" ]]; then
  printf 'PASS: symlinked staged key is left in place\n'
else
  printf 'FAIL: symlinked staged key was removed or replaced\n'
  failures=$((failures + 1))
fi
assert_eq 'refusal leaves no key snapshot behind' "$(find "$ls2_tmp" -mindepth 1 | wc -l)" '0'
# Control: a staged key the tenant can read still uploads, and the snapshot
# is gone afterwards.
rm -f "$staged_key"
printf '%s\n' "$fixture_key" >"$staged_key"
chmod 0600 "$staged_key"
rv=0
TMPDIR="$ls2_tmp" upload_tenant_ssh_key fixture || rv=$?
assert_eq 'readable staged key still uploads' "$rv" '0'
assert_eq 'uploaded payload comes from the tenant-side snapshot' "$(jq -r .key_data "$fixture_dir/upload.body")" "$fixture_key"
assert_eq 'successful upload leaves no key snapshot behind' "$(find "$ls2_tmp" -mindepth 1 | wc -l)" '0'

# WI#423 (run-171 LS-3): provision_tenant_ssh_key used to `cat` the tenant's
# public key as root into a grep argument — a symlink at ~/.ssh/*.pub would
# leak any root-readable file into a command line every local user can read.
# The key is now read as the tenant (the same denying tenant_cat models a
# target the tenant cannot read) and matched on stdin.
tenant_home() { printf '%s/home' "$fixture_dir"; }
ls3_ssh="$fixture_dir/home/.ssh"
mkdir -p "$ls3_ssh"
chmod 700 "$ls3_ssh"
printf 'dummy-private-key\n' >"$ls3_ssh/id_ed25519_lunarwing"
chmod 600 "$ls3_ssh/id_ed25519_lunarwing"
ls3_victim="$fixture_dir/ls3-root-only.pub"
printf 'ssh-ed25519 AAAAROOTONLY root-only-line\n' >"$ls3_victim"
ln -s "$ls3_victim" "$ls3_ssh/id_ed25519_lunarwing.pub"
rm -f "$staged_key" "$fixture_dir/messages.log"
rv=0
ls3_err="$( ( provision_tenant_ssh_key fixture ) 2>&1 )" || rv=$?
assert_eq 'symlinked public key fails closed' "$rv" '1'
if [[ "$ls3_err" == *"cannot read the public key"* ]]; then
  printf 'PASS: public-key refusal names the unreadable key\n'
else
  printf 'FAIL: public-key refusal does not name the unreadable key: %s\n' "$ls3_err"
  failures=$((failures + 1))
fi
assert_not_contains 'root-only line never reaches authorized_keys' "$ls3_ssh/authorized_keys" 'root-only-line'
assert_file_absent 'symlinked public key stages no private key' "$staged_key"
# Control: a readable public key is appended once and stays single.
rm -f "$ls3_ssh/id_ed25519_lunarwing.pub"
ls3_line='ssh-ed25519 AAAATESTKEY lunarwing-ssh-harness-fixture'
printf '%s\n' "$ls3_line" >"$ls3_ssh/id_ed25519_lunarwing.pub"
provision_tenant_ssh_key fixture
provision_tenant_ssh_key fixture
assert_eq 'readable public key is authorized exactly once' "$(grep -cF "$ls3_line" "$ls3_ssh/authorized_keys")" '1'
assert_eq 'readable public key stages the private key' "$(cat "$staged_key")" 'dummy-private-key'
rm -f "$staged_key"
eval "$ls2_orig_tenant_cat"

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all SSH key upload tests passed\n'
