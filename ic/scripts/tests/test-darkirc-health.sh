#!/usr/bin/env bash
# Focused contract test for the strict DarkIRC activation gate.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

source "$ADMIN_SCRIPT"

TENANT="fixture"
INIT_SYSTEM="systemd"
tenant_exists_in_registry() { return 0; }
tenant_darkirc_enabled() { return 0; }
ensure_init_system() { :; }
_systemctl_user() { return 0; }
ports_get() {
  [[ "$2" == darkirc_adapter ]] && { printf '21010\n'; return 0; }
  return 1
}
tenant_env_dir() { printf '%s\n' "$TMP_ROOT"; }
sudo() {
  [[ "${1:-}" == -u ]] && shift 2
  if [[ "${1:-}" == env ]]; then
    shift
    while [[ "${1:-}" == *=* ]]; do
      export "$1"
      shift
    done
  fi
  "$@"
}
curl() {
  # The test intentionally does not accept a bearer value in argv.
  for arg in "$@"; do
    [[ "$arg" != *stable-secret* ]] || {
      printf 'adapter secret leaked into curl argv\n' >&2
      return 1
    }
  done
  cat >/dev/null
  printf '{"status":"ok","irc_connected":true}\n'
}
cat >"$TMP_ROOT/lunarwing.env" <<'ENV'
DARKIRC_ADAPTER_SECRET=stable-secret
ENV
chmod 600 "$TMP_ROOT/lunarwing.env"

darkirc_health_strict "$TENANT"

INIT_SYSTEM="openrc"
tenant_writer_services() { printf 'lunarwing-darkirc-%s\n' "$TENANT"; }
rc-service() {
  printf 'inactive\n'
  return 3
}
if tenant_writers_active "$TENANT"; then
  printf 'OpenRC inactive status was misclassified as active\n' >&2
  exit 1
fi
printf 'ALL TESTS PASSED\n'
