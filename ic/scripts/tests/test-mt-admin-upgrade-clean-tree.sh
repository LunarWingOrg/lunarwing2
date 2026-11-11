#!/usr/bin/env bash
# shellcheck disable=SC2034  # fixture globals are consumed by sourced helpers.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"
REAL_LIFECYCLE_LOCK="$(declare -f with_tenant_lifecycle_lock)"
export INIT_SYSTEM=systemd

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
repo="$fixture/repo"
lockfile="$repo/ic/channels-src/xmpp/Cargo.lock"
tracked_file="$repo/README.md"
failures=0
fail_checkout=false

pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }

sudo() {
  [[ "${1:-}" == "-u" ]] || return 64
  shift 2
  if [[ "$fail_checkout" == "true" && "${1:-}" == "git" ]]; then
    local arg
    for arg in "$@"; do
      if [[ "$arg" == "checkout" ]]; then
        printf 'fatal: simulated checkout failure\n' >&2
        return 128
      fi
    done
  fi
  "$@"
}

mkdir -p "$(dirname "$lockfile")" "$repo/ic/migrations"
git -C "$fixture" init -q repo
printf 'committed lock\n' >"$lockfile"
printf 'committed source\n' >"$tracked_file"
printf 'fixture migration\n' >"$repo/ic/migrations/V1.sql"
git -C "$repo" add .
git -C "$repo" -c user.name=fixture -c user.email=fixture@example.invalid \
  commit -qm 'Initial fixture'
git clone -q --bare "$repo" "$fixture/origin.git"
git -C "$repo" remote add origin "$fixture/origin.git"
git clone -q "$fixture/origin.git" "$fixture/target-work"
git -C "$fixture/target-work" checkout -qb collision
mkdir -p "$fixture/target-work/state"
printf 'target-owned data\n' >"$fixture/target-work/state/runtime.txt"
git -C "$fixture/target-work" add state/runtime.txt
git -C "$fixture/target-work" -c user.name=fixture -c user.email=fixture@example.invalid \
  commit -qm 'Add target collision'
git -C "$fixture/target-work" push -q origin collision
git -C "$fixture/target-work" checkout -qb invalid-layout HEAD^
rm -rf "$fixture/target-work/ic/migrations"
printf 'not a directory\n' >"$fixture/target-work/ic/migrations"
git -C "$fixture/target-work" add -A ic/migrations
git -C "$fixture/target-work" -c user.name=fixture -c user.email=fixture@example.invalid \
  commit -qm 'Replace migrations directory'
git -C "$fixture/target-work" push -q origin invalid-layout
fixture_invalid_commit="$(git -C "$fixture/target-work" rev-parse HEAD)"
git -C "$repo" push -q origin HEAD:refs/heads/moving
git -C "$repo" push -q origin HEAD:refs/heads/checkout-failure

printf 'build-regenerated lock\n' >"$lockfile"
mkdir -p "$repo/env" "$repo/opencode-workspace" "$repo/state"
# The tenant-side atomic writer (WI#390) requires a tenant-private (0700) env
# dir for its 'private' parent policy, matching create_tenant_user on a real
# provision; the bare mkdir above lands at the umask default (0755).
chmod 0700 "$repo/env"
# WI#330: the fixture tenant has opencode+kimi enabled (see tenant_worker_enabled
# below), and upgrade-tenant now backfills missing worker LLM base URLs before
# rebuilding images — give it a derivable tenant endpoint to inherit.
printf 'LLM_BASE_URL=http://tz-fixture:3030/openai/v1\n' >"$repo/env/lunarwing.env"
printf 'runtime data\n' >"$repo/state/runtime.txt"
if output="$(prepare_tenant_checkout fixture "$repo" HEAD 2>&1)"; then
  pass 'build-regenerated Cargo.lock is accepted'
else
  fail "build-regenerated Cargo.lock should be accepted: $output"
fi
if [[ "$(<"$lockfile")" == 'committed lock' ]]; then
  pass 'unstaged Cargo.lock change is restored'
else
  fail 'unstaged Cargo.lock change was not restored'
fi
if [[ -f "$repo/state/runtime.txt" ]]; then
  pass 'untracked runtime directories are preserved'
else
  fail 'untracked runtime directory was removed'
fi
if [[ "$output" == *'ic/channels-src/xmpp/Cargo.lock'* ]]; then
  pass 'restored lockfile is reported'
else
  fail 'restored lockfile was not reported'
fi

