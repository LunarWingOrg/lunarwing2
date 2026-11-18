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

# Every staged copy drops group and other write. The copies are made with
# `cp -a`, so they would otherwise carry the modes of the checkout the harness
# runs from, and CI's checkout is group- and other-writable: the real `find`
# then refuses the copy for a reason no case is about.
stage_scripts() {
  mkdir -p "$1/ic"
  cp -a "$REPO_ROOT/ic/scripts" "$1/ic/scripts"
  chmod -R go-w "$1/ic"
}

stage_plain_checkout() {
  local root="$1" mode="${2:-0644}"
  stage_scripts "$root"
  cp -a "$REPO_ROOT/ic-infrastructure-health-check" "$root/ic-infrastructure-health-check"
  chmod -R go-w "$root/ic-infrastructure-health-check"
  chmod "$mode" "$root/ic-infrastructure-health-check/openrc-proxy-unit-contract.sh"
}

stage_linked_checkout() {
  local root="$1" lib="$2"
  stage_scripts "$root"
  mkdir -p "$(dirname -- "$lib")"
  cp -a "$REPO_ROOT/ic-infrastructure-health-check" "$lib"
  chmod -R go-w "$lib"
  ln -s "$lib" "$root/ic-infrastructure-health-check"
  sed -i "s|^HEALTH_LIB_DIR=.*|HEALTH_LIB_DIR=\"$lib\"|" "$root/ic/scripts/lunarwing-mt-admin.sh"
}

