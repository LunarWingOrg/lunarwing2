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

# EQ-1: at the CLI boundary a dispatcher refusal is "unknown" (exit 2) for
# writers-active — never 1, which export-tenant/import-tenant read as stopped.
rc=0
(main writers-active alice) >"$fixture/refused.log" 2>&1 || rc=$?
[[ "$rc" == 2 ]] || fail "main writers-active refusal exited $rc, not 2"
grep -Fq 'render-units --all-proxies' "$fixture/refused.log" \
  || fail 'main writers-active refusal omitted the repair command'

# EQ-6 Q3: production main() starts with INIT_SYSTEM empty (01-detect.sh), so
# the preflight must detect the init system itself; with INIT_SYSTEM cleared
# and the override selecting openrc, a legacy unit must still refuse before
# the (stubbed) handler can run. Pins the preflight's ensure_init_system.
(
  INIT_SYSTEM=""
  export LUNARWING_SERVICE_MANAGER=openrc
  status_tenant() { printf 'status\n' >>"$fixture/destructive"; }
  expect_refusal preflight-detects-openrc main status alice
)

# FL-1: an init-detection failure must not become a false "migration required"
# refusal. With detect_init_system stubbed to die and no override, an unsafe
# legacy unit still refuses (the file evidence is authoritative), while a
# command on a host with no legacy units reaches its own usage checks.
(
  INIT_SYSTEM=""
  unset LUNARWING_SERVICE_MANAGER
  detect_init_system() { die "could not detect a supported service manager"; }
  status_tenant() { printf 'status\n' >>"$fixture/destructive"; }
  expect_refusal detection-failure-keeps-refusal main status alice
)
(
  INIT_SYSTEM=""
  unset LUNARWING_SERVICE_MANAGER
  detect_init_system() { die "could not detect a supported service manager"; }
  mv -- "$OPENRC_INITD_DIR/lunarwing-proxy-deadline" "$fixture/parked-unit"
  # die() exits the shell, so main runs in an inner subshell whose status the
  # || captures; a plain call would trip the harness errexit.
  rc=0
  ( main configure-weechat-relay ) >"$fixture/usage.log" 2>&1 || rc=$?
  mv -- "$fixture/parked-unit" "$OPENRC_INITD_DIR/lunarwing-proxy-deadline"
  [[ "$rc" != 0 ]] || fail 'configure-weechat-relay without a name succeeded'
  grep -Fq 'usage:' "$fixture/usage.log" \
    || fail 'configure-weechat-relay omitted its usage error'
  if grep -Fq 'migration required' "$fixture/usage.log"; then
    fail 'detection failure became a false migration refusal'
  fi
)

# FV-2 (EQ-6 propagation): ensure_init_system's detection-failure return must
# reach tenant_writers_active's '|| return 2'. With detection stubbed to die
# and no override, the writer probe is unknown (2) — without the propagation
# (mutant M6) INIT_SYSTEM stays empty yet the probe falls into the OpenRC
# lane, where the stubbed 'started' status would read as an active writer (0).
(
  INIT_SYSTEM=""
  unset LUNARWING_SERVICE_MANAGER
  detect_init_system() { die "could not detect a supported service manager"; }
  mv -- "$OPENRC_INITD_DIR/lunarwing-proxy-deadline" "$fixture/parked-unit"
  rm -f -- "$fixture/service-calls"
  rc=0
  tenant_writers_active alice >"$fixture/writers.log" 2>&1 || rc=$?
  mv -- "$fixture/parked-unit" "$OPENRC_INITD_DIR/lunarwing-proxy-deadline"
  [[ "$rc" == 2 ]] || fail "detection failure classified writers as $rc, not unknown (2)"
  [[ ! -e "$fixture/service-calls" ]] \
    || fail 'detection failure reached rc-service in the writer probe'
)

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

