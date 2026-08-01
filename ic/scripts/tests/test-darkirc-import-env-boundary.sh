#!/usr/bin/env bash
# Credential manifests must merge atomically under tenant identity.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
IMPORT_SRC="$SCRIPT_DIR/../import-tenant.sh"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

# Load definitions only; stop before command-line preconditions.
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
inject_keys "$TMP_ROOT/manifest-lunarwing.env" "$ENVF" lunarwing
grep -qxF 'SECRETS_MASTER_KEY=new-master' "$ENVF"
grep -qxF "LLM_MODEL=\$(touch $marker)" "$ENVF"
grep -qxF 'BASE_VALUE=preserved' "$ENVF"
[[ ! -e "$marker" && "$INJECTED_MASTER_KEY_VERIFIED" == true ]]

printf 'XMPP_JID=new@example.test\n' >"$TMP_ROOT/manifest-bridge.env"
inject_keys "$TMP_ROOT/manifest-bridge.env" "$BRIDGE_ENVF" bridge
[[ "$INJECTED_MASTER_KEY_VERIFIED" == true ]]

before="$(cat "$ENVF")"
printf 'LLM_MODEL=without-master\n' >"$TMP_ROOT/missing-master.env"
if ( inject_keys "$TMP_ROOT/missing-master.env" "$ENVF" lunarwing ) >/dev/null 2>&1; then
  printf 'merge without master key succeeded\n' >&2
  exit 1
fi
[[ "$(cat "$ENVF")" == "$before" ]]
[[ -z "$(find "$LWROOT/env" -maxdepth 1 -name '.lunarwing.env.*' -print -quit)" ]]

ln "$ENVF" "$TMP_ROOT/shared-env"
if ( validate_import_target_roots ) >/dev/null 2>&1; then
  printf 'multiply-linked target env accepted\n' >&2
  exit 1
fi
rm -f "$TMP_ROOT/shared-env"

chmod 0644 "$ENVF"
if ( validate_import_target_roots ) >/dev/null 2>&1; then
  printf 'permissive target env accepted\n' >&2
  exit 1
fi
chmod 0600 "$ENVF"

sentinel="$TMP_ROOT/foreign-env"
printf 'SENTINEL\n' >"$sentinel"
rm -f "$ENVF"
ln -s "$sentinel" "$ENVF"
if ( validate_import_target_roots ) >/dev/null 2>&1; then
  printf 'target env symlink accepted\n' >&2
  exit 1
fi
[[ "$(cat "$sentinel")" == SENTINEL ]]

printf 'ALL TESTS PASSED\n'
