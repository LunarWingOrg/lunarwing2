#!/usr/bin/env bash
# shellcheck disable=SC1090,SC1091,SC2034,SC2317,SC2329
set -euo pipefail

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"

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

stage_scripts() {
  mkdir -p "$1/ic"
  cp -a "$REPO_ROOT/ic/scripts" "$1/ic/scripts"
}

stage_plain_checkout() {
  local root="$1" mode="${2:-0644}"
  stage_scripts "$root"
  cp -a "$REPO_ROOT/ic-infrastructure-health-check" "$root/ic-infrastructure-health-check"
  chmod "$mode" "$root/ic-infrastructure-health-check/openrc-proxy-unit-contract.sh"
}

stage_linked_checkout() {
  local root="$1" lib="$2"
  stage_scripts "$root"
  mkdir -p "$(dirname -- "$lib")"
  cp -a "$REPO_ROOT/ic-infrastructure-health-check" "$lib"
  ln -s "$lib" "$root/ic-infrastructure-health-check"
}

mock_metadata() {
  stat() {
    local flag="${1:-}" format="${2:-}" path="${3:-}"
    if [[ "$flag" == -c && "$format" == %u ]]; then
      if [[ -n "${MOCK_NONROOT:-}" && "$path" == "$MOCK_NONROOT" ]]; then
        printf '65534\n'
      else
        printf '0\n'
      fi
    elif [[ "$flag" == -c && "$format" == %a ]]; then
      if [[ -n "${MOCK_WRITABLE:-}" && "$path" == "$MOCK_WRITABLE" ]]; then
        printf '775\n'
      else
        printf '755\n'
      fi
    else
      command stat "$@"
    fi
  }
  find() {
    if [[ -n "${MOCK_UNSAFE_ENTRY:-}" && "${1:-}" == "$MOCK_HEALTH_LIB_DIR" ]]; then
      printf '%s\n' "$MOCK_UNSAFE_ENTRY"
      return 0
    fi
    if [[ -n "${MOCK_SAFE_TREE:-}" && "${1:-}" == "$MOCK_SAFE_TREE" ]]; then
      return 0
    fi
    local -a args=()
    while (($#)); do
      if [[ "$1" == ! && "${2:-}" == -user && "${3:-}" == root ]]; then
        args+=('!' '(' -user root -o -user "$MOCK_REAL_USER" ')')
        shift 3
      else
        args+=("$1")
        shift
      fi
    done
    command find "${args[@]}"
  }
  chown() { :; }
}

run_mock_load() {  # <root> <nonroot> <writable> <safe-tree> <unsafe-entry>
  local root="$1" nonroot="$2" writable="$3" safe_tree="$4" unsafe_entry="$5"
  local result removal_output removal_status main_output main_status
  result="$(
    set +e
    MOCK_REAL_USER="$(id -un)"
    MOCK_NONROOT="$nonroot"
    MOCK_WRITABLE="$writable"
    MOCK_SAFE_TREE="$safe_tree"
    MOCK_UNSAFE_ENTRY="$unsafe_entry"
    MOCK_HEALTH_LIB_DIR="$root-lib"
    mock_metadata
    source "$root/ic/scripts/lunarwing-mt-admin.sh" 2>"$root/load.err"
    set +e
    printf 'REFUSAL=%s\n' "${MT_ADMIN_OPENRC_CONTRACT_REFUSAL:-}"
    main --help >/dev/null
    printf 'HELP_RC=%s\n' "$?"
    require_openrc_proxy_contract >/dev/null 2>&1
    printf 'PREFLIGHT_RC=%s\n' "$?"
    require_safe_openrc_proxy_units >/dev/null 2>&1
    printf 'UNITS_RC=%s\n' "$?"
    ( openrc_proxy_unit_is_safe "$root/ic/scripts/absent-unit" ) 2>&1
    printf 'CHECKER_RC=%s\n' "$?"
    if [[ -n "${MT_ADMIN_OPENRC_CONTRACT_REFUSAL:-}" ]]; then
      main_output="$( (INIT_SYSTEM=openrc; main list-tenants) 2>&1 )"
      main_status=$?
      printf 'MAIN_OPENRC_RC=%s\nMAIN_OUTPUT=%s\n' "$main_status" "$main_output"
      removal_output="$( (INIT_SYSTEM=openrc; remove_tenant alpha false) 2>&1 )"
      removal_status=$?
      printf 'REMOVE_RC=%s\nREMOVE_OUTPUT=%s\n' "$removal_status" "$removal_output"
    fi
  )"
  printf '%s\n' "$result"
}

assert_mock_refusal() {
  local label="$1" result="$2" component="$3"
  assert_contains "$label: contract is marked unavailable" "$result" "REFUSAL=refusing health library"
  assert_contains "$label: refusal names the failing component" "$result" "$component"
  assert_contains "$label: help still runs" "$result" "HELP_RC=0"
  assert_contains "$label: OpenRC preflight refuses" "$result" "PREFLIGHT_RC=126"
  assert_contains "$label: unit checks refuse" "$result" "UNITS_RC=125"
  assert_contains "$label: contract entry point refuses" "$result" "OpenRC proxy unit contract unavailable"
  assert_contains "$label: another OpenRC command refuses" "$result" "MAIN_OPENRC_RC=1"
  assert_contains "$label: remove-tenant names the contract refusal" "$result" \
    "OpenRC proxy unit contract unavailable; refusing removal"
  assert_absent "$label: contract refusal is not a migration" "$result" "OpenRC proxy migration required"
  assert_absent "$label: no shell error" "$result" "command not found"
  assert_absent "$label: no in-place re-owning repair" "$result" "chown"
  assert_contains "$label: repair uses a trusted checkout" "$result" "reinstall-health-library <trusted-checkout>"
}

check_mock_nonroot_library() {
  local root="$WORK/mock-nonroot" result
  local lib="$root-lib"
  stage_linked_checkout "$root" "$lib"
  result="$(run_mock_load "$root" "$lib" "" "" "")"
  assert_mock_refusal "mock non-root-owned library" "$result" "$lib"
}

check_mock_writable_parent() {
  local root="$WORK/mock-writable" result
  local parent="$WORK/mock-writable-parent"
  local lib="$parent/library"
  stage_linked_checkout "$root" "$lib"
  result="$(run_mock_load "$root" "" "$parent" "" "")"
  assert_mock_refusal "mock group-writable parent" "$result" "$parent"
}

check_mock_unsafe_entry() {
  local root="$WORK/mock-entry" result
  local lib="$root-lib"
  stage_linked_checkout "$root" "$lib"
  result="$(run_mock_load "$root" "" "" "" "$lib/cron-wrapper.sh")"
  assert_mock_refusal "mock unsafe entry" "$result" "$lib/cron-wrapper.sh"
  assert_contains "unsafe entry uses entry wording" "$result" "REFUSAL=refusing health library entry"
  assert_absent "unsafe entry is not called a parent" "$result" "offending parent"
}

check_mock_safe_library() {
  local root="$WORK/mock-safe" result
  local lib="$root-lib"
  stage_linked_checkout "$root" "$lib"
  result="$(run_mock_load "$root" "" "" "$lib" "")"
  assert_absent "mock safe library: no refusal recorded" "$result" "REFUSAL=refusing"
  assert_contains "mock safe library: help runs" "$result" "HELP_RC=0"
  assert_contains "mock safe library: OpenRC preflight passes" "$result" "PREFLIGHT_RC=0"
  assert_absent "mock safe library: real checker is not stubbed" "$result" \
    "OpenRC proxy unit contract unavailable"
}

check_plain_writable_checkout() {
  local root="$WORK/plain-writable" result stderr_text
  stage_plain_checkout "$root" 0666
  result="$(
    set +e
    source "$root/ic/scripts/lunarwing-mt-admin.sh" 2>"$root/load.err"
    printf 'REFUSAL=%s\n' "${MT_ADMIN_OPENRC_CONTRACT_REFUSAL:-}"
    require_openrc_proxy_contract >/dev/null 2>&1
    printf 'PREFLIGHT_RC=%s\n' "$?"
  )"
  stderr_text="$(<"$root/load.err")"
  assert_absent "ordinary group-writable checkout: no refusal" "$result" "REFUSAL=refusing"
  assert_contains "ordinary group-writable checkout: preflight passes" "$result" "PREFLIGHT_RC=0"
  assert_eq "ordinary group-writable checkout: no warning" "" "$stderr_text"
}

