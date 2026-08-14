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
assert_contains "OpenRC daemon settings var" "$daemon_unit" 'rc_cgroup_settings'
assert_contains "OpenRC daemon cleanup"      "$daemon_unit" 'rc_cgroup_cleanup="YES"'
assert_contains "OpenRC daemon memory.max"   "$daemon_unit" 'lunarwing_cgroup_memory_max:=8G'
assert_contains "OpenRC daemon memory.high"  "$daemon_unit" 'lunarwing_cgroup_memory_high:=6G'
assert_contains "OpenRC daemon cpu.max"      "$daemon_unit" 'lunarwing_cgroup_cpu_max:=800000 100000'
assert_contains "OpenRC daemon pids.max"     "$daemon_unit" 'lunarwing_cgroup_pids_max:=2048'
assert_contains "OpenRC conf.d documents knobs" "$OPENRC_CONFD_DIR/lunarwing-t1" "lunarwing_cgroup_memory_max"
assert_contains "OpenRC conf.d opt-out"         "$OPENRC_CONFD_DIR/lunarwing-t1" "lunarwing_cgroup_limits_enabled"
assert_contains "OpenRC conf.d keeps ulimit fallback" "$OPENRC_CONFD_DIR/lunarwing-t1" "rc_ulimit"
assert_not_contains "OpenRC has no systemd-only knobs" "$OPENRC_CONFD_DIR/lunarwing-t1" "MemoryMax"

echo "== OpenRC cgroup overrides =="
export WORKLOAD_MEMORY_MAX=12G
export WORKLOAD_TASKS_MAX=4096
render_tenant_openrc_units t1
assert_contains "OpenRC memory override" "$daemon_unit" 'lunarwing_cgroup_memory_max:=12G'
assert_contains "OpenRC pids override"   "$daemon_unit" 'lunarwing_cgroup_pids_max:=4096'
export WORKLOAD_TASKS_MAX=2048
export WORKLOAD_MEMORY_MAX=8G

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

: >"$CAPTURE"
_openrc_build_cgroup_supported() { return 0; }
_run_tenant_build_cgroup() { echo "CGROUP-BUILD $*" >>"$CAPTURE"; return 0; }
_run_tenant_build t1 "cd /x && cargo build --profile release --bin lunarwing -j$BUILD_JOBS"
assert_contains "openrc build prefers cgroup bounding" "$CAPTURE" "CGROUP-BUILD t1"
assert_contains "openrc cgroup build has -j"           "$CAPTURE" "-j$BUILD_JOBS"

: >"$CAPTURE"
_run_tenant_build_cgroup() { return 2; } # setup failure -> nice fallback
_run_tenant_build t1 "cd /x && cargo build --profile release --bin lunarwing -j$BUILD_JOBS"
assert_contains "cgroup setup failure falls back to nice" "$CAPTURE" "nice -n 10"

echo "== cpu quota conversion =="
assert_eq() { # <label> <expected> <actual>
  if [[ "$2" == "$3" ]]; then
    echo "  PASS: $1"
  else
    echo "  FAIL: $1 (want [$2], got [$3])"
    failures=$((failures + 1))
  fi
}
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
