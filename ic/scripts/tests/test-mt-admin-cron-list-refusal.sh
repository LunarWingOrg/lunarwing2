#!/usr/bin/env bash
# WI#743: a failed cron listing is not an empty crontab. Run real production
# writers as root, with only cron commands and service boundaries replaced.
set -euo pipefail
[[ "$EUID" -eq 0 ]] || { printf 'requires root; CI shell-harnesses supplies it\n' >&2; exit 1; }
TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lunarwing-mt-admin.sh
source "$TEST_DIR/../lunarwing-mt-admin.sh"
fixture="$(mktemp -d)"
trap 'rm -rf -- "$fixture"' EXIT
failures=0
output=""
pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1" >&2; failures=$((failures + 1)); }
assert() { local label="$1"; shift; if "$@"; then pass "$label"; else fail "$label"; fi; }
assert_matches() {
  local label="$1" text="$2" pattern="$3"
  if [[ "$text" == *"$pattern"* ]]; then pass "$label"; else fail "$label (actual=<$text>)"; fi
}


cron_stub() {
  if [[ "${1:-}" == "-l" ]]; then
    case "$TOOL_BEHAVIOR" in
      fail-silent) return 1 ;;
      fail-message) printf 'spool temporarily locked\n' >&2; return 1 ;;
      partial) head -n 1 -- "$TOOL_TABLE"; printf 'read interrupted\n' >&2; return 1 ;;
      missing-fcron)
        printf "2026-11-17 06:10:00  INFO listing root\x27s fcrontab\n" >&2
        printf '2026-11-17 06:10:00  INFO user root has no fcrontab.\n' >&2
        return 0 ;;
      missing-cronie) printf 'no crontab for root\n' >&2; return 1 ;;
      missing-dcron) printf 'no crontab for root\n' >&2; return 0 ;;
      missing-busybox) printf "crontab: can't open 'root': No such file or directory\n" >&2; return 1 ;;
      existing) cat -- "$TOOL_TABLE"; return 0 ;;
      *) printf 'unknown test behavior\n' >&2; return 1 ;;
    esac
  fi
  printf '1\n' >>"$TOOL_INSTALLS"
  cp -- "$1" "$TOOL_TABLE"
}

select_tool() {
  if [[ "$1" == fcrontab ]]; then
    fcrontab() { cron_stub "$@"; }
    unset -f crontab 2>/dev/null || true
  else
    crontab() { cron_stub "$@"; }
    unset -f fcrontab 2>/dev/null || true
  fi
}
# shellcheck disable=SC2034  # consumed in sourced health scheduler
ensure_init_system() { INIT_SYSTEM=openrc; }
_ensure_cron_runlevel() { :; }
_require_knowledge_sync_entrypoint() { :; }

run_case() {
  local operation="$1" behavior="$2" command="$3"
  TOOL_TABLE="$fixture/$operation-$behavior.table"
  TOOL_INSTALLS="$fixture/$operation-$behavior.installs"
  TOOL_BEHAVIOR="$behavior"

  printf '%s\n' '0 3 * * * /usr/local/bin/nightly-backup' \
    '15 4 * * * /usr/local/bin/rotate-logs' \
    '30 5 * * * /usr/local/bin/watchdog' >"$TOOL_TABLE"
  cp -- "$TOOL_TABLE" "$TOOL_TABLE.before"
  : >"$TOOL_INSTALLS"
  # shellcheck disable=SC2034  # consumed in the sourced health installer
  HEALTH_INTERVAL_MIN=15
  HEALTH_LAUNCHER="$fixture/$operation-$behavior.launcher"
  printf '#!/bin/bash\n' >"$HEALTH_LAUNCHER"
  chmod 0755 "$HEALTH_LAUNCHER"
  select_tool "$command"
  output=""
  rc=0
  case "$operation" in
    health-install) output="$(_install_health_cron 2>&1)" || rc=$? ;;
    health-remove) output="$(remove_health_pipeline 2>&1)" || rc=$? ;;
    study-install) output="$(_install_knowledge_source_cron /safe/staged/mt-admin.sh 2>&1)" || rc=$? ;;
  esac

  if [[ "$operation" == health-remove ]]; then
    assert "$operation $command $behavior preserves root jobs" cmp -s "$TOOL_TABLE.before" "$TOOL_TABLE"
    assert "$operation $command $behavior keeps launcher" test -f "$HEALTH_LAUNCHER"
    assert "$operation $command $behavior returns established status" test "$rc" -eq 0
    assert_matches "$operation $command $behavior prints the refusal" "$output" 'could not remove the health pipeline block'
  else
    expected=1
    if [[ "$TOOL_BEHAVIOR" == missing* ]]; then expected=0; fi
    assert "$operation $command $behavior returns expected status" test "$rc" -eq "$expected"
  fi
  if [[ "$TOOL_BEHAVIOR" == missing* ]]; then
    assert "$operation $command $behavior positive response installs" test "$(wc -l <"$TOOL_INSTALLS")" -eq 1
  else
    assert "$operation $command $behavior no install on failure" test ! -s "$TOOL_INSTALLS"
  fi
  if [[ "$TOOL_BEHAVIOR" == missing* ]]; then
    # A positively missing table means there were no other jobs to keep: the
    # result is exactly the managed block.
    jobs_after="$(grep -c '^[0-9].*/usr/local/bin/' "$TOOL_TABLE" || true)"
    block_after="$(grep -cFx "# BEGIN lunarwing-mt-health managed block" "$TOOL_TABLE" || true)"
    assert "$operation $command $behavior writes only the block" test "$jobs_after:$block_after" = "0:1"
    return 0
  fi

  if [[ "$operation" != health-remove ]]; then
    assert "$operation $command $behavior not scheduled" test "$output" != *scheduled*
    assert "$operation $command $behavior preserves root jobs" cmp -s "$TOOL_TABLE.before" "$TOOL_TABLE"
  fi
  if [[ "$behavior" == fail-message ]]; then
    assert_matches "$behavior prints the tool message" "$output" 'spool temporarily locked'
  fi
}

