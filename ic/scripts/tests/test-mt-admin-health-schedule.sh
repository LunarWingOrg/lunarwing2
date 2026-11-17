#!/usr/bin/env bash
# WI#740: the health pipeline's schedule installers replace files root's
# scheduler runs — root's whole crontab, and both unit files of the timer. A
# failure must therefore leave the previous schedule exactly as it was, and a
# managed block that cannot be delimited must never be rebuilt around, because
# that would take every later job of root's with it.
set -euo pipefail

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$TEST_DIR/../lunarwing-mt-admin.sh"

fixture="$(mktemp -d)"
cleanup() { rm -rf -- "$fixture"; }
trap cleanup EXIT

failures=0
pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1" >&2; failures=$((failures + 1)); }

assert_eq() {
  local label="$1" expected="$2" actual="$3"
  if [[ "$actual" == "$expected" ]]; then pass "$label"; else fail "$label (expected '$expected', got '$actual')"; fi
}

assert_contains() {
  local label="$1" text="$2" needle="$3"
  if [[ "$text" == *"$needle"* ]]; then pass "$label"; else fail "$label (missing '$needle' in: $text)"; fi
}

assert_not_contains() {
  local label="$1" text="$2" needle="$3"
  if [[ "$text" != *"$needle"* ]]; then pass "$label"; else fail "$label (unexpected '$needle' in: $text)"; fi
}

assert_same_file() {
  local label="$1" expected="$2" actual="$3"
  if cmp -s -- "$expected" "$actual"; then pass "$label"; else fail "$label (files differ: $(diff -u -- "$expected" "$actual" || true))"; fi
}

# ── OpenRC: root's crontab ───────────────────────────────────────────────────

CRONTAB_STATE="$fixture/crontab"
CRONTAB_INSTALLS="$fixture/crontab.installs"
: >"$CRONTAB_INSTALLS"

fcrontab() {
  if [[ "${1:-}" == "-l" ]]; then
    [[ -e "$CRONTAB_STATE" ]] || { printf 'user root has no fcrontab.\n' >&2; return 0; }
    command cat -- "$CRONTAB_STATE"
    [[ -z "${FCRONTAB_PARTIAL_READ:-}" ]] || return 1
    return 0
  fi
  printf 'install\n' >>"$CRONTAB_INSTALLS"
  [[ -z "${FCRONTAB_FAIL_INSTALL:-}" ]] || return 1
  command cp -- "$1" "$CRONTAB_STATE"
}

# The second cron tool, with a table of its own. The installer picks fcrontab
# when both exist, but removal looks at every tool that exists, so this
# stand-in is defined before any case runs: without it a removal case would
# reach the real crontab of a host that has one. Its table is absent (`-l`
# exits 1) until a case writes it.
PLAIN_CRONTAB_STATE="$fixture/plain-crontab"
crontab() {
  if [[ "${1:-}" == "-l" ]]; then
    [[ -e "$PLAIN_CRONTAB_STATE" ]] || { printf 'no crontab for root\n' >&2; return 1; }
    command cat -- "$PLAIN_CRONTAB_STATE"
    return 0
  fi
  command cp -- "$1" "$PLAIN_CRONTAB_STATE"
}
_ensure_cron_runlevel() { :; }

# shellcheck disable=SC2034  # consumed by the sourced installers
HEALTH_INTERVAL_MIN=15
# shellcheck disable=SC2034  # consumed by the sourced installers
HEALTH_LAUNCHER="/usr/local/sbin/lunarwing-mt-health"
begin='# BEGIN lunarwing-mt-health managed block'
end='# END lunarwing-mt-health managed block'
sched='*/15 * * * * /usr/local/sbin/lunarwing-mt-health'
install_count() { wc -l <"$CRONTAB_INSTALLS"; }

# A job line holding a '%' and a backslash: the table is a file, so neither is
# expanded, and both must survive byte for byte.
# shellcheck disable=SC2016  # the job line is cron text, not a shell expansion
job_before='5 4 * * * /usr/bin/true "today is $(date +\%F)" job-before\the-block'
job_after='6 4 * * * /usr/bin/true job-after-the-block'

