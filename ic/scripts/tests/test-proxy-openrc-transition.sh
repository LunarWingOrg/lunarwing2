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

# FL-6 (WI#393, run-107 GR-1/GR-3, run-113 HF-1/HF-3/HF-6):
# openrc_proxy_unit_is_safe is byte-for-byte equality with a fresh render from
# the shared template — the installed unit must reproduce
# _openrc_proxy_unit_body for the filename's tenant exactly, with no extra,
# missing, reordered, or payload-carrying bytes. The fixture is rendered by
# the REAL renderer (rollout-style registry/home/exec stubs) so the pins bind
# the contract itself, and renderer/template drift fails here instead of
# passing a hand-written excerpt. Every negative lives at a REAL
# lunarwing-proxy-<name> filename (HF-3): a name the filename check refuses
# first would make the content-equality pin vacuous.
(
  export PORTS_REGISTRY="$fixture/ports.json"
  export OPENRC_PROXY_BACKUP_DIR="$fixture/backup-units"
  cat >"$PORTS_REGISTRY" <<'JSON'
{"tenants":{"alice":{"enable_proxy":true,"ports":{"proxy":23104}}}}
JSON
  tenant_home() { printf '%s/%s' "$fixture" "$1"; }
  install_openrc_env_exec() { :; }
  render_proxy_openrc_unit alice || fail 'could not render the fixture unit'
  safe_unit="$OPENRC_INITD_DIR/lunarwing-proxy-alice"
  openrc_proxy_unit_is_safe "$safe_unit" \
    || fail 'a genuine renderer output failed the safe-unit contract'
  # EQ-4 under equality: re-rendering a conformant unit must not archive it.
  render_proxy_openrc_unit alice || fail 're-render of a safe unit failed'
  [[ ! -e "$OPENRC_PROXY_BACKUP_DIR" ]] \
    || fail 're-rendering a safe unit grew the backup archive'
  # Negatives — none of these trip the old three-pattern denylist; the
  # equality contract must catch every one (GR-1 matrix classes). rm before
  # each write so the verdict cache can never mask a negative behind a
  # same-inode truncate.
  neg_dir="$fixture/neg"
  mkdir -p "$neg_dir"
  neg_unit="$neg_dir/lunarwing-proxy-alice"
  # Control: genuine bytes at the negative path are ACCEPTED, so each refusal
  # below is attributable to the content, not the location (HF-3).
  cp "$safe_unit" "$neg_unit"
  openrc_proxy_unit_is_safe "$neg_unit" \
    || fail 'genuine bytes at the negative path were refused'
  for payload in \
    'proxy_command=/tmp/evil' \
    ': "${proxy_command:=/tmp/evil}"' \
    'command_user="root:root"' \
    ': "${proxy_args:=/tmp/evil.py --tensorzero=http://fixture.invalid/x}"' \
    'start_pre() { /tmp/evil; }' \
    'proxy_note="$(id)"' \
    'proxy_note=`id`'
  do
    rm -f -- "$neg_unit"
    cp "$safe_unit" "$neg_unit"
    printf '%s\n' "$payload" >>"$neg_unit"
    if openrc_proxy_unit_is_safe "$neg_unit"; then
      fail "safe-template-plus-extra-content unit classified safe: $payload"
    fi
  done
  # NUL byte: bash line readers stop at NUL while /bin/sh skips it and
  # executes what follows, so a NUL-hidden payload must fail the byte
  # comparison outright (HF-3).
  rm -f -- "$neg_unit"
  cp "$safe_unit" "$neg_unit"
  printf 'proxy_note="ok"\0; echo PWNED\n' >>"$neg_unit"
  if openrc_proxy_unit_is_safe "$neg_unit"; then
    fail 'a NUL-hidden payload line classified safe'
  fi
  # A same-line-count substitution naming a foreign tenant, an over-long port
  # (HF-6), and a truncated unit all fail equality without tripping any
  # denylist pattern.
  rm -f -- "$neg_unit"
  sed 's/before lunarwing-alice/before lunarwing-other/' "$safe_unit" \
    >"$neg_unit"
  if openrc_proxy_unit_is_safe "$neg_unit"; then
    fail 'a unit pointing at a foreign tenant classified safe'
  fi
  rm -f -- "$neg_unit"
  sed 's/--port 23104/--port 231041/' "$safe_unit" >"$neg_unit"
  if openrc_proxy_unit_is_safe "$neg_unit"; then
    fail 'a unit with an over-long port classified safe'
  fi
  rm -f -- "$neg_unit"
  head -n -3 "$safe_unit" >"$neg_unit"
  if openrc_proxy_unit_is_safe "$neg_unit"; then
    fail 'a truncated unit classified safe'
  fi
  # HU-3: the two load-bearing refusals pinned directly. A symlink at a real
  # unit name pointing at genuine safe bytes must be refused — cmp follows
  # symlinks, so without the `! -L` guard a link to a safe unit would pass.
  rm -f -- "$neg_unit"
  ln -s "$safe_unit" "$neg_unit"
  if openrc_proxy_unit_is_safe "$neg_unit"; then
    fail 'a symlink to genuine bytes classified safe'
  fi
  rm -f -- "$neg_unit"
  # A correctly rendered body for an upper-case name must be refused: the
  # filename's tenant must survive sanitize_name unchanged, so a unit whose
  # name case-folds onto another tenant never reaches the byte comparison.
  upper_unit="$neg_dir/lunarwing-proxy-Alice"
  _openrc_proxy_unit_body Alice 23104 >"$upper_unit" \
    || fail 'could not render the upper-case fixture body'
  if openrc_proxy_unit_is_safe "$upper_unit"; then
    fail 'an upper-case-name unit classified safe'
  fi
  rm -f -- "$upper_unit"
  # HF-1: renderer and checker resolve the interpreter through one
  # PATH-independent helper. A render made while the caller's PATH shadows
  # python3 (a venv, /usr/local/bin, pyenv) must still satisfy the guard —
  # and must not grow the archive (no ping-pong repair loop).
  mkdir -p "$fixture/pyshadow"
  ln -s "$(command -v python3)" "$fixture/pyshadow/python3"
  ( _OPENRC_PROXY_PYTHON3_RESOLVED=""
    _OPENRC_PROXY_PYTHON3_RESOLVED_FROM=$'\001'
    PATH="$fixture/pyshadow:$PATH"
    render_proxy_openrc_unit alice ) \
    || fail 'render under a python3-shadowing PATH failed'
  openrc_proxy_unit_is_safe "$safe_unit" \
    || fail 'unit rendered under a shadowing PATH failed the guard'
  [[ ! -e "$OPENRC_PROXY_BACKUP_DIR" ]] \
    || fail 'a PATH flip grew the backup archive (ping-pong repair loop)'
  # HU-1: a `python3` shell function in the caller's environment (exported via
  # BASH_FUNC_python3%% or defined through BASH_ENV) must not reach the
  # resolver — `command -v` reports it as the bare word, which the
  # absolute-path allowlist refuses, flipping every guarded OpenRC operation
  # to 125 fleet-wide; `type -P` ignores functions (the hash table is flushed
  # by the PATH= prefix, pinned separately below). Clear the memo and the
  # verdict cache so the resolution really re-runs here.
  ( python3() { :; }
    _OPENRC_PROXY_PYTHON3_RESOLVED=""
    _OPENRC_PROXY_PYTHON3_RESOLVED_FROM=$'\001'
    _OPENRC_PROXY_UNIT_SAFE_CACHE=()
    openrc_proxy_unit_is_safe "$safe_unit" ) \
    || fail 'a python3 shell function in the caller flipped the safe verdict'
  # IC-1 (run-125): `type -P` DOES read bash's hash table; what keeps a stale
  # hashed python3 out of the resolution is the temporary PATH= assignment,
  # which flushes the table before the lookup. Pin that directly: a hash
  # entry pointing at a fake interpreter must not flip the safe verdict.
  mkdir -p "$fixture/pyhash"
  { echo '#!/bin/sh'; echo 'exit 0'; } >"$fixture/pyhash/python3"
  chmod 0755 "$fixture/pyhash/python3"
  ( hash -p "$fixture/pyhash/python3" python3
    _OPENRC_PROXY_PYTHON3_RESOLVED=""
    _OPENRC_PROXY_PYTHON3_RESOLVED_FROM=$'\001'
    _OPENRC_PROXY_UNIT_SAFE_CACHE=()
    openrc_proxy_unit_is_safe "$safe_unit" ) \
    || fail 'a stale python3 hash entry in the caller flipped the safe verdict'
  # HF-1 override: LUNARWING_MT_PROXY_PYTHON3 pins the interpreter for both
  # sides. A unit rendered under the override is accepted while the override
  # is set and refused once it is unset (the documented one-time refusal);
  # re-rendering then repairs it. Each replacement archives exactly the unit
  # that fails the contract under the resolution in force at that render.
  LUNARWING_MT_PROXY_PYTHON3=/opt/py3/bin/python3 render_proxy_openrc_unit alice \
    || fail 'render under LUNARWING_MT_PROXY_PYTHON3 failed'
  grep -Fq ': "${proxy_command:=/opt/py3/bin/python3}"' "$safe_unit" \
    || fail 'override interpreter did not reach the rendered unit'
  [[ "$(find "$OPENRC_PROXY_BACKUP_DIR" -maxdepth 1 -name 'lunarwing-proxy-*' | wc -l)" == 1 ]] \
    || fail 'override render did not archive exactly the previous unit'
  LUNARWING_MT_PROXY_PYTHON3=/opt/py3/bin/python3 openrc_proxy_unit_is_safe "$safe_unit" \
    || fail 'override-rendered unit refused under the same override'
  if openrc_proxy_unit_is_safe "$safe_unit"; then
    fail 'override-rendered unit accepted after the override was unset'
  fi
  render_proxy_openrc_unit alice || fail 'repair render after the override failed'
  openrc_proxy_unit_is_safe "$safe_unit" \
    || fail 'repaired unit failed the guard'
  [[ "$(find "$OPENRC_PROXY_BACKUP_DIR" -maxdepth 1 -name 'lunarwing-proxy-*' | wc -l)" == 2 ]] \
    || fail 'the one-time refusal repair did not archive exactly the mismatched unit'
  rm -rf -- "$OPENRC_PROXY_BACKUP_DIR"
  # HF-4: the interpreter allowlist rejects shell syntax before any
  # substitution happens (an & would otherwise expand under bash 5.2+
  # patsub_replacement), and the refusal leaves the installed unit untouched.
  if ( LUNARWING_MT_PROXY_PYTHON3='/tmp/ev&il/python3' render_proxy_openrc_unit alice ) \
    >"$fixture/ampersand.log" 2>&1; then
    fail 'an & in LUNARWING_MT_PROXY_PYTHON3 rendered a unit'
  fi
  openrc_proxy_unit_is_safe "$safe_unit" \
    || fail 'a refused override render still touched the installed unit'
  # HF-6: the renderer accepts only a 1-5 ASCII-digit port from the registry.
  if ( ports_get() { printf '231041\n'; }
       render_proxy_openrc_unit alice ) >"$fixture/port.log" 2>&1; then
    fail 'a 6-digit registry port rendered a unit'
  fi
  openrc_proxy_unit_is_safe "$safe_unit" \
    || fail 'a refused port render still touched the installed unit'
  # The preflight guard is the boundary that keeps OpenRC from sourcing an
  # installed unit: it must accept the genuine render and refuse the same
  # unit once extra content rides along (it is the only proxy unit here).
  require_safe_openrc_proxy_units \
    || fail 'a genuine renderer output failed the preflight guard'
  printf '%s\n' 'proxy_note=`id`' >>"$safe_unit"
  rc=0
  ( require_safe_openrc_proxy_units ) >"$fixture/refused.log" 2>&1 || rc=$?
  [[ "$rc" == 125 ]] || fail 'safe-render-plus-payload unit passed the preflight guard'
  grep -Fq 'render-units --all-proxies' "$fixture/refused.log" \
    || fail 'preflight refusal omitted the repair command'
  rm -f -- "$safe_unit"
)

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
