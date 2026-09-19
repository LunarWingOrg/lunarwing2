#!/usr/bin/env bash
# Tests for health-knowledge.sh — knowledge publication/sync/sweep monitoring
# (WI#180 F8, MR !1041 review finding 6).
#
# Self-contained: a fake ports registry + fixture state files + fixture tenant
# env files. Never touches the host's real tenant state.
#
#   A: no state files + default env (sync off, sweep on) -> healthy
#      (not-yet-observed sweep stays inside the grace window)
#   B: sync ENABLED + consecutive failures -> degraded, then critical
#   C: sync enabled + deferred notes -> degraded
#   D: sweep overdue -> degraded; sweep DISABLED + stale state -> healthy
#   E: malformed state of an enabled feature -> degraded; disabled -> inert
#   F: invalid registry -> degraded, never healthy
#   G: enabled + never observed: inside grace -> healthy, past grace -> degraded
#   H: enabled + sync not attempting (past cadence x multiplier) -> degraded
#   I: enablement surfaced as metrics; cadence read from the env file
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/lib.sh
source "$DIR/lib.sh"
HC="$DIR/../health-knowledge.sh"

[[ -r "$HC" ]] || { echo "FATAL: health-knowledge.sh not found at $HC"; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "FATAL: jq required"; exit 1; }

new_sb() {
    local sb; sb="$(mktemp -d "${TMPDIR:-/tmp}/health-knowledge-test.XXXXXX")"
    mkdir -p "$sb/state" "$sb/env"
    printf '%s' "$sb"
}
mk_registry() { jq -n --arg t "$2" '{tenants:{($t):{user:$t}}}' > "$1/ports.json"; }
# mk_env <sb> [sync-enabled] [sweep-enabled] [sync-cadence] [sweep-cadence] [age-hours]
mk_env() {
    local sb="$1" sync="${2:-false}" sweep="${3:-true}" \
          sync_cad="${4:-6}" sweep_cad="${5:-24}" age="${6:-1}"
    {
        printf 'KNOWLEDGE_SYNC_ENABLED=%s\n' "$sync"
        printf 'KNOWLEDGE_SWEEP_ENABLED=%s\n' "$sweep"
        printf 'KNOWLEDGE_SYNC_CADENCE_HOURS=%s\n' "$sync_cad"
        printf 'KNOWLEDGE_SWEEP_CADENCE_HOURS=%s\n' "$sweep_cad"
    } > "$sb/env/lunarwing.env"
    # The content rewrite updates mtime; re-stamp the intended age afterwards.
    touch -d "@$(( $(date +%s) - age * 3600 ))" "$sb/env/lunarwing.env"
}
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
    env HEALTH_KNOWLEDGE_TENANTS_FILE="$sb/none.json" HEALTH_KNOWLEDGE_STATE_ROOT="$sb/state" \
        HEALTH_KNOWLEDGE_ENV_FILE="$sb/env/lunarwing.env" "$@" bash "$HC" 2>&1
}

# ── A: defaults (sync off, sweep on) + no state -> healthy inside grace ─────
a="$(new_sb)"; mk_registry "$a" acme; mk_env "$a"
oA="$(run_hc "$a" LUNARWING_TENANTS_FILE="$a/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oA")" "healthy" "A1: absent state + defaults -> healthy (grace)"
assert_eq "$(jq -r '.metrics.instances[0].sync_state_present' <<<"$oA")" "false" "A2: sync state absent surfaced"
assert_eq "$(jq -r '.metrics.instances[0].sync_enabled' <<<"$oA")" "false" "A3: sync disabled surfaced"
assert_eq "$(jq -r '.metrics.instances[0].sweep_enabled' <<<"$oA")" "true" "A4: sweep enabled surfaced"

# ── B: sync ENABLED + consecutive failures degrade, then critical ────────────
b="$(new_sb)"; mk_registry "$b" acme; mk_env "$b" true; mk_sync "$b" 2 0 3600 "note_import_failed"
oB="$(run_hc "$b" LUNARWING_TENANTS_FILE="$b/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oB")" "degraded" "B1: enabled sync, 2 consecutive failures -> degraded"
assert_eq "$(jq -r '.metrics.instances[0].sync_consecutive_failures' <<<"$oB")" "2" "B2: failure count surfaced"
assert_eq "$(jq -r '.metrics.instances[0].sync_last_error_class' <<<"$oB")" "note_import_failed" "B3: error class surfaced"
assert_contains "$(jq -r '.issues[0]' <<<"$oB")" "consecutive failures" "B4: failure issue named"

b2="$(new_sb)"; mk_registry "$b2" acme; mk_env "$b2" true; mk_sync "$b2" 7 0 3600 "note_import_failed"
oB2="$(run_hc "$b2" LUNARWING_TENANTS_FILE="$b2/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oB2")" "critical" "B5: 7 consecutive failures -> critical"

# B6 — the same failing state with sync DISABLED is inert (past feature, not
# an actionable failure).
b3="$(new_sb)"; mk_registry "$b3" acme; mk_env "$b3" false; mk_sync "$b3" 7 5 3600 "note_import_failed"
oB3="$(run_hc "$b3" LUNARWING_TENANTS_FILE="$b3/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oB3")" "healthy" "B6: disabled sync ignores stale failures"

# ── C: enabled sync + deferred notes -> degraded ────────────────────────────
c="$(new_sb)"; mk_registry "$c" acme; mk_env "$c" true; mk_sync "$c" 0 3 3600
oC="$(run_hc "$c" LUNARWING_TENANTS_FILE="$c/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oC")" "degraded" "C1: enabled sync, deferred notes -> degraded"
assert_eq "$(jq -r '.metrics.instances[0].sync_deferred_notes' <<<"$oC")" "3" "C2: deferred count surfaced"

