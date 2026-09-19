#!/usr/bin/env bash
# Tests for health-gateway.sh — legacy session artifact monitoring.
#
# MR !1041 review finding 1: an absent legacy sessions directory must NEVER
# degrade (it used to schedule daemon restarts on hosts whose only "problem"
# was the absence of obsolete telemetry). The check measures legacy artifacts
# when they exist and stays healthy-with-note when they do not; live gateway
# health belongs to the runtime component.
#
#   A: absent legacy dir -> healthy, exit 0, telemetry note surfaced
#   B: present dir, no problems -> healthy
#   C: present dir + stale locks -> degraded (real findings still actionable)
#   D: present dir + oversized sessions -> degraded
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/lib.sh
source "$DIR/lib.sh"
HC="$DIR/../health-gateway.sh"

[[ -r "$HC" ]] || { echo "FATAL: health-gateway.sh not found at $HC"; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "FATAL: jq required"; exit 1; }

new_sb() {
    local sb; sb="$(mktemp -d "${TMPDIR:-/tmp}/health-gateway-test.XXXXXX")"
    mkdir -p "$sb/agents"
    printf '%s' "$sb"
}
run_hc() {  # <sb> [ENV=VALUE ...] -> stdout+stderr, rc via $?
    local sb="$1"; shift
    env LUNARWING_BASE_DIR="$sb" "$@" bash "$HC" 2>&1
}

# ── A: absent legacy directory -> healthy (no fabricated degradation) ───────
a="$(mktemp -d "${TMPDIR:-/tmp}/health-gateway-test.XXXXXX")"   # no agents/ dir
oA="$(run_hc "$a")"; a_rc=$?
assert_eq "$(jq -r '.status' <<<"$oA")" "healthy" "A1: absent legacy dir -> healthy"
assert_eq "$a_rc" "0" "A2: absent legacy dir exits 0 (cannot trigger self-heal restarts)"
assert_eq "$(jq -r '.metrics.legacy_sessions_dir' <<<"$oA")" "absent" "A3: telemetry gap surfaced as a metric"
assert_contains "$(jq -r '.issues[0]' <<<"$oA")" "legacy sessions directory absent" "A4: telemetry note explained"

# ── B: present dir, nothing wrong -> healthy ────────────────────────────────
b="$(new_sb)"
oB="$(run_hc "$b")"; b_rc=$?
assert_eq "$(jq -r '.status' <<<"$oB")" "healthy" "B1: empty legacy dir -> healthy"
assert_eq "$b_rc" "0" "B2: empty legacy dir exits 0"

# ── C: present dir + stale lock -> degraded (real artifact, real action) ────
c="$(new_sb)"
: > "$c/agents/session-1.lock"
touch -d "@$(( $(date +%s) - 3600 ))" "$c/agents/session-1.lock"
oC="$(run_hc "$c")"; c_rc=$?
assert_eq "$(jq -r '.status' <<<"$oC")" "degraded" "C1: stale lock -> degraded"
assert_eq "$c_rc" "1" "C2: stale lock exits 1"
assert_contains "$(jq -r '.issues[0]' <<<"$oC")" "Stale lock" "C3: stale lock issue named"
assert_eq "$(jq -r '.metrics.stale_locks' <<<"$oC")" "1" "C4: stale lock count surfaced"

# ── D: present dir + oversized session -> degraded ─────────────────────────
d="$(new_sb)"
dd if=/dev/zero of="$d/agents/big.jsonl" bs=1024 count=600 2>/dev/null
oD="$(run_hc "$d")"; d_rc=$?
assert_eq "$(jq -r '.status' <<<"$oD")" "degraded" "D1: oversized session -> degraded"
assert_eq "$d_rc" "1" "D2: oversized session exits 1"
assert_contains "$(jq -r '.issues[0]' <<<"$oD")" "Oversized session" "D3: oversized issue named"

finish
