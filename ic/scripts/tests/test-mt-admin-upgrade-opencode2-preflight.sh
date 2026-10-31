#!/usr/bin/env bash
# WI#456: upgrade-tenant must preflight the opencode2 builder floor (Podman
# >= 4.5 / Buildah >= 1.30, or Docker BuildKit) BEFORE stopping the tenant.
# build_tenant rebuilds pebble/opencode/kimi images first and only then hits
# the opencode2 build gate; without the preflight a below-floor host aborted
# the upgrade after the tenant was already stopped, leaving every lane down
# and the kimi/opencode2 images missing/refused. No live runtime is used.
# shellcheck disable=SC2034  # fixture state is consumed by sourced helpers.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"
export INIT_SYSTEM=systemd

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
# upgrade_tenant_locked declares `local lw_root repo`, which would shadow a
# global `repo` under set -u inside our stubs; use a distinct fixture name.
fixture_repo="$fixture/repo"
failures=0
pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }

# Minimal git checkout so upgrade_tenant_locked's describe/rev-parse calls work.
mkdir -p "$fixture_repo/ic/migrations"
git -C "$fixture" init -q repo
printf 'fixture migration\n' >"$fixture_repo/ic/migrations/V1.sql"
git -C "$fixture_repo" add .
git -C "$fixture_repo" -c user.name=fixture -c user.email=fixture@example.invalid \
  commit -qm 'Initial fixture'
git clone -q --bare "$fixture_repo" "$fixture/origin.git"
git -C "$fixture_repo" remote add origin "$fixture/origin.git"
git -C "$fixture_repo" push -q origin HEAD:refs/heads/moving
target="$(git -C "$fixture_repo" rev-parse HEAD)"

export BUILD_LOCK="$fixture/build.lock"
events="$fixture/events"
: >"$events"

# Stub everything except the preflight and the helpers it consults.
say() { :; }
sudo() { [[ "${1:-}" == "-u" ]] || return 64; shift 2; "$@"; }
tenant_lw_root() { printf '%s\n' "$fixture_repo"; }
tenant_repo() { printf '%s/ic\n' "$fixture_repo"; }
prepare_tenant_checkout() { :; }
backfill_tenant_worker_base_urls() { :; }
tenant_proxy_enabled() { return 1; }
backup_tenant_postgres() { printf 'backup\n' >>"$events"; }
stop_tenant() { printf 'stop\n' >>"$events"; }
start_tenant() { printf 'start\n' >>"$events"; }
build_tenant() { printf 'build\n' >>"$events"; }
render_tenant_units() { :; }
patch_tenant_env() { :; }
refresh_deployed_health_pipeline() { :; }
recover_failed_tenant_checkout() { :; }
# WI#527: candidates build pre-stop; stubbed here (this harness pins the floor
# preflight, not the candidate build itself).
tenant_state_dir() { printf '%s\n' "$fixture/state"; }
mkdir -p "$fixture/state"
build_candidate_workers() { printf 'candidates\n' >>"$events"; }
promote_candidate_workers() { :; }
cleanup_candidate_workers() { :; }
upgrade_candidate_lanes() { :; }

# opencode2 is the ONLY enabled worker in this fixture.
tenant_worker_enabled() { [[ "$1" == "fixture" && "$2" == "opencode2" ]]; }

# Keep the real probe so the last case exercises the build path against it.
REAL_OPENCODE2_PROBE="$(declare -f opencode2_builder_supported)"

# Below-floor builder: the preflight must refuse the upgrade before stop.
opencode2_builder_supported() {
  OPENCODE2_BUILDER_FLOOR_PROBLEM="fixture floor below Podman 4.5"
  return 1
}

set +e
output="$(upgrade_tenant_locked fixture "$target" '' false false 2>&1)"
rc=$?
set -e

if [[ "$rc" -eq 0 ]]; then
  fail 'upgrade succeeded on a below-floor opencode2 builder'
elif grep -q '^stop$' "$events"; then
  fail 'upgrade stopped the tenant before the opencode2 builder preflight'
elif grep -q '^build$' "$events"; then
  fail 'upgrade rebuilt worker images before the opencode2 builder preflight'
elif [[ "$output" == *"fixture floor below Podman 4.5"* \
    && "$output" == *"tenant 'fixture' was not stopped"* ]]; then
  pass 'below-floor builder blocks upgrade before stop, naming floor and safe stop'
else
  fail "below-floor preflight error lacked floor/safe-stop wording: $output"
fi

# Supported builder: the preflight passes and the upgrade proceeds to stop.
opencode2_builder_supported() { OPENCODE2_BUILDER_FLOOR_PROBLEM=""; return 0; }
: >"$events"
set +e
output="$(upgrade_tenant_locked fixture "$target" '' false false 2>&1)"
rc=$?
set -e

if [[ "$rc" -eq 0 ]] \
  && grep -q '^stop$' "$events" && grep -q '^build$' "$events"; then
  pass 'supported opencode2 builder lets the upgrade stop and rebuild'
