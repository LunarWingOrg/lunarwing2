#!/bin/bash
# Load a tenant-owned KEY=value file after OpenRC has applied command_user.
# Values are exported literally; this file never sources or evaluates them.
set -euo pipefail

fail() {
  printf 'error: unsafe tenant service environment\n' >&2
  exit 78
}

[[ "$EUID" -ne 0 ]] || fail
[[ "${1:-}" == --env-file && -n "${2:-}" ]] || fail
env_file="$2"
shift 2
[[ "${1:-}" == -- ]] || fail
shift
[[ $# -gt 0 && "$1" == /* && -x "$1" ]] || fail

[[ "$env_file" == /* && "$env_file" != */ ]] || fail
parent="${env_file%/*}"
[[ -n "$parent" ]] || parent="/"
[[ -d "$parent" && ! -L "$parent" ]] || fail
parent_owner="$(stat -c '%u' "$parent" 2>/dev/null || true)"
parent_mode="$(stat -c '%a' "$parent" 2>/dev/null || true)"
[[ "$parent_owner" == "$EUID" && "$parent_mode" == 700 ]] || fail

[[ -f "$env_file" && ! -L "$env_file" ]] || fail
owner="$(stat -c '%u' "$env_file" 2>/dev/null || true)"
mode="$(stat -c '%a' "$env_file" 2>/dev/null || true)"
links="$(stat -c '%h' "$env_file" 2>/dev/null || true)"
[[ "$owner" == "$EUID" && "$links" == 1 && "$mode" == 600 ]] || fail

# WI#505: never carry a descriptor for the mt-admin tenant lifecycle lock
# into the daemon. supervise-daemon starts this wrapper as a descendant of
# mt-admin's rc-service call, and bash's `exec {fd}>` has no close-on-exec,
# so an inherited fd would keep the tenant flock held for the daemon's whole
# lifetime (a die() inside the locked section then orphans the lock). Close
# every fd that resolves into the lock directory; a same-path fd opened
# without the lock is closed too, which no daemon needs.
lifecycle_lock_dir="${LUNARWING_MT_LIFECYCLE_LOCK_DIR:-/run/lock/lunarwing-tenants}"
if [[ -d "$lifecycle_lock_dir" ]]; then
  lock_dir_id="$(stat -Lc '%d:%i' "$lifecycle_lock_dir" 2>/dev/null || true)"
  if [[ -n "$lock_dir_id" ]]; then
    for fd_path in /proc/self/fd/*; do
      fd="${fd_path##*/}"
      [[ "$fd" =~ ^[0-9]+$ && "$fd" -ge 3 ]] || continue
      target="$(stat -Lc '%d:%i' "$fd_path" 2>/dev/null || true)"
      [[ -n "$target" && "$target" == "$lock_dir_id" ]] || continue
      eval "exec ${fd}>&-" 2>/dev/null || true
    done
  fi
fi
unset lifecycle_lock_dir lock_dir_id fd_path fd target

while IFS= read -r line || [[ -n "$line" ]]; do
  line="${line%$'\r'}"
  case "$line" in
    ''|'#'*) continue ;;
  esac
  [[ "$line" =~ ^[A-Za-z_][A-Za-z0-9_]*=.*$ ]] || fail
  name="${line%%=*}"
  value="${line#*=}"
  first="${value:0:1}"
  if [[ "$first" == '"' || "$first" == "'" ]]; then
    [[ "${#value}" -ge 2 && "${value: -1}" == "$first" ]] || fail
    value="${value:1:${#value}-2}"
  fi
  export "$name=$value"
done <"$env_file"

exec -- "$@"