printf 'build-regenerated lock\n' >"$lockfile"
printf 'operator edit\n' >"$tracked_file"
if output="$(prepare_tenant_checkout fixture "$repo" HEAD 2>&1)"; then
  fail 'tracked source edit should block checkout'
elif [[ "$output" == *'tracked changes'* && "$output" == *'README.md'* ]]; then
  pass 'tracked source edit blocks checkout with details'
else
  fail "tracked source edit returned wrong error: $output"
fi
if [[ "$(<"$lockfile")" == 'build-regenerated lock' && "$(<"$tracked_file")" == 'operator edit' ]]; then
  pass 'gate preserves whole tree when source edit exists'
else
  fail 'gate changed files while rejecting source edit'
fi
git -C "$repo" restore --worktree -- "$tracked_file" "$lockfile"

printf 'intentional staged lock\n' >"$lockfile"
git -C "$repo" add "$lockfile"
if output="$(prepare_tenant_checkout fixture "$repo" HEAD 2>&1)"; then
  fail 'staged Cargo.lock edit should block checkout'
elif [[ "$output" == *'tracked changes'* && "$output" == *'Cargo.lock'* ]]; then
  pass 'staged Cargo.lock edit is treated as intentional'
else
  fail "staged Cargo.lock edit returned wrong error: $output"
fi
if [[ "$(<"$lockfile")" == 'intentional staged lock' ]] \
  && ! git -C "$repo" diff --cached --quiet -- "$lockfile"; then
  pass 'staged Cargo.lock edit is preserved'
else
  fail 'staged Cargo.lock edit was discarded'
fi
git -C "$repo" restore --staged --worktree -- "$lockfile"

printf 'operator edit\n' >"$tracked_file"
stopped_marker="$fixture/stopped"
restarted_marker="$fixture/restarted"
build_args_file="$fixture/build-args"
fixture_repo="$repo"
export BUILD_LOCK="$fixture/build.lock"
HEALTH_LAUNCHER="$fixture/health-launcher"
HEALTH_LIB_DIR="$fixture/health-lib"
health_refresh_log="$fixture/health-refresh.log"
mkdir -p "$HEALTH_LIB_DIR"
# remove_health_pipeline leaves the copy in place; that alone must not trigger
# a fresh host-global install during a tenant upgrade.
: >"$HEALTH_LIB_DIR/cron-wrapper.sh"
ensure_health_pipeline() { printf 'refreshed\n' >>"$health_refresh_log"; }
move_ref_on_stop=false
tenant_lw_root() { printf '%s\n' "$fixture_repo"; }
tenant_repo() { printf '%s/ic\n' "$fixture_repo"; }
with_tenant_lifecycle_lock() { shift; "$@"; }
tenant_worker_enabled() { [[ "$1" == "fixture" && ( "$2" == "opencode" || "$2" == "kimi" ) ]]; }
stop_tenant() {
  : >"$stopped_marker"
  if [[ "$move_ref_on_stop" == "true" ]]; then
    git -C "$fixture_repo" update-ref refs/remotes/origin/moving "$fixture_invalid_commit"
  fi
}
start_tenant() { : >"$restarted_marker"; }
build_tenant() { printf '%s\n' "$*" >"$build_args_file"; }
# WI#527: worker candidates build to candidate tags before the stop, and the
# post-stop build_tenant call carries the daemon/bridge/WASM flags only.
tenant_state_dir() { printf '%s\n' "$fixture/state"; }
candidate_log="$fixture/candidate-events"
: >"$candidate_log"
build_candidate_workers() { printf 'candidates\n' >>"$candidate_log"; }
promote_candidate_workers() { :; }
cleanup_candidate_workers() { :; }
upgrade_candidate_lanes() { :; }
patch_tenant_env() { :; }
target="$(git -C "$repo" rev-parse HEAD)"
if output="$(upgrade_tenant fixture "$target" '' false false 2>&1)"; then
  fail 'upgrade should reject dirty tracked source'
elif [[ -e "$stopped_marker" ]]; then
  fail 'upgrade stopped tenant before clean-tree gate'
elif [[ "$output" == *'tenant was not stopped'* ]]; then
  pass 'upgrade rejects dirty tree before stopping tenant'
else
  fail "upgrade dirty-tree error lacked safe-stop result: $output"
fi
git -C "$repo" restore --worktree -- "$tracked_file"

if output="$(upgrade_tenant fixture invalid-layout '' false false 2>&1)"; then
  fail 'upgrade should reject target without migrations directory'
