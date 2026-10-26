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
  " $secret"
  "$secret "
  "$secret"$'\t'
  "$secret"$'\nPEBBLE_MODEL=injected'
)
labels=(double-quoted single-quoted leading-space trailing-space trailing-tab embedded-newline)
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
printf 'NANOGPT_API_KEY=%s\nPEBBLE_MODEL=openai/test-model\n' "$secret" >"$fixture/expected"
cmp -s "$fixture/expected" "$env_file" || fail 'clean key/model env-file output differs'
[[ "$output" != *"$secret"* ]] || fail 'clean configuration leaked key material'
[[ "$output" == *'stop-tenant alice'* && "$output" == *'start-tenant alice'* ]] \
  || fail 'existing container did not produce restart guidance'
mapfile -t calls <"$calls_log"
[[ ${#calls[@]} -eq 1 && "${calls[0]}" == 'alice inspect lunarwing-pebble-alice' ]] \
  || fail 'clean configuration did not follow the existing-container inspection path'
mapfile -t writes <"$writer_log"
[[ ${#writes[@]} -eq 1 ]] || fail 'clean configuration did not commit exactly one env file'

printf 'PASS: Pebble raw-key validation, env-file output, secret hygiene, and restart guidance\n'
