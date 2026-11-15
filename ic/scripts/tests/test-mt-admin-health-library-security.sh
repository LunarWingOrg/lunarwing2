#!/usr/bin/env bash
# shellcheck disable=SC2034  # fixture values are consumed by sourced mt-admin functions
set -euo pipefail

TEST_SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT_DIR="$TEST_SCRIPT_DIR"
MT_ADMIN="$TEST_SCRIPT_DIR/../lunarwing-mt-admin.sh"
# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"

failures=0
MOCK_ROOT=""
REAL_ROOT=""
trap '[[ -z "$MOCK_ROOT$REAL_ROOT" ]] || rm -rf -- "$MOCK_ROOT" "$REAL_ROOT"' EXIT

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

assert_before() {
  local label="$1" first="$2" second="$3"
  if [[ "$first" =~ ^[0-9]+$ && "$second" =~ ^[0-9]+$ ]] && ((first < second)); then
    pass "$label"
  else
    fail "$label (expected '$first' before '$second')"
  fi
}

logger() { printf '%s\n' "$*" >>"${HEALTH_TEST_LOGGER_LOG:?}"; }

configure_health_fixture() {
  local root="$1"
  HEALTH_SRC_DIR="$root/source"
  HEALTH_LIB_DIR="$root/health-lib"
  HEALTH_ENV_FILE="$root/config/health.env"
  HEALTH_BASE_DIR="$root/state"
  HEALTH_LAUNCHER="$root/lunarwing-mt-health"
  HEALTH_TEST_LOGGER_LOG="$root/logger.log"
  PORTS_REGISTRY="$root/ports.json"
  HEALTH_INTERVAL_MIN=15
  HEALTH_INTERVAL_SOURCE="test"
  HEALTH_GOTIFY_URL=""
  HEALTH_GOTIFY_TOKEN=""
  mkdir -p "$HEALTH_SRC_DIR"
  cat >"$HEALTH_SRC_DIR/cron-wrapper.sh" <<'EOF'
#!/bin/bash
printf 'executed\n' >"$HEALTH_TEST_MARKER"
EOF
  chmod 0755 "$HEALTH_SRC_DIR/cron-wrapper.sh"
  : >"$PORTS_REGISTRY"
  : >"$HEALTH_TEST_LOGGER_LOG"
  export HEALTH_TEST_LOGGER_LOG
  ensure_init_system() { INIT_SYSTEM=systemd; }
  ensure_container_runtime() { return 1; }
  _install_health_systemd_timer() { :; }
}

prepare_launcher_for_test() {
  sed -i 's|^HEALTH_LIB_LOGGER=.*$|HEALTH_LIB_LOGGER=logger|' "$HEALTH_LAUNCHER"
  if [[ -n "$MOCK_ROOT" || "${HEALTH_TEST_NAMESPACE:-false}" == true ]]; then
    sed -i 's|^HEALTH_LIB_STAT=.*$|HEALTH_LIB_STAT=stat|; s|^HEALTH_LIB_FIND=.*$|HEALTH_LIB_FIND=find|' \
      "$HEALTH_LAUNCHER"
  fi
  : >"$HEALTH_TEST_LOGGER_LOG"
  export -f logger
}

assert_launcher_refused() {
  local label="$1" output="" status=0
  rm -f "$HEALTH_TEST_MARKER"
  prepare_launcher_for_test
  if output="$(bash "$HEALTH_LAUNCHER" 2>&1)"; then
    fail "$label: launcher succeeded"
  else
    status=$?
    assert_eq "$label: exit status" "126" "$status"
    assert_contains "$label: refusal logged" "$output" "error: refusing health library"
    assert_contains "$label: refusal sent to syslog" "$(<"$HEALTH_TEST_LOGGER_LOG")" \
      "daemon.err -t lunarwing-mt-health -- error: refusing health library"
  fi
  LAST_REFUSAL_OUTPUT="$output"
  assert_eq "$label: wrapper not executed" "0" "$([[ -e "$HEALTH_TEST_MARKER" ]] && printf 1 || printf 0)"
}

assert_launcher_guard_precedes_env() {
  local guard_line env_line
  guard_line="$(grep -n 'if ! health_refusal=' "$HEALTH_LAUNCHER" | cut -d: -f1)"
  env_line="$(grep -n '^set -a$' "$HEALTH_LAUNCHER" | cut -d: -f1)"
  assert_before "launcher checks the library before loading the environment" "$guard_line" "$env_line"
}

# ADO-5: only the launcher pins the inspection binaries. A value inherited
# from root's environment must not survive mt-admin's load.
check_inspection_binaries_not_inherited() {
  local status=0
  (
    HEALTH_LIB_STAT=/bin/false
    HEALTH_LIB_FIND=/bin/false
    export HEALTH_LIB_STAT HEALTH_LIB_FIND
    # shellcheck source=../lunarwing-mt-admin.sh
    source "$MT_ADMIN"
    [[ -z "${HEALTH_LIB_STAT:-}" && -z "${HEALTH_LIB_FIND:-}" ]]
  ) || status=$?
  assert_eq "mt-admin drops an inherited HEALTH_LIB_STAT and HEALTH_LIB_FIND" "0" "$status"
}

assert_root_owned_tree() {
  local label="$1" unsafe
  unsafe="$(find "$HEALTH_LIB_DIR" \( ! -user root -o ! -group root -o -perm /022 \) -print -quit)"
  assert_eq "$label: every entry is root:root and not group/other-writable" "" "$unsafe"
  assert_eq "$label: destination directory owner" "0:0" "$(stat -c '%u:%g' "$HEALTH_LIB_DIR")"
}

