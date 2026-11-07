#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
unset LUNARWING_MT_HEALTH_INTERVAL_MIN
# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"

TEST_ROOT="$(mktemp -d)"
trap 'rm -rf "$TEST_ROOT"' EXIT
HEALTH_ENV_FILE="$TEST_ROOT/health.env"
SD_DIR="$TEST_ROOT/systemd"
mkdir -p "$SD_DIR"

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
assert_eq "OpenRC default interval" "15" "$HEALTH_INTERVAL_MIN"
assert_eq "OpenRC default interval source" "default" "$HEALTH_INTERVAL_SOURCE"

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
if grep -qF '*/15 * * * * /usr/local/sbin/lunarwing-mt-health' <<<"$FCRON_TAB"; then
  printf 'PASS: OpenRC fcrontab uses */15\n'
else
  printf 'FAIL: OpenRC fcrontab does not use */15\n'
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

# ── systemd timer rendering ───────────────────────────────────────────────────
# Mock systemctl (enable/daemon-reload no-ops) and redirect the unit dir into a
# scratch dir via LUNARWING_MT_SYSTEMD_UNIT_DIR so nothing touches the host.
systemctl() { :; }

# shellcheck disable=SC2034
INIT_SYSTEM="systemd"
HEALTH_INTERVAL_MIN=""
ensure_init_system
LUNARWING_MT_SYSTEMD_UNIT_DIR="$SD_DIR" _install_health_systemd_timer >/dev/null
assert_eq "systemd oneshot rendered" "1" "$([[ -f "$SD_DIR/lunarwing-mt-health.service" ]] && echo 1 || echo 0)"
assert_eq "systemd timer rendered" "1" "$([[ -f "$SD_DIR/lunarwing-mt-health.timer" ]] && echo 1 || echo 0)"
assert_eq "oneshot bounded (default 15min interval < 900s floor)" "TimeoutStartSec=900" \
  "$(grep '^TimeoutStartSec=' "$SD_DIR/lunarwing-mt-health.service")"
assert_eq "timer cadence follows interval" "OnCalendar=*:0/15" \
  "$(grep '^OnCalendar=' "$SD_DIR/lunarwing-mt-health.timer")"
assert_eq "timer replays missed runs" "Persistent=true" \
  "$(grep '^Persistent=' "$SD_DIR/lunarwing-mt-health.timer")"
assert_eq "timer targets the oneshot" "Unit=lunarwing-mt-health.service" \
  "$(grep '^Unit=' "$SD_DIR/lunarwing-mt-health.timer")"

# Long interval scales the run bound past the floor (45 min -> 2700s).
rm -f "$SD_DIR"/lunarwing-mt-health.*
HEALTH_INTERVAL_MIN="45"
LUNARWING_MT_SYSTEMD_UNIT_DIR="$SD_DIR" _install_health_systemd_timer >/dev/null
assert_eq "oneshot bound scales with long interval" "TimeoutStartSec=2700" \
  "$(grep '^TimeoutStartSec=' "$SD_DIR/lunarwing-mt-health.service")"
assert_eq "timer cadence scales with long interval" "OnCalendar=*:0/45" \
  "$(grep '^OnCalendar=' "$SD_DIR/lunarwing-mt-health.timer")"

printf 'LUNARWING_MT_HEALTH_INTERVAL_MIN=23\n' >"$HEALTH_ENV_FILE"
INIT_SYSTEM="openrc"
HEALTH_INTERVAL_MIN=""
ensure_init_system
assert_eq "persisted interval overrides default" "23" "$HEALTH_INTERVAL_MIN"
assert_eq "persisted interval names its source" "$HEALTH_ENV_FILE" "$HEALTH_INTERVAL_SOURCE"

HEALTH_INTERVAL_MIN=""
HEALTH_INTERVAL_SOURCE=""
ensure_init_system
assert_eq "persisted interval survives re-install resolution" "23" "$HEALTH_INTERVAL_MIN"

override_result="$(
  LUNARWING_MT_HEALTH_INTERVAL_MIN=7 bash -c '
    source "$1"
    HEALTH_ENV_FILE="$2"
    INIT_SYSTEM="openrc"
    ensure_init_system
    printf "%s|%s" "$HEALTH_INTERVAL_MIN" "$HEALTH_INTERVAL_SOURCE"
  ' _ "$MT_ADMIN" "$HEALTH_ENV_FILE"
)"
assert_eq "environment overrides persisted interval and names its source" \
  "7|environment (LUNARWING_MT_HEALTH_INTERVAL_MIN)" "$override_result"

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all health interval tests passed\n'
