#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d -t darkirc-stage-a.XXXXXX)"
trap 'rm -rf "$fixture"' EXIT
TENANT_LIFECYCLE_LOCK_DIR="$fixture/lifecycle-locks"
calls="$fixture/calls"
locks="$fixture/locks"

tenant_exists_in_registry() { [[ "$1" == "alice" ]]; }
tenant_darkirc_enabled() { [[ "$1" == "alice" ]]; }
darkirc_scope_id() { printf '%s' '0123456789abcdef0123456789abcdef'; }
run_darkirc_key_helper() { printf '%s\n' "$@" >>"$calls"; }
with_darkirc_writer_lock() {
  local name="$1"
  shift
  printf '%s\n' "$name" >>"$locks"
  "$@"
}
with_tenant_lifecycle_lock() {
  shift
  "$@"
}

failures=0
assert_line() {
  local label="$1" expected="$2"
  if grep -Fqx -- "$expected" "$calls"; then
    printf '  PASS: %s\n' "$label"
  else
    printf '  FAIL: %s\n' "$label"
    failures=$((failures + 1))
  fi
}

darkirc_contact_command prepare alice 'victim"] #' --out offer.json --json
assert_line "prepare reaches helper" prepare
assert_line "hostile contact stays one opaque argument" 'victim"] #'
assert_line "prepare forwards output path" offer.json
[[ "$(wc -l <"$locks")" == 1 ]] || failures=$((failures + 1))

: >"$locks"
darkirc_contact_command list alice --json
assert_line "list reaches helper" list
[[ ! -s "$locks" ]] || failures=$((failures + 1))

darkirc_contact_command complete alice bob \
  --exchange-id aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
  --in response.json \
  --expect-peer-fingerprint "sha256:$(printf 'a%.0s' {1..64})"
assert_line "complete remains deferred" --defer-apply
assert_line "complete forwards expected fingerprint" --expect-peer-fingerprint
[[ "$(wc -l <"$locks")" == 1 ]] || failures=$((failures + 1))

for migration_operation in export-migration stage-migration import-migration; do
  : >"$locks"
  darkirc_contact_command "$migration_operation" alice
  assert_line "$migration_operation reaches helper" "$migration_operation"
  [[ "$(wc -l <"$locks")" == 1 ]] || failures=$((failures + 1))
done

: >"$calls"
: >"$locks"
darkirc_contact_command restore-migration alice
assert_line "restore-migration stages manifest" stage-migration
assert_line "restore-migration imports staged manifest" import-migration
[[ "$(wc -l <"$locks")" == 1 ]] || failures=$((failures + 1))

lock_active="false"
readiness_checked="false"
upgrade_ran="false"
BUILD_LOCK="$fixture/build"
darkirc_managed_state_exists() { return 0; }
with_darkirc_writer_lock() {
  local name="$1" result=0
  shift
  [[ "$name" == alice ]] || return 1
  lock_active="true"
  "$@" || result=$?
  lock_active="false"
  return "$result"
}
run_darkirc_key_helper() {
  [[ "$lock_active" == true && "$2" == migration-ready ]] || return 1
  readiness_checked="true"
}
upgrade_tenant_locked() {
  [[ "$lock_active" == true ]] || return 1
  upgrade_ran="true"
}
upgrade_tenant alice target '' true true
if [[ "$readiness_checked" == true && "$upgrade_ran" == true && "$lock_active" == false ]]; then
  printf '  PASS: upgrade holds writer lock from readiness through upgrade\n'
else
  printf '  FAIL: upgrade holds writer lock from readiness through upgrade\n'
  failures=$((failures + 1))
fi

if [[ "$failures" -eq 0 ]]; then
  printf 'RESULT: PASS\n'
else
  printf 'RESULT: FAIL (%s)\n' "$failures"
  exit 1
fi