elif [[ -e "$stopped_marker" ]]; then
  fail 'upgrade stopped tenant before target-layout validation'
elif [[ "$output" == *'has no ic/migrations directory'* && "$output" == *'tenant was not stopped'* ]]; then
  pass 'upgrade validates target layout before stopping tenant'
else
  fail "upgrade target-layout error lacked safe-stop result: $output"
fi

if output="$(upgrade_tenant fixture collision '' false false 2>&1)"; then
  fail 'upgrade should reject untracked target collision'
elif [[ -e "$stopped_marker" ]]; then
  fail 'upgrade stopped tenant before target-collision preflight'
elif [[ "$output" == *'state/runtime.txt'* && "$output" == *'tenant was not stopped'* ]]; then
  pass 'upgrade rejects untracked target collision before stopping tenant'
else
  fail "upgrade target-collision error lacked details: $output"
fi

# WI#330: an unresolved endpoint must not turn a failed upgrade into downtime.
printf 'LLM_BASE_URL=http://127.0.0.1:10004/v1\n' >"$repo/env/lunarwing.env"
if output="$(upgrade_tenant fixture "$target" '' false false 2>&1)"; then
  fail 'upgrade should reject enabled workers with no usable endpoint'
elif [[ -e "$stopped_marker" || -e "$build_args_file" ]]; then
  fail 'upgrade stopped or rebuilt tenant before worker endpoint preflight'
elif [[ "$output" == *'no container-reachable LLM endpoint'* ]] \
  && [[ "$(git -C "$repo" rev-parse HEAD)" == "$target" ]]; then
  pass 'upgrade rejects missing worker endpoint before stop, checkout, or build'
else
  fail "upgrade endpoint preflight returned wrong error: $output"
fi
printf 'LLM_BASE_URL=http://tz-fixture:3030/openai/v1\n' >"$repo/env/lunarwing.env"

fail_checkout=true
if output="$(upgrade_tenant fixture checkout-failure '' false false 2>&1)"; then
  fail 'upgrade should reject failed checkout'
elif [[ -e "$stopped_marker" ]]; then
  fail 'upgrade stopped the tenant before a checkout it could not complete'
elif [[ "$output" == *'simulated checkout failure'* && "$output" == *'still running its previous build'* ]]; then
  pass 'actual checkout failure reports tenant still running its previous build'
else
  fail "upgrade checkout error lacked recovery result: $output"
fi
fail_checkout=false

rm -f "$stopped_marker" "$restarted_marker"
move_ref_on_stop=true
moving_commit="$(git -C "$repo" rev-parse HEAD)"
if output="$(upgrade_tenant fixture moving '' false false 2>&1)" \
  && [[ -e "$stopped_marker" && -e "$restarted_marker" ]] \
  && [[ "$(git -C "$repo" rev-parse HEAD)" == "$moving_commit" ]]; then
  pass 'upgrade pins fetched target commit across stop and checkout'
else
  fail "upgrade did not pin moving target commit: $output"
fi
# WI#527: the post-stop build_tenant builds only the daemon/bridge/WASM; the
# selected worker lanes build to candidate tags before the stop.
if [[ -f "$build_args_file" ]] \
  && [[ "$(<"$build_args_file")" == "fixture true false false false false true false" ]] \
  && grep -qx 'candidates' "$candidate_log"; then
  pass 'upgrade rebuilds selected worker lanes as pre-stop candidates with strict WASM'
else
  fail "upgrade forwarded wrong build flags: $(cat "$build_args_file" 2>/dev/null || true)"
fi
# WI#330 CX-2: the pre-build backfill gave both enabled workers an inherited
# base URL from the tenant's endpoint (the placeholder-refusing images would
# otherwise kill them on first start after the upgrade).
if [[ "$(sed -n 's/^OPENCODE_BASE_URL=//p' "$repo/env/lunarwing.env" | tail -1)" == '"http://tz-fixture:3030/openai/v1"' ]] \
  && [[ "$(sed -n 's/^KIMI_BASE_URL=//p' "$repo/env/lunarwing.env" | tail -1)" == '"http://tz-fixture:3030/openai/v1"' ]] \
  && grep -q '^OPENCODE_BASE_URL_SOURCE=inherited$' "$repo/env/lunarwing.env" \
  && grep -q '^KIMI_BASE_URL_SOURCE=inherited$' "$repo/env/lunarwing.env"; then
  pass 'upgrade backfills inherited worker base URLs before rebuilding images'
