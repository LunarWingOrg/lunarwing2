#!/usr/bin/env bash
# Test harness for BUG-005 cgroup isolation in lunarwing-mt-admin.sh.
# Verifies:
#  - per-tenant systemd workload/build slices carry memory/CPU/task caps
#  - the daemon unit gets its own MemoryHigh/MemoryMax/TasksMax
#  - operator env overrides flow through to rendered units
#  - OpenRC daemon unit carries rc_cgroup_settings cgroup v2 ceilings
#    (memory.high/memory.max/cpu.max/pids.max) with conf.d-overridable knobs
#  - _run_tenant_build scopes builds on systemd, bounds them with a temporary
#    cgroup v2 dir on other inits when available, and nice's them otherwise,
#    passing -j$BUILD_JOBS to cargo
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_SCRIPT="$SCRIPT_DIR/../lunarwing-mt-admin.sh"

# CI containers have no init-system markers (/run/systemd/system,
# /run/openrc/softlevel) and detect_init_system() dies without one. Pin the
# override so the renderers are deterministic everywhere. "systemd" keeps the
# OpenRC babysitter renderer a no-op, as on any systemd host.
export LUNARWING_SERVICE_MANAGER=systemd

# shellcheck source=../lunarwing-mt-admin.sh
source "$ADMIN_SCRIPT"

failures=0
assert_contains() { # <label> <file> <needle>
  if grep -qF -- "$3" "$2" 2>/dev/null; then
    echo "  PASS: $1"
  else
    echo "  FAIL: $1 (missing [$3] in $2)"
    failures=$((failures + 1))
  fi
}
assert_not_contains() { # <label> <file> <needle>
  if grep -qF -- "$3" "$2" 2>/dev/null; then
    echo "  FAIL: $1 (unexpected [$3] in $2)"
    failures=$((failures + 1))
  else
    echo "  PASS: $1"
  fi
}
assert_eq() { # <label> <expected> <actual>
  if [[ "$2" == "$3" ]]; then
    echo "  PASS: $1"
  else
    echo "  FAIL: $1 (want [$2], got [$3])"
    failures=$((failures + 1))
  fi
}
assert_fails() { # <label> <command> [args...]
  local label="$1"
  shift
  if ( "$@" ) >/dev/null 2>&1; then
    echo "  FAIL: $label (unexpected success)"
    failures=$((failures + 1))
  else
    echo "  PASS: $label"
  fi
}
assert_files_equal() { # <label> <expected> <actual>
  if cmp -s -- "$2" "$3"; then
    echo "  PASS: $1"
  else
    echo "  FAIL: $1 (files differ)"
    failures=$((failures + 1))
  fi
}

# ── Fixture ──────────────────────────────────────────────────────────────────
MT_FIXTURE="$(mktemp -d)"
trap 'rm -rf "$MT_FIXTURE"' EXIT

tenant_home()        { echo "$MT_FIXTURE/$1"; }
tenant_repo()        { echo "$MT_FIXTURE/$1/lunarwing/ic"; }
tenant_env_dir()     { echo "$MT_FIXTURE/$1/lunarwing/env"; }
tenant_state_dir()   { echo "$MT_FIXTURE/$1/lunarwing/state"; }
tenant_log_dir()     { echo "$MT_FIXTURE/$1/lunarwing/logs"; }
tenant_run_dir()     { echo "$MT_FIXTURE/$1/lunarwing/run"; }
tenant_lw_root()     { echo "$MT_FIXTURE/$1/lunarwing"; }
tenant_quadlet_dir() { echo "$MT_FIXTURE/$1/.config/containers/systemd"; }
ports_get() { echo 0; }
tenant_weechat_enabled() { return 1; }
tenant_proxy_enabled()   { return 1; }
tenant_darkirc_enabled() { return 1; }
ensure_container_runtime() { :; }
install_openrc_env_exec() { :; }
chown() { :; }
chmod() { :; }

mkdir -p "$(tenant_repo t1)"
export OPENRC_INITD_DIR="$MT_FIXTURE/init.d"
export OPENRC_CONFD_DIR="$MT_FIXTURE/conf.d"

