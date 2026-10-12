#!/usr/bin/env bash
# WI#376: tenant URLs must stay data across render, root-side init parsing,
# OpenRC argument parsing, and the post-drop environment/application boundary.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
TEST_REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

FIXTURE="$(mktemp -d)"
trap 'rm -rf "$FIXTURE"' EXIT
TENANT="$(id -un)"
if [[ "$EUID" -eq 0 ]]; then
  TENANT=nobody
  chown "$TENANT:$(id -gn "$TENANT")" "$FIXTURE"
  # Minimal CI images may have runuser but no sudo. Still perform a real drop
  # when the production env reader invokes sudo -u.
  sudo() { local user="$2"; shift 2; runuser -u "$user" -- "$@"; }
fi
fail() { printf 'FAIL: %s\n' "$1" >&2; exit 1; }

# Render only into the fixture. Stub host installation/discovery, not either
# the proxy renderer, URL validator, literal env parser or application parser.
tenant_home() { printf '%s/tenant' "$FIXTURE"; }
tenant_lw_root() { printf '%s/lunarwing' "$(tenant_home)"; }
tenant_repo() { printf '%s/ic' "$(tenant_lw_root)"; }
tenant_env_dir() { printf '%s/env' "$(tenant_lw_root)"; }
tenant_state_dir() { printf '%s/state' "$(tenant_lw_root)"; }
tenant_run_dir() { printf '%s/run' "$(tenant_lw_root)"; }
tenant_log_dir() { printf '%s/logs' "$(tenant_lw_root)"; }
tenant_weechat_home() { printf '%s/.config/weechat' "$(tenant_home)"; }
ports_get() { printf '23104'; }
tenant_proxy_enabled() { return 0; }
tenant_weechat_enabled() { return 0; }
tenant_darkirc_enabled() { return 0; }
install_openrc_env_exec() { :; }
ensure_container_runtime() { :; }
prepare_relay_start_barrier() { :; }
render_container_babysitter_unit() { :; }
render_vision_openrc_assets() { :; }
_render_openrc_lunarwing_confd() { :; }

OPENRC_INITD_DIR="$FIXTURE/init.d"
# Renderer configuration is consumed by the sourced mt-admin functions.
# shellcheck disable=SC2034
OPENRC_CONFD_DIR="$FIXTURE/conf.d"
# shellcheck disable=SC2034
OPENRC_RUNLEVELS_DIR="$FIXTURE/runlevels"
OPENRC_ENV_EXEC="$FIXTURE/env-exec"
# Consumed by the renderer's backup_openrc_proxy_unit, like the vars above.
# shellcheck disable=SC2034
OPENRC_PROXY_BACKUP_DIR="$FIXTURE/backup-units"
env_dir="$(tenant_env_dir)"
mkdir -p "$env_dir" "$(tenant_lw_root)/tensorzero-proxy-configurations"
chmod 0700 "$env_dir"
cp "$TEST_REPO_ROOT/ic/scripts/lunarwing-openrc-env-exec.sh" "$OPENRC_ENV_EXEC"
chmod 0755 "$OPENRC_ENV_EXEC"
cp "$TEST_REPO_ROOT/tensorzero-proxy-configurations/lunarwing-proxy.py" "$FIXTURE/proxy.py"

# Exercise the real proxy's configuration parser without binding a listener.
# The harness simulates OpenRC argv handling and runs the real env-exec helper
# as the tenant; it does not register or start services on the host.
cat >"$(tenant_lw_root)/tensorzero-proxy-configurations/lunarwing-proxy.py" <<'PY'
import json
import os
import pathlib
import runpy
import sys

fixture = pathlib.Path(__file__).parents[3]
proxy = runpy.run_path(str(fixture / "proxy.py"))
args = proxy["parse_args"]()
print(json.dumps({"upstream": args.tensorzero, "argv": sys.argv[1:], "uid": os.geteuid()}))
PY

run_as_tenant() {
  if [[ "$EUID" -eq 0 ]]; then
    runuser -u "$TENANT" -- "$@"
  else
    "$@"
  fi
}

