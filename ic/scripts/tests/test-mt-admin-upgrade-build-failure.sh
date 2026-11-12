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

# run-294 TJ-7: keep the real candidate/promote/restore implementations. Parts
# 1-5 replace them with event-recording stubs; part 6 evals these back so the
# real promote path (the TJ-1 blocker) actually runs.
REAL_BUILD_CANDIDATE_WORKER="$(declare -f build_candidate_worker)"
REAL_BUILD_CANDIDATE_WORKERS="$(declare -f build_candidate_workers)"
REAL_PROMOTE_CANDIDATE_WORKERS="$(declare -f promote_candidate_workers)"
REAL_RESTORE_PROMOTED_WORKERS="$(declare -f restore_promoted_workers)"
REAL_CLEANUP_CANDIDATE_WORKERS="$(declare -f cleanup_candidate_workers)"

# The marker dir is root-owned in production; a non-root harness cannot chown it.
install() { command mkdir -p "${!#}"; }
stat() { printf '0:700\n'; }

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
# run-294 TJ-14: upgrade build logs (mktemp under TMPDIR) stay behind on the
# convergence paths; keep them inside the fixture so the trap removes them.
export TMPDIR="$fixture"
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
  if ! build_candidate_sandbox_image "$name" "${9:-auto}" "${7:-}"; then
    printf 'sandbox-failed\n' >>"$cand_events"
  fi
}
upgrade_candidate_lanes() {
  local file
  file="$(tenant_candidate_lanes_file "$1")"
  [[ -r "$file" ]] || return 0
  grep -v '^[[:space:]]*$' "$file" 2>/dev/null || true
}
cleanup_candidate_workers() { : >"$(tenant_candidate_lanes_file "$1")"; }
promote_candidate_workers() {
  printf 'promote\n' >>"$promote_events"
  if [[ -s "$(tenant_candidate_sandbox_file "$1")" ]]; then
    printf 'sandbox-promote\n' >>"$events"
    : >"$(tenant_candidate_sandbox_file "$1")"
  fi
  : >"$(tenant_candidate_lanes_file "$1")"
}
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

# ── 2b. managed sandbox candidates share the pre-stop/post-build train ──────
REAL_TENANT_USES_SANDBOX="$(declare -f tenant_uses_managed_sandbox_image)"
REAL_SANDBOX_BUILD_REQUIRED="$(declare -f sandbox_image_build_required)"
REAL_SANDBOX_RUNTIME="$(declare -f sandbox_container_runtime)"
REAL_SANDBOX_BUILD="$(declare -f build_sandbox_image)"
REAL_SANDBOX_PREPARE="$(declare -f _prepare_sandbox_image_source)"
tenant_uses_managed_sandbox_image() { return 0; }
sandbox_image_build_required() { return 0; }
sandbox_container_runtime() { printf 'podman'; }
_prepare_sandbox_image_source() {
  printf '%s\n' "${2:-}" >"$fixture/sandbox-release-ref"
  # shellcheck disable=SC2034  # consumed by build_candidate_sandbox_image
  SANDBOX_IMAGE_BUILD_SOURCE_ROOT="$fixture_repo/ic"
}
build_sandbox_image() {
  printf 'sandbox-build\n' >>"$events"
  [[ "${SANDBOX_CANDIDATE_FAIL:-false}" != true ]]
}
podman() { return 0; }
render_all_openrc_proxy_units() { :; }
SANDBOX_CANDIDATE_FAIL=false
for init in openrc systemd; do
  INIT_SYSTEM="$init"
  : >"$events"; : >"$cand_events"; : >"$promote_events"
  capture run_upgrade
  if [[ "$rc" -eq 0 && "$(cat "$events")" == $'sandbox-build\nstop\nbuild\nsandbox-promote\nstart' \
      && "$(cat "$fixture/sandbox-release-ref")" == "$target" ]]; then
    pass "$init sandbox candidate builds before stop and promotes after the daemon build"
  else
    fail "$init sandbox candidate ordering failed (rc=$rc): $output; events: $(cat "$events")"
  fi