check_openrc_repair() {
  local broken="$WORK/repair-broken" trusted="$WORK/repair-trusted"
  local lib="$broken-lib"
  local result output status marker
  stage_linked_checkout "$broken" "$lib"
  stage_plain_checkout "$trusted"
  printf 'untrusted\n' >"$lib/untrusted-marker"
  marker="$lib/untrusted-marker"
  result="$(
    set +e
    MOCK_REAL_USER="$(id -un)"
    MOCK_NONROOT=""
    MOCK_WRITABLE=""
    MOCK_SAFE_TREE=""
    MOCK_UNSAFE_ENTRY="$marker"
    MOCK_HEALTH_LIB_DIR="$lib"
    mock_metadata
    source "$broken/ic/scripts/lunarwing-mt-admin.sh" 2>"$broken/load.err"
    set +e
    HEALTH_LIB_DIR="$lib"
    HEALTH_BASE_DIR="$WORK/repair-state"
    INIT_SYSTEM=openrc
    require_root() { :; }
    MOCK_UNSAFE_ENTRY=""
    output="$( (main reinstall-health-library "$trusted") 2>&1 )"
    status=$?
    printf 'REPAIR_RC=%s\nREPAIR_OUTPUT=%s\n' "$status" "$output"
  )"
  assert_contains "OpenRC refused state runs the documented repair" "$result" "REPAIR_RC=0"
  assert_contains "repair reports the trusted checkout" "$result" "reinstalled health library from trusted checkout $trusted"
  assert_eq "repair replaces the untrusted tree" "0" "$([[ ! -e "$marker" ]] && printf 0 || printf 1)"
  assert_eq "repair installs the trusted contract" "0" \
    "$(cmp -s "$trusted/ic-infrastructure-health-check/openrc-proxy-unit-contract.sh" "$lib/openrc-proxy-unit-contract.sh" && printf 0 || printf 1)"
  assert_contains "repair starts from a recorded refusal" "$(<"$broken/load.err")" "OpenRC proxy unit contract not loaded"
  assert_absent "repair text never suggests re-owning the refused tree" "$result" "chown"
}

