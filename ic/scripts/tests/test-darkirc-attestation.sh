#!/usr/bin/env bash
# Regression coverage for DarkIRC helper and daemon attestation boundaries.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

# shellcheck source=../lunarwing-mt-admin.sh
source "$ADMIN_SCRIPT"

unsafe_helper="$TMP_ROOT/lunarwing-darkirc-key-helper"
printf '#!/usr/bin/env bash\nexit 0\n' >"$unsafe_helper"
chmod 0777 "$unsafe_helper"
DEFAULT_DARKIRC_KEY_HELPER="$unsafe_helper"

selected="$(darkirc_key_helper_path fixture 2>/dev/null || true)"
if [[ "$selected" == "$unsafe_helper" ]]; then
  printf 'unsafe configured DarkIRC helper was selected\n' >&2
  exit 1
fi

chmod 4755 "$unsafe_helper"
selected="$(darkirc_key_helper_path fixture 2>/dev/null || true)"
if [[ "$selected" == "$unsafe_helper" ]]; then
  printf 'setuid DarkIRC helper was selected\n' >&2
  exit 1
fi

build_body="$(declare -f build_darkirc)"
if grep -Fq 'sudo -u' <<<"$build_body" \
   || grep -Fq 'tenant_' <<<"$build_body" \
   || grep -Fq 'DARKIRC_SOURCE' <<<"$build_body" \
   || grep -Fq 'SUDO_USER' <<<"$build_body"; then
  printf 'shared DarkIRC build still trusts tenant-controlled source configuration\n' >&2
  exit 1
fi
grep -Fq 'env -i' <<<"$build_body" || {
  printf 'shared DarkIRC build does not sanitize the privileged toolchain environment\n' >&2
  exit 1
}
grep -Fq 'DARKIRC_BUILD_ROOT' <<<"$build_body" || {
  printf 'shared DarkIRC build does not use a fixed root-controlled workspace\n' >&2
  exit 1
}
declare -F pin_darkirc_build_candidate >/dev/null || {
  printf 'shared DarkIRC build lacks a root-controlled candidate pin step\n' >&2
  exit 1
}
pin_body="$(declare -f pin_darkirc_build_candidate)"
grep -Fq 'cp --no-dereference' <<<"$pin_body" || {
  printf 'DarkIRC candidate pin follows a mutable source symlink\n' >&2
  exit 1
}
grep -Fq 'darkirc_validate_root_executable' <<<"$pin_body" || {
  printf 'DarkIRC candidate pin is not validated as a root-owned executable\n' >&2
  exit 1
}
pin_line="$(grep -n 'pin_darkirc_build_candidate' <<<"$build_body" | head -1 | cut -d: -f1 || true)"
validation_line="$(grep -n 'validate_darkirc_build_candidate' <<<"$build_body" | head -1 | cut -d: -f1 || true)"
install_line="$(grep -nE 'install .*DARKIRC_BIN|install_darkirc_binary_atomic' <<<"$build_body" | head -1 | cut -d: -f1 || true)"
[[ "$pin_line" =~ ^[0-9]+$ && "$validation_line" =~ ^[0-9]+$ && "$install_line" =~ ^[0-9]+$ ]] || {
  printf 'build-darkirc lacks an explicit pin/validate/install sequence\n' >&2
  exit 1
}
(( pin_line < validation_line && validation_line < install_line )) || {
  printf 'build-darkirc does not pin before validating and installing\n' >&2
  exit 1
}

tenant_build_body="$(declare -f build_tenant)"
if grep -Fq '$repo/target/$PROFILE/lunarwing-darkirc-key-helper' <<<"$tenant_build_body"; then
  printf 'build-tenant installs a root helper from the tenant-owned checkout\n' >&2
  exit 1
fi
declare -F install_trusted_darkirc_key_helper >/dev/null || {
  printf 'build-tenant lacks a trusted shared-helper install path\n' >&2
  exit 1
}
trusted_helper_body="$(declare -f install_trusted_darkirc_key_helper)"
grep -Fq 'cd "$REPO_ROOT"' <<<"$trusted_helper_body" || {
  printf 'trusted helper is not built from the admin checkout\n' >&2
  exit 1
}
grep -Fq 'cargo build --release -j6 --bin lunarwing-darkirc-key-helper' \
  <<<"$trusted_helper_body" || {
  printf 'trusted helper build is not a resource-bounded release build\n' >&2
  exit 1
}

ordinary_repo="$TMP_ROOT/ordinary-repo"
ordinary_env="$TMP_ROOT/ordinary-env"
helper_marker="$TMP_ROOT/helper-installed"
mkdir -p "$ordinary_repo" "$ordinary_env"
BUILD_LOCK="$TMP_ROOT/build.lock"
tenant_repo() { printf '%s' "$ordinary_repo"; }
tenant_env_dir() { printf '%s' "$ordinary_env"; }
tenant_darkirc_enabled() { return 1; }
install_trusted_darkirc_key_helper() { : >"$helper_marker"; }
sudo() { :; }
build_tenant ordinary false false false false false >/dev/null
if [[ -e "$helper_marker" ]]; then
  printf 'non-DarkIRC tenant build installed the shared DarkIRC helper\n' >&2
  exit 1
fi

printf 'ALL TESTS PASSED\n'