# ── systemd unit rendering ───────────────────────────────────────────────────
echo "== systemd unit rendering =="
render_tenant_systemd_units t1
unit_dir="$MT_FIXTURE/t1/.config/systemd/user"

assert_contains "workload slice cap"  "$unit_dir/lunarwing-workloads.slice" "MemoryMax=8G"
assert_contains "workload slice high" "$unit_dir/lunarwing-workloads.slice" "MemoryHigh=6G"
assert_contains "workload slice cpu"  "$unit_dir/lunarwing-workloads.slice" "CPUQuota=800%"
assert_contains "workload slice pids" "$unit_dir/lunarwing-workloads.slice" "TasksMax=2048"
assert_contains "build slice cap"     "$unit_dir/lunarwing-build.slice" "MemoryMax=24G"
assert_contains "build slice high"    "$unit_dir/lunarwing-build.slice" "MemoryHigh=16G"
assert_contains "daemon unit cap"     "$unit_dir/lunarwing-t1.service" "MemoryMax=2G"
assert_contains "daemon unit high"    "$unit_dir/lunarwing-t1.service" "MemoryHigh=1536M"
assert_contains "daemon unit tasks"   "$unit_dir/lunarwing-t1.service" "TasksMax=512"

echo "== operator overrides =="
export WORKLOAD_MEMORY_MAX=12G
export DAEMON_MEMORY_MAX=3G
render_tenant_systemd_units t1
assert_contains "workload cap override" "$unit_dir/lunarwing-workloads.slice" "MemoryMax=12G"
assert_contains "daemon cap override"   "$unit_dir/lunarwing-t1.service" "MemoryMax=3G"

# ── OpenRC rendering (cgroup v2 ceilings via rc_cgroup_settings, #54) ────────
echo "== OpenRC cgroup ceilings =="
# Restore defaults (mt-admin reads the plain variables; unset trips set -u).
export WORKLOAD_MEMORY_MAX=8G
export DAEMON_MEMORY_MAX=2G
render_tenant_openrc_units t1
daemon_unit="$OPENRC_INITD_DIR/lunarwing-t1"
daemon_confd="$OPENRC_CONFD_DIR/lunarwing-t1"
assert_contains "OpenRC daemon settings var" "$daemon_unit" 'rc_cgroup_settings'
assert_contains "OpenRC daemon cleanup"      "$daemon_unit" 'rc_cgroup_cleanup="YES"'
assert_contains "OpenRC daemon memory.max"   "$daemon_unit" 'lunarwing_cgroup_memory_max:=8G'
assert_contains "OpenRC daemon memory.high"  "$daemon_unit" 'lunarwing_cgroup_memory_high:=6G'
assert_contains "OpenRC daemon cpu.max"      "$daemon_unit" 'lunarwing_cgroup_cpu_max:=800000 100000'
assert_contains "OpenRC daemon pids.max"     "$daemon_unit" 'lunarwing_cgroup_pids_max:=2048'
assert_contains "OpenRC memory.high uses converted bytes" "$daemon_unit" 'memory.high ${_lw_memory_high}'
assert_contains "OpenRC memory.max uses converted bytes"  "$daemon_unit" 'memory.max ${_lw_memory_max}'
assert_contains "OpenRC cleanup checks unified path" "$daemon_unit" '${_lw_root}/openrc.${RC_SVCNAME}'
assert_contains "OpenRC cleanup checks hybrid path"  "$daemon_unit" '${_lw_root}/unified/openrc.${RC_SVCNAME}'
assert_contains "OpenRC cleanup uses cgroup.kill"     "$daemon_unit" 'cgroup.kill'
assert_contains "OpenRC cleanup has PID fallback"     "$daemon_unit" 'kill -KILL'
assert_contains "OpenRC cleanup is bounded"           "$daemon_unit" '${_lw_tries}" -ge 5'
assert_contains "OpenRC cleanup wraps normal stop"     "$daemon_unit" 'default_stop || _lw_stop_rc=$?'
assert_contains "OpenRC conf.d managed block" "$daemon_confd" "# BEGIN lunarwing-mt-admin managed dependencies"
assert_contains "OpenRC conf.d documents knobs" "$daemon_confd" "lunarwing_cgroup_memory_max"
assert_contains "OpenRC conf.d opt-out" "$daemon_confd" "lunarwing_cgroup_limits_enabled"
assert_contains "OpenRC conf.d keeps ulimit fallback" "$daemon_confd" "rc_ulimit"
assert_not_contains "OpenRC has no systemd-only knobs" "$daemon_confd" "MemoryMax"

