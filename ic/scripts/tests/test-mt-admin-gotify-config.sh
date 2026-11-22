#!/usr/bin/env bash
# WI#817: a custom Gotify URL set through mt-admin must reach the tool.
#
# mt-admin writes `<tenant state>/workspace/config/gotify.json` and derives the
# capability allowlist from that same file; the daemon's WASM wrapper reads
# `<LUNARWING_BASE_DIR>/workspace/config/gotify.json` into the tool's workspace
# view at execution time (`read_host_provisioned_workspace_config` in
# ic/src/tools/wasm/wrapper.rs, pinned by its unit tests). This harness pins
# the writer side: the path, the shape, and the allowlist agreement.
# shellcheck disable=SC1091,SC2034,SC2329 # dynamic source and fixture overrides
set -euo pipefail
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d)"
trap 'rm -rf -- "$fixture"' EXIT
name="$(id -un)"
if [[ "$EUID" == 0 ]]; then
  name=nobody
  chmod 755 "$fixture"
  # CI container images ship no sudo binary; route the nobody privilege drop
  # through runuser (util-linux), matching test-mt-admin-tenant-capability-writers.sh.
  sudo() {
    [[ "$1" == -u && "$2" == nobody ]] || return 1
    shift 2
    runuser -u nobody -- "$@"
  }
else
  # The fixture tenant is the current user; the mkdir in
  # write_tenant_gotify_config still calls sudo, so run it as self.
  sudo() {
    [[ "$1" == -u && "$2" == "$name" ]] || return 1
    shift 2
    "$@"
  }
fi
tenant_state_dir() { printf '%s/state' "$fixture"; }
mkdir -p "$fixture/state/tools"
chmod 755 "$fixture/state" "$fixture/state/tools"
if [[ "$EUID" == 0 ]]; then
  chown -R nobody "$fixture/state"
fi

config="$fixture/state/workspace/config/gotify.json"

# The frozen path contract: the daemon reads this file relative to the base
# dir, and mt-admin renders LUNARWING_BASE_DIR as the tenant state dir.
[[ "$(tenant_state_dir "$name")/workspace/config/gotify.json" == "$config" ]]

# A trailing slash is normalized so the tool's `<url>/message` join is clean.
write_tenant_gotify_config "$name" 'https://gotify.example.org/' 'My Agent'
[[ -f "$config" && ! -L "$config" ]] \
  || { echo 'FAIL: configure-gotify did not write the tenant workspace config' >&2; exit 1; }
[[ "$(jq -r '.url' "$config")" == 'https://gotify.example.org' ]] \
  || { echo 'FAIL: url not normalized without trailing slash' >&2; exit 1; }
[[ "$(jq -r '.title' "$config")" == 'My Agent' ]] \
  || { echo 'FAIL: title missing from the tenant workspace config' >&2; exit 1; }
[[ "$(stat -c %a "$config")" == 600 ]] \
  || { echo 'FAIL: workspace config mode is not 600' >&2; exit 1; }
[[ "$(stat -c %U "$config")" == "$name" ]] \
  || { echo 'FAIL: workspace config is not tenant-owned' >&2; exit 1; }

# No title -> the key is omitted rather than written empty.
write_tenant_gotify_config "$name" 'https://gotify.example.org' ''
jq -e 'has("title") | not' "$config" >/dev/null \
  || { echo 'FAIL: empty title was written instead of omitted' >&2; exit 1; }

# WI#831 (AJB-4): a title or URL carrying `"` or `\` must still produce valid
# JSON with exactly the two expected keys, not a broken document that makes the
# tool fall back to its compiled-in defaults.
write_tenant_gotify_config "$name" 'https://gotify.example.org' 'Agent "quoted" \ backslash'
jq -e '.url and .title' "$config" >/dev/null \
  || { echo 'FAIL: a quoted title produced invalid JSON' >&2; exit 1; }
[[ "$(jq -r '.title' "$config")" == 'Agent "quoted" \ backslash' ]] \
  || { echo 'FAIL: a quoted title was not preserved verbatim' >&2; exit 1; }
[[ "$(jq -r 'keys | join(",")' "$config")" == 'title,url' ]] \
  || { echo 'FAIL: a quoted title leaked extra keys into the config' >&2; exit 1; }

# The capability rewrite consumes the URL from that same file, so the
# allowlist and the tool's effective base URL always name one host.
gotify_caps="$fixture/state/tools/gotify-tool.capabilities.json"
printf '{"capabilities":{"http":{"allowlist":[{"host":"old.example.org"}],"credentials":{"gotify":{"host_patterns":["old.example.org"]}}}}}\n' >"$gotify_caps"
chmod 600 "$gotify_caps"
if [[ "$EUID" == 0 ]]; then chown nobody "$gotify_caps"; fi
url="$(jq -r '.url' "$config")"
configure_gotify_capabilities "$name" "$url"
host="$(extract_host_from_url "$url")"
[[ "$(jq -r '.capabilities.http.allowlist[0].host' "$gotify_caps")" == "$host" ]] \
  || { echo 'FAIL: allowlist host diverged from the workspace config' >&2; exit 1; }
[[ "$(jq -r '.capabilities.http.credentials.gotify.host_patterns[0]' "$gotify_caps")" == "$host" ]] \
  || { echo 'FAIL: credential host pattern diverged from the workspace config' >&2; exit 1; }
[[ "$(stat -c %a "$gotify_caps")" == 600 ]] \
  || { echo 'FAIL: capabilities mode is not 600' >&2; exit 1; }

# Failure path: a symlinked config is refused by the tenant-side writer and
# the link target stays untouched (no partial publish).
sentinel="$fixture/sentinel"
printf 'untouched\n' >"$sentinel"
rm -f "$config"
ln -s "$sentinel" "$config"
if write_tenant_gotify_config "$name" 'https://gotify.example.org' '' >/dev/null 2>&1; then
  echo 'FAIL: writer accepted a symlinked config path' >&2
  exit 1
fi
[[ "$(<"$sentinel")" == untouched ]] \
  || { echo 'FAIL: symlink target was clobbered' >&2; exit 1; }

printf 'PASS: mt-admin writes the tenant workspace gotify config and the allowlist agrees\n'
