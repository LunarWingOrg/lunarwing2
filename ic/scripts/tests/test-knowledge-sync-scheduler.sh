#!/usr/bin/env bash
set -euo pipefail

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$TEST_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d)"
cleanup() { rm -rf "$fixture"; }
trap cleanup EXIT
mkdir -p "$fixture/bin" "$fixture/systemd"

export SYSTEMCTL_LOG="$fixture/systemctl.log"
cat >"$fixture/bin/systemctl" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$SYSTEMCTL_LOG"
EOF
chmod +x "$fixture/bin/systemctl"
PATH="$fixture/bin:$PATH"

# shellcheck disable=SC2034  # Consumed by sourced mt-admin functions.
LUNARWING_MT_SYSTEMD_UNIT_DIR="$fixture/systemd"
# shellcheck disable=SC2034  # Consumed by sourced mt-admin functions.
LUNARWING_MT_KNOWLEDGE_SYNC_ADMIN_ROOT="$fixture/admin"
stable_entrypoint="$fixture/admin/current/lunarwing-mt-admin.sh"
INIT_SYSTEM=systemd
ensure_knowledge_source_scheduler
test -x "$stable_entrypoint"
test -x "$fixture/admin/current/mt-admin-lib/14-knowledge-sync.sh"
grep -Fq "ExecStart=$stable_entrypoint sync-knowledge-sources" \
  "$fixture/systemd/lunarwing-study-notes-sync.service"
grep -Fq 'OnCalendar=hourly' "$fixture/systemd/lunarwing-study-notes-sync.timer"
grep -Fqx 'daemon-reload' "$SYSTEMCTL_LOG"
grep -Fqx 'enable --now lunarwing-study-notes-sync.timer' "$SYSTEMCTL_LOG"

# WI#715: the published release names the checkout commit it was staged from,
# so a frozen snapshot can be told apart from a fresh one.
checkout_commit="$(git -C "$SCRIPT_DIR" rev-parse HEAD)"
staged_commit="$(sed -n 's/^commit=//p' "$fixture/admin/current/STAGED-FROM")"
[[ "$staged_commit" == "$checkout_commit" ]] || {
  printf 'staged release names %s, checkout is %s\n' \
    "$staged_commit" "$checkout_commit" >&2
  exit 1
}
grep -Fqx "source=$SCRIPT_DIR" "$fixture/admin/current/STAGED-FROM"

export FCRONTAB_STATE="$fixture/fcrontab"
cat >"$fixture/bin/fcrontab" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "-l" ]]; then
  [[ ! -f "$FCRONTAB_STATE" ]] || cat "$FCRONTAB_STATE"
  exit 0
fi
cp "$1" "$FCRONTAB_STATE"
EOF
chmod +x "$fixture/bin/fcrontab"
_ensure_cron_runlevel() { :; }
# shellcheck disable=SC2034  # Consumed by sourced mt-admin functions.
INIT_SYSTEM=openrc
ensure_knowledge_source_scheduler
ensure_knowledge_source_scheduler
grep -Fq "17 * * * * $stable_entrypoint sync-knowledge-sources" \
  "$FCRONTAB_STATE"
[[ "$(grep -c '^# BEGIN lunarwing-study-notes-sync managed block$' "$FCRONTAB_STATE")" -eq 1 ]]
[[ "$(grep -c '^# END lunarwing-study-notes-sync managed block$' "$FCRONTAB_STATE")" -eq 1 ]]

# Repeated installs keep only the newest releases: old timestamped snapshots
# are pruned after the current symlink flips, and the live release survives.
for _ in $(seq 1 $((KNOWLEDGE_SYNC_SNAPSHOT_KEEP + 2))); do
  ensure_knowledge_source_scheduler
done
release_count="$(find "$fixture/admin/releases" -mindepth 1 -maxdepth 1 -type d | wc -l)"
[[ "$release_count" -le "$((KNOWLEDGE_SYNC_SNAPSHOT_KEEP + 1))" ]] || {
  printf 'old knowledge sync snapshots were not pruned (%s left)\n' "$release_count" >&2
  exit 1
}
[[ -x "$stable_entrypoint" ]] || {
  printf 'pruning removed the live knowledge sync snapshot\n' >&2
  exit 1
}

# WI#684: the release is self-contained. Its parent has no sibling health
# checkout, and loading must not fall back to the installed host health library.
test ! -e "$fixture/admin/ic-infrastructure-health-check"
test -x "$fixture/admin/current/ic-infrastructure-health-check/openrc-proxy-unit-contract.sh"
(
  cd "$fixture"
  "$stable_entrypoint" --help
) >"$fixture/snapshot-help"
grep -Fq 'sync-knowledge-sources' "$fixture/snapshot-help"