cat >>"$daemon_confd" <<'CONFD'
# operator settings must survive render-units and upgrades
lunarwing_cgroup_memory_high="7G"
lunarwing_cgroup_memory_max="12G"
lunarwing_cgroup_cpu_max="400000 100000"
lunarwing_cgroup_pids_max="4096"
lunarwing_cgroup_limits_enabled="true"
rc_ulimit="-v 12582912"
operator_setting="preserve-me"
CONFD

echo "== OpenRC cgroup overrides =="
export WORKLOAD_MEMORY_MAX=12G
export WORKLOAD_TASKS_MAX=4096
render_tenant_openrc_units t1
assert_contains "OpenRC memory override" "$daemon_unit" 'lunarwing_cgroup_memory_max:=12G'
assert_contains "OpenRC pids override"   "$daemon_unit" 'lunarwing_cgroup_pids_max:=4096'
assert_contains "OpenRC conf.d preserves memory.high" "$daemon_confd" 'lunarwing_cgroup_memory_high="7G"'
assert_contains "OpenRC conf.d preserves memory.max" "$daemon_confd" 'lunarwing_cgroup_memory_max="12G"'
assert_contains "OpenRC conf.d preserves cpu.max" "$daemon_confd" 'lunarwing_cgroup_cpu_max="400000 100000"'
assert_contains "OpenRC conf.d preserves pids.max" "$daemon_confd" 'lunarwing_cgroup_pids_max="4096"'
assert_contains "OpenRC conf.d preserves opt-out knob" "$daemon_confd" 'lunarwing_cgroup_limits_enabled="true"'
assert_contains "OpenRC conf.d preserves rc_ulimit" "$daemon_confd" 'rc_ulimit="-v 12582912"'
assert_contains "OpenRC conf.d preserves unrelated settings" "$daemon_confd" 'operator_setting="preserve-me"'

tenant_proxy_enabled() { return 0; }
render_tenant_openrc_units t1
assert_contains "OpenRC managed dependency updates" "$daemon_confd" 'lunarwing_rc_need="xmpp-bridge-t1 lunarwing-proxy-t1"'
tenant_proxy_enabled() { return 1; }
render_tenant_openrc_units t1
assert_contains "OpenRC stale managed dependency removed" "$daemon_confd" 'lunarwing_rc_need="xmpp-bridge-t1"'
assert_eq "OpenRC conf.d has one managed block" "1" "$(grep -Fc '# BEGIN lunarwing-mt-admin managed dependencies' "$daemon_confd")"
cp "$daemon_confd" "$MT_FIXTURE/confd-before-idempotent-render"
render_tenant_openrc_units t1
assert_files_equal "OpenRC conf.d rerender is idempotent" "$MT_FIXTURE/confd-before-idempotent-render" "$daemon_confd"

legacy_confd="$OPENRC_CONFD_DIR/lunarwing-legacy"
cat >"$legacy_confd" <<'CONFD'
# Auto-generated by lunarwing-mt-admin.sh for tenant: legacy
lunarwing_rc_need="xmpp-bridge-legacy"

lunarwing_cgroup_memory_max="10G"
CONFD
_render_openrc_lunarwing_confd legacy "xmpp-bridge-legacy lunarwing-proxy-legacy"
assert_contains "OpenRC legacy conf.d migrates managed dependency" "$legacy_confd" 'lunarwing_rc_need="xmpp-bridge-legacy lunarwing-proxy-legacy"'
assert_contains "OpenRC legacy conf.d preserves override" "$legacy_confd" 'lunarwing_cgroup_memory_max="10G"'