done
INIT_SYSTEM=systemd
SANDBOX_CANDIDATE_FAIL=true
: >"$events"; : >"$cand_events"; : >"$promote_events"
capture run_upgrade
if [[ "$rc" -eq 0 && "$(cat "$events")" == $'sandbox-build\nstop\nbuild\nstart' ]] \
    && grep -qx 'sandbox-failed' "$cand_events" \
    && ! grep -qx 'sandbox-promote' "$events"; then
  pass 'failed sandbox candidate is non-fatal and is never promoted'
else
  fail "failed sandbox candidate stranded or promoted the tenant (rc=$rc): $output"
fi
SANDBOX_CANDIDATE_FAIL=false
eval "$REAL_TENANT_USES_SANDBOX"
eval "$REAL_SANDBOX_BUILD_REQUIRED"
eval "$REAL_SANDBOX_RUNTIME"
eval "$REAL_SANDBOX_BUILD"
eval "$REAL_SANDBOX_PREPARE"
unset -f podman

# ── 3. post-stop build failure BEFORE the daemon stage: restart in place ─────
# The new daemon was not built, so the previous binary is still on disk and an
# in-place restart is honest.
: >"$events"; : >"$cand_events"
build_tenant() {
  printf 'build\n' >>"$events"
  # The real build_tenant resets the stage marker on entry (start sentinel)
  # before the first cargo build; mirror that so convergence sees "nothing
  # built yet" instead of fail-closing on a missing marker (run-294 TJ-9).
  printf 'start\n' >"$(tenant_build_stage_file fixture)"
  printf 'WASM toolchain incomplete for fixture\n' >&2
  return 1
}
capture run_upgrade
rm -f "$(tenant_build_stage_file fixture)"
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
# converge_after_failed_upgrade delegates the clone restore to
# revert_clone_position, so both real helpers are extracted (run-294 TJ-11).
converge="$(awk '/^(converge_after_failed_upgrade|revert_clone_position)\(\) \{/{f=1} f{print} f&&/^\}$/{f=0}' "$wrapper")"
[[ "$converge" == *'converge_after_failed_upgrade() {'* ]] \
  || fail 'wrapper has no converge_after_failed_upgrade helper'
[[ "$converge" == *'revert_clone_position() {'* ]] \
  || fail 'wrapper has no revert_clone_position helper'

# run-294 TJ-2: the live branch must rebuild the daemon after the stop. Without
# this the tenant restarts on the OLD binary beside the NEW worker images.
if grep -qF 'build_args=(build-tenant "$TENANT" --with-wasm --require-wasm)' "$wrapper"; then
  pass 'wrapper live path rebuilds the daemon after the stop (TJ-2)'
else
  fail 'wrapper live path has no build-tenant call after the stop'
fi

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

# ── 6. the REAL promote path (run-294 TJ-7) ──────────────────────────────────
# Run-291 shipped a promote that aborted on an unset
# ${LUNARWING_MT_CANDIDATE_LANES_FILE} because every harness stubbed it. Here the
# real builder, promote and restore run with the env override UNSET (production
# shape) against a reference-validating runtime stub that rejects any image
# reference containing '/'.
unset LUNARWING_MT_CANDIDATE_LANES_FILE
eval "$REAL_BUILD_CANDIDATE_WORKER"
eval "$REAL_BUILD_CANDIDATE_WORKERS"
eval "$REAL_PROMOTE_CANDIDATE_WORKERS"
eval "$REAL_RESTORE_PROMOTED_WORKERS"
eval "$REAL_CLEANUP_CANDIDATE_WORKERS"