# Drive the staged dispatcher against a real Study Notes checkout. Only the
# host account/init boundaries are stubbed; registry parsing, tenant filtering,
# Git synchronization and generation publication remain the production paths.
study_origin="$fixture/study-origin"
enabled_home="$fixture/shared/enabled"
mkdir -p "$study_origin/agent-notes-by-agent/test-agent" \
  "$study_origin/skills/study-notes"
touch "$study_origin/study_notes_mcp.py" "$study_origin/memorydb.py" \
  "$study_origin/agent-notes.sqlite3" "$study_origin/skills/study-notes/SKILL.md"
printf 'snapshot v1\n' >"$study_origin/agent-notes-by-agent/test-agent/topic.md"
git -C "$study_origin" init -q -b main
git -C "$study_origin" add -A
git -C "$study_origin" -c user.email=test@example.com -c user.name=test commit -q -m initial

for name in enabled unset false zero; do
  mkdir -p "$fixture/env/$name"
done
printf 'KNOWLEDGE_SYNC_ENABLED=true\nLW2STUDYMCP_HOME=%s\n' "$enabled_home" \
  >"$fixture/env/enabled/lunarwing.env"
printf 'LW2STUDYMCP_HOME=%s\n' "$fixture/shared/unset" \
  >"$fixture/env/unset/lunarwing.env"
printf 'KNOWLEDGE_SYNC_ENABLED=false\nLW2STUDYMCP_HOME=%s\n' "$fixture/shared/false" \
  >"$fixture/env/false/lunarwing.env"
printf 'KNOWLEDGE_SYNC_ENABLED=0\nLW2STUDYMCP_HOME=%s\n' "$fixture/shared/zero" \
  >"$fixture/env/zero/lunarwing.env"
jq -n '{version:16,tenants:{enabled:{},unset:{},false:{},zero:{}}}' >"$fixture/ports.json"

# shellcheck disable=SC2034  # Fixture values are consumed by sourced mt-admin functions.
run_snapshot_sync() (
  # shellcheck source=../lunarwing-mt-admin.sh
  source "$stable_entrypoint"
  require_root() { :; }
  openrc_dispatch_preflight() { :; }
  tenant_env_dir() { printf '%s/env/%s' "$fixture" "$1"; }
  PORTS_REGISTRY="$fixture/ports.json"
  STUDY_NOTES_REPO="$study_origin"
  STUDY_NOTES_DEFAULT_HOME="$enabled_home"
  main sync-knowledge-sources
)

sync_output="$(run_snapshot_sync)"
printf 'snapshot v2\n' >"$study_origin/agent-notes-by-agent/test-agent/topic.md"
git -C "$study_origin" add -A
git -C "$study_origin" -c user.email=test@example.com -c user.name=test commit -q -m refresh
sync_output+="$(run_snapshot_sync)"
grep -Fqx 'snapshot v2' "$enabled_home/agent-notes-by-agent/test-agent/topic.md"
[[ "$(jq -r '.source_revision' "$enabled_home/.git/lunarwing-study-notes-generation.json")" \
  == "$(git -C "$study_origin" rev-parse HEAD)" ]]
for name in unset false zero; do
  [[ ! -e "$fixture/shared/$name" ]] || {
    printf 'disabled tenant %s unexpectedly reached Study Notes sync\n' "$name" >&2
    exit 1
  }
done
[[ "$sync_output" != *"enables knowledge sync"* ]] || {
  printf 'unset KNOWLEDGE_SYNC_ENABLED produced an enabled-tenant warning\n' >&2
  exit 1
}
[[ "$sync_output" != *"tenant 'false'"* && "$sync_output" != *"tenant 'zero'"* ]] || {
  printf 'false/0 KNOWLEDGE_SYNC_ENABLED produced a warning\n' >&2
  exit 1
}

# WI#715: upgrade-tenant must restage an EXISTING scheduler snapshot and must
# never invent one on a host that never armed it. Drive the same function the
# upgrade calls (after refresh_deployed_health_pipeline).
export LUNARWING_MT_KNOWLEDGE_SYNC_ADMIN_ROOT="$fixture/refresh-admin"
refresh_deployed_knowledge_sync_scheduler
[[ ! -e "$fixture/refresh-admin/current" && ! -e "$fixture/refresh-admin/releases" ]] || {
  printf 'refresh created a scheduler on a host without one\n' >&2
  exit 1
}

