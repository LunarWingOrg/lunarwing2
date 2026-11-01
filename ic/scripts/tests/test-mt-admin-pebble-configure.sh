#!/usr/bin/env bash
# WI#471: Pebble configuration accepts only a raw, single-line NanoGPT token.
# shellcheck disable=SC1091 # mt-admin is sourced from a computed path
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d)"
trap 'rm -rf -- "$fixture"' EXIT
env_dir="$fixture/env"
env_file="$env_dir/pebble.env"
calls_log="$fixture/container-calls"
writer_log="$fixture/writer-calls"
mkdir -p "$env_dir"

fail() { printf 'FAIL: %s\n' "$1" >&2; exit 1; }
tenant_exists_in_registry() { return 0; }
tenant_env_dir() { printf '%s/env\n' "$fixture"; }
# WI#506: configure_pebble reads the existing backend tenant-side before
# rewriting pebble.env; the fixture supplies it directly.
EXISTING_BASE_URL=""
read_tenant_env_value() {
  [[ "$1" == alice && "$3" == PEBBLE_BASE_URL ]] || return 0
  [[ -n "$EXISTING_BASE_URL" ]] && printf '%s\n' "$EXISTING_BASE_URL"
  return 0
}
worker_egress_enabled() { return 1; }
_ctr() {
  printf '%s\n' "$*" >>"$calls_log"
  return "${container_inspect_rc:-0}"
}
write_tenant_file_atomic() {
  [[ "$1" == alice && "$2" == "$env_file" && "$3" == 600 ]] || return 1
  printf '%s\n' "$*" >>"$writer_log"
  cat >"$2"
}

secret='sk-nano-fixture-secret'
printf 'NANOGPT_API_KEY=previous-fixture-key\nPEBBLE_MODEL=previous-model\n' >"$env_file"
before_sha="$(sha256sum "$env_file" | cut -d' ' -f1)"
bad_keys=(
  "\"$secret\""
  "'$secret'"
  "NANOGPT_API_KEY=$secret"
  "export NANOGPT_API_KEY=$secret"
  $'export\t'"NANOGPT_API_KEY=$secret"
  "export  NANOGPT_API_KEY=$secret"
  "\"$secret"
  "$secret\""
  "'$secret"
  "$secret'"
  '"'
  "'"
  "\"$secret'"
  "'$secret\""
  " $secret"
  "$secret "
  "$secret"$'\t'
  "$secret"$'\nPEBBLE_MODEL=injected'
)
labels=(
  double-quoted single-quoted
  assignment exported-assignment tab-exported-assignment spaced-exported-assignment
  leading-double-quote trailing-double-quote leading-single-quote trailing-single-quote
  lone-double-quote lone-single-quote mismatched-double-single-quotes mismatched-single-double-quotes
  leading-space trailing-space trailing-tab embedded-newline
)
for index in "${!bad_keys[@]}"; do
  bad_key="${bad_keys[$index]}"
  if output="$(configure_pebble alice "$bad_key" openai/rejected 2>&1)"; then
    fail "${labels[$index]} key was accepted"
  fi
  [[ "$output" == *NANOGPT_API_KEY* && "$output" == *raw* ]] \
    || fail "${labels[$index]} rejection is not actionable"
  [[ "$output" != *"$secret"* ]] || fail "${labels[$index]} rejection leaked key material"
  [[ "$(sha256sum "$env_file" | cut -d' ' -f1)" == "$before_sha" ]] \
    || fail "${labels[$index]} rejection changed pebble.env"
done
[[ ! -e "$calls_log" ]] || fail 'rejected keys reached the container restart path'
[[ ! -e "$writer_log" ]] || fail 'rejected keys reached the env-file writer'

rm -f -- "$env_file"
if output="$(configure_pebble alice " $secret" '' 2>&1)"; then
  fail 'padded key was accepted without an existing env file'
fi
[[ "$output" != *"$secret"* ]] || fail 'missing-file rejection leaked key material'
[[ ! -e "$env_file" ]] || fail 'rejected key created pebble.env'

