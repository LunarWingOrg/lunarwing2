#!/bin/bash
# Health Check: Gateway Session Health
# Checks: active sessions, stale locks, orphan transcripts
# Output: JSON to stdout
# Exit codes: 0=healthy, 1=degraded, 2=critical

set -euo pipefail

SESSIONS_DIR="${LUNARWING_BASE_DIR:-${IRONCLAW_BASE_DIR:-$HOME/.lunarwing}}/agents"
# Thresholds (env-configurable; operator typos reset to the defaults instead
# of breaking the integer comparisons below — health-netbird.sh discipline).
STALE_LOCK_MINUTES="${HEALTH_GATEWAY_STALE_LOCK_MINUTES:-10}"
[[ "$STALE_LOCK_MINUTES" =~ ^[0-9]+$ ]] || STALE_LOCK_MINUTES=10
STALE_LOCKS_CRITICAL="${HEALTH_GATEWAY_STALE_LOCKS_CRITICAL:-3}"
[[ "$STALE_LOCKS_CRITICAL" =~ ^[0-9]+$ ]] || STALE_LOCKS_CRITICAL=3
ORPHAN_THRESHOLD_DEGRADED="${HEALTH_GATEWAY_ORPHAN_DEGRADED:-10}"
[[ "$ORPHAN_THRESHOLD_DEGRADED" =~ ^[0-9]+$ ]] || ORPHAN_THRESHOLD_DEGRADED=10
ORPHAN_THRESHOLD_CRITICAL="${HEALTH_GATEWAY_ORPHAN_CRITICAL:-50}"
[[ "$ORPHAN_THRESHOLD_CRITICAL" =~ ^[0-9]+$ ]] || ORPHAN_THRESHOLD_CRITICAL=50
OVERSIZED_DEGRADED="${HEALTH_GATEWAY_OVERSIZED_DEGRADED:-0}"
[[ "$OVERSIZED_DEGRADED" =~ ^[0-9]+$ ]] || OVERSIZED_DEGRADED=0

# Initialize results
active_sessions=0
stale_locks=0
orphan_count=0
issues=()

# An absent legacy sessions directory is NOT a daemon failure (MR !1041 review
# finding 1): the directory belongs to the legacy session-store layout, and a
# current deployment legitimately has no such artifacts to measure. Degrading
# here made the self-healer schedule daemon restarts on healthy hosts whose
# only "problem" was the absence of obsolete telemetry. The live gateway
# contract is probed by the tenant-aware runtime check (health-runtime.sh);
# this check only measures legacy artifacts when they exist. An absent
# directory reports healthy with an explicit telemetry note.
if [ ! -d "$SESSIONS_DIR" ]; then
    printf '{"component":"gateway","status":"healthy","timestamp":"%s","metrics":{"enabled":true,"active_sessions":0,"stale_locks":0,"orphan_transcripts":0,"oversized_sessions":0,"legacy_sessions_dir":"absent"},"issues":["legacy sessions directory absent (%s); legacy telemetry not measurable; runtime diagnostics come from the runtime component"]}\n' \
        "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" "$SESSIONS_DIR"
    exit 0
fi

# Check for stale lock files. Populates the global STALE_LOCKS_FOUND and
# appends to the global issues array. MUST be called without command
# substitution — a $(...) subshell would silently discard the issue entries
# (the reason the issues array was historically always empty here).
check_stale_locks() {
    STALE_LOCKS_FOUND=0
    while IFS= read -r lockfile; do
        lock_age=$(( $(date +%s) - $(stat -c %Y "$lockfile" 2>/dev/null || stat -f %m "$lockfile" 2>/dev/null || echo 0) ))
        age_minutes=$(( lock_age / 60 ))

        if [ "$age_minutes" -gt "$STALE_LOCK_MINUTES" ]; then
            STALE_LOCKS_FOUND=$((STALE_LOCKS_FOUND + 1))
            issues+=("Stale lock: $lockfile (${age_minutes}m old)")
        fi
    done < <(find "$SESSIONS_DIR" -name "*.lock" -type f 2>/dev/null)
}

# Check for orphan transcript files (echoes a count; no global mutation).
check_orphans() {
    local orphans=0

    if [ -d "$SESSIONS_DIR" ]; then
        # Orphans = .jsonl files not referenced in sessions.json
        while IFS= read -r jsonl; do
            orphans=$((orphans + 1))
        done < <(find "$SESSIONS_DIR" -name "*.jsonl" -size +10M -type f 2>/dev/null)
    fi

    echo $orphans
}

# Count active sessions (echoes a count; no global mutation).
count_sessions() {
    local count=0

    if [ -d "$SESSIONS_DIR" ]; then
        # Count session directories or jsonl files modified recently
        count=$(find "$SESSIONS_DIR" -name "*.jsonl" -mmin -60 -type f 2>/dev/null | wc -l)
    fi

    echo $count
}

# Check for oversized sessions (context window issues). Populates the global
# OVERSIZED_FOUND and appends to the global issues array — same no-subshell
# rule as check_stale_locks.
check_oversized_sessions() {
    OVERSIZED_FOUND=0
    while IFS= read -r jsonl; do
        OVERSIZED_FOUND=$((OVERSIZED_FOUND + 1))
        local size
        size=$(du -h "$jsonl" | cut -f1)
        issues+=("Oversized session: $jsonl ($size)")
    done < <(find "$SESSIONS_DIR" -name "*.jsonl" -size +500k -type f 2>/dev/null)
}

# Run checks
check_stale_locks
stale_locks=$STALE_LOCKS_FOUND
orphan_count=$(check_orphans)
active_sessions=$(count_sessions)
check_oversized_sessions
oversized=$OVERSIZED_FOUND

# Determine status
status="healthy"
exit_code=0

if [ $stale_locks -gt $STALE_LOCKS_CRITICAL ] || [ $orphan_count -gt $ORPHAN_THRESHOLD_CRITICAL ]; then
    status="critical"
    exit_code=2
elif [ $stale_locks -gt 0 ] || [ $orphan_count -gt $ORPHAN_THRESHOLD_DEGRADED ] || [ $oversized -gt $OVERSIZED_DEGRADED ]; then
    status="degraded"
    exit_code=1
fi

# Build issues array JSON
issues_json="[]"
if [ ${#issues[@]} -gt 0 ]; then
    issues_json=$(printf '%s\n' "${issues[@]}" | jq -R . | jq -s .)
fi

# Output JSON
cat <<EOF
{
    "component": "gateway",
    "status": "$status",
    "timestamp": "$(date -u +"%Y-%m-%dT%H:%M:%SZ")",
    "metrics": {
        "active_sessions": $active_sessions,
        "stale_locks": $stale_locks,
        "orphan_transcripts": $orphan_count,
        "oversized_sessions": $oversized
    },
    "issues": $issues_json
}
EOF

exit $exit_code
