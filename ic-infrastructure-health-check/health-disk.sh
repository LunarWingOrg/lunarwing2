#!/usr/bin/env bash
# Health Check: host and tenant filesystem capacity (blocks and inodes).
# Read-only: this component never invokes container-runtime cleanup.
set -uo pipefail

DEGRADED_PCT="${HEALTH_DISK_DEGRADED_USED_PCT:-80}"
CRITICAL_PCT="${HEALTH_DISK_CRITICAL_USED_PCT:-90}"
DEGRADED_FREE_GIB="${HEALTH_DISK_DEGRADED_FREE_GIB:-5}"
CRITICAL_FREE_GIB="${HEALTH_DISK_CRITICAL_FREE_GIB:-2}"
PROBE_TIMEOUT="${HEALTH_DISK_TIMEOUT:-3}"
SWEEP_DEADLINE="${HEALTH_DISK_SWEEP_DEADLINE_SECS:-19}"

validate_integer() {
  local name="$1" fallback="$2"
  local value="${!name}"
  if ! [[ "$value" =~ ^[0-9]+$ ]]; then
    printf 'WARNING: invalid %s=%q; using %s\n' "$name" "$value" "$fallback" >&2
    value="$fallback"
  fi
  printf -v "$name" '%d' "$((10#$value))"
}
validate_integer DEGRADED_PCT 80
validate_integer CRITICAL_PCT 90
validate_integer DEGRADED_FREE_GIB 5
validate_integer CRITICAL_FREE_GIB 2
validate_integer PROBE_TIMEOUT 3
validate_integer SWEEP_DEADLINE 19
if ((PROBE_TIMEOUT == 0)); then
  printf 'WARNING: HEALTH_DISK_TIMEOUT must be positive; using 3\n' >&2
  PROBE_TIMEOUT=3
fi
if ((DEGRADED_PCT > CRITICAL_PCT)); then
  printf 'WARNING: degraded used percentage exceeds critical; using 80 and 90\n' >&2
  DEGRADED_PCT=80
  CRITICAL_PCT=90
fi
if ((DEGRADED_FREE_GIB < CRITICAL_FREE_GIB)); then
  printf 'WARNING: degraded free-space floor is below critical; using 5 and 2\n' >&2
  DEGRADED_FREE_GIB=5
  CRITICAL_FREE_GIB=2
fi

DEGRADED_FREE_BYTES=$((DEGRADED_FREE_GIB * 1024 * 1024 * 1024))
CRITICAL_FREE_BYTES=$((CRITICAL_FREE_GIB * 1024 * 1024 * 1024))
BASE_DIR="${LUNARWING_BASE_DIR:-${HEALTH_BASE_DIR:-$HOME/.lunarwing}}"
BASE_PATH_KIND=strict
if [[ -z "${LUNARWING_BASE_DIR:-}" && -z "${HEALTH_BASE_DIR:-}" ]]; then
  BASE_PATH_KIND=discovered
fi
TENANTS_FILE="${SELF_HEAL_TENANTS_FILE:-${LUNARWING_TENANTS_FILE:-/etc/lunarwing/ports.json}}"
PODMAN_ROOT="${HEALTH_DISK_PODMAN_ROOT:-/var/lib/containers/storage}"
DOCKER_ROOT="${HEALTH_DISK_DOCKER_ROOT:-/var/lib/docker}"
RUNTIME="${HEALTH_DISK_RUNTIME:-${HEALTH_WORKER_EGRESS_RUNTIME:-auto}}"
SECONDS=0

issues=()
filesystems=()
overall_rc=0
sweep_incomplete=false
declare -A SEEN_PATHS=()
declare -A FS_PATHS FS_MOUNT FS_SIZE FS_FREE FS_USED FS_INODES FS_STATUS FS_AVAILABLE FS_PRESENT