mock_metadata() {
  stat() {
    local flag="${1:-}" format="${2:-}" path="${3:-}"
    if [[ "$flag" == -c && "$format" == %u ]]; then
      if [[ -n "${MOCK_NONROOT:-}" && "$path" == "$MOCK_NONROOT" ]] \
        || [[ -n "${MOCK_NONROOT_ADMIN:-}" && "$path" == "$MOCK_NONROOT_ADMIN" ]]; then
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
      elif [[ "$1" == ! && "${2:-}" == -uid && "${3:-}" == 0 ]]; then
        args+=('!' '(' -uid 0 -o -user "$MOCK_REAL_USER" ')')
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

run_mock_load() {  # <root> <nonroot> <writable> <safe-tree> <unsafe-entry> [nonroot-admin] [library]
  local root="$1" nonroot="$2" writable="$3" safe_tree="$4" unsafe_entry="$5"
  local nonroot_admin="${6:-}" library="${7:-$1-lib}"
  local result removal_output removal_status main_output main_status
  local writers_output writers_status
  result="$(
    set +e
    MOCK_REAL_USER="$(id -un)"
    MOCK_NONROOT="$nonroot"
    MOCK_WRITABLE="$writable"
    MOCK_SAFE_TREE="$safe_tree"
    MOCK_UNSAFE_ENTRY="$unsafe_entry"
    MOCK_NONROOT_ADMIN="$nonroot_admin"
    MOCK_HEALTH_LIB_DIR="$library"
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
      writers_output="$( (INIT_SYSTEM=openrc; main writers-active alpha) 2>&1 )"
      writers_status=$?
      printf 'WRITERS_ACTIVE_RC=%s\nWRITERS_ACTIVE_OUTPUT=%s\n' "$writers_status" "$writers_output"
      removal_output="$( (INIT_SYSTEM=openrc; remove_tenant alpha false) 2>&1 )"
      removal_status=$?
      printf 'REMOVE_RC=%s\nREMOVE_OUTPUT=%s\n' "$removal_status" "$removal_output"
    fi
  )"
  printf '%s\n' "$result"
}

assert_mock_refusal() {
  local label="$1" result="$2" component="$3" repair_kind="${4:-library}"
  assert_contains "$label: contract is marked unavailable" "$result" "REFUSAL=refusing health library"
  assert_contains "$label: refusal names the failing component" "$result" "$component"
  assert_contains "$label: help still runs" "$result" "HELP_RC=0"
  assert_contains "$label: OpenRC preflight refuses" "$result" "PREFLIGHT_RC=126"
  assert_contains "$label: unit checks refuse" "$result" "UNITS_RC=125"
  assert_contains "$label: contract entry point refuses" "$result" "OpenRC proxy unit contract unavailable"
  assert_contains "$label: refused contract checker exits nonzero" "$result" $'CHECKER_RC=1\n'
  assert_contains "$label: another OpenRC command refuses" "$result" "MAIN_OPENRC_RC=1"
  assert_contains "$label: writers-active reports unknown" "$result" $'WRITERS_ACTIVE_RC=2\n'
  assert_contains "$label: writers-active explains the refusal" "$result" "writer state unknown"
  assert_contains "$label: remove-tenant names the contract refusal" "$result" \
    "OpenRC proxy unit contract unavailable; refusing removal"
  assert_absent "$label: contract refusal is not a migration" "$result" "OpenRC proxy migration required"
  assert_absent "$label: no shell error" "$result" "command not found"
  if [[ "$repair_kind" == parent ]]; then
    assert_contains "$label: exact parent is named" "$result" "parent directory $component must"
    assert_contains "$label: parent repair is non-recursive and specific" "$result" \
      "chown root:root -- $component && chmod go-w -- $component"
    assert_absent "$label: parent repair is not a reinstall" "$result" "reinstall-health-library"
  else
    assert_absent "$label: no in-place re-owning repair" "$result" "chown"
    assert_contains "$label: repair uses a trusted checkout" "$result" "reinstall-health-library <trusted-checkout>"
  fi
}

check_mock_nonroot_library() {
  local root="$WORK/mock-nonroot" result
  local lib="$root-lib"
  stage_linked_checkout "$root" "$lib"
  printf "printf 'UNTRUSTED_CONTRACT_SOURCED\\n'\n" >>"$lib/openrc-proxy-unit-contract.sh"
  result="$(run_mock_load "$root" "$lib" "" "" "")"
  assert_mock_refusal "mock non-root-owned library" "$result" "$lib"
  assert_absent "refused contract is never sourced" "$result" "UNTRUSTED_CONTRACT_SOURCED"
}

check_mock_writable_parent() {
  local root="$WORK/mock-writable" result
  local parent="$WORK/mock-writable-parent"
  local lib="$parent/library"
  stage_linked_checkout "$root" "$lib"
  result="$(run_mock_load "$root" "" "$parent" "" "")"
  assert_mock_refusal "mock group-writable parent" "$result" "$parent" parent
  assert_absent "parent refusal does not substitute the library path" "$result" "path component $lib"
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

# WI#725: a contract source that is not the installed library is checked when
# mt-admin's own directory is root-owned, and only then.
check_unrelated_linked_source() {
  local root="$WORK/unrelated-link" result
  local lib="$WORK/unrelated-health"
  stage_linked_checkout "$root" "$lib"
  sed -i "s|^HEALTH_LIB_DIR=.*|HEALTH_LIB_DIR=\"$root/installed-health\"|" \
    "$root/ic/scripts/lunarwing-mt-admin.sh"
  printf "printf 'UNTRUSTED_CONTRACT_SOURCED\\n'\n" >>"$lib/openrc-proxy-unit-contract.sh"

  # An ordinary operator checkout: mt-admin's own directory is not root-owned.
  result="$(run_mock_load "$root" "$lib" "" "" "" "$root/ic/scripts")"
  assert_absent "unrelated linked source, operator checkout: no refusal" "$result" "REFUSAL=refusing"
  assert_contains "unrelated linked source, operator checkout: help runs" "$result" "HELP_RC=0"
  assert_contains "unrelated linked source, operator checkout: preflight passes" "$result" "PREFLIGHT_RC=0"
  assert_contains "unrelated linked source, operator checkout: contract is loaded" "$result" \
    "UNTRUSTED_CONTRACT_SOURCED"
  assert_eq "unrelated linked source, operator checkout: no warning" "" "$(<"$root/load.err")"

  # A root-owned mt-admin (a snapshot, a root-owned checkout): the same source
  # is refused, and the text does not offer a reinstall of the library.
  result="$(run_mock_load "$root" "$lib" "" "" "")"
  assert_contains "unrelated linked source, root-owned mt-admin: refused as a source" "$result" \
    "REFUSAL=refusing health contract source $lib"
  assert_contains "unrelated linked source, root-owned mt-admin: help still runs" "$result" "HELP_RC=0"
  assert_contains "unrelated linked source, root-owned mt-admin: OpenRC preflight refuses" "$result" \
    "PREFLIGHT_RC=126"
  assert_absent "unrelated linked source, root-owned mt-admin: never sourced" "$result" \
    "UNTRUSTED_CONTRACT_SOURCED"
  assert_absent "unrelated linked source, root-owned mt-admin: no library reinstall offered" "$result" \
    "reinstall-health-library <trusted-checkout>"
}

# A parent of a contract source belongs to whoever owns that checkout. The
# text for the library's parent prints `chown root:root -- <parent>`; for a
# source it must not, and no reinstall of the library changes a source.
check_source_parent_text() {
  local root="$WORK/source-parent" parent="$WORK/source-parent-home" result
  local lib="$parent/health"
  stage_linked_checkout "$root" "$lib"
  sed -i "s|^HEALTH_LIB_DIR=.*|HEALTH_LIB_DIR=\"$root/installed-health\"|" \
    "$root/ic/scripts/lunarwing-mt-admin.sh"
  result="$(run_mock_load "$root" "" "$parent" "" "")"
  assert_contains "source under an unsafe parent: refused as a source" "$result" \
    "REFUSAL=refusing health contract source $lib: parent directory $parent must"
  assert_contains "source under an unsafe parent: the repair is another checkout" "$result" \
    "run mt-admin from a clean root-owned checkout"
  assert_absent "source under an unsafe parent: no chown is printed" "$result" "chown"
  assert_absent "source under an unsafe parent: no library reinstall offered" "$result" \
    "reinstall-health-library <trusted-checkout>"
}

# The guard checks the tree the source resolves to. The contract must be read
# from that tree too: a link in the given path can be turned between the check
# and the read, and the path as given would then lead to a tree nobody checked.
# The fake readlink is the moment of the check; the link itself already points
# at the other tree.
check_contract_is_read_from_the_checked_tree() {
  local root="$WORK/turned" checked="$WORK/turned-checked" turned="$WORK/turned-other" result staged
  stage_linked_checkout "$root" "$checked"
  cp -a "$checked" "$turned"
  printf "printf 'CHECKED_CONTRACT_SOURCED\\n'\n" >>"$checked/openrc-proxy-unit-contract.sh"
  printf "printf 'TURNED_CONTRACT_SOURCED\\n'\n" >>"$turned/openrc-proxy-unit-contract.sh"
  sed -i "s|^HEALTH_LIB_DIR=.*|HEALTH_LIB_DIR=\"$root/installed-health\"|" \
    "$root/ic/scripts/lunarwing-mt-admin.sh"
  rm "$root/ic-infrastructure-health-check"
  ln -s "$turned" "$root/ic-infrastructure-health-check"
  result="$(
    set +e
    MOCK_REAL_USER="$(id -un)"
    MOCK_NONROOT="" MOCK_WRITABLE="" MOCK_SAFE_TREE="" MOCK_UNSAFE_ENTRY="" MOCK_HEALTH_LIB_DIR=""
    mock_metadata
    # MOCK_ names: a fake runs inside the guard and sees the guard's locals.
    MOCK_TURNED_LINK="$root/ic-infrastructure-health-check"
    MOCK_CHECKED_TREE="$checked"
    readlink() {
      if [[ "$*" == "-f -- $MOCK_TURNED_LINK" ]]; then
        printf '%s\n' "$MOCK_CHECKED_TREE"
      else
        command readlink "$@"
      fi
    }
    source "$root/ic/scripts/lunarwing-mt-admin.sh" 2>"$root/load.err"
    set +e
    printf 'REFUSAL=%s\n' "${MT_ADMIN_OPENRC_CONTRACT_REFUSAL:-}"
    LUNARWING_MT_KNOWLEDGE_SYNC_ADMIN_ROOT="$WORK/turned-sync" \
      install_knowledge_sync_admin_snapshot >/dev/null 2>&1
    printf 'STAGE_RC=%s\n' "$?"
  )"
  assert_absent "turned link: no refusal" "$result" "REFUSAL=refusing"
  assert_contains "turned link: the checked tree's contract is read" "$result" "CHECKED_CONTRACT_SOURCED"
  assert_absent "turned link: the other tree's contract is not read" "$result" "TURNED_CONTRACT_SOURCED"
  # The Study Notes snapshot copies the contract into a root-owned release:
  # the same rule, or the release holds bytes that were never checked.
  staged="$WORK/turned-sync/current/ic-infrastructure-health-check/openrc-proxy-unit-contract.sh"
  assert_contains "turned link: the snapshot is staged" "$result" "STAGE_RC=0"
  assert_eq "turned link: the snapshot holds the checked tree's contract" "checked" \
    "$(grep -o 'CHECKED_CONTRACT_SOURCED\|TURNED_CONTRACT_SOURCED' "$staged" 2>/dev/null \
      | sed 's/_CONTRACT_SOURCED//' | tr 'A-Z\n' 'a-z ' | sed 's/ $//')"
}

# ADU-2 (review of !2477): a symbolic link in the installed library's path must
# not switch the check off. Both shapes, for a root-owned mt-admin and for an
# operator checkout: the source is the installed library either way.
assert_linked_library_refused() {  # <label> <root> <real-library> <text after "refusing health library ">
  local label="$1" root="$2" real="$3" named="$4" result admin
  for admin in "" "$root/ic/scripts"; do
    result="$(run_mock_load "$root" "$real" "" "" "" "$admin" "$real")"
    assert_contains "$label${admin:+, operator checkout}: refused" "$result" \
      "REFUSAL=refusing health library $named"
    assert_contains "$label${admin:+, operator checkout}: the repair is the reinstall" "$result" \
      "reinstall-health-library <trusted-checkout>"
    assert_absent "$label${admin:+, operator checkout}: no chown of the link's target" "$result" "chown"
    assert_contains "$label${admin:+, operator checkout}: help still runs" "$result" "HELP_RC=0"
    assert_contains "$label${admin:+, operator checkout}: OpenRC preflight refuses" "$result" \
      "PREFLIGHT_RC=126"
    assert_absent "$label${admin:+, operator checkout}: never sourced" "$result" \
      "UNTRUSTED_CONTRACT_SOURCED"
  done
}

check_library_path_is_a_link() {
  local root="$WORK/link-lib" real="$WORK/link-lib-real" link="$WORK/link-lib-installed"
  stage_linked_checkout "$root" "$real"
  printf "printf 'UNTRUSTED_CONTRACT_SOURCED\\n'\n" >>"$real/openrc-proxy-unit-contract.sh"
  ln -s "$real" "$link"
  rm "$root/ic-infrastructure-health-check"
  ln -s "$link" "$root/ic-infrastructure-health-check"
  sed -i "s|^HEALTH_LIB_DIR=.*|HEALTH_LIB_DIR=\"$link\"|" "$root/ic/scripts/lunarwing-mt-admin.sh"
  # The link is what the operator installed and what the reinstall replaces,
  # so the refusal names it, and its target only as where it points.
  assert_linked_library_refused "library path is a link to a non-root tree" "$root" "$real" \
    "$link: it is a symbolic link to $real, which is not a safe root-owned tree"
}

# The same link, with the fault in a parent of its target. The parent text
# would print `chown root:root` for a directory the link merely points into
# (an operator's home, in the reviewed case); the repair is still the reinstall.
check_library_link_target_parent() {
  local root="$WORK/link-lib-parent" parent="$WORK/link-lib-parent-home" link="$WORK/link-lib-parent-installed"
  local real="$parent/health" result
  stage_linked_checkout "$root" "$real"
  ln -s "$real" "$link"
  rm "$root/ic-infrastructure-health-check"
  ln -s "$link" "$root/ic-infrastructure-health-check"
  sed -i "s|^HEALTH_LIB_DIR=.*|HEALTH_LIB_DIR=\"$link\"|" "$root/ic/scripts/lunarwing-mt-admin.sh"
  result="$(run_mock_load "$root" "" "$parent" "" "" "" "$real")"
  assert_contains "library link into an unsafe parent: the link is named" "$result" \
    "REFUSAL=refusing health library $link: it is a symbolic link to $real"
  assert_contains "library link into an unsafe parent: the repair is the reinstall" "$result" \
    "reinstall-health-library <trusted-checkout>"
  assert_absent "library link into an unsafe parent: no chown of the target's parent" "$result" "chown"
}

check_library_parent_is_a_link() {
  local root="$WORK/link-parent" real_parent="$WORK/link-parent-real" link_parent="$WORK/link-parent-link"
  local real="$real_parent/library"
  stage_linked_checkout "$root" "$real"
  printf "printf 'UNTRUSTED_CONTRACT_SOURCED\\n'\n" >>"$real/openrc-proxy-unit-contract.sh"
  ln -s "$real_parent" "$link_parent"
  rm "$root/ic-infrastructure-health-check"
  ln -s "$link_parent/library" "$root/ic-infrastructure-health-check"
  sed -i "s|^HEALTH_LIB_DIR=.*|HEALTH_LIB_DIR=\"$link_parent/library\"|" \
    "$root/ic/scripts/lunarwing-mt-admin.sh"
  assert_linked_library_refused "library parent is a link, library is non-root" "$root" "$real" "$real: path component"
}

# ADU-3 (review of !2477): chown and chmod follow a link, so for a parent that
# is a symbolic link the refusal must not print them.
check_link_parent_text() {
  local root="$WORK/link-text" real_parent="$WORK/link-text-real" link_parent="$WORK/link-text-link"
  local result
  stage_plain_checkout "$root"
  mkdir -p "$real_parent/library"
  ln -s "$real_parent" "$link_parent"
  result="$(
    set +e
    MOCK_REAL_USER="$(id -un)"
    MOCK_NONROOT="" MOCK_WRITABLE="" MOCK_SAFE_TREE="" MOCK_UNSAFE_ENTRY="" MOCK_HEALTH_LIB_DIR=""
    mock_metadata
    source "$root/ic/scripts/lunarwing-mt-admin.sh" 2>/dev/null
    set +e
    health_lib_tree_require_safe "$link_parent/library" 2>&1
    printf 'LINK_PARENT_RC=%s\n' "$?"
  )"
  assert_contains "link parent: the tree is refused" "$result" "LINK_PARENT_RC=1"
  assert_contains "link parent: the link is named" "$result" "parent directory $link_parent must"
  assert_contains "link parent: the repair is to replace the link" "$result" \
    "replace it with a real root-owned directory"
  assert_absent "link parent: no chown or chmod is printed" "$result" "repair that parent"
}

