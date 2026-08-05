#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

fixture="$(mktemp -d)"
trap 'chmod -R u+w -- "$fixture" 2>/dev/null || true; rm -rf -- "$fixture"' EXIT
DARKIRC_RELEASE_ROOT="$fixture/root"
DARKIRC_RELEASES_DIR="$DARKIRC_RELEASE_ROOT/releases"
DARKIRC_RELEASE_SELECTOR="$DARKIRC_RELEASE_ROOT/current"
mkdir -p "$DARKIRC_RELEASES_DIR"

# Fixture runs unprivileged. Keep production validator logic while adapting only
# root ownership observations at test boundary.
darkirc_path_components_safe() { return 0; }
darkirc_validate_root_owned_directory() { return 0; }
darkirc_validate_clean_root_checkout() { return 0; }
darkirc_validate_root_executable() { [[ -f "$1" && ! -L "$1" && -x "$1" ]]; }
stat() {
  local format="" path="${*: -1}"
  if [[ "${1:-}" == -c ]]; then format="$2"; fi
  case "$format" in
    %u) printf '0\n' ;;
    %u:%a:%h) command stat -c "0:%a:%h" -- "$path" ;;
    *) command stat "$@" ;;
  esac
}

make_release() {
  local id="$1" daemon="$2" helper="$3" helper_revision="${4:-$helper_source_revision}"
  local dir daemon_digest helper_digest
  dir="$DARKIRC_RELEASES_DIR/$id"
  mkdir "$dir"
  printf '%s' "$daemon" >"$dir/darkirc"
  printf '%s' "$helper" >"$dir/lunarwing-darkirc-key-helper"
  daemon_digest="sha256:$(sha256sum "$dir/darkirc" | cut -d' ' -f1)"
  helper_digest="sha256:$(sha256sum "$dir/lunarwing-darkirc-key-helper" | cut -d' ' -f1)"
  jq -n --arg release "$id" --arg daemon "$daemon_digest" --arg helper "$helper_digest" \
    --arg helper_revision "$helper_revision" \
    --arg revision "${id%%-*}" \
    '{schema:"lunarwing.darkirc-compat/v2",release_id:$release,profile_id:"darkfi-a05956d41-chacha-v1",daemon_sha256:$daemon,helper_sha256:$helper,helper_source_revision:$helper_revision,key_format:"darkfi-chacha-base58-32",source_revision:$revision}' \
    >"$dir/attestation.json"
  chmod 0555 "$dir/darkirc" "$dir/lunarwing-darkirc-key-helper" "$dir"
  chmod 0444 "$dir/attestation.json"
}

helper_source_revision="$(git -C "$LUNARWING_ROOT" rev-parse HEAD)"
stale_helper_source_revision="$(printf '0%.0s' {1..40})"
old_id="$(printf 'a%.0s' {1..40})-$(printf 'b%.0s' {1..16})-$(printf 'c%.0s' {1..16})-$(printf 'd%.0s' {1..16})"
new_daemon_digest="$(printf '%s' new-daemon | sha256sum | cut -d' ' -f1)"
new_helper_digest="$(printf '%s' new-helper | sha256sum | cut -d' ' -f1)"
stale_daemon_digest="$(printf '%s' stale-daemon | sha256sum | cut -d' ' -f1)"
stale_helper_digest="$(printf '%s' stale-helper | sha256sum | cut -d' ' -f1)"
new_id="$DARKIRC_APPROVED_REV-${helper_source_revision:0:16}-${new_daemon_digest:0:16}-${new_helper_digest:0:16}"
stale_id="$DARKIRC_APPROVED_REV-${stale_helper_source_revision:0:16}-${stale_daemon_digest:0:16}-${stale_helper_digest:0:16}"
make_release "$old_id" old-daemon old-helper
make_release "$new_id" new-daemon new-helper
make_release "$stale_id" stale-daemon stale-helper "$stale_helper_source_revision"
ln -s "releases/$old_id" "$DARKIRC_RELEASE_SELECTOR"

darkirc_resolve_release || fail "old selector rejected"
pinned="$DARKIRC_PINNED_RELEASE"

mv() { return 23; }
if (darkirc_publish_selector "$new_id") >/dev/null 2>&1; then
  fail "selector publish ignored pre-rename failure"
fi
unset -f mv
[[ "$(readlink "$DARKIRC_RELEASE_SELECTOR")" == "releases/$old_id" ]] \
  || fail "pre-rename failure changed current release"
[[ ! -e "$DARKIRC_RELEASE_ROOT/.current.$$.tmp" && ! -L "$DARKIRC_RELEASE_ROOT/.current.$$.tmp" ]] \
  || fail "failed selector publication left candidate"

