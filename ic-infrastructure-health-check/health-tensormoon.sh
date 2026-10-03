#!/bin/bash
# Health Check: TensorMoon gateway health (formerly health-tensorzero.sh)
# Checks: inference latency (p50/p95 from TensorMoon's ClickHouse ModelInference
#         table), error-rate / queue-depth placeholders, GPU utilization
# Output: JSON to stdout
# Exit codes: 0=healthy, 1=degraded, 2=critical

set -euo pipefail

# Thresholds (env-configurable; operator typos reset to the defaults instead
# of breaking the numeric comparisons below — health-netbird.sh discipline).
# ERROR_* are floats (error_rate is a percent) so they validate as decimals.
# HEALTH_TENSORZERO_* (the pre-fork names) are still honoured as fallbacks.
P95_DEGRADED_MS="${HEALTH_TENSORMOON_P95_DEGRADED_MS:-${HEALTH_TENSORZERO_P95_DEGRADED_MS:-5000}}"
[[ "$P95_DEGRADED_MS" =~ ^[0-9]+(\.[0-9]+)?$ ]] || P95_DEGRADED_MS=5000
P95_CRITICAL_MS="${HEALTH_TENSORMOON_P95_CRITICAL_MS:-${HEALTH_TENSORZERO_P95_CRITICAL_MS:-15000}}"
[[ "$P95_CRITICAL_MS" =~ ^[0-9]+(\.[0-9]+)?$ ]] || P95_CRITICAL_MS=15000
ERROR_DEGRADED_PCT="${HEALTH_TENSORMOON_ERROR_DEGRADED_PCT:-${HEALTH_TENSORZERO_ERROR_DEGRADED_PCT:-1}}"
[[ "$ERROR_DEGRADED_PCT" =~ ^[0-9]+(\.[0-9]+)?$ ]] || ERROR_DEGRADED_PCT=1
ERROR_CRITICAL_PCT="${HEALTH_TENSORMOON_ERROR_CRITICAL_PCT:-${HEALTH_TENSORZERO_ERROR_CRITICAL_PCT:-5}}"
[[ "$ERROR_CRITICAL_PCT" =~ ^[0-9]+(\.[0-9]+)?$ ]] || ERROR_CRITICAL_PCT=5
QUEUE_DEGRADED="${HEALTH_TENSORMOON_QUEUE_DEGRADED:-${HEALTH_TENSORZERO_QUEUE_DEGRADED:-5}}"
[[ "$QUEUE_DEGRADED" =~ ^[0-9]+$ ]] || QUEUE_DEGRADED=5
QUEUE_CRITICAL="${HEALTH_TENSORMOON_QUEUE_CRITICAL:-${HEALTH_TENSORZERO_QUEUE_CRITICAL:-20}}"
[[ "$QUEUE_CRITICAL" =~ ^[0-9]+$ ]] || QUEUE_CRITICAL=20
GPU_UTIL_WARN_PCT="${HEALTH_TENSORMOON_GPU_UTIL_WARN_PCT:-${HEALTH_TENSORZERO_GPU_UTIL_WARN_PCT:-90}}"
[[ "$GPU_UTIL_WARN_PCT" =~ ^[0-9]+$ ]] || GPU_UTIL_WARN_PCT=90

# Initialize
p50_ms=0
p95_ms=0
error_rate=0.0
queue_depth=0
gpu_util=0
issues=()

# ClickHouse database holding TensorMoon's observability tables: the path
# component of the gateway's TENSORMOON_CLICKHOUSE_URL (`tensormoon` in the
# quadlets shipped with the tensormoon repo).
CLICKHOUSE_DB="${HEALTH_TENSORMOON_CLICKHOUSE_DB:-tensormoon}"
[[ "$CLICKHOUSE_DB" =~ ^[A-Za-z0-9_-]+$ ]] || CLICKHOUSE_DB=tensormoon