# Two texts that must not mislead. An entry's name is chosen by whoever can
# write the tree, so a line break in it must not start a line of its own in
# root's terminal or in syslog. And when the owner cannot be read at all (no
# stat), the refusal must say that, not print a chown for the root directory.
check_refusal_text_details() {
  local root="$WORK/text-details" result lines
  stage_plain_checkout "$root"
  mkdir -p "$WORK/text-tree"
  result="$(
    set +e
    source "$root/ic/scripts/lunarwing-mt-admin.sh" 2>/dev/null
    set +e
    HEALTH_LIB_UNSAFE_COMPONENT="$WORK/text-tree/x"$'\n''error: all checks passed'
    health_lib_refusal_text "$WORK/text-tree" entry
    printf '\nENTRY_TEXT_END\n'
    HEALTH_LIB_STAT="$WORK/no-such-stat" health_lib_tree_require_safe "$WORK/text-tree" 2>&1
    printf 'NO_STAT_RC=%s\n' "$?"
  )"
  lines="$(printf '%s\n' "${result%%ENTRY_TEXT_END*}" | grep -c .)"
  assert_eq "entry name with a line break stays on one line" "1" "$lines"
  assert_contains "unreadable owner: the tree is refused" "$result" "NO_STAT_RC=1"
  assert_contains "unreadable owner: the refusal says what failed" "$result" "could not be read with $WORK/no-such-stat"
  assert_absent "unreadable owner: no chown is printed" "$result" "chown"
}

