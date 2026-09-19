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

# An absent legacy sessions directory is NOT proof of health (WI#180 F7): the
# gateway check must distinguish "no legacy artifacts to measure" from "all
# clear". The tenant-aware runtime probe (health-runtime.sh) queries the live
# gateway contract; here an absent directory reports degraded with explicit
# unavailable telemetry instead of healthy with all-zero metrics.
if [ ! -d "$SESSIONS_DIR" ]; then
    printf '{"component":"gateway","status":"degraded","timestamp":"%s","metrics":{"enabled":true,"active_sessions":0,"stale_locks":0,"orphan_transcripts":0,"oversized_sessions":0,"legacy_sessions_dir":"absent"},"issues":["legacy sessions directory absent (%s); runtime diagnostics unavailable to this check"]}\n' \
        "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" "$SESSIONS_DIR"
    exit 1
fi

# Check for stale lock files
check_stale_locks() {
    local locks_found=0
    
    # Look for .lock files older than threshold
    if [ -d "$SESSIONS_DIR" ]; then
        while IFS= read -r lockfile; do
            local lock_age=$(( $(date +%s) - $(stat -c %Y "$lockfile" 2>/dev/null || stat -f %m "$lockfile" 2>/dev/null || echo 0) ))
            local age_minutes=$(( lock_age / 60 ))
            
            if [ $age_minutes -gt $STALE_LOCK_MINUTES ]; then
                locks_found=$((locks_found + 1))
                issues+=("Stale lock: $lockfile (${age_minutes}m old)")
            fi
        done < <(find "$SESSIONS_DIR" -name "*.lock" -type f 2>/dev/null)
    fi
    
    echo $locks_found
}

# Check for orphan transcript files
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

# Count active sessions
count_sessions() {
    local count=0
    
    if [ -d "$SESSIONS_DIR" ]; then
        # Count session directories or jsonl files modified recently
        count=$(find "$SESSIONS_DIR" -name "*.jsonl" -mmin -60 -type f 2>/dev/null | wc -l)
    fi
    
    echo $count
}

# Check for oversized sessions (context window issues)
check_oversized_sessions() {
    local oversized=0
    
    if [ -d "$SESSIONS_DIR" ]; then
        # Sessions over 500KB are likely to cause issues
        while IFS= read -r jsonl; do
            oversized=$((oversized + 1))
            local size
            size=$(du -h "$jsonl" | cut -f1)
            issues+=("Oversized session: $jsonl ($size)")
        done < <(find "$SESSIONS_DIR" -name "*.jsonl" -size +500k -type f 2>/dev/null)
    fi
    
    echo $oversized
}

# Run checks
active_sessions=$(count_sessions)
stale_locks=$(check_stale_locks)
orphan_count=$(check_orphans)
oversized=$(check_oversized_sessions)

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
