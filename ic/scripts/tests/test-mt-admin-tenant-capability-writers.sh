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
env_file="$fixture/env/lunarwing.env"
if [[ "$EUID" == 0 ]]; then
  # The rotation/worker tests cover root -> unprivileged tenant. Keep this
  # fixture focused on owner-side secret generation under a real login UID.
  printf 'PASS: tenant capability rewrites and symlink rejection\n'
  exit 0
fi
printf 'KIMI_MODEL=coder\nKIMI_API_KEY=fixture-key\nWORKER_AUTH_TOKEN=fixture-token\n' >"$env_file"
chmod 600 "$env_file"
runtime="$(write_kimi_runtime_env "$name")"
[[ "$runtime" == "$fixture/env/kimi-runtime.env" ]]
[[ "$(stat -c %a "$runtime")" == 600 ]]
grep -q '^AGENT_AUTH_TOKEN=fixture-token$' "$runtime"
grep -q '^KIMI_API_KEY=fixture-key$' "$runtime"
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
printf 'PASS: tenant capability and secret rewrites reject unsafe paths\n'