# EQ-6: with no legacy unit present, restore's daemon probe classification is
# fail-closed: indeterminate OpenRC status codes (1, 32) refuse; only a
# positively-stopped daemon (rc 3) reaches pg_restore. The subshells sit in an
# if-condition so die()'s subshell exit cannot trip the harness errexit.
rm -f -- "$OPENRC_INITD_DIR/lunarwing-proxy-deadline"
for probe_rc in 1 32; do
  if (
    rc-service() { return "$probe_rc"; }
    restore_tenant_postgres alice "$fixture/backup.dump" true
  ) >"$fixture/probe.log" 2>&1; then
    fail "restore proceeded with indeterminate probe rc=$probe_rc"
  fi
  grep -Fq "cannot confirm" "$fixture/probe.log" \
    || fail "restore rc=$probe_rc refused without the fail-closed reason"
  [[ ! -e "$fixture/restore-ran" ]] || fail "restore rc=$probe_rc ran pg_restore"
done
if ! (
  rc-service() { return 3; }
  _ctr() {
    if [[ "$*" == *pg_restore* ]]; then
      printf 'restore\n' >>"$fixture/restore-ran"
    else
      printf 'true\n'
    fi
  }
  restore_tenant_postgres alice "$fixture/backup.dump" true
) >"$fixture/probe.log" 2>&1; then
  fail 'restore with positively-stopped daemon (rc 3) was refused'
fi
[[ -e "$fixture/restore-ran" ]] || fail 'restore with rc 3 did not reach pg_restore'

# FL-2: the systemd restore probe classifies by rc AND printed state — a
# crash-looping Restart=always unit reads rc 3 while activating/deactivating
# and must be indeterminate, never "stopped". The stub systemctl on PATH makes
# the branch reachable where no real systemd exists; _systemctl_user is
# stubbed to the matrix row.
mkdir -p "$fixture/bin"
printf '#!/bin/sh\nexit 0\n' >"$fixture/bin/systemctl"
chmod +x "$fixture/bin/systemctl"
while IFS=, read -r probe_rc probe_state probe_expect; do
  rm -f -- "$fixture/restore-ran"
  (
    INIT_SYSTEM=systemd
    PATH="$fixture/bin:$PATH"
    _systemctl_user() { printf '%s\n' "$probe_state"; return "$probe_rc"; }
    _ctr() {
      if [[ "$*" == *pg_restore* ]]; then
        printf 'restore\n' >>"$fixture/restore-ran"
      else
        printf 'true\n'
      fi
    }
    rc=0
    ( restore_tenant_postgres alice "$fixture/backup.dump" true ) \
      >"$fixture/probe.log" 2>&1 || rc=$?
    printf '%s' "$rc" >"$fixture/probe.rc"
  )
  rc="$(cat "$fixture/probe.rc")"
  case "$probe_expect" in
    proceeds)
      [[ "$rc" == 0 && -e "$fixture/restore-ran" ]] \
        || fail "systemd rc=$probe_rc state=$probe_state: restore did not proceed"
      ;;
    active)
      [[ "$rc" != 0 && ! -e "$fixture/restore-ran" ]] \
        || fail "systemd rc=$probe_rc state=$probe_state: active daemon reached pg_restore"
      grep -Fq 'stop the daemon first' "$fixture/probe.log" \
        || fail "systemd rc=$probe_rc state=$probe_state: wrong refusal"
      ;;
    refuse)
      [[ "$rc" != 0 && ! -e "$fixture/restore-ran" ]] \
        || fail "systemd rc=$probe_rc state=$probe_state: indeterminate state reached pg_restore"
      grep -Fq 'cannot confirm' "$fixture/probe.log" \
        || fail "systemd rc=$probe_rc state=$probe_state: refused without the fail-closed reason"
      ;;
  esac
done <<'MATRIX'
0,active,active
3,inactive,proceeds
3,failed,proceeds
3,activating,refuse
3,deactivating,refuse
4,inactive,proceeds
1,,refuse
MATRIX