check_plain_writable_checkout() {
  local root="$WORK/plain-writable" result stderr_text
  stage_plain_checkout "$root" 0666
  # An operator checkout, wherever this harness runs: as root under a
  # root-owned TMPDIR the staged copy would otherwise be a root-owned mt-admin,
  # whose 0666 contract is rightly refused.
  chmod 0775 "$root/ic/scripts"
  result="$(
    set +e
    source "$root/ic/scripts/lunarwing-mt-admin.sh" 2>"$root/load.err"
    set +e
    printf 'REFUSAL=%s\n' "${MT_ADMIN_OPENRC_CONTRACT_REFUSAL:-}"
    require_openrc_proxy_contract >/dev/null 2>&1
    printf 'PREFLIGHT_RC=%s\n' "$?"
  )"
  stderr_text="$(<"$root/load.err")"
  assert_absent "ordinary group-writable checkout: no refusal" "$result" "REFUSAL=refusing"
  assert_contains "ordinary group-writable checkout: preflight passes" "$result" "PREFLIGHT_RC=0"
  assert_eq "ordinary group-writable checkout: no warning" "" "$stderr_text"
}

# enable-health-fleet.sh sources mt-admin and carries its own OpenRC guard. A
# contract that could not be loaded is not a proxy migration: the script must
# not send the operator to render-units, which is refused for the same reason.
# Real metadata, no fakes: a group-writable library is refused for every user.
check_fleet_script_names_the_contract() {
  local root="$WORK/fleet" lib="$WORK/fleet-lib" output status=0
  stage_linked_checkout "$root" "$lib"
  chmod 0775 "$lib"
  mkdir -p "$root/bin" "$root/initd"
  printf '#!/bin/sh\nprintf "0\\n"\n' >"$root/bin/id"
  chmod +x "$root/bin/id"
  # WI#730: simulate an OpenRC host so the override does not contradict the
  # real host's PID-1 markers.
  mkdir -p "$root/openrc-marker"
  output="$(PATH="$root/bin:$PATH" LUNARWING_SERVICE_MANAGER=openrc \
    LUNARWING_MT_OPENRC_MARKER="$root/openrc-marker" \
    LUNARWING_MT_SYSTEMD_MARKER="$root/no-systemd-marker" \
    LUNARWING_MT_OPENRC_INITD_DIR="$root/initd" \
    bash "$root/ic/scripts/enable-health-fleet.sh" --dry-run --yes --allow-down 2>&1)" || status=$?
  assert_eq "fleet script, refused contract on OpenRC: stops" "1" "$status"
  assert_contains "fleet script, refused contract on OpenRC: names the contract" "$output" \
    "the OpenRC proxy unit contract could not be loaded"
  assert_absent "fleet script, refused contract on OpenRC: not called a migration" "$output" \
    "render-units --all-proxies"
}