printf '%s\n' "$job_before" "$job_after" >"$CRONTAB_STATE"
_install_health_cron >/dev/null
{
  printf '%s\n' "$job_before" "$job_after" "$begin" "$sched" "$end"
} >"$fixture/expected"
assert_same_file "first install keeps both jobs byte for byte and appends the block" \
  "$fixture/expected" "$CRONTAB_STATE"

# A stale block in the middle is replaced, and the jobs around it are kept in
# order: the old entry is gone and only one block remains.
printf '%s\n' "$job_before" "$begin" '*/9 * * * * /usr/local/sbin/lunarwing-mt-health' \
  "$end" "$job_after" >"$CRONTAB_STATE"
_install_health_cron >/dev/null
printf '%s\n' "$job_before" "$job_after" "$begin" "$sched" "$end" >"$fixture/expected"
assert_same_file "reinstall replaces only the managed block" "$fixture/expected" "$CRONTAB_STATE"
assert_eq "reinstall leaves one BEGIN line" "1" "$(grep -cFx -- "$begin" "$CRONTAB_STATE")"
assert_eq "reinstall leaves one END line" "1" "$(grep -cFx -- "$end" "$CRONTAB_STATE")"

# A host without a crontab for root still gets its schedule (`-l` exits 1).
rm -f -- "$CRONTAB_STATE"
_install_health_cron >/dev/null
printf '%s\n' "$begin" "$sched" "$end" >"$fixture/expected"
assert_same_file "a root without a crontab gets the block" "$fixture/expected" "$CRONTAB_STATE"

# ── A step before the install fails: nothing is replaced ────────────────────
printf '%s\n' "$job_before" "$begin" "$sched" "$end" "$job_after" >"$CRONTAB_STATE"
command cp -- "$CRONTAB_STATE" "$fixture/cron.before"

run_blocked_install() {  # <function override>
  local status=0
  BLOCKED_INSTALL_OUTPUT="$(
    (
      eval "$1"
      _install_health_cron
    ) 2>&1
  )" || status=$?
  BLOCKED_INSTALL_STATUS="$status"
}

installs_before="$(install_count)"
run_blocked_install 'mktemp() { return 1; }'
assert_eq "no temporary file: the installer fails" "1" "$BLOCKED_INSTALL_STATUS"
assert_same_file "no temporary file: the crontab is unchanged" "$fixture/cron.before" "$CRONTAB_STATE"
assert_eq "no temporary file: no install was attempted" "$installs_before" "$(install_count)"
assert_contains "no temporary file: the reason is printed" "$BLOCKED_INSTALL_OUTPUT" \
  "could not create a temporary file"
assert_not_contains "no temporary file: not reported as scheduled" "$BLOCKED_INSTALL_OUTPUT" "scheduled health pipeline"

# A listing that fails after printing part of the table must not be taken for
# an empty one: that would install a crontab holding only the block, and every
# job root had would be gone.
command cat -- "$CRONTAB_STATE" >"$fixture/cron.before"
installs_before="$(install_count)"
status=0
output="$(FCRONTAB_PARTIAL_READ=1 _install_health_cron 2>&1)" || status=$?
assert_eq "partially read table: the installer fails" "1" "$status"
assert_same_file "partially read table: the crontab is unchanged" "$fixture/cron.before" "$CRONTAB_STATE"
assert_eq "partially read table: no install was attempted" "$installs_before" "$(install_count)"
assert_contains "partially read table: the reason is printed" "$output" "after printing part of the table"
assert_not_contains "partially read table: not reported as scheduled" "$output" "scheduled health pipeline"

# A write into the temporary file that fails (a full disk): the table built
# from root's jobs is never handed to the scheduler.
installs_before="$(install_count)"
# shellcheck disable=SC2016  # the override is shell text evaluated in the subshell
run_blocked_install 'printf() {
  [[ "${1:-}" != "%s\n" || "${2:-}" != "$sched" ]] || return 1
  builtin printf "$@"
}'
assert_eq "failed write: the installer fails" "1" "$BLOCKED_INSTALL_STATUS"
assert_same_file "failed write: the crontab is unchanged" "$fixture/cron.before" "$CRONTAB_STATE"
assert_eq "failed write: no install was attempted" "$installs_before" "$(install_count)"
assert_contains "failed write: the reason is printed" "$BLOCKED_INSTALL_OUTPUT" "could not rebuild root's crontab"