# FL-3/FL-4 stop classification: only a truly absent unit is "already
# stopped" (EQ-2). An installed unit whose stop fails, and a present but
# symlinked unit (not mt-admin's to classify), both fail the stop; an empty
# init.d stops nothing and succeeds without a single rc-service call.
(
  OPENRC_INITD_DIR="$fixture/empty-initd"
  mkdir -p "$OPENRC_INITD_DIR"
  rm -f -- "$fixture/service-calls"
  rc=0
  ( stop_tenant_openrc alice ) >"$fixture/stop.log" 2>&1 || rc=$?
  [[ "$rc" == 0 ]] || fail 'empty init.d did not stop cleanly'
  [[ ! -e "$fixture/service-calls" ]] || fail 'empty init.d made rc-service calls'
)
(
  OPENRC_INITD_DIR="$fixture/failing-initd"
  mkdir -p "$OPENRC_INITD_DIR"
  printf '# unit\n' >"$OPENRC_INITD_DIR/lunarwing-alice"
  chmod 0755 "$OPENRC_INITD_DIR/lunarwing-alice"
  rc-service() { printf 'rc-service %s\n' "$*" >>"$fixture/service-calls"; return 1; }
  rm -f -- "$fixture/service-calls"
  rc=0
  ( stop_tenant_openrc alice ) >"$fixture/stop.log" 2>&1 || rc=$?
  [[ "$rc" == 1 ]] || fail 'a failed stop of an installed unit did not fail the stop'
  grep -Fq 'lunarwing-alice stop' "$fixture/service-calls" \
    || fail 'installed unit was never asked to stop'
)
(
  OPENRC_INITD_DIR="$fixture/symlink-initd"
  mkdir -p "$OPENRC_INITD_DIR"
  ln -s "$fixture/missing-target" "$OPENRC_INITD_DIR/lunarwing-alice"
  rm -f -- "$fixture/service-calls"
  rc=0
  ( stop_tenant_openrc alice ) >"$fixture/stop.log" 2>&1 || rc=$?
  [[ "$rc" == 1 ]] || fail 'a symlinked unit counted as already stopped'
  [[ ! -e "$fixture/service-calls" ]] \
    || fail 'symlinked unit reached rc-service instead of being refused'
  grep -Fq 'not a regular init script' "$fixture/stop.log" \
    || fail 'symlinked unit refusal did not name the reason'
)
# FV-1: the symlink half of FL-3 needs a live target — the dangling-symlink
# fixture above fails the -f test on its own, leaving the refusal's '-L' half
# unpinned (mutant N3b). A symlink to an existing regular 0755 script must
# refuse identically: rc 1, the named reason, and no rc-service call.
(
  OPENRC_INITD_DIR="$fixture/live-symlink-initd"
  mkdir -p "$OPENRC_INITD_DIR"
  printf '# real init script\n' >"$fixture/real-init-script"
  chmod 0755 "$fixture/real-init-script"
  ln -s "$fixture/real-init-script" "$OPENRC_INITD_DIR/lunarwing-alice"
  rm -f -- "$fixture/service-calls"
  rc=0
  ( stop_tenant_openrc alice ) >"$fixture/stop.log" 2>&1 || rc=$?
  [[ "$rc" == 1 ]] || fail 'a symlink to a real init script counted as already stopped'
  [[ ! -e "$fixture/service-calls" ]] \
    || fail 'symlink-to-script unit reached rc-service instead of being refused'
  grep -Fq 'not a regular init script' "$fixture/stop.log" \
    || fail 'symlink-to-script refusal did not name the reason'
)

# FL-6 (WI#393): openrc_proxy_unit_is_safe must check equality, not
# presence — a unit carrying the contract line plus extra content is NOT
# safe. Left in place it would be sourced by OpenRC without ever being
# archived or refused. Rejected extra content: a legacy inline --tensorzero
# flag (not -env), command substitution, or a backtick on any line.
safe_unit="$fixture/safe-proxy-unit"
printf '%s\n' \
  '#!/sbin/openrc-run' \
  '' \
  'description="LunarWing TensorZero proxy (alice)"' \
  '' \
  ': "${proxy_command:=/usr/bin/python3}"' \
  ': "${proxy_args:=/home/alice/lunarwing/tensorzero-proxy-configurations/lunarwing-proxy.py --port 23104 --bind 127.0.0.1 --tensorzero-env}"' \
  >"$safe_unit"
