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

DARKIRC_WRITER_LOCK_HELD="false"
REGISTRY_LOCKED=false
PORTS_LOCK_HELD=false
CONFIG_DIR_CHOWNED=false

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

tenant_state_dir() { printf '%s\n' "$STATE_ROOT"; }
tenant_log_dir() { printf '%s\n' "$CONFIG_ROOT/logs"; }
acquire_ports_lock() {
  [[ "$DARKIRC_WRITER_LOCK_HELD" == true ]] \
    || fail "registry lock acquired before DarkIRC writer lock"
  REGISTRY_LOCKED=true
  PORTS_LOCK_HELD=true
}
release_ports_lock() {
  REGISTRY_LOCKED=false
  PORTS_LOCK_HELD=false
}
darkirc_writer_lock() {
  [[ "$PORTS_LOCK_HELD" == false ]] \
    || fail "writer lock acquired while registry lock held"
  DARKIRC_WRITER_LOCK_HELD=true
}
darkirc_writer_unlock() {
  [[ "$REGISTRY_LOCKED" == false ]] \
    || fail "writer lock released before registry lock"
  DARKIRC_WRITER_LOCK_HELD=false
}
ports_get() {
  [[ "$REGISTRY_LOCKED" == true ]] \
    || fail "DarkIRC port read occurred outside registry lock"
  case "$2" in
    darkirc_irc) printf '21001\n' ;;
    darkirc_rpc) printf '21002\n' ;;
    *) return 1 ;;
  esac
}
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
  [[ "$REGISTRY_LOCKED" == true ]] || fail "scope read outside registry lock"
  printf '00112233445566778899aabbccddeeff\n'
}
run_darkirc_key_helper() {
  local _tenant="$1" operation="$2"
  shift 2
  [[ "$_tenant" == "$TENANT" && "$operation" == rewrite-baseline ]] \
    || fail "unexpected helper invocation"
  [[ "$DARKIRC_WRITER_LOCK_HELD" == true ]] \
    || fail "helper invoked without writer lock"
  [[ "$CONFIG_DIR_CHOWNED" == false ]] \
    || fail "root must not chown tenant DarkIRC config before helper invocation"
  [[ "$REGISTRY_LOCKED" == true ]] \
    || fail "DarkIRC helper commit occurred outside registry lock"

  local baseline existing_contacts
  baseline="$(cat)"
  existing_contacts="$(sed -n '/^\[contact\./,$p' "$CONFIG_DIR/darkirc_config.toml")"
  {
    printf '%s\n' "$baseline"
    printf '\noperator_extension = "must-survive"\n\n'
    printf '%s\n' "$existing_contacts"
  } >"$CONFIG_DIR/darkirc_config.toml"
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
[[ "$REGISTRY_LOCKED" == false ]] || fail "DarkIRC config generation leaked registry lock"
[[ "$DARKIRC_WRITER_LOCK_HELD" == false ]] || fail "DarkIRC config generation leaked writer lock"

grep -Fq 'irc_listen = "tcp://127.0.0.1:21001"' "$CONFIG_DIR/darkirc_config.toml"
grep -Fq 'operator_extension = "must-survive"' "$CONFIG_DIR/darkirc_config.toml"
grep -Fq '[contact."alice"]' "$CONFIG_DIR/darkirc_config.toml"
grep -Fq 'dm_chacha_public = "peer-public-alice"' "$CONFIG_DIR/darkirc_config.toml"
grep -Fq 'my_dm_chacha_secret = "local-secret-alice"' "$CONFIG_DIR/darkirc_config.toml"
grep -Fq 'legacy_field = "preserve-me"' "$CONFIG_DIR/darkirc_config.toml"

[[ "$(stat -c '%a' "$CONFIG_DIR/darkirc_config.toml")" == 600 ]]
[[ "$(stat -c '%a' "$CONFIG_DIR")" == 700 ]]

if grep -Eq 'render_template[[:space:]]+.*darkirc_config\.toml' "$ADMIN_SCRIPT"; then
  fail "direct DarkIRC template writer reintroduced"
fi

printf 'ALL TESTS PASSED\n'