# ── D: sweep overdue -> degraded; disabled sweep ignores stale state ─────────
d="$(new_sb)"; mk_registry "$d" acme; mk_env "$d"; mk_sweep "$d" 300000
oD="$(run_hc "$d" LUNARWING_TENANTS_FILE="$d/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oD")" "degraded" "D1: enabled sweep overdue -> degraded"
assert_contains "$(jq -r '.issues[0]' <<<"$oD")" "sweep overdue" "D2: overdue issue named"

d2="$(new_sb)"; mk_registry "$d2" acme; mk_env "$d2" false false; mk_sweep "$d2" 300000
oD2="$(run_hc "$d2" LUNARWING_TENANTS_FILE="$d2/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oD2")" "healthy" "D3: disabled sweep ignores stale state (review finding 6)"
assert_eq "$(jq -r '.metrics.instances[0].sweep_enabled' <<<"$oD2")" "false" "D4: sweep disabled surfaced"

# ── E: malformed state of an enabled feature -> degraded; disabled -> inert ─
e="$(new_sb)"; mk_registry "$e" acme; mk_env "$e" true
printf 'not json' > "$e/state/knowledge_sync_state.json"
oE="$(run_hc "$e" LUNARWING_TENANTS_FILE="$e/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oE")" "degraded" "E1: malformed state of enabled sync -> degraded"
assert_contains "$(jq -r '.issues[0]' <<<"$oE")" "malformed" "E2: malformed issue named"

e2="$(new_sb)"; mk_registry "$e2" acme; mk_env "$e2" false false
printf 'not json' > "$e2/state/knowledge_sync_state.json"
oE2="$(run_hc "$e2" LUNARWING_TENANTS_FILE="$e2/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oE2")" "healthy" "E3: malformed state of a disabled feature is inert"

# ── F: invalid registry -> degraded, never healthy ──────────────────────────
f="$(new_sb)"; printf 'not json' > "$f/ports.json"
oF="$(run_hc "$f" LUNARWING_TENANTS_FILE="$f/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oF")" "degraded" "F1: invalid registry -> degraded"

# ── G: enabled + never observed: grace window vs past-grace ─────────────────
g="$(new_sb)"; mk_registry "$g" acme; mk_env "$g" true true 6 24 1
oG="$(run_hc "$g" LUNARWING_TENANTS_FILE="$g/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oG")" "healthy" "G1: enabled, never observed, inside grace -> healthy"

g2="$(new_sb)"; mk_registry "$g2" acme; mk_env "$g2" true true 6 24 48
oG2="$(run_hc "$g2" LUNARWING_TENANTS_FILE="$g2/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oG2")" "degraded" "G2: enabled, never observed, past grace -> degraded"
assert_contains "$(jq -r '.issues[0]' <<<"$oG2")" "never observed" "G3: never-observed issue named"

g3="$(new_sb)"; mk_registry "$g3" acme; mk_env "$g3" true true 6 24 48
oG3="$(run_hc "$g3" LUNARWING_TENANTS_FILE="$g3/ports.json" HEALTH_KNOWLEDGE_STARTUP_GRACE_HOURS=0)"
assert_eq "$(jq -r '.status' <<<"$oG3")" "degraded" "G4: zero grace degrades immediately"

# ── H: enabled sync present but not attempting (past cadence x multiplier) ───
h="$(new_sb)"; mk_registry "$h" acme
mk_env "$h" true true 6 24 48
# Attempt ~30h ago (cadence 6h x multiplier 3 = 18h overdue), no failures.
success_ts="$(date -u -d "@$(( $(date +%s) - 110000 ))" +%Y-%m-%dT%H:%M:%SZ)"
attempt_ts="$success_ts"
jq -n --arg s "$success_ts" --arg a "$attempt_ts" \
  '{schema_version:1,consecutive_failures:0,notes_deferred_last_run:0,
    last_success_at:$s,last_attempt_at:$a,last_error_class:""}' > "$h/state/knowledge_sync_state.json"
oH="$(run_hc "$h" LUNARWING_TENANTS_FILE="$h/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oH")" "degraded" "H1: enabled sync not attempting -> degraded"
assert_contains "$(jq -r '.issues[0]' <<<"$oH")" "sync overdue" "H2: not-attempting issue named"

# ── I: enablement + cadence surfaced as metrics ─────────────────────────────
i="$(new_sb)"; mk_registry "$i" acme; mk_env "$i" true true 4 12
oI="$(run_hc "$i" LUNARWING_TENANTS_FILE="$i/ports.json")"
assert_eq "$(jq -r '.metrics.instances[0].sync_cadence_hours' <<<"$oI")" "4" "I1: sync cadence surfaced"
assert_eq "$(jq -r '.metrics.instances[0].sweep_cadence_hours' <<<"$oI")" "12" "I2: sweep cadence surfaced"
# 12h cadence x 3 = 36h overdue budget (default multiplier), not the old fixed 72h.
mk_sweep "$i" $(( 40 * 3600 ))
oI2="$(run_hc "$i" LUNARWING_TENANTS_FILE="$i/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oI2")" "degraded" "I3: overdue budget derives from the actual cadence"
# The same 40h age is NOT overdue at the default 24h cadence (72h budget).
i3="$(new_sb)"; mk_registry "$i3" acme; mk_env "$i3"; mk_sweep "$i3" $(( 40 * 3600 ))
oI3="$(run_hc "$i3" LUNARWING_TENANTS_FILE="$i3/ports.json")"
assert_eq "$(jq -r '.status' <<<"$oI3")" "healthy" "I4: default cadence keeps 40h within budget"

finish
