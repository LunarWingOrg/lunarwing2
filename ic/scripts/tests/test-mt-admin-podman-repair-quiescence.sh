#!/usr/bin/env bash
# Exercise real repair -> native-init stop -> writer probes with podman broken
# until migration. Snapshot/export checks must never inherit a repair bypass.
# shellcheck disable=SC2034  # Settings consumed by the sourced mt-admin modules.
set -euo pipefail
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
TENANT=acorn
CONTAINER_RT=podman
MT_ROOTLESS=true
LUNARWING_MT_QUIESCE_DEADLINE=0
OPENRC_INITD_DIR="$fixture/init.d"
mkdir -p "$OPENRC_INITD_DIR"
for service in lunarwing-acorn xmpp-bridge-acorn lunarwing-pg-acorn \
  lunarwing-pebble-acorn lunarwing-pebble-acorn-sup lunarwing-kimi-acorn-sup; do
  touch "$OPENRC_INITD_DIR/$service"
  chmod +x "$OPENRC_INITD_DIR/$service"
done

tenant_exists_in_registry() { [[ "$1" == "$TENANT" ]]; }
ensure_init_system() { :; }
ensure_container_runtime() { :; }
detect_container_runtime() { printf 'podman\n'; }
with_tenant_lifecycle_lock() { shift; "$@"; }
tenant_worker_enabled() { [[ "$2" == pebble ]]; }
tenant_proxy_enabled() { return 1; }
tenant_weechat_enabled() { return 1; }
tenant_darkirc_enabled() { return 1; }
tenant_vision_external() { return 0; }
tenant_voice_enabled() { return 1; }
clean_stale_pause_pid() { printf 'CLEAN\n' >>"$calls"; }
retire_tenant_nanocode() { printf 'RETIRE\n' >>"$calls"; }
start_tenant() { printf 'START\n' >>"$calls"; }

# Keep the actual stop_tenant and quiescence logic; replace only individual
# container stop commands, which cannot establish the writer proof themselves.
id() { if [[ "${1:-}" == -u && "${2:-}" == "$TENANT" ]]; then command id -u; else command id "$@"; fi; }
stop_tenant_vision() { :; }
stop_tenant_voice() { :; }
stop_tenant_pebble() { native_stop "lunarwing-pebble-$1"; }
stop_tenant_opencode() { :; }
stop_tenant_kimi() { :; }
stop_tenant_opencode2() { :; }
stop_tenant_postgres() { :; }

native_stop() {
  local service="$1"
  printf 'STOP %s\n' "$service" >>"$calls"
  [[ "$scenario" != stop-fails || "$service" != lunarwing-acorn ]] || return 1
  touch "$CASE_DIR/$service.stopped"
}
_systemctl_user() {
  local service="${!#}"
  service="${service%.service}"
  case "$2" in
    is-active)
      if [[ "$scenario" == native-unknown ]]; then return 1; fi
      case "$service" in
        lunarwing-opencode*|lunarwing-kimi-*) printf 'unknown\n'; return 4 ;;
      esac
      if [[ -f "$CASE_DIR/$service.stopped" ]]; then
        printf 'inactive\n'; return 3
      fi
      if [[ "$scenario" == activating ]]; then printf 'activating\n'; return 3; fi
      printf 'active\n' ;;
    stop) native_stop "$service" ;;
    *) return 2 ;;
  esac
}
rc-service() {
  case "$2" in
    stop) native_stop "$1" ;;
    status)
      if [[ -f "$CASE_DIR/$1.stopped" ]]; then
        printf 'stopped\n'; return 3
      fi
      printf 'started\n' ;;
    *) return 2 ;;
  esac
}
_ctr() {
  shift
  if [[ "$1 ${2:-}" == 'system migrate' ]]; then
    printf 'MIGRATE\n' >>"$calls"
    [[ "$scenario" != migrate-fails ]] || return 125
    touch "$CASE_DIR/repaired"
    return 0
  fi
  if [[ ! -f "$CASE_DIR/repaired" ]]; then
    printf 'Error: invalid internal status. Try running "podman system migrate"\n' >&2
    return 125
  fi
  case "$1" in
    ps) printf 'PROBE\n' >>"$calls"; printf 'lunarwing-pebble-acorn\n' ;;
    inspect)
      printf 'WRITER_CHECK\n' >>"$calls"
      if [[ "$scenario" == writer-survives ]]; then printf 'true\n'; else printf 'false\n'; fi ;;
    *) return 2 ;;
  esac
}

for INIT_SYSTEM in systemd openrc; do
  scenarios=(recovered stop-fails migrate-fails writer-survives)
  [[ "$INIT_SYSTEM" != systemd ]] || scenarios+=(activating native-unknown)
  for scenario in "${scenarios[@]}"; do
    CASE_DIR="$fixture/$INIT_SYSTEM-$scenario"
    mkdir "$CASE_DIR"
    calls="$CASE_DIR/calls"
    : >"$calls"
    if (repair_tenant_podman_userns "$TENANT") >"$CASE_DIR/output" 2>&1; then
      [[ "$scenario" == recovered || "$scenario" == activating ]] || { cat "$CASE_DIR/output"; exit 1; }
      python3 - "$calls" "$INIT_SYSTEM" <<'PY'
import sys
from pathlib import Path
steps = Path(sys.argv[1]).read_text().splitlines()
assert steps.index('STOP lunarwing-acorn') < steps.index('CLEAN') < steps.index('MIGRATE')
assert steps.index('MIGRATE') < steps.index('WRITER_CHECK') < steps.index('RETIRE') < steps.index('START')
if sys.argv[2] == 'openrc':
    # Deselected worker's extant supervisor must also stop before migration.
    assert steps.index('STOP lunarwing-kimi-acorn-sup') < steps.index('MIGRATE')
PY
    else
      [[ "$scenario" != recovered && "$scenario" != activating ]] || { cat "$CASE_DIR/output"; exit 1; }
      ! grep -qx START "$calls" || { cat "$CASE_DIR/output"; exit 1; }
      if [[ "$scenario" == stop-fails || "$scenario" == native-unknown ]]; then
        ! grep -qx MIGRATE "$calls" || { cat "$CASE_DIR/output"; exit 1; }
      fi
    fi
    printf 'PASS: %s repair boundary (%s)\n' "$INIT_SYSTEM" "$scenario"
  done

  # A normal stop and the export writer probe still fail closed on exactly the
  # same broken runtime: only explicit repair may migrate before that proof.
  scenario=normal-stop
  CASE_DIR="$fixture/$INIT_SYSTEM-$scenario"
  mkdir "$CASE_DIR"
  calls="$CASE_DIR/calls"
  : >"$calls"
  if (stop_tenant "$TENANT") >"$CASE_DIR/output" 2>&1; then
    cat "$CASE_DIR/output"; exit 1
  fi
  rc=0
  tenant_writers_probe_status "$TENANT" || rc=$?
  [[ "$rc" == 2 ]] || { printf 'broken runtime incorrectly passed export writer probe\n'; exit 1; }
  ! grep -qx MIGRATE "$calls" || exit 1
  printf 'PASS: %s normal stop/export still fail closed\n' "$INIT_SYSTEM"
done