for operation in health-install study-install; do
  for behavior in fail-silent fail-message; do
    run_case "$operation" "$behavior" fcrontab
  done
done
for behavior in fail-silent fail-message; do
  run_case health-remove "$behavior" fcrontab
done

# Half of the table must never become its complete replacement.
TOOL_TABLE="$fixture/partial.table"
TOOL_INSTALLS="$fixture/partial.installs"
TOOL_BEHAVIOR=partial
printf '%s\n' '0 3 * * * /usr/local/bin/nightly-backup' '15 4 * * * /usr/local/bin/rotate-logs' >"$TOOL_TABLE"
cp "$TOOL_TABLE" "$TOOL_TABLE.before"
: >"$TOOL_INSTALLS"
output=""
rc=0
output="$(_install_knowledge_source_cron /safe/staged/mt-admin.sh 2>&1)" || rc=$?

assert "partial Study Notes table preserved" cmp -s "$TOOL_TABLE.before" "$TOOL_TABLE"
assert "partial table: no install" test ! -s "$TOOL_INSTALLS"
assert "partial table: nonzero refusal" test "$rc" -ne 0
assert_matches "partial table: partial stderr explanation" "$output" 'after printing part of the table'

# Every supported tool positively reports a missing table differently.
for signature in cronie dcron busybox fcron; do
  run_case health-install "missing-$signature" "$([[ "$signature" == fcron ]] && printf 'fcrontab' || printf 'crontab')"
  last=$(wc -l <"$TOOL_INSTALLS")
  printf 'signature %s installs=%s\n' "$signature" "$last" >&2
done

# Signature unit checks enforce exact matching and no false positives.
for check in \
  'crontab|no crontab for root|accept' \
  "crontab|crontab: can't open 'root': No such file or directory|accept" \
  'fcrontab|user root has no fcrontab.|accept' \
  'crontab|no crontab for rootish|refuse' \
  'crontab||refuse' \
  'crontab|spool temporarily locked|refuse'; do
  IFS='|' read -r _ message expected <<<"$check"
  rc=0
  if _cron_listing_says_no_table "$message"; then actual=accept; else actual=refuse; fi
  assert "tool-signature $check behavior $expected (actual $actual)" test "$expected" = "$actual"
done

# The real fcron 3.3.1 output stamps both lines, as measured on 2026-11-17.
fcron_real=$'2026-11-17 06:10:00  INFO listing root\x27s fcrontab\n2026-11-17 06:10:00  INFO user root has no fcrontab.'
if _cron_listing_says_no_table "$fcron_real"; then pass 'real timestamped fcrontab output is a missing table'; else fail 'real timestamped fcrontab output is a missing table'; fi

if (( failures != 0 )); then
  printf 'RESULT: %s failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all cron list refusal tests passed\n'
