#!/usr/bin/env bash
# Tests for health-knowledge.sh — knowledge publication/sync/sweep monitoring
# (WI#180 F8).
#
# Self-contained: a fake ports registry + fixture state files. Never touches
# the host's real tenant state.
#
#   A: no state files -> healthy (sync disabled is not failure)
#   B: consecutive sync failures -> degraded, then critical at threshold
#   C: deferred notes -> degraded
#   D: sweep overdue -> degraded
#   E: malformed state -> degraded monitoring gap
#   F: invalid registry -> degraded, never healthy
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/lib.sh
source "$DIR/lib.sh"
HC="$DIR/../health-knowledge.sh"

[[ -r "$HC" ]] || { echo "FATAL: health-knowledge.sh not found at $HC"; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "FATAL: jq required"; exit 1; }

new_sb() {
    local sb; sb="$(mktemp -d "${TMPDIR:-/tmp}/health-knowledge-test.XXXXXX")"
    mkdir -p "$sb/state"
    printf '%s' "$sb"
}
mk_registry() { jq -n --arg t "$2" '{tenants:{($t):{user:$t}}}' > "$1/ports.json"; }
# mk_sync <sb> <failures> <deferred> <last-success-age-secs> [error-class]
mk_sync() {
    local sb="$1" failures="$2" deferred="$3" age="$4" err="${5:-}"
    local success_ts attempt_ts
    success_ts="$(date -u -d "@$(( $(date +%s) - age ))" +%Y-%m-%dT%H:%M:%SZ)"
    attempt_ts="$(date -u -d "@$(( $(date +%s) - 300 ))" +%Y-%m-%dT%H:%M:%SZ)"
    jq -n --argjson f "$failures" --argjson d "$deferred" --arg s "$success_ts" \
      --arg a "$attempt_ts" --arg e "$err" \
      '{schema_version:1,consecutive_failures:$f,notes_deferred_last_run:$d,
        last_success_at:$s,last_attempt_at:$a,last_error_class:$e}' > "$sb/state/knowledge_sync_state.json"
}
# mk_sweep <sb> <last-run-age-secs>
mk_sweep() {
    local sb="$1" age="$2"
    local ts; ts="$(date -u -d "@$(( $(date +%s) - age ))" +%Y-%m-%dT%H:%M:%SZ)"
    jq -n --arg t "$ts" '{last_run:$t}' > "$sb/state/knowledge_sweep_state.json"
}

run_hc() {  # <sb> [ENV=VALUE ...] -> stdout+stderr, rc via $?
    local sb="$1"; shift
    env HEALTH_KNOWLEDGE_TENANTS_FILE="$sb/none.json" HEALTH_KNOWLEDGE_STATE_ROOT="$sb/state" "$@" bash "$HC" 2>&1
}

# ── A: no state files -> healthy (disabled sync is not failure) ─────────────
a="$(new_sb)"; mk_registry "$a" acme
oA="$(run_hc "$a" LUNARWING_TENANTS_FILE="$a/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oA")" "healthy" "A1: absent state files -> healthy"
assert_eq "$(jq -r '.metrics.instances[0].sync_state_present' <<<"$oA")" "false" "A2: sync state absent surfaced"

# ── B: consecutive failures degrade, then critical ──────────────────────────
b="$(new_sb)"; mk_registry "$b" acme; mk_sync "$b" 2 0 3600 "note_import_failed"
oB="$(run_hc "$b" LUNARWING_TENANTS_FILE="$b/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oB")" "degraded" "B1: 2 consecutive failures -> degraded"
assert_eq "$(jq -r '.metrics.instances[0].sync_consecutive_failures' <<<"$oB")" "2" "B2: failure count surfaced"
assert_eq "$(jq -r '.metrics.instances[0].sync_last_error_class' <<<"$oB")" "note_import_failed" "B3: error class surfaced"
assert_contains "$(jq -r '.issues[0]' <<<"$oB")" "consecutive failures" "B4: failure issue named"

b2="$(new_sb)"; mk_registry "$b2" acme; mk_sync "$b2" 7 0 3600 "note_import_failed"
oB2="$(run_hc "$b2" LUNARWING_TENANTS_FILE="$b2/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oB2")" "critical" "B5: 7 consecutive failures -> critical"

# ── C: deferred notes -> degraded ───────────────────────────────────────────
c="$(new_sb)"; mk_registry "$c" acme; mk_sync "$c" 0 3 3600
oC="$(run_hc "$c" LUNARWING_TENANTS_FILE="$c/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oC")" "degraded" "C1: deferred notes -> degraded"
assert_eq "$(jq -r '.metrics.instances[0].sync_deferred_notes' <<<"$oC")" "3" "C2: deferred count surfaced"

# ── D: sweep overdue -> degraded ────────────────────────────────────────────
d="$(new_sb)"; mk_registry "$d" acme; mk_sweep "$d" 300000
oD="$(run_hc "$d" LUNARWING_TENANTS_FILE="$d/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oD")" "degraded" "D1: sweep overdue -> degraded"
assert_contains "$(jq -r '.issues[0]' <<<"$oD")" "sweep overdue" "D2: overdue issue named"

# ── E: malformed state -> degraded monitoring gap ───────────────────────────
e="$(new_sb)"; mk_registry "$e" acme
printf 'not json' > "$e/state/knowledge_sync_state.json"
oE="$(run_hc "$e" LUNARWING_TENANTS_FILE="$e/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oE")" "degraded" "E1: malformed sync state -> degraded"
assert_contains "$(jq -r '.issues[0]' <<<"$oE")" "malformed" "E2: malformed issue named"

# ── F: invalid registry -> degraded, never healthy ──────────────────────────
f="$(new_sb)"; printf 'not json' > "$f/ports.json"
oF="$(run_hc "$f" LUNARWING_TENANTS_FILE="$f/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oF")" "degraded" "F1: invalid registry -> degraded"

finish
