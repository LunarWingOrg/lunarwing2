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
export FCRONTAB_INSTALLS="$fixture/fcrontab.installs"
: >"$FCRONTAB_INSTALLS"
cat >"$fixture/bin/fcrontab" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "-l" ]]; then
  [[ ! -f "$FCRONTAB_STATE" ]] || cat "$FCRONTAB_STATE"
  exit 0
fi
[[ -z "${FCRONTAB_FAIL_INSTALL:-}" ]] || exit 1
printf 'install\n' >>"$FCRONTAB_INSTALLS"
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
# shellcheck disable=SC2034  # Consumed by sourced mt-admin functions.
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

# Review AEG-2: on OpenRC the schedule is live only while the managed block
# holds an entry cron would run. An operator who commented the line out switched
# the schedule off, and an upgrade must not switch it on again.
managed_entry="17 * * * * $fixture/refresh-admin/current/lunarwing-mt-admin.sh sync-knowledge-sources"
job_before='5 4 * * * /usr/bin/true "$(date +\%F)" job-before\the-block'
job_after='6 4 * * * /usr/bin/true job-after-the-block'
write_managed_block() {  # <the block's lines, one argument each>
  {
    printf '%s\n' "$job_before" '# BEGIN lunarwing-study-notes-sync managed block' "$@" \
      '# END lunarwing-study-notes-sync managed block' "$job_after"
  } >"$FCRONTAB_STATE"
}
other_jobs_kept() {
  grep -Fqx -- "$job_before" "$FCRONTAB_STATE" && grep -Fqx -- "$job_after" "$FCRONTAB_STATE"
}
write_managed_block "#$managed_entry"
if knowledge_sync_scheduler_live; then
  printf 'a commented-out cron entry was counted as a live schedule\n' >&2
  exit 1
fi
ln -sfn -- "$old_release" "$fixture/refresh-admin/current"
cron_before="$(cat "$FCRONTAB_STATE")"
refresh_output="$(refresh_deployed_knowledge_sync_scheduler)"
[[ "$refresh_output" == *"hourly schedule is not active"* ]] || {
  printf 'refresh did not treat the commented-out entry as switched off: %s\n' "$refresh_output" >&2
  exit 1
}
[[ "$(cat "$FCRONTAB_STATE")" == "$cron_before" ]] || {
  printf 'refresh rewrote a commented-out cron entry:\n%s\n' "$(cat "$FCRONTAB_STATE")" >&2
  exit 1
}
write_managed_block "$managed_entry"
knowledge_sync_scheduler_live || {
  printf 'an active cron entry was not counted as a live schedule\n' >&2
  exit 1
}

# WI#736 (review AEG-3): a refused staging must leave a live schedule exactly as
# it was. The refusal comes from the real load guard: a contract source whose
# path cannot be resolved. Before the fix the installer went on with an empty
# entrypoint and rewrote root's cron line to a bare command.
ln -sfn -- "$old_release" "$fixture/refresh-admin/current"
cron_before="$(cat "$FCRONTAB_STATE")"
installs_before="$(wc -l <"$FCRONTAB_INSTALLS")"
releases_before="$(find "$fixture/refresh-admin/releases" -mindepth 1 -maxdepth 1 -type d | wc -l)"
refresh_output="$(HEALTH_SRC_DIR="$fixture/missing/health" refresh_deployed_knowledge_sync_scheduler 2>&1)"
[[ "$(wc -l <"$FCRONTAB_INSTALLS")" -eq "$installs_before" ]] || {
  printf 'a refused staging installed a crontab\n' >&2
  exit 1
}
[[ "$(cat "$FCRONTAB_STATE")" == "$cron_before" ]] || {
  printf 'a refused staging rewrote the live cron entry:\n%s\n' "$(cat "$FCRONTAB_STATE")" >&2
  exit 1
}
[[ "$(readlink -- "$fixture/refresh-admin/current")" == "$old_release" ]] || {
  printf 'a refused staging moved current\n' >&2
  exit 1
}
[[ "$refresh_output" == *"refusing health contract source"* \
  && "$refresh_output" == *"could not stage the Study Notes sync snapshot"* \
  && "$refresh_output" == *"could not restage the root Study Notes sync snapshot"* ]] || {
  printf 'a refused staging did not give the refusal as the reason: %s\n' "$refresh_output" >&2
  exit 1
}
[[ "$refresh_output" != *"scheduled with"* && "$refresh_output" != *"restaged from"* ]] || {
  printf 'a refused staging was reported as scheduled or restaged: %s\n' "$refresh_output" >&2
  exit 1
}

