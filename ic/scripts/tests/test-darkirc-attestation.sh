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
source_match_body="$(declare -f darkirc_release_matches_current_helper_source)"
sync_body="$(declare -f darkirc_sync_release_durability)"
ensure_body="$(declare -f ensure_darkirc_release)"
start_body="$(declare -f start_tenant)"
service_body="$(declare -f darkirc_service_uses_release_selector)"
systemd_start_body="$(declare -f start_tenant_systemd)"
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
require_body "$helper_body" 'DARKIRC_HELPER_ARTIFACT="$artifact"' \
  "key helper build does not return staged artifact"
require_body "$helper_body" 'DARKIRC_HELPER_SOURCE_REVISION="$source_revision"' \
  "key helper build does not return its trusted source revision"

require_body "$build_body" '[[ "$DARKIRC_REV" == "$DARKIRC_APPROVED_REV" ]]' \
  "DarkIRC build does not require the approved source revision"
require_body "$build_body" 'darkirc_validate_build_artifact "$built_bin"' \
  "DarkIRC artifact ownership is not validated"
require_body "$build_body" 'write_darkirc_compatibility "$staging_dir"' \
  "DarkIRC release does not create staged compatibility attestation"
require_body "$build_body" 'darkirc_fsync_path "$staging_dir/darkirc"' \
  "DarkIRC release does not fsync staged daemon"
require_body "$build_body" 'darkirc_fsync_path "$staging_dir"' \
  "DarkIRC release does not fsync staged release directory"
require_body "$build_body" 'darkirc_publish_selector "$release_id"' \
  "DarkIRC release does not switch one selector"
require_body "$build_body" 'chmod 0555 "$staging_dir"' \
  "DarkIRC release is not immutable before publication"
require_body "$build_body" 'darkirc_fsync_path "$release_parent"' \
  "DarkIRC first publication does not sync newly created release parent"
require_body "$build_body" 'darkirc_fsync_path "$(dirname "$release_parent")"' \
  "DarkIRC first publication does not sync release-parent directory entry"
sync_line="$(grep -nF 'darkirc_sync_release_durability "$release_dir"' <<<"$build_body" | cut -d: -f1)"
publish_line="$(grep -nF 'darkirc_publish_selector "$release_id"' <<<"$build_body" | cut -d: -f1)"
[[ -n "$sync_line" && -n "$publish_line" && "$sync_line" -lt "$publish_line" ]] \
  || fail "DarkIRC selector can switch before complete release durability"

for path in darkirc lunarwing-darkirc-key-helper attestation.json; do
  require_body "$sync_body" 'darkirc_fsync_path "$release_dir/'"$path"'"' \
    "DarkIRC durability repair omits $path"
done
require_body "$sync_body" 'darkirc_fsync_path "$release_dir"' \
  "DarkIRC durability repair omits release directory"
require_body "$sync_body" 'darkirc_fsync_path "$DARKIRC_RELEASES_DIR"' \
  "DarkIRC durability repair omits releases parent"
require_body "$sync_body" 'darkirc_fsync_path "$DARKIRC_RELEASE_ROOT"' \
  "DarkIRC durability repair omits selector directory"
require_body "$sync_body" 'darkirc_fsync_path "$(dirname "$DARKIRC_RELEASE_ROOT")"' \
  "DarkIRC durability repair omits release-root parent"

require_body "$compat_body" 'lunarwing.darkirc-compat/v2' \
  "compatibility writer does not emit attestation v2"
require_body "$compat_body" 'daemon_sha256:$daemon' \
  "compatibility writer does not bind daemon digest"
require_body "$compat_body" 'helper_sha256:$helper' \
  "compatibility writer does not bind helper digest"
require_body "$compat_body" 'helper_source_revision:$helper_revision' \
  "compatibility writer does not bind helper source revision"
require_body "$compat_body" 'release_id:$release' \
  "compatibility writer does not bind release ID"
require_body "$compat_body" 'self-test-generator' \
  "compatibility writer does not test the attested generator contract"
require_body "$compat_body" 'env -i HOME=' \
  "generator self-test does not use a sanitized environment"
