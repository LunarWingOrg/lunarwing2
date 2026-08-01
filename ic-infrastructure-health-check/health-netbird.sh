#!/bin/bash
# Health Check: NetBird daemon (tunnel dataplane)
# Checks: netbird daemon responsiveness, management-plane connection state
# Output: JSON to stdout
# Exit codes: 0=healthy, 1=degraded, 2=critical
#
# Why this exists: since v2.0.3.0 the tunnel provider can be NetBird
# (`netbird expose`, ic/src/tunnel/netbird.rs). Peer enrollment and the daemon
# lifecycle are operator-managed — LunarWing only owns the ephemeral expose
# process — so a dead netbird daemon silently kills every tenant tunnel while
# all other checks stay green. This probe closes that blind spot.
#
# The check auto-disables (healthy, metrics.enabled=false) when netbird is not
# installed, so hosts on the `none`/`custom` tunnel providers see no false
# criticals. Disable explicitly with HEALTH_NETBIRD_ENABLED=false.

set -euo pipefail

# Configuration
ENABLED="${HEALTH_NETBIRD_ENABLED:-true}"
BINARY="${NETBIRD_BINARY:-netbird}"
PROBE_TIMEOUT="${HEALTH_NETBIRD_TIMEOUT:-5}"

timestamp() { date -u +%Y-%m-%dT%H:%M:%SZ; }

# Probe disabled by config — emit healthy/disabled and stop.
if [ "$ENABLED" != "true" ]; then
    printf '{"component":"netbird","status":"healthy","timestamp":"%s","metrics":{"enabled":false},"issues":["netbird check disabled (HEALTH_NETBIRD_ENABLED)"]}\n' "$(timestamp)"
    exit 0
fi

# Binary absent → netbird is not the tunnel provider here; healthy/disabled.
if ! command -v "$BINARY" >/dev/null 2>&1; then
    printf '{"component":"netbird","status":"healthy","timestamp":"%s","metrics":{"enabled":false},"issues":["netbird binary not found; tunnel provider is not netbird (or set NETBIRD_BINARY)"]}\n' "$(timestamp)"
    exit 0
fi

daemon="down"
management="unknown"
latency_ms=0
issues=()

# Bounded `netbird status`; degrade gracefully when GNU timeout is absent.
start=$(date +%s%N)
if command -v timeout >/dev/null 2>&1; then
    status_out="$(timeout "$PROBE_TIMEOUT" "$BINARY" status 2>&1)" && probe_rc=0 || probe_rc=$?
else
    status_out="$("$BINARY" status 2>&1)" && probe_rc=0 || probe_rc=$?
fi
end=$(date +%s%N)
latency_ms=$(( (end - start) / 1000000 ))

if [ "$probe_rc" -eq 0 ]; then
    daemon="up"
    # Daemon answered; inspect the management-plane state. `netbird status`
    # prints e.g. "Management: Connected" / "Management: Disconnected".
    mgmt_line="$(printf '%s\n' "$status_out" | grep -i '^Management:' | head -1 || true)"
    if printf '%s' "$mgmt_line" | grep -qi 'connected' && ! printf '%s' "$mgmt_line" | grep -qi 'disconnected'; then
        management="connected"
    elif [ -n "$mgmt_line" ]; then
        management="disconnected"
        issues+=("netbird daemon up but management plane not connected: ${mgmt_line#Management: }")
    fi
else
    issues+=("netbird daemon not responding (netbird status exit $probe_rc); tenant tunnels are down")
fi

# Determine status
status="healthy"
exit_code=0

if [ "$daemon" = "down" ]; then
    status="critical"
    exit_code=2
elif [ "$management" = "disconnected" ]; then
    status="degraded"
    exit_code=1
fi

# Build issues JSON
issues_json="[]"
if [ ${#issues[@]} -gt 0 ]; then
    issues_json=$(printf '%s\n' "${issues[@]}" | jq -R . | jq -s .)
fi

# Output JSON
cat <<EOF
{
    "component": "netbird",
    "status": "$status",
    "timestamp": "$(timestamp)",
    "metrics": {
        "enabled": true,
        "daemon": "$daemon",
        "management": "$management",
        "latency_ms": $latency_ms
    },
    "issues": $issues_json
}
EOF

exit $exit_code