runtime_log="$fixture/runtime-log"
ids_dir="$fixture/runtime-ids"
by_id_dir="$fixture/runtime-ids-by-id"
mkdir -p "$ids_dir" "$by_id_dir"
register_image() {  # <ref> <id>
  printf '%s\n' "$2" >"$ids_dir/$1"
  printf '%s\n' "$1" >"$by_id_dir/$2"
}
# Function, not an executable: /tmp is noexec on some hosts.
podman() {
  case "${1:-}" in
    image)
      [[ "${2:-}" == inspect ]] || return 64
      local ref="${5:-}"
      printf 'image inspect %s\n' "$ref" >>"$runtime_log"
      [[ -n "$ref" && "$ref" != */* ]] || return 65
      [[ -f "$ids_dir/$ref" ]] && cat "$ids_dir/$ref"
      ;;
    tag)
      local src="${2:-}" dst="${3:-}" id=""
      printf 'tag %s %s\n' "$src" "$dst" >>"$runtime_log"
      [[ "$src" != */* && "$dst" != */* ]] || return 65
      if [[ -f "$ids_dir/$src" ]]; then id="$(cat "$ids_dir/$src")"
      elif [[ -f "$by_id_dir/$src" ]]; then id="$src"
      else return 66
      fi
      printf '%s\n' "$id" >"$ids_dir/$dst"
      printf '%s\n' "$dst" >"$by_id_dir/$id"
      ;;
    rmi)
      printf 'rmi %s\n' "${2:-}" >>"$runtime_log"
      rm -f "$ids_dir/${2:-}"
      ;;
    *) return 64 ;;
  esac
}
CONTAINER_RT=podman
MT_ROOTLESS=false

lane_file="$(tenant_candidate_lanes_file fixture)"
candidate_ref="$(upgrade_candidate_image_ref fixture kimi)"
mkdir -p "$(dirname "$lane_file")"

# 6a. the real builder builds to the per-tenant tag and records the lane.
# The builder runs inside capture's subshell, so the tag is recorded in a file.
# LUNARWING_WORKER_IMAGE_TAG must be the bare tag: the real builders name their
# output worker_image_ref <lane>, and that ref must be the candidate ref that
# promote and cleanup use. A full ref in the variable built
# "lunarwing-worker-kimi:lunarwing-worker-kimi:wi527-fixture" (invalid).
tag_file="$fixture/built-tag"
ref_file="$fixture/built-ref"
rm -f "$tag_file" "$ref_file"
build_kimi_worker() {
  printf '%s\n' "${LUNARWING_WORKER_IMAGE_TAG:-}" >"$tag_file"
  worker_image_ref kimi >"$ref_file"
  return 0
}
: >"$lane_file"
rm -f "$runtime_log"
capture build_candidate_workers fixture false true false false false
if [[ "$rc" -ne 0 ]]; then
  fail "real candidate builder failed (rc=$rc): $output"
elif [[ "$(cat "$tag_file" 2>/dev/null)" != wi527-fixture ]]; then
  fail "real candidate builder passed a wrong image tag: '$(cat "$tag_file" 2>/dev/null)'"
elif [[ "$(cat "$ref_file" 2>/dev/null)" != "$candidate_ref" ]]; then
  fail "real candidate builder would build '$(cat "$ref_file" 2>/dev/null)', not the promoted candidate '$candidate_ref'"
elif ! grep -qx kimi "$lane_file"; then
  fail "real candidate builder did not record the lane: $(cat "$lane_file")"
else
  pass 'real candidate builder builds to the per-tenant tag and records the lane'
fi

# WI#507: the pebble candidate must use the tenant checkout too. Keep this
# functional pin beside the other lanes so a merge cannot drop the third arg.
pebble_root_file="$fixture/pebble-source-root"
pebble_tag_file="$fixture/pebble-built-tag"
pebble_ref_file="$fixture/pebble-built-ref"
pebble_release_file="$fixture/pebble-release-ref"
build_pebble_worker() {
  printf '%s\n' "${3:-}" >"$pebble_root_file"
  printf '%s\n' "${4:-}" >"$pebble_release_file"
  printf '%s\n' "${LUNARWING_WORKER_IMAGE_TAG:-}" >"$pebble_tag_file"
  worker_image_ref pebble >"$pebble_ref_file"
  return 0
}
: >"$lane_file"
capture build_candidate_workers fixture false false false true false refs/heads/moving
if [[ "$rc" -ne 0 ]]; then
  fail "real pebble candidate builder failed (rc=$rc): $output"
elif [[ "$(cat "$pebble_root_file" 2>/dev/null)" != "$fixture_repo" ]]; then
  fail "pebble candidate used source root '$(cat "$pebble_root_file" 2>/dev/null)' instead of the tenant checkout"
elif [[ "$(cat "$pebble_release_file" 2>/dev/null)" != refs/heads/moving ]]; then
  fail "pebble candidate lost the operator release ref: '$(cat "$pebble_release_file" 2>/dev/null)'"
elif [[ "$(cat "$pebble_tag_file" 2>/dev/null)" != wi527-fixture ]]; then
  fail "pebble candidate passed a wrong image tag: '$(cat "$pebble_tag_file" 2>/dev/null)'"
elif [[ "$(cat "$pebble_ref_file" 2>/dev/null)" != "$(upgrade_candidate_image_ref fixture pebble)" ]]; then
  fail "pebble candidate would build '$(cat "$pebble_ref_file" 2>/dev/null)', not lunarwing-worker-pebble:wi527-fixture"
elif ! grep -qx pebble "$lane_file"; then
  fail "pebble candidate builder did not record the lane: $(cat "$lane_file")"
else
  pass 'real pebble candidate builder forwards the tenant root, release ref, and per-tenant tag'
fi
printf 'kimi\n' >"$lane_file"

# 6b. the real promote path must not abort on unset env overrides (TJ-1) and
# must tag candidate -> :latest for every recorded lane.
register_image "$candidate_ref" sha256:kimi-candidate
register_image "lunarwing-worker-kimi:latest" sha256:kimi-old
: >"$runtime_log"
capture promote_candidate_workers fixture
if [[ "$rc" -ne 0 ]]; then
  fail "real promote aborted (rc=$rc): $output"
elif [[ "$output" == *'unbound variable'* ]]; then
  fail 'real promote hit an unbound variable with the env override unset'
elif ! grep -qx "tag $candidate_ref lunarwing-worker-kimi:latest" "$runtime_log"; then
  fail "real promote did not tag the candidate: $(cat "$runtime_log")"
elif [[ -s "$lane_file" ]]; then
  fail "real promote did not clear the lane file: $(cat "$lane_file")"
else
  pass 'real promote tags candidate -> :latest with the env override unset'
fi

# 6c. a mid-promote failure keeps the rollback record, and the real restore
# re-tags the pre-promotion id (never claims a rollback it did not do).
printf 'kimi\nopencode\n' >"$lane_file"
register_image "$candidate_ref" sha256:kimi-candidate
register_image "lunarwing-worker-kimi:latest" sha256:kimi-old
: >"$runtime_log"
capture promote_candidate_workers fixture
rollback_file="$(_promote_rollback_file fixture)"
if [[ "$rc" -eq 0 ]]; then
  fail 'real promote reported success although the opencode candidate is missing'
elif [[ ! -s "$rollback_file" ]]; then
  fail 'a mid-promote failure did not keep the rollback record'
else
  capture restore_promoted_workers fixture
  if [[ "$rc" -ne 0 ]]; then
    fail "real restore failed (rc=$rc): $output"
  elif ! grep -qx 'tag sha256:kimi-old lunarwing-worker-kimi:latest' "$runtime_log"; then
    fail "real restore did not re-tag the old id: $(cat "$runtime_log")"
  else
    pass 'real restore re-tags the pre-promotion image id after a mid-promote failure'
  fi
fi

# 6d. a promote whose :latest already equals the candidate is a no-op re-run.
printf 'kimi\n' >"$lane_file"
register_image "$candidate_ref" sha256:kimi-candidate
register_image "lunarwing-worker-kimi:latest" sha256:kimi-candidate
: >"$runtime_log"
capture promote_candidate_workers fixture
if [[ "$rc" -ne 0 ]]; then
  fail "an already-promoted lane failed the re-run (rc=$rc): $output"
elif grep -q '^tag ' "$runtime_log"; then
  fail "an already-promoted lane was re-tagged: $(cat "$runtime_log")"
else
  pass 'real promote is re-runnable (an already-promoted lane is skipped)'
fi

# ── 7. the upgrade wrapper's own verbs must not deadlock (run-300 TO-1) ──────
# The wrapper's boundary takes flock on <build lock>.<tenant>.upgrade for the
# whole run and re-enters mt-admin for build-worker-candidates and
# promote-worker-images. Before TO-1 both verbs preflighted that lock, met their
# own parent, and died — so no wrapper upgrade could ever succeed. This part
# runs the REAL dispatch (main) with an inherited upgrade descriptor.
require_root() { :; }
ports_registry_init() { :; }
tenant_exists_in_registry() { return 0; }

upgrade_lock_file="${BUILD_LOCK}.fixture.upgrade"
exec {inherited_upgrade_fd}>"$upgrade_lock_file"
flock -x "$inherited_upgrade_fd"
export LUNARWING_MT_INHERITED_UPGRADE_LOCK_FD="$inherited_upgrade_fd"

: >"$lane_file"
rm -f "$tag_file" "$ref_file"
register_image "$candidate_ref" sha256:kimi-candidate
register_image "lunarwing-worker-kimi:latest" sha256:kimi-old
capture main build-worker-candidates fixture --with-kimi
if [[ "$rc" -ne 0 ]]; then
  fail "build-worker-candidates failed under the inherited upgrade lock (rc=$rc): $output"
elif [[ "$output" == *'lock is already held'* ]]; then
  fail 'build-worker-candidates preflighted the lock its own parent holds (TO-1)'
elif [[ "$(cat "$tag_file" 2>/dev/null)" != wi527-fixture || "$(cat "$ref_file" 2>/dev/null)" != "$candidate_ref" ]]; then
  fail "build-worker-candidates never built the lane under the inherited lock: tag '$(cat "$tag_file" 2>/dev/null)', ref '$(cat "$ref_file" 2>/dev/null)'"
else
  pass 'build-worker-candidates uses the inherited upgrade lock instead of deadlocking'
fi

capture main promote-worker-images fixture
rollback_file="$(_promote_rollback_file fixture)"
if [[ "$rc" -ne 0 ]]; then
  fail "promote-worker-images failed under the inherited upgrade lock (rc=$rc): $output"
elif [[ "$output" == *'lock is already held'* ]]; then
  fail 'promote-worker-images preflighted the lock its own parent holds (TO-1)'
elif ! grep -qx "tag $candidate_ref lunarwing-worker-kimi:latest" "$runtime_log"; then
  fail "promote-worker-images did not promote under the inherited lock: $(cat "$runtime_log")"
else
  pass 'promote-worker-images uses the inherited upgrade lock instead of deadlocking'
fi

# TO-2: a successful promote no longer deletes the rollback record; the record
# lives until the whole upgrade succeeds (or a restore consumes it).
if [[ ! -s "$rollback_file" ]]; then
  fail 'a successful promote dropped the rollback record (TO-2)'
else
  capture clear_promote_rollback fixture
  if [[ "$rc" -ne 0 || -e "$rollback_file" ]]; then
    fail 'clear_promote_rollback did not remove the record at the end of the upgrade'
  else
    pass 'the rollback record survives promote and is cleared when the upgrade finishes'
  fi
fi

# The preflight is still real for any caller that does not inherit the lock.
: >"$lane_file"
(
  unset LUNARWING_MT_INHERITED_UPGRADE_LOCK_FD
  main build-worker-candidates fixture --with-kimi
) >"$fixture/to1-external.out" 2>&1 && external_rc=0 || external_rc=$?
if [[ "$external_rc" -eq 0 ]]; then
  fail 'an external candidate build succeeded while another process held the upgrade lock'
elif ! grep -q 'lock is already held' "$fixture/to1-external.out"; then
  fail "an external candidate build did not refuse on the held lock: $(cat "$fixture/to1-external.out")"
else
  pass 'an external caller still fails fast on a held upgrade lock'
fi

flock -u "$inherited_upgrade_fd"
exec {inherited_upgrade_fd}>&-
unset LUNARWING_MT_INHERITED_UPGRADE_LOCK_FD

# Without the inherited descriptor the verb acquires the lock for itself.
: >"$lane_file"
capture main build-worker-candidates fixture
if [[ "$rc" -ne 0 ]]; then
  fail "a standalone candidate build could not take the upgrade lock (rc=$rc): $output"
else
  pass 'a standalone candidate build takes and releases the upgrade lock itself'
fi

# TO-7: a missing stage marker is fail-closed on the wrapper path too. It used
# to read as "none" (restart the previous build), the one shape where a failed
# marker write after cargo replaced the binary could start the new binary.
rm -f "$(tenant_build_stage_file fixture)"
stage_missing="$(upgrade_last_completed_stage fixture)"
if [[ "$stage_missing" != unknown ]]; then
  fail "a missing build-stage marker read as '$stage_missing' (expected unknown)"
else
  pass 'a missing build-stage marker is fail-closed (unknown), not none'
fi

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all mt-admin upgrade build-failure (WI#527) tests passed\n'
