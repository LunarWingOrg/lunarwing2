#!/usr/bin/env bash
# WI#527: an upgrade build failure must not leave a tenant stranded stopped.
#
# Two independent paths are covered:
#   * mt-admin `upgrade-tenant` (upgrade_tenant_locked): the target checkout and
#     every selected worker image build to candidate tags BEFORE stop_tenant;
#     post-stop failures converge by the build stage markers.
#   * the external wrapper `ic/scripts/upgrade-tenant.sh`: candidate build and
#     stop ordering plus the same marker-aware convergence.
#
# The forced lane failure is a non-opencode2 lane (kimi), so this is not another
# copy of the WI#456 opencode2 builder-floor preflight. No live container
# runtime is used. Run-287 SX-3: the stage order is exercised through the real
# upgrade_tenant_locked, and the daemon-stage case proves the reported state
# matches what was actually started (SX-1).
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
# Two commits: the clone starts at the source revision (A) and the upgrade
# target is a newer commit (B), so previous_commit != target_commit and the
# restore path is observable.
mkdir -p "$fixture_repo/ic/migrations"
git -C "$fixture" init -q repo
printf 'fixture migration\n' >"$fixture_repo/ic/migrations/V1.sql"
git -C "$fixture_repo" add .
git -C "$fixture_repo" -c user.name=fixture -c user.email=fixture@example.invalid \
  commit -qm 'Initial fixture'
source_rev="$(git -C "$fixture_repo" rev-parse HEAD)"
git clone -q --bare "$fixture_repo" "$fixture/origin.git"
git -C "$fixture_repo" remote add origin "$fixture/origin.git"
git -C "$fixture_repo" push -q origin HEAD:refs/heads/moving
printf 'second\n' >"$fixture_repo/second.txt"
git -C "$fixture_repo" add second.txt
git -C "$fixture_repo" -c user.name=fixture -c user.email=fixture@example.invalid \
  commit -qm 'Second fixture'
target="$(git -C "$fixture_repo" rev-parse HEAD)"
git -C "$fixture_repo" push -q origin HEAD:refs/heads/moving
git -C "$fixture_repo" reset -q --hard "$source_rev"

export BUILD_LOCK="$fixture/build.lock"
# TD-2: stage/candidate marker files live in the root-owned lifecycle lock
# directory, not the tenant-writable state directory. The harness exports the
# env overrides so the real mt-admin functions find them in the fixture.
TENANT_LIFECYCLE_LOCK_DIR="$fixture/locks"
export TENANT_LIFECYCLE_LOCK_DIR
export LUNARWING_MT_BUILD_STAGE_FILE="$TENANT_LIFECYCLE_LOCK_DIR/test-build-stages"
export LUNARWING_MT_CANDIDATE_LANES_FILE="$TENANT_LIFECYCLE_LOCK_DIR/test-candidate-lanes"
mkdir -p "$TENANT_LIFECYCLE_LOCK_DIR"
tenant_state_dir() { printf '%s/state' "$fixture"; }
mkdir -p "$fixture/state"

# ── stubs (mirror test-mt-admin-upgrade-opencode2-preflight.sh) ───────────────
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
opencode2_builder_supported() { OPENCODE2_BUILDER_FLOOR_PROBLEM=""; return 0; }

