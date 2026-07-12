#!/usr/bin/env bash
# Regression coverage for root-side DarkIRC environment-file boundaries.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

# shellcheck source=../lunarwing-mt-admin.sh
export LUNARWING_MT_DARKIRC_LOCK_ROOT="$TMP_ROOT/env-lock-override"
source "$ADMIN_SCRIPT"
[[ "$DARKIRC_WRITER_LOCK_ROOT" == /run/lunarwing ]] || {
  printf 'DarkIRC writer lock root honored an environment override\n' >&2
  exit 1
}

TENANT="fixture"
INIT_SYSTEM="systemd"
ENV_DIR="$TMP_ROOT/env"
mkdir -p "$ENV_DIR"
chmod 700 "$ENV_DIR"

say() { :; }
chown() { :; }
tenant_exists_in_registry() { return 0; }
tenant_darkirc_enabled() { return 0; }
ensure_init_system() { :; }
_systemctl_user() { return 0; }
tenant_env_dir() { printf '%s\n' "$ENV_DIR"; }
ports_get() {
  case "$2" in
    darkirc_adapter) printf '21010\n' ;;
    darkirc_irc) printf '21011\n' ;;
    orchestrator) printf '21012\n' ;;
    *) return 1 ;;
  esac
}

# Execute the command that the production helper delegates to, while retaining
# an audit trail. The current implementation's direct root reads will not add a
# read-side delegation entry, which is the regression this test catches.
SUDO_LOG="$TMP_ROOT/sudo.log"
sudo() {
  printf '%s\n' "$*" >>"$SUDO_LOG"
  if [[ "${1:-}" == -u ]]; then
    shift 2
  fi
  if [[ "${1:-}" == env ]]; then
    shift
    while [[ "${1:-}" == *=* ]]; do
      export "$1"
      shift
    done
  fi
  "$@"
}

assert_tenant_read_delegated() {
  if ! grep -Fq 'DARKIRC_ADAPTER_SECRET' "$SUDO_LOG" 2>/dev/null; then
    printf 'DarkIRC secret read was not delegated to the tenant identity\n' >&2
    exit 1
  fi
}

cat >"$ENV_DIR/lunarwing.env" <<'ENV'
DARKIRC_ADAPTER_SECRET=stable-secret
ENV
chmod 600 "$ENV_DIR/lunarwing.env"

# The adapter writer must obtain the secret through a tenant-side command, not
# by opening the tenant env file as root.
DARKIRC_WRITER_LOCK_HELD=true
adapter_rc=0
write_tenant_darkirc_adapter_env "$TENANT" >/dev/null || adapter_rc=$?
[[ "$adapter_rc" -eq 0 ]]
assert_tenant_read_delegated

# The strict health path has the same requirement before it sends the bearer to
# curl. The secret must not be present in curl's argv.
: >"$SUDO_LOG"
curl() {
  for arg in "$@"; do
    [[ "$arg" != *stable-secret* ]] || {
      printf 'adapter secret leaked into curl argv\n' >&2
      return 1
    }
  done
  cat >/dev/null
  printf '{"status":"ok","irc_connected":true}\n'
}
darkirc_health_strict "$TENANT" 1 true
assert_tenant_read_delegated

# Legitimate patch-env use must remain additive and idempotent while all file
# access is delegated to the tenant-side atomic helper.
tenant_darkirc_enabled() { return 1; }
_owner_scope_needs_migration() { return 1; }
ensure_external_worker_config() { :; }
ports_get() {
  case "$2" in
    orchestrator) printf '21012\n' ;;
    nanocode_wss) printf '21013\n' ;;
    pebble_wss) printf '21014\n' ;;
    opencode_wss) printf '21015\n' ;;
    weechat_adapter) printf '21016\n' ;;
    weechat) printf '21017\n' ;;
    *) return 1 ;;
  esac
}
printf 'BASE_VALUE=preserved\n' >"$ENV_DIR/lunarwing.env"
chmod 600 "$ENV_DIR/lunarwing.env"
patch_tenant_env "$TENANT" >/dev/null
patch_tenant_env "$TENANT" >/dev/null
grep -qx 'BASE_VALUE=preserved' "$ENV_DIR/lunarwing.env"
[[ "$(grep -c '^LUNARWING_OWNER_ID=fixture$' "$ENV_DIR/lunarwing.env")" -eq 1 ]]
[[ "$(grep -c '^ORCHESTRATOR_PORT=21012$' "$ENV_DIR/lunarwing.env")" -eq 1 ]]
[[ "$(stat -c '%a' "$ENV_DIR/lunarwing.env")" == 600 ]]

# Shared hard links and permissive secret-file modes are rejected even when the
# final path itself is a regular file.
ln "$ENV_DIR/lunarwing.env" "$TMP_ROOT/hardlink"
if ( patch_tenant_env "$TENANT" ) >/dev/null 2>&1; then
  printf 'patch-env accepted a multiply-linked env file\n' >&2
  exit 1
fi
rm -f "$TMP_ROOT/hardlink"
chmod 0644 "$ENV_DIR/lunarwing.env"
if ( patch_tenant_env "$TENANT" ) >/dev/null 2>&1; then
  printf 'patch-env accepted a permissive env-file mode\n' >&2
  exit 1
fi
chmod 0600 "$ENV_DIR/lunarwing.env"

# A final env-file symlink must be rejected before patch-env can append through
# it. The sentinel represents another tenant's file.
sentinel="$TMP_ROOT/foreign-env"
printf 'SENTINEL\n' >"$sentinel"
rm -f "$ENV_DIR/lunarwing.env"
ln -s "$sentinel" "$ENV_DIR/lunarwing.env"
if ( patch_tenant_env "$TENANT" ) >/dev/null 2>&1; then
  printf 'patch-env accepted a final env-file symlink\n' >&2
  exit 1
fi
[[ "$(cat "$sentinel")" == SENTINEL ]]

# An environment override must not let root chmod or create a lock below an
# insecure directory. This catches the pre-validation chmod in the old path.
unsafe_lock_root="$TMP_ROOT/unsafe-lock-root"
mkdir -p "$unsafe_lock_root"
chmod 0777 "$unsafe_lock_root"
DARKIRC_WRITER_LOCK_ROOT="$unsafe_lock_root"
DARKIRC_WRITER_LOCK_HELD=false
darkirc_scope_id() { printf '00112233445566778899aabbccddeeff\n'; }
require_cmd() { :; }
if ( darkirc_writer_lock "$TENANT" ) >/dev/null 2>&1; then
  printf 'world-writable lock root was accepted\n' >&2
  exit 1
fi
[[ "$(stat -c '%a' "$unsafe_lock_root")" == 777 ]]

printf 'ALL TESTS PASSED\n'