# The same holds for the installer itself in a context where bash ignores
# errexit, which is how the upgrade contains it.
installer_rc=0
( set -e; HEALTH_SRC_DIR="$fixture/missing/health" ensure_knowledge_source_scheduler ) \
  >/dev/null 2>&1 || installer_rc=$?
[[ "$installer_rc" -ne 0 && "$(cat "$FCRONTAB_STATE")" == "$cron_before" ]] || {
  printf 'the installer wrote a schedule after a failed staging (rc=%s):\n%s\n' \
    "$installer_rc" "$(cat "$FCRONTAB_STATE")" >&2
  exit 1
}
# Neither installer accepts a command that is not the absolute path of an
# executable, however it is reached.
# The relative path names an existing executable from the fixture directory,
# so only the "absolute path" half of the check refuses it; the directory is
# executable, so only the "regular file" half does.
for bad_entrypoint in '' 'sync-knowledge-sources' "$fixture/refresh-admin/missing/lunarwing-mt-admin.sh" \
  'refresh-admin/current/lunarwing-mt-admin.sh' "$fixture/refresh-admin/current"; do
  installer_rc=0
  ( cd "$fixture" && _install_knowledge_source_cron "$bad_entrypoint" ) >/dev/null 2>&1 || installer_rc=$?
  [[ "$installer_rc" -ne 0 && "$(cat "$FCRONTAB_STATE")" == "$cron_before" ]] || {
    printf 'the cron installer accepted the entrypoint "%s"\n' "$bad_entrypoint" >&2
    exit 1
  }
  service_before="$(cat "$fixture/systemd/lunarwing-study-notes-sync.service")"
  : >"$SYSTEMCTL_LOG"
  installer_rc=0
  ( cd "$fixture" && _install_knowledge_source_systemd_timer "$bad_entrypoint" ) >/dev/null 2>&1 || installer_rc=$?
  [[ "$installer_rc" -ne 0 && ! -s "$SYSTEMCTL_LOG" \
    && "$(cat "$fixture/systemd/lunarwing-study-notes-sync.service")" == "$service_before" ]] || {
    printf 'the systemd installer accepted the entrypoint "%s"\n' "$bad_entrypoint" >&2
    exit 1
  }
done

# Review AEG-4: a release that lacks a file is never published. A failed copy
# stands in for a full disk; the staging stops, removes the partial release and
# leaves current and the schedule alone.
staging_left_everything() {  # <what failed> <the message the operator must see>
  [[ "$(readlink -- "$fixture/refresh-admin/current")" == "$old_release" \
    && "$(cat "$FCRONTAB_STATE")" == "$cron_before" ]] || {
    printf 'with %s an incomplete release was published or scheduled: %s\n' "$1" "$refresh_output" >&2
    exit 1
  }
  [[ "$(find "$fixture/refresh-admin/releases" -mindepth 1 -maxdepth 1 -type d | wc -l)" -eq "$releases_before" ]] || {
    printf 'with %s a partial release was left behind\n' "$1" >&2
    exit 1
  }
  [[ -z "$(find "$fixture/refresh-admin" -maxdepth 1 -name '.current.*' -print -quit)" ]] || {
    printf 'with %s a temporary current link was left behind\n' "$1" >&2
    exit 1
  }
  [[ "$refresh_output" == *"$2"* && "$refresh_output" != *"restaged from"* \
    && "$refresh_output" != *"scheduled with"* ]] || {
    printf 'with %s the failed restage was not reported as one: %s\n' "$1" "$refresh_output" >&2
    exit 1
  }
}
for failed_copy in '*/mt-admin-lib/14-knowledge-sync.sh' '*/lunarwing-mt-admin.sh' \
  '*/ic-infrastructure-health-check/openrc-proxy-unit-contract.sh'; do
  install() {
    # shellcheck disable=SC2053  # The right side is the case's glob.
    [[ "${*: -1}" != $failed_copy ]] || return 1
    command install "$@"
  }
  refresh_output="$(refresh_deployed_knowledge_sync_scheduler 2>&1)"
  unset -f install
  staging_left_everything "a failed copy of $failed_copy" 'could not stage a complete Study Notes sync release'