# The scheduler refusing the table is reported, and the old table stays.
installs_before="$(install_count)"
run_blocked_install 'FCRONTAB_FAIL_INSTALL=1'
assert_eq "refused install: the installer fails" "1" "$BLOCKED_INSTALL_STATUS"
assert_same_file "refused install: the crontab is unchanged" "$fixture/cron.before" "$CRONTAB_STATE"
assert_eq "refused install: the install was attempted once" "$((installs_before + 1))" "$(install_count)"
assert_contains "refused install: the reason names the scheduler" "$BLOCKED_INSTALL_OUTPUT" \
  "could not hand the new root crontab to fcrontab"
assert_not_contains "refused install: not reported as scheduled" "$BLOCKED_INSTALL_OUTPUT" "scheduled health pipeline"

# A table too large for bash to pass to its own loop through a pipe: bash keeps
# a here-string above its pipe size (64 KiB) in a temporary file, and when that
# file cannot be written the loop that copies root's jobs does not run at all.
# Such a table must not be taken for an empty one — the block alone would then
# replace every job root has. The file size limit stands in for a temporary
# directory without room for the table; it holds for the subshell only.
write_large_table() {  # <file> <line>... : root's jobs around many filler jobs
  local file="$1" i
  shift
  {
    printf '%s\n' "$job_before"
    for ((i = 0; i < 1500; i++)); do
      printf '7 3 * * * /usr/bin/true filler-job-%04d-with-padding-to-pass-the-pipe-size\n' "$i"
    done
    printf '%s\n' "$@"
  } >"$file"
}
same_bytes() { if cmp -s -- "$1" "$2"; then printf 'same'; else printf 'differs'; fi; }
write_large_table "$CRONTAB_STATE" "$begin" '*/9 * * * * /usr/local/sbin/lunarwing-mt-health' "$end" "$job_after"
command cp -- "$CRONTAB_STATE" "$fixture/cron.before"
large_lines="$(wc -l <"$CRONTAB_STATE")"
assert_eq "large table: the fixture is larger than bash's pipe size" "1" \
  "$(( $(wc -c <"$CRONTAB_STATE") > 65536 ))"
installs_before="$(install_count)"
run_blocked_install 'ulimit -f 32'
assert_eq "large table, no room to buffer it: the installer fails" "1" "$BLOCKED_INSTALL_STATUS"
assert_eq "large table, no room to buffer it: the crontab is unchanged" "same" \
  "$(same_bytes "$fixture/cron.before" "$CRONTAB_STATE")"
assert_eq "large table, no room to buffer it: every line is still there" "$large_lines" \
  "$(wc -l <"$CRONTAB_STATE")"
assert_eq "large table, no room to buffer it: no install was attempted" "$installs_before" "$(install_count)"
assert_contains "large table, no room to buffer it: the reason is printed" "$BLOCKED_INSTALL_OUTPUT" \
  "could not read the whole listing of root's crontab"
assert_not_contains "large table, no room to buffer it: not reported as scheduled" "$BLOCKED_INSTALL_OUTPUT" \
  "scheduled health pipeline"

# The same table with room for it is rebuilt completely, so the refusal above
# is about the missing room and not about the size.
_install_health_cron >/dev/null
write_large_table "$fixture/expected" "$job_after" "$begin" "$sched" "$end"
assert_eq "large table with room: every job is kept and only the block is replaced" "same" \
  "$(same_bytes "$fixture/expected" "$CRONTAB_STATE")"
assert_eq "large table with room: one install" "$((installs_before + 1))" "$(install_count)"

# A BEGIN line with no later END line: the block cannot be delimited, so the
# table is not rebuilt at all — every other job is left where it was.
printf '%s\n' "$job_before" "$begin" "$sched" "$job_after" >"$CRONTAB_STATE"
command cp -- "$CRONTAB_STATE" "$fixture/cron.before"
installs_before="$(install_count)"
run_blocked_install ':'
assert_eq "unterminated block: the installer fails" "1" "$BLOCKED_INSTALL_STATUS"
assert_same_file "unterminated block: the crontab is unchanged" "$fixture/cron.before" "$CRONTAB_STATE"
assert_eq "unterminated block: no install was attempted" "$installs_before" "$(install_count)"
assert_contains "unterminated block: the reason names the missing END line" "$BLOCKED_INSTALL_OUTPUT" \
  "without a later line that reads exactly"