unit="$OPENRC_INITD_DIR/lunarwing-proxy-$TENANT"
marker="$FIXTURE/evaluated"
printf '#!/bin/sh\ntouch "%s"\n' "$marker" >"$FIXTURE/payload"
chmod 0755 "$FIXTURE/payload"
payloads=(
  "http://upstream.invalid/\$($FIXTURE/payload)"
  "http://upstream.invalid/\$(touch\${IFS}$marker)"
  "http://upstream.invalid/\`touch\${IFS}$marker\`"
  "http://upstream.invalid/\";touch\${IFS}$marker;#"
  'https://upstream.invalid/path?first=one&second=two'
)
for upstream in "${payloads[@]}"; do
  printf 'TENSORZERO_URL=%s\n' "$upstream" >"$env_dir/proxy.env"
  chmod 0600 "$env_dir/proxy.env"
  if [[ "$EUID" -eq 0 ]]; then
    chown "$TENANT:$(id -gn "$TENANT")" "$env_dir" "$env_dir/proxy.env"
  fi
  if (validate_llm_url "$upstream" fixture) >/dev/null 2>&1; then
    render_tenant_openrc_units "$TENANT" >/dev/null
  else
    # Non-URI bytes are now rejected at render time. Still exercise them after
    # a tenant edits its env: the actual security boundary must remain literal.
    if (render_tenant_openrc_units "$TENANT") >"$FIXTURE/rejected.log" 2>&1; then
      fail 'render accepted non-URI characters'
    fi
    printf 'TENSORZERO_URL=https://upstream.invalid\n' >"$env_dir/proxy.env"
    render_tenant_openrc_units "$TENANT" >/dev/null
    printf 'TENSORZERO_URL=%s\n' "$upstream" >"$env_dir/proxy.env"
  fi
  render_vision_openrc_unit "$TENANT"
  render_voice_openrc_unit "$TENANT"
  [[ ! -e "$marker" ]] || fail 'render evaluated tenant URL'
  # These are the shell files OpenRC reads as root, before command_user applies.
  for rendered in "$OPENRC_INITD_DIR"/*; do
    /bin/sh -n "$rendered"
    /bin/sh -c 'yesno() { return 1; }; . "$1"' sh "$rendered"
  done
  # OpenRC evaluates command_args before applying command_user. Exercise that
  # phase separately from the tenant-side exec and check the rendered identity.
  /bin/sh -c '
    . "$1"
    [ "$command_user" = "$2:$2" ] || exit 1
    eval "set -- $command_args"
  ' sh "$unit" "$TENANT" || fail 'OpenRC argv/identity contract changed'
  [[ ! -e "$marker" ]] || fail 'root-side init parsing evaluated tenant URL'
  if grep -Fq -- "$upstream" "$unit"; then
    fail 'tenant URL was baked into privileged init code'
  fi
  # Match OpenRC's command_args evaluation before the launcher exec. All argv
  # text here must originate from trusted render parameters, never env values.
  result="$(run_as_tenant /bin/sh -c '
    . "$1"
    eval "set -- $command_args"
    exec "$command" "$@"
  ' sh "$unit")"
  python3 - "$result" "$upstream" "$(id -u "$TENANT")" <<'PY'
import json
import sys
result = json.loads(sys.argv[1])
assert result["upstream"] == sys.argv[2], result
assert result["argv"] == ["--port", "23104", "--bind", "127.0.0.1", "--tensorzero-env"], result
assert result["uid"] == int(sys.argv[3]), result
assert result["uid"] != 0, result
PY
  [[ ! -e "$marker" ]] || fail 'post-drop URL handling evaluated shell syntax'
done

# Editing proxy.env takes effect at the next start without re-rendering units.
printf 'TENSORZERO_URL=https://new-upstream.invalid/prefix\n' >"$env_dir/proxy.env"
result="$(run_as_tenant /bin/sh -c '. "$1"; eval "set -- $command_args"; exec "$command" "$@"' sh "$unit")"
python3 - "$result" <<'PY'
import json
import sys
assert json.loads(sys.argv[1])["upstream"] == "https://new-upstream.invalid/prefix"
PY

# Missing startup data must fail, rather than revive the old fleet/LAN default.
printf '# upstream removed after render\n' >"$env_dir/proxy.env"
if run_as_tenant env -u TENSORZERO_URL /bin/sh -c \
  '. "$1"; eval "set -- $command_args"; exec "$command" "$@"' sh "$unit" \
  >"$FIXTURE/missing.log" 2>&1; then
  fail 'proxy started without an upstream'
fi
grep -Fq 'TENSORZERO_URL' "$FIXTURE/missing.log" || fail 'missing-upstream diagnostic absent'

# Both renderers must use the tenant-side reader, not open a symlink/hardlink
# as root. Use a valid outside value so the URL validator cannot mask a bad read.
printf 'TENSORZERO_URL=https://outside.invalid\n' >"$env_dir/outside.env"
chmod 0600 "$env_dir/outside.env"
if [[ "$EUID" -eq 0 ]]; then
  chown "$TENANT:$(id -gn "$TENANT")" "$env_dir/outside.env"
fi
for link_kind in symlink hardlink; do
  rm "$env_dir/proxy.env"
  if [[ "$link_kind" == symlink ]]; then
    ln -s "$env_dir/outside.env" "$env_dir/proxy.env"
  else
    ln "$env_dir/outside.env" "$env_dir/proxy.env"
  fi
  for renderer in render_tenant_openrc_units render_tenant_systemd_units; do
    if (chown() { :; }; "$renderer" "$TENANT") >"$FIXTURE/unsafe.log" 2>&1; then
      fail "$renderer accepted a $link_kind env file"
    fi
    grep -Eq 'unsafe tenant env path|cannot safely read proxy.env' "$FIXTURE/unsafe.log" \
      || fail 'renderer failed for a reason other than the unsafe env file'
    if grep -Fq 'outside.invalid' "$FIXTURE/unsafe.log"; then
      fail 'renderer exposed contents of unsafe env file'
    fi
  done
done

# Include the parser/HTTP smoke regressions in the existing shell-harness CI lane.
python3 -B -m unittest discover -s "$TEST_REPO_ROOT/tensorzero-proxy-configurations/tests"

printf 'ALL TESTS PASSED\n'
