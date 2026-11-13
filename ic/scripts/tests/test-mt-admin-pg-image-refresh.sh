#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
FIXTURE_ROOT="$(mktemp -d)"
trap 'rm -rf "$FIXTURE_ROOT"' EXIT
export FIXTURE_ROOT
failures=0

pass() { printf 'PASS: %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*"; failures=$((failures + 1)); }

run_refresh() {
  local init="$1" daemon="$2" volume="$3" pull_ok="$4" health_ok="$5"
  rm -f "$FIXTURE_ROOT/events" "$FIXTURE_ROOT/output"
  mkdir -p "$FIXTURE_ROOT/state"
  if FIXTURE_INIT="$init" FIXTURE_DAEMON="$daemon" FIXTURE_VOLUME="$volume" \
      FIXTURE_PULL_OK="$pull_ok" FIXTURE_HEALTH_OK="$health_ok" bash -c '
    set -uo pipefail
    source "$1"
    INIT_SYSTEM="$FIXTURE_INIT"
    MT_ROOTLESS=true
    PG_IMAGE=docker.io/pgvector/pgvector:pg16@sha256:reviewed
    sleep() { :; }
    ensure_container_runtime() { :; }
    ensure_init_system() { :; }
    tenant_exists_in_registry() { return 0; }
    sanitize_name() { printf "%s\n" "$1"; }
    podman_supports_quadlet() { [[ "$INIT_SYSTEM" == systemd ]]; }
    render_pg_quadlet() { printf "render-quadlet\n" >>"$FIXTURE_ROOT/events"; }
    tenant_postgres_image_status() { printf "inventory\n" >>"$FIXTURE_ROOT/events"; }
    start_tenant_postgres() {
      printf "pg-create\n" >>"$FIXTURE_ROOT/events"
      touch "$FIXTURE_ROOT/state/container" "$FIXTURE_ROOT/state/running"
    }
    stop_tenant_postgres() {
      printf "pg-stop\n" >>"$FIXTURE_ROOT/events"
      rm -f "$FIXTURE_ROOT/state/running"
    }
    _systemctl_user() {
      printf "systemctl:%s\n" "$*" >>"$FIXTURE_ROOT/events"
      local action="${2:-}" service="${3:-}"
      case "$action:$service" in
        is-active:lunarwing-alice.service)
          if [[ "$FIXTURE_DAEMON" == unknown ]]; then printf "activating\n"; return 1; fi
          if [[ -e "$FIXTURE_ROOT/state/daemon-active" ]]; then
            printf "active\n"; return 0
          fi
          printf "inactive\n"; return 3 ;;
        stop:lunarwing-alice.service) rm -f "$FIXTURE_ROOT/state/daemon-active" ;;
        start:lunarwing-alice.service) touch "$FIXTURE_ROOT/state/daemon-active" ;;
        start:lunarwing-pg-alice.service)
          touch "$FIXTURE_ROOT/state/container" "$FIXTURE_ROOT/state/running" ;;
      esac
      return 0
    }
    _openrc_service() {
      printf "openrc:%s\n" "$*" >>"$FIXTURE_ROOT/events"
      local service="$1" action="$2"
      case "$service:$action" in
        lunarwing-alice:status)
          if [[ "$FIXTURE_DAEMON" == unknown ]]; then return 1; fi
          [[ -e "$FIXTURE_ROOT/state/daemon-active" ]] && return 0
          return 3 ;;
        lunarwing-alice:stop) rm -f "$FIXTURE_ROOT/state/daemon-active" ;;
        lunarwing-alice:start) touch "$FIXTURE_ROOT/state/daemon-active" ;;
        lunarwing-pg-alice:stop) rm -f "$FIXTURE_ROOT/state/running" ;;
      esac
      return 0
    }
    _openrc_stop_service_if_installed() { _openrc_service "$1" stop; }
    _ctr_inspect() {
      local name="$1"; shift
      if [[ "${1:-}" == -f ]]; then
        local format="$2" target="$3"
        [[ "$target" == lunarwing-pg-alice ]] || return 1
        case "$format" in
          "{{.State.Running}}")
            [[ -e "$FIXTURE_ROOT/state/running" ]] && printf "true\n" || printf "false\n"
            return 0 ;;
          "{{.Config.Image}}") printf "%s\n" "$PG_IMAGE"; return 0 ;;
          "{{.Image}}") printf "sha256:image-id\n"; return 0 ;;
        esac
      fi
      [[ -e "$FIXTURE_ROOT/state/container" ]]
    }
    _ctr() {
      local name="$1"; shift
      printf "ctr:%s\n" "$*" >>"$FIXTURE_ROOT/events"
      case "${1:-}" in
        volume)
          if [[ "$FIXTURE_VOLUME" == true ]]; then printf "lunarwing-pg-alice\n"; return 0; fi
          return 1 ;;
        pull) [[ "$FIXTURE_PULL_OK" == true ]] || return 1 ;;
        rm) rm -f "$FIXTURE_ROOT/state/container" "$FIXTURE_ROOT/state/running" ;;
        exec)
          [[ "$FIXTURE_HEALTH_OK" == true ]] || return 1
          if [[ "$*" == *"SELECT extversion"* ]]; then printf "0.8.7\n"; fi
          return 0 ;;
      esac
      return 0
    }
    if [[ "$FIXTURE_DAEMON" == active ]]; then touch "$FIXTURE_ROOT/state/daemon-active"; fi
    touch "$FIXTURE_ROOT/state/container" "$FIXTURE_ROOT/state/running"
    refresh_tenant_postgres_image alice
  ' _ "$MT_ADMIN" >"$FIXTURE_ROOT/output" 2>&1; then
    REFRESH_RC=0
  else
    REFRESH_RC=$?
  fi
}