assert_contains "unterminated block: the reason names the repair" "$BLOCKED_INSTALL_OUTPUT" "fcrontab -e"
assert_eq "unterminated block: the job after it survives" "1" "$(grep -cFx -- "$job_after" "$CRONTAB_STATE")"

# A second BEGIN line before the first END line cannot be delimited either.
printf '%s\n' "$job_before" "$begin" "$sched" "$begin" "$end" "$job_after" >"$CRONTAB_STATE"
command cp -- "$CRONTAB_STATE" "$fixture/cron.before"
run_blocked_install ':'
assert_eq "nested block: the installer fails" "1" "$BLOCKED_INSTALL_STATUS"
assert_same_file "nested block: the crontab is unchanged" "$fixture/cron.before" "$CRONTAB_STATE"
assert_eq "nested block: the job after it survives" "1" "$(grep -cFx -- "$job_after" "$CRONTAB_STATE")"

# An END line with no BEGIN line above it is one of root's own lines.
printf '%s\n' "$job_before" "$end" "$job_after" >"$CRONTAB_STATE"
_install_health_cron >/dev/null
printf '%s\n' "$job_before" "$end" "$job_after" "$begin" "$sched" "$end" >"$fixture/expected"
assert_same_file "a stray END line is kept as root's own" "$fixture/expected" "$CRONTAB_STATE"

# ── OpenRC: retiring the block goes through the same checked path ───────────
# shellcheck disable=SC2034  # read by the sourced installers
ensure_init_system() { INIT_SYSTEM=openrc; }
HEALTH_LAUNCHER="$fixture/health-launcher"
printf '#!/bin/bash\n' >"$HEALTH_LAUNCHER"
chmod 0755 "$HEALTH_LAUNCHER"
printf '%s\n' "$job_before" "$begin" "$sched" "$end" "$job_after" >"$CRONTAB_STATE"
remove_health_pipeline >/dev/null
printf '%s\n' "$job_before" "$job_after" >"$fixture/expected"
assert_same_file "removal drops only the managed block" "$fixture/expected" "$CRONTAB_STATE"
assert_eq "removal retires the launcher" "0" "$([[ -e "$HEALTH_LAUNCHER" ]] && printf 1 || printf 0)"

# An unterminated block cannot be removed safely, so it is left alone — with
# the launcher, because that block still runs it every interval. The tenant is
# already removed at this point, so this is a warning, not a failure.
printf '#!/bin/bash\n' >"$HEALTH_LAUNCHER"
chmod 0755 "$HEALTH_LAUNCHER"
printf '%s\n' "$job_before" "$begin" "$sched" "$job_after" >"$CRONTAB_STATE"
command cp -- "$CRONTAB_STATE" "$fixture/cron.before"
status=0
output="$(remove_health_pipeline 2>&1)" || status=$?
assert_eq "unterminated removal: reported as success" "0" "$status"
assert_same_file "unterminated removal: the crontab is unchanged" "$fixture/cron.before" "$CRONTAB_STATE"
assert_eq "unterminated removal: the launcher is kept" "1" \
  "$([[ -e "$HEALTH_LAUNCHER" ]] && printf 1 || printf 0)"
assert_contains "unterminated removal: the reason is printed" "$output" \
  "could not remove the health pipeline block"

# Both cron tools exist and the block is in crontab's table: a host that was
# scheduled while only crontab existed and got fcron later. The installer would
# pick fcrontab today, but the block must be removed from the table it is in —
# retiring the launcher while crontab's table still runs it leaves root with a
# job for a file that is gone.
launcher_exists() { if [[ -e "$HEALTH_LAUNCHER" ]]; then printf 1; else printf 0; fi; }
write_launcher() {
  printf '#!/bin/bash\n' >"$HEALTH_LAUNCHER"
  chmod 0755 "$HEALTH_LAUNCHER"
}
write_launcher
printf '%s\n' "$job_before" >"$CRONTAB_STATE"
printf '%s\n' "$job_after" "$begin" "$sched" "$end" >"$PLAIN_CRONTAB_STATE"
remove_health_pipeline >/dev/null
printf '%s\n' "$job_after" >"$fixture/expected"
assert_same_file "both tools, block in crontab's table: the block is removed there" \
  "$fixture/expected" "$PLAIN_CRONTAB_STATE"