check_openrc_parent_repair() {
  local broken="$WORK/repair-parent"
  local parent="$WORK/repair-parent-dir"
  local lib="$parent/library"
  local result refusal repair output status mode
  stage_linked_checkout "$broken" "$lib"
  chmod 0775 "$parent"
  result="$(
    set +e
    MOCK_REAL_USER="$(id -un)"
    MOCK_NONROOT=""
    MOCK_WRITABLE="$parent"
    MOCK_SAFE_TREE=""
    MOCK_UNSAFE_ENTRY=""
    MOCK_HEALTH_LIB_DIR="$lib"
    mock_metadata
    source "$broken/ic/scripts/lunarwing-mt-admin.sh" 2>"$broken/load.err"
    refusal="${MT_ADMIN_OPENRC_CONTRACT_REFUSAL:-}"
    printf 'PARENT_REFUSAL=%s\n' "$refusal"
    repair=""
    if [[ "$refusal" == *"repair that parent (not the library tree) with: "* ]]; then
      repair="${refusal#*repair that parent (not the library tree) with: }"
    fi
    printf 'PRINTED_PARENT_REPAIR=%s\n' "$repair"
    if [[ -n "$repair" ]]; then
      output="$(eval "$repair" 2>&1)"
      status=$?
    else
      output="no printed parent repair"
      status=1
    fi
    printf 'PARENT_REPAIR_RC=%s\nPARENT_REPAIR_OUTPUT=%s\n' "$status" "$output"
    MOCK_WRITABLE=""
    MT_ADMIN_OPENRC_CONTRACT_REFUSAL=""
    load_openrc_proxy_unit_contract >/dev/null 2>&1
    printf 'PARENT_RELOAD_RC=%s\nPARENT_RELOAD_REFUSAL=%s\nPARENT_RELOAD_DONE\n' \
      "$?" "${MT_ADMIN_OPENRC_CONTRACT_REFUSAL:-}"
  )"
  assert_contains "parent case names the exact unsafe directory" "$result" "parent directory $parent must"
  assert_contains "parent case prints its own repair" "$result" \
    "PRINTED_PARENT_REPAIR=chown root:root -- $parent && chmod go-w -- $parent"
  assert_contains "printed parent repair runs successfully" "$result" $'PARENT_REPAIR_RC=0\n'
  assert_contains "parent repair permits contract reload" "$result" $'PARENT_RELOAD_RC=0\n'
  assert_contains "parent repair clears the refusal" "$result" $'PARENT_RELOAD_REFUSAL=\n'
  mode="$(command stat -c '%a' "$parent")"
  assert_eq "printed parent repair removes group/other write" "755" "$mode"
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