check_source_symlink_refused() {
  local label="$1" output="" status=0
  printf 'secret\n' >"$TEST_ROOT/secret"
  chmod 0600 "$TEST_ROOT/secret"
  ln -s "$TEST_ROOT/secret" "$HEALTH_SRC_DIR/escape.sh"
  if output="$(ensure_health_pipeline 2>&1)"; then
    fail "$label: symbolic-link source accepted"
  else
    status=$?
    assert_eq "$label: install refused" "1" "$status"
    assert_contains "$label: refusal names the link" "$output" "contains a symbolic link"
  fi
  assert_eq "$label: symlink target mode unchanged" "600" "$(stat -c '%a' "$TEST_ROOT/secret")"
  assert_eq "$label: previous valid wrapper remains installed" "0" \
    "$([[ -x "$HEALTH_LIB_DIR/cron-wrapper.sh" ]] && printf 0 || printf 1)"
}

# ABZ-2: the optional metrics listener binary lives in the library. A
# reinstall keeps a safe one. An unsafe one is moved to a root-only
# quarantine and the install still completes, so a library from before
# WI#687 is always repaired.
plant_listener_binary() {
  rm -rf -- "$HEALTH_LIB_DIR/lunarwing-health-metrics"
  printf 'listener\n' >"$HEALTH_LIB_DIR/lunarwing-health-metrics"
  chmod 0755 "$HEALTH_LIB_DIR/lunarwing-health-metrics"
}

check_carry_quarantined() {
  local label="$1" reason="$2" output="" status=0 leftover moved
  rm -rf -- "$HEALTH_BASE_DIR/quarantine"
  output="$(ensure_health_pipeline 2>&1)" || status=$?
  assert_eq "$label: install completes" "0" "$status"
  assert_contains "$label: warning names the binary" "$output" \
    "WARNING: health library: $HEALTH_LIB_DIR/lunarwing-health-metrics was not carried"
  assert_contains "$label: warning gives the reason" "$output" "because $reason"
  assert_contains "$label: warning says to stop a running listener" "$output" \
    "Stop the metrics listener now if it is running"
  assert_eq "$label: binary not in the new library" "1" \
    "$([[ -e "$HEALTH_LIB_DIR/lunarwing-health-metrics" || -L "$HEALTH_LIB_DIR/lunarwing-health-metrics" ]] && printf 0 || printf 1)"
  moved="$(command find "$HEALTH_BASE_DIR/quarantine" -mindepth 2 -maxdepth 2 -name lunarwing-health-metrics -print -quit 2>/dev/null || true)"
  assert_eq "$label: binary kept in the quarantine" "0" "$([[ -n "$moved" ]] && printf 0 || printf 1)"
  assert_eq "$label: quarantine is root-only" "700" "$(command stat -c '%a' "$HEALTH_BASE_DIR/quarantine" 2>/dev/null || true)"
  if [[ "$(command id -u)" == 0 ]]; then
    assert_eq "$label: quarantine owned by root" "0" "$(command stat -c '%u' "$HEALTH_BASE_DIR/quarantine" 2>/dev/null || true)"
  fi
  assert_eq "$label: wrapper installed" "0" \
    "$([[ -x "$HEALTH_LIB_DIR/cron-wrapper.sh" ]] && printf 0 || printf 1)"
  assert_contains "$label: checked launcher written" "$(<"$HEALTH_LAUNCHER")" 'if ! health_refusal='
  leftover="$(command find "$(dirname -- "$HEALTH_LIB_DIR")" -maxdepth 1 -name '.lunarwing-health.new.*' -print -quit)"
  assert_eq "$label: no staging directory left" "" "$leftover"
  rm -rf -- "$HEALTH_BASE_DIR/quarantine"
}

# When no root-only quarantine can be had, the unsafe entry is still not
# carried and the install still completes (it is what repairs the library).
check_carry_dropped() {
  local label="$1" output="" status=0 leftover
  plant_listener_binary
  chmod 0757 "$HEALTH_LIB_DIR/lunarwing-health-metrics"
  output="$(ensure_health_pipeline 2>&1)" || status=$?
  assert_eq "$label: install completes" "0" "$status"
  assert_contains "$label: warning says no quarantine could be made" "$output" \
    "and no root-only quarantine could be made under $HEALTH_BASE_DIR/quarantine"
  assert_eq "$label: binary not in the new library" "1" \
    "$([[ -e "$HEALTH_LIB_DIR/lunarwing-health-metrics" || -L "$HEALTH_LIB_DIR/lunarwing-health-metrics" ]] && printf 0 || printf 1)"
  assert_eq "$label: nothing moved into the unsafe quarantine" "" \
    "$(command find -P "$HEALTH_BASE_DIR/quarantine" -mindepth 1 -name 'lunarwing-health-metrics*' -print -quit 2>/dev/null || true)"
  assert_contains "$label: checked launcher written" "$(<"$HEALTH_LAUNCHER")" 'if ! health_refusal='
  leftover="$(command find "$(dirname -- "$HEALTH_LIB_DIR")" -maxdepth 1 -name '.lunarwing-health.new.*' -print -quit)"
  assert_eq "$label: no staging directory left" "" "$leftover"
  rm -rf -- "$HEALTH_BASE_DIR/quarantine"
}

