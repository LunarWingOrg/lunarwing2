#!/usr/bin/env bash
# WI#638 — hermetic remove-tenant worker/egress ordering harness.
# shellcheck disable=SC1091,SC2034,SC2317
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

failures=0
fixture="$(mktemp -d)"
trap 'command rm -rf "$fixture"' EXIT
shopt -s nullglob
WORKER_LANES=(pebble opencode kimi opencode2 lunardepth)
UNIT_LANES=(nanocode pebble opencode opencode2 lunardepth kimi vision voice)

pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1" >&2; failures=$((failures + 1)); }
assert_true() {
  if [[ "$1" == true ]]; then pass "$2"; else fail "$2"; fi
}
record() { printf '%s\n' "$*" >>"$CALL_LOG"; }
line_of() { grep -n -F -m1 -- "$1" "$CALL_LOG" | cut -d: -f1 || true; }

id() {
  if [[ "${1:-}" == -u && "${2:-}" == alpha ]]; then
    printf '1000\n'
    return 0
  fi
  command id "$@"
}
ensure_init_system() { :; }
require_safe_openrc_proxy_units() { return 0; }
ensure_container_runtime() {
  CONTAINER_RT="$FAKE_RUNTIME"
  MT_ROOTLESS=true
  [[ "$FAKE_RUNTIME" != docker ]] || MT_ROOTLESS=false
}
podman_supports_quadlet() { return 0; }
tenant_darkirc_enabled() { return 1; }
tenant_vision_external() { return 1; }
tenant_home() { printf '%s/home\n' "$CASE_DIR"; }
tenant_quadlet_dir() { printf '%s/quadlets\n' "$CASE_DIR"; }
_hash_dir() { printf '%s/hashes\n' "$CASE_DIR"; }
_openrc_cgroups_locald_path() { printf '%s/no-cgroups\n' "$CASE_DIR"; }
_openrc_tenant_subtree_remove() { return 0; }
all_tenant_names() { printf 'alpha\n'; }
retire_tenant_nanocode() { record "retire-nanocode:$1"; }
stop_tenant_postgres() { record "stop-postgres:$1"; }
reset_tenant_postgres() { record "reset-postgres:$1"; }
ports_deallocate() { record "ports-deallocate:$1"; }
remove_tenant_user() { record "remove-user:$1:$2"; }
_remove_vision_systemd_fallback() { return 0; }
_remove_voice_systemd_fallback() { return 0; }

_systemctl_user() {
  local name="$1" command="$2" unit="${!#}"
  shift 2
  record "systemctl:$name $command $*"
  case "$command" in
    is-active) printf 'active\n' ;;
    show) printf 'loaded\n' ;;
    stop) [[ "$unit" != "$SYSTEMCTL_STOP_FAIL" ]] ;;
    *) return 0 ;;
  esac
}

rc-service() {
  local unit="$1" command="$2"
  record "rc-service:$unit $command"
  if [[ "$command" == stop ]]; then
    [[ "$unit" != "$RC_STOP_FAIL" ]]
  elif [[ "$command" == status ]]; then
    printf 'stopped\n'
  fi
}
rc-update() {
  local command="$1" unit="$2" runlevel="${3:-default}"
  record "rc-update:$command $unit $runlevel"
  if [[ "$command" == del ]]; then
    command rm -f -- "$OPENRC_RUNLEVELS_DIR/$runlevel/$unit"
  fi
}

_container_file() { printf '%s/containers/%s\n' "$RUNTIME_DIR" "$1"; }
_volume_file() { printf '%s/volumes/%s\n' "$RUNTIME_DIR" "$1"; }

