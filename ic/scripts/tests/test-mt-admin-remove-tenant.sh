#!/usr/bin/env bash
# WI#638, WI#650, WI#672, WI#688 — hermetic remove-tenant worker/egress harness.
# shellcheck disable=SC1091,SC2034,SC2317
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"
# Keep the real PostgreSQL steps: the missing-account cases run them against
# the strict fake runtime, which dies like the real `_ctr` (WI#688, ACA-1).
eval "$(declare -f stop_tenant_postgres | sed '1s/^stop_tenant_postgres/real_stop_tenant_postgres/')"
eval "$(declare -f reset_tenant_postgres | sed '1s/^reset_tenant_postgres/real_reset_tenant_postgres/')"
eval "$(declare -f remove_tenant_user | sed '1s/^remove_tenant_user/real_remove_tenant_user/')"

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
    [[ "${MISSING_ACCOUNT:-false}" != true && "${GETENT_FAIL:-false}" != true ]] || return 1
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
stop_tenant_postgres() {
  if [[ "$REAL_POSTGRES" == true ]]; then real_stop_tenant_postgres "$@"; return; fi
  record "stop-postgres:$1"
}
reset_tenant_postgres() {
  if [[ "$REAL_POSTGRES" == true ]]; then real_reset_tenant_postgres "$@"; return; fi
  record "reset-postgres:$1"
}
getent() {
  # The tenant: an entry, "not found" (2) once the account is gone, or a
  # failed NSS lookup (1), which must never count as gone.
  if [[ "${1:-}" == passwd && "${2:-}" == alpha ]]; then
    [[ "${GETENT_FAIL:-false}" != true ]] || return 1
    [[ "${MISSING_ACCOUNT:-false}" != true ]] || return 2
    printf 'alpha:x:1000:1000::%s/home:/bin/bash\n' "$CASE_DIR"
    return 0
  fi
  if [[ "${1:-}" == passwd && -n "$HELD_UID" && "${2:-}" == "$HELD_UID" ]]; then
    printf 'bob:x:%s:%s::/home/bob:/bin/sh\n' "$HELD_UID" "$HELD_UID"
    return 0
  fi
  command getent "$@"
}
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

# The fake runtime lists containers as "<id> <name>" and truncates IDs unless
# --no-trunc is used. The keeper's infra container has a fixed ID, which
# `pod inspect` also reports; every other container gets an ID from its name.
KEEPER_INFRA_ID="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
KEEPER_INFRA_NAME="0123456789ab-infra"
_fake_container_id() { printf 'c0ffee%058d' "$(printf '%s' "$1" | cksum | cut -d' ' -f1)"; }

_fake_print_container() {
  local id="$1" container="$2" no_trunc="$3"
  [[ "$no_trunc" == true ]] || id="${id:0:12}"
  printf '%s %s\n' "$id" "$container"
}

_ctr() {
  local name="$1"
  # Like the real `_ctr`: only the rootless path needs the tenant account.
  if [[ "${STRICT_MISSING_ACCOUNT:-false}" == true && "${MISSING_ACCOUNT:-false}" == true \
      && "$MT_ROOTLESS" == true ]]; then
    die "cannot resolve uid for tenant '$name'"
  fi
  shift
  record "${FAKE_RUNTIME}:$name $*"
  local command="${1:-}" container path network_filter format no_trunc
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
      shift
      network_filter=""
      format=""
      no_trunc=false
      while (($# > 0)); do
        case "$1" in
          --all) shift ;;
          --no-trunc) no_trunc=true; shift ;;
          --filter)
            case "${2:-}" in
              network=*) network_filter="${2#network=}" ;;
              # `ps` on Docker knows no negated filter, and no Podman older
              # than the `label!=` filter does either. The check must not
              # depend on one (WI#650 review, ABA-1).
              *)
                printf 'Error: invalid filter %s\n' "${2%%=*}" >&2
                return 1
                ;;
            esac
            shift 2
            ;;
          --format) format="${2:-}"; shift 2 ;;
          *) return 1 ;;
        esac
      done
      [[ "$network_filter" == "lw-egress-alpha" ]] || return 1
      [[ "$format" == '{{.ID}} {{.Names}}' ]] || return 1
      # List the keeper first so a loop that stops at it cannot hide the other
      # containers. Its network attachment is independent of pod existence.
      if [[ -f "$RUNTIME_DIR/keeper" && "$KEEPER_ON_NETWORK" == true ]]; then
        _fake_print_container "$KEEPER_INFRA_ID" "$KEEPER_INFRA_NAME" "$no_trunc"
      fi
      for path in "$RUNTIME_DIR"/containers/*; do
        [[ ! -f "$path" ]] || _fake_print_container \
          "$(_fake_container_id "${path##*/}")" "${path##*/}" "$no_trunc"
      done
      ;;
    pod)
      [[ "$FAKE_RUNTIME" == podman ]] || return 1
      case "${2:-}" in
        exists)
          [[ "${3:-}" == lw-egress-keeper-alpha ]] || return 125
          if [[ -n "$POD_EXISTS_STATUS" ]]; then
            return "$POD_EXISTS_STATUS"
          fi
          [[ -f "$RUNTIME_DIR/keeper" ]]
          ;;
        inspect)
          [[ "${3:-}" == --format && "${5:-}" == lw-egress-keeper-alpha \
            && -f "$RUNTIME_DIR/keeper" ]] || return 1
          case "${4:-}" in
            '{{index .Labels "lunarwing.purpose"}}')
              [[ "$POD_LABEL_INSPECT_FAIL" != true ]] || return 1
              printf '%s\n' "$KEEPER_PURPOSE"
              ;;
            '{{.NumContainers}}')
              [[ "$POD_COUNT_INSPECT_FAIL" != true ]] || return 1
              printf '%s\n' "$KEEPER_NUM_CONTAINERS"
              ;;
            '{{.InfraContainerID}}')
              [[ "$POD_INFRA_INSPECT_FAIL" != true ]] || return 1
              printf '%s\n' "$KEEPER_INFRA_ID"
              ;;
            *) return 1 ;;
          esac
          ;;
        rm)
          [[ "${3:-}" == -f && "${4:-}" == lw-egress-keeper-alpha ]] || return 1
          command rm -f -- "$RUNTIME_DIR/keeper"
          ;;
        *) return 1 ;;
      esac
      ;;
    network)
      [[ "${2:-}" == rm ]] || return 1
      command rm -f -- "$RUNTIME_DIR/network"
      if [[ "$NETWORK_RM_MISSING" == true ]]; then
        printf 'network lw-egress-%s not found\n' "$name" >&2
        return 1
      fi
      ;;
    *) return 0 ;;
  esac
}

