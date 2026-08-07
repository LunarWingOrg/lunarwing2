#!/usr/bin/env bash
set -euo pipefail

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
MT="$TEST_DIR/../lunarwing-mt-admin.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# shellcheck source=../lunarwing-mt-admin.sh
source "$MT"

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
assert_file() { [[ -e "$1" || -L "$1" ]] || fail "expected path: $1"; }
assert_absent() { [[ ! -e "$1" && ! -L "$1" ]] || fail "unexpected path: $1"; }
assert_log() { grep -Fqx -- "$2" "$1" || fail "missing log line '$2'"; }
sudo() {
  if [[ "${1:-}" == -u ]]; then shift 2; fi
  [[ "${1:-}" != -- ]] || shift
  command "$@"
}

test_systemd_present_absent_idempotent() {
  local root="$TMP/systemd" unit_dir="$TMP/systemd/home/.config/systemd/user"
  local unit="weechat-test.service" log
  log="$root/calls"
  mkdir -p "$unit_dir/default.target.wants" "$unit_dir/other.target.requires"
  : >"$unit_dir/$unit"
  ln -s "../$unit" "$unit_dir/default.target.wants/$unit"
  ln -s "../$unit" "$unit_dir/other.target.requires/$unit"
  : >"$unit_dir/weechat-test-extra.service"
  tenant_home() { printf '%s\n' "$TMP/systemd/home"; }
  _systemctl_user() {
    printf '%s\n' "$*" >>"$log"
    if [[ "${2:-}" == show ]]; then
      if [[ -e "$unit_dir/$unit" ]]; then printf 'loaded\n'; else printf 'not-found\n'; fi
    fi
  }

  cleanup_legacy_weechat_systemd test
  assert_absent "$unit_dir/$unit"
  assert_absent "$unit_dir/default.target.wants/$unit"
  assert_absent "$unit_dir/other.target.requires/$unit"
  assert_file "$unit_dir/weechat-test-extra.service"
  assert_log "$log" "test disable --now $unit"
  assert_log "$log" "test daemon-reload"
  : >"$log"
  cleanup_legacy_weechat_systemd test >/dev/null
  assert_log "$log" "test show --value -p LoadState $unit"
  [[ "$(wc -l <"$log")" == 1 ]] || fail "absent systemd cleanup was not idempotent"

  _systemctl_user() {
    printf '%s\n' "$*" >>"$log"
    [[ "${2:-}" != show ]] || printf 'loaded\n'
  }
  : >"$log"
  cleanup_legacy_weechat_systemd test >/dev/null
  assert_log "$log" "test disable --now $unit"
  assert_log "$log" "test daemon-reload"
}

test_systemd_failures_and_path_safety() {
  local root="$TMP/systemd-fail" unit_dir
  unit_dir="$root/home/.config/systemd/user"
  mkdir -p "$unit_dir/default.target.wants"
  : >"$unit_dir/weechat-test.service"
  tenant_home() { printf '%s\n' "$TMP/systemd-fail/home"; }
  _systemctl_user() { [[ "$2" != disable ]]; }
  if (cleanup_legacy_weechat_systemd test) >/dev/null 2>&1; then
    fail "systemd disable failure was ignored"
  fi
  assert_file "$unit_dir/weechat-test.service"

  rm "$unit_dir/weechat-test.service"
  : >"$unit_dir/default.target.wants/weechat-test.service"
  _systemctl_user() { return 0; }
  if (cleanup_legacy_weechat_systemd test) >/dev/null 2>&1; then
    fail "non-symlink systemd enablement was removed"
  fi
  assert_file "$unit_dir/default.target.wants/weechat-test.service"
}

test_openrc_present_absent_idempotent() {
  local root="$TMP/openrc" service="weechat-test" log
  log="$root/calls"
  OPENRC_INITD_DIR="$root/init.d"
  OPENRC_CONFD_DIR="$root/conf.d"
  OPENRC_RUNLEVELS_DIR="$root/runlevels"
  mkdir -p "$OPENRC_INITD_DIR" "$OPENRC_CONFD_DIR" \
    "$OPENRC_RUNLEVELS_DIR/default" "$OPENRC_RUNLEVELS_DIR/custom"
  : >"$OPENRC_INITD_DIR/$service"
  : >"$OPENRC_CONFD_DIR/$service"
  : >"$OPENRC_INITD_DIR/${service}-extra"
  ln -s "$OPENRC_INITD_DIR/$service" "$OPENRC_RUNLEVELS_DIR/default/$service"
  ln -s "$OPENRC_INITD_DIR/$service" "$OPENRC_RUNLEVELS_DIR/custom/$service"
  rc-service() { printf 'service %s\n' "$*" >>"$log"; }
  rc-update() {
    printf 'update %s\n' "$*" >>"$log"
    rm -- "$OPENRC_RUNLEVELS_DIR/$3/$2"
  }

  cleanup_legacy_weechat_openrc test
  assert_absent "$OPENRC_INITD_DIR/$service"
  assert_absent "$OPENRC_CONFD_DIR/$service"
  assert_absent "$OPENRC_RUNLEVELS_DIR/default/$service"
  assert_absent "$OPENRC_RUNLEVELS_DIR/custom/$service"
  assert_file "$OPENRC_INITD_DIR/${service}-extra"
  assert_log "$log" "service $service stop"
  assert_log "$log" "update del $service default"
  assert_log "$log" "update del $service custom"
  local before
  before="$(wc -l <"$log")"
  cleanup_legacy_weechat_openrc test >/dev/null
  [[ "$(wc -l <"$log")" == "$before" ]] || fail "absent OpenRC cleanup was not idempotent"
}