json_objects() {
  if [[ $# -eq 0 ]]; then
    printf '[]\n'
  else
    printf '%s\n' "$@" | jq -s .
  fi
}

json_issues() {
  if [[ $# -eq 0 ]]; then
    printf '[]\n'
  else
    printf '%s\n' "$@" | jq -R . | jq -s .
  fi
}

report() {
  local status=healthy
  case "$overall_rc" in
    1) status=degraded ;;
    2) status=critical ;;
  esac
  jq -n \
    --arg status "$status" \
    --arg timestamp "$(date -u +'%Y-%m-%dT%H:%M:%SZ')" \
    --argjson filesystems "$(json_objects "${filesystems[@]}")" \
    --argjson issues "$(json_issues "${issues[@]}")" \
    '{component:"disk", status:$status, timestamp:$timestamp,
      metrics:{filesystems:$filesystems}, issues:$issues}'
  exit "$overall_rc"
}

if ! command -v jq >/dev/null 2>&1; then
  printf '{"component":"disk","status":"degraded","timestamp":"%s","metrics":{"filesystems":[]},"issues":["jq is unavailable"]}\n' \
    "$(date -u +'%Y-%m-%dT%H:%M:%SZ')"
  exit 1
fi
if ! command -v timeout >/dev/null 2>&1; then
  issues+=("timeout is unavailable; bounded filesystem probes cannot run")
  overall_rc=1
  report
fi

bounded() {
  local budget="$PROBE_TIMEOUT" remaining
  if ((SWEEP_DEADLINE > 0)); then
    remaining=$((SWEEP_DEADLINE - SECONDS))
    ((remaining > 0)) || return 124
    ((remaining < budget)) && budget="$remaining"
  fi
  timeout -k 1 "$budget" "$@"
}

raise_overall() {
  local rc="$1"
  ((rc > overall_rc)) && overall_rc="$rc"
  return 0
}

degrade_discovery() {
  issues+=("$1")
  raise_overall 1
}

append_fs_path() {
  local key="$1" path="$2"
  FS_PATHS[$key]="$(jq -c --arg path "$path" '. + [$path]' <<<"${FS_PATHS[$key]:-[]}")"
}

mark_unavailable() {
  local key="$1" mount="$2" reason="$3"
  FS_MOUNT[$key]="$mount"
  FS_SIZE[$key]=0
  FS_FREE[$key]=0
  FS_USED[$key]=0
  FS_INODES[$key]=null
  FS_STATUS[$key]=degraded
  FS_AVAILABLE[$key]=false
  FS_PRESENT[$key]="${FS_PRESENT[$key]:-null}"
  issues+=("disk@$mount degraded: $reason")
  raise_overall 1
}

mark_absent() {
  local key="$1" mount="$2"
  FS_MOUNT[$key]="$mount"
  FS_SIZE[$key]=0
  FS_FREE[$key]=0
  FS_USED[$key]=0
  FS_INODES[$key]=null
  FS_STATUS[$key]=healthy
  FS_AVAILABLE[$key]=false
  FS_PRESENT[$key]=false
}

evaluate_capacity() {
  local key="$1" mount="$2" used="$3" free="$4" inode_used="$5"
  local rc=0
  if ((10#$used >= CRITICAL_PCT || 10#$free < CRITICAL_FREE_BYTES)); then
    rc=2
  elif ((10#$used >= DEGRADED_PCT || 10#$free < DEGRADED_FREE_BYTES)); then
    rc=1
  fi
  if [[ "$inode_used" != "-" ]]; then
    if ((10#$inode_used >= CRITICAL_PCT)); then
      rc=2
    elif ((10#$inode_used >= DEGRADED_PCT && rc < 1)); then
      rc=1
    fi
  fi
  case "$rc" in
    2) FS_STATUS[$key]=critical ;;
    1) FS_STATUS[$key]=degraded ;;
    *) FS_STATUS[$key]=healthy ;;
  esac
  if ((rc > 0)); then
    if [[ "$inode_used" == "-" ]]; then
      issues+=("disk@$mount ${FS_STATUS[$key]}: $used% blocks used, inodes not tracked, $free bytes free")
    else
      issues+=("disk@$mount ${FS_STATUS[$key]}: $used% blocks and $inode_used% inodes used, $free bytes free")
    fi
    raise_overall "$rc"
  fi
}

observe_path() {
  local path="$1" kind="${2:-strict}"
  local device key output line size free used mount inode_output inode_used inode_total
  local stat_output stat_rc
  [[ -n "$path" && -z "${SEEN_PATHS[$path]:-}" ]] || return 0
  if ((SWEEP_DEADLINE > 0 && SECONDS >= SWEEP_DEADLINE)); then
    sweep_incomplete=true
    return 0
  fi
  SEEN_PATHS[$path]=1

  stat_output="$(bounded env LC_ALL=C stat -L -c %d -- "$path" 2>&1)"
  stat_rc=$?
  if ((stat_rc != 0)) || ! [[ "$stat_output" =~ ^[0-9]+$ ]]; then
    device="$stat_output"
    key="path:$path"
    append_fs_path "$key" "$path"
    if [[ "$kind" == discovered && "$stat_rc" -ne 124 && "$stat_rc" -ne 137 \
      && "$device" == *"No such file or directory"* ]]; then
      mark_absent "$key" "$path"
    else
      mark_unavailable "$key" "$path" "path cannot be stat'ed"
    fi
    return 0
  fi
  device="$stat_output"

  key="device:$device"
  append_fs_path "$key" "$path"
  if [[ -n "${FS_MOUNT[$key]:-}" ]]; then
    return 0
  fi
  FS_PRESENT[$key]=true

  if ! output="$(bounded df -B1 --output=size,avail,pcent,target -- "$path" 2>/dev/null)"; then
    mark_unavailable "$key" "$path" "block df failed or timed out"
    return 0
  fi
  line="${output##*$'\n'}"
  read -r size free used mount <<<"$line"
  used="${used%\%}"
  if ! [[ "$size" =~ ^[0-9]+$ && "$free" =~ ^[0-9]+$ && "$used" =~ ^[0-9]+$ ]] || [[ -z "$mount" ]]; then
    mark_unavailable "$key" "$path" "block df returned an unrecognized result"
    return 0
  fi

  FS_MOUNT[$key]="$mount"
  FS_SIZE[$key]="$size"
  FS_FREE[$key]="$free"
  FS_USED[$key]="$used"
  FS_AVAILABLE[$key]=true
  if ! inode_output="$(bounded df --output=itotal,iavail,ipcent,target -- "$path" 2>/dev/null)"; then
    FS_INODES[$key]=null
    FS_STATUS[$key]=degraded
    FS_AVAILABLE[$key]=false
    issues+=("disk@$mount degraded: inode df failed or timed out; $used% blocks used, $free bytes free")
    raise_overall 1
    return 0
  fi
  line="${inode_output##*$'\n'}"
  read -r inode_total _ inode_used _ <<<"$line"
  inode_used="${inode_used%\%}"
  if [[ "$inode_total" == "0" || "$inode_used" == "-" ]]; then
    FS_INODES[$key]=null
    evaluate_capacity "$key" "$mount" "$used" "$free" "-"
    return 0
  fi
  if ! [[ "$inode_total" =~ ^[0-9]+$ && "$inode_used" =~ ^[0-9]+$ ]]; then
    FS_INODES[$key]=null
    FS_STATUS[$key]=degraded
    FS_AVAILABLE[$key]=false
    issues+=("disk@$mount degraded: inode df returned an unrecognized result; $used% blocks used, $free bytes free")
    raise_overall 1
    return 0
  fi
  FS_INODES[$key]="$inode_used"
  evaluate_capacity "$key" "$mount" "$used" "$free" "$inode_used"
}

add_runtime_paths() {
  case "$RUNTIME" in
    auto)
      command -v podman >/dev/null 2>&1 && observe_path "$PODMAN_ROOT" discovered
      command -v docker >/dev/null 2>&1 && observe_path "$DOCKER_ROOT" discovered
      ;;
    podman) observe_path "$PODMAN_ROOT" discovered ;;
    docker) observe_path "$DOCKER_ROOT" discovered ;;
    both)
      observe_path "$PODMAN_ROOT" discovered
      observe_path "$DOCKER_ROOT" discovered
      ;;
    none) ;;
    *)
      printf 'WARNING: invalid HEALTH_DISK_RUNTIME=%q; using auto\n' "$RUNTIME" >&2
      RUNTIME=auto
      add_runtime_paths
      ;;
  esac
}