# The carry cases that need no real ownership: modes, links, file types, and
# (through the stat mock) a foreign owner. They run in CI's mocked mode too.
run_carry_cases() {
  plant_listener_binary
  ensure_health_pipeline >/dev/null
  assert_eq "reinstall keeps a safe metrics listener binary" "listener" \
    "$(cat "$HEALTH_LIB_DIR/lunarwing-health-metrics" 2>/dev/null || true)"
  assert_eq "a carried listener binary stays executable" "0" \
    "$([[ -x "$HEALTH_LIB_DIR/lunarwing-health-metrics" ]] && printf 0 || printf 1)"
  plant_listener_binary
  chmod 0757 "$HEALTH_LIB_DIR/lunarwing-health-metrics"
  check_carry_quarantined "other-writable listener binary" "it is group/other-writable"
  plant_listener_binary
  chmod 0775 "$HEALTH_LIB_DIR/lunarwing-health-metrics"
  check_carry_quarantined "group-writable listener binary" "it is group/other-writable"
  plant_listener_binary
  chmod 4755 "$HEALTH_LIB_DIR/lunarwing-health-metrics"
  check_carry_quarantined "set-ID listener binary" "it has a set-ID bit"
  rm -f -- "$HEALTH_LIB_DIR/lunarwing-health-metrics"
  printf 'listener\n' >"$TEST_ROOT/listener.real"
  ln -s "$TEST_ROOT/listener.real" "$HEALTH_LIB_DIR/lunarwing-health-metrics"
  check_carry_quarantined "linked listener binary" "it is a symbolic link"
  mkdir "$HEALTH_LIB_DIR/lunarwing-health-metrics"
  check_carry_quarantined "directory in place of the listener binary" "it is not a regular file"
  plant_listener_binary
  chmod 0777 "$HEALTH_LIB_DIR"
  check_carry_quarantined "safe listener binary in a writable library" \
    "the installed library or a parent directory is not root-owned"
  plant_listener_binary
  printf 'shipped\n' >"$HEALTH_SRC_DIR/lunarwing-health-metrics"
  local output
  output="$(ensure_health_pipeline 2>&1)"
  assert_contains "a copy shipped by the checkout is announced" "$output" \
    "the checkout ships lunarwing-health-metrics"
  assert_eq "a copy shipped by the checkout replaces the installed one" "shipped" \
    "$(cat "$HEALTH_LIB_DIR/lunarwing-health-metrics" 2>/dev/null || true)"
  assert_eq "a copy shipped by the checkout stays executable" "0" \
    "$([[ -x "$HEALTH_LIB_DIR/lunarwing-health-metrics" ]] && printf 0 || printf 1)"
  assert_eq "a copy shipped by the checkout uses the executable mode" "755" \
    "$(stat -c '%a' "$HEALTH_LIB_DIR/lunarwing-health-metrics" 2>/dev/null || true)"
  rm -f -- "$HEALTH_SRC_DIR/lunarwing-health-metrics"
  ensure_health_pipeline >/dev/null
  rm -f -- "$HEALTH_LIB_DIR/lunarwing-health-metrics"
  # A library that is itself a link is replaced, and nothing is taken out of
  # the directory it points at.
  mkdir -p "$TEST_ROOT/elsewhere"
  printf 'elsewhere\n' >"$TEST_ROOT/elsewhere/lunarwing-health-metrics"
  mv -T -- "$HEALTH_LIB_DIR" "$TEST_ROOT/lib.real"
  ln -s "$TEST_ROOT/elsewhere" "$HEALTH_LIB_DIR"
  ensure_health_pipeline >/dev/null
  assert_eq "a linked library is replaced by a directory" "0" \
    "$([[ -d "$HEALTH_LIB_DIR" && ! -L "$HEALTH_LIB_DIR" ]] && printf 0 || printf 1)"
  assert_eq "nothing is moved out of a linked library's target" "elsewhere" \
    "$(cat "$TEST_ROOT/elsewhere/lunarwing-health-metrics" 2>/dev/null || true)"
  rm -rf -- "$TEST_ROOT/lib.real" "$TEST_ROOT/elsewhere" "$HEALTH_BASE_DIR/quarantine"
  rm -rf -- "$HEALTH_BASE_DIR/quarantine"
  : >"$HEALTH_BASE_DIR/quarantine"
  check_carry_dropped "quarantine path is a file"
  mkdir -p "$TEST_ROOT/qelsewhere"
  ln -s "$TEST_ROOT/qelsewhere" "$HEALTH_BASE_DIR/quarantine"
  chmod 0755 "$TEST_ROOT/qelsewhere"
  check_carry_dropped "quarantine path is a link"
  assert_eq "nothing moved through a linked quarantine" "" \
    "$(command find "$TEST_ROOT/qelsewhere" -mindepth 1 -print -quit)"
  assert_eq "a linked quarantine's target keeps its mode" "755" \
    "$(command stat -c '%a' "$TEST_ROOT/qelsewhere")"
  rm -rf -- "$TEST_ROOT/qelsewhere"
  mkdir -p "$HEALTH_BASE_DIR/quarantine"
  chmod 0777 "$HEALTH_BASE_DIR"
  check_carry_dropped "quarantine under a writable parent"
  chmod 0755 "$HEALTH_BASE_DIR"
}

expect_install_refused() {
  local label="$1" reason="$2" output="" status=0
  output="$(ensure_health_pipeline 2>&1)" || status=$?
  assert_eq "$label: install refused" "1" "$status"
  assert_contains "$label: refusal reason" "$output" "$reason"
  assert_eq "$label: previous valid wrapper remains installed" "0" \
    "$([[ -x "$HEALTH_LIB_DIR/cron-wrapper.sh" ]] && printf 0 || printf 1)"
}

check_linked_source_directory_refused() {
  mv -- "$HEALTH_SRC_DIR" "$TEST_ROOT/source.real"
  ln -s "$TEST_ROOT/source.real" "$HEALTH_SRC_DIR"
  expect_install_refused "linked health source directory" \
    "source must be a real directory, not a symbolic link: $HEALTH_SRC_DIR"
  rm -- "$HEALTH_SRC_DIR"
  mv -- "$TEST_ROOT/source.real" "$HEALTH_SRC_DIR"
}

check_fifo_source_refused() {
  mkfifo "$HEALTH_SRC_DIR/not-a-regular-file"
  expect_install_refused "FIFO in health source" "contains a non-regular entry: $HEALTH_SRC_DIR/not-a-regular-file"
  rm -- "$HEALTH_SRC_DIR/not-a-regular-file"
}