else
  fail "upgrade did not backfill worker base URLs: $(grep -E 'BASE_URL' "$repo/env/lunarwing.env" || true)"
fi
if [[ ! -e "$health_refresh_log" ]]; then
  pass 'upgrade leaves a removed host-global health pipeline uninstalled'
else
  fail 'upgrade refreshed a removed host-global health pipeline'
fi

# The fleet opt-out controls first-time auto-install; an existing host-global
# schedule still needs its scripts refreshed from the current admin tree.
: >"$HEALTH_LAUNCHER"
export DEFAULT_HEALTH_ENABLED=false
if output="$(upgrade_tenant fixture moving '' false false 2>&1)" \
  && [[ "$(wc -l <"$health_refresh_log")" -eq 1 ]]; then
  pass 'upgrade refreshes an installed health pipeline once despite auto-install opt-out'
else
  fail "upgrade did not refresh an installed health pipeline: $output"
fi

# !1547 EG-1/EP-4: migration precedes backup probes; a proxy-enabled OpenRC
# tenant is env-validated (not fully re-rendered) before stop, so stop keeps the
# installed unit contract. --skip-render cannot bypass fleet neutralization.
events="$fixture/rollout-events"
# WI#527: from here on the candidate build is part of the observable order.
build_candidate_workers() { printf 'candidates\n' >>"$events"; }
render_all_openrc_proxy_units() { printf 'neutralize\n' >>"$events"; }
render_tenant_units() { printf 'render\n' >>"$events"; }
backup_tenant_postgres() { printf 'backup\n' >>"$events"; }
stop_tenant() { printf 'stop\n' >>"$events"; }
start_tenant() { printf 'start\n' >>"$events"; }
for init in openrc systemd; do
  INIT_SYSTEM="$init"
  for do_render in true false; do
    : >"$events"
    expected=""
    [[ "$init" != openrc ]] || expected+=$'neutralize\n'
    expected+=$'backup\n'
    expected+=$'candidates\n'
    expected+=$'stop\n'
    [[ "$do_render" != true ]] || expected+=$'render\n'
    expected+='start'
    if output="$(upgrade_tenant fixture "$target" '' true "$do_render" 2>&1)" \
      && [[ "$(cat "$events")" == "$expected" ]]; then
      pass "$init upgrade rollout order (render=$do_render)"
    else
      fail "$init upgrade rollout order (render=$do_render): $output"
    fi
  done
done

# A proxy-enabled OpenRC tenant validates its env before stopping, without a
# pre-stop full render, and validation failure leaves the tenant running.
INIT_SYSTEM=openrc
PROXY_FIXTURE_ENABLED=true
tenant_proxy_enabled() { [[ "$PROXY_FIXTURE_ENABLED" == "true" ]]; }
validate_openrc_proxy_env() { printf 'validate\n' >>"$events"; }
: >"$events"
if output="$(upgrade_tenant fixture "$target" '' true true 2>&1)" \
  && [[ "$(cat "$events")" == $'neutralize\nbackup\nvalidate\ncandidates\nstop\nrender\nstart' ]]; then
  pass 'proxy-enabled OpenRC upgrade validates env before stop without pre-stop render'
else
  fail "proxy-enabled upgrade order wrong: $output"
fi
validate_openrc_proxy_env() { printf 'validate\n' >>"$events"; return 1; }
: >"$events"
if output="$(upgrade_tenant fixture "$target" '' true true 2>&1)"; then
  fail 'proxy validation failure did not block upgrade'
elif [[ "$(cat "$events")" != *stop* && "$output" == *'validation failed'* ]]; then
  pass 'proxy validation failure blocks upgrade before stop'
else
  fail "proxy validation failure still stopped the tenant: $output"
fi
tenant_proxy_enabled() { return 1; }

# Fleet neutralization failure blocks everything, and a stop failure aborts
# before checkout/build/start instead of upgrading a running tenant (EP-4).
render_all_openrc_proxy_units() { return 1; }
: >"$events"
if output="$(upgrade_tenant fixture "$target" '' true true 2>&1)"; then
  fail 'failed fleet neutralization did not block upgrade'
elif [[ -s "$events" ]]; then
  fail 'failed neutralization still invoked backup/stop/start'
else
  pass 'failed neutralization blocks upgrade before service operations'
