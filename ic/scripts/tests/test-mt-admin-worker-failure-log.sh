#!/usr/bin/env bash
# WI#882: `worker-failure-log` gives an operator (or an agent in the tenant
# sandbox, through the daemon log) the first-failure evidence for one worker
# lane without a hand-run `podman logs`. Kageho's 2.2.1.0 upgrade left both
# configured workers refusing loopback connections and the only evidence was a
# generic "Connection refused"; the host-side container logs were not reachable
# from the agent's view at all.
#
# Pins:
#  - the lane allowlist and the line-count argument are validated before any
#    runtime call, so a typo cannot shell out to a made-up container
#  - an absent bridge container is named, not silently skipped
#  - both the bridge and task container logs are read when they exist
#  - every log line passes through sanitize_display_value (a worker log can
#    echo a base URL, a model name, or a credential-bearing query)
#  - an unknown tenant is refused
# shellcheck disable=SC2034  # fixture globals are consumed by sourced helpers.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MT_ADMIN="$SCRIPT_DIR/../lunarwing-mt-admin.sh"

failures=0
pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }

# shellcheck source=../lunarwing-mt-admin.sh
source "$MT_ADMIN"

say() { printf '%s\n' "$*"; }
sanitize_name() { printf '%s' "$1"; }
tenant_exists_in_registry() { [[ "$1" == alice ]]; }
ensure_container_runtime() { CONTAINER_RT=podman; MT_ROOTLESS=false; }
tenant_worker_enabled() { [[ "$2" == pebble ]]; }

# Fixture runtime: pebble bridge exists and is running; its task container
# exists; every other lane is absent. Logs carry a secret-shaped query string
# and a non-printable byte, both of which must survive sanitization.
declare -A LOGS=(
  [lunarwing-pebble-alice]="listening on 127.0.0.1:20011
connecting ws://127.0.0.1:20011/ws/agent?token=s3cr3t"
  [lunarwing-pebble-task-alice]=$'task runner ready\ntrace=\x01'
)
_CALLS=""
_ctr() {  # <tenant> [container] <verb> ...
  local name="$1"; shift
  if [[ "${1:-}" == container ]]; then shift; fi
  _CALLS+="$1:$*"$'\n'
  case "${1:-}" in
    inspect)
      local fmt="" container=""
      if [[ "${2:-}" == "-f" ]]; then fmt="${3:-}"; container="${4:-}"; else container="${2:-}"; fi
      [[ -n "${LOGS[$container]+set}" ]] || return 1
      if [[ -z "$fmt" ]]; then printf 'true\n'; return 0; fi
      printf 'true running sha256:abcdef0123456789\n'
      ;;
    logs)
      local tail="0" container="${*: -1}"
      [[ "${2:-}" == "--tail" ]] && tail="${3:-0}"
      [[ -n "${LOGS[$container]+set}" ]] || return 1
      printf '%s\n' "${LOGS[$container]}" | tail -n "$tail"
      ;;
    *) return 1 ;;
  esac
}

out="$(worker_failure_log alice pebble 2)"

contains() { [[ "$out" == *"$2"* ]]; }
if contains x "enabled: true"; then pass "reports lane enablement"; else fail "reports lane enablement"; fi
if contains x "bridge container: lunarwing-pebble-alice running=true status=running image=sha256:abc"; then
  pass "reports the bridge container state and image"
else fail "reports the bridge container state and image"; printf '%s\n' "$out" >&2; fi
if contains x "--- last 2 log lines: lunarwing-pebble-alice ---"; then
  pass "reads the bridge container log"
else fail "reads the bridge container log"; fi
if contains x "--- last 2 log lines: lunarwing-pebble-task-alice ---"; then
  pass "reads the task container log too"
else fail "reads the task container log too"; fi
if contains x "token=s3cr3t"; then
  pass "log lines are passed through sanitize_display_value (secret-bearing text kept, not mangled)"
else fail "log lines are passed through sanitize_display_value"; fi
if contains x "trace=%01"; then
  pass "a non-printable byte is escaped by the sanitizer"
else fail "a non-printable byte is escaped by the sanitizer (raw %01 leaked)"; fi

# Absent lane: the bridge container is named, no log read is attempted.
out="$(worker_failure_log alice opencode 5)"
if [[ "$out" == *"bridge container: absent (lunarwing-opencode-alice)"* ]]; then
  pass "an absent bridge container is named"
else fail "an absent bridge container is named"; printf '%s\n' "$out" >&2; fi
if [[ "$out" != *"log lines"* ]]; then pass "no log read for an absent container"; else fail "no log read for an absent container"; fi

# Argument validation happens before any runtime call.
_CALLS=""
if ( worker_failure_log alice nosuchlane 5 ) >/dev/null 2>&1; then
  fail "an unknown lane is refused"
else pass "an unknown lane is refused"; fi
if [[ -z "$_CALLS" ]]; then pass "lane refusal happens before any runtime call"; else
  fail "lane refusal happens before any runtime call (calls: $_CALLS)"; fi

_CALLS=""
if ( worker_failure_log alice pebble zero ) >/dev/null 2>&1; then
  fail "a non-numeric line count is refused"
else pass "a non-numeric line count is refused"; fi
if [[ -z "$_CALLS" ]]; then pass "line-count refusal happens before any runtime call"; else
  fail "line-count refusal happens before any runtime call (calls: $_CALLS)"; fi

if ( worker_failure_log bob pebble ) >/dev/null 2>&1; then
  fail "an unknown tenant is refused"
else pass "an unknown tenant is refused"; fi

(( failures == 0 )) || exit 1
printf 'ALL TESTS PASSED\n'