discover_tenant_paths() {
  [[ -e "$TENANTS_FILE" ]] || return 0
  if [[ ! -r "$TENANTS_FILE" ]] || ! jq -e '
    type == "object" and (.tenants | type == "object")
    and ([.tenants | to_entries[] |
      (.value | type == "object")
      and (.value.user | type == "string")
      and (.value.user | length > 0)] | all)
  ' "$TENANTS_FILE" >/dev/null 2>&1; then
    degrade_discovery "tenant registry is unreadable or invalid; rootless container stores are not fully monitored"
    return 0
  fi

  local tenant user entry home
  while IFS=$'\t' read -r tenant user; do
    [[ -n "$tenant" && -n "$user" ]] || continue
    if ((SWEEP_DEADLINE > 0 && SECONDS >= SWEEP_DEADLINE)); then
      sweep_incomplete=true
      break
    fi
    if ! entry="$(bounded getent passwd "$user" 2>/dev/null)" || [[ -z "$entry" ]]; then
      degrade_discovery "tenant $tenant user $user has no resolvable home; rootless container store is not monitored"
      continue
    fi
    IFS=: read -r _ _ _ _ _ home _ <<<"$entry"
    if [[ -z "$home" ]]; then
      degrade_discovery "tenant $tenant user $user has an empty home; rootless container store is not monitored"
      continue
    fi
    observe_path "$home/.local/share/containers/storage" discovered
  done < <(jq -r '.tenants // {} | to_entries[] | [.key, .value.user] | @tsv' "$TENANTS_FILE" 2>/dev/null)
}