# A health directory planted inside the unsafe library is the library's bytes
# as well: naming the library as the "trusted checkout" must not publish it.
check_unsafe_library_subtree_install() {
  local broken="$WORK/inside-broken" result output status marker
  local lib="$broken-lib"
  stage_linked_checkout "$broken" "$lib"
  cp -a "$REPO_ROOT/ic-infrastructure-health-check" "$lib/ic-infrastructure-health-check"
  chmod -R go-w "$lib/ic-infrastructure-health-check"
  marker="$lib/ic-infrastructure-health-check/untrusted-marker"
  printf 'untrusted\n' >"$marker"
  result="$(
    set +e
    MOCK_REAL_USER="$(id -un)"
    MOCK_NONROOT="" MOCK_WRITABLE="" MOCK_SAFE_TREE=""
    MOCK_UNSAFE_ENTRY="$marker"
    MOCK_HEALTH_LIB_DIR="$lib"
    MOCK_NONROOT_ADMIN="$broken/ic/scripts"
    mock_metadata
    source "$broken/ic/scripts/lunarwing-mt-admin.sh" 2>"$broken/load.err"
    set +e
    HEALTH_LIB_DIR="$lib"
    HEALTH_BASE_DIR="$WORK/inside-state"
    INIT_SYSTEM=openrc
    require_root() { :; }
    output="$( (main reinstall-health-library "$lib") 2>&1 )"
    status=$?
    printf 'INSIDE_INSTALL_RC=%s\nINSIDE_INSTALL_OUTPUT=%s\n' "$status" "$output"
  )"
  assert_contains "install from inside an unsafe library refuses" "$result" "INSIDE_INSTALL_RC=1"
  assert_contains "install from inside an unsafe library explains the source" "$result" \
    "refusing to install from the unsafe installed health library"
  assert_eq "install from inside an unsafe library publishes nothing" "0" \
    "$([[ ! -e "$lib/untrusted-marker" ]] && printf 0 || printf 1)"
}

WORK="$(mktemp -d "${TMPDIR:-/tmp}/lunarwing-wi722.XXXXXX")"
check_mock_nonroot_library
check_mock_writable_parent
check_mock_unsafe_entry
check_mock_safe_library
check_unrelated_linked_source
check_source_parent_text
check_contract_is_read_from_the_checked_tree
check_library_path_is_a_link
check_library_link_target_parent
check_library_parent_is_a_link
check_link_parent_text
check_refusal_text_details
check_plain_writable_checkout
check_fleet_script_names_the_contract
check_openrc_parent_repair
check_openrc_repair
check_unsafe_self_install
check_unsafe_library_subtree_install

if ((failures != 0)); then
  exit 1
fi
printf 'RESULT: WI#722 contract-load and repair harness passed\n'