printf '%s\n' "$job_before" >"$fixture/expected"
assert_same_file "both tools, block in crontab's table: fcrontab's jobs are kept" \
  "$fixture/expected" "$CRONTAB_STATE"
assert_eq "both tools, block in crontab's table: the launcher is retired" "0" "$(launcher_exists)"

# The block is in both tables and only fcrontab's can be removed: crontab's
# table still runs the launcher, so the launcher stays, and the warning names
# the tool whose table kept the block.
write_launcher
printf '%s\n' "$job_before" "$begin" "$sched" "$end" >"$CRONTAB_STATE"
printf '%s\n' "$begin" "$sched" "$job_after" >"$PLAIN_CRONTAB_STATE"
command cp -- "$PLAIN_CRONTAB_STATE" "$fixture/cron.before"
status=0
output="$(remove_health_pipeline 2>&1)" || status=$?
assert_eq "block left in crontab's table: reported as success" "0" "$status"
printf '%s\n' "$job_before" >"$fixture/expected"
assert_same_file "block left in crontab's table: fcrontab's block is removed" "$fixture/expected" "$CRONTAB_STATE"
assert_same_file "block left in crontab's table: crontab's table is unchanged" \
  "$fixture/cron.before" "$PLAIN_CRONTAB_STATE"
assert_eq "block left in crontab's table: the launcher is kept" "1" "$(launcher_exists)"
assert_contains "block left in crontab's table: the warning names the tool" "$output" \
  "could not remove the health pipeline block from root's crontab table"
assert_not_contains "block left in crontab's table: not reported as retired" "$output" \
  "retired host-global health pipeline schedule"

# The other way round: fcrontab's block cannot be removed. crontab's table is
# still cleaned — one tool's refusal does not end the removal — and the
# launcher stays for the block that is left.
write_launcher
printf '%s\n' "$job_before" "$begin" "$sched" >"$CRONTAB_STATE"
command cp -- "$CRONTAB_STATE" "$fixture/cron.before"
printf '%s\n' "$begin" "$sched" "$end" "$job_after" >"$PLAIN_CRONTAB_STATE"
status=0
output="$(remove_health_pipeline 2>&1)" || status=$?
assert_eq "block left in fcrontab's table: reported as success" "0" "$status"
assert_same_file "block left in fcrontab's table: fcrontab's table is unchanged" \
  "$fixture/cron.before" "$CRONTAB_STATE"
printf '%s\n' "$job_after" >"$fixture/expected"
assert_same_file "block left in fcrontab's table: crontab's block is removed" \
  "$fixture/expected" "$PLAIN_CRONTAB_STATE"
assert_eq "block left in fcrontab's table: the launcher is kept" "1" "$(launcher_exists)"
assert_contains "block left in fcrontab's table: the warning names the tool" "$output" \
  "could not remove the health pipeline block from root's fcrontab table"

# ── OpenRC: what ensure_health_pipeline reports after the installer ─────────
# The crontab is installed but the cron service cannot be enabled and started:
# the installer says the schedule is installed, so the line that
# ensure_health_pipeline adds must not say that it is not. The installer's
# status tells the two results apart (1: not installed, 2: installed, and the
# cron service is in doubt).
printf '%s\n' "$job_before" >"$CRONTAB_STATE"
run_blocked_install '_ensure_cron_runlevel() { return 1; }'
assert_eq "cron service not enabled: the installer's status is not the refusal's" "2" "$BLOCKED_INSTALL_STATUS"
printf '%s\n' "$job_before" "$begin" "*/15 * * * * $HEALTH_LAUNCHER" "$end" >"$fixture/expected"
assert_same_file "cron service not enabled: the crontab holds the block" "$fixture/expected" "$CRONTAB_STATE"