check_staging_mode_and_acl_rules() {
  mkdir -p "$HEALTH_SRC_DIR/nested"
  printf 'data\n' >"$HEALTH_SRC_DIR/nested/data"
  chmod 0777 "$HEALTH_SRC_DIR" "$HEALTH_SRC_DIR/nested"
  chmod 4755 "$HEALTH_SRC_DIR/cron-wrapper.sh"
  chmod 2775 "$HEALTH_SRC_DIR/nested"
  chmod 0666 "$HEALTH_SRC_DIR/nested/data"
  local acl_case=false acl_output
  if command -v setfacl >/dev/null 2>&1 && command -v getfacl >/dev/null 2>&1; then
    setfacl -m d:u:65534:rwx "$HEALTH_SRC_DIR/nested"
    acl_case=true
  fi
  # shellcheck disable=SC2329  # invoked indirectly by ensure_health_pipeline
  cp() {
    command cp "$@" || return
    if [[ " $* " == *" --no-preserve=all "* ]]; then
      local destination="${!#}"
      command stat -c '%a' "$destination" "$destination/nested" "$(dirname -- "$destination")" \
        >"$TEST_ROOT/stage.modes"
    fi
  }
  ensure_health_pipeline >/dev/null
  unset -f cp
  local -a stage_modes=()
  mapfile -t stage_modes <"$TEST_ROOT/stage.modes"
  assert_eq "stage tree stays root-only during a world-writable checkout copy" "700" "${stage_modes[0]:-}"
  assert_eq "stage subdirectory stays root-only during the copy" "700" "${stage_modes[1]:-}"
  assert_eq "stage parent stays root-only during the copy" "700" "${stage_modes[2]:-}"
  assert_eq "installed source directory uses the directory mode rule" "755" "$(stat -c '%a' "$HEALTH_LIB_DIR")"
  assert_eq "installed nested directory loses set-ID and write bits" "755" "$(stat -c '%a' "$HEALTH_LIB_DIR/nested")"
  assert_eq "installed top-level shell script loses set-ID and keeps execute" "755" \
    "$(stat -c '%a' "$HEALTH_LIB_DIR/cron-wrapper.sh")"
  assert_eq "installed regular data file uses the file mode rule" "644" \
    "$(stat -c '%a' "$HEALTH_LIB_DIR/nested/data")"
  assert_eq "installed tree has no set-ID entries" "" \
    "$(command find "$HEALTH_LIB_DIR" -perm /6000 -print -quit)"
  if [[ "$acl_case" == true ]]; then
    acl_output="$(getfacl -cp "$HEALTH_LIB_DIR/nested" 2>/dev/null)"
    if [[ "$acl_output" != *"user:65534"* && "$acl_output" != *"default:"* ]]; then
      pass "default ACLs are not inherited from the checkout"
    else
      fail "default ACLs are not inherited from the checkout"
    fi
    setfacl -b -k "$HEALTH_SRC_DIR/nested"
  else
    assert_contains "copy explicitly disables ACL and other metadata preservation" \
      "$(declare -f sync_health_pipeline_library)" '--no-preserve=all --preserve=timestamps'
  fi
  chmod 0755 "$HEALTH_SRC_DIR" "$HEALTH_SRC_DIR/nested" "$HEALTH_SRC_DIR/cron-wrapper.sh"
  chmod 0644 "$HEALTH_SRC_DIR/nested/data"
}

# ADO-2: a directory made under a set-group-ID parent inherits the bit and
# chmod 0755 does not clear it, so the run-time check must accept it: on a
# root-owned directory nobody else can write, the bit changes only the group of
# new entries. A set-ID bit on a regular file changes what runs and stays
# refused.
check_setid_directory_accepted_at_runtime() {
  mkdir "$HEALTH_LIB_DIR/setgid-directory"
  chmod 2755 "$HEALTH_LIB_DIR/setgid-directory"
  rm -f "$HEALTH_TEST_MARKER"
  prepare_launcher_for_test
  if bash "$HEALTH_LAUNCHER" >/dev/null 2>&1; then
    pass "set-group-ID library directory is accepted"
  else
    fail "set-group-ID library directory is accepted"
  fi
  assert_eq "launcher still runs its wrapper under a set-group-ID directory" "executed" \
    "$(cat "$HEALTH_TEST_MARKER" 2>/dev/null || true)"
  rmdir "$HEALTH_LIB_DIR/setgid-directory"
}

check_setid_file_refused_at_runtime() {
  chmod 4755 "$HEALTH_LIB_DIR/cron-wrapper.sh"
  assert_launcher_refused "set-ID library file"
  assert_contains "set-ID file refusal names the cause" "$LAST_REFUSAL_OUTPUT" \
    "set-ID bits on a regular file"
  assert_contains "set-ID file refusal names the entry" "$LAST_REFUSAL_OUTPUT" \
    "$HEALTH_LIB_DIR/cron-wrapper.sh"
  chmod 0755 "$HEALTH_LIB_DIR/cron-wrapper.sh"
}

# ADO-3: the FIFO case above covers the source side; the run-time check must
# also refuse a non-regular entry that sits in the installed library.
check_non_regular_entry_refused_at_runtime() {
  mkfifo "$HEALTH_LIB_DIR/surprise-fifo"
  assert_launcher_refused "non-regular entry in the installed library"
  assert_contains "non-regular refusal names the cause" "$LAST_REFUSAL_OUTPUT" "non-regular file type"
  assert_contains "non-regular refusal names the entry" "$LAST_REFUSAL_OUTPUT" \
    "$HEALTH_LIB_DIR/surprise-fifo"
  rm -f "$HEALTH_LIB_DIR/surprise-fifo"
}

