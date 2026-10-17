#!/usr/bin/env bash
# Fixture-only checks for capability rewrites in a tenant-owned state directory.
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
  # through runuser (util-linux), matching test-rotate-relay-password.sh.
  sudo() {
    [[ "$1" == -u && "$2" == nobody ]] || return 1
    shift 2
    runuser -u nobody -- "$@"
  }
fi
tenant_state_dir() { printf '%s/state' "$fixture"; }
tenant_env_dir() { printf '%s/env' "$fixture"; }
ports_get() { [[ "$2" == weechat ]] && printf '18005\n' || printf '18006\n'; }
_ssh_hosts_from_config() { printf 'example.org\n'; }
extract_host_from_url() { printf 'gotify.example.org\n'; }
mkdir -p "$fixture/state/tools" "$fixture/state/channels"
mkdir -p "$fixture/env"
chmod 700 "$fixture/env"
chmod 755 "$fixture/state/tools" "$fixture/state/channels"
ssh_caps="$fixture/state/tools/ssh-tool.capabilities.json"
wc_caps="$fixture/state/channels/weechat.capabilities.json"
gotify_caps="$fixture/state/tools/gotify-tool.capabilities.json"
printf '{"capabilities":{"ssh":{"allowed_hosts":[]}}}\n' >"$ssh_caps"
printf '{"config":{"relay_url":"old","ws_adapter_url":"old"}}\n' >"$wc_caps"
printf '{"capabilities":{"http":{"allowlist":[{"host":"old"}],"credentials":{"gotify":{"host_patterns":[]}}}}}\n' >"$gotify_caps"
if [[ "$EUID" == 0 ]]; then
  chown -R nobody "$fixture/state"
  chown -R nobody "$fixture/env"
