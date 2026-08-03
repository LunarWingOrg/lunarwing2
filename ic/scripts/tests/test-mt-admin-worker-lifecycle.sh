#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"

failures=0
CALLS=()

fail() {
  printf 'FAIL: %s\n' "$1"
  failures=$((failures + 1))
}

assert_call() {
  local expected="$1"
  local call
  for call in "${CALLS[@]}"; do
    [[ "$call" == "$expected" ]] && { printf 'PASS: %s\n' "$expected"; return; }
  done
  fail "missing call: $expected"
}

assert_no_call() {
  local unexpected="$1" call
  for call in "${CALLS[@]}"; do
    [[ "$call" != "$unexpected" ]] || { fail "unexpected call: $unexpected"; return; }
  done
  printf 'PASS: no %s\n' "$unexpected"
}

assert_before() {
  local first="$1" second="$2" first_index=-1 second_index=-1 index
  for index in "${!CALLS[@]}"; do
    [[ "${CALLS[$index]}" == "$first" ]] && first_index="$index"
    [[ "${CALLS[$index]}" == "$second" ]] && second_index="$index"
  done
  if [[ "$first_index" -ge 0 && "$second_index" -gt "$first_index" ]]; then
    printf 'PASS: %s before %s\n' "$first" "$second"
  else
    fail "expected '$first' before '$second'"
  fi
}

sanitize_name() { printf '%s' "$1"; }
tenant_exists_in_registry() { return 0; }
tenant_worker_enabled() { return 0; }
ensure_container_runtime() {
  # Globals consumed by sourced lifecycle helpers.
  # shellcheck disable=SC2034
  CONTAINER_RT="podman"
  MT_ROOTLESS="${MT_ROOTLESS:-true}"
}
ensure_init_system() { :; }
podman_supports_quadlet() { return 0; }
say() { :; }
_systemctl_user() { CALLS+=("systemctl:$*"); }
rc-service() { CALLS+=("rc:$*"); }
_ctr() {
  CALLS+=("ctr:$*")
  return 0
}
start_tenant_opencode() { CALLS+=("start-opencode:$1"); }
start_tenant_kimi() { CALLS+=("start-kimi:$1"); }

INIT_SYSTEM="systemd"
CALLS=()
stop_worker alice opencode
assert_call "systemctl:alice stop lunarwing-opencode-alice.service"

CALLS=()
kill_worker alice opencode
assert_call "systemctl:alice stop --no-block lunarwing-opencode-alice.service"
assert_call "ctr:alice stop --time 0 lunarwing-opencode-alice"
assert_call "systemctl:alice stop lunarwing-opencode-alice.service"
assert_before \
  "systemctl:alice stop --no-block lunarwing-opencode-alice.service" \
  "ctr:alice stop --time 0 lunarwing-opencode-alice"
assert_before \
  "ctr:alice stop --time 0 lunarwing-opencode-alice" \
  "systemctl:alice stop lunarwing-opencode-alice.service"

# Override consumed by sourced stop_tenant dispatcher.
# shellcheck disable=SC2034
INIT_SYSTEM="openrc"
CALLS=()
stop_worker alice opencode
assert_call "rc:lunarwing-opencode-alice-sup stop"
assert_call "ctr:alice stop lunarwing-opencode-alice"
assert_before "rc:lunarwing-opencode-alice-sup stop" "ctr:alice stop lunarwing-opencode-alice"

CALLS=()
kill_worker alice opencode
assert_call "rc:lunarwing-opencode-alice-sup stop"
assert_call "ctr:alice stop --time 0 lunarwing-opencode-alice"
assert_before \
  "rc:lunarwing-opencode-alice-sup stop" \
  "ctr:alice stop --time 0 lunarwing-opencode-alice"

CALLS=()
start_worker alice opencode
assert_call "start-opencode:alice"

CALLS=()
start_worker alice kimi
assert_call "start-kimi:alice"

CALLS=()
stop_worker alice kimi
assert_call "rc:lunarwing-kimi-alice-sup stop"
assert_call "ctr:alice stop lunarwing-kimi-alice"

INIT_SYSTEM="systemd"
MT_ROOTLESS="false"
CALLS=()
stop_worker alice opencode
assert_call "ctr:alice stop lunarwing-opencode-alice"

if (stop_worker alice arbitrary >/dev/null 2>&1); then
  fail "unknown worker accepted"
else
  printf 'PASS: unknown worker rejected\n'
fi

if (stop_worker alice nanocode >/dev/null 2>&1); then
  fail "retired Nanocode worker accepted"
else
  printf 'PASS: retired Nanocode worker rejected\n'
fi

if (render_worker_quadlet alice nanocode 8443 >/dev/null 2>&1); then
  fail "retired Nanocode Quadlet accepted"
else
  printf 'PASS: retired Nanocode Quadlet rejected\n'
fi

