#!/usr/bin/env bash
# WI#703: loading lunarwing-mt-admin.sh against an unsafe health library must
# not abort the CLI while it is sourcing its libraries. The chosen design is a
# scoped refusal: the OpenRC proxy unit contract is not sourced, the refusal is
# reported with the failing path component, and only OpenRC operations refuse.
#
# Each case stages a throwaway checkout (the real ic/scripts plus a health
# directory) so the loader runs exactly as it does in production, with the
# composition root's `set -euo pipefail` in force.
set -euo pipefail

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
CONTRACT_SRC="$REPO_ROOT/ic-infrastructure-health-check/openrc-proxy-unit-contract.sh"

failures=0
WORK=""
cleanup() { [[ -z "$WORK" ]] || rm -rf -- "$WORK"; }
trap cleanup EXIT

pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1" >&2; failures=$((failures + 1)); }

assert_eq() {
  local label="$1" expected="$2" actual="$3"
  if [[ "$actual" == "$expected" ]]; then pass "$label"; else fail "$label (expected '$expected', got '$actual')"; fi
}

assert_contains() {
  local label="$1" text="$2" needle="$3"
  if [[ "$text" == *"$needle"* ]]; then pass "$label"; else fail "$label (missing '$needle')"; fi
}

assert_absent() {
  local label="$1" text="$2" needle="$3"
  if [[ "$text" != *"$needle"* ]]; then pass "$label"; else fail "$label (unexpected '$needle')"; fi
}

stage_scripts() {  # <root>
  mkdir -p "$1/ic"
  cp -a "$REPO_ROOT/ic/scripts" "$1/ic/scripts"
}

stage_linked_checkout() {  # <root> <health-library-dir>
  local root="$1" lib="$2"
  stage_scripts "$root"
  mkdir -p "$lib"
  cp "$CONTRACT_SRC" "$lib/"
  ln -s "$lib" "$root/ic-infrastructure-health-check"
}

stage_plain_checkout() {  # <root> [contract-mode]
  local root="$1" mode="${2:-0644}"
  stage_scripts "$root"
  mkdir -p "$root/ic-infrastructure-health-check"
  cp "$CONTRACT_SRC" "$root/ic-infrastructure-health-check/"
  chmod "$mode" "$root/ic-infrastructure-health-check/openrc-proxy-unit-contract.sh"
}

# WI#703 case 1: the real CLI, real ownership, a health dir symlinked to a
# non-root-owned library. --help must still render, and diagnostics must name
# the resolved library. Removing the checked loader makes this case silent.
check_real_unsafe_library() {
  local root="$WORK/real-linked" lib="$WORK/real-linked-lib" out err status=0
  stage_linked_checkout "$root" "$lib"
  if [[ "$(id -u)" == 0 ]]; then
    chown -R 65534:65534 "$lib"
  fi
  err="$WORK/real-linked.err"
  out="$(bash "$root/ic/scripts/lunarwing-mt-admin.sh" --help 2>"$err")" || status=$?
  assert_eq "real unsafe library: --help exits 0" "0" "$status"
  assert_contains "real unsafe library: help is rendered" "$out" "Usage:"
  local text
  text="$(<"$err")"
  assert_contains "real unsafe library: refusal is reported" "$text" \
    "OpenRC proxy unit contract not loaded"
  assert_contains "real unsafe library: refusal names the resolved library" "$text" "$lib"
  assert_contains "real unsafe library: refusal names a failing component" "$text" \
    "must be root-owned, symlink-free, and not group/other-writable"
  assert_absent "real unsafe library: no shell error" "$text" "command not found"
}

# Source a staged checkout with a stat shim so the ownership shape is
# deterministic on any runner (root or not). The shim reports every path as
# root:0755 except the one component under test.
run_mock_load() {  # <root> <nonroot-path-or-empty> <writable-path-or-empty> <safe-tree-or-empty>
  local root="$1" nonroot="$2" writable="$3" safe_tree="$4" result
  result="$(
    set +e
    MOCK_NONROOT="$nonroot" MOCK_WRITABLE="$writable" MOCK_SAFE_TREE="$safe_tree"
    # shellcheck disable=SC2329  # invoked indirectly by the sourced mt-admin guard
    stat() {
      local flag="${1:-}" fmt="${2:-}" path="${3:-}"
      if [[ "$flag" == -c && "$fmt" == %u ]]; then
        if [[ -n "$MOCK_NONROOT" && "$path" == "$MOCK_NONROOT" ]]; then
          printf '65534\n'
          return 0
        fi
        printf '0\n'
      elif [[ "$flag" == -c && "$fmt" == %a ]]; then
        if [[ -n "$MOCK_WRITABLE" && "$path" == "$MOCK_WRITABLE" ]]; then
          printf '775\n'
          return 0
        fi
        printf '755\n'
      else
        command stat "$@"
      fi
    }
    # shellcheck disable=SC2329  # invoked indirectly by the sourced mt-admin guard
    find() {
      if [[ -n "$MOCK_SAFE_TREE" && "${1:-}" == "$MOCK_SAFE_TREE" ]]; then
        return 0
      fi
      command find "$@"
    }
    source "$root/ic/scripts/lunarwing-mt-admin.sh" 2>"$WORK/mock.err"
    # The composition root enables `set -e`; keep inspecting after refusals.
    set +e
    printf 'REFUSAL=%s\n' "${MT_ADMIN_OPENRC_CONTRACT_REFUSAL:-}"
    require_openrc_proxy_contract >/dev/null 2>&1
    printf 'PREFLIGHT_RC=%s\n' "$?"
    require_safe_openrc_proxy_units >/dev/null 2>&1
    printf 'UNITS_RC=%s\n' "$?"
    ( openrc_proxy_unit_is_safe "$root/ic/scripts/absent-unit" ) 2>&1
    printf 'CHECKER_RC=%s\n' "$?"
  )"
  printf '%s\n' "$result"
}

