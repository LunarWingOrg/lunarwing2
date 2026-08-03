#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
unset LUNARWING_MT_HEALTH_INTERVAL_MIN
# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"

failures=0

assert_eq() {
  local label="$1" expected="$2" actual="$3"
  if [[ "$actual" == "$expected" ]]; then
    printf 'PASS: %s\n' "$label"
  else
    printf 'FAIL: %s (expected %s, got %s)\n' "$label" "$expected" "$actual"
    failures=$((failures + 1))
  fi
}

INIT_SYSTEM="openrc"
HEALTH_INTERVAL_MIN=""
ensure_init_system
assert_eq "OpenRC default interval" "5" "$HEALTH_INTERVAL_MIN"

FCRON_TAB=""
fcrontab() {
  if [[ "${1:-}" == "-l" ]]; then
    printf '%s' "$FCRON_TAB"
  else
    FCRON_TAB="$(<"$1")"
  fi
}
_ensure_cron_runlevel() { :; }

# Override consumed by sourced health scheduler.
# shellcheck disable=SC2034
HEALTH_LAUNCHER="/usr/local/sbin/lunarwing-mt-health"
_install_health_cron >/dev/null
if grep -qF '*/5 * * * * /usr/local/sbin/lunarwing-mt-health' <<<"$FCRON_TAB"; then
  printf 'PASS: OpenRC fcrontab uses */5\n'
else
  printf 'FAIL: OpenRC fcrontab does not use */5\n'
  failures=$((failures + 1))
fi

# Override consumed by sourced init detection.
# shellcheck disable=SC2034
INIT_SYSTEM="systemd"
HEALTH_INTERVAL_MIN=""
ensure_init_system
assert_eq "systemd default interval remains unchanged" "15" "$HEALTH_INTERVAL_MIN"

override_interval="$(
  LUNARWING_MT_HEALTH_INTERVAL_MIN=7 bash -c '
    source "$1"
    INIT_SYSTEM="openrc"
    ensure_init_system
    printf "%s" "$HEALTH_INTERVAL_MIN"
  ' _ "$MT_ADMIN"
)"
assert_eq "explicit interval overrides OpenRC default" "7" "$override_interval"

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all health interval tests passed\n'