# Candidate-building stubs: record which lanes built and which were promoted.
cand_events="$fixture/candidates"
promote_events="$fixture/promotes"
CANDIDATE_FAIL_LANE=""
build_candidate_worker() {
  printf '%s\n' "$2" >>"$cand_events"
  [[ "$2" != "$CANDIDATE_FAIL_LANE" ]] || return 1
}
build_candidate_workers() {
  local name="$1" oc="$2" kimi="$3" oc2="$4" pebble="${5:-false}"
  : >"$(tenant_candidate_lanes_file "$name")"
  local lane
  for lane in pebble opencode kimi opencode2; do
    case "$lane" in
      pebble)    [[ "$pebble" == true ]] || continue ;;
      opencode)  [[ "$oc" == true ]] || continue ;;
      kimi)      [[ "$kimi" == true ]] || continue ;;
      opencode2) [[ "$oc2" == true ]] || continue ;;
    esac
    build_candidate_worker "$name" "$lane" "false" \
      || die "$lane worker image candidate build failed; tenant '$name' has not been stopped"
    printf '%s\n' "$lane" >>"$(tenant_candidate_lanes_file "$name")"
  done
}
upgrade_candidate_lanes() {
  local file
  file="$(tenant_candidate_lanes_file "$1")"
  [[ -r "$file" ]] || return 0
  grep -v '^[[:space:]]*$' "$file" 2>/dev/null || true
}
cleanup_candidate_workers() { : >"$(tenant_candidate_lanes_file "$1")"; }
promote_candidate_workers() { printf 'promote\n' >>"$promote_events"; : >"$(tenant_candidate_lanes_file "$1")"; }
ensure_container_runtime() { :; }

# ── 1. a failing pre-stop kimi lane leaves the RUNNING tenant untouched ──────
# The old order was stop -> build; the WI#527 order builds candidates first, so
# the tenant is never stopped on a lane build failure.
# Capture without command-substitution failure aborting the harness under set -e.
capture() {
  set +e
  output="$("$@" 2>&1)"
  rc=$?
  set -e
}
run_upgrade() { upgrade_tenant_locked fixture "$target" '' false false; }

build_tenant() { printf 'build\n' >>"$events"; return 0; }
: >"$events"; : >"$cand_events"
CANDIDATE_FAIL_LANE="kimi"
capture run_upgrade

if [[ "$rc" -eq 0 ]]; then
  fail 'upgrade reported success although the kimi candidate lane failed'
elif grep -q '^stop$' "$events"; then
  fail "upgrade stopped the tenant although the candidate build failed pre-stop: $output"
elif ! grep -q '^kimi$' "$cand_events"; then
  fail "the kimi candidate lane was never attempted: $(cat "$cand_events")"
elif [[ "$output" == *'was not stopped and is unchanged'* ]]; then
  pass 'a failing pre-stop kimi lane leaves the running tenant untouched'
else
  fail "pre-stop lane failure misreported: $output"
fi
CANDIDATE_FAIL_LANE=""

# The checkout must also be returned to the previous commit so the running
# tenant matches its source again.
head_now="$(git -C "$fixture_repo" rev-parse HEAD)"
if [[ "$head_now" == "$target" ]]; then
  fail 'a pre-stop candidate failure left the clone at the target commit'
else
  pass 'a pre-stop candidate failure restores the previous checkout'
fi

# ── 2. happy path: candidates build pre-stop, promote happens post-stop ──────
build_tenant() { printf 'build\n' >>"$events"; return 0; }
: >"$events"; : >"$cand_events"; : >"$promote_events"
capture run_upgrade
if [[ "$rc" -ne 0 ]]; then
  fail "a fully successful upgrade failed (rc=$rc): $output"
elif ! grep -qx 'kimi' "$cand_events"; then
  fail "candidate lane never built: $(cat "$cand_events")"
elif ! grep -qx 'stop' "$events"; then
  fail "tenant was never stopped: $(cat "$events")"
elif ! grep -qx 'promote' "$promote_events"; then
  fail 'candidate images were never promoted'
else
  pass 'candidates build pre-stop and promotion runs after the daemon build'
fi

# ── 3. post-stop build failure BEFORE the daemon stage: restart in place ─────
# The new daemon was not built, so the previous binary is still on disk and an
# in-place restart is honest.
: >"$events"; : >"$cand_events"
build_tenant() {
  printf 'build\n' >>"$events"
  printf 'WASM toolchain incomplete for fixture\n' >&2
  return 1
}
capture run_upgrade
if [[ "$rc" -eq 0 ]]; then
  fail 'a post-stop build failure was ignored'
