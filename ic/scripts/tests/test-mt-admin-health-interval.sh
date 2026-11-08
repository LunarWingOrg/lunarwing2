#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"
unset LUNARWING_MT_HEALTH_INTERVAL_MIN LUNARWING_MT_CONTAINER_HEALTH_INTERVAL_SEC
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

assert_invalid_interval() {
  local label="$1" value="$2" origin="$3"
  local output="" expected_source="$HEALTH_ENV_FILE"
  local status=0
  # shellcheck disable=SC2016  # literal script receives its expansions as bash -c arguments
  local -a resolver=(
    bash -c '
      source "$1"
      HEALTH_ENV_FILE="$2"
      INIT_SYSTEM="openrc"
      ensure_init_system
    ' _ "$MT_ADMIN" "$HEALTH_ENV_FILE"
  )

  if [[ "$origin" == "environment" ]]; then
    expected_source="environment (LUNARWING_MT_HEALTH_INTERVAL_MIN)"
    if output="$(LUNARWING_MT_HEALTH_INTERVAL_MIN="$value" "${resolver[@]}" 2>&1)"; then
      status=0
    else
      status=$?
    fi
  else
    printf 'LUNARWING_MT_HEALTH_INTERVAL_MIN=%s\n' "$value" >"$HEALTH_ENV_FILE"
    if output="$("${resolver[@]}" 2>&1)"; then
      status=0
    else
      status=$?
    fi
  fi

  assert_eq "$label exits nonzero" "1" "$status"
  assert_eq "$label names its source" \
    "error: invalid LUNARWING_MT_HEALTH_INTERVAL_MIN from $expected_source: '$value' (expected an integer from 1 to 59)" \
    "$output"
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
    HEALTH_ENV_FILE="$2"
    INIT_SYSTEM="openrc"
    ensure_init_system
    printf "%s" "$HEALTH_INTERVAL_MIN"
  ' _ "$MT_ADMIN" "$HEALTH_ENV_FILE"
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
# shellcheck disable=SC2034
INIT_SYSTEM="openrc"
HEALTH_INTERVAL_MIN=""
HEALTH_INTERVAL_SOURCE=""
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
assert_eq "environment override does not rewrite persisted interval" "23" "$(_persisted_health_interval)"

assert_invalid_interval "quoted persisted interval" '"15"' "file"
assert_invalid_interval "zero persisted interval" "0" "file"
assert_invalid_interval "non-numeric environment interval" "often" "environment"
assert_invalid_interval "above-range environment interval" "60" "environment"

# ── Podman container healthcheck cadence ──────────────────────────────────────
# Podman runs each container healthcheck from its own systemd timer as a full
# `podman healthcheck run` process, so the default is 60 s (it was 10-15 s).
container_interval() {  # [env value] -> "<sec>|<source>|rc=<status>" or the error
  local env_value="${1:-}" output="" status=0
  # shellcheck disable=SC2016  # literal script receives its expansions as bash -c arguments
  output="$(LUNARWING_MT_CONTAINER_HEALTH_INTERVAL_SEC="$env_value" bash -c '
    source "$1"
    HEALTH_ENV_FILE="$2"
    INIT_SYSTEM="systemd"
    ensure_init_system
    printf "%s|%s" "$CONTAINER_HEALTH_INTERVAL_SEC" "$CONTAINER_HEALTH_INTERVAL_SOURCE"
  ' _ "$MT_ADMIN" "$HEALTH_ENV_FILE" 2>&1)" || status=$?
  printf '%s|rc=%s' "$output" "$status"
}

container_error() {  # <value> <source> -> the expected die message
  printf "error: invalid LUNARWING_MT_CONTAINER_HEALTH_INTERVAL_SEC from %s: '%s' (expected an integer from 10 to 3600)|rc=1" "$2" "$1"
}

env_source="environment (LUNARWING_MT_CONTAINER_HEALTH_INTERVAL_SEC)"
printf 'LUNARWING_MT_HEALTH_INTERVAL_MIN=15\n' >"$HEALTH_ENV_FILE"
assert_eq "container interval defaults to 60s" "60|default|rc=0" "$(container_interval)"

printf 'LUNARWING_MT_HEALTH_INTERVAL_MIN=15\n#LUNARWING_MT_CONTAINER_HEALTH_INTERVAL_SEC=30\n' >"$HEALTH_ENV_FILE"
assert_eq "commented container interval is ignored" "60|default|rc=0" "$(container_interval)"

printf 'LUNARWING_MT_HEALTH_INTERVAL_MIN=15\nLUNARWING_MT_CONTAINER_HEALTH_INTERVAL_SEC=120\n' >"$HEALTH_ENV_FILE"
assert_eq "persisted container interval names its source" "120|$HEALTH_ENV_FILE|rc=0" "$(container_interval)"
assert_eq "environment overrides persisted container interval" "45|$env_source|rc=0" "$(container_interval 45)"
assert_eq "environment override does not rewrite health.env" "120" \
  "$(_persisted_health_setting LUNARWING_MT_CONTAINER_HEALTH_INTERVAL_SEC)"
assert_eq "previous 15s cadence stays selectable" "15|$env_source|rc=0" "$(container_interval 15)"
assert_eq "container interval upper bound accepted" "3600|$env_source|rc=0" "$(container_interval 3600)"

assert_eq "container interval below 10s refused" "$(container_error 9 "$env_source")" "$(container_interval 9)"
assert_eq "container interval above 3600s refused" "$(container_error 3601 "$env_source")" "$(container_interval 3601)"
assert_eq "container interval with a unit suffix refused" "$(container_error 60s "$env_source")" "$(container_interval 60s)"
assert_eq "container interval with a leading zero refused" "$(container_error 0060 "$env_source")" "$(container_interval 0060)"
printf 'LUNARWING_MT_HEALTH_INTERVAL_MIN=15\nLUNARWING_MT_CONTAINER_HEALTH_INTERVAL_SEC="60"\n' >"$HEALTH_ENV_FILE"
assert_eq "quoted persisted container interval refused" "$(container_error '"60"' "$HEALTH_ENV_FILE")" "$(container_interval)"

# The health.env template documents the knob without pinning it.
template_text="$(declare -f ensure_health_pipeline)"
assert_eq "health.env template documents the container interval" "1" \
  "$(grep -cF '#LUNARWING_MT_CONTAINER_HEALTH_INTERVAL_SEC=60' <<<"$template_text")"

if [[ "$failures" -ne 0 ]]; then
  printf 'RESULT: %d failure(s)\n' "$failures"
  exit 1
fi
printf 'RESULT: all health interval tests passed\n'