done
# A copy that reports success but left other bytes: the comparison decides.
for bad_compare in '*/mt-admin-lib/14-knowledge-sync.sh' '*/lunarwing-mt-admin.sh' \
  '*/ic-infrastructure-health-check/openrc-proxy-unit-contract.sh'; do
  cmp() {
    # shellcheck disable=SC2053  # The right side is the case's glob.
    [[ "${*: -1}" != $bad_compare ]] || return 1
    command cmp "$@"
  }
  refresh_output="$(refresh_deployed_knowledge_sync_scheduler 2>&1)"
  unset -f cmp
  staging_left_everything "a copy of $bad_compare that differs" 'could not stage a complete Study Notes sync release'
done
# The stamp's first line cannot be written: a stamp without its commit must
# not be published.
printf() {
  [[ "$1" != 'commit=%s\n' ]] || return 1
  builtin printf "$@"
}
refresh_output="$(refresh_deployed_knowledge_sync_scheduler 2>&1)"
unset -f printf
staging_left_everything 'a failed first line of the stamp' 'could not stage a complete Study Notes sync release'
# The stamp cannot be made readable, and the publish itself fails.
chmod() { return 1; }
refresh_output="$(refresh_deployed_knowledge_sync_scheduler 2>&1)"
unset -f chmod
staging_left_everything 'a failed chmod of the stamp' 'could not stage a complete Study Notes sync release'
mv() { return 1; }
refresh_output="$(refresh_deployed_knowledge_sync_scheduler 2>&1)"
unset -f mv
staging_left_everything 'a failed publish' 'could not publish the Study Notes sync release'

# WI#736: a host that already carries the empty command is repaired by the next
# restage that succeeds.
write_managed_block '17 * * * *  sync-knowledge-sources'
refresh_output="$(refresh_deployed_knowledge_sync_scheduler 2>&1)"
grep -Fqx "$managed_entry" "$FCRONTAB_STATE" || {
  printf 'a successful restage did not repair the empty cron command:\n%s\n' "$(cat "$FCRONTAB_STATE")" >&2
  exit 1
}
[[ "$refresh_output" == *"restaged from $checkout_commit"* ]] || {
  printf 'the repairing restage was not reported: %s\n' "$refresh_output" >&2
  exit 1
}
other_jobs_kept || {
  printf 'the repairing restage lost one of the other jobs of the crontab:\n%s\n' "$(cat "$FCRONTAB_STATE")" >&2
  exit 1
}

# Review of the fix (R4): what else the probe must not take for a live
# schedule. Each shape has root's other jobs around it.
probe_shape_not_live() {  # <what the case models>
  if knowledge_sync_scheduler_live; then
    printf 'the schedule was counted as live with %s:\n%s\n' "$1" "$(cat "$FCRONTAB_STATE")" >&2
    exit 1
  fi
}
write_managed_block "   # $managed_entry"
probe_shape_not_live 'an indented comment'
write_managed_block 'PATH=/usr/local/bin:/usr/bin' 'MAILTO=""' '!nice(10)' "#$managed_entry"
probe_shape_not_live 'only environment and option lines beside a commented entry'
printf '%s\n' "$job_before" '# BEGIN lunarwing-study-notes-sync managed block' "#$managed_entry" \
  "$job_after" >"$FCRONTAB_STATE"
probe_shape_not_live 'a block without its END line in front of another job'
printf '%s\n' "$job_before" "$job_after" >"$FCRONTAB_STATE"
probe_shape_not_live 'other jobs and no managed block'
printf '%s\n' "$job_before" '# BEGIN lunarwing-study-notes-sync managed block' "$job_after" \
  '# BEGIN lunarwing-study-notes-sync managed block' "#$managed_entry" \
  '# END lunarwing-study-notes-sync managed block' >"$FCRONTAB_STATE"