elif [[ "$(grep -c '^start$' "$events")" != 1 ]]; then
  fail "pre-daemon failure did not restart the tenant: $output"
elif [[ "$output" == *'running its previous build'* ]]; then
  pass 'a pre-daemon post-stop failure restarts the previous build in place'
else
  fail "pre-daemon convergence misreported: $output"
fi

# ── 4. post-stop failure AFTER the daemon stage: never start the new binary ──
# run-287 SX-1: restarting here would run the NEW binary and apply its
# migrations while claiming a restore. The converge path must rebuild the
# previous commit first; if that rebuild fails the tenant is reported stopped.
: >"$events"; : >"$cand_events"
stage_file="$LUNARWING_MT_BUILD_STAGE_FILE"
checkout_marker="$fixture/checkout-seen"
rm -f "$checkout_marker"
build_tenant() {
  printf 'build\n' >>"$events"
  printf 'daemon\n' >"$stage_file"
  return 1
}
restore_build_calls=0
# The restore path rebuilds the previous commit; its build must fail so we can
# assert the tenant is reported stopped, not restored.
restore_build_tenant() { printf 'restore-build\n' >>"$events"; return 1; }
sudo() {
  [[ "${1:-}" == "-u" ]] || return 64
  shift 2
  local arg
  for arg in "$@"; do
    if [[ "$arg" == "checkout" ]]; then : >"$checkout_marker"; fi
  done
  "$@"
}
capture run_upgrade
rm -f "$stage_file"
if [[ "$rc" -eq 0 ]]; then
  fail 'a post-daemon build failure was ignored'
elif ! grep -q '^start$' "$events"; then
  pass 'a post-daemon build failure never starts the new binary'
elif [[ "$output" == *'automatic restore FAILED'* && "$output" == *'remains stopped'* ]]; then
  pass 'a failed restore is reported honestly (tenant stopped, rollback steps)'
else
  fail "post-daemon convergence misreported: $output"
fi
# And when the previous commit rebuilds cleanly, it is started and reported.
: >"$events"; : >"$cand_events"
rm -f "$checkout_marker"
build_tenant() {
  if [[ -f "$stage_file" ]]; then
    # Restore pass: called with the previous commit checked out.
    printf 'restore-build\n' >>"$events"
    return 0
  fi
  printf 'build\n' >>"$events"
  printf 'daemon\n' >"$stage_file"
  return 1
}
render_tenant_units() { :; }
patch_tenant_env() { :; }
capture run_upgrade
rm -f "$stage_file"
if [[ "$rc" -eq 0 ]]; then
  fail 'a post-daemon failure was reported as a successful upgrade'
elif [[ "$(grep -c '^start$' "$events")" != 1 ]]; then
  fail "the rebuilt previous version was never started: $output"
elif [[ "$output" == *'automatically restored'* && "$output" != *'new daemon'*'restored'* ]]; then
  pass 'a successful restore of the previous commit is started and reported'
else
  fail "restore path misreported: $output"
fi
build_tenant() { printf 'build\n' >>"$events"; return 0; }

# ── 5. wrapper: marker-aware convergence ─────────────────────────────────────
# Extract the wrapper's convergence function and drive it against a stub mt-admin
# that reports the last completed stage. The stub is a function, not an executable
# file: /tmp is noexec on some hosts, and converge calls "$MT" directly.
export FIXTURE_EVENTS="$fixture/wrapper-events"
mt_admin_stub() {
  printf '%s\n' "$*" >>"${FIXTURE_EVENTS:?}"
  case "${1:-}" in
    last-build-stage) printf '%s\n' "${WRAPPER_STAGE:-none}" ;;
    start-tenant)
      if [[ "${WRAPPER_START_FAIL:-}" == "1" ]]; then return 1; fi
      ;;
  esac
}
MT=mt_admin_stub