fi
render_all_openrc_proxy_units() { printf 'neutralize\n' >>"$events"; }
stop_tenant() { printf 'stop\n' >>"$events"; return 1; }
: >"$events"
if output="$(upgrade_tenant fixture "$target" '' true true 2>&1)"; then
  fail 'stop failure did not block upgrade'
elif [[ "$(cat "$events")" == $'neutralize\nbackup\ncandidates\nstop' && "$output" == *'not upgrading'* ]]; then
  pass 'stop failure aborts upgrade before checkout and start'
else
  fail "stop failure was ignored: $output"
fi

# WI#469 / !2010 QN-1 / !2022 QT-2: drive both upgrade layers and the REAL
# lifecycle/writer wrappers, worker pass and Kimi starter. Capture outside
# `if`/`||` so inner worker errexit is actually tested. Stub other lanes and
# host ownership/init/runtime operations; no live tenant is involved.
eval "$REAL_LIFECYCLE_LOCK"
TENANT_LIFECYCLE_LOCK_DIR="$fixture/lifecycle-locks"
install() { command mkdir -p "$TENANT_LIFECYCLE_LOCK_DIR"; command chmod 700 "$TENANT_LIFECYCLE_LOCK_DIR"; }
stat() {
  if [[ "$*" == "-c %u:%a $TENANT_LIFECYCLE_LOCK_DIR" ]]; then printf '0:700\n';
  else command stat "$@"; fi
}
stop_tenant() { printf 'stop\n' >>"$events"; }
tenant_worker_enabled() { return 0; }
tenant_darkirc_enabled() { [[ "$use_darkirc" == true ]]; }
darkirc_managed_state_exists() { return 0; }
darkirc_scope_id() { printf '%032d' 1; }
validate_darkirc_scope_id() { :; }
run_darkirc_key_helper() { :; }
darkirc_writer_lock() {
  exec {writer_fd}>"$fixture/darkirc.lock"
  flock -x "$writer_fd"
}
darkirc_writer_unlock() { flock -u "$writer_fd"; exec {writer_fd}>&-; }
# Keep the real helper and Kimi starter: Pebble's status-4 config refusal and
# a stale Kimi image's status 3 must stay lane-local through upgrades, whereas
# real _ctr-run/credential-assignment failures must not record a hash, report
# ready or attempt opencode2.
start_tenant() { _start_tenant_workers "$1"; }
start_tenant_pebble() { [[ "$startup_mode" != degraded ]] || return 4; }
start_tenant_opencode() { :; }
start_tenant_opencode2() { printf 'later-lane\n' >>"$events"; }
start_tenant_lunardepth() { :; }
tenant_worker_desired_running() { return 0; }
ensure_container_runtime() { CONTAINER_RT=podman; MT_ROOTLESS=false; }
# This block enables every worker (tenant_worker_enabled returns 0), so the
# WI#456 pre-stop opencode2 builder floor would probe the real host builder.
# Stub it: the floor itself is pinned by
# test-mt-admin-upgrade-opencode2-preflight.sh and test-mt-admin-opencode2-build-network.sh.
opencode2_builder_supported() { OPENCODE2_BUILDER_FLOOR_PROBLEM=""; return 0; }
lunardepth_preflight() { :; }
ports_get() { printf '19091\n'; }
_ensure_tenant_image() { return 0; }
_worker_launch_contract() { printf fixture-contract; }
prepare_worker_egress() { WORKER_EGRESS_LAUNCH_ARGS=(); }
worker_egress_reconcile_container() { :; }
worker_egress_network_name() { printf fixture-egress; }
_openrc_podman_uses_init_health() { return 1; }
_container_config_changed() { return 1; }
_store_container_hash() { printf 'hash\n' >>"$events"; }
_register_worker_unit() { printf 'unit\n' >>"$events"; }
prepare_worker_workspace() { printf '%s/state' "$repo"; }
prepare_worker_task_launch() {
  TASK_WORKSPACE_DIR="$repo/state"
  TASK_USERNS_ARGS=() TASK_ENV_FLAGS=() TASK_MOUNTS=() TASK_RESTART_ARG=()
}
start_worker_task_container() { :; }
write_kimi_runtime_env() { [[ "$startup_mode" != auth ]] || return 1; printf '%s/env/kimi-runtime.env' "$repo"; }
_ctr() {
  case "$2" in
    image)
      if [[ "$startup_mode" == degraded ]]; then printf '0:0\n';
      else printf '%s:%s\n' "$KIMI_CONTAINER_UID" "$KIMI_CONTAINER_GID"; fi ;;
    container)
      [[ "$3" == inspect ]] || return 0
      [[ "$startup_mode" == clean ]] || return 1
      printf 'true\n' ;;
    inspect)
      [[ "$startup_mode" == clean ]] || return 1
      printf 'true\n' ;;
    run) printf 'run\n' >>"$events"; return 125 ;;
  esac
}
for INIT_SYSTEM in systemd openrc; do
  for use_darkirc in false true; do
    for startup_mode in degraded runtime auth clean; do
      : >"$events"
      : >"$health_refresh_log"
      # A subsequent clean pass must not retain a previous startup's failures.
      TENANT_START_FAILED_WORKERS=(stale-previous-result)
      set +e
      output="$( {
        upgrade_tenant fixture "$target" '' false false
        result=$?
        [[ -z "${LUNARWING_MT_ACTIVE_LIFECYCLE_LOCK_FD:-}" ]] || exit 96
        flock -n "$TENANT_LIFECYCLE_LOCK_DIR/fixture.lock" true || exit 97
        flock -n "${BUILD_LOCK}.fixture.upgrade" true || exit 98
        flock -n "$fixture/darkirc.lock" true || exit 99
        exit "$result"
      } 2>&1)"
      startup_rc=$?
      set -e
      label="$INIT_SYSTEM upgrade $startup_mode (DarkIRC=$use_darkirc)"
      expected_rc=1
      [[ "$startup_mode" != runtime ]] || expected_rc=125
      [[ "$startup_mode" != clean ]] || expected_rc=0
      if [[ "$startup_rc" == "$expected_rc" && "$(wc -l <"$health_refresh_log")" == 1 ]]; then
        pass "$label preserves startup status and refreshes health, releasing all locks"
      else
        fail "$label status/health/lock contract (got $startup_rc): $output"
      fi
      if [[ "$startup_mode" == clean ]]; then
        if [[ "$output" == *"tenant 'fixture' upgraded:"* && "$output" != *stale-previous-result* ]]; then
          pass "$label reports success without stale failed lanes"
        else fail "$label did not report clean success: $output"; fi
      elif [[ "$output" == *"upgrade incomplete:"* && "$output" != *"tenant 'fixture' upgraded:"* \
          && "$output" == *"failed worker lanes: "*kimi* ]]; then
        pass "$label names failed lanes, not success"
      else fail "$label has a misleading/missing degraded summary: $output"; fi
      if [[ "$startup_mode" == degraded ]]; then
        if grep -q later-lane "$events" && [[ "$output" == *'failed worker lanes: pebble kimi'* ]]; then
          pass "$label attempts later selected lanes after two lane refusals"
        else fail "$label aborted the worker pass early: $output"; fi
      elif [[ "$startup_mode" != clean ]]; then
        if ! grep -Eq 'hash|unit|later-lane' "$events" \
            && [[ "$output" != *'kimi worker ready'* \
            && "$output" == *'failed worker lanes: kimi; not attempted: opencode2'* ]]; then
          pass "$label keeps inner runtime/auth failures fatal and names skipped lanes"
        else fail "$label continued after a fatal worker failure or hid skipped lanes: $output"; fi
      fi
    done
  done