worker_egress_nft() {
  local name="$1"
  # Like the real one: rootless Podman runs nft in the tenant's rootless
  # network namespace, which needs the account.
  if [[ "${STRICT_MISSING_ACCOUNT:-false}" == true && "${MISSING_ACCOUNT:-false}" == true \
      && "$FAKE_RUNTIME" == podman && "$MT_ROOTLESS" == true ]]; then
    die "cannot resolve uid for tenant '$name'"
  fi
  shift
  record "nft:$name $*"
  case " $* " in
    *" flush chain "*|*" delete chain "*) command rm -f -- "$RUNTIME_DIR/chains" ;;
  esac
}

_fake_status() {
  printf 'Name:\tfake\nUid:\t%s\t%s\t%s\t%s\nGid:\t0\t0\t0\t0\n' "$2" "$3" "$4" "$5" >"$1"
}

# A fake /proc/<pid>/status, and its leader's task entry, with the given
# real/effective/saved/fs uids (one value repeats into all four).
seed_process() {
  local pid="$1" real="$2" effective="${3:-$2}" saved="${4:-$2}" fs="${5:-$2}"
  mkdir -p "$TENANT_PROC_ROOT/$pid/task/$pid"
  _fake_status "$TENANT_PROC_ROOT/$pid/status" "$real" "$effective" "$saved" "$fs"
  _fake_status "$TENANT_PROC_ROOT/$pid/task/$pid/status" "$real" "$effective" "$saved" "$fs"
}

# One more thread of a process, with credentials of its own.
seed_thread() {
  local pid="$1" tid="$2" uid="$3"
  mkdir -p "$TENANT_PROC_ROOT/$pid/task/$tid"
  _fake_status "$TENANT_PROC_ROOT/$pid/task/$tid/status" "$uid" "$uid" "$uid" "$uid"
}

