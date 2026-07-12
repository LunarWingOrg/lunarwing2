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