assert_log() {
  local label="$1" pattern="$2" expected="$3"
  if [[ "$expected" == present ]] && grep -Fq -- "$pattern" "$FIXTURE_ROOT/events"; then
    pass "$label"
  elif [[ "$expected" == absent ]] && ! grep -Fq -- "$pattern" "$FIXTURE_ROOT/events"; then
    pass "$label"
  else
    fail "$label (expected $expected: $pattern)"
  fi
}

assert_order() {
  local label="$1" first="$2" second="$3" first_line second_line
  first_line="$(grep -n -F -- "$first" "$FIXTURE_ROOT/events" | head -n1 | cut -d: -f1 || true)"
  second_line="$(grep -n -F -- "$second" "$FIXTURE_ROOT/events" | head -n1 | cut -d: -f1 || true)"
  if [[ -n "$first_line" && -n "$second_line" && "$first_line" -lt "$second_line" ]]; then
    pass "$label"
  else
    fail "$label ($first vs $second)"
  fi
}

assert_rc() {
  local label="$1" expected="$2"
  if [[ "$expected" == zero && "$REFRESH_RC" -eq 0 ]] \
      || [[ "$expected" == nonzero && "$REFRESH_RC" -ne 0 ]]; then
    pass "$label"
  else
    fail "$label (rc=$REFRESH_RC)"
  fi
}

assert_output() {
  local label="$1" pattern="$2"
  if grep -Fq -- "$pattern" "$FIXTURE_ROOT/output"; then
    pass "$label"
  else
    fail "$label (missing $pattern)"
  fi
}

run_refresh systemd active true true true
assert_rc "systemd refresh succeeds" zero
assert_order "systemd pulls before stopping daemon" "pull docker.io/pgvector" "stop lunarwing-alice.service"
assert_order "systemd removes old container before starting replacement" "rm -f lunarwing-pg-alice" "start lunarwing-pg-alice.service"
assert_order "systemd restarts daemon after replacement" "start lunarwing-pg-alice.service" "start lunarwing-alice.service"
assert_log "systemd records before and after" "inventory" present
if [[ "$(grep -c '^inventory$' "$FIXTURE_ROOT/events")" -eq 2 ]]; then
  pass "systemd has two inventory records"
else
  fail "systemd inventory count is not two"
fi
assert_log "refresh never deletes data volume" "volume rm" absent

run_refresh openrc active true true true
assert_rc "OpenRC refresh succeeds" zero
assert_order "OpenRC supervisor stops before container removal" "lunarwing-pg-alice-sup stop" "rm -f lunarwing-pg-alice"
assert_order "OpenRC recreates before restarting supervisor" "pg-create" "lunarwing-pg-alice-sup start"
assert_order "OpenRC daemon restarts after database" "lunarwing-pg-alice-sup start" "lunarwing-alice start"

run_refresh systemd active true false true
assert_rc "pull failure is fatal" nonzero
assert_log "pull failure does not stop daemon" "stop lunarwing-alice.service" absent
assert_log "pull failure does not remove container" "rm -f lunarwing-pg-alice" absent
assert_output "pull failure explains fail-closed behavior" "no service was stopped"

run_refresh systemd active false true true
assert_rc "missing named volume is fatal" nonzero
assert_log "missing volume refuses before pull" "pull docker.io/pgvector" absent
assert_log "missing volume refuses before daemon stop" "stop lunarwing-alice.service" absent

run_refresh systemd active true true false
assert_rc "unhealthy replacement is fatal" nonzero
assert_log "unhealthy replacement leaves daemon stopped" "start lunarwing-alice.service" absent
assert_log "unhealthy replacement retains volume" "volume rm" absent
assert_output "health failure names readiness and pgvector" "failed readiness/pgvector checks"

run_refresh openrc stopped true true true
assert_rc "stopped-daemon refresh succeeds" zero
assert_log "initially stopped daemon is not stopped again" "lunarwing-alice stop" absent
assert_log "initially stopped daemon stays stopped" "lunarwing-alice start" absent

run_refresh systemd unknown true true true
assert_rc "unknown daemon state is fatal" nonzero
assert_log "unknown daemon state prevents container removal" "rm -f lunarwing-pg-alice" absent

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %s failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: PostgreSQL image refresh harness passed\n'