observe_path /
observe_path /tmp
observe_path /var/tmp
observe_path "$BASE_DIR" "$BASE_PATH_KIND"
add_runtime_paths
if [[ -n "${HEALTH_DISK_EXTRA_PATHS:-}" ]]; then
  read -r -a extra_paths <<<"$HEALTH_DISK_EXTRA_PATHS"
  for extra_path in "${extra_paths[@]}"; do
    observe_path "$extra_path"
  done
fi
discover_tenant_paths
if [[ "$sweep_incomplete" == true ]]; then
  degrade_discovery "disk sweep incomplete: deadline ${SWEEP_DEADLINE}s reached; results partial"
fi

fs_keys=()
if ((${#FS_PATHS[@]} > 0)); then
  mapfile -t fs_keys < <(printf '%s\n' "${!FS_PATHS[@]}" | sort)
fi
for key in "${fs_keys[@]}"; do
  filesystems+=("$(jq -n \
    --arg mount "${FS_MOUNT[$key]:-unknown}" \
    --arg status "${FS_STATUS[$key]:-degraded}" \
    --argjson paths "${FS_PATHS[$key]}" \
    --argjson size "${FS_SIZE[$key]:-0}" \
    --argjson free "${FS_FREE[$key]:-0}" \
    --argjson used "${FS_USED[$key]:-0}" \
    --argjson inodes "${FS_INODES[$key]:-null}" \
    --argjson available "${FS_AVAILABLE[$key]:-false}" \
    --argjson present "${FS_PRESENT[$key]:-null}" \
    '{mount:$mount, paths:$paths, size_bytes:$size, free_bytes:$free,
      used_pct:$used, inodes_used_pct:$inodes, status:$status,
      available:$available, present:$present}')")
done
report