# Query ClickHouse for TensorMoon gateway latency. TensorMoon writes one
# ModelInference row per upstream model call (response_time_ms + materialized
# timestamp — unchanged from the TensorZero schema). Failed calls are not
# persisted there, so error_rate and queue_depth stay 0 from this probe; GPU
# utilization is a host-local signal read separately below. Any query failure
# (no clickhouse-client, no access, empty database) degrades to the GPU-only
# fallback exactly as before.
query_clickhouse() {
    local query_result=""

    if command -v clickhouse-client &>/dev/null; then
        query_result=$(clickhouse-client --format JSONEachRow 2>/dev/null <<EOF
SELECT
    if(isNaN(quantile(0.5)(response_time_ms)), 0, quantile(0.5)(response_time_ms)) AS p50_ms,
    if(isNaN(quantile(0.95)(response_time_ms)), 0, quantile(0.95)(response_time_ms)) AS p95_ms,
    0 AS error_rate,
    0 AS queue_depth
FROM \`${CLICKHOUSE_DB}\`.ModelInference
WHERE timestamp >= now() - INTERVAL 15 MINUTE
EOF
) || query_result=""
    fi

    echo "$query_result"
}

# Get GPU utilization (fallback)
get_gpu_util() {
    local util=0
    
    # Try nvidia-smi first
    if command -v nvidia-smi &>/dev/null; then
        util=$(nvidia-smi --query-gpu=utilization.gpu --format=csv,noheader,nounits | head -1 | tr -d ' ' 2>/dev/null || echo 0)
    # Try AMD ROCm
    elif command -v rocm-smi &>/dev/null; then
        util=$(rocm-smi --showuse --csv | grep -E "^[0-9]" | head -1 | cut -d',' -f2 | tr -d '%' 2>/dev/null || echo 0)
    fi
    
    echo ${util:-0}
}

# Run checks
metrics=$(query_clickhouse)

if [ -n "$metrics" ]; then
    # Parse ClickHouse results
    p50_ms=$(echo "$metrics" | jq -r '.p50_ms // 0')
    p95_ms=$(echo "$metrics" | jq -r '.p95_ms // 0')
    error_rate=$(echo "$metrics" | jq -r '.error_rate // 0')
    queue_depth=$(echo "$metrics" | jq -r '.queue_depth // 0')
fi
# GPU utilization is host-local (nvidia-smi / rocm-smi), independent of ClickHouse.
gpu_util=$(get_gpu_util)

# Add issues if thresholds exceeded
# Use awk for float comparisons (no bc dependency)
float_gt() { awk -v a="$1" -v b="$2" 'BEGIN { exit !(a > b) }'; }

if float_gt "$p95_ms" "$P95_CRITICAL_MS"; then
    issues+=("P95 latency critical: ${p95_ms}ms")
elif float_gt "$p95_ms" "$P95_DEGRADED_MS"; then
    issues+=("P95 latency elevated: ${p95_ms}ms")
fi

if float_gt "$error_rate" "$ERROR_CRITICAL_PCT"; then
    issues+=("Error rate critical: ${error_rate}%")
elif float_gt "$error_rate" "$ERROR_DEGRADED_PCT"; then
    issues+=("Error rate elevated: ${error_rate}%")
fi

if [ $queue_depth -gt $QUEUE_CRITICAL ]; then
    issues+=("Queue depth critical: ${queue_depth}")
elif [ $queue_depth -gt $QUEUE_DEGRADED ]; then
    issues+=("Queue depth elevated: ${queue_depth}")
fi

# GPU utilization warning (if high but not necessarily critical)
if float_gt "$gpu_util" "$GPU_UTIL_WARN_PCT"; then
    issues+=("GPU utilization high: ${gpu_util}%")
fi

# Determine status
status="healthy"
exit_code=0

if float_gt "$p95_ms" "$P95_CRITICAL_MS" || \
   float_gt "$error_rate" "$ERROR_CRITICAL_PCT" || \
   [ $queue_depth -gt $QUEUE_CRITICAL ]; then
    status="critical"
    exit_code=2
elif float_gt "$p95_ms" "$P95_DEGRADED_MS" || \
     float_gt "$error_rate" "$ERROR_DEGRADED_PCT" || \
     [ $queue_depth -gt $QUEUE_DEGRADED ]; then
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
    "component": "tensormoon",
    "status": "$status",
    "timestamp": "$(date -u +"%Y-%m-%dT%H:%M:%SZ")",
    "metrics": {
        "p50_ms": $p50_ms,
        "p95_ms": $p95_ms,
        "error_rate": $error_rate,
        "queue_depth": $queue_depth,
        "gpu_utilization": $gpu_util
    },
    "issues": $issues_json
}
EOF

exit $exit_code
