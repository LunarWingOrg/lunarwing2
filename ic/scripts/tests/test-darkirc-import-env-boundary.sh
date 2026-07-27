#!/usr/bin/env bash
# Regression coverage for credential-manifest parsing and tenant-side env merges.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
IMPORT_SRC="$SCRIPT_DIR/../import-tenant.sh"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

# Load only function definitions. import-tenant.sh intentionally has no public
# source mode, so stop before its command-line preconditions begin.
# shellcheck disable=SC1090
source <(sed '/^\[\[ -n "\$BUNDLE" \]\]/,$d' "$IMPORT_SRC")

TENANT=fixture
HOME_T="$TMP_ROOT/home"
LWROOT="$HOME_T/lunarwing"
ENVF="$LWROOT/env/lunarwing.env"
BRIDGE_ENVF="$LWROOT/env/xmpp-bridge.env"
VISION_ENVF="$LWROOT/env/vision.env"
mkdir -p "$LWROOT/env" "$LWROOT/state"
chmod 0700 "$HOME_T" "$LWROOT" "$LWROOT/env" "$LWROOT/state"
printf 'SECRETS_MASTER_KEY=old-master\nBASE_VALUE=preserved\n' >"$ENVF"
printf 'XMPP_JID=old@example.test\n' >"$BRIDGE_ENVF"
printf 'LUNARWING_AUTH_TOKEN=old-token\n' >"$VISION_ENVF"
chmod 0600 "$ENVF" "$BRIDGE_ENVF" "$VISION_ENVF"

# Run the tenant-side subprocess as this test user. The child still sees a real
# non-root EUID and exercises the same ownership/mode/link checks.
id() {
  if [[ "${1:-}" == -u && "${2:-}" == "$TENANT" ]]; then
    /usr/bin/id -u
  else
    /usr/bin/id "$@"
  fi
}
sudo() {
  [[ "${1:-}" == -u ]] || return 2
  shift 2
  command "$@"
}

validate_import_target_roots

marker="$TMP_ROOT/command-substitution-ran"
cat >"$TMP_ROOT/manifest-lunarwing.env" <<ENV
SECRETS_MASTER_KEY=new-master
LLM_MODEL=\$(touch $marker)
XMPP_ALLOW_FROM="Alice Example <alice@example.test>"
ENV
inject_keys "$TMP_ROOT/manifest-lunarwing.env" "$ENVF" "${IMPORT_LUNARWING_KEYS[@]}"
grep -qxF 'SECRETS_MASTER_KEY=new-master' "$ENVF"
grep -qxF "LLM_MODEL=\$(touch $marker)" "$ENVF"
grep -qxF 'XMPP_ALLOW_FROM="Alice Example <alice@example.test>"' "$ENVF"
grep -qxF 'BASE_VALUE=preserved' "$ENVF"
[[ ! -e "$marker" ]] || {
  printf 'manifest command substitution was evaluated\n' >&2
  exit 1
}
[[ "$INJECTED_MASTER_KEY_VERIFIED" == true ]] || {
  printf 'master-key verification did not complete inside the tenant helper\n' >&2
  exit 1
}

# Later bridge/vision merges must not erase the captured master-key proof.
printf 'XMPP_JID=new@example.test\n' >"$TMP_ROOT/manifest-bridge.env"
inject_keys "$TMP_ROOT/manifest-bridge.env" "$BRIDGE_ENVF" "${IMPORT_BRIDGE_KEYS[@]}"
[[ "$INJECTED_MASTER_KEY_VERIFIED" == true ]] || {
  printf 'later manifest merge erased master-key verification state\n' >&2
  exit 1
}

cat >"$TMP_ROOT/duplicate.env" <<'ENV'
LLM_MODEL=one
LLM_MODEL=two
ENV
if ( validate_dotenv_manifest "$TMP_ROOT/duplicate.env" "${IMPORT_LUNARWING_KEYS[@]}" ) >/dev/null 2>&1; then
  printf 'duplicate manifest key was accepted\n' >&2
  exit 1
fi

before_without_master="$(cat "$ENVF")"
printf 'LLM_MODEL=without-master\n' >"$TMP_ROOT/missing-master.env"
if ( inject_keys "$TMP_ROOT/missing-master.env" "$ENVF" "${IMPORT_LUNARWING_KEYS[@]}" ) >/dev/null 2>&1; then
  printf 'merge without a master key unexpectedly succeeded\n' >&2
  exit 1
fi
[[ "$(cat "$ENVF")" == "$before_without_master" ]] || {
  printf 'failed master-key verification mutated the target file\n' >&2
  exit 1
}
[[ -z "$(find "$LWROOT/env" -maxdepth 1 -name '.lunarwing.env.*' -print -quit)" ]]

ln "$ENVF" "$TMP_ROOT/shared-env"
if ( validate_import_target_roots ) >/dev/null 2>&1; then
  printf 'multiply-linked target env was accepted\n' >&2
  exit 1
fi
rm -f "$TMP_ROOT/shared-env"

chmod 0644 "$ENVF"
if ( validate_import_target_roots ) >/dev/null 2>&1; then
  printf 'permissive target env mode was accepted\n' >&2
  exit 1
fi
chmod 0600 "$ENVF"

sentinel="$TMP_ROOT/foreign-env"
printf 'SENTINEL\n' >"$sentinel"
rm -f "$ENVF"
ln -s "$sentinel" "$ENVF"
if ( validate_import_target_roots ) >/dev/null 2>&1; then
  printf 'final target env symlink was accepted\n' >&2
  exit 1
fi
[[ "$(cat "$sentinel")" == SENTINEL ]]
rm -f "$ENVF"
printf 'SECRETS_MASTER_KEY=new-master\n' >"$ENVF"
chmod 0600 "$ENVF"

foreign_state="$TMP_ROOT/foreign-state"
mkdir "$foreign_state"
rmdir "$LWROOT/state"
ln -s "$foreign_state" "$LWROOT/state"
if ( validate_import_target_roots ) >/dev/null 2>&1; then
  printf 'intermediate target state symlink was accepted\n' >&2
  exit 1
fi
[[ -z "$(find "$foreign_state" -mindepth 1 -print -quit)" ]]

printf 'ALL TESTS PASSED\n'