ln -s "releases/$new_id" "$DARKIRC_RELEASE_ROOT/.next"
mv -Tf "$DARKIRC_RELEASE_ROOT/.next" "$DARKIRC_RELEASE_SELECTOR"
[[ "$(<"$pinned/darkirc")" == old-daemon ]] || fail "pinned reader mixed generations"
darkirc_resolve_release || fail "new selector rejected"
[[ "$(<"$DARKIRC_PINNED_RELEASE/darkirc")" == new-daemon ]] || fail "selector switch missed new release"
[[ -d "$DARKIRC_RELEASES_DIR/$old_id" ]] || fail "old release was garbage-collected"
darkirc_load_compatibility || fail "coherent v2 release rejected"

for failure in 1 2 3 4 5 6 7; do
  if (
    calls=0
    darkirc_fsync_path() {
      calls=$((calls + 1))
      [[ "$calls" -ne "$failure" ]]
    }
    darkirc_sync_release_durability "$DARKIRC_PINNED_RELEASE"
  ); then
    fail "release durability repair ignored fsync failure $failure"
  fi
done

ln -s "releases/$old_id" "$DARKIRC_RELEASE_ROOT/.next"
mv -Tf "$DARKIRC_RELEASE_ROOT/.next" "$DARKIRC_RELEASE_SELECTOR"
darkirc_fsync_path() { return 29; }
if (darkirc_publish_selector "$new_id") >/dev/null 2>&1; then
  fail "selector publish ignored post-rename sync failure"
fi
unset -f darkirc_fsync_path
darkirc_fsync_path() {
  sync -f -- "$1" || die "could not durably sync DarkIRC release path: $1"
}
[[ "$(readlink "$DARKIRC_RELEASE_SELECTOR")" == "releases/$new_id" ]] \
  || fail "post-rename failure did not leave a complete selected release"
darkirc_load_compatibility || fail "retry did not repair visible selector durability"

cp "$DARKIRC_PINNED_RELEASE/attestation.json" "$fixture/attestation.original"
chmod 0755 "$DARKIRC_PINNED_RELEASE"
jq --arg revision "$(printf '0%.0s' {1..40})" \
  '.helper_source_revision = $revision' "$fixture/attestation.original" \
  >"$fixture/attestation.stale"
mv -f -- "$fixture/attestation.stale" "$DARKIRC_PINNED_RELEASE/attestation.json"
chmod 0444 "$DARKIRC_PINNED_RELEASE/attestation.json"
chmod 0555 "$DARKIRC_PINNED_RELEASE"
(darkirc_load_compatibility >/dev/null 2>&1) \
  && fail "release ID accepted inconsistent helper source revision"
chmod 0755 "$DARKIRC_PINNED_RELEASE"
rm -f -- "$DARKIRC_PINNED_RELEASE/attestation.json"
cp "$fixture/attestation.original" "$DARKIRC_PINNED_RELEASE/attestation.json"
chmod 0444 "$DARKIRC_PINNED_RELEASE/attestation.json"
chmod 0555 "$DARKIRC_PINNED_RELEASE"
darkirc_load_compatibility || fail "current helper source revision rejected"
darkirc_release_matches_current_helper_source \
  || fail "upgrade freshness check rejected current helper source revision"

ln -s "releases/$stale_id" "$DARKIRC_RELEASE_ROOT/.next"
mv -Tf "$DARKIRC_RELEASE_ROOT/.next" "$DARKIRC_RELEASE_SELECTOR"
darkirc_load_compatibility \
  || fail "coherent digest-bound retained release rejected after source revision changed"
darkirc_release_matches_current_helper_source \
  && fail "upgrade freshness check accepted stale helper source revision"
ln -s "releases/$new_id" "$DARKIRC_RELEASE_ROOT/.next"
mv -Tf "$DARKIRC_RELEASE_ROOT/.next" "$DARKIRC_RELEASE_SELECTOR"
darkirc_load_compatibility || fail "current release rejected after retained-release test"

chmod 0755 "$DARKIRC_PINNED_RELEASE/lunarwing-darkirc-key-helper"
printf '%s' tampered-helper >"$DARKIRC_PINNED_RELEASE/lunarwing-darkirc-key-helper"
chmod 0555 "$DARKIRC_PINNED_RELEASE/lunarwing-darkirc-key-helper"
(darkirc_load_compatibility >/dev/null 2>&1) && fail "helper digest mismatch accepted"

rm "$DARKIRC_RELEASE_SELECTOR"
printf 'releases/%s/extra\n' "$new_id" >"$DARKIRC_RELEASE_SELECTOR"
darkirc_resolve_release >/dev/null 2>&1 && fail "regular-file selector accepted"
rm "$DARKIRC_RELEASE_SELECTOR"
ln -s "../$new_id" "$DARKIRC_RELEASE_SELECTOR"
darkirc_resolve_release >/dev/null 2>&1 && fail "escaping selector accepted"
rm "$DARKIRC_RELEASE_SELECTOR"
ln -s "releases/$new_id" "$DARKIRC_RELEASE_SELECTOR"
chmod 0755 "$DARKIRC_RELEASES_DIR/$new_id"
darkirc_resolve_release >/dev/null 2>&1 && fail "writable release accepted"

printf 'ALL TESTS PASSED\n'
