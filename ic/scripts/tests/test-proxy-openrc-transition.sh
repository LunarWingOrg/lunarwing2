#!/usr/bin/env bash
# !1547 EP-1/2: refusal is never a stopped service, even with errexit suppressed.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
fail() { printf 'FAIL: %s\n' "$1" >&2; exit 1; }

export INIT_SYSTEM=openrc
export OPENRC_INITD_DIR="$fixture/init.d"
export OPENRC_CONFD_DIR="$fixture/conf.d"
export OPENRC_RUNLEVELS_DIR="$fixture/runlevels"
mkdir -p "$OPENRC_INITD_DIR" "$OPENRC_CONFD_DIR"
# "deadline" makes an incautious text parser see "dead" in the refusal message.
printf ': "${proxy_args:=proxy --tensorzero http://fixture.invalid}"\n' \
  >"$OPENRC_INITD_DIR/lunarwing-proxy-deadline"
printf '# victim service\n' >"$OPENRC_INITD_DIR/lunarwing-alice"
chmod 0755 "$OPENRC_INITD_DIR/lunarwing-alice"
printf '# victim configuration\n' >"$OPENRC_CONFD_DIR/lunarwing-alice"
printf 'PGDMPfixture\n' >"$fixture/backup.dump"

require_root() { :; }
ports_registry_init() { :; }
tenant_exists_in_registry() { return 0; }
retire_tenant_nanocode() { :; }
tenant_darkirc_enabled() { return 1; }
all_tenant_names() { printf 'alice\ndeadline\n'; }
ports_deallocate() { printf 'deallocate\n' >>"$fixture/destructive"; }
remove_tenant_user() { printf 'remove-user\n' >>"$fixture/destructive"; }
stop_tenant_postgres() { :; }
_openrc_tenant_subtree_remove() { :; }
_openrc_cgroups_locald_path() { printf '%s/absent' "$fixture"; }
ensure_container_runtime() { :; }
_ctr() {
  if [[ "$*" == *pg_restore* ]]; then
    printf 'restore\n' >>"$fixture/destructive"
  else
    printf 'true\n'
  fi
}
rc-service() { printf 'rc-service\n' >>"$fixture/service-calls"; printf ' * status: started\n'; }
rc-update() { printf 'rc-update\n' >>"$fixture/service-calls"; }
# Reproduce the production lifecycle lock's conditional invocation of its body.
with_tenant_lifecycle_lock() { shift; "$@" || return $?; }

expect_refusal() {
  local label="$1"
  shift
  if ("$@") >"$fixture/refused.log" 2>&1; then fail "$label succeeded"; fi
  grep -Fq 'render-units --all-proxies' "$fixture/refused.log" \
    || fail "$label omitted the repair command"
  [[ ! -e "$fixture/destructive" && ! -e "$fixture/service-calls" ]] \
    || fail "$label performed an operation after refusal"
  [[ -f "$OPENRC_INITD_DIR/lunarwing-alice" && -f "$OPENRC_CONFD_DIR/lunarwing-alice" ]] \
    || fail "$label deleted units after refusal"
}

expect_refusal removal with_tenant_lifecycle_lock alice remove_tenant alice false
expect_refusal restore restore_tenant_postgres alice "$fixture/backup.dump" true

# The dispatcher must reject before handlers can misinterpret a status or
# suppress an error. Stub handlers prove this independently of local guards.
(
  remove_tenant() { printf 'remove\n' >>"$fixture/destructive"; }
  restore_tenant_postgres() { printf 'restore\n' >>"$fixture/destructive"; }
  migrate_owner_scope() { printf 'migrate\n' >>"$fixture/destructive"; }
  expect_refusal dispatch-remove main remove-tenant alice
  expect_refusal dispatch-restore main restore-tenant alice "$fixture/backup.dump" --yes
  expect_refusal dispatch-migrate main migrate-owner-scope alice
)

for tool in _openrc_service _openrc_update; do
  rc=0
  "$tool" unused status >"$fixture/refused.log" 2>&1 || rc=$?
  [[ "$rc" == 125 ]] || fail "$tool did not use the distinct refusal status"
done
tenant_writer_services() { printf 'lunarwing-alice\n'; }
detect_container_runtime() { return 1; }
rc=0
tenant_writers_active alice >"$fixture/refused.log" 2>&1 || rc=$?
[[ "$rc" == 2 ]] || fail 'refusal path containing dead was classified as stopped'

# Help and both supported migration entrypoints remain available.
main help >"$fixture/help.log"
(
  render_all_openrc_proxy_units() { printf 'fleet\n' >>"$fixture/exempt"; }
  upgrade_tenant() { printf 'upgrade\n' >>"$fixture/exempt"; }
  main render-units --all-proxies
  main upgrade-tenant alice --target fixture-ref
)
[[ "$(cat "$fixture/exempt")" == $'fleet\nupgrade' ]] || fail 'migration entrypoints were blocked'

# Systemd operations do not consume these installed OpenRC files.
(
  INIT_SYSTEM=systemd
  remove_tenant() { printf 'systemd\n' >"$fixture/systemd"; }
  main remove-tenant alice
)
[[ -f "$fixture/systemd" ]] || fail 'OpenRC preflight blocked systemd'

printf 'ALL TESTS PASSED\n'