run_metadata_cases() {
  check_linked_source_directory_refused
  check_fifo_source_refused
  check_staging_mode_and_acl_rules
  check_setid_directory_accepted_at_runtime
  check_setid_file_refused_at_runtime
  check_non_regular_entry_refused_at_runtime
}

assert_no_stage_left() {
  local label="$1" leftover
  leftover="$(command find "$(dirname -- "$HEALTH_LIB_DIR")" -maxdepth 1 \
    \( -name '.lunarwing-health.new.*' -o -name '.lunarwing-health.old.*' \) -print -quit)"
  assert_eq "$label: no staging or backup directory left" "" "$leftover"
}

# ADO-3: the last check of the staged tree before publication. A carry that
# leaves an unsafe entry behind (a race, or a future carry rule) must refuse
# the install and leave no stage behind.
check_final_stage_check() {
  local output="" status=0
  output="$(
    # shellcheck disable=SC2329  # override consumed by ensure_health_pipeline
    carry_health_library_entries() {
      mkdir -p "$1/nested"
      : >"$1/nested/unsafe"
      chmod 0777 "$1/nested/unsafe"
    }
    ensure_health_pipeline 2>&1
  )" || status=$?
  assert_eq "an unsafe entry left by the carry refuses the install" "1" "$status"
  assert_contains "final stage refusal names the cause" "$output" \
    "staged health pipeline failed its ownership check"
  assert_contains "final stage refusal names the entry" "$output" "nested/unsafe"
  assert_no_stage_left "final stage refusal"
  assert_eq "final stage refusal keeps the previous wrapper" "0" \
    "$([[ -x "$HEALTH_LIB_DIR/cron-wrapper.sh" ]] && printf 0 || printf 1)"
}

# ADO-3: the second regular-file check after the copy, reached only when the
# source changes between the two checks.
check_post_copy_file_type_check() {
  local output="" status=0
  output="$(
    # shellcheck disable=SC2329  # override consumed by ensure_health_pipeline
    cp() {
      command cp "$@" || return
      if [[ " $* " == *" --no-preserve=all "* ]]; then
        mkfifo "${!#}/injected-fifo"
      fi
    }
    ensure_health_pipeline 2>&1
  )" || status=$?
  assert_eq "a non-regular entry that appears after the source check refuses the install" "1" "$status"
  assert_contains "post-copy refusal names the cause" "$output" "contains a non-regular entry"
  assert_contains "post-copy refusal names the entry" "$output" "injected-fifo"
  assert_no_stage_left "post-copy refusal"
  assert_eq "post-copy refusal keeps the previous wrapper" "0" \
    "$([[ -x "$HEALTH_LIB_DIR/cron-wrapper.sh" ]] && printf 0 || printf 1)"
}

# ADO-3: CI's image has coreutils 9.1, which has no mv --exchange, so CI runs
# only the fallback. Both paths are exercised here: an mv that reports the
# option, and one that does not. The report is what the code decides on, so a
# fake is enough to drive either branch on any host.
fake_mv_exchange() {
  # shellcheck disable=SC2329  # invoked indirectly by ensure_health_pipeline
  mv() {
    if [[ "${1:-}" == --help ]]; then
      printf '      --exchange           exchange source and destination\n'
      return 0
    fi
    if [[ "${1:-}" == --exchange ]]; then
      local source="$3" destination="$4" parking
      parking="$(mktemp -d "$(dirname -- "$source")/.exchange.XXXXXX")" && rmdir -- "$parking" || return 1
      command mv -T -- "$source" "$parking" || return 1
      command mv -T -- "$destination" "$source" || return 1
      command mv -T -- "$parking" "$destination"
      return
    fi
    command mv "$@"
  }
}

fake_mv_without_exchange() {
  # shellcheck disable=SC2329  # invoked indirectly by ensure_health_pipeline
  mv() {
    if [[ "${1:-}" == --help ]]; then
      printf 'Usage: mv [OPTION]... SOURCE... DIRECTORY\n'
      return 0
    fi
    command mv "$@"
  }
}

assert_publication_path() {
  local label="$1" expect_exchange="$2" output="" status=0 fake="$3"
  : >"$HEALTH_LIB_DIR/stale-before-publication"
  output="$(
    "$fake"
    ensure_health_pipeline 2>&1
  )" || status=$?
  assert_eq "$label: install completes" "0" "$status"
  assert_eq "$label: previous library replaced" "0" \
    "$([[ -e "$HEALTH_LIB_DIR/stale-before-publication" ]] && printf 1 || printf 0)"
  assert_eq "$label: new wrapper installed" "0" \
    "$([[ -x "$HEALTH_LIB_DIR/cron-wrapper.sh" ]] && printf 0 || printf 1)"
  assert_no_stage_left "$label"
  if [[ "$expect_exchange" == true ]]; then
    if [[ "$output" == *"rollback-protected replacement"* ]]; then
      fail "$label: took the rollback fallback instead of the exchange"
    else
      pass "$label: used the atomic exchange"
    fi
  else
    assert_contains "$label: announced the rollback fallback" "$output" \
      "rollback-protected replacement"
  fi
}

run_publication_cases() {
  check_final_stage_check
  check_post_copy_file_type_check
  assert_publication_path "exchange publication" true fake_mv_exchange
  assert_publication_path "fallback publication" false fake_mv_without_exchange
}

assert_cli_source_refused() {
  local label="$1" cli="$2" component="$3"
  local stdout="$TEST_ROOT/cli.stdout" stderr="$TEST_ROOT/cli.stderr"
  if bash "$cli" --help >"$stdout" 2>"$stderr"; then
    fail "$label: CLI loaded an unsafe health contract"
  else
    pass "$label: CLI refuses before dispatch"
  fi
  assert_eq "$label: no normal output precedes the refusal" "" "$(<"$stdout")"
  assert_contains "$label: refusal names the failing component" "$(<"$stderr")" "$component"
}