touch "$MT_FIXTURE/confd-symlink-target"
ln -s "$MT_FIXTURE/confd-symlink-target" "$OPENRC_CONFD_DIR/lunarwing-symlink"
assert_fails "OpenRC conf.d rejects symlink target" _render_openrc_lunarwing_confd symlink xmpp-bridge-symlink

cp "$daemon_confd" "$OPENRC_CONFD_DIR/lunarwing-malformed"
printf '%s\n' '# BEGIN lunarwing-mt-admin managed dependencies' >>"$OPENRC_CONFD_DIR/lunarwing-malformed"
cp "$OPENRC_CONFD_DIR/lunarwing-malformed" "$MT_FIXTURE/malformed-before-render"
assert_fails "OpenRC conf.d rejects duplicate managed block" _render_openrc_lunarwing_confd malformed xmpp-bridge-malformed
assert_files_equal "OpenRC malformed conf.d remains unchanged" "$MT_FIXTURE/malformed-before-render" "$OPENRC_CONFD_DIR/lunarwing-malformed"

command chmod 0640 "$daemon_confd"
unset -f chmod
_render_openrc_lunarwing_confd t1 "xmpp-bridge-t1"
assert_eq "OpenRC conf.d preserves mode" "640" "$(stat -c %a "$daemon_confd")"

export WORKLOAD_TASKS_MAX=2048
export WORKLOAD_MEMORY_MAX=8G
export WORKLOAD_CPU_QUOTA=max
render_tenant_openrc_units t1

if (
  set -euo pipefail
  yesno() { [[ "${1:-}" == true || "${1:-}" == YES ]]; }
  eerror() { :; }
  ewarn() { :; }
  # shellcheck disable=SC1090
  source "$daemon_unit"
  cpu_root="$MT_FIXTURE/cpu-optional-cgroup"
  RC_SVCNAME=lunarwing-cpu-optional-test
  cpu_cg="$cpu_root/openrc.$RC_SVCNAME"
  mkdir -p "$cpu_cg"
  : >"$cpu_root/cgroup.controllers"
  printf '%s\n' 6442450944 >"$cpu_cg/memory.high"
  printf '%s\n' 8589934592 >"$cpu_cg/memory.max"
  printf '%s\n' 2048 >"$cpu_cg/pids.max"
  [[ -z "$lunarwing_cgroup_cpu_max" ]]
  _lunarwing_validate_service_cgroup "$cpu_root"
); then
  echo "  PASS: optional OpenRC CPU limit omission preserves other ceilings"
else
  echo "  FAIL: optional OpenRC CPU limit omission blocks startup"
  failures=$((failures + 1))
fi

export WORKLOAD_CPU_QUOTA=800%
render_tenant_openrc_units t1

echo "== OpenRC runtime helpers =="
if (
  set -euo pipefail
  yesno() { [[ "${1:-}" == true || "${1:-}" == YES ]]; }
  eerror() { :; }
  ewarn() { :; }
  rc_cgroup_settings=""
  # shellcheck disable=SC1090
  source "$daemon_unit"
  [[ "$(_lunarwing_cgroup_memory_bytes 6G)" == 6442450944 ]]
  [[ "$(_lunarwing_cgroup_memory_bytes 8G)" == 8589934592 ]]
  [[ "$rc_cgroup_settings" == *"memory.high 6442450944"* ]]
  [[ "$rc_cgroup_settings" == *"memory.max 8589934592"* ]]
  ! _lunarwing_cgroup_memory_bytes max >/dev/null 2>&1
); then
  echo "  PASS: OpenRC service memory conversion"
else
  echo "  FAIL: OpenRC service memory conversion"
  failures=$((failures + 1))
fi

if (
  set -euo pipefail
  yesno() { [[ "${1:-}" == true || "${1:-}" == YES ]]; }
  eerror() { :; }
  ewarn() { :; }
  # shellcheck disable=SC1090
  source "$daemon_unit"
  : >"$MT_FIXTURE/failed-default-stop-cleanup"
  default_stop() { return 7; }
  _lunarwing_service_cgroup_cleanup() {
    printf '%s\n' called >"$MT_FIXTURE/failed-default-stop-cleanup"
    _LUNARWING_CGROUP_CLEANUP_HANDLED=true
  }
  stop_rc=0
  stop || stop_rc=$?
  [[ "$stop_rc" == 7 ]]
  grep -qF called "$MT_FIXTURE/failed-default-stop-cleanup"
); then
  echo "  PASS: OpenRC cleanup runs when default stop fails"
