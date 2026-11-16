#!/usr/bin/env bash
# WI#715: the root-side Study Notes sync runs from a staged copy of mt-admin.
# upgrade-tenant must restage that copy from the checkout it upgrades from when
# (and only when) a scheduler is already installed, so fixes — including the
# WI#445 trust gate — actually reach the hourly root job. No live runtime is
# used; only the tenant/init boundaries are stubbed, and the staged release's
# provenance stamp is produced by the production installer.
# shellcheck disable=SC2034  # fixture state is consumed by sourced helpers.
set -euo pipefail

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$TEST_DIR/../lunarwing-mt-admin.sh"
export INIT_SYSTEM=systemd

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
failures=0
pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }

fixture_repo="$fixture/repo"
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
tenant_state_dir() { printf '%s\n' "$fixture/state"; }
mkdir -p "$fixture/state"
build_candidate_workers() { :; }
promote_candidate_workers() { :; }
cleanup_candidate_workers() { :; }
upgrade_candidate_lanes() { :; }
tenant_worker_enabled() { return 1; }
clear_promote_rollback() { :; }

# Fake systemd + fixture admin root so the restaged timer is observable without
# touching the host, including on hosts that really run the scheduler.
export SYSTEMCTL_LOG="$fixture/systemctl.log"
mkdir -p "$fixture/bin" "$fixture/systemd"
cat >"$fixture/bin/systemctl" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$SYSTEMCTL_LOG"
EOF
chmod +x "$fixture/bin/systemctl"
PATH="$fixture/bin:$PATH"
# shellcheck disable=SC2034  # Consumed by sourced mt-admin functions.
export LUNARWING_MT_SYSTEMD_UNIT_DIR="$fixture/systemd"
# shellcheck disable=SC2034  # Consumed by sourced mt-admin functions.
export LUNARWING_MT_KNOWLEDGE_SYNC_ADMIN_ROOT="$fixture/admin"
checkout_commit="$(git -C "$SCRIPT_DIR" rev-parse HEAD)"

# ── host without a scheduler: an upgrade must not create one ─────────────────
set +e
output="$(upgrade_tenant_locked fixture "$target" '' false false 2>&1)"
rc=$?
set -e
if [[ "$rc" -eq 0 ]] && grep -qx 'start' "$events"; then
  pass 'upgrade completes on a host without the scheduler'
else
  fail "upgrade without a scheduler failed (rc=$rc): $output"
fi
if [[ -e "$fixture/admin/current" || -e "$fixture/admin/releases" ]]; then
  fail 'upgrade created a scheduler on a host that never armed it'
else
  pass 'upgrade leaves a host without the scheduler untouched'
fi

# ── host with a stale scheduler: upgrade restages from this checkout ────────
old_release="$fixture/admin/releases/20260101000000-1"
mkdir -p "$old_release/mt-admin-lib"
printf 'commit=deadbeef\nsource=/old/checkout\nstaged_at=2026-01-01T00:00:00Z\n' \
  >"$old_release/STAGED-FROM"
printf '#!/usr/bin/env bash\n' >"$old_release/lunarwing-mt-admin.sh"
chmod +x "$old_release/lunarwing-mt-admin.sh"
ln -s -- "$old_release" "$fixture/admin/current"
: >"$events"
: >"$SYSTEMCTL_LOG"

set +e
output="$(upgrade_tenant_locked fixture "$target" '' false false 2>&1)"
rc=$?
set -e
if [[ "$rc" -eq 0 ]] && grep -qx 'start' "$events"; then
  pass 'upgrade completes on a host with the scheduler'
else
  fail "upgrade with a scheduler failed (rc=$rc): $output"
fi

staged_commit="$(sed -n 's/^commit=//p' "$fixture/admin/current/STAGED-FROM")"
if [[ "$staged_commit" == "$checkout_commit" ]]; then
  pass 'upgrade restages current from the upgraded-from checkout'
else
  fail "current names commit '$staged_commit', checkout is '$checkout_commit'"
fi
if [[ "$(find "$fixture/admin/releases" -mindepth 1 -maxdepth 1 -type d | wc -l)" -eq 2 ]] \
  && [[ -d "$old_release" ]]; then
  pass 'restage publishes a new release and keeps the previous one'
else
  fail 'restage did not publish a distinct release'
fi
if grep -Fqx 'enable --now lunarwing-study-notes-sync.timer' "$SYSTEMCTL_LOG"; then
  pass 'restage re-arms the hourly timer'
else
  fail 'restage did not re-arm the hourly timer'
fi
if [[ -x "$fixture/admin/current/lunarwing-mt-admin.sh" ]] \
  && "$fixture/admin/current/lunarwing-mt-admin.sh" --help 2>&1 \
    | grep -Fq 'sync-knowledge-sources'; then
  pass 'restaged entrypoint is executable and dispatches the sync verb'
else
  fail 'restaged entrypoint is not runnable'
fi

# A second upgrade already staged from this checkout must not churn a release.
: >"$events"
set +e
output="$(upgrade_tenant_locked fixture "$target" '' false false 2>&1)"
rc=$?
set -e
if [[ "$rc" -eq 0 ]] \
  && [[ "$(find "$fixture/admin/releases" -mindepth 1 -maxdepth 1 -type d | wc -l)" -eq 2 ]]; then
  pass 'a second upgrade does not restage an already-current snapshot'
else
  fail "second upgrade restaged again or failed (rc=$rc): $output"
fi

if [[ "$failures" -ne 0 ]]; then
  printf '%s failure(s)\n' "$failures" >&2
  exit 1
fi
printf 'Knowledge sync scheduler upgrade tests passed\n'