wrapper="$SCRIPT_DIR/../upgrade-tenant.sh"
converge="$(awk '/^converge_after_failed_upgrade\(\) \{/{f=1} f{print} f&&/^\}$/{exit}' "$wrapper")"
[[ -n "$converge" ]] || fail 'wrapper has no converge_after_failed_upgrade helper'

TENANT=fixture
LWROOT="$fixture/lwroot"
PRIOR_REV=deadbeef
DRY_RUN=false
sudo() { [[ "${1:-}" == "-u" ]] || return 64; shift 2; "$@"; }
say() { printf '%s\n' "$*" >&2; }
note() { :; }
run() {
  if $DRY_RUN; then printf '  [dry-run] %s\n' "$*"; return 0; fi
  "$@"
}
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
mt() { "$MT" "$@"; }
mt_rootful() { LUNARWING_MT_ROOTLESS=false "$MT" "$@"; }

eval "$converge"

# 5a. before the daemon stage: restore the previous revision and restart.
mkdir -p "$LWROOT"
git -C "$fixture" init -q lwroot
printf 'old\n' >"$LWROOT/file"
git -C "$LWROOT" add file
git -C "$LWROOT" -c user.name=fixture -c user.email=fixture@example.invalid \
  commit -qm 'old version'
PRIOR_REV="$(git -C "$LWROOT" rev-parse --short HEAD)"
: >"$FIXTURE_EVENTS"
export WRAPPER_STAGE=none
capture converge_after_failed_upgrade 'tenant build failed (exit 1)'
wrapper_calls="$(cat "$FIXTURE_EVENTS")"
head_now="$(git -C "$LWROOT" rev-parse --short HEAD)"
if [[ "$rc" -eq 0 ]]; then
  fail 'wrapper convergence did not fail the upgrade'
elif [[ "$head_now" != "$PRIOR_REV" ]]; then
  fail "wrapper did not return the clone to $PRIOR_REV (now $head_now)"
elif [[ "$wrapper_calls" != *'start-tenant fixture'* ]]; then
  fail "wrapper did not start the restored tenant: $wrapper_calls"
elif [[ "$output" == *'automatically restored'* ]]; then
  pass 'wrapper restores the previous version and restarts before the daemon stage'
else
  fail "wrapper pre-daemon convergence misreported: $output"
fi

# 5b. past the daemon stage: never auto-start; report stopped + rollback.
: >"$FIXTURE_EVENTS"
git -C "$LWROOT" checkout -q "$PRIOR_REV" 2>/dev/null || true
export WRAPPER_STAGE=daemon; export WRAPPER_START_FAIL=0
capture converge_after_failed_upgrade 'tenant build failed (exit 1)'
wrapper_calls="$(cat "$FIXTURE_EVENTS")"
if [[ "$rc" -eq 0 ]]; then
  fail 'wrapper did not fail the upgrade past the daemon stage'
elif grep -qx 'start-tenant fixture' <<<"$wrapper_calls"; then
  fail 'wrapper started the tenant although the new daemon binary was built'
elif [[ "$output" == *'remains stopped'* && "$output" == *'checkout '"$PRIOR_REV"* ]]; then
  pass 'wrapper past the daemon stage reports the tenant stopped with rollback steps'
else
  fail "wrapper post-daemon convergence misreported: $output"
fi

# 5c. a failed restart before the daemon stage is not reported as restored.
: >"$FIXTURE_EVENTS"
export WRAPPER_STAGE=none WRAPPER_START_FAIL=1
capture converge_after_failed_upgrade 'tenant build failed (exit 1)'
if [[ "$rc" -eq 0 || "$output" != *'automatic restore FAILED'* ]]; then
  fail "wrapper misreported a failed restart (rc=$rc): $output"
else
  pass 'wrapper reports a failed restart as the tenant remaining stopped'
fi

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all mt-admin upgrade build-failure (WI#527) tests passed\n'