probe_shape_not_live 'another job between an unclosed BEGIN line and a complete block that is switched off'
for live_entry in "@hourly $fixture/refresh-admin/current/lunarwing-mt-admin.sh sync-knowledge-sources" \
  "   $managed_entry" "&bootrun $managed_entry" \
  "%hourly 17 $fixture/refresh-admin/current/lunarwing-mt-admin.sh sync-knowledge-sources"; do
  write_managed_block "$live_entry"
  knowledge_sync_scheduler_live || {
    printf 'the entry "%s" in the managed block was not counted as live\n' "$live_entry" >&2
    exit 1
  }
done
printf '%s\n%s\n%s' '# BEGIN lunarwing-study-notes-sync managed block' "$managed_entry" \
  '# END lunarwing-study-notes-sync managed block' >"$FCRONTAB_STATE"
knowledge_sync_scheduler_live || {
  printf 'a block whose END line has no newline was not counted as live\n' >&2
  exit 1
}

# The installer replaces root's whole crontab, so it must not build the new one
# from a block it cannot delimit, and a failed write must leave the old one
# (R1). Before the fix an unterminated block took every later job with it, and
# a failed write installed an empty table.
printf '%s\n' "$job_before" '# BEGIN lunarwing-study-notes-sync managed block' "#$managed_entry" \
  "$job_after" >"$FCRONTAB_STATE"
cron_before="$(cat "$FCRONTAB_STATE")"
installs_before="$(wc -l <"$FCRONTAB_INSTALLS")"
installer_rc=0
installer_output="$( ( _install_knowledge_source_cron "$fixture/refresh-admin/current/lunarwing-mt-admin.sh" ) 2>&1 )" \
  || installer_rc=$?
[[ "$installer_rc" -ne 0 && "$(cat "$FCRONTAB_STATE")" == "$cron_before" \
  && "$(wc -l <"$FCRONTAB_INSTALLS")" -eq "$installs_before" \
  && "$installer_output" == *"without a later line that reads exactly"* \
  && "$installer_output" == *"fcrontab -e"* ]] || {
  printf 'the cron installer rebuilt a crontab around an unterminated block (rc=%s): %s\n%s\n' \
    "$installer_rc" "$installer_output" "$(cat "$FCRONTAB_STATE")" >&2
  exit 1
}
write_managed_block "$managed_entry"
cron_before="$(cat "$FCRONTAB_STATE")"
# A failing printf of the entry leaves the temporary file without it, as a
# full disk would; the stubs leave every other printf, die and say alone.
for failing in mktemp printf; do
  installs_before="$(wc -l <"$FCRONTAB_INSTALLS")"
  installer_rc=0
  installer_output="$(
    (
      set -e
      if [[ "$failing" == mktemp ]]; then
        mktemp() { return 1; }
      else
        printf() {
          [[ "$1" != "%s\n" || "$2" != "17 * * * * /safe/staged/mt-admin.sh sync-knowledge-sources" ]] || return 1
          [[ "$1" != "%s\n" || "$2" != '17 * * * * %s sync-knowledge-sources' ]] || return 1
          [[ "$2" != *" sync-knowledge-sources" ]] || return 1
          builtin printf "$@"
        }
      fi
      _install_knowledge_source_cron "$fixture/refresh-admin/current/lunarwing-mt-admin.sh"
    ) 2>&1
  )" || installer_rc=$?
  [[ "$installer_rc" -ne 0 && "$(cat "$FCRONTAB_STATE")" == "$cron_before" \
    && "$(wc -l <"$FCRONTAB_INSTALLS")" -eq "$installs_before" \
    && "$installer_output" == *"error: c"* \
    && "$installer_output" != *"scheduled with"* ]] || {
    printf 'with a failing %s the cron installer replaced the crontab (rc=%s): %s\n%s\n' \
      "$failing" "$installer_rc" "$installer_output" "$(cat "$FCRONTAB_STATE")" >&2
    exit 1
  }