test_openrc_failures_and_path_safety() {
  local root="$TMP/openrc-fail" service="weechat-test"
  OPENRC_INITD_DIR="$root/init.d"
  OPENRC_CONFD_DIR="$root/conf.d"
  OPENRC_RUNLEVELS_DIR="$root/runlevels"
  mkdir -p "$OPENRC_INITD_DIR" "$OPENRC_CONFD_DIR" "$OPENRC_RUNLEVELS_DIR/default"
  : >"$OPENRC_INITD_DIR/$service"
  rc-service() { return 17; }
  rc-update() { return 0; }
  if (cleanup_legacy_weechat_openrc test) >/dev/null 2>&1; then
    fail "OpenRC stop failure was ignored"
  fi
  assert_file "$OPENRC_INITD_DIR/$service"

  rm "$OPENRC_INITD_DIR/$service"
  : >"$OPENRC_RUNLEVELS_DIR/default/$service"
  rc-service() { return 0; }
  if (cleanup_legacy_weechat_openrc test) >/dev/null 2>&1; then
    fail "non-symlink OpenRC registration was removed"
  fi
  assert_file "$OPENRC_RUNLEVELS_DIR/default/$service"

  rm "$OPENRC_RUNLEVELS_DIR/default/$service"
  ln -s "$OPENRC_INITD_DIR/$service" "$OPENRC_RUNLEVELS_DIR/default/$service"
  rc-update() { return 23; }
  if (cleanup_legacy_weechat_openrc test) >/dev/null 2>&1; then
    fail "OpenRC rc-update failure was ignored"
  fi
  assert_file "$OPENRC_RUNLEVELS_DIR/default/$service"
}

test_dispatch_lock_and_wrappers() {
  local called=false wrapper cleanup_line start_line stop_line build_line
  require_root() { :; }
  with_tenant_lifecycle_lock() {
    local name="$1"
    shift
    LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD=9
    LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_TENANT="$name"
    export LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_TENANT
    "$@"
    unset LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_TENANT
  }
  cleanup_legacy_weechat() {
    [[ "${LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD:-}" == 9 \
      && "${LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_TENANT:-}" == test ]] \
      || fail "cleanup did not inherit active lifecycle lock"
    called=true
  }
  main cleanup-legacy-weechat test
  [[ "$called" == true ]] || fail "cleanup dispatcher did not run"

  for wrapper in upgrade-tenant.sh upgrade-tenant-version.sh upgrade-tenant-kageho.sh; do
    wrapper="$TEST_DIR/../$wrapper"
    grep -Fq 'cleanup-legacy-weechat "$TENANT"' "$wrapper" \
      || fail "wrapper missing mt-admin cleanup: $wrapper"
    if grep -Eq 'systemctl --user disable --now "weechat-|rc-(service|update).*weechat-' "$wrapper"; then
      fail "wrapper contains direct legacy init cleanup: $wrapper"
    fi
    cleanup_line="$(grep -n 'cleanup-legacy-weechat "\$TENANT"' "$wrapper" | cut -d: -f1 | tail -n1)"
    start_line="$(grep -n 'start-tenant "\$TENANT"' "$wrapper" | cut -d: -f1 | tail -n1)"
    stop_line="$(grep -n 'stop-tenant\(-for-upgrade\)\? "\$TENANT"' "$wrapper" | cut -d: -f1 | tail -n1)"
    build_line="$(grep -n 'build-tenant "\$TENANT"' "$wrapper" | cut -d: -f1 | head -n1)"
    [[ "$stop_line" -lt "$cleanup_line" && "$cleanup_line" -lt "$build_line" ]] \
      || fail "cleanup must immediately follow stop phase: $wrapper"
    [[ "$cleanup_line" -lt "$start_line" ]] || fail "cleanup must precede start: $wrapper"
    grep -Fq 'ensure-darkirc-release "$TENANT"' "$wrapper" \
      || fail "wrapper does not ensure atomic DarkIRC release: $wrapper"
    if [[ "${wrapper##*/}" == upgrade-tenant.sh ]]; then
      grep -Fq 'run mt "${add_args[@]}"' "$wrapper" \
        || fail "generic wrapper does not re-provision rendered units: $wrapper"
    else
      grep -Fq 'render-units "$TENANT"' "$wrapper" \
        || fail "wrapper does not render selector-backed units before start: $wrapper"
    fi
  done
  grep -Fq 'run mt cleanup-legacy-weechat "$TENANT"' "$TEST_DIR/../upgrade-tenant.sh" \
    || fail "upgrade-tenant dry-run does not route cleanup through run helper"
  if grep -Eq '(^|[;&|[:space:]])(systemctl|rc-service)([[:space:]]|$)' \
      "$TEST_DIR/../upgrade-tenant-version.sh"; then
    fail "upgrade wrapper still performs init lifecycle directly"
  fi
}

test_adapter_restart_parity() {
  local log="$TMP/adapter-calls"
  tenant_exists_in_registry() { return 0; }
  ensure_init_system() { :; }
  _systemctl_user() { printf 'systemd %s\n' "$*" >>"$log"; }
  rc-service() { printf 'openrc %s\n' "$*" >>"$log"; }

  INIT_SYSTEM=systemd
  export INIT_SYSTEM
  restart_weechat_adapter test
  INIT_SYSTEM=openrc
  restart_weechat_adapter test
  assert_log "$log" "systemd test restart lunarwing-weechat-adapter-test.service"
  assert_log "$log" "openrc lunarwing-weechat-adapter-test restart"
}

test_systemd_present_absent_idempotent
test_systemd_failures_and_path_safety
test_openrc_present_absent_idempotent
test_openrc_failures_and_path_safety
test_dispatch_lock_and_wrappers
test_adapter_restart_parity
printf 'PASS: legacy WeeChat cleanup\n'
