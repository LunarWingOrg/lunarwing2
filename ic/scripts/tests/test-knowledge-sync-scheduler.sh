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

printf 'Knowledge sync scheduler tests passed\n'