done
# A cron service that cannot be enabled is not reported as a schedule.
installer_rc=0
installer_output="$(
  (
    set -e
    _ensure_cron_runlevel() { return 1; }
    _install_knowledge_source_cron "$fixture/refresh-admin/current/lunarwing-mt-admin.sh"
  ) 2>&1
)" || installer_rc=$?
[[ "$installer_rc" -ne 0 && "$installer_output" == *"cron service could not be enabled"* \
  && "$installer_output" != *"scheduled with"* ]] || {
  printf 'a cron service that could not be enabled was reported as scheduled (rc=%s): %s\n' \
    "$installer_rc" "$installer_output" >&2
  exit 1
}
other_jobs_kept || {
  printf 'the cron installer lost one of the other jobs of the crontab:\n%s\n' "$(cat "$FCRONTAB_STATE")" >&2
  exit 1
}
# The systemd installer writes beside the units and renames: a failed write
# leaves both as they were and reloads nothing.
service_before="$(cat "$fixture/systemd/lunarwing-study-notes-sync.service")"
timer_before="$(cat "$fixture/systemd/lunarwing-study-notes-sync.timer")"
for failing in mktemp cat chmod mv timer-mktemp timer-cat; do
  : >"$SYSTEMCTL_LOG"
  installer_rc=0
  installer_output="$(
    (
      set -e
      case "$failing" in
        timer-mktemp)
          mktemp() {
            [[ "$1" != *.timer.* ]] || return 1
            command mktemp "$@"
          }
          ;;
        timer-cat)
          cat_calls=0
          cat() {
            cat_calls=$((cat_calls + 1))
            ((cat_calls < 2)) || return 1
            command cat "$@"
          }
          ;;
        *) eval "$failing() { return 1; }" ;;
      esac
      _install_knowledge_source_systemd_timer "$fixture/refresh-admin/current/lunarwing-mt-admin.sh"
    ) 2>&1
  )" || installer_rc=$?
  [[ "$installer_rc" -ne 0 && ! -s "$SYSTEMCTL_LOG" \
    && "$(command cat "$fixture/systemd/lunarwing-study-notes-sync.service")" == "$service_before" \
    && "$(command cat "$fixture/systemd/lunarwing-study-notes-sync.timer")" == "$timer_before" \
    && "$installer_output" != *"scheduled with"* ]] || {
    printf 'with a failing %s the systemd installer changed a unit or reloaded (rc=%s): %s\n' \
      "$failing" "$installer_rc" "$installer_output" >&2
    exit 1
  }
  [[ -z "$(find "$fixture/systemd" -name 'lunarwing-study-notes-sync.*.*' -print -quit)" ]] || {
    printf 'with a failing %s the systemd installer left a temporary unit file\n' "$failing" >&2
    exit 1
  }
done

# A unit an operator masked is a symbolic link. The installer must refuse it:
# a rename over the link would unmask the unit.
for masked in service timer; do
  mv "$fixture/systemd/lunarwing-study-notes-sync.$masked" "$fixture/systemd/aside.$masked"
  ln -s /dev/null "$fixture/systemd/lunarwing-study-notes-sync.$masked"
  : >"$SYSTEMCTL_LOG"
  installer_rc=0
  installer_output="$(
    ( _install_knowledge_source_systemd_timer "$fixture/refresh-admin/current/lunarwing-mt-admin.sh" ) 2>&1
  )" || installer_rc=$?
  [[ "$installer_rc" -ne 0 && -L "$fixture/systemd/lunarwing-study-notes-sync.$masked" \
    && ! -s "$SYSTEMCTL_LOG" && "$installer_output" == *"symbolic link"* ]] || {
    printf 'the systemd installer replaced or enabled a masked %s unit (rc=%s): %s\n' \
      "$masked" "$installer_rc" "$installer_output" >&2
    exit 1
  }
  [[ -z "$(find "$fixture/systemd" -name 'lunarwing-study-notes-sync.*.*' -print -quit)" ]] || {
    printf 'the systemd installer left a temporary file beside a masked %s unit\n' "$masked" >&2
    exit 1
  }
  rm -f "$fixture/systemd/lunarwing-study-notes-sync.$masked"
  mv "$fixture/systemd/aside.$masked" "$fixture/systemd/lunarwing-study-notes-sync.$masked"
done