require_body "$compat_body" 'chmod 0444 "$tmp"' \
  "compatibility candidate is not immutable mode 0444"
require_body "$compat_body" 'chown root:root "$tmp"' \
  "compatibility candidate is not root-owned"

require_body "$load_body" 'profile_id' \
  "compatibility loader does not validate the profile"
require_body "$load_body" 'key_format' \
  "compatibility loader does not validate key format"
require_body "$load_body" 'source_revision' \
  "compatibility loader does not validate source revision"
require_body "$load_body" 'darkirc_resolve_release' \
  "compatibility loader does not pin selector once"
require_body "$load_body" 'darkirc_sync_release_durability "$DARKIRC_PINNED_RELEASE"' \
  "compatibility loader does not repair visible release durability"
require_body "$load_body" 'darkirc_fsync_path "$DARKIRC_RELEASE_ROOT"' \
  "compatibility loader does not repair selector-parent durability"
require_body "$load_body" 'sha256sum "$DARKIRC_PINNED_RELEASE/darkirc"' \
  "compatibility loader does not remeasure the installed daemon"
require_body "$load_body" '[[ "$actual" == "$digest" ]]' \
  "compatibility loader does not bind the daemon to its attestation"
require_body "$load_body" '[[ "$helper_actual" == "$helper_digest" ]]' \
  "compatibility loader does not bind helper to its attestation"
require_body "$load_body" 'DARKIRC_PINNED_HELPER_SOURCE_REVISION="$helper_source_revision"' \
  "compatibility loader does not expose attested helper source revision"
require_body "$source_match_body" 'darkirc_current_helper_source_revision' \
  "upgrade freshness check does not resolve current helper source revision"
require_body "$source_match_body" 'darkirc_validate_clean_root_checkout "$LUNARWING_ROOT"' \
  "upgrade freshness check accepts a dirty or untrusted helper checkout"
require_body "$ensure_body" 'darkirc_release_matches_current_helper_source' \
  "release bootstrap does not rebuild a stale helper during upgrade"
require_body "$ensure_body" 'build_darkirc "$name"' \
  "release bootstrap does not rebuild a missing atomic release"
require_body "$start_body" 'darkirc_load_compatibility' \
  "tenant start does not validate the selected release"
require_body "$start_body" 'darkirc_service_uses_release_selector "$name"' \
  "tenant start does not reject stale DarkIRC units"
require_body "$service_body" '[[ -f "$service_path" && ! -L "$service_path" ]]' \
  "DarkIRC service validation accepts missing or symlinked units"
require_body "$service_body" 'grep -Fxc -- "$expected" "$service_path"' \
  "DarkIRC service validation does not require exactly one selector command"
require_body "$service_body" 'grep -Fc -- "$command_prefix" "$service_path"' \
  "DarkIRC service validation accepts additional command overrides"
require_body "$service_body" 'DropInPaths' \
  "DarkIRC systemd validation ignores command drop-ins"
require_body "$systemd_start_body" 'darkirc_service_uses_release_selector "$name"' \
  "systemd start does not revalidate effective DarkIRC unit after reload"

systemd_body="$(declare -f render_tenant_systemd_units)"
openrc_body="$(declare -f render_tenant_openrc_units)"
require_body "$systemd_body" 'ExecStart=$DARKIRC_RELEASE_DAEMON' \
  "systemd DarkIRC command does not use release selector"
require_body "$openrc_body" 'darkirc_command="$DARKIRC_RELEASE_DAEMON"' \
  "OpenRC DarkIRC command is not hard-bound to the release selector"

require_body "$executable_body" '[[ -f "$path" && ! -L "$path" && -x "$path" ]]' \
  "trusted executable validation does not reject links and non-files"
require_body "$executable_body" '"$owner" == 0' \
  "trusted executable validation does not require root ownership"
require_body "$executable_body" '"$links" == 1' \
  "trusted executable validation does not reject hard links"
require_body "$executable_body" '07022' \
  "trusted executable validation does not reject special/write permission bits"

printf 'ALL TESTS PASSED\n'