# The identity add-tenant records for the tenant whose account is now gone.
seed_identity() {
  mkdir -p "$TENANT_IDENTITY_DIR"
  printf 'UID=61000\nSUBUID=231072:65536\n' >"$TENANT_IDENTITY_DIR/alpha"
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
  TENANT_IDENTITY_DIR="$CASE_DIR/identity"
  TENANT_SUBUID_FILE="$CASE_DIR/subuid"
  TENANT_PROC_ROOT="$CASE_DIR/proc"
  REAL_POSTGRES=false
  HELD_UID=""
  GETENT_FAIL=false
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
  POD_EXISTS_STATUS=""
  POD_LABEL_INSPECT_FAIL=false
  POD_COUNT_INSPECT_FAIL=false
  POD_INFRA_INSPECT_FAIL=false
  KEEPER_PURPOSE=worker-egress-keeper
  KEEPER_NUM_CONTAINERS=1
  KEEPER_ON_NETWORK=true
  NETWORK_RM_MISSING=false
  mkdir -p "$RUNTIME_DIR/containers" "$RUNTIME_DIR/volumes" \
    "$OPENRC_INITD_DIR" "$OPENRC_CONFD_DIR" \
    "$OPENRC_RUNLEVELS_DIR/default" "$OPENRC_RUNLEVELS_DIR/custom" \
    "$(tenant_quadlet_dir alpha)" "$CASE_DIR/home/.config/systemd/user" "$WORKER_EGRESS_DIR"
  : >"$CALL_LOG"
  printf 'alpha:231072:65536\n' >"$TENANT_SUBUID_FILE"
  # Unrelated processes: root, a system account, and the uid right after the
  # tenant's subordinate range.
  seed_process 1 0
  seed_process 412 966
  seed_process 977 296608
  printf 'SUBNET=10.89.40.0/24\n' >"$WORKER_EGRESS_DIR/alpha.conf"
  : >"$WORKER_EGRESS_DIR/alpha.nft"
  : >"$WORKER_EGRESS_DIR/alpha.hosts"
  : >"$RUNTIME_DIR/network"
  : >"$RUNTIME_DIR/keeper"
  : >"$RUNTIME_DIR/chains"

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
  [[ ! -e "$RUNTIME_DIR/keeper" && ! -e "$RUNTIME_DIR/network" && ! -e "$RUNTIME_DIR/chains" ]] || {
    fail "$INIT_SYSTEM left the keeper, chains, or egress network"
    return
  }
  pass "$INIT_SYSTEM removed every worker, IPC volume, keeper, chain, and network"
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

assert_teardown_order() {
  local ps_line flush_line keeper_line network_line
  ps_line="$(line_of 'podman:alpha ps --all --no-trunc --filter network=lw-egress-alpha --format')"
  flush_line="$(line_of 'nft:alpha flush chain')"
  keeper_line="$(line_of 'podman:alpha pod rm -f lw-egress-keeper-alpha')"
  network_line="$(line_of 'podman:alpha network rm lw-egress-alpha')"
  if [[ -z "$ps_line" || -z "$flush_line" || -z "$keeper_line" || -z "$network_line" ]]; then
    fail "$INIT_SYSTEM skipped a required egress teardown step"
  elif ((ps_line < flush_line && flush_line < keeper_line && keeper_line < network_line)); then
    pass "$INIT_SYSTEM checks, flushes, removes the keeper, then removes the network"
  else
    fail "$INIT_SYSTEM egress teardown order is unsafe"
  fi
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
  if [[ ! -e "$TENANT_IDENTITY_DIR/alpha" ]] && grep -q "^remove-user:alpha:$purge\$" "$CALL_LOG"; then
    pass "$init purge=$purge removal with the account deletes the identity record it wrote"
  else
    fail "$init purge=$purge removal with the account left the identity record"
  fi
  assert_removals_before_flush
  assert_teardown_order
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

run_missing_account_stop_case() {
  local init="$1" lane rc
  setup_case "$init" missing-account-stops
  for lane in vision voice pebble opencode kimi opencode2 lunardepth; do
    rc=0
    (MISSING_ACCOUNT=true STRICT_MISSING_ACCOUNT=true; "stop_tenant_${lane}" alpha) \
      >"$CASE_DIR/stop-$lane.out" 2>&1 || rc=$?
    if [[ "$rc" -eq 0 ]]; then
      pass "$init $lane stop tolerates an absent tenant account"
    else
      fail "$init $lane stop reached _ctr without a tenant account: $(command cat "$CASE_DIR/stop-$lane.out")"
    fi
    if grep -q '^skipped .* stop for alpha: the tenant account is gone' "$CASE_DIR/stop-$lane.out"; then
      pass "$init $lane stop says that it skipped the unreachable store"
    else
      fail "$init $lane stop skipped silently: $(command cat "$CASE_DIR/stop-$lane.out")"
    fi
  done
}

# ACA-2: a rootful runtime needs no tenant account, so every lane still stops
# its containers when the account is gone.
# add-tenant and remove-tenant record the uid and the subordinate ranges that
# belong to the account, by name or by uid, in a root-state file.
run_identity_record_case() {
  local rc=0 mode
  setup_case openrc identity-record
  printf 'other:100000:65536\nalpha:231072:65536\n1000:400000:1000\nalpha:bad:1\n' >"$TENANT_SUBUID_FILE"
  tenant_identity_record alpha || rc=$?
  if [[ "$rc" -eq 0 && "$(command cat "$TENANT_IDENTITY_DIR/alpha")" == \
      $'UID=1000\nSUBUID=231072:65536\nSUBUID=400000:1000' ]]; then
    pass 'identity record holds the uid and only the account'"'"'s valid subordinate ranges'
  else
    fail "identity record is wrong (rc=$rc): $(command cat "$TENANT_IDENTITY_DIR/alpha" 2>/dev/null)"
  fi
  mode="$(stat -c %a "$TENANT_IDENTITY_DIR/alpha" 2>/dev/null || true)"
  [[ "$mode" == 644 ]] && pass 'identity record is mode 0644' || fail "identity record mode is $mode"
  rc=0
  (MISSING_ACCOUNT=true; tenant_identity_record alpha) >/dev/null 2>&1 || rc=$?
  [[ "$rc" -ne 0 ]] && pass 'identity record needs the account' || fail 'identity record was written without the account'
  command rm -rf -- "$TENANT_IDENTITY_DIR"
  mkdir -p "$CASE_DIR/elsewhere"
  ln -s "$CASE_DIR/elsewhere" "$TENANT_IDENTITY_DIR"
  rc=0
  tenant_identity_record alpha >/dev/null 2>&1 || rc=$?
  if [[ "$rc" -ne 0 && ! -e "$CASE_DIR/elsewhere/alpha" ]]; then
    pass 'identity record refuses a linked state directory'
  else
    fail 'identity record followed a linked state directory'
  fi
  if declare -f create_tenant_user | grep -q 'tenant_identity_record "\$name"'; then
    pass 'add-tenant records the identity when it creates the account'
  else
    fail 'add-tenant does not record the tenant identity'
  fi
}

run_rootful_missing_account_stop_case() {
  local lane rc
  setup_case openrc rootful-missing-account-stops
  FAKE_RUNTIME=docker
  for lane in vision voice pebble opencode kimi opencode2 lunardepth; do
    rc=0
    (MISSING_ACCOUNT=true STRICT_MISSING_ACCOUNT=true; "stop_tenant_${lane}" alpha) \
      >"$CASE_DIR/stop-$lane.out" 2>&1 || rc=$?
    if [[ "$rc" -eq 0 ]] && lane_stop_called "$lane" \
        && ! grep -q '^skipped ' "$CASE_DIR/stop-$lane.out"; then
      pass "rootful $lane stop still stops the lane without the tenant account"
    else
      fail "rootful $lane stop did not stop the lane (rc=$rc): $(command cat "$CASE_DIR/stop-$lane.out")"
    fi
  done
}

# Any stop of one of the lane's roles: its OpenRC unit or babysitter, or the
# container itself through the root store.
lane_stop_called() {
  local lane="$1"
  grep -Eq "^(docker:alpha stop( --time [0-9]+)? lunarwing-${lane}(-task)?-alpha|rc-service:lunarwing-${lane}(-task)?-alpha(-sup)? stop)\$" \
    "$CALL_LOG"
}

assert_nothing_removed_after_refusal() {
  local label="$1"
  if grep -q 'nft:alpha' "$CALL_LOG"; then
    fail "$label touched the egress chains"
  else
    pass "$label leaves the egress chains alone"
  fi
  if grep -Fxq 'ports-deallocate:alpha' "$CALL_LOG" || grep -q '^remove-user:alpha' "$CALL_LOG"; then
    fail "$label released ports or removed the user"
  else
    pass "$label keeps ports and user"
  fi
  [[ -f "$WORKER_EGRESS_DIR/alpha.conf" && -f "$RUNTIME_DIR/chains" ]] \
    || fail "$label discarded the egress state needed for a retry"
}

# ACA-1: without the account, removal proves from the process table that no
# process of the recorded uid or subordinate uids is left, then skips the
# unreachable rootless store. The strict fake dies on any `_ctr` call, as the
# real one does; `keep` leaves the store's containers in place so nothing can
# pretend to have removed them.
run_missing_account_case() {
  local init="$1" purge="$2" keep="$3" rc=0 path
  setup_case "$init" "missing-account-$purge-$keep"
  if [[ "$keep" == false ]]; then
    for path in "$RUNTIME_DIR"/containers/* "$RUNTIME_DIR"/volumes/*; do
      command rm -f -- "$path"
    done
  fi
  seed_identity
  REAL_POSTGRES=true
  (MISSING_ACCOUNT=true STRICT_MISSING_ACCOUNT=true; remove_tenant alpha "$purge") \
    >"$CASE_DIR/remove.out" 2>&1 || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    fail "$init purge=$purge keep=$keep missing-account removal failed: $(command cat "$CASE_DIR/remove.out")"
    return
  fi
  pass "$init purge=$purge keep=$keep missing-account removal completes under the strict fake"
  if grep -Fxq 'ports-deallocate:alpha' "$CALL_LOG" \
      && grep -Fxq "remove-user:alpha:$purge" "$CALL_LOG"; then
    pass "$init purge=$purge keep=$keep missing-account removal reaches ports and user cleanup"
  else
    fail "$init purge=$purge keep=$keep missing-account removal skipped ports or user cleanup"
  fi
  if grep -q '^podman:' "$CALL_LOG"; then
    fail "$init purge=$purge keep=$keep reached the rootless store without its account"
  else
    pass "$init purge=$purge keep=$keep never calls the unreachable rootless store"
  fi
  # The rootless chains live in the tenant's namespace, which needs the account
  # (the fake nft dies there like the real one): nothing is flushed, only the
  # rendered state goes.
  if ! grep -q '^nft:alpha' "$CALL_LOG" && [[ ! -e "$WORKER_EGRESS_DIR/alpha.conf" \
      && ! -e "$WORKER_EGRESS_DIR/alpha.nft" && ! -e "$WORKER_EGRESS_DIR/alpha.hosts" ]]; then
    pass "$init purge=$purge keep=$keep drops the rendered egress state without entering the namespace"
  else
    fail "$init purge=$purge keep=$keep entered the rootless namespace or left egress state"
  fi
  if grep -q 'flushed' "$CASE_DIR/remove.out"; then
    fail "$init purge=$purge keep=$keep claims a flush it did not do"
  else
    pass "$init purge=$purge keep=$keep does not claim a flush"
  fi
  if grep -q 'skipped worker container removal for alpha' "$CASE_DIR/remove.out" \
      && grep -q 'skipped PostgreSQL stop for alpha' "$CASE_DIR/remove.out"; then
    pass "$init purge=$purge keep=$keep says which store steps it skipped"
  else
    fail "$init purge=$purge keep=$keep skipped store steps silently"
  fi
  if [[ "$purge" == true ]]; then
    grep -q 'skipped PostgreSQL data removal for alpha' "$CASE_DIR/remove.out" \
      && pass "$init purge=true keep=$keep names the PostgreSQL data it cannot remove" \
      || fail "$init purge=true keep=$keep hid the PostgreSQL data left behind"
  fi
  if [[ "$keep" == true ]]; then
    if [[ -f "$(_container_file lunarwing-pebble-alpha)" && -f "$(_volume_file lunarwing-kimi-alpha-ipc-v2)" ]]; then
      pass "$init purge=$purge keep=true leaves the unreachable store as it was"
    else
      fail "$init purge=$purge keep=true claims to have changed the unreachable store"
    fi
  fi
  [[ ! -e "$TENANT_IDENTITY_DIR/alpha" ]] \
    && pass "$init purge=$purge keep=$keep drops the identity record at the end" \
    || fail "$init purge=$purge keep=$keep left the identity record"
  assert_no_unit_left
}

# The removal must refuse, before it changes anything, whenever the former
# identity cannot be proven idle.
run_missing_account_refusal_case() {
  local init="$1" label="$2" expect="$3" rc=0
  setup_case "$init" "missing-account-refusal-$label"
  [[ "$label" == no-record ]] || seed_identity
  case "$label" in
    live-uid) seed_process 4242 61000 ;;
    live-effective-uid) seed_process 4243 0 61000 0 0 ;;
    live-fs-uid) seed_process 4244 0 0 0 61000 ;;
    live-saved-uid) seed_process 4247 0 0 61000 0 ;;
    live-subuid-first) seed_process 4245 231072 ;;
    live-subuid-last) seed_process 4246 296607 ;;
    no-record) ;;
    no-subuid) printf 'UID=61000\n' >"$TENANT_IDENTITY_DIR/alpha" ;;
    bad-record) printf 'UID=61000\nSUBUID=231072:65536\nEXTRA=1\n' >"$TENANT_IDENTITY_DIR/alpha" ;;
    system-uid) printf 'UID=0\nSUBUID=231072:65536\n' >"$TENANT_IDENTITY_DIR/alpha" ;;
    held-uid) HELD_UID=61000 ;;
    linked-record)
      command mv -- "$TENANT_IDENTITY_DIR/alpha" "$CASE_DIR/record"
      ln -s "$CASE_DIR/record" "$TENANT_IDENTITY_DIR/alpha"
      ;;
    bad-subuid) printf 'UID=61000\nSUBUID=231072:x\n' >"$TENANT_IDENTITY_DIR/alpha" ;;
    two-uids) printf 'UID=61000\nUID=62000\nSUBUID=231072:65536\n' >"$TENANT_IDENTITY_DIR/alpha" ;;
    crlf-record) printf 'UID=61000\r\nSUBUID=231072:65536\r\n' >"$TENANT_IDENTITY_DIR/alpha" ;;
    unreadable-live-status)
      # A status that exists and cannot be read (even by root) while its task lives.
      mkdir -p "$TENANT_PROC_ROOT/4500/status"
      ;;
    thread-only)
      seed_process 5000 0
      seed_thread 5000 5001 231500
      ;;
    nss-failure) GETENT_FAIL=true ;;
    empty-proc) command rm -rf -- "$TENANT_PROC_ROOT"; mkdir -p "$TENANT_PROC_ROOT" ;;
  esac
  REAL_POSTGRES=true
  (MISSING_ACCOUNT=true STRICT_MISSING_ACCOUNT=true; remove_tenant alpha true) \
    >"$CASE_DIR/remove.out" 2>&1 || rc=$?
  if [[ "$rc" -eq 0 ]]; then
    fail "$init missing-account removal with $label succeeded"
  elif grep -Fq -- "$expect" "$CASE_DIR/remove.out"; then
    pass "$init missing-account removal with $label refuses: $expect"
  else
    fail "$init missing-account removal with $label gave the wrong reason: $(command cat "$CASE_DIR/remove.out")"
  fi
  assert_nothing_removed_after_refusal "$init $label refusal"
  [[ -e "$(tenant_quadlet_dir alpha)/lunarwing-pg-alpha.container" || -e "$OPENRC_INITD_DIR/lunarwing-alpha" ]] \
    || fail "$init $label refusal removed units before refusing"
}

# A task listed by the glob that exits before its status is read is skipped,
# not refused: the read fails and its directory is gone. Without nullglob (as
# in production) an unmatched task glob is skipped too.
run_exited_task_case() {
  local rc=0
  setup_case openrc exited-task
  seed_identity
  seed_process 4700 61000
  eval "$(declare -f _tenant_status_uids | sed '1s/^_tenant_status_uids/real_tenant_status_uids/')"
  (
    _tenant_status_uids() {
      if [[ "$1" == "$TENANT_PROC_ROOT/4700/status" ]]; then
        command rm -rf -- "$TENANT_PROC_ROOT/4700"
        return 1
      fi
      real_tenant_status_uids "$@"
    }
    tenant_former_identity_idle alpha
  ) >"$CASE_DIR/idle.out" 2>&1 || rc=$?
  if [[ "$rc" -eq 0 ]]; then
    pass 'a task that exits between listing and read is skipped, not refused'
  else
    fail "a task that exited before its read refused the proof: $(command cat "$CASE_DIR/idle.out")"
  fi
  rc=0
  command rm -rf -- "$TENANT_PROC_ROOT"/*/task
  (shopt -u nullglob; tenant_former_identity_idle alpha) >"$CASE_DIR/idle-noglob.out" 2>&1 || rc=$?
  if [[ "$rc" -eq 0 ]]; then
    pass 'an unmatched task glob without nullglob is skipped, not refused'
  else
    fail "an unmatched task glob refused the proof: $(command cat "$CASE_DIR/idle-noglob.out")"
  fi
}

# After a removal without the account the former home stays, owned by the
# orphaned uid that the next useradd may reuse; the operator is told so. A
# failed account lookup removes nothing.
run_orphan_home_warning_case() {
  local rc=0
  setup_case openrc orphan-home
  seed_identity
  (MISSING_ACCOUNT=true; real_remove_tenant_user alpha true) >"$CASE_DIR/user.out" 2>&1 || rc=$?
  if [[ "$rc" -eq 0 ]] \
      && grep -Fq "WARNING: tenant account 'alpha' was already gone; its former home $CASE_DIR/home stays, owned by the orphaned uid 61000" "$CASE_DIR/user.out" \
      && grep -Fq 'remove the home before the next add-tenant' "$CASE_DIR/user.out"; then
    pass 'removal without the account warns about the orphaned home and its uid'
  else
    fail "removal without the account did not warn about the orphaned home (rc=$rc): $(command cat "$CASE_DIR/user.out")"
  fi
  rc=0
  (GETENT_FAIL=true; real_remove_tenant_user alpha true) >"$CASE_DIR/user-nss.out" 2>&1 || rc=$?
  if [[ "$rc" -ne 0 ]] && grep -q 'cannot look up the account of tenant' "$CASE_DIR/user-nss.out"; then
    pass 'a failed account lookup does not count as an already removed account'
  else
    fail "a failed account lookup was taken as gone (rc=$rc): $(command cat "$CASE_DIR/user-nss.out")"
  fi
}

# The teardown proves the former identity idle again right before its flush.
run_missing_account_teardown_refusal() {
  local rc=0
  setup_case openrc missing-account-teardown
  seed_identity
  seed_process 4300 231100
  (MISSING_ACCOUNT=true STRICT_MISSING_ACCOUNT=true; worker_egress_teardown alpha) \
    >"$CASE_DIR/teardown.out" 2>&1 || rc=$?
  if [[ "$rc" -ne 0 ]] && grep -q 'former uids are not proven idle; refusing teardown' "$CASE_DIR/teardown.out"; then
    pass 'missing-account egress teardown refuses while a former subordinate uid runs'
  else
    fail "missing-account egress teardown did not refuse (rc=$rc): $(command cat "$CASE_DIR/teardown.out")"
  fi
  assert_nothing_removed_after_refusal 'missing-account teardown refusal'
}

# The container removal applies the same rule on its own, as the OpenRC
# uninstall and the systemd uninstall call it.
run_missing_account_container_removal_refusal() {
  local init="$1" rc=0
  setup_case "$init" missing-account-container-removal
  seed_identity
  seed_process 4400 61000
  if [[ "$init" == openrc ]]; then
    (MISSING_ACCOUNT=true STRICT_MISSING_ACCOUNT=true; uninstall_tenant_openrc alpha) \
      >"$CASE_DIR/uninstall.out" 2>&1 || rc=$?
  else
    (MISSING_ACCOUNT=true STRICT_MISSING_ACCOUNT=true; uninstall_tenant_systemd alpha) \
      >"$CASE_DIR/uninstall.out" 2>&1 || rc=$?
  fi
  if [[ "$rc" -ne 0 ]] && grep -q 'entries of the former uid 61000' "$CASE_DIR/uninstall.out"; then
    pass "$init container removal without the account refuses while the former uid runs"
  else
    fail "$init container removal without the account did not refuse (rc=$rc): $(command cat "$CASE_DIR/uninstall.out")"
  fi
}

# ACA-2 end to end: on a rootful host the removal without the account still
# stops and removes every lane's containers through the root store.
run_rootful_missing_account_case() {
  local rc=0 lane
  setup_case openrc rootful-missing-account
  FAKE_RUNTIME=docker
  command rm -f -- "$RUNTIME_DIR/keeper"
  (MISSING_ACCOUNT=true STRICT_MISSING_ACCOUNT=true; remove_tenant alpha false) \
    >"$CASE_DIR/remove.out" 2>&1 || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    fail "rootful missing-account removal failed: $(command cat "$CASE_DIR/remove.out")"
    return
  fi
  pass 'rootful missing-account removal completes'
  for lane in vision voice pebble opencode kimi opencode2 lunardepth; do
    if lane_stop_called "$lane"; then
      pass "rootful missing-account removal stops $lane"
    else
      fail "rootful missing-account removal did not stop $lane"
    fi
  done
  assert_removals_before_flush
  assert_no_container_left
  assert_no_unit_left
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
  # The record is written before anything stops, so a rerun after a later
  # `userdel` can still prove the old uid idle.
  if [[ "$(command cat "$TENANT_IDENTITY_DIR/alpha" 2>/dev/null)" == $'UID=1000\nSUBUID=231072:65536' ]]; then
    pass "$init aborted removal keeps the identity it recorded first"
  else
    fail "$init removal did not record the tenant identity before stopping"
  fi
}

run_remove_failure_case() {
  local init="$1" rc=0
  setup_case "$init" false
  FAIL_RM_CONTAINER="lunarwing-opencode2-task-alpha"
  ALLOW_FAILED_STOP=true
  (remove_tenant alpha false) >"$CASE_DIR/remove.out" 2>&1 || rc=$?
  [[ "$rc" -ne 0 ]] || fail "$init removal succeeded despite a container removal failure"
  if grep -q 'nft:alpha flush chain' "$CALL_LOG"; then
    fail "$init flushed egress after a container removal failure"
  else
    pass "$init container removal failure aborts before any nft flush"
  fi
  if grep -Fq "ERROR: failed to stop and remove tenant container $FAIL_RM_CONTAINER" "$CASE_DIR/remove.out"; then
    pass "$init propagates the container rm failure before attachment checking"
  else
    fail "$init did not propagate the container rm failure itself"
  fi
  [[ -f "$(_container_file "$FAIL_RM_CONTAINER")" ]] || fail "$init lost the failed-container fixture"
}

run_volume_failure_case() {
  local rc=0
  setup_case openrc false
  FAIL_VOLUME="lunarwing-kimi-alpha-ipc-v2"
  (remove_tenant alpha false) >"$CASE_DIR/remove.out" 2>&1 || rc=$?
  [[ "$rc" -ne 0 ]] || fail 'removal succeeded despite a volume removal failure'
  if grep -q 'nft:alpha flush chain' "$CALL_LOG"; then
    fail 'egress was flushed after a volume removal failure'
  else
    pass 'volume removal failure aborts before any nft flush'
  fi
  if grep -Fq "ERROR: failed to remove worker IPC volume $FAIL_VOLUME" "$CASE_DIR/remove.out"; then
    pass 'volume rm failure is propagated before egress teardown'
  else
    fail 'volume rm failure was not propagated'
  fi
  [[ -f "$(_volume_file "$FAIL_VOLUME")" ]] || fail 'failed-volume fixture was discarded'
  [[ -f "$RUNTIME_DIR/keeper" && -f "$RUNTIME_DIR/chains" && -f "$RUNTIME_DIR/network" ]] \
    || fail 'volume failure did not preserve the live egress fixture'
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
  if [[ -f "$WORKER_EGRESS_DIR/alpha.conf" && -f "$RUNTIME_DIR/network" \
    && -f "$RUNTIME_DIR/keeper" && -f "$RUNTIME_DIR/chains" ]]; then
    pass "refusal query_failure=$query_failure preserves keeper, chains, network, and rendered state"
  else
    fail "refused teardown discarded state needed for retry"
  fi
}

# After every worker is gone, exactly one container may stay on the egress
# network: the keeper's own infra container, known by its ID. Each of these
# leaves something else there, or makes the keeper unidentifiable, and the
# teardown must refuse before it flushes anything (WI#650 and WI#672).
run_keeper_identity_case() {
  local label="$1" expect="$2" rc=0 path
  setup_case openrc false
  for path in "$RUNTIME_DIR"/containers/*; do command rm -f -- "$path"; done
  case "$label" in
    keeper-only) ;;
    lookalike)
      # Same name as the keeper's infra container, as a container that copied
      # the keeper's label would look to a label filter. Another ID.
      printf 'running\n' >"$(_container_file "$KEEPER_INFRA_NAME")"
      ;;
    foreign-pod) KEEPER_PURPOSE=something-else ;;
    foreign-off-network)
      KEEPER_PURPOSE=something-else
      KEEPER_ON_NETWORK=false
      ;;
    extra-container) KEEPER_NUM_CONTAINERS=2 ;;
    pod-exists-error) POD_EXISTS_STATUS=125 ;;
    inspect-fails) POD_LABEL_INSPECT_FAIL=true ;;
    count-inspect-fails) POD_COUNT_INSPECT_FAIL=true ;;
    infra-inspect-fails) POD_INFRA_INSPECT_FAIL=true ;;
    empty-infra) KEEPER_INFRA_ID="" ;;
  esac
  (worker_egress_teardown alpha) >"$CASE_DIR/teardown.out" 2>&1 || rc=$?
  if [[ "$expect" == success ]]; then
    if [[ "$rc" -eq 0 && ! -e "$RUNTIME_DIR/keeper" && ! -e "$RUNTIME_DIR/network" ]]; then
      pass "egress teardown with $label succeeds and removes keeper and network"
    else
      fail "egress teardown with $label failed: $(command cat "$CASE_DIR/teardown.out")"
    fi
    return
  fi
  [[ "$rc" -ne 0 ]] || fail "egress teardown succeeded with $label"
  if grep -q 'nft:alpha flush chain' "$CALL_LOG"; then
    fail "egress teardown flushed with $label"
  elif grep -Fq "$expect" "$CASE_DIR/teardown.out"; then
    pass "egress teardown refuses $label before flushing: $expect"
  else
    fail "egress teardown with $label gave the wrong reason: $(command cat "$CASE_DIR/teardown.out")"
  fi
  [[ -f "$RUNTIME_DIR/keeper" && -f "$RUNTIME_DIR/chains" && -f "$RUNTIME_DIR/network" ]] \
    || fail "refused teardown with $label discarded live egress state"
}

run_rootful_docker_case() {
  local missing_network="$1" rc=0
  setup_case openrc false
  FAKE_RUNTIME=docker
  CONTAINER_RT=docker
  MT_ROOTLESS=false
  NETWORK_RM_MISSING="$missing_network"
  command rm -f -- "$RUNTIME_DIR/keeper"
  (remove_tenant alpha false) >"$CASE_DIR/remove.out" 2>&1 || rc=$?
  [[ "$rc" -eq 0 ]] || {
    fail "rootful Docker remove-tenant failed: $(command cat "$CASE_DIR/remove.out")"
    return
  }
  pass "rootful Docker remove-tenant succeeds with missing_network=$missing_network"
  assert_removals_before_flush
  assert_no_container_left
  assert_no_unit_left
}

run_rootful_docker_attached_case() {
  local rc=0 unexpected="docker-leftover"
  setup_case openrc false
  FAKE_RUNTIME=docker
  CONTAINER_RT=docker
  MT_ROOTLESS=false
  command rm -f -- "$RUNTIME_DIR/keeper"
  printf 'running\n' >"$(_container_file "$unexpected")"
  (remove_tenant alpha false) >"$CASE_DIR/remove.out" 2>&1 || rc=$?
  [[ "$rc" -ne 0 ]] || fail 'rootful Docker removed a tenant with an unmanaged attachment'
  if grep -Fq "refusing teardown: $unexpected" "$CASE_DIR/remove.out"; then
    pass 'rootful Docker refusal names the unmanaged attachment'
  else
    fail "rootful Docker refusal gave the wrong reason: $(command cat "$CASE_DIR/remove.out")"
  fi
  if grep -q 'nft:alpha flush chain' "$CALL_LOG"; then
    fail 'rootful Docker flushed egress with an unmanaged attachment'
  else
    pass 'rootful Docker attachment refusal happens before flushing'
  fi
  [[ -f "$(_container_file "$unexpected")" && -f "$RUNTIME_DIR/network" \
    && -f "$RUNTIME_DIR/chains" && -f "$WORKER_EGRESS_DIR/alpha.conf" ]] \
    || fail 'rootful Docker refusal did not preserve attachment and egress state'
}

run_no_keeper_case() {
  local attached="$1" rc=0 unexpected="rootless-leftover" path
  setup_case openrc false
  command rm -f -- "$RUNTIME_DIR/keeper"
  for path in "$RUNTIME_DIR"/containers/*; do command rm -f -- "$path"; done
  if [[ "$attached" == true ]]; then
    printf 'running\n' >"$(_container_file "$unexpected")"
  fi
  (worker_egress_teardown alpha) >"$CASE_DIR/teardown.out" 2>&1 || rc=$?
  if [[ "$attached" == false ]]; then
    if [[ "$rc" -eq 0 && ! -e "$RUNTIME_DIR/network" && ! -e "$RUNTIME_DIR/chains" \
      && ! -e "$WORKER_EGRESS_DIR/alpha.conf" ]]; then
      pass 'rootless Podman without a keeper or attachments tears down cleanly'
    else
      fail "rootless Podman no-keeper teardown failed: $(command cat "$CASE_DIR/teardown.out")"
    fi
    return
  fi
  [[ "$rc" -ne 0 ]] || fail 'rootless Podman without a keeper ignored an attachment'
  if grep -Fq "refusing teardown: $unexpected" "$CASE_DIR/teardown.out"; then
    pass 'rootless Podman without a keeper refuses and names its attachment'
  else
    fail "rootless Podman no-keeper refusal gave the wrong reason: $(command cat "$CASE_DIR/teardown.out")"
  fi
  if grep -q 'nft:alpha flush chain' "$CALL_LOG"; then
    fail 'rootless Podman without a keeper flushed with an attachment'
  else
    pass 'rootless Podman no-keeper attachment refusal happens before flushing'
  fi
  [[ -f "$RUNTIME_DIR/network" && -f "$RUNTIME_DIR/chains" \
    && -f "$WORKER_EGRESS_DIR/alpha.conf" && ! -e "$RUNTIME_DIR/keeper" ]] \
    || fail 'rootless Podman no-keeper refusal did not preserve egress state'
}

for init in openrc systemd; do
  run_success_case "$init" false
  run_success_case "$init" true
  run_missing_account_stop_case "$init"
  run_missing_account_case "$init" false false
  run_missing_account_case "$init" true false
  run_missing_account_case "$init" false true
  run_missing_account_case "$init" true true
  run_missing_account_refusal_case "$init" live-uid 'entries of the former uid 61000'
  run_missing_account_refusal_case "$init" live-subuid-first 'still run (4245)'
  run_missing_account_refusal_case "$init" no-record 'no uid was recorded for it'
  run_stop_failure_case "$init"
  run_remove_failure_case "$init"
done
run_volume_failure_case
run_identity_record_case
run_rootful_missing_account_stop_case
run_rootful_missing_account_case
run_missing_account_refusal_case openrc live-effective-uid 'still run (4243)'
run_missing_account_refusal_case openrc live-fs-uid 'still run (4244)'
run_missing_account_refusal_case openrc live-saved-uid 'still run (4247)'
run_missing_account_container_removal_refusal openrc
run_missing_account_container_removal_refusal systemd
run_missing_account_refusal_case openrc live-subuid-last 'still run (4246)'
run_missing_account_refusal_case openrc no-subuid 'no subordinate uid range recorded'
run_missing_account_refusal_case openrc bad-record 'unexpected line in'
run_missing_account_refusal_case openrc system-uid 'no valid tenant uid recorded'
run_missing_account_refusal_case openrc held-uid "now belongs to account 'bob'"
run_missing_account_refusal_case openrc empty-proc 'cannot read the process table'
run_missing_account_refusal_case openrc linked-record 'is a symbolic link; refusing to trust it'
run_missing_account_refusal_case openrc bad-subuid "invalid subordinate uid range '231072:x'"
run_missing_account_refusal_case openrc two-uids 'must hold exactly one UID= line, it holds 2'
run_missing_account_refusal_case openrc crlf-record 'carriage return in'
run_missing_account_refusal_case openrc unreadable-live-status 'proc/4500/status of a live task'
run_missing_account_refusal_case openrc thread-only 'still run (5000/task/5001)'
run_missing_account_refusal_case systemd thread-only 'still run (5000/task/5001)'
run_missing_account_refusal_case openrc nss-failure 'the account lookup failed without a not-found answer'
run_exited_task_case
run_orphan_home_warning_case
run_missing_account_teardown_refusal
run_rootful_docker_case false
run_rootful_docker_case true
run_rootful_docker_attached_case
run_no_keeper_case true
run_no_keeper_case false
run_attached_teardown_refusal false
run_attached_teardown_refusal true
run_keeper_identity_case keeper-only success
run_keeper_identity_case lookalike "refusing teardown: $KEEPER_INFRA_NAME"
run_keeper_identity_case foreign-pod 'refusing to remove foreign keeper pod lw-egress-keeper-alpha'
run_keeper_identity_case foreign-off-network 'refusing to remove foreign keeper pod lw-egress-keeper-alpha'
run_keeper_identity_case extra-container 'unexpected container count 2'
run_keeper_identity_case pod-exists-error 'cannot check namespace keeper lw-egress-keeper-alpha'
run_keeper_identity_case inspect-fails 'cannot identify the namespace keeper'
run_keeper_identity_case count-inspect-fails 'cannot identify the namespace keeper'
run_keeper_identity_case infra-inspect-fails 'cannot identify the namespace keeper'
run_keeper_identity_case empty-infra 'cannot identify the namespace keeper'

if ((failures > 0)); then
  printf '\n%d failure(s)\n' "$failures" >&2
  exit 1
fi
printf '\nremove-tenant harness: all checks passed\n'
