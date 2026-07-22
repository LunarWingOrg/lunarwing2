#!/usr/bin/env bash
# Regression tests for contact-preserving DarkIRC config ownership.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

# shellcheck source=../lunarwing-mt-admin.sh
source "$ADMIN_SCRIPT"

TENANT="fixture"
CONFIG_ROOT="$TMP_ROOT/lunarwing"
STATE_ROOT="$CONFIG_ROOT/state"
CONFIG_DIR="$STATE_ROOT/darkirc"
mkdir -p "$CONFIG_DIR" "$CONFIG_ROOT/logs"
chmod 0700 "$CONFIG_DIR"

tenant_state_dir() { printf '%s\n' "$STATE_ROOT"; }
tenant_log_dir() { printf '%s\n' "$CONFIG_ROOT/logs"; }
REGISTRY_LOCKED=false
ports_registry_lock() {
  REGISTRY_LOCKED=true
  PORTS_REGISTRY_LOCK_FD=fixture
}
ports_registry_unlock() {
  REGISTRY_LOCKED=false
  PORTS_REGISTRY_LOCK_FD=""
}
ports_get() {
  [[ "$REGISTRY_LOCKED" == true ]] || {
    printf 'DarkIRC port read occurred outside the registry lock\n' >&2
    return 1
  }
  case "$2" in
    darkirc_irc) printf '21001\n' ;;
    darkirc_rpc) printf '21002\n' ;;
    *) return 1 ;;
  esac
}
CONFIG_DIR_CHOWNED=false
chown() {
  if [[ "$*" == *"$CONFIG_DIR"* ]]; then
    CONFIG_DIR_CHOWNED=true
  fi
}
sudo() {
  [[ "${1:-}" == -u ]] && shift 2
  [[ "${1:-}" == env ]] && shift
  while [[ "${1:-}" == *=* ]]; do shift; done
  "$@"
}
say() { :; }
ensure_darkirc_scope_id() {
  [[ "$REGISTRY_LOCKED" == true ]] || return 1
  printf '00112233445566778899aabbccddeeff\n'
}
run_darkirc_key_helper() {
  local _tenant="$1"
  shift
  [[ "$CONFIG_DIR_CHOWNED" == false ]] || {
    printf 'root must not chown the tenant DarkIRC config before helper invocation\n' >&2
    return 1
  }
  [[ "$REGISTRY_LOCKED" == true ]] || {
    printf 'DarkIRC helper commit occurred outside the registry lock\n' >&2
    return 1
  }
  local baseline
  baseline="$(cat)"
  cat >"$CONFIG_DIR/darkirc_config.toml" <<TOML
$baseline

operator_extension = "must-survive"

[contact."alice"]
dm_chacha_public = "peer-public-alice"
my_dm_chacha_secret = "local-secret-alice"
legacy_field = "preserve-me"
TOML
  chmod 0600 "$CONFIG_DIR/darkirc_config.toml"
}

cat >"$CONFIG_DIR/darkirc_config.toml" <<'TOML'
irc_listen = "tcp://127.0.0.1:old"
operator_extension = "must-survive"

[contact."alice"]
dm_chacha_public = "peer-public-alice"
my_dm_chacha_secret = "local-secret-alice"
legacy_field = "preserve-me"
TOML
chmod 0600 "$CONFIG_DIR/darkirc_config.toml"

generate_darkirc_config "$TENANT"
[[ "$REGISTRY_LOCKED" == false ]] || {
  printf 'DarkIRC config generation leaked the registry lock\n' >&2
  exit 1
}

grep -Fq 'irc_listen = "tcp://127.0.0.1:21001"' "$CONFIG_DIR/darkirc_config.toml"
grep -Fq 'operator_extension = "must-survive"' "$CONFIG_DIR/darkirc_config.toml"
grep -Fq '[contact."alice"]' "$CONFIG_DIR/darkirc_config.toml"
grep -Fq 'dm_chacha_public = "peer-public-alice"' "$CONFIG_DIR/darkirc_config.toml"
grep -Fq 'my_dm_chacha_secret = "local-secret-alice"' "$CONFIG_DIR/darkirc_config.toml"
grep -Fq 'legacy_field = "preserve-me"' "$CONFIG_DIR/darkirc_config.toml"

[[ "$(stat -c '%a' "$CONFIG_DIR/darkirc_config.toml")" == "600" ]]
[[ "$(stat -c '%a' "$CONFIG_DIR")" == "700" ]]

if rg -n 'render_template[[:space:]]+.*darkirc_config\.toml' "$ADMIN_SCRIPT" >/dev/null; then
  printf 'direct DarkIRC template writer reintroduced\n' >&2
  exit 1
fi

printf 'ALL TESTS PASSED\n'