# A snapshot staged from an older checkout, as on a host that enabled knowledge
# sync before a fix landed.
old_release="$fixture/refresh-admin/releases/20260101000000-1"
mkdir -p "$old_release/mt-admin-lib"
printf 'commit=deadbeef\nsource=/old/checkout\nstaged_at=2026-01-01T00:00:00Z\n' \
  >"$old_release/STAGED-FROM"
printf '#!/usr/bin/env bash\n' >"$old_release/lunarwing-mt-admin.sh"
chmod +x "$old_release/lunarwing-mt-admin.sh"
ln -s -- "$old_release" "$fixture/refresh-admin/current"

refresh_output="$(refresh_deployed_knowledge_sync_scheduler)"
[[ "$refresh_output" == *"restaged from $checkout_commit"* ]] || {
  printf 'restage did not report the new commit: %s\n' "$refresh_output" >&2
  exit 1
}
new_release_count="$(find "$fixture/refresh-admin/releases" -mindepth 1 -maxdepth 1 -type d | wc -l)"
[[ "$new_release_count" -eq 2 ]] || {
  printf 'restage published %s releases, expected 2\n' "$new_release_count" >&2
  exit 1
}
test -d "$old_release"
[[ "$(sed -n 's/^commit=//p' "$fixture/refresh-admin/current/STAGED-FROM")" == "$checkout_commit" ]] || {
  printf 'current does not point at a release staged from the new checkout\n' >&2
  exit 1
}

# An already-current snapshot is left alone (no release churn on every upgrade).
refresh_output="$(refresh_deployed_knowledge_sync_scheduler)"
[[ "$refresh_output" == *"already staged from $checkout_commit"* ]]
[[ "$(find "$fixture/refresh-admin/releases" -mindepth 1 -maxdepth 1 -type d | wc -l)" -eq 2 ]]

# One operator command prints the provenance: doctor calls this reporter.
report_output="$(report_knowledge_sync_scheduler_status)"
[[ "$report_output" == *"knowledge sync scheduler: staged from $checkout_commit"* ]] || {
  printf 'status did not print the staged commit: %s\n' "$report_output" >&2
  exit 1
}
[[ "$report_output" != *"[WARN]"* ]] || {
  printf 'status warned about a fresh snapshot: %s\n' "$report_output" >&2
  exit 1
}

# A snapshot that lags the checkout is reported as stale, not silently run.
ln -sfn -- "$old_release" "$fixture/refresh-admin/current"
report_output="$(report_knowledge_sync_scheduler_status)"
[[ "$report_output" == *"[WARN] knowledge sync scheduler is stale"* ]] || {
  printf 'status did not warn about a stale snapshot: %s\n' "$report_output" >&2
  exit 1
}

# WI#715 box 2 / review ADV-2: removing the hourly schedule is the operator's
# call, and an upgrade must not restore it. On OpenRC the managed crontab block
# IS the schedule, so a host without that block gets its staged copy refreshed
# and nothing else.
INIT_SYSTEM=openrc
sed '/^# BEGIN lunarwing-study-notes-sync managed block$/,/^# END lunarwing-study-notes-sync managed block$/d' \
  "$FCRONTAB_STATE" >"$FCRONTAB_STATE.tmp"
mv "$FCRONTAB_STATE.tmp" "$FCRONTAB_STATE"
refresh_output="$(refresh_deployed_knowledge_sync_scheduler)"
[[ "$refresh_output" == *"hourly schedule is not active"* ]] || {
  printf 'refresh did not notice the removed cron block: %s\n' "$refresh_output" >&2
  exit 1
}
[[ "$(sed -n 's/^commit=//p' "$fixture/refresh-admin/current/STAGED-FROM")" == "$checkout_commit" ]] || {
  printf 'a host without the hourly schedule did not get its staged copy refreshed\n' >&2
  exit 1
}
if grep -Fq 'lunarwing-study-notes-sync managed block' "$FCRONTAB_STATE"; then
  printf 'refresh re-created a removed cron schedule:\n%s\n' "$FCRONTAB_STATE" >&2
  exit 1
fi
report_output="$(report_knowledge_sync_scheduler_status)"
[[ "$report_output" == *"hourly schedule is not active"* ]] || {
  printf 'doctor did not report the dormant OpenRC schedule: %s\n' "$report_output" >&2
  exit 1
}

printf 'Knowledge sync scheduler tests passed\n'
