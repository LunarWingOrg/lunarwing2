#!/usr/bin/env bash
# WI#456: upgrade-tenant must preflight the opencode2 builder floor (Podman
# >= 4.5 / Buildah >= 1.30, or Docker BuildKit) BEFORE stopping the tenant.
# build_tenant rebuilds pebble/opencode/kimi images first and only then hits
# build_opencode2_worker's gate; without the preflight a below-floor host
# aborted the upgrade after the tenant was already stopped, leaving pebble/
# opencode down and the kimi/opencode2 images missing/refused (barracuda
# inttest). No live runtime is used.
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

# opencode2 is the ONLY enabled worker in this fixture.
tenant_worker_enabled() { [[ "$1" == "fixture" && "$2" == "opencode2" ]]; }

# Below-floor builder: the preflight must refuse.
opencode2_builder_supported() { return 1; }

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
elif [[ "$output" == *'tenant was not stopped'* ]]; then
  pass 'below-floor opencode2 builder blocks upgrade before stop or rebuild'
else
  fail "below-floor preflight error lacked safe-stop result: $output"
fi

# Supported builder: the preflight passes and the upgrade proceeds to stop.
opencode2_builder_supported() { return 0; }
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

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all mt-admin upgrade opencode2-preflight tests passed\n'