# ── Legacy Nanocode lifecycle ────────────────────────────────────────────────

fixture="$(mktemp -d)"
trap 'command rm -rf "$fixture"' EXIT
tenant_home() { printf '%s/home/%s' "$fixture" "$1"; }
tenant_quadlet_dir() { printf '%s/quadlets' "$fixture"; }
id() {
  if [[ "${1:-}" == "-u" ]]; then printf '1000\n'; return 0; fi
  return 1
}
getent() {
  [[ "${1:-}" == "passwd" ]] || return 1
  printf 'alice:x:1000:1000::%s/home/alice:/bin/bash\n' "$fixture"
}
command() {
  if [[ "${1:-}" == "-v" && ( "${2:-}" == "docker" || "${2:-}" == "podman" ) ]]; then
    return 0
  fi
  builtin command "$@"
}

_stop_nanocode_container_store() {
  local label="$1" container="$2"
  shift 2
  CALLS+=("store:$label:$container:$*")
}

CALLS=()
_stop_nanocode_containers alice
assert_call "store:root Docker store:lunarwing-nanocode-alice:docker"
assert_call "store:root Podman store:lunarwing-nanocode-alice:podman"
assert_call "store:tenant alice rootless Podman store:lunarwing-nanocode-alice:sudo -u alice env HOME=$fixture/home/alice XDG_RUNTIME_DIR=/run/user/1000 podman"

mkdir -p "$fixture/quadlets" "$fixture/home/alice/.config/systemd/user"
touch "$fixture/quadlets/lunarwing-nanocode-alice.container"
_systemctl_user() {
  CALLS+=("systemctl:$*")
  case "${2:-}" in
    show) printf 'loaded\n' ;;
    is-active) return 1 ;;
  esac
}
INIT_SYSTEM="systemd"
CALLS=()
_stop_nanocode_supervision alice
assert_call "systemctl:alice stop lunarwing-nanocode-alice.service"

OPENRC_INITD_DIR="$fixture/init.d"
OPENRC_CONFD_DIR="$fixture/conf.d"
OPENRC_RUNLEVELS_DIR="$fixture/runlevels"
mkdir -p "$OPENRC_INITD_DIR" "$OPENRC_CONFD_DIR" "$OPENRC_RUNLEVELS_DIR/default"
touch "$OPENRC_INITD_DIR/lunarwing-nanocode-alice" \
  "$OPENRC_INITD_DIR/lunarwing-nanocode-alice-sup"
INIT_SYSTEM="openrc"
CALLS=()
_stop_nanocode_supervision alice
assert_call "rc:lunarwing-nanocode-alice-sup stop"
assert_call "rc:lunarwing-nanocode-alice stop"

INIT_SYSTEM="systemd"
CALLS=()
uninstall_tenant_systemd alice
assert_no_call "ctr:alice rm -f lunarwing-nanocode-alice"
assert_call "ctr:alice rm -f lunarwing-pebble-alice"
assert_call "ctr:alice rm -f lunarwing-opencode-alice"
assert_call "ctr:alice rm -f lunarwing-kimi-alice"

stop_tenant_systemd() { CALLS+=("stop-systemd:$1"); }
stop_tenant_openrc() { CALLS+=("stop-openrc:$1"); }
stop_tenant_vision() { CALLS+=("stop-vision:$1"); }
stop_tenant_pebble() { CALLS+=("stop-pebble:$1"); }
stop_tenant_opencode() { CALLS+=("stop-opencode:$1"); }
stop_tenant_kimi() { CALLS+=("stop-kimi:$1"); }
stop_tenant_postgres() { CALLS+=("stop-postgres:$1"); }
RETIRE_FAILURE="false"
retire_tenant_nanocode() {
  CALLS+=("retire-nanocode:$1")
  [[ "$RETIRE_FAILURE" != "true" ]]
}
die() { return 1; }

INIT_SYSTEM="systemd"
CALLS=()
stop_tenant alice
assert_before "stop-systemd:alice" "retire-nanocode:alice"
assert_before "stop-vision:alice" "retire-nanocode:alice"
assert_before "stop-pebble:alice" "retire-nanocode:alice"
assert_before "stop-opencode:alice" "retire-nanocode:alice"
assert_before "stop-kimi:alice" "retire-nanocode:alice"
assert_before "stop-postgres:alice" "retire-nanocode:alice"

# Override consumed by sourced stop_tenant dispatcher.
# shellcheck disable=SC2034
INIT_SYSTEM="openrc"
RETIRE_FAILURE="true"
CALLS=()
if stop_tenant alice >/dev/null 2>&1; then
  fail "stop succeeded despite Nanocode retirement failure"
else
  printf 'PASS: retirement failure reported after shutdown\n'
fi
assert_before "stop-openrc:alice" "retire-nanocode:alice"
assert_before "stop-postgres:alice" "retire-nanocode:alice"

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all worker lifecycle tests passed\n'