_ctr() {
  local name="$1"
  shift
  record "${FAKE_RUNTIME}:$name $*"
  local command="${1:-}" container path
  case "$command" in
    container)
      shift
      [[ "${1:-}" == inspect ]] || return 1
      shift
      if [[ "${1:-}" == -f || "${1:-}" == --format ]]; then
        shift 2
      fi
      container="${1:-}"
      path="$(_container_file "$container")"
      if [[ ! -f "$path" ]]; then
        printf 'Error: no such container "%s"\n' "$container" >&2
        return 1
      fi
      command cat "$path"
      ;;
    stop)
      shift
      [[ "${1:-}" == --time ]] && shift 2
      container="${1:-}"
      [[ "$container" != "$FAIL_RM_CONTAINER" || "$ALLOW_FAILED_STOP" == true ]] || return 1
      path="$(_container_file "$container")"
      [[ ! -f "$path" ]] || printf 'stopped\n' >"$path"
      ;;
    rm)
      container="${!#}"
      [[ "$container" != "$FAIL_RM_CONTAINER" ]] || return 1
      command rm -f -- "$(_container_file "$container")"
      ;;
    volume)
      case "${2:-}" in
        ls)
          for path in "$RUNTIME_DIR"/volumes/*; do
            [[ -f "$path" ]] && printf '%s\n' "${path##*/}"
          done
          ;;
        rm)
          [[ "${3:-}" != "$FAIL_VOLUME" ]] || return 1
          command rm -f -- "$(_volume_file "${3:-}")"
          ;;
      esac
      ;;
    ps)
      [[ "$PS_FAIL" != true ]] || return 1
      for path in "$RUNTIME_DIR"/containers/*; do
        [[ -f "$path" ]] && printf '%s\n' "${path##*/}"
      done
      [[ ! -f "$RUNTIME_DIR/keeper" ]] || printf 'lw-egress-keeper-alpha-infra\n'
      ;;
    pod)
      case "${2:-}" in
        exists) [[ -f "$RUNTIME_DIR/keeper" ]] ;;
        inspect)
          [[ -f "$RUNTIME_DIR/keeper" ]] || return 1
          printf 'worker-egress-keeper\n'
          ;;
        rm) command rm -f -- "$RUNTIME_DIR/keeper" ;;
      esac
      ;;
    network)
      [[ "${2:-}" == rm ]] || return 1
      command rm -f -- "$RUNTIME_DIR/network"
      ;;
    *) return 0 ;;
  esac
}

worker_egress_nft() {
  local name="$1"
  shift
  record "nft:$name $*"
}

seed_unit() {
  local svc="$1"
  if [[ "$INIT_SYSTEM" == openrc ]]; then
    : >"$OPENRC_INITD_DIR/$svc"
    : >"$OPENRC_CONFD_DIR/$svc"
    ln -s "$OPENRC_INITD_DIR/$svc" "$OPENRC_RUNLEVELS_DIR/default/$svc"
    ln -s "$OPENRC_INITD_DIR/$svc" "$OPENRC_RUNLEVELS_DIR/custom/$svc"
  else
    : >"$(tenant_quadlet_dir alpha)/$svc.container"
  fi
}

setup_case() {
  local init="$1" lane svc
  CASE_DIR="$fixture/$init-$2-$RANDOM"
  RUNTIME_DIR="$CASE_DIR/runtime"
  CALL_LOG="$CASE_DIR/calls.log"
  OPENRC_INITD_DIR="$CASE_DIR/init.d"
  OPENRC_CONFD_DIR="$CASE_DIR/conf.d"
  OPENRC_RUNLEVELS_DIR="$CASE_DIR/runlevels"
  CONTAINER_HASH_DIR="$CASE_DIR/hashes"
  WORKER_EGRESS_DIR="$CASE_DIR/egress"
  INIT_SYSTEM="$init"
  FAKE_RUNTIME=podman
  CONTAINER_RT="$FAKE_RUNTIME"
  MT_ROOTLESS=true
  SYSTEMCTL_STOP_FAIL=""
  RC_STOP_FAIL=""
  FAIL_RM_CONTAINER=""
  FAIL_VOLUME=""
  ALLOW_FAILED_STOP=false
  PS_FAIL=false
  mkdir -p "$RUNTIME_DIR/containers" "$RUNTIME_DIR/volumes" \
    "$OPENRC_INITD_DIR" "$OPENRC_CONFD_DIR" \
    "$OPENRC_RUNLEVELS_DIR/default" "$OPENRC_RUNLEVELS_DIR/custom" \
    "$(tenant_quadlet_dir alpha)" "$CASE_DIR/home/.config/systemd/user" "$WORKER_EGRESS_DIR"
  : >"$CALL_LOG"
  printf 'SUBNET=10.89.40.0/24\n' >"$WORKER_EGRESS_DIR/alpha.conf"
  : >"$WORKER_EGRESS_DIR/alpha.nft"
  : >"$WORKER_EGRESS_DIR/alpha.hosts"
  : >"$RUNTIME_DIR/network"
  : >"$RUNTIME_DIR/keeper"

  for lane in "${WORKER_LANES[@]}"; do
    printf 'running\n' >"$(_container_file "lunarwing-${lane}-alpha")"
    printf 'running\n' >"$(_container_file "lunarwing-${lane}-task-alpha")"
    : >"$(_volume_file "lunarwing-${lane}-alpha-ipc-v2")"
  done
  for lane in vision voice; do
    printf 'running\n' >"$(_container_file "lunarwing-${lane}-alpha")"
  done

  if [[ "$INIT_SYSTEM" == openrc ]]; then
    seed_unit "lunarwing-alpha"
    seed_unit "xmpp-bridge-alpha"
    seed_unit "lunarwing-pg-alpha"
    seed_unit "lunarwing-pg-alpha-sup"
  else
    : >"$CASE_DIR/home/.config/systemd/user/lunarwing-alpha.service"
    : >"$CASE_DIR/home/.config/systemd/user/xmpp-bridge-alpha.service"
    seed_unit "lunarwing-pg-alpha"
  fi
  for lane in "${UNIT_LANES[@]}"; do
    if [[ "$INIT_SYSTEM" == systemd ]]; then
      seed_unit "lunarwing-${lane}-alpha"
      case "$lane" in
        pebble|opencode|opencode2|lunardepth|kimi) seed_unit "lunarwing-${lane}-task-alpha" ;;
      esac
    else
      for svc in "lunarwing-${lane}-alpha" "lunarwing-${lane}-alpha-sup" \
        "lunarwing-${lane}-task-alpha" "lunarwing-${lane}-task-alpha-sup"; do
        seed_unit "$svc"
      done
    fi
  done
}

