#!/usr/bin/env bash
# Health Check: NetBird host tunnel peer
# Output: one JSON object on stdout; logs/errors remain off stdout.
# Exit codes: 0=healthy/disabled, 2=critical.

set -uo pipefail

ENABLED="${HEALTH_NETBIRD_ENABLED:-false}"
BINARY="${NETBIRD_BINARY:-netbird}"
PROBE_TIMEOUT="${HEALTH_NETBIRD_TIMEOUT:-5}"
TIMESTAMP="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

if [[ "$ENABLED" != true ]]; then
    jq -n --arg ts "$TIMESTAMP" '{
        component: "netbird",
        status: "healthy",
        timestamp: $ts,
        metrics: {enabled: false},
        issues: []
    }'
    exit 0
fi

if ! [[ "$PROBE_TIMEOUT" =~ ^[1-9][0-9]*$ ]]; then
    PROBE_TIMEOUT=5
fi

if ! command -v "$BINARY" >/dev/null 2>&1; then
    jq -n --arg ts "$TIMESTAMP" --arg binary "$BINARY" '{
        component: "netbird",
        status: "critical",
        timestamp: $ts,
        metrics: {enabled: true, ready: false},
        issues: ["NetBird monitoring is enabled but the configured binary is unavailable: " + $binary]
    }'
    exit 2
fi

probe_rc=0
if command -v timeout >/dev/null 2>&1; then
    timeout "$PROBE_TIMEOUT" "$BINARY" status --check startup >/dev/null 2>&1 || probe_rc=$?
else
    "$BINARY" status --check startup >/dev/null 2>&1 || probe_rc=$?
fi

if [[ "$probe_rc" -eq 0 ]]; then
    jq -n --arg ts "$TIMESTAMP" '{
        component: "netbird",
        status: "healthy",
        timestamp: $ts,
        metrics: {enabled: true, ready: true},
        issues: []
    }'
    exit 0
fi

issue="NetBird peer readiness failed (netbird status --check startup exit ${probe_rc})"
if [[ "$probe_rc" -eq 124 ]]; then
    issue="NetBird peer readiness probe timed out"
fi
jq -n --arg ts "$TIMESTAMP" --arg issue "$issue" --argjson rc "$probe_rc" '{
    component: "netbird",
    status: "critical",
    timestamp: $ts,
    metrics: {enabled: true, ready: false, probe_exit: $rc},
    issues: [$issue]
}'
exit 2
