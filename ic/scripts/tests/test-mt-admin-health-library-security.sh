#!/usr/bin/env bash
# shellcheck disable=SC2034  # fixture values are consumed by sourced mt-admin functions
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
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

configure_health_fixture() {
  local root="$1"
  HEALTH_SRC_DIR="$root/source"
  HEALTH_LIB_DIR="$root/health-lib"
  HEALTH_ENV_FILE="$root/config/health.env"
  HEALTH_BASE_DIR="$root/state"
  HEALTH_LAUNCHER="$root/lunarwing-mt-health"
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
  ensure_init_system() { INIT_SYSTEM=systemd; }
  ensure_container_runtime() { return 1; }
  _install_health_systemd_timer() { :; }
}

assert_launcher_refused() {
  local label="$1" output="" status=0
  rm -f "$HEALTH_TEST_MARKER"
  if output="$("$HEALTH_LAUNCHER" 2>&1)"; then
    fail "$label: launcher succeeded"
  else
    status=$?
    assert_eq "$label: exit status" "126" "$status"
    assert_contains "$label: refusal logged" "$output" "error: refusing health library"
  fi
  assert_eq "$label: wrapper not executed" "0" "$([[ -e "$HEALTH_TEST_MARKER" ]] && printf 1 || printf 0)"
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
    else
      args+=("$1")
      shift
    fi
  done
  command find "${args[@]}"
}

chown() { printf '%s\n' "$*" >>"$MOCK_CHOWN_LOG"; }

run_mock_tests() {
  TEST_ROOT="$(mktemp -d /tmp/lunarwing-health-mock.XXXXXX)"
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
  assert_contains "launcher embeds the run-time guard" "$(<"$HEALTH_LAUNCHER")" 'if ! health_lib_tree_require_safe '
  export HEALTH_TEST_MARKER="$TEST_ROOT/marker"
  "$HEALTH_LAUNCHER"
  assert_eq "safe mock launcher executes its wrapper" "executed" "$(<"$HEALTH_TEST_MARKER")"

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

run_real_tests() {
  TEST_ROOT="$(mktemp -d /tmp/lunarwing-health-root.XXXXXX)"
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

  export HEALTH_TEST_MARKER="$TEST_ROOT/marker"
  "$HEALTH_LAUNCHER"
  assert_eq "root-owned pipeline still executes" "executed" "$(<"$HEALTH_TEST_MARKER")"
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
  if ((failures != 0)); then return 1; fi
  printf 'RESULT: real user-namespace ownership and refusal harness passed\n'
}

if [[ "${1:-}" == --namespace ]]; then
  unset -f stat find chown
  unset MOCK_REAL_USER MOCK_CHOWN_LOG MOCK_HEALTH_LIB_DIR MOCK_NONROOT_DIR MOCK_UNSAFE_ENTRY
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
else
  printf 'SKIP: real ownership namespace unavailable; fake-chown and launcher refusal cases completed\n'
fi