assert_no_unit_left() {
  local path
  for path in "$OPENRC_INITD_DIR"/*alpha* "$OPENRC_CONFD_DIR"/*alpha* \
    "$OPENRC_RUNLEVELS_DIR"/*/*alpha* "$(tenant_quadlet_dir alpha)"/*alpha* \
    "$CASE_DIR/home/.config/systemd/user"/*alpha*; do
    if [[ -e "$path" || -L "$path" ]]; then
      fail "$INIT_SYSTEM left tenant unit path $path"
      return
    fi
  done
  pass "$INIT_SYSTEM leaves no tenant unit or runlevel link"
}

assert_no_container_left() {
  local path
  for path in "$RUNTIME_DIR"/containers/* "$RUNTIME_DIR"/volumes/*; do
    if [[ -e "$path" ]]; then
      fail "$INIT_SYSTEM left runtime object $path"
      return
    fi
  done
  [[ ! -e "$RUNTIME_DIR/keeper" && ! -e "$RUNTIME_DIR/network" ]] || {
    fail "$INIT_SYSTEM left the keeper or egress network"
    return
  }
  pass "$INIT_SYSTEM removed every worker, IPC volume, keeper, and network"
}

assert_removals_before_flush() {
  local flush_line rm_line lane container
  flush_line="$(line_of 'nft:alpha flush chain')"
  [[ -n "$flush_line" ]] || { fail "$INIT_SYSTEM never flushed the empty tenant chains"; return; }
  for lane in "${WORKER_LANES[@]}"; do
    for container in "lunarwing-${lane}-alpha" "lunarwing-${lane}-task-alpha"; do
      rm_line="$(line_of "${FAKE_RUNTIME}:alpha rm -f $container")"
      if [[ -z "$rm_line" || "$rm_line" -ge "$flush_line" ]]; then
        fail "$INIT_SYSTEM removed $container after the first nft flush"
        return
      fi
    done
  done
  pass "$INIT_SYSTEM removes both roles of every lane before the first nft flush"
}

run_success_case() {
  local init="$1" purge="$2" rc=0 lane unit_line reload_line first_rm
  setup_case "$init" "$purge"
  (remove_tenant alpha "$purge") >"$CASE_DIR/remove.out" 2>&1 || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    fail "$init purge=$purge remove-tenant failed: $(command cat "$CASE_DIR/remove.out")"
    return
  fi
  pass "$init purge=$purge remove-tenant succeeds"
  assert_removals_before_flush
  assert_no_container_left
  assert_no_unit_left
  for lane in "${WORKER_LANES[@]}"; do
    if [[ "$init" == openrc ]]; then
      unit_line="$(line_of "rc-service:lunarwing-${lane}-task-alpha-sup stop")"
      [[ -n "$unit_line" ]] || fail "OpenRC did not stop $lane task babysitter"
    else
      unit_line="$(line_of "systemctl:alpha stop lunarwing-${lane}-task-alpha.service")"
      [[ -n "$unit_line" ]] || fail "systemd did not stop $lane task service"
    fi
  done
  if [[ "$init" == systemd ]]; then
    reload_line="$(line_of 'systemctl:alpha daemon-reload')"
    first_rm="$(line_of "${FAKE_RUNTIME}:alpha rm -f lunarwing-pebble-alpha")"
    if [[ -n "$reload_line" && -n "$first_rm" && "$reload_line" -lt "$first_rm" ]]; then
      pass 'systemd forgets Quadlets before removing their containers'
    else
      fail 'systemd can restart a task Quadlet during container removal'
    fi
  fi
}

run_stop_failure_case() {
  local init="$1" rc=0
  setup_case "$init" false
  if [[ "$init" == openrc ]]; then
    RC_STOP_FAIL="lunarwing-kimi-task-alpha-sup"
  else
    SYSTEMCTL_STOP_FAIL="lunarwing-kimi-task-alpha.service"
  fi
  (remove_tenant alpha false) >"$CASE_DIR/remove.out" 2>&1 || rc=$?
  [[ "$rc" -ne 0 ]] || fail "$init removal succeeded despite a task stop failure"
  if grep -q 'nft:alpha flush chain' "$CALL_LOG"; then
    fail "$init flushed egress after a task stop failure"
  else
    pass "$init task stop failure aborts before any nft flush"
  fi
  [[ -f "$WORKER_EGRESS_DIR/alpha.conf" ]] || fail "$init discarded recoverable egress state"
}

run_remove_failure_case() {
  local init="$1" rc=0
  setup_case "$init" false
  FAIL_RM_CONTAINER="lunarwing-opencode2-task-alpha"
  (remove_tenant alpha false) >"$CASE_DIR/remove.out" 2>&1 || rc=$?
  [[ "$rc" -ne 0 ]] || fail "$init removal succeeded despite a container removal failure"
  if grep -q 'nft:alpha flush chain' "$CALL_LOG"; then
    fail "$init flushed egress after a container removal failure"
  else
    pass "$init container removal failure aborts before any nft flush"
  fi
  [[ -f "$(_container_file "$FAIL_RM_CONTAINER")" ]] || fail "$init lost the failed-container fixture"
}

run_attached_teardown_refusal() {
  local query_failure="$1" rc=0
  setup_case openrc false
  PS_FAIL="$query_failure"
  (worker_egress_teardown alpha) >"$CASE_DIR/teardown.out" 2>&1 || rc=$?
  [[ "$rc" -ne 0 ]] || fail "egress teardown succeeded with query_failure=$query_failure and attachments"
  if grep -q 'nft:alpha flush chain' "$CALL_LOG"; then
    fail "egress teardown flushed with query_failure=$query_failure and attachments"
  else
    pass "egress teardown refuses attachments/query_failure=$query_failure before flushing"
  fi
  [[ -f "$WORKER_EGRESS_DIR/alpha.conf" && -f "$RUNTIME_DIR/network" ]] \
    || fail "refused teardown discarded state needed for retry"
}

run_rootful_docker_case() {
  local rc=0
  setup_case openrc false
  FAKE_RUNTIME=docker
  CONTAINER_RT=docker
  MT_ROOTLESS=false
  command rm -f -- "$RUNTIME_DIR/keeper"
  (remove_tenant alpha false) >"$CASE_DIR/remove.out" 2>&1 || rc=$?
  [[ "$rc" -eq 0 ]] || {
    fail "rootful Docker remove-tenant failed: $(command cat "$CASE_DIR/remove.out")"
    return
  }
  pass 'rootful Docker remove-tenant succeeds without a namespace keeper'
  assert_removals_before_flush
  assert_no_container_left
  assert_no_unit_left
}

for init in openrc systemd; do
  run_success_case "$init" false
  run_success_case "$init" true
  run_stop_failure_case "$init"
  run_remove_failure_case "$init"
done
run_rootful_docker_case
run_attached_teardown_refusal false
run_attached_teardown_refusal true

if ((failures > 0)); then
  printf '\n%d failure(s)\n' "$failures" >&2
  exit 1
fi
printf '\nremove-tenant harness: all checks passed\n'