else
  echo "  FAIL: OpenRC cleanup skipped after default stop failure"
  failures=$((failures + 1))
fi

if (
  set -euo pipefail
  yesno() { [[ "${1:-}" == true || "${1:-}" == YES ]]; }
  eerror() { :; }
  ewarn() { :; }
  checkpath() { :; }
  default_stop() { return 0; }
  export lunarwing_cgroup_memory_max=invalid
  # shellcheck disable=SC1090
  source "$daemon_unit"
  [[ -n "$_lw_cgroup_config_error" ]]
  [[ -z "$rc_cgroup_settings" ]]
  ! start_pre
  RC_SVCNAME=lunarwing-invalid-config-test
  stop
); then
  echo "  PASS: invalid OpenRC memory config blocks start without blocking stop"
else
  echo "  FAIL: invalid OpenRC memory config handling"
  failures=$((failures + 1))
fi

if (
  set -euo pipefail
  yesno() { [[ "${1:-}" == true || "${1:-}" == YES ]]; }
  eerror() { :; }
  ewarn() { :; }
  # shellcheck disable=SC1090
  source "$daemon_unit"
  runtime_root="$MT_FIXTURE/runtime-cgroup"
  RC_SVCNAME=lunarwing-runtime-test
  runtime_cg="$runtime_root/openrc.$RC_SVCNAME"
  mkdir -p "$runtime_cg"
  printf '%s\n' 'cpu memory pids' >"$runtime_root/cgroup.controllers"
  printf '%s\n' 6442450944 >"$runtime_cg/memory.high"
  printf '%s\n' 8589934592 >"$runtime_cg/memory.max"
  printf '%s\n' '800000 100000' >"$runtime_cg/cpu.max"
  printf '%s\n' 2048 >"$runtime_cg/pids.max"
  : >"$runtime_cg/cgroup.procs"
  _lunarwing_validate_service_cgroup "$runtime_root"
  rm "$runtime_cg/memory.max"
  ! _lunarwing_validate_service_cgroup "$runtime_root"
  printf '%s\n' 8589934592 >"$runtime_cg/memory.max"

  printf '%s\n' 4242 >"$runtime_cg/cgroup.procs"
  : >"$MT_FIXTURE/runtime-kills"
  kill() {
    printf '%s\n' "$*" >>"$MT_FIXTURE/runtime-kills"
    : >"$runtime_cg/cgroup.procs"
  }
  sleep() { :; }
  _lunarwing_service_cgroup_cleanup "$runtime_root"
  grep -qF -- '-KILL 4242' "$MT_FIXTURE/runtime-kills"
  [[ "$_LUNARWING_CGROUP_CLEANUP_HANDLED" == true ]]

  : >"$runtime_root/cgroup.procs"
  : >"$runtime_cg/cgroup.procs"
  : >"$runtime_cg/cgroup.kill"
  _lunarwing_service_cgroup_cleanup "$runtime_root"
  [[ "$(<"$runtime_cg/cgroup.kill")" == 1 ]]
  [[ "$(<"$runtime_root/cgroup.procs")" == "$$" ]]

  rm -rf "$runtime_root"
  runtime_cg="$runtime_root/unified/openrc.$RC_SVCNAME"
  mkdir -p "$runtime_cg"
  : >"$runtime_cg/cgroup.procs"
  _lunarwing_service_cgroup_cleanup "$runtime_root"
  [[ "$_LUNARWING_CGROUP_CLEANUP_HANDLED" == true ]]

  printf '%s\n' 5252 >"$runtime_cg/cgroup.procs"
  kill() { return 1; }
  ! _lunarwing_service_cgroup_cleanup "$runtime_root"
); then
  echo "  PASS: OpenRC service cleanup fallback"