run_health_pipeline() {  # <function override> : ensure_health_pipeline inside the fixture
  local status=0
  # shellcheck disable=SC2034,SC2329  # paths and stand-ins are consumed by the sourced ensure_health_pipeline
  PIPELINE_OUTPUT="$(
    (
      HEALTH_SRC_DIR="$fixture/pipeline/source"
      HEALTH_LIB_DIR="$fixture/pipeline/lib"
      HEALTH_BASE_DIR="$fixture/pipeline/base"
      HEALTH_ENV_FILE="$fixture/pipeline/config/health.env"
      PORTS_REGISTRY="$fixture/pipeline/ports.json"
      OPENRC_CONFD_DIR="$fixture/pipeline/conf.d"
      HEALTH_GOTIFY_URL=""
      HEALTH_GOTIFY_TOKEN=""
      mkdir -p -- "$HEALTH_SRC_DIR"
      # Installing the library is test-mt-admin-health-library-security.sh's
      # subject; this case reads only what is said about the schedule.
      sync_health_pipeline_library() { :; }
      ensure_container_runtime() { return 1; }
      eval "$1"
      ensure_health_pipeline
    ) 2>&1
  )" || status=$?
  PIPELINE_STATUS="$status"
}

printf '%s\n' "$job_before" >"$CRONTAB_STATE"
run_health_pipeline '_ensure_cron_runlevel() { return 1; }'
assert_eq "cron service not enabled: the pipeline install goes on" "0" "$PIPELINE_STATUS"
assert_eq "cron service not enabled: the pipeline's crontab holds the block" "1" \
  "$(grep -cFx -- "$begin" "$CRONTAB_STATE")"
assert_contains "cron service not enabled: the installer says the schedule is installed" "$PIPELINE_OUTPUT" \
  "the health pipeline schedule is installed, but the cron service could not be enabled and started"
assert_contains "cron service not enabled: the closing line says it may not be running" "$PIPELINE_OUTPUT" \
  "files and schedule are installed, but the schedule may NOT be running"
assert_not_contains "cron service not enabled: the closing line does not call the schedule missing" \
  "$PIPELINE_OUTPUT" "but the schedule is NOT;"
assert_not_contains "cron service not enabled: not reported as ready" "$PIPELINE_OUTPUT" "health pipeline ready"

# A refused schedule (an unterminated block) keeps the other closing line:
# there the schedule is missing.
printf '%s\n' "$job_before" "$begin" "$sched" >"$CRONTAB_STATE"
run_health_pipeline ':'
assert_eq "schedule refused: the pipeline install goes on" "0" "$PIPELINE_STATUS"
assert_contains "schedule refused: the closing line says the schedule is not installed" "$PIPELINE_OUTPUT" \
  "files are installed, but the schedule is NOT;"
assert_not_contains "schedule refused: the closing line does not call the schedule installed" \
  "$PIPELINE_OUTPUT" "files and schedule are installed"

# ── systemd: the unit pair ──────────────────────────────────────────────────

sd="$fixture/systemd"
export LUNARWING_MT_SYSTEMD_UNIT_DIR="$sd"
mkdir -p -- "$sd"
SYSTEMCTL_LOG="$fixture/systemctl.log"
: >"$SYSTEMCTL_LOG"
systemctl() { printf '%s\n' "$*" >>"$SYSTEMCTL_LOG"; }
service="$sd/lunarwing-mt-health.service"
timer="$sd/lunarwing-mt-health.timer"

LUNARWING_MT_SYSTEMD_UNIT_DIR="$sd" _install_health_systemd_timer >/dev/null
assert_eq "the service is written" "1" "$([[ -f "$service" ]] && printf 1 || printf 0)"
assert_eq "the timer is written" "1" "$([[ -f "$timer" ]] && printf 1 || printf 0)"
assert_eq "the timer reloads systemd" "1" "$(grep -cFx 'daemon-reload' "$SYSTEMCTL_LOG")"
assert_eq "the timer is enabled" "1" "$(grep -cFx 'enable --now lunarwing-mt-health.timer' "$SYSTEMCTL_LOG")"
assert_eq "the timer cadence follows the interval" "OnCalendar=*:0/15" "$(grep '^OnCalendar=' "$timer")"
command cp -- "$service" "$fixture/service.before"
command cp -- "$timer" "$fixture/timer.before"

