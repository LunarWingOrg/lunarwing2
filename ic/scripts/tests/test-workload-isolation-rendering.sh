#!/usr/bin/env bash
# Test harness for BUG-005 cgroup isolation in lunarwing-mt-admin.sh.
# Verifies:
#  - per-tenant systemd workload/build slices carry memory/CPU/task caps
#  - the daemon unit gets its own MemoryHigh/MemoryMax/TasksMax
#  - operator env overrides flow through to rendered units
#  - OpenRC conf.d documents the cgroup-cap gap (and has no systemd caps)
#  - _run_tenant_build scopes builds on systemd and nice's them elsewhere,
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

# ── OpenRC rendering (parity: documented gap, no systemd-only knobs) ─────────
echo "== OpenRC conf.d =="
render_tenant_openrc_units t1
assert_contains "OpenRC BUG-005 note"      "$OPENRC_CONFD_DIR/lunarwing-t1" "rc_ulimit"
assert_not_contains "OpenRC has no MemoryMax" "$OPENRC_CONFD_DIR/lunarwing-t1" "MemoryMax"

# ── _run_tenant_build bounding ───────────────────────────────────────────────
echo "== build bounding =="
id() { echo 1001; }
CAPTURE="$MT_FIXTURE/capture"

sudo() { echo "SUDO $*" >>"$CAPTURE"; }

: >"$CAPTURE"
export INIT_SYSTEM=openrc
_run_tenant_build t1 "cd /x && cargo build --profile release --bin lunarwing -j$BUILD_JOBS"
assert_contains "openrc build is nice'd" "$CAPTURE" "nice -n 10"
assert_contains "openrc build has -j"    "$CAPTURE" "-j$BUILD_JOBS"

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
