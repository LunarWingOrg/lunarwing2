#!/usr/bin/env bash
# Stage A mt-admin routing, authorization, and lock-boundary regressions.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
calls="$fixture/calls"
locks="$fixture/locks"
lifecycle="$fixture/lifecycle"

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

tenant_exists_in_registry() { [[ "$1" == alice ]]; }
tenant_darkirc_enabled() { [[ "$1" == alice ]]; }
darkirc_scope_id() { printf '%s' '0123456789abcdef0123456789abcdef'; }
run_darkirc_key_helper() {
  local argument
  for argument in "$@"; do printf '%s\n' "$argument" >>"$calls"; done
  printf '%s\n' END >>"$calls"
}
with_darkirc_writer_lock() {
  local name="$1"
  shift
  printf '%s\n' "$name" >>"$locks"
  "$@"
}
with_tenant_lifecycle_lock() {
  local name="$1"
  shift
  printf '%s\n' "$name" >>"$lifecycle"
  "$@"
}

assert_call_contains() {
  local expected="$1"
  grep -Fqx -- "$expected" "$calls" || fail "missing helper argument: $expected"
}

assert_lock_count() {
  local path="$1" expected="$2"
  local actual=0
  [[ ! -f "$path" ]] || actual="$(wc -l <"$path")"
  [[ "$actual" == "$expected" ]] || fail "expected $expected locks in $path, got $actual"
}

: >"$calls"
darkirc_contact_command prepare alice 'victim"] #' --out offer.json --json
assert_call_contains prepare
assert_call_contains 'victim"] #'
assert_call_contains offer.json
assert_call_contains --scope-id
assert_lock_count "$locks" 1
assert_lock_count "$lifecycle" 1

: >"$calls"; : >"$locks"; : >"$lifecycle"
darkirc_contact_command list alice
assert_call_contains list
assert_call_contains --json
assert_lock_count "$locks" 0
assert_lock_count "$lifecycle" 0

: >"$calls"; : >"$locks"; : >"$lifecycle"
darkirc_contact_command complete alice bob \
  --exchange-id aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
  --in response.json \
  --expect-peer-fingerprint "sha256:$(printf 'a%.0s' {1..64})"
assert_call_contains complete
assert_call_contains --defer-apply
assert_call_contains --expect-peer-fingerprint
assert_lock_count "$locks" 1
assert_lock_count "$lifecycle" 1

if ( darkirc_contact_command complete alice bob \
  --exchange-id aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
  --in response.json ) >/dev/null 2>&1; then
  fail "complete accepted no expected fingerprint"
fi
if ( darkirc_contact_command complete alice bob \
  --exchange-id aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
  --in response.json --yes ) >/dev/null 2>&1; then
  fail "complete accepted generic --yes authorization"
fi

for operation in export-migration import-migration migration-ready recover; do
  : >"$calls"; : >"$locks"; : >"$lifecycle"
  darkirc_contact_command "$operation" alice
  assert_call_contains "$operation"
  assert_lock_count "$locks" 1
  assert_lock_count "$lifecycle" 1
done

: >"$calls"; : >"$locks"; : >"$lifecycle"
printf '%s' '{}' | darkirc_contact_command stage-migration alice --in -
assert_call_contains stage-migration
assert_lock_count "$locks" 1
assert_lock_count "$lifecycle" 1
if ( darkirc_contact_command stage-migration alice --in artifact.json ) >/dev/null 2>&1; then
  fail "stage-migration accepted a path instead of protected stdin"
fi

: >"$calls"; : >"$locks"; : >"$lifecycle"
printf '%s' '{}' | darkirc_contact_command restore-migration alice
assert_call_contains stage-migration
assert_call_contains import-migration
assert_lock_count "$locks" 1
assert_lock_count "$lifecycle" 1

for forbidden in apply confirm-roundtrip rotate revoke pair-local; do
  if ( darkirc_contact_command "$forbidden" alice ) >/dev/null 2>&1; then
    fail "future-stage command '$forbidden' is enabled"
  fi
done

printf 'ALL TESTS PASSED\n'