# A write that fails half-way (a full disk) leaves both units as they were and
# reloads nothing: the service is staged first, so failing the timer's write is
# the case that would otherwise install half a pair.
: >"$SYSTEMCTL_LOG"
run_blocked_timer() {  # <function override>
  local status=0
  BLOCKED_INSTALL_OUTPUT="$(
    (
      eval "$1"
      _install_health_systemd_timer
    ) 2>&1
  )" || status=$?
  BLOCKED_INSTALL_STATUS="$status"
}
# shellcheck disable=SC2016  # the override is shell text evaluated in the subshell
run_blocked_timer 'printf() {
  [[ "${1:-}" != "%s" || "${2:-}" != *"Unit=lunarwing-mt-health.service"* ]] || return 1
  builtin printf "$@"
}'
assert_eq "failed unit write: the installer fails" "1" "$BLOCKED_INSTALL_STATUS"
assert_same_file "failed unit write: the service is as before" "$fixture/service.before" "$service"
assert_same_file "failed unit write: the timer is as before" "$fixture/timer.before" "$timer"
assert_eq "failed unit write: nothing was reloaded or enabled" "0" "$(wc -l <"$SYSTEMCTL_LOG")"
assert_eq "failed unit write: no temporary unit file is left" "0" \
  "$(find "$sd" -name 'lunarwing-mt-health.*.*' -print -quit | wc -l)"
assert_contains "failed unit write: the reason names the file" "$BLOCKED_INSTALL_OUTPUT" \
  "could not write $timer"
assert_not_contains "failed unit write: not reported as scheduled" "$BLOCKED_INSTALL_OUTPUT" \
  "scheduled health pipeline"

# A temporary file that cannot be created beside the timer: same guarantee.
: >"$SYSTEMCTL_LOG"
run_blocked_timer 'mktemp() {
  [[ "${1:-}" != *.timer.* ]] || return 1
  command mktemp "$@"
}'
assert_eq "no timer temporary file: the installer fails" "1" "$BLOCKED_INSTALL_STATUS"
assert_same_file "no timer temporary file: both units are as before" "$fixture/service.before" "$service"
assert_eq "no timer temporary file: nothing was reloaded or enabled" "0" "$(wc -l <"$SYSTEMCTL_LOG")"

# A masked unit is a symbolic link; renaming over it would unmask it.
for masked in service timer; do
  command mv -- "$sd/lunarwing-mt-health.$masked" "$sd/aside.$masked"
  ln -s /dev/null "$sd/lunarwing-mt-health.$masked"
  : >"$SYSTEMCTL_LOG"
  status=0
  output="$(LUNARWING_MT_SYSTEMD_UNIT_DIR="$sd" _install_health_systemd_timer 2>&1)" || status=$?
  assert_eq "masked $masked: the installer fails" "1" "$status"
  assert_eq "masked $masked: the link is not replaced" "1" \
    "$([[ -L "$sd/lunarwing-mt-health.$masked" ]] && printf 1 || printf 0)"
  assert_eq "masked $masked: nothing was reloaded or enabled" "0" "$(wc -l <"$SYSTEMCTL_LOG")"
  assert_contains "masked $masked: the reason names the link" "$output" "symbolic link"
  assert_eq "masked $masked: no temporary file is left" "0" \
    "$(find "$sd" -name 'lunarwing-mt-health.*.*' -print -quit | wc -l)"
  rm -f -- "$sd/lunarwing-mt-health.$masked"
  command mv -- "$sd/aside.$masked" "$sd/lunarwing-mt-health.$masked"
done

# A daemon-reload that fails is no longer discarded: the units are in place but
# the host is told the timer is not armed.
status=0
output="$(
  (
    systemctl() { [[ "${1:-}" != daemon-reload ]] || return 1; }
    LUNARWING_MT_SYSTEMD_UNIT_DIR="$sd" _install_health_systemd_timer
  ) 2>&1
)" || status=$?
assert_eq "failed daemon-reload: the installer fails" "1" "$status"
assert_contains "failed daemon-reload: the reason is printed" "$output" "not reloaded"
assert_not_contains "failed daemon-reload: not reported as scheduled" "$output" "scheduled health pipeline"

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all health schedule tests passed\n'
