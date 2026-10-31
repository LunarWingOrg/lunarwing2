#!/usr/bin/env bash
# WI#527: an upgrade that fails AFTER the tenant is stopped must converge back
# to a running tenant instead of leaving every lane down.
#
# Covers both upgrade paths:
#   * mt-admin `upgrade-tenant` (upgrade_tenant_locked), which builds after
#     stop_tenant;
#   * the external wrapper `ic/scripts/upgrade-tenant.sh`
#     (upgrade-preflight -> stop-tenant-for-upgrade -> build-tenant).
#
# The forced failure is a non-opencode2 lane (kimi) so this is not another
# copy of the WI#456 opencode2 builder-floor preflight. No live runtime is used.
# shellcheck disable=SC2034  # fixture state is consumed by sourced helpers.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_DIR="$SCRIPT_DIR"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"
# The composition root redefines SCRIPT_DIR; keep the harness's own path.
SCRIPT_DIR="$TEST_DIR"
export INIT_SYSTEM=systemd

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
fixture_repo="$fixture/repo"
events="$fixture/events"
failures=0
pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }

# ── minimal git checkout so upgrade_tenant_locked's git calls work ───────────
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
: >"$events"

# ── stubs (mirror test-mt-admin-upgrade-opencode2-preflight.sh) ──────────────
say() { :; }
sudo() { [[ "${1:-}" == "-u" ]] || return 64; shift 2; "$@"; }
tenant_lw_root() { printf '%s\n' "$fixture_repo"; }
tenant_repo() { printf '%s/ic\n' "$fixture_repo"; }
prepare_tenant_checkout() { :; }
backfill_tenant_worker_base_urls() { :; }
tenant_proxy_enabled() { return 1; }
backup_tenant_postgres() { :; }
stop_tenant() { printf 'stop\n' >>"$events"; }
start_tenant() { printf 'start\n' >>"$events"; }
render_tenant_units() { :; }
patch_tenant_env() { :; }
refresh_deployed_health_pipeline() { :; }
recover_failed_tenant_checkout() { :; }
tenant_worker_enabled() { [[ "$2" == kimi ]]; }
# WI#456 preflight is out of scope here (pinned by its own harness).
opencode2_builder_supported() { OPENCODE2_BUILDER_FLOOR_PROBLEM=""; return 0; }

run_upgrade() {
  local output rc
  set +e
  output="$(upgrade_tenant_locked fixture "$target" '' false false 2>&1)"
  rc=$?
  set -e
  printf '%s\n' "$output"
  return "$rc"
}

# ── 1. a failing kimi lane restores the running tenant ───────────────────────
# The old code left the tenant stopped and reported "tenant left stopped".
build_tenant() {
  printf 'build\n' >>"$events"
  printf 'kimi worker image build failed\n' >&2
  return 1
}
: >"$events"
set +e
output="$(upgrade_tenant_locked fixture "$target" '' false false 2>&1)"
rc=$?
set -e

if [[ "$rc" -eq 0 ]]; then
  fail 'upgrade reported success although the kimi lane build failed'
elif ! grep -q '^start$' "$events"; then
  fail "upgrade left the tenant stopped after a kimi build failure: $output"
elif [[ "$output" == *'tenant left stopped'* ]]; then
  fail 'upgrade still claims the tenant was left stopped'
elif [[ "$output" == *'kimi lane'* && "$output" == *'automatically restored'* ]]; then
  pass 'failed kimi lane names the lane and restores the running tenant'
else
  fail "failure message did not name the lane/restore state: $output"
fi

# ── 2. restore failure is reported honestly, not as a clean restore ─────────
build_tenant() { printf 'build\n' >>"$events"; return 1; }
start_tenant() { printf 'start-attempt\n' >>"$events"; return 1; }
: >"$events"
set +e
output="$(upgrade_tenant_locked fixture "$target" '' false false 2>&1)"
rc=$?
set -e
if [[ "$rc" -ne 0 && "$output" == *'automatic restore FAILED'* \
    && "$output" == *'remains stopped'* && "$output" != *'automatically restored'* ]]; then
  pass 'a failed automatic restore reports the tenant remains stopped'
else
  fail "failed-restore path was misreported (rc=$rc): $output"
fi