else
  echo "  FAIL: OpenRC service cleanup fallback"
  failures=$((failures + 1))
fi

if (
  : >"$MT_FIXTURE/failed-tenant-stop-calls"
  : >"$OPENRC_INITD_DIR/lunarwing-vision-t1"
  rc-service() {
    printf '%s\n' "$*" >>"$MT_FIXTURE/failed-tenant-stop-calls"
    [[ "$1 $2" != "lunarwing-t1 stop" ]]
  }
  ! stop_tenant_openrc t1 >/dev/null 2>&1
  grep -qF 'lunarwing-pg-t1 stop' "$MT_FIXTURE/failed-tenant-stop-calls"
  grep -qF 'lunarwing-vision-t1 stop' "$MT_FIXTURE/failed-tenant-stop-calls"
); then
  echo "  PASS: OpenRC tenant stop propagates failure after remaining shutdown"
else
  echo "  FAIL: OpenRC tenant stop hides failure or aborts remaining shutdown"
  failures=$((failures + 1))
fi

# ── _run_tenant_build bounding ───────────────────────────────────────────────
echo "== build bounding =="
id() { echo 1001; }
CAPTURE="$MT_FIXTURE/capture"

sudo() { echo "SUDO $*" >>"$CAPTURE"; }

: >"$CAPTURE"
export INIT_SYSTEM=openrc
_openrc_build_cgroup_supported() { return 1; } # no cgroup v2 -> nice fallback
_run_tenant_build t1 "cd /x && cargo build --profile release --bin lunarwing -j$BUILD_JOBS"
assert_contains "openrc build is nice'd" "$CAPTURE" "nice -n 10"
assert_contains "openrc build has -j"    "$CAPTURE" "-j$BUILD_JOBS"

echo "== build cgroup validation =="
assert_eq "16G -> cgroup bytes" "17179869184" "$(_cgroup_memory_bytes 16G)"
assert_eq "24G -> cgroup bytes" "25769803776" "$(_cgroup_memory_bytes 24G)"
assert_eq "lowercase size suffix" "1073741824" "$(_cgroup_memory_bytes 1g)"
assert_fails "cgroup size rejects max" _cgroup_memory_bytes max
assert_fails "cgroup size rejects zero" _cgroup_memory_bytes 0
assert_fails "cgroup size rejects overflow" _cgroup_memory_bytes 999999999999999999T

fixture_cgroup_root="$MT_FIXTURE/cgroup-root"
mkdir -p "$fixture_cgroup_root"
printf '%s\n' 'cpu pids' >"$fixture_cgroup_root/cgroup.controllers"
: >"$fixture_cgroup_root/cgroup.subtree_control"
assert_fails "build cgroup requires memory controller" _openrc_build_memory_controller_ready "$fixture_cgroup_root"
printf '%s\n' 'cpu memory pids' >"$fixture_cgroup_root/cgroup.controllers"
_openrc_build_memory_controller_ready "$fixture_cgroup_root"
assert_contains "build cgroup enables memory delegation" "$fixture_cgroup_root/cgroup.subtree_control" memory

fixture_build_cgroup="$fixture_cgroup_root/lunarwing-build-t1"
mkdir -p "$fixture_build_cgroup"
: >"$fixture_build_cgroup/memory.high"
: >"$fixture_build_cgroup/memory.max"
_openrc_build_cgroup_set_memory_limits "$fixture_build_cgroup"
assert_eq "build memory.high effective bytes" "17179869184" "$(<"$fixture_build_cgroup/memory.high")"
assert_eq "build memory.max effective bytes" "25769803776" "$(<"$fixture_build_cgroup/memory.max")"
rm "$fixture_build_cgroup/memory.max"
assert_fails "build cgroup rejects missing hard limit" _openrc_build_cgroup_set_memory_limits "$fixture_build_cgroup"
: >"$fixture_build_cgroup/memory.max"
export BUILD_MEMORY_HIGH=25G BUILD_MEMORY_MAX=24G
assert_fails "build cgroup rejects high above max" _openrc_build_cgroup_set_memory_limits "$fixture_build_cgroup"
export BUILD_MEMORY_HIGH=16G BUILD_MEMORY_MAX=24G