fi
patch_ssh_tool_allowlist "$name"
patch_weechat_caps "$name"
configure_gotify_capabilities "$name" 'https://gotify.example.org'
[[ "$(jq -r '.capabilities.ssh.allowed_hosts[0]' "$ssh_caps")" == example.org ]]
[[ "$(jq -r '.config.relay_url' "$wc_caps")" == http://127.0.0.1:18005 ]]
[[ "$(jq -r '.capabilities.http.allowlist[0].host' "$gotify_caps")" == gotify.example.org ]]
[[ "$(stat -c %a "$ssh_caps")" == 600 && "$(stat -c %a "$wc_caps")" == 600 ]]
sentinel="$fixture/sentinel"
printf 'untouched\n' >"$sentinel"
rm "$wc_caps"
ln -s "$sentinel" "$wc_caps"
patch_weechat_caps "$name"
[[ "$(<"$sentinel")" == untouched && -L "$wc_caps" ]]

# Failure paths: a failing producer must never publish a partial result.
# Corrupt JSON makes the tenant-side jq exit nonzero mid-parse; the staged
# rewrite is discarded and the installed caps stay byte-for-byte intact.
rm -f "$wc_caps"
printf '{"config":{"relay_url":' >"$wc_caps"
if [[ "$EUID" == 0 ]]; then chown nobody "$wc_caps"; fi
wc_sha="$(sha256sum "$wc_caps" | cut -d' ' -f1)"
patch_weechat_caps "$name" >/dev/null 2>&1
[[ "$(sha256sum "$wc_caps" | cut -d' ' -f1)" == "$wc_sha" ]] \
  || { echo 'FAIL: weechat caps clobbered by failing jq' >&2; exit 1; }
printf '{"capabilities":{"ssh":' >"$ssh_caps"
if [[ "$EUID" == 0 ]]; then chown nobody "$ssh_caps"; fi
ssh_sha="$(sha256sum "$ssh_caps" | cut -d' ' -f1)"
patch_ssh_tool_allowlist "$name" >/dev/null 2>&1
[[ "$(sha256sum "$ssh_caps" | cut -d' ' -f1)" == "$ssh_sha" ]] \
  || { echo 'FAIL: ssh-tool caps clobbered by failing jq' >&2; exit 1; }
printf '{"capabilities":{"http":' >"$gotify_caps"
if [[ "$EUID" == 0 ]]; then chown nobody "$gotify_caps"; fi
gotify_sha="$(sha256sum "$gotify_caps" | cut -d' ' -f1)"
if configure_gotify_capabilities "$name" 'https://gotify.example.org' >/dev/null 2>&1; then
  echo 'FAIL: gotify caps accepted corrupt JSON' >&2
  exit 1
fi
[[ "$(sha256sum "$gotify_caps" | cut -d' ' -f1)" == "$gotify_sha" ]] \
  || { echo 'FAIL: gotify caps clobbered by failing jq' >&2; exit 1; }

env_file="$fixture/env/lunarwing.env"
if [[ "$EUID" == 0 ]]; then
  # Fresh install-wasm copies the shipped caps as root. The tenant-UID writer
  # must see tenant ownership on both the destination and its parent before
  # replacing the generic ports; start-tenant must not be needed to fix them.
  fixture_repo="$fixture/repo"
  tenant_repo() { printf '%s\n' "$fixture_repo"; }
  tenant_exists_in_registry() { :; }
  tenant_darkirc_enabled() { return 1; }
  mkdir -p "$fixture_repo/channels-src/weechat/target/wasm32-wasip2/release"
  printf 'fixture wasm\n' >"$fixture_repo/channels-src/weechat/target/wasm32-wasip2/release/weechat_relay_channel.wasm"
  printf '{"config":{"relay_url":"http://127.0.0.1:9001","ws_adapter_url":"http://127.0.0.1:6681"}}\n' \
    >"$fixture_repo/channels-src/weechat/weechat.capabilities.json"
  rm -rf -- "$fixture/state/channels" "$fixture/state/tools"
  mkdir -p "$fixture/state/channels"
  cp "$fixture_repo/channels-src/weechat/weechat.capabilities.json" "$wc_caps"
  [[ "$(stat -c %U "$fixture/state/channels")" == root ]]
  [[ "$(stat -c %U "$wc_caps")" == root ]]
  patch_weechat_caps "$name" >/dev/null 2>&1
  [[ "$(jq -r '.config.relay_url' "$wc_caps")" == http://127.0.0.1:9001 ]]
  # Real tenants have a matching group, but some CI images provide nobody
  # only with a different primary group (e.g. nogroup). Translate just the
  # install fixture's tenant:tenant chown; still perform a real chown so the
  # tenant-UID writer and ownership assertions exercise the actual boundary.
  if ! getent group "$name" >/dev/null; then
    chown() {
      [[ "$1" == -R && "$2" == "$name:$name" ]] || return 1
      shift 2
      command chown -R "$name:$(id -g "$name")" "$@"
    }
  fi
  install_wasm_tenant "$name"
  [[ "$(stat -c %U "$fixture/state/channels")" == "$name" ]]
  [[ "$(stat -c %U "$wc_caps")" == "$name" ]]
  [[ "$(stat -c %a "$wc_caps")" == 600 ]]
  [[ "$(jq -r '.config.relay_url' "$wc_caps")" == http://127.0.0.1:18005 ]]
  [[ "$(jq -r '.config.ws_adapter_url' "$wc_caps")" == http://127.0.0.1:18006 ]]
  install_wasm_tenant "$name"
  [[ "$(jq -r '.config.relay_url' "$wc_caps")" == http://127.0.0.1:18005 ]]
  # The rotation/worker tests cover root -> unprivileged tenant. Keep this
  # fixture focused on owner-side secret generation under a real login UID.
  printf 'PASS: fresh root-created WASM caps patched, tenant capability rewrites, symlink rejection, and producer-failure containment\n'
  exit 0
fi
printf 'KIMI_MODEL=coder\nKIMI_API_KEY=fixture-key\nWORKER_AUTH_TOKEN=fixture-token\n' >"$env_file"
chmod 600 "$env_file"
runtime="$(write_kimi_runtime_env "$name")"
[[ "$runtime" == "$fixture/env/kimi-runtime.env" ]]
[[ "$(stat -c %a "$runtime")" == 600 ]]
grep -q '^AGENT_AUTH_TOKEN=fixture-token$' "$runtime"
grep -q '^KIMI_API_KEY=fixture-key$' "$runtime"

# A failed tenant-side read/transform must leave committed files intact: the
# relaxed env parent (mode 711) violates the private-parent policy, so the
# staged rewrite is discarded before any rename can happen.
env_sha="$(sha256sum "$env_file" | cut -d' ' -f1)"
kimi_sha="$(sha256sum "$runtime" | cut -d' ' -f1)"
chmod 711 "$fixture/env"
if _tenant_env_replace "$name" "$env_file" DATABASE_URL 'postgres://replacement-value'; then
  echo 'FAIL: _tenant_env_replace accepted a non-private env parent' >&2
  exit 1
fi
[[ "$(sha256sum "$env_file" | cut -d' ' -f1)" == "$env_sha" ]] \
  || { echo 'FAIL: lunarwing.env clobbered by failed replace' >&2; exit 1; }
if write_kimi_runtime_env "$name" >/dev/null 2>&1; then
  echo 'FAIL: write_kimi_runtime_env accepted a non-private env parent' >&2
  exit 1
fi
[[ "$(sha256sum "$runtime" | cut -d' ' -f1)" == "$kimi_sha" ]] \
  || { echo 'FAIL: kimi-runtime.env clobbered by failed generation' >&2; exit 1; }
chmod 700 "$fixture/env"

printf 'DATABASE_URL=postgres://old\nPRESERVE=yes\n' >"$env_file"
chmod 600 "$env_file"
ensure_container_runtime() { :; }
tenant_exists_in_registry() { :; }
generate_token() { printf '0123456789abcdef0123456789abcdef\n'; }
ports_get() { printf '10003\n'; }
_ctr() {
  if [[ "$2" == inspect ]]; then printf 'true\n'; else cat >/dev/null; fi
}
rotate_tenant_pg_password "$name" >/dev/null
grep -q '^PRESERVE=yes$' "$env_file"
grep -q '^DATABASE_URL=postgres://lunarwing:0123456789abcdef0123456789abcdef@127.0.0.1:10003/lunarwing$' "$env_file"
[[ "$(<"$fixture/env/pg.secret")" == 0123456789abcdef0123456789abcdef ]]

# WI#390 (IY-3): the rotation's DATABASE_URL replace must be fatal — a
# hard-linked lunarwing.env (link count > 1, refused by the tenant-side
# replace) must abort the rotation naming DATABASE_URL instead of reporting
# "Rotated" with pg.secret and the env diverged.
printf 'DATABASE_URL=postgres://lunarwing:oldpw@127.0.0.1:10003/lunarwing\nPRESERVE=yes\n' >"$env_file"
chmod 600 "$env_file"
ln "$env_file" "$env_file.hardlink"
if rotate_err="$(rotate_tenant_pg_password "$name" 2>&1 >/dev/null)"; then
  echo 'FAIL: rotation accepted a hard-linked lunarwing.env' >&2
  exit 1
fi
[[ "$rotate_err" == *DATABASE_URL* ]] \
  || { echo "FAIL: rotation refusal did not name DATABASE_URL: $rotate_err" >&2; exit 1; }
grep -q '^DATABASE_URL=postgres://lunarwing:oldpw@127.0.0.1:10003/lunarwing$' "$env_file" \
  || { echo 'FAIL: refused rotation rewrote DATABASE_URL' >&2; exit 1; }
grep -q '^PRESERVE=yes$' "$env_file" \
  || { echo 'FAIL: refused rotation dropped unrelated keys' >&2; exit 1; }
rm -f "$env_file.hardlink"
printf 'PASS: tenant capability and secret rewrites reject unsafe paths\n'
