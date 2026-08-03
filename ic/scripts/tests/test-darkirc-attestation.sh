#!/usr/bin/env bash
# Regression coverage for DarkIRC helper and daemon attestation boundaries.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$SCRIPT_DIR/../lunarwing-mt-admin.sh"

# shellcheck source=../lunarwing-mt-admin.sh
source "$ADMIN_SCRIPT"

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

require_body() {
  local body="$1" needle="$2" message="$3"
  grep -Fq -- "$needle" <<<"$body" || fail "$message"
}

build_body="$(declare -f build_darkirc)"
helper_body="$(declare -f install_darkirc_key_helper)"
compat_body="$(declare -f write_darkirc_compatibility)"
load_body="$(declare -f darkirc_load_compatibility)"
checkout_body="$(declare -f darkirc_validate_clean_root_checkout)"
executable_body="$(declare -f darkirc_validate_root_executable)"

require_body "$checkout_body" 'darkirc_validate_root_owned_directory "$path"' \
  "trusted checkout is not rooted in an owner-checked directory"
require_body "$checkout_body" 'status --porcelain --untracked-files=all' \
  "trusted checkout does not reject dirty or untracked files"
require_body "$checkout_body" "-type l -o ! -user root -o -perm /022" \
  "trusted Git metadata is not checked for links, ownership, and writability"
require_body "$checkout_body" "ls-tree -r" \
  "trusted checkout does not reject symlinks or gitlinks"

for body in "$build_body" "$helper_body"; do
  require_body "$body" 'darkirc_validate_clean_root_checkout' \
    "build path does not validate its root-owned source checkout"
  require_body "$body" 'git -C' \
    "build path does not create a pinned Git archive"
  require_body "$body" 'archive --format=tar' \
    "build path does not use a pinned source archive"
  require_body "$body" 'setpriv --reuid=' \
    "build path does not drop to the dedicated build user"
  require_body "$body" '--clear-groups --no-new-privs' \
    "build path does not clear groups and set no-new-privs"
  require_body "$body" 'env -i HOME=' \
    "build path does not sanitize its environment"
  require_body "$body" 'taskset -c 0-1' \
    "build path is not CPU bounded"
done

require_body "$helper_body" 'cargo build --manifest-path' \
  "key helper is not built from the archived trusted manifest"
require_body "$helper_body" '--release -j2 --bin lunarwing-darkirc-key-helper' \
  "key helper build is not release-only and resource bounded"
require_body "$helper_body" 'darkirc_validate_build_artifact "$artifact"' \
  "key helper artifact ownership is not validated before publication"
require_body "$helper_body" 'install -o root -g root -m 0755' \
  "key helper is not published root-owned"

require_body "$build_body" '[[ "$DARKIRC_REV" == "$DARKIRC_APPROVED_REV" ]]' \
  "DarkIRC build does not require the approved source revision"
require_body "$build_body" 'darkirc_validate_build_artifact "$built_bin"' \
  "DarkIRC artifact ownership is not validated"
require_body "$build_body" 'write_darkirc_compatibility "$source_revision" "$test_user"' \
  "DarkIRC publication does not create a measured compatibility attestation"
require_body "$build_body" 'publish_pending=true' \
  "DarkIRC publication lacks rollback state"
require_body "$build_body" 'previous_binary' \
  "DarkIRC publication lacks binary rollback"
require_body "$build_body" 'previous_compatibility' \
  "DarkIRC publication lacks attestation rollback"

require_body "$compat_body" 'sha256sum "$DARKIRC_BIN"' \
  "compatibility writer does not hash the installed daemon"
require_body "$compat_body" 'self-test-generator' \
  "compatibility writer does not test the attested generator contract"
require_body "$compat_body" 'env -i HOME=' \
  "generator self-test does not use a sanitized environment"
require_body "$compat_body" 'chmod 0600 "$tmp"' \
  "compatibility candidate is not mode 0600"
require_body "$compat_body" 'chown root:root "$tmp"' \
  "compatibility candidate is not root-owned"
require_body "$compat_body" 'mv "$tmp" "$DARKIRC_COMPAT_FILE"' \
  "compatibility attestation is not atomically published"

require_body "$load_body" 'profile_id' \
  "compatibility loader does not validate the profile"
require_body "$load_body" 'key_format' \
  "compatibility loader does not validate key format"
require_body "$load_body" 'source_revision' \
  "compatibility loader does not validate source revision"
require_body "$load_body" 'sha256sum "$DARKIRC_BIN"' \
  "compatibility loader does not remeasure the installed daemon"
require_body "$load_body" '[[ "$actual" == "$digest" ]]' \
  "compatibility loader does not bind the daemon to its attestation"

require_body "$executable_body" '[[ -f "$path" && ! -L "$path" && -x "$path" ]]' \
  "trusted executable validation does not reject links and non-files"
require_body "$executable_body" '"$owner" == 0' \
  "trusted executable validation does not require root ownership"
require_body "$executable_body" '"$links" == 1' \
  "trusted executable validation does not reject hard links"
require_body "$executable_body" '07022' \
  "trusted executable validation does not reject special/write permission bits"

printf 'ALL TESTS PASSED\n'
