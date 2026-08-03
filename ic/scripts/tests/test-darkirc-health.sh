#!/usr/bin/env bash
# Strict DarkIRC health checks both init services and authenticated adapter state.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

# shellcheck source=../lunarwing-mt-admin.sh
source "$ADMIN_SCRIPT"

TENANT=fixture
INIT_SYSTEM=systemd
SYSTEMD_CALLS="$TMP_ROOT/systemd.calls"
OPENRC_CALLS="$TMP_ROOT/openrc.calls"
HEALTH_RESPONSE='{"status":"ok","irc_connected":true}'

tenant_exists_in_registry() { [[ "$1" == "$TENANT" ]]; }
tenant_darkirc_enabled() { [[ "$1" == "$TENANT" ]]; }
ensure_init_system() { :; }
_systemctl_user() {
  local argument
  for argument in "$@"; do printf '%s\n' "$argument" >>"$SYSTEMD_CALLS"; done
  printf '%s\n' END >>"$SYSTEMD_CALLS"
  return 0
}
ports_get() {
  [[ "$1" == "$TENANT" && "$2" == darkirc_adapter ]] || return 1
  printf '21010\n'
}
tenant_env_dir() { printf '%s\n' "$TMP_ROOT"; }
sudo() {
  [[ "${1:-}" == -u ]] && shift 2
  [[ "${1:-}" == env ]] && shift
  [[ "${1:-}" == -i ]] && shift
  while [[ "${1:-}" == *=* ]]; do export "$1"; shift; done
  "$@"
}
curl() {
  local argument header
  for argument in "$@"; do
    [[ "$argument" != *stable-secret* ]] || {
      printf 'adapter secret leaked into curl argv\n' >&2
      return 1
    }
  done
  IFS= read -r header
  [[ "$header" == 'Authorization: Bearer stable-secret' ]] || {
    printf 'adapter bearer was not supplied through private stdin\n' >&2
    return 1
  }
  printf '%s\n' "$HEALTH_RESPONSE"
}
rc-service() {
  printf '%s\n' "$*" >>"$OPENRC_CALLS"
  return 0
}

printf 'DARKIRC_ADAPTER_SECRET=stable-secret\n' >"$TMP_ROOT/lunarwing.env"
chmod 0600 "$TMP_ROOT/lunarwing.env"

darkirc_health_strict "$TENANT" 1 true
for unit in "lunarwing-darkirc-${TENANT}.service" "lunarwing-darkirc-adapter-${TENANT}.service"; do
  grep -Fqx -- "$unit" "$SYSTEMD_CALLS" || {
    printf 'systemd-user health omitted %s\n' "$unit" >&2
    exit 1
  }
done

INIT_SYSTEM=openrc
darkirc_health_strict "$TENANT" 1 true
for service in "lunarwing-darkirc-${TENANT} status" "lunarwing-darkirc-adapter-${TENANT} status"; do
  grep -Fqx -- "$service" "$OPENRC_CALLS" || {
    printf 'OpenRC health omitted %s\n' "$service" >&2
    exit 1
  }
done

HEALTH_RESPONSE='{"status":"ok","irc_connected":false}'
if darkirc_health_strict "$TENANT" 1 true >/dev/null 2>&1; then
  printf 'disconnected adapter passed strict health\n' >&2
  exit 1
fi

HEALTH_RESPONSE='{"status":"degraded","irc_connected":true}'
if darkirc_health_strict "$TENANT" 1 true >/dev/null 2>&1; then
  printf 'degraded adapter passed strict health\n' >&2
  exit 1
fi

rc-service() { return 1; }
HEALTH_RESPONSE='{"status":"ok","irc_connected":true}'
if darkirc_health_strict "$TENANT" 1 true >/dev/null 2>&1; then
  printf 'failed OpenRC service passed strict health\n' >&2
  exit 1
fi

printf 'ALL TESTS PASSED\n'