if (
  actual_cgroup_root="$fixture_cgroup_root/actual-helper"
  mkdir -p "$actual_cgroup_root"
  printf '%s\n' 'cpu memory pids' >"$actual_cgroup_root/cgroup.controllers"
  printf '%s\n' memory >"$actual_cgroup_root/cgroup.subtree_control"
  _openrc_cgroup_root() { printf '%s\n' "$actual_cgroup_root"; }
  mkdir() {
    local target="${*: -1}"
    command mkdir "$@"
    if [[ "$target" == "$actual_cgroup_root/lunarwing-build-t1" ]]; then
      : >"$target/memory.high"
      : >"$target/memory.max"
      : >"$target/cgroup.procs"
    fi
  }
  sudo() {
    [[ "$1" == -u ]] || return 1
    shift 2
    "$@"
  }
  chown() { :; } # syncdir delegation is a no-op for the fixture user
  _openrc_build_cgroup_cleanup() { rm -rf "$1"; }
  actual_rc=0
  _run_tenant_build_cgroup t1 "exit 2" || actual_rc=$?
  [[ "$actual_rc" == 2 ]]
  [[ "$_OPENRC_BUILD_CGROUP_SETUP_COMPLETE" == true ]]
); then
  echo "  PASS: actual bounded helper preserves build status 2"
else
  echo "  FAIL: actual bounded helper status propagation"
  failures=$((failures + 1))
fi

if (
  denied_root="$MT_FIXTURE/cgroup-root-denied"
  mkdir -p "$denied_root"
  printf '%s\n' 'cpu memory pids' >"$denied_root/cgroup.controllers"
  : >"$denied_root/cgroup.subtree_control"
  _openrc_cgroup_root() { printf '%s\n' "$denied_root"; }
  _openrc_build_cgroup_supported() { return 0; }
  mkdir() {
    local target="${*: -1}"
    command mkdir "$@"
    if [[ "$target" == "$denied_root/lunarwing-build-t1" ]]; then
      : >"$target/memory.high"
      : >"$target/memory.max"
      : >"$target/cgroup.procs"
      # Launcher-side placement into the build cgroup denied: an unwritable
      # cgroup.procs makes the privileged write fail before the gated wrapper
      # is released, so the build may only run through the nice fallback.
      chmod 0444 "$target/cgroup.procs"
    fi
  }
  sudo() {
    [[ "$1" == -u ]] || return 1
    shift 2
    echo "SUDO $*" >>"$CAPTURE"
    "$@"
  }
  chown() { :; } # syncdir delegation is a no-op for the fixture user
  _openrc_build_cgroup_cleanup() { rm -rf "$1"; }
  export CAPTURE
  : >"$CAPTURE"
  denied_rc=0
  _run_tenant_build t1 'echo FALLBACK-RAN >>"$CAPTURE"' || denied_rc=$?
  [[ "$denied_rc" == 0 ]]
  [[ "${_OPENRC_BUILD_CGROUP_SETUP_COMPLETE:-unset}" == false ]]
  # The gated wrapper is killed before exec, so the build command runs exactly
  # once, through the nice-only fallback.
  [[ "$(grep -cF 'FALLBACK-RAN' "$CAPTURE")" == 1 ]]
  grep -qF 'nice -n 10' "$CAPTURE"
); then
  echo "  PASS: cgroup placement denial falls back to nice build"
else
  echo "  FAIL: cgroup placement denial fallback"
  failures=$((failures + 1))
fi

if (
  cleanup_cg="$MT_FIXTURE/failed-cgroup-kill"
  mkdir -p "$cleanup_cg"
  : >"$cleanup_cg/cgroup.kill"
  printf '%s\n' 6060 >"$cleanup_cg/cgroup.procs"
  : >"$MT_FIXTURE/failed-cgroup-kill-pids"
  _openrc_cgroup_kill_all() { return 1; }
  kill() {
    printf '%s\n' "$*" >>"$MT_FIXTURE/failed-cgroup-kill-pids"
    rm -f "$cleanup_cg/cgroup.kill" "$cleanup_cg/cgroup.procs"
  }
  sleep() { :; }
  _openrc_build_cgroup_cleanup "$cleanup_cg"
  grep -qF -- '-KILL 6060' "$MT_FIXTURE/failed-cgroup-kill-pids"
); then
  echo "  PASS: failed cgroup.kill falls back to PID cleanup"
