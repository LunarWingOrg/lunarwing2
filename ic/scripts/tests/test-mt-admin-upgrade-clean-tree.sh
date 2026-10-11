#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$SCRIPT_DIR/../lunarwing-mt-admin.sh"

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

fail_checkout=true
if output="$(upgrade_tenant fixture checkout-failure '' false false 2>&1)"; then
  fail 'upgrade should reject failed checkout'
elif [[ ! -e "$stopped_marker" || ! -e "$restarted_marker" ]]; then
  fail 'upgrade did not restart tenant after checkout failed'
elif [[ "$output" == *'simulated checkout failure'* && "$output" == *'previous tenant version restarted'* ]]; then
  pass 'actual checkout failure restarts previous tenant version'
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
if [[ -f "$build_args_file" ]] \
  && [[ "$(<"$build_args_file")" == "fixture true false true true false true false" ]]; then
  pass 'upgrade rebuilds selected OpenCode and Kimi workers with strict WASM build'
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

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all mt-admin upgrade clean-tree tests passed\n'