# ── 3. daemon-binary failure restores the previous commit before start ──────
# A lunarwing/xmpp/WASM failure leaves an old binary under the new checkout, so
# the lane classifier must pick the previous commit path. The checkout must
# happen before start_tenant is attempted.
build_tenant() {
  printf 'build\n' >>"$events"
  printf 'lunarwing build failed for fixture\n' >&2
  return 1
}
start_tenant() { printf 'start\n' >>"$events"; }
checkout_marker="$fixture/checkout-seen"
rm -f "$checkout_marker"
sudo() {
  [[ "${1:-}" == "-u" ]] || return 64
  shift 2
  local arg
  for arg in "$@"; do
    if [[ "$arg" == "checkout" ]]; then : >"$checkout_marker"; fi
  done
  "$@"
}
: >"$events"
set +e
output="$(upgrade_tenant_locked fixture "$target" '' false false 2>&1)"
rc=$?
set -e
if [[ "$rc" -ne 0 && -f "$checkout_marker" && "$(grep -c '^start$' "$events")" == 1 ]]; then
  pass 'daemon/WASM failure restores the previous commit before restarting'
else
  fail "daemon-failure path did not restore-then-start (rc=$rc, checkout=$([[ -f "$checkout_marker" ]] && echo yes || echo no)): $output"
fi

# ── 4. external wrapper: build-tenant failure after stop restores the tenant ─
# ic/scripts/upgrade-tenant.sh re-enters mt-admin via upgrade-wrapper-boundary,
# so exercise its step-3 convergence function directly with the real body.
export LUNARWING_MT_ADMIN="$fixture/mt-admin-stub.sh"
cat >"$LUNARWING_MT_ADMIN" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"${FIXTURE_EVENTS:?}"
STUB
chmod +x "$LUNARWING_MT_ADMIN"
export FIXTURE_EVENTS="$fixture/wrapper-events"

# Source only the convergence helper + its dependencies from the wrapper by
# extracting the function body (the rest of the script has top-level effects).
wrapper="$SCRIPT_DIR/../upgrade-tenant.sh"
converge="$(
  awk '/^converge_after_failed_upgrade\(\) \{/{f=1} f{print} f&&/^\}$/{exit}' "$wrapper"
)"
[[ -n "$converge" ]] || fail 'wrapper has no converge_after_failed_upgrade helper'

TENANT=fixture
LWROOT="$fixture/lwroot"
PRIOR_REV=deadbeef
MT="$LUNARWING_MT_ADMIN"
DRY_RUN=false
mkdir -p "$LWROOT"
git -C "$fixture" init -q lwroot
printf 'old\n' >"$LWROOT/file"
git -C "$LWROOT" add file
git -C "$LWROOT" -c user.name=fixture -c user.email=fixture@example.invalid \
  commit -qm 'old version'
PRIOR_REV="$(git -C "$LWROOT" rev-parse --short HEAD)"
sudo() { [[ "${1:-}" == "-u" ]] || return 64; shift 2; "$@"; }
say() { printf '%s\n' "$*" >&2; }
note() { :; }
run() {
  if $DRY_RUN; then printf '  [dry-run] %s\n' "$*"; return 0; fi
  "$@"
}
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
mt_rootful() { LUNARWING_MT_ROOTLESS=false bash "$MT" "$@"; }

eval "$converge"
: >"$FIXTURE_EVENTS"
set +e
output="$( (converge_after_failed_upgrade 'tenant build failed (exit 1)') 2>&1)"
rc=$?
set -e
wrapper_calls="$(cat "$FIXTURE_EVENTS")"
head_now="$(git -C "$LWROOT" rev-parse --short HEAD)"
if [[ "$rc" -eq 0 ]]; then
  fail 'wrapper convergence did not fail the upgrade'
elif [[ "$head_now" != "$PRIOR_REV" ]]; then
  fail "wrapper did not return the clone to $PRIOR_REV (now $head_now)"
elif [[ "$wrapper_calls" != *"start-tenant fixture"* ]]; then
  fail "wrapper did not start the restored tenant: $wrapper_calls"
elif [[ "$output" == *'automatically restored'* && "$output" == *'tenant build failed'* ]]; then
  pass 'wrapper restores the previous version and restarts after a build failure'
else
  fail "wrapper failure message lost the cause/restore state: $output"
fi

# A restore that cannot start must not claim the tenant is running.
: >"$FIXTURE_EVENTS"
cat >"$LUNARWING_MT_ADMIN" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"${FIXTURE_EVENTS:?}"
[[ "${1:-}" != "start-tenant" ]] || exit 1
STUB
set +e
output="$( (converge_after_failed_upgrade 'tenant build failed (exit 1)') 2>&1)"
rc=$?
set -e
if [[ "$rc" -ne 0 && "$output" == *'automatic restore FAILED'* \
    && "$output" == *'remains stopped'* && "$output" != *'automatically restored and is running'* ]]; then
  pass 'wrapper reports a failed restart as the tenant remaining stopped'
else
  fail "wrapper misreported a failed restart (rc=$rc): $output"
fi

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all mt-admin upgrade build-failure (WI#527) tests passed\n'