# A listing the installer cannot read back must not be taken for an empty
# table. bash keeps a large here-string in a temporary file; a file size limit
# stands in for a temporary directory that cannot hold it.
{
  printf '%s\n' "$job_before"
  for ((filler = 0; filler < 3000; filler++)); do
    printf '7 3 * * * /usr/bin/true filler-job-%04d-with-padding-to-pass-the-pipe-size\n' "$filler"
  done
  printf '%s\n' '# BEGIN lunarwing-study-notes-sync managed block' "$managed_entry" \
    '# END lunarwing-study-notes-sync managed block' "$job_after"
} >"$FCRONTAB_STATE"
cron_lines_before="$(wc -l <"$FCRONTAB_STATE")"
installs_before="$(wc -l <"$FCRONTAB_INSTALLS")"
installer_rc=0
installer_output="$(
  (
    ulimit -f 32
    _install_knowledge_source_cron "$fixture/refresh-admin/current/lunarwing-mt-admin.sh"
  ) 2>&1
)" || installer_rc=$?
[[ "$(wc -l <"$FCRONTAB_STATE")" -eq "$cron_lines_before" ]] && other_jobs_kept || {
  printf 'a listing that could not be read back cost root its other jobs (rc=%s, %s lines left): %s\n' \
    "$installer_rc" "$(wc -l <"$FCRONTAB_STATE")" "$installer_output" >&2
  exit 1
}
[[ "$installer_rc" -ne 0 && "$(wc -l <"$FCRONTAB_INSTALLS")" -eq "$installs_before" ]] || {
  printf 'a listing that could not be read back was followed by an install (rc=%s): %s\n' \
    "$installer_rc" "$installer_output" >&2
  exit 1
}
write_managed_block "$managed_entry"

# Review of the fix (R2, R3, R6), on an admin root of its own so the release
# count of the root above stays where the older cases expect it.
(
  export LUNARWING_MT_KNOWLEDGE_SYNC_ADMIN_ROOT="$fixture/late-admin"
  late_old="$fixture/late-admin/releases/20260101000000-1"
  mkdir -p "$late_old/mt-admin-lib"
  printf 'commit=deadbeef\nsource=/old/checkout\nstaged_at=2026-01-01T00:00:00Z\n' >"$late_old/STAGED-FROM"
  printf '#!/usr/bin/env bash\n' >"$late_old/lunarwing-mt-admin.sh"
  chmod +x "$late_old/lunarwing-mt-admin.sh"
  ln -s -- "$late_old" "$fixture/late-admin/current"
  late_entry="17 * * * * $fixture/late-admin/current/lunarwing-mt-admin.sh sync-knowledge-sources"
  write_managed_block "$late_entry"

  # The staging delivers, then the crontab install fails: the operator is told
  # that the copy was replaced, not that everything was left as it was.
  late_output="$(FCRONTAB_FAIL_INSTALL=1 refresh_deployed_knowledge_sync_scheduler 2>&1)"
  [[ "$(readlink -- "$fixture/late-admin/current")" != "$late_old" \
    && "$late_output" == *"was restaged, but the schedule install did not finish"* \
    && "$late_output" != *"left as they were"* && "$late_output" != *"restaged from"* ]] || {
    printf 'a failure after the staging was reported wrongly: %s\n' "$late_output" >&2
    exit 1
  }

  # A stale temporary link of this pid, left by a killed run, must not be what
  # gets published: it points at the old release.
  ln -sfn -- "$late_old" "$fixture/late-admin/current"
  ln -s -- "$late_old" "$fixture/late-admin/.current.$$"
  late_entrypoint="$( ( install_knowledge_sync_admin_snapshot ) 2>/dev/null )"
  [[ "$late_entrypoint" == "$fixture/late-admin/current/lunarwing-mt-admin.sh" \
    && "$(readlink -- "$fixture/late-admin/current")" != "$late_old" \
    && ! -e "$late_old/$(basename -- "$(readlink -- "$fixture/late-admin/current")")" ]] || {
    printf 'a stale temporary link was published instead of the new release: %s\n' \
      "$(readlink -- "$fixture/late-admin/current")" >&2
    exit 1
  }

  # A prune that cannot remove an old release warns and must not fail the
  # install: the warning goes to stderr, not into the captured entrypoint.
  for _ in $(seq 1 $((KNOWLEDGE_SYNC_SNAPSHOT_KEEP + 2))); do
    install_knowledge_sync_admin_snapshot >/dev/null
  done
  rm() {
    [[ "$*" != *"/late-admin/releases/"* ]] || return 1
    command rm "$@"
  }
  mkdir -p "$fixture/late-admin/releases/20250101000000-1" "$fixture/late-admin/releases/20250101000000-2"
  prune_rc=0
  prune_output="$( ( set -e; ensure_knowledge_source_scheduler ) 2>&1 )" || prune_rc=$?
  unset -f rm
  [[ "$prune_rc" -eq 0 && "$prune_output" == *"could not prune old knowledge sync snapshot"* \
    && "$prune_output" == *"scheduled with fcrontab"* ]] || {
    printf 'a prune warning failed the scheduler install (rc=%s): %s\n' "$prune_rc" "$prune_output" >&2
    exit 1
  }
)