else
  echo "  FAIL: failed cgroup.kill skips PID cleanup"
  failures=$((failures + 1))
fi

: >"$CAPTURE"
_openrc_build_cgroup_supported() { return 0; }
_run_tenant_build_cgroup() {
  echo "CGROUP-BUILD $*" >>"$CAPTURE"
  _OPENRC_BUILD_CGROUP_SETUP_COMPLETE=true
  return 0
}
_run_tenant_build t1 "cd /x && cargo build --profile release --bin lunarwing -j$BUILD_JOBS"
assert_contains "openrc build prefers cgroup bounding" "$CAPTURE" "CGROUP-BUILD t1"
assert_contains "openrc cgroup build has -j"           "$CAPTURE" "-j$BUILD_JOBS"
assert_not_contains "bounded build does not also run fallback" "$CAPTURE" "nice -n 10"

: >"$CAPTURE"
_run_tenant_build_cgroup() {
  echo "CGROUP-BUILD $*" >>"$CAPTURE"
  _OPENRC_BUILD_CGROUP_SETUP_COMPLETE=false
  return 2
}
_run_tenant_build t1 "cd /x && cargo build --profile release --bin lunarwing -j$BUILD_JOBS"
assert_contains "cgroup setup failure falls back to nice" "$CAPTURE" "nice -n 10"

: >"$CAPTURE"
_run_tenant_build_cgroup() {
  echo "CGROUP-BUILD $*" >>"$CAPTURE"
  _OPENRC_BUILD_CGROUP_SETUP_COMPLETE=true
  return 2
}
build_rc=0
_run_tenant_build t1 "exit 2" || build_rc=$?
assert_eq "bounded build status 2 propagates" "2" "$build_rc"
assert_contains "status-2 build runs bounded path" "$CAPTURE" "CGROUP-BUILD t1 exit 2"
assert_not_contains "status-2 build never reruns fallback" "$CAPTURE" "nice -n 10"

: >"$CAPTURE"
_run_tenant_build_cgroup() {
  echo "CGROUP-BUILD $*" >>"$CAPTURE"
  _OPENRC_BUILD_CGROUP_SETUP_COMPLETE=true
  return 125
}
build_rc=0
_run_tenant_build t1 "exit 125" || build_rc=$?
assert_eq "post-setup placement failure propagates" "125" "$build_rc"
assert_not_contains "placement failure never reruns fallback" "$CAPTURE" "nice -n 10"

echo "== cpu quota conversion =="
assert_eq "800% -> cpu.max" "800000 100000" "$(_openrc_cpu_max_from_quota "800%")"
assert_eq "100% -> cpu.max" "100000 100000" "$(_openrc_cpu_max_from_quota "100%")"
_openrc_cpu_max_from_quota "max" 2>/dev/null && { echo "  FAIL: non-percent rejected"; failures=$((failures + 1)); } || echo "  PASS: non-percent rejected"

: >"$CAPTURE"
export INIT_SYSTEM=systemd
systemd-run() { :; } # makes command -v succeed; sudo stub captures the call
_run_tenant_build t1 "cd /x && cargo build --profile release --bin lunarwing -j$BUILD_JOBS"
assert_contains "systemd build scoped"   "$CAPTURE" "systemd-run --user --scope --quiet --collect"
assert_contains "systemd build slice"    "$CAPTURE" "--slice=lunarwing-build.slice"
assert_contains "systemd build cap"      "$CAPTURE" "MemoryMax=24G"
assert_contains "systemd build has -j"   "$CAPTURE" "-j$BUILD_JOBS"

echo
if (( failures > 0 )); then
  echo "FAILURES: $failures"
  exit 1
fi
echo "all workload-isolation rendering tests passed"