done

# Failed-checkout recovery must never say the previous version restarted when
# its real Kimi startup failed internally; the upgrade remains failed either way.
INIT_SYSTEM=systemd
use_darkirc=false
fail_checkout=true
for startup_mode in degraded runtime auth clean; do
  : >"$events"
  : >"$health_refresh_log"
  set +e
  output="$(upgrade_tenant fixture "$target" '' false false 2>&1)"
  recovery_rc=$?
  set -e
  if [[ "$recovery_rc" == 1 && ! -s "$health_refresh_log" ]]; then
    pass "failed checkout ($startup_mode) remains failed without post-upgrade health refresh"
  else fail "failed checkout ($startup_mode) lost its failure: $output"; fi
  if [[ "$startup_mode" == clean ]]; then
    if [[ "$output" == *'still running its previous build'* ]]; then
      pass 'healthy old-version recovery is reported accurately'
    else fail "healthy recovery was not reported: $output"; fi
  elif [[ "$output" == *'still running its previous build'* && "$output" != *'automatic tenant restart failed'* ]]; then
    pass "failed checkout ($startup_mode) does not claim successful recovery"
  else fail "failed checkout ($startup_mode) falsely reports recovery: $output"; fi
done

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all mt-admin upgrade clean-tree tests passed\n'