else
  fail "supported-builder upgrade did not proceed (rc=$rc): $output"
fi

# opencode2 not selected: the preflight is skipped entirely (never consulted).
tenant_worker_enabled() { return 1; }
opencode2_builder_supported() { printf 'preflight-called\n' >>"$events"; return 1; }
: >"$events"
set +e
output="$(upgrade_tenant_locked fixture "$target" '' false false 2>&1)"
rc=$?
set -e

if grep -q 'preflight-called' "$events"; then
  fail 'preflight ran for a tenant without opencode2 selected'
elif [[ "$rc" -eq 0 ]] && grep -q '^stop$' "$events"; then
  pass 'opencode2-unselected tenant skips the builder preflight'
else
  fail "unselected-tenant upgrade behaved unexpectedly (rc=$rc): $output"
fi

# build_opencode2_worker must keep dying with the real probe's reason, not a
# bare fallback, and must not stop silently when the floor is missing. Use the
# real opencode2_builder_supported with only the host runtime probes stubbed.
tenant_worker_enabled() { :; }
tenant_lw_root() { printf '%s\n' "$fixture"; }
eval "$REAL_OPENCODE2_PROBE"
ensure_container_runtime() { CONTAINER_RT=podman; MT_ROOTLESS=false; }
podman_supports_run_network() { return 1; }
LUNARWING_ROOT="$fixture"
mkdir -p "$fixture/opencode2.4lunarwing"
set +e
output="$(build_opencode2_worker false false "$fixture" 2>&1)"
rc=$?
set -e
if [[ "$rc" -ne 0 && "$output" == *"Podman >= 4.5"* ]]; then
  pass 'build_opencode2_worker dies with the probe reason when the floor is missing'
else
  fail "build_opencode2_worker floor failure missing probe reason (rc=$rc): $output"
fi

# WI#456 review SR-1: the external upgrade wrapper
# (ic/scripts/upgrade-tenant.sh) goes upgrade-preflight.sh ->
# `stop-tenant-for-upgrade` -> build-tenant, so the floor must be enforced at
# that stop boundary too. Those cases stub stop_tenant directly, so re-enable
# the real stop_tenant_for_upgrade body plus its minimal dependencies.
tenant_exists_in_registry() { :; }
tenant_darkirc_enabled() { return 1; }
stop_tenant() { printf 'stop\n' >>"$events"; }
unset -f tenant_worker_enabled
tenant_worker_enabled() { [[ "$1" == "fixture" && "$2" == "opencode2" ]]; }

opencode2_builder_supported() {
  OPENCODE2_BUILDER_FLOOR_PROBLEM="fixture floor below Podman 4.5"
  return 1
}
: >"$events"
set +e
output="$(stop_tenant_for_upgrade fixture 2>&1)"
rc=$?
set -e
if [[ "$rc" -eq 0 ]]; then
  fail 'stop-tenant-for-upgrade accepted a below-floor opencode2 builder'
elif grep -q '^stop$' "$events"; then
  fail 'stop-tenant-for-upgrade stopped the tenant before the builder-floor check'
elif [[ "$output" == *"fixture floor below Podman 4.5"* \
    && "$output" == *"tenant 'fixture' was not stopped"* ]]; then
  pass 'wrapper stop boundary refuses a below-floor builder before stopping'
else
  fail "wrapper stop boundary floor error lacked floor/safe-stop wording: $output"
fi

opencode2_builder_supported() { OPENCODE2_BUILDER_FLOOR_PROBLEM=""; return 0; }
: >"$events"
set +e
output="$(stop_tenant_for_upgrade fixture 2>&1)"
rc=$?
set -e
if [[ "$rc" -eq 0 ]] && grep -q '^stop$' "$events"; then
  pass 'wrapper stop boundary proceeds when the opencode2 builder meets the floor'
else
  fail "wrapper stop boundary blocked a supported builder (rc=$rc): $output"
fi

# SR-3: no container runtime at all must die in the probe's own words with the
# tenant still running, rather than falling through to the stop.
tenant_worker_enabled() { [[ "$1" == "fixture" && "$2" == "opencode2" ]]; }
ensure_container_runtime() { die "neither docker nor podman found; install one"; }
eval "$REAL_OPENCODE2_PROBE"
: >"$events"
set +e
output="$(stop_tenant_for_upgrade fixture 2>&1)"
rc=$?
set -e
if [[ "$rc" -eq 0 ]]; then
  fail 'stop-tenant-for-upgrade accepted a host with no container runtime'
elif grep -q '^stop$' "$events"; then
  fail 'stop-tenant-for-upgrade stopped the tenant with no container runtime'
elif [[ "$output" == *"neither docker nor podman found"* ]]; then
  pass 'missing container runtime fails closed before the tenant is stopped'
else
  fail "missing-runtime failure did not carry the runtime message: $output"
fi

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all mt-admin upgrade opencode2-preflight tests passed\n'