openrc_proxy_unit_is_safe "$safe_unit" \
  || fail 'a contract-conformant proxy unit was rejected'
for payload in \
  ': "${proxy_args:=proxy --tensorzero http://fixture.invalid}"' \
  'proxy_note="$(curl -s http://fixture.invalid/payload)"' \
  'proxy_note=`id`'
do
  cp "$safe_unit" "$fixture/extra-proxy-unit"
  printf '%s\n' "$payload" >>"$fixture/extra-proxy-unit"
  if openrc_proxy_unit_is_safe "$fixture/extra-proxy-unit"; then
    fail "safe-line-plus-extra-content unit classified safe: $payload"
  fi
done
# The preflight guard is the boundary that keeps OpenRC from sourcing an
# installed unit: it must accept a contract-conformant unit and refuse the
# same unit once extra content rides along (no proxy units exist in the
# fixture init.d at this point, so the payload unit is the only one).
cp "$safe_unit" "$OPENRC_INITD_DIR/lunarwing-proxy-alice"
require_safe_openrc_proxy_units \
  || fail 'a contract-conformant installed unit failed the preflight guard'
printf '%s\n' 'proxy_note=`id`' >>"$OPENRC_INITD_DIR/lunarwing-proxy-alice"
rc=0
( require_safe_openrc_proxy_units ) >"$fixture/refused.log" 2>&1 || rc=$?
[[ "$rc" == 125 ]] || fail 'safe-line-plus-payload unit passed the preflight guard'
grep -Fq 'render-units --all-proxies' "$fixture/refused.log" \
  || fail 'preflight refusal omitted the repair command'
rm -f -- "$OPENRC_INITD_DIR/lunarwing-proxy-alice"

# FL-4 (M3): enable-health-fleet sources mt-admin directly, bypassing main's
# preflight, so it carries its own proxy-unit guard — a legacy unit must
# refuse before the root health cron is armed in the migration window. The id
# stub only clears the script's root gate; the refusal comes from the real
# sourced mt-admin guard against the real legacy unit file. Detection is
# forced through the documented override because sourcing mt-admin resets
# INIT_SYSTEM and the script re-detects (a stub systemctl from the FL-2
# matrix must not flip this pin to the systemd lane).
printf ': "${proxy_args:=proxy --tensorzero http://fixture.invalid}"\n' \
  >"$OPENRC_INITD_DIR/lunarwing-proxy-deadline"
mkdir -p "$fixture/bin"
printf '#!/bin/sh\nprintf "0\\n"\n' >"$fixture/bin/id"
chmod +x "$fixture/bin/id"
health_fleet_script="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/../enable-health-fleet.sh"
rc=0
( PATH="$fixture/bin:$PATH" LUNARWING_SERVICE_MANAGER=openrc \
    LUNARWING_MT_OPENRC_INITD_DIR="$OPENRC_INITD_DIR" \
    bash "$health_fleet_script" \
    --dry-run --yes --allow-down ) >"$fixture/healthfleet.log" 2>&1 || rc=$?
[[ "$rc" != 0 ]] || fail 'enable-health-fleet armed the cron beside a legacy unit'
grep -Fq 'render-units --all-proxies' "$fixture/healthfleet.log" \
  || fail 'enable-health-fleet refusal omitted the repair command'

# FL-4 (EQ-8): an rm failure in uninstall_tenant_openrc reaches the caller —
# previously the trailing say always returned 0, making remove-tenant's
# '|| die' dead code.
(
  rm() { return 1; }
  rc=0
  ( uninstall_tenant_openrc alice ) >"$fixture/uninstall.log" 2>&1 || rc=$?
  [[ "$rc" == 1 ]] || fail 'uninstall_tenant_openrc swallowed an rm failure'
  grep -Fq 'failed to remove' "$fixture/uninstall.log" \
    || fail 'uninstall rm failure was not reported'
)

printf 'ALL TESTS PASSED\n'