# Review AEG-5: the probe's other answers. Each unit file is needed on systemd,
# and every state the probe cannot read counts as "not live", so an upgrade only
# refreshes files there.
probe_not_live() {  # <what the case models>
  if knowledge_sync_scheduler_live; then
    printf 'the schedule was counted as live with %s\n' "$1" >&2
    exit 1
  fi
}
(
  INIT_SYSTEM=systemd
  knowledge_sync_scheduler_live || {
    printf 'an enabled timer with both unit files was not counted as live\n' >&2
    exit 1
  }
  for unit in timer service; do
    mv "$fixture/systemd/lunarwing-study-notes-sync.$unit" "$fixture/systemd/aside.$unit"
    probe_not_live "the $unit unit file missing"
    mv "$fixture/systemd/aside.$unit" "$fixture/systemd/lunarwing-study-notes-sync.$unit"
  done
  INIT_SYSTEM=launchd
  probe_not_live 'an init system it does not schedule on'
  INIT_SYSTEM=''
  detect_init_system() { return 1; }
  probe_not_live 'an init system that cannot be detected'
)
(
  mkdir -p "$fixture/no-cron-bin"
  # shellcheck disable=SC2034  # Consumed by sourced mt-admin functions.
  INIT_SYSTEM=openrc
  # shellcheck disable=SC2123  # The case is a search path without a cron command.
  PATH="$fixture/no-cron-bin"
  probe_not_live 'no cron command on OpenRC'
)

# The cases below need a host whose schedule is not live again.
sed '/^# BEGIN lunarwing-study-notes-sync managed block$/,/^# END lunarwing-study-notes-sync managed block$/d' \
  "$FCRONTAB_STATE" >"$FCRONTAB_STATE.tmp"
mv "$FCRONTAB_STATE.tmp" "$FCRONTAB_STATE"

# Review ADV-1: a restage that could not replace the release must warn instead
# of reporting delivery. Stubbing the installer models a read-only releases
# directory whose publish silently left the old copy in place.
ln -sfn -- "$old_release" "$fixture/refresh-admin/current"
install_knowledge_sync_admin_snapshot() { return 0; }
refresh_output="$(refresh_deployed_knowledge_sync_scheduler)"
[[ "$refresh_output" == *"still names 'deadbeef' after restaging"* ]] || {
  printf 'a restage that changed nothing was not reported: %s\n' "$refresh_output" >&2
  exit 1
}
[[ "$refresh_output" != *"restaged from"* ]] || {
  printf 'a restage that changed nothing was reported as delivered: %s\n' "$refresh_output" >&2
  exit 1
}

# Review AEG-4: the same must hold for a checkout without git metadata, where
# there is no commit to compare. The link under current decides.
knowledge_sync_checkout_commit() { :; }
refresh_output="$(refresh_deployed_knowledge_sync_scheduler)"
[[ "$refresh_output" == *"was not replaced"* && "$refresh_output" != *"restaged from"* ]] || {
  printf 'a restage that changed nothing was reported as delivered without git metadata: %s\n' "$refresh_output" >&2
  exit 1
}

printf 'Knowledge sync scheduler tests passed\n'