check_unsafe_self_install() {
  local broken="$WORK/self-broken" result output status marker leftover
  local lib="$broken-lib"
  stage_linked_checkout "$broken" "$lib"
  marker="$lib/untrusted-marker"
  printf 'untrusted\n' >"$marker"
  result="$(
    set +e
    MOCK_REAL_USER="$(id -un)"
    MOCK_NONROOT=""
    MOCK_WRITABLE=""
    MOCK_SAFE_TREE=""
    MOCK_UNSAFE_ENTRY="$marker"
    MOCK_HEALTH_LIB_DIR="$lib"
    mock_metadata
    source "$broken/ic/scripts/lunarwing-mt-admin.sh" 2>"$broken/load.err"
    set +e
    HEALTH_LIB_DIR="$lib"
    HEALTH_BASE_DIR="$WORK/self-state"
    INIT_SYSTEM=openrc
    require_root() { :; }
    output="$( (main reinstall-health-library "$broken") 2>&1 )"
    status=$?
    printf 'SELF_INSTALL_RC=%s\nSELF_INSTALL_OUTPUT=%s\n' "$status" "$output"
  )"
  assert_contains "self-install from an unsafe library refuses" "$result" "SELF_INSTALL_RC=1"
  assert_contains "self-install refusal explains the unsafe source" "$result" \
    "refusing to install from the unsafe installed health library"
  assert_eq "self-install leaves the refused tree untouched" "untrusted" "$(<"$marker")"
  leftover="$(command find "$WORK" -maxdepth 1 -name '.lunarwing-health.new.*' -print -quit)"
  assert_eq "self-install leaves no staging directory" "" "$leftover"
}

WORK="$(mktemp -d "${TMPDIR:-/tmp}/lunarwing-wi709.XXXXXX")"
check_mock_nonroot_library
check_mock_writable_parent
check_mock_unsafe_entry
check_mock_safe_library
check_plain_writable_checkout
check_openrc_repair
check_unsafe_self_install

if ((failures != 0)); then
  exit 1
fi
printf 'RESULT: WI#709 contract-load and repair harness passed\n'