check_snapshot_source_load() {
  local snapshot="$TEST_ROOT/snapshot" cli contract
  cli="$snapshot/ic/scripts/lunarwing-mt-admin.sh"
  contract="$TEST_SCRIPT_DIR/../../../ic-infrastructure-health-check/openrc-proxy-unit-contract.sh"
  mkdir -p "$snapshot/ic/scripts/mt-admin-lib" "$snapshot/ic-infrastructure-health-check"
  cp -- "$MT_ADMIN" "$cli"
  cp -R -- "$TEST_SCRIPT_DIR/../mt-admin-lib/." "$snapshot/ic/scripts/mt-admin-lib/"
  cp -- "$contract" "$snapshot/ic-infrastructure-health-check/openrc-proxy-unit-contract.sh"
  if bash "$cli" --help >"$TEST_ROOT/safe-cli.stdout" 2>"$TEST_ROOT/safe-cli.stderr"; then
    pass "root-owned snapshot loads a safe real-directory contract"
  else
    fail "root-owned snapshot loads a safe real-directory contract"
  fi
  assert_contains "safe snapshot control reaches usage output" "$(<"$TEST_ROOT/safe-cli.stdout")" 'Usage:'

  cp -- "$contract" "$HEALTH_LIB_DIR/openrc-proxy-unit-contract.sh"
  chown 65534:65534 "$HEALTH_LIB_DIR/cron-wrapper.sh"
  rm -rf -- "$snapshot/ic-infrastructure-health-check"
  ln -s "$HEALTH_LIB_DIR" "$snapshot/ic-infrastructure-health-check"
  assert_cli_source_refused "snapshot linked to an unsafe library" "$cli" "$HEALTH_LIB_DIR/cron-wrapper.sh"

  rm -- "$snapshot/ic-infrastructure-health-check"
  mkdir "$snapshot/ic-infrastructure-health-check"
  cp -- "$contract" "$snapshot/ic-infrastructure-health-check/openrc-proxy-unit-contract.sh"
  chown -R 65534:65534 "$snapshot/ic-infrastructure-health-check"
  assert_cli_source_refused "snapshot with an unsafe real-directory sibling" "$cli" \
    "$snapshot/ic-infrastructure-health-check"
}

stat() {
  if [[ "${1:-}" == -c && "${2:-}" == %u && "${3:-}" == "$MOCK_NONROOT_DIR" ]]; then
    printf '65534\n'
  elif [[ "${1:-}" == -c && "${2:-}" == %u ]]; then
    printf '0\n'
  elif [[ "${1:-}" == -c && "${2:-}" == %a && "${3:-}" == /tmp ]]; then
    printf '755\n'
  else
    command stat "$@"
  fi
}