output="$(configure_pebble alice "$secret" openai/test-model 2>&1)" \
  || fail 'clean key was rejected'
printf 'NANOGPT_API_KEY=%s\nPEBBLE_MODEL=openai/test-model\nPEBBLE_BASE_URL=https://nano-gpt.com/api\nNANOGPT_BASE_URL=https://nano-gpt.com/api\n' \
  "$secret" >"$fixture/expected"
cmp -s "$fixture/expected" "$env_file" || fail 'clean key/model env-file output differs'
[[ "$output" != *"$secret"* ]] || fail 'clean configuration leaked key material'
[[ "$output" == *'stop-tenant alice'* && "$output" == *'start-tenant alice'* ]] \
  || fail 'existing container did not produce restart guidance'

# WI#506 migration: a tenant configured before this change has no backend key;
# re-running configure-pebble pins the default without touching the key/model.
printf 'NANOGPT_API_KEY=%s\nPEBBLE_MODEL=openai/test-model\n' "$secret" >"$env_file"
configure_pebble alice "$secret" openai/test-model >/dev/null 2>&1 \
  || fail 'legacy env-file migration was rejected'
grep -Fxq 'PEBBLE_BASE_URL=https://nano-gpt.com/api' "$env_file" \
  || fail 'legacy env file did not gain the default backend'

# WI#506: an omitted --base-url preserves a configured backend, so rotating the
# API key cannot silently re-point the lane at a different provider.
EXISTING_BASE_URL='https://backend.example/v1'
configure_pebble alice "$secret" '' >/dev/null 2>&1 || fail 'preserved-backend reconfigure was rejected'
printf 'NANOGPT_API_KEY=%s\nPEBBLE_BASE_URL=%s\nNANOGPT_BASE_URL=%s\n' \
  "$secret" "$EXISTING_BASE_URL" "$EXISTING_BASE_URL" >"$fixture/expected"
cmp -s "$fixture/expected" "$env_file" || fail 'configured backend was not preserved on reconfigure'

EXISTING_BASE_URL=""
output="$(configure_pebble alice "$secret" openai/test-model https://explicit.example/api 2>&1)" \
  || fail 'explicit --base-url was rejected'
grep -Fxq 'PEBBLE_BASE_URL=https://explicit.example/api' "$env_file" \
  || fail 'explicit backend missing from pebble.env'
grep -Fxq 'NANOGPT_BASE_URL=https://explicit.example/api' "$env_file" \
  || fail 'explicit backend was not written in the key pebble honours'
[[ "$output" == *'model backend https://explicit.example/api'* ]] \
  || fail 'configure did not report the pinned backend'

before_sha="$(sha256sum "$env_file" | cut -d' ' -f1)"
if output="$(configure_pebble alice "$secret" openai/test-model 'ftp://insecure.example/api' 2>&1)"; then
  fail 'non-http backend URL was accepted'
fi
[[ "$output" == *'--base-url'* && "$output" == *'http(s)'* ]] \
  || fail 'invalid backend rejection is not actionable'
[[ "$(sha256sum "$env_file" | cut -d' ' -f1)" == "$before_sha" ]] \
  || fail 'invalid backend rejection changed pebble.env'

mapfile -t calls <"$calls_log"
# The typed subcommand is the point: a bare `inspect` resolves a container name
# that is a prefix of the worker IPC volume (`*-ipc-v2`), a false-positive
# existence match (see _ctr_inspect in mt-admin-lib/02-containers.sh).
[[ ${#calls[@]} -eq 4 ]] || fail "expected four container inspections (got ${#calls[@]})"
for call in "${calls[@]}"; do
  [[ "$call" == 'alice container inspect lunarwing-pebble-alice' ]] \
    || fail "unexpected container call: $call"
done
mapfile -t writes <"$writer_log"
[[ ${#writes[@]} -eq 4 ]] || fail "clean configuration did not commit four env files (got ${#writes[@]})"

printf 'PASS: Pebble raw-key validation, env-file output, backend pinning, secret hygiene, and restart guidance\n'