assert_mock_refusal() {  # <label> <result> <component>
  local label="$1" result="$2" component="$3"
  assert_contains "$label: contract is marked unavailable" "$result" "REFUSAL=refusing health library"
  assert_contains "$label: refusal names the failing component" "$result" "$component"
  assert_contains "$label: OpenRC preflight refuses" "$result" "PREFLIGHT_RC=126"
  assert_contains "$label: unit checks refuse" "$result" "UNITS_RC=125"
  assert_contains "$label: contract entry point refuses" "$result" \
    "OpenRC proxy unit contract unavailable"
}

check_mock_nonroot_library() {
  local root="$WORK/mock-nonroot" lib="$WORK/mock-nonroot-lib" result
  stage_linked_checkout "$root" "$lib"
  result="$(run_mock_load "$root" "$lib" "" "")"
  assert_mock_refusal "mock non-root-owned library" "$result" "$lib"
  assert_absent "mock non-root-owned library: no shell error" "$result" "command not found"
}

check_mock_writable_parent() {
  local root="$WORK/mock-writable" parent="$WORK/mock-writable-parent" lib result
  lib="$parent/library"
  stage_linked_checkout "$root" "$lib"
  result="$(run_mock_load "$root" "" "$parent" "")"
  assert_mock_refusal "mock group-writable parent" "$result" "$parent"
}

check_mock_safe_library() {
  local root="$WORK/mock-safe" lib="$WORK/mock-safe-lib" result
  stage_linked_checkout "$root" "$lib"
  result="$(run_mock_load "$root" "" "" "$lib")"
  assert_absent "mock safe library: no refusal recorded" "$result" "REFUSAL=refusing"
  assert_contains "mock safe library: OpenRC preflight passes" "$result" "PREFLIGHT_RC=0"
  assert_absent "mock safe library: real checker is not stubbed" "$result" \
    "OpenRC proxy unit contract unavailable"
}

# WI#696 ABZ-3 / WI#703: the ordinary checkout layout is sourced as root too.
# A contract file another account can write is refused even though the checkout
# itself is legitimately user-owned.
check_plain_unsafe_contract_file() {
  local root="$WORK/plain-writable" contract out err status=0
  stage_plain_checkout "$root" 0666
  contract="$root/ic-infrastructure-health-check/openrc-proxy-unit-contract.sh"
  err="$WORK/plain-writable.err"
  out="$(bash "$root/ic/scripts/lunarwing-mt-admin.sh" --help 2>"$err")" || status=$?
  assert_eq "group/other-writable contract: --help exits 0" "0" "$status"
  assert_contains "group/other-writable contract: help is rendered" "$out" "Usage:"
  local text
  text="$(<"$err")"
  assert_contains "group/other-writable contract: refusal names the file" "$text" "$contract"
  assert_contains "group/other-writable contract: refusal says go-w" "$text" "chmod go-w"
}

check_plain_safe_checkout() {
  local root="$WORK/plain-safe" contract result stderr_text
  stage_plain_checkout "$root" 0644
  contract="$root/ic-infrastructure-health-check/openrc-proxy-unit-contract.sh"
  result="$(
    set +e
    source "$root/ic/scripts/lunarwing-mt-admin.sh" 2>"$WORK/plain-safe.err"
    printf 'REFUSAL=%s\n' "${MT_ADMIN_OPENRC_CONTRACT_REFUSAL:-}"
    require_openrc_proxy_contract >/dev/null 2>&1
    printf 'PREFLIGHT_RC=%s\n' "$?"
  )"
  stderr_text="$(<"$WORK/plain-safe.err")"
  assert_absent "plain safe checkout: no refusal recorded" "$result" "REFUSAL=refusing"
  assert_contains "plain safe checkout: OpenRC preflight passes" "$result" "PREFLIGHT_RC=0"
  assert_eq "plain safe checkout: no warning on load" "" "$stderr_text"
  assert_eq "plain safe checkout: fixture contract present" "0" \
    "$([[ -f "$contract" ]] && printf 0 || printf 1)"
}

WORK="$(mktemp -d "${TMPDIR:-/tmp}/lunarwing-wi703.XXXXXX")"
check_real_unsafe_library
check_mock_nonroot_library
check_mock_writable_parent
check_mock_safe_library
check_plain_unsafe_contract_file
check_plain_safe_checkout

if ((failures != 0)); then
  exit 1
fi
printf 'RESULT: WI#703 contract-load harness passed\n'
