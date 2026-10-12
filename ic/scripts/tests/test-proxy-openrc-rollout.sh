#!/usr/bin/env bash
# WI#376 / !1547: neutralize the whole fleet before OpenRC can source any unit.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
fail() { printf 'FAIL: %s\n' "$1" >&2; exit 1; }

export OPENRC_INITD_DIR="$fixture/init.d"
export OPENRC_CONFD_DIR="$fixture/conf.d"
export PORTS_REGISTRY="$fixture/ports.json"
export INIT_SYSTEM=openrc
mkdir -p "$OPENRC_INITD_DIR" "$OPENRC_CONFD_DIR"
cat >"$PORTS_REGISTRY" <<'JSON'
{"tenants":{"alice":{"enable_proxy":true,"ports":{"proxy":23104}},"bob":{"enable_proxy":true,"ports":{"proxy":23204}},"disabled":{"enable_proxy":false,"ports":{"proxy":23304}},"new":{"enable_proxy":true,"ports":{"proxy":23404}}}}
JSON
tenant_home() { printf '%s/%s' "$fixture" "$1"; }
install_openrc_env_exec() { :; }
require_root() { :; }
tenant_darkirc_enabled() { return 1; }

# Simulate a dependency-cache rebuild: every OpenRC invocation sources ALL
# installed proxy scripts, including other tenants and disabled legacy units.
rc-service() {
  printf 'rc-service %s\n' "$*" >>"$fixture/commands"
  local unit
  for unit in "$OPENRC_INITD_DIR"/lunarwing-proxy-*; do
    /bin/sh -c '. "$1"' sh "$unit"
  done
}
rc-update() { rc-service "$@"; }

legacy_unit() {
  printf ': "${proxy_args:=proxy --tensorzero http://fixture.invalid/$(touch${IFS}%s)}"\n' \
    "$fixture/evaluated" >"$OPENRC_INITD_DIR/lunarwing-proxy-$1"
}
for tenant in alice bob disabled; do legacy_unit "$tenant"; done
# A mention of the new flag is not enough: the default must actually be fixed.
printf '# --tensorzero-env\n' >>"$OPENRC_INITD_DIR/lunarwing-proxy-bob"

for operation in 'stop_tenant_openrc alice' 'stop_tenant alice' 'openrc_service_state lunarwing-proxy-alice' \
  'status_tenant alice' '_openrc_service lunarwing-bob status' '_openrc_update add unrelated default'; do
  read -r -a argv <<<"$operation"
  if "${argv[@]}" >"$fixture/refused.log" 2>&1; then
    fail "unsafe $operation succeeded"
  fi
  [[ ! -e "$fixture/commands" && ! -e "$fixture/evaluated" ]] \
    || fail 'OpenRC ran before fleet neutralization'
  grep -Fq 'render-units --all-proxies' "$fixture/refused.log" \
    || fail 'refusal did not explain how to repair installed units'
done

# B remains a hazard even after A's unit is safe. The real migration must also
# work without readable/valid proxy.env data or updated tenant proxy code.
render_proxy_openrc_unit alice
if stop_tenant_openrc alice >"$fixture/refused.log" 2>&1; then
  fail 'repairing only tenant A allowed OpenRC to parse tenant B'
fi
[[ ! -e "$fixture/commands" ]] || fail 'per-tenant repair reached OpenRC'

main render-units --all-proxies >"$fixture/render.log"
[[ ! -e "$fixture/commands" && ! -e "$fixture/evaluated" ]] \
  || fail 'fleet rendering invoked OpenRC or evaluated an installed script'
for tenant in alice bob disabled new; do
  grep -Fq -- '--tensorzero-env}' "$OPENRC_INITD_DIR/lunarwing-proxy-$tenant" \
    || fail "fleet render missed $tenant"
done
stop_tenant_openrc alice
openrc_service_state lunarwing-proxy-alice >/dev/null
[[ -s "$fixture/commands" && ! -e "$fixture/evaluated" ]] \
  || fail 'safe fleet did not permit OpenRC operations'

# Unknown and symlinked installed units must fail closed, not get ignored by
# a registry-only sweep or followed to some other file.
legacy_unit orphan
if (main render-units --all-proxies) >"$fixture/orphan.log" 2>&1; then
  fail 'fleet render silently ignored an unregistered installed proxy'
fi
grep -Fq 'orphan' "$fixture/orphan.log" || fail 'orphan diagnostic missing'
rm "$OPENRC_INITD_DIR/lunarwing-proxy-orphan"
mv "$OPENRC_INITD_DIR/lunarwing-proxy-bob" "$fixture/bob-unit"
ln -s "$fixture/bob-unit" "$OPENRC_INITD_DIR/lunarwing-proxy-bob"
if (main render-units --all-proxies) >"$fixture/symlink.log" 2>&1; then
  fail 'fleet render followed a symlinked installed proxy'
fi
[[ -L "$OPENRC_INITD_DIR/lunarwing-proxy-bob" ]] || fail 'unsafe link was replaced'

printf 'ALL TESTS PASSED\n'