find() {
  if [[ -n "$MOCK_UNSAFE_ENTRY" && "${1:-}" == "$MOCK_HEALTH_LIB_DIR" ]]; then
    printf '%s\n' "$MOCK_UNSAFE_ENTRY"
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

chown() { printf '%s\n' "$*" >>"$MOCK_CHOWN_LOG"; }

run_mock_tests() {
  TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/lunarwing-health-mock.XXXXXX")"
  MOCK_ROOT="$TEST_ROOT"
  MOCK_REAL_USER="$(id -un)"
  MOCK_CHOWN_LOG="$TEST_ROOT/chown.log"
  : >"$MOCK_CHOWN_LOG"
  MOCK_HEALTH_LIB_DIR="$TEST_ROOT/health-lib"
  MOCK_NONROOT_DIR=""
  MOCK_UNSAFE_ENTRY=""
  export MOCK_REAL_USER MOCK_CHOWN_LOG MOCK_HEALTH_LIB_DIR MOCK_NONROOT_DIR MOCK_UNSAFE_ENTRY
  export -f stat find chown
  configure_health_fixture "$TEST_ROOT"
  mkdir -p "$HEALTH_LIB_DIR"
  : >"$HEALTH_LIB_DIR/stale-file"
  ensure_health_pipeline >/dev/null

  assert_contains "fake chown forces root ownership recursively" "$(<"$MOCK_CHOWN_LOG")" "-R -h root:root "
  assert_eq "refresh removes stale entries" "0" "$([[ -e "$HEALTH_LIB_DIR/stale-file" ]] && printf 1 || printf 0)"
  assert_contains "launcher embeds the run-time guard" "$(<"$HEALTH_LAUNCHER")" 'if ! health_refusal='
  assert_contains "launcher pins stat outside the checkout PATH" "$(<"$HEALTH_LAUNCHER")" 'HEALTH_LIB_STAT=/usr/bin/stat'
  assert_contains "launcher pins find outside the checkout PATH" "$(<"$HEALTH_LAUNCHER")" 'HEALTH_LIB_FIND=/usr/bin/find'
  assert_launcher_guard_precedes_env
  check_inspection_binaries_not_inherited
  export HEALTH_TEST_MARKER="$TEST_ROOT/marker"
  prepare_launcher_for_test
  bash "$HEALTH_LAUNCHER"
  assert_eq "safe mock launcher executes its wrapper" "executed" "$(<"$HEALTH_TEST_MARKER")"
  run_metadata_cases
  run_carry_cases
  run_publication_cases
  plant_listener_binary
  MOCK_NONROOT_DIR="$HEALTH_LIB_DIR/lunarwing-health-metrics"
  check_carry_quarantined "mock foreign-owned listener binary" "it is not owned by root"
  plant_listener_binary
  MOCK_NONROOT_DIR="$HEALTH_LIB_DIR"
  check_carry_quarantined "mock listener binary in a foreign-owned library" \
    "the installed library or a parent directory is not root-owned"
  mkdir -p "$HEALTH_BASE_DIR/quarantine"
  MOCK_NONROOT_DIR="$HEALTH_BASE_DIR/quarantine"
  check_carry_dropped "mock foreign-owned quarantine"
  MOCK_NONROOT_DIR=""

  chmod 0777 "$HEALTH_LIB_DIR/cron-wrapper.sh"
  assert_launcher_refused "mock group/other-writable wrapper"
  chmod 0755 "$HEALTH_LIB_DIR/cron-wrapper.sh"
  MOCK_UNSAFE_ENTRY="$HEALTH_LIB_DIR/cron-wrapper.sh"
  assert_launcher_refused "mock non-root-owned wrapper"
  MOCK_UNSAFE_ENTRY=""
  MOCK_NONROOT_DIR="$HEALTH_LIB_DIR"
  assert_launcher_refused "mock non-root-owned directory"
  MOCK_NONROOT_DIR=""
  check_source_symlink_refused "mock chmod symlink attack"
  rm -rf -- "$MOCK_ROOT"
  MOCK_ROOT=""
  unset -f stat find chown
  unset MOCK_REAL_USER MOCK_CHOWN_LOG MOCK_HEALTH_LIB_DIR MOCK_NONROOT_DIR MOCK_UNSAFE_ENTRY
  if ((failures != 0)); then return 1; fi
  printf 'RESULT: mock ownership and refusal harness passed\n'
}

# ADO-2: a set-group-ID parent must not stop the install. A directory made
# under it inherits the bit, and chmod 0755 does not clear it, so the old
# `-perm /6000` test refused the stage, the snapshot and the checkout. The
# stage clears the bit; the check accepts it on a directory.
check_setgid_parent_install() {
  local parent="$TEST_ROOT/setgid-parent" saved_lib="$HEALTH_LIB_DIR"
  # check_source_symlink_refused leaves its decoy link in the source.
  rm -f -- "$HEALTH_SRC_DIR/escape.sh"
  mkdir -p "$parent"
  chown root:root "$parent"
  chmod 2755 "$parent"
  HEALTH_LIB_DIR="$parent/health-lib"
  local output="" status=0
  output="$(ensure_health_pipeline 2>&1)" || status=$?
  assert_eq "install under a set-group-ID parent completes" "0" "$status"
  assert_eq "install under a set-group-ID parent leaves no staging directory" "0" \
    "$([[ -d "$HEALTH_LIB_DIR" ]] && printf 0 || printf 1)"
  assert_eq "the staged library drops the inherited set-group-ID bit" "755" \
    "$(command stat -c '%a' "$HEALTH_LIB_DIR" 2>/dev/null || true)"
  assert_eq "the set-group-ID parent keeps its own mode" "2755" \
    "$(command stat -c '%a' "$parent" 2>/dev/null || true)"
  rm -f "$HEALTH_TEST_MARKER"
  prepare_launcher_for_test
  if bash "$HEALTH_LAUNCHER" >/dev/null 2>&1; then
    pass "the launcher accepts a library published under a set-group-ID parent"
  else
    fail "the launcher accepts a library published under a set-group-ID parent"
  fi
  assert_eq "the launcher runs its wrapper under a set-group-ID parent" "executed" \
    "$(cat "$HEALTH_TEST_MARKER" 2>/dev/null || true)"
  assert_no_stage_left "set-group-ID parent install"
  rm -rf -- "$parent"
  HEALTH_LIB_DIR="$saved_lib"
  ensure_health_pipeline >/dev/null
}

run_real_tests() {
  TEST_ROOT="$(mktemp -d "${HEALTH_TEST_REAL_PARENT:-/tmp}/lunarwing-health-root.XXXXXX")"
  REAL_ROOT="$TEST_ROOT"
  configure_health_fixture "$TEST_ROOT"
  chown -R 65534:65534 "$HEALTH_SRC_DIR"
  mkdir -p "$HEALTH_LIB_DIR"
  : >"$HEALTH_LIB_DIR/stale-file"
  chown -R 65534:65534 "$HEALTH_LIB_DIR"
  chmod 0777 "$HEALTH_LIB_DIR"
  ensure_health_pipeline >/dev/null
  assert_root_owned_tree "non-root checkout install"
  assert_eq "install removes stale entries" "0" "$([[ -e "$HEALTH_LIB_DIR/stale-file" ]] && printf 1 || printf 0)"
  assert_launcher_guard_precedes_env

  export HEALTH_TEST_MARKER="$TEST_ROOT/marker"
  prepare_launcher_for_test
  bash "$HEALTH_LAUNCHER"
  assert_eq "root-owned pipeline still executes" "executed" "$(<"$HEALTH_TEST_MARKER")"
  run_metadata_cases

  plant_listener_binary
  : >"$HEALTH_LIB_DIR/stale-file"
  ensure_health_pipeline >/dev/null
  assert_eq "kept listener binary stays 755 root:root" "755 0:0" \
    "$(stat -c '%a %u:%g' "$HEALTH_LIB_DIR/lunarwing-health-metrics")"
  assert_eq "reinstall still removes other stale entries" "0" \
    "$([[ -e "$HEALTH_LIB_DIR/stale-file" ]] && printf 1 || printf 0)"
  run_carry_cases
  plant_listener_binary
  chown 65534:65534 "$HEALTH_LIB_DIR/lunarwing-health-metrics"
  check_carry_quarantined "foreign-owned listener binary" "it is not owned by root"
  mkdir -p "$HEALTH_BASE_DIR/quarantine"
  chmod 0755 "$HEALTH_BASE_DIR/quarantine"
  chown 65534:65534 "$HEALTH_BASE_DIR/quarantine"
  check_carry_dropped "foreign-owned quarantine"
  # check_carry_dropped removed it; the refusal itself must not have chmodded it.
  mkdir -p "$HEALTH_BASE_DIR/quarantine"
  chmod 0755 "$HEALTH_BASE_DIR/quarantine"
  chown 65534:65534 "$HEALTH_BASE_DIR/quarantine"
  plant_listener_binary
  chmod 0757 "$HEALTH_LIB_DIR/lunarwing-health-metrics"
  ensure_health_pipeline >/dev/null 2>&1
  assert_eq "a foreign-owned quarantine keeps its mode" "755" \
    "$(command stat -c '%a' "$HEALTH_BASE_DIR/quarantine")"
  rm -rf -- "$HEALTH_BASE_DIR/quarantine"
  # The WI#687 upgrade case: a library left owned by the checkout user (old
  # `cp -a` install) holding a root-installed listener binary. The binary
  # cannot be trusted, and the library must still be repaired.
  plant_listener_binary
  chown 65534:65534 "$HEALTH_LIB_DIR"
  chmod 0755 "$HEALTH_LIB_DIR"
  check_carry_quarantined "listener binary in a library from before WI#687" \
    "the installed library or a parent directory is not root-owned"
  assert_root_owned_tree "pre-WI#687 library with a listener binary is repaired"

  chown -R 65534:65534 "$HEALTH_LIB_DIR"
  chmod 0777 "$HEALTH_LIB_DIR"
  ensure_health_pipeline >/dev/null
  assert_root_owned_tree "upgrade re-owns an exposed installation"

  chmod 0777 "$HEALTH_LIB_DIR/cron-wrapper.sh"
  assert_launcher_refused "group/other-writable wrapper"
  chmod 0755 "$HEALTH_LIB_DIR/cron-wrapper.sh"
  chown 65534:65534 "$HEALTH_LIB_DIR/cron-wrapper.sh"
  assert_launcher_refused "non-root-owned wrapper"
  HEALTH_SRC_DIR="$TEST_ROOT/health-contract-link"
  ln -s "$HEALTH_LIB_DIR" "$HEALTH_SRC_DIR"
  local contract_status=0
  require_safe_installed_health_source >/dev/null 2>&1 || contract_status=$?
  assert_eq "OpenRC snapshot source guard refuses a linked unsafe library" "1" "$contract_status"
  rm "$HEALTH_SRC_DIR"
  HEALTH_SRC_DIR="$TEST_ROOT/source"
  chown root:root "$HEALTH_LIB_DIR/cron-wrapper.sh"
  chown 65534:65534 "$HEALTH_LIB_DIR"
  assert_launcher_refused "non-root-owned library directory"
  chown root:root "$HEALTH_LIB_DIR"
  chmod 0777 "$HEALTH_LIB_DIR"
  assert_launcher_refused "group/other-writable library directory"
  chmod 0755 "$HEALTH_LIB_DIR"
  chmod 0777 "$TEST_ROOT"
  assert_launcher_refused "group/other-writable parent"
  assert_contains "parent refusal names the failing component" "$LAST_REFUSAL_OUTPUT" "component $TEST_ROOT "
  assert_contains "syslog refusal names the failing component" "$(<"$HEALTH_TEST_LOGGER_LOG")" "component $TEST_ROOT "
  chmod 0700 "$TEST_ROOT"
  : >"$HEALTH_LIB_DIR/nested-data"
  chown 65534:65534 "$HEALTH_LIB_DIR/nested-data"
  assert_launcher_refused "non-root-owned nested entry"
  chown root:root "$HEALTH_LIB_DIR/nested-data"
  mv "$HEALTH_LIB_DIR/cron-wrapper.sh" "$TEST_ROOT/cron-wrapper.real"
  ln -s "$TEST_ROOT/cron-wrapper.real" "$HEALTH_LIB_DIR/cron-wrapper.sh"
  assert_launcher_refused "symbolic-link wrapper"
  rm "$HEALTH_LIB_DIR/cron-wrapper.sh"
  mv "$TEST_ROOT/cron-wrapper.real" "$HEALTH_LIB_DIR/cron-wrapper.sh"
  check_source_symlink_refused "real chmod symlink attack"
  check_setgid_parent_install
  check_snapshot_source_load
  if ((failures != 0)); then return 1; fi
  printf 'RESULT: real ownership and refusal harness passed\n'
}

if [[ "${1:-}" == --namespace ]]; then
  unset -f stat find chown
  unset MOCK_REAL_USER MOCK_CHOWN_LOG MOCK_HEALTH_LIB_DIR MOCK_NONROOT_DIR MOCK_UNSAFE_ENTRY
  HEALTH_TEST_NAMESPACE=true
  # Host-owned / is the overflow UID in a rootless user namespace. Normalize
  # only that report; installed-tree ownership still uses real chown and find.
  stat() {
    if [[ "${1:-}" == -c && "${2:-}" == %u && "${3:-}" == / ]]; then
      printf '0\n'
    else
      command stat "$@"
    fi
  }
  export -f stat
  mount -t tmpfs tmpfs /tmp
  chmod 0755 /tmp
  run_real_tests
  exit $?
fi

run_mock_tests
if command -v podman >/dev/null 2>&1 && command -v unshare >/dev/null 2>&1 \
  && podman unshare unshare --mount sh -c 'mount -t tmpfs tmpfs /tmp' >/dev/null 2>&1; then
  podman unshare unshare --mount bash "$0" --namespace
elif [[ "$(command id -u)" == 0 ]]; then
  HEALTH_TEST_REAL_PARENT=/root run_real_tests
elif [[ -n "${CI:-}" ]]; then
  printf 'FAIL: real ownership tests require root or a working podman user namespace in CI\n' >&2
  exit 1
else
  printf 'SKIP: real ownership namespace unavailable; fake-chown and launcher refusal cases completed\n'
fi
