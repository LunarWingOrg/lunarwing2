#!/bin/bash
# Health Check: LunarVision Sidecar (OCR + Vision-Language)
# Checks: service reachability, /health endpoint, optional /vision/metrics
# Output: JSON to stdout
# Exit codes: 0=healthy, 1=degraded, 2=critical
#
# Env vars:
#   HEALTH_LUNARVISION_ENABLED       (default: true)   — false disables
#   HEALTH_LUNARVISION_URL           (optional)        — single base URL override
#                                                        (e.g. http://127.0.0.1:8088).
#                                                        When unset, multi-tenant hosts
#                                                        auto-discover vision_health ports
#                                                        from the ports registry; single-node
#                                                        falls back to http://127.0.0.1:8088.
#   SELF_HEAL_TENANTS_FILE / LUNARWING_TENANTS_FILE    — ports registry path
#                                                        (default: /etc/lunarwing/ports.json)
#   HEALTH_LUNARVISION_TIMEOUT       (default: 5)      — curl timeout in seconds
#   HEALTH_LUNARVISION_FETCH_METRICS (default: true)   — also probe /vision/metrics
#   HEALTH_LUNARVISION_REQUIRE_VL    (default: false)  — degrade if VL backend not confirmed
#   HEALTH_LUNARVISION_LATENCY_DEGRADED_MS  (default: 2000)
#   HEALTH_LUNARVISION_LATENCY_CRITICAL_MS (default: 5000)

set -uo pipefail

# ── Config ──────────────────────────────────────────────────────────────────

TIMEOUT_SECS="${HEALTH_LUNARVISION_TIMEOUT:-5}"
FETCH_METRICS="${HEALTH_LUNARVISION_FETCH_METRICS:-true}"
REQUIRE_VL="${HEALTH_LUNARVISION_REQUIRE_VL:-false}"
LATENCY_DEGRADED_MS="${HEALTH_LUNARVISION_LATENCY_DEGRADED_MS:-2000}"
LATENCY_CRITICAL_MS="${HEALTH_LUNARVISION_LATENCY_CRITICAL_MS:-5000}"
DEFAULT_SINGLE_NODE_URL="http://127.0.0.1:8088"

TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

# ── Disable shortcut ─────────────────────────────────────────────────────────

if [ "${HEALTH_LUNARVISION_ENABLED:-true}" != "true" ]; then
    printf '{"component":"lunarvision","status":"healthy","timestamp":"%s","metrics":{"enabled":false}}\n' \
        "$TIMESTAMP"
    exit 0
fi

# ── Target resolution (MT-aware) ─────────────────────────────────────────────
# Explicit HEALTH_LUNARVISION_URL always wins (operator override).
# Otherwise discover every tenant's vision_health port from the ports registry
# so multi-tenant hosts never false-fail against the single-node 8088 default.
# If no registry / no vision ports, fall back to single-node 8088.

_resolve_tenants_file() {
    if [ -n "${SELF_HEAL_TENANTS_FILE:-}" ] && [ -f "${SELF_HEAL_TENANTS_FILE}" ]; then
        printf '%s\n' "${SELF_HEAL_TENANTS_FILE}"
        return 0
    fi
    if [ -n "${LUNARWING_TENANTS_FILE:-}" ] && [ -f "${LUNARWING_TENANTS_FILE}" ]; then
        printf '%s\n' "${LUNARWING_TENANTS_FILE}"
        return 0
    fi
    if [ -f /etc/lunarwing/ports.json ]; then
        printf '%s\n' /etc/lunarwing/ports.json
        return 0
    fi
    local base="${LUNARWING_BASE_DIR:-${IRONCLAW_BASE_DIR:-}}"
    if [ -n "$base" ] && [ -f "$base/tenants/ports.json" ]; then
        printf '%s\n' "$base/tenants/ports.json"
        return 0
    fi
    return 1
}

# Populate TARGETS as lines: "tenant_or_host|http://127.0.0.1:PORT"
TARGETS=()
if [ -n "${HEALTH_LUNARVISION_URL:-}" ]; then
    TARGETS+=("override|${HEALTH_LUNARVISION_URL%/}")
else
    tenants_file=""
    if tenants_file="$(_resolve_tenants_file)"; then
        if command -v jq >/dev/null 2>&1; then
            # Prefer dedicated vision_health; fall back to vision_service if older registry.
            while IFS=$'\t' read -r tname port; do
                [ -n "$tname" ] && [ -n "$port" ] && [ "$port" != "null" ] || continue
                TARGETS+=("${tname}|http://127.0.0.1:${port}")
            done < <(jq -r '
                .tenants // {}
                | to_entries[]
                | . as $e
                | (
                    $e.value.extended_ports.vision_health
                    // $e.value.extended_ports.vision_service
                    // empty
                  ) as $p
                | select($p != null and $p != "")
                | "\($e.key)\t\($p)"
            ' "$tenants_file" 2>/dev/null)
        fi
    fi
    if [ "${#TARGETS[@]}" -eq 0 ]; then
        TARGETS+=("default|${DEFAULT_SINGLE_NODE_URL}")
    fi
fi

# ── Probe one URL ────────────────────────────────────────────────────────────

# Sets globals: p_status p_exit p_issues p_latency p_svc_status p_tesseract
# p_uptime p_vl p_total p_ocr p_vision p_hits p_misses p_rate p_hit_rate p_avg
_probe_one() {
    local base_url="$1"
    p_status="healthy"
    p_exit=0
    p_issues=()
    p_latency=0
    p_svc_status=""
    p_tesseract=""
    p_uptime=0
    p_vl="unknown"
    p_total=0
    p_ocr=0
    p_vision=0
    p_hits=0
    p_misses=0
    p_rate=0
    p_hit_rate=0.0
    p_avg=0

    local health_raw http_code health_body curl_exit start_ns end_ns
    health_raw=""
    http_code="000"
    curl_exit=0
    start_ns=$(date +%s%N)
    health_raw=$(curl -s --connect-timeout "$TIMEOUT_SECS" --max-time "$TIMEOUT_SECS" \
        -w "\n%{http_code}" \
        "${base_url}/health" 2>/dev/null) || curl_exit=$?
    end_ns=$(date +%s%N)
    p_latency=$(( (end_ns - start_ns) / 1000000 ))
    http_code=$(printf '%s' "$health_raw" | tail -1)
    health_body=$(printf '%s' "$health_raw" | sed '$d')

    if [ "$http_code" = "000" ] || [ "$curl_exit" -ne 0 ]; then
        p_status="critical"
        p_exit=2
        p_issues+=("Service unreachable at ${base_url}/health")
    elif [ "$http_code" != "200" ]; then
        p_status="critical"
        p_exit=2
        p_issues+=("Health endpoint returned HTTP ${http_code}")
    elif ! printf '%s' "$health_body" | jq . >/dev/null 2>&1; then
        p_status="critical"
        p_exit=2
        p_issues+=("Health endpoint returned invalid JSON")
    else
        p_svc_status=$(printf '%s' "$health_body" | jq -r '.status // "unknown"')
        p_tesseract=$(printf '%s' "$health_body" | jq -r '.tesseract_version // "unknown"')
        p_uptime=$(printf '%s' "$health_body" | jq -r '.uptime_secs // 0')
        p_vl=$(printf '%s' "$health_body" | jq -r '.vl_available // "unknown"')

        if [ "$p_svc_status" != "ok" ]; then
            p_status="critical"
            p_exit=2
            p_issues+=("Service reports status='${p_svc_status}', expected 'ok'")
        fi

        if [ "$p_tesseract" = "unknown" ] || [ -z "$p_tesseract" ]; then
            if [ "$p_status" = "healthy" ]; then
                p_status="degraded"
                p_exit=1
            fi
            p_issues+=("Tesseract version unavailable — OCR engine may not be installed")
        fi
    fi

    if [ "$p_latency" -gt "$LATENCY_CRITICAL_MS" ] 2>/dev/null; then
        if [ "$p_status" = "healthy" ]; then
            p_status="degraded"
            p_exit=1
        fi
        p_issues+=("Health endpoint latency critical: ${p_latency}ms")
    elif [ "$p_latency" -gt "$LATENCY_DEGRADED_MS" ] 2>/dev/null; then
        if [ "$p_status" = "healthy" ]; then
            p_status="degraded"
            p_exit=1
        fi
        p_issues+=("Health endpoint latency elevated: ${p_latency}ms")
    fi

    if [ "$FETCH_METRICS" = "true" ] && [ "$http_code" = "200" ]; then
        local metrics_raw
        metrics_raw=$(curl -s --connect-timeout "$TIMEOUT_SECS" --max-time "$TIMEOUT_SECS" \
            "${base_url}/vision/metrics" 2>/dev/null) || true
        if printf '%s' "$metrics_raw" | jq . >/dev/null 2>&1; then
            p_total=$(printf '%s' "$metrics_raw" | jq -r '.total_requests // 0')
            p_ocr=$(printf '%s' "$metrics_raw" | jq -r '.ocr_requests // 0')
            p_vision=$(printf '%s' "$metrics_raw" | jq -r '.vision_requests // 0')
            p_hits=$(printf '%s' "$metrics_raw" | jq -r '.cache_hits // 0')
            p_misses=$(printf '%s' "$metrics_raw" | jq -r '.cache_misses // 0')
            p_rate=$(printf '%s' "$metrics_raw" | jq -r '.rate_limited // 0')
            p_hit_rate=$(printf '%s' "$metrics_raw" | jq -r '.cache_hit_rate // 0.0')
            p_avg=$(printf '%s' "$metrics_raw" | jq -r '.avg_latency_ms // 0')
        fi
    fi

    if [ "$REQUIRE_VL" = "true" ]; then
        if [ "$p_vl" = "false" ]; then
            if [ "$p_status" = "healthy" ]; then
                p_status="degraded"
                p_exit=1
            fi
            p_issues+=("VL backend required but unavailable")
        elif [ "$p_vl" = "unknown" ]; then
            if [ "$p_status" = "healthy" ]; then
                p_status="degraded"
                p_exit=1
            fi
            p_issues+=("VL backend required but availability unknown (sidecar health endpoint doesn't report vl_available)")
        fi
    fi

    # numeric guards
    [[ -z "$p_latency" || ! "$p_latency" =~ ^[0-9]+$ ]] && p_latency=0
    [[ -z "$p_uptime" || ! "$p_uptime" =~ ^[0-9]+$ ]] && p_uptime=0
    [[ -z "$p_total" || ! "$p_total" =~ ^[0-9]+$ ]] && p_total=0
    [[ -z "$p_ocr" || ! "$p_ocr" =~ ^[0-9]+$ ]] && p_ocr=0
    [[ -z "$p_vision" || ! "$p_vision" =~ ^[0-9]+$ ]] && p_vision=0
    [[ -z "$p_hits" || ! "$p_hits" =~ ^[0-9]+$ ]] && p_hits=0
    [[ -z "$p_misses" || ! "$p_misses" =~ ^[0-9]+$ ]] && p_misses=0
    [[ -z "$p_rate" || ! "$p_rate" =~ ^[0-9]+$ ]] && p_rate=0
    [[ -z "$p_avg" || ! "$p_avg" =~ ^[0-9]+$ ]] && p_avg=0
    [[ -z "$p_hit_rate" || ! "$p_hit_rate" =~ ^[0-9.]+$ ]] && p_hit_rate=0.0
}

# ── Rank helpers ─────────────────────────────────────────────────────────────

_rank() {
    case "$1" in
        critical) echo 3 ;;
        degraded|unknown) echo 2 ;;
        healthy) echo 1 ;;
        *) echo 0 ;;
    esac
}

# ── Probe all targets ────────────────────────────────────────────────────────

status="healthy"
exit_code=0
issues=()
instances_json="[]"
# Roll-up metrics from first healthy / last probed instance for top-level fields
svc_status=""
tesseract_version=""
uptime_secs=0
vl_available="unknown"
latency_ms=0
total_requests=0
ocr_requests=0
vision_requests=0
cache_hits=0
cache_misses=0
rate_limited=0
cache_hit_rate=0.0
avg_latency_ms=0
primary_url=""

for entry in "${TARGETS[@]}"; do
    tname="${entry%%|*}"
    base_url="${entry#*|}"
    _probe_one "$base_url"

    # Prefix issues with tenant when multi-target
    local_issues=()
    for iss in "${p_issues[@]+"${p_issues[@]}"}"; do
        if [ "${#TARGETS[@]}" -gt 1 ] && [ "$tname" != "override" ] && [ "$tname" != "default" ]; then
            local_issues+=("${tname}: ${iss}")
        else
            local_issues+=("$iss")
        fi
    done

    if [ "$(_rank "$p_status")" -gt "$(_rank "$status")" ]; then
        status="$p_status"
        exit_code="$p_exit"
    fi
    for iss in "${local_issues[@]+"${local_issues[@]}"}"; do
        issues+=("$iss")
    done

    # Prefer healthy instance metrics for top-level rollup; else last
    if [ -z "$primary_url" ] || [ "$p_status" = "healthy" ]; then
        primary_url="$base_url"
        svc_status="$p_svc_status"
        tesseract_version="$p_tesseract"
        uptime_secs="$p_uptime"
        vl_available="$p_vl"
        latency_ms="$p_latency"
        total_requests="$p_total"
        ocr_requests="$p_ocr"
        vision_requests="$p_vision"
        cache_hits="$p_hits"
        cache_misses="$p_misses"
        rate_limited="$p_rate"
        cache_hit_rate="$p_hit_rate"
        avg_latency_ms="$p_avg"
    fi

    iss_json="[]"
    if [ "${#local_issues[@]}" -gt 0 ]; then
        iss_json=$(printf '%s\n' "${local_issues[@]}" | jq -R . | jq -s .)
    fi
    inst=$(jq -n \
        --arg tenant "$tname" \
        --arg url "${base_url}/health" \
        --arg st "$p_status" \
        --arg latency "$p_latency" \
        --arg svc "$p_svc_status" \
        --arg tess "$p_tesseract" \
        --arg uptime "$p_uptime" \
        --arg vl "$p_vl" \
        --argjson issues "$iss_json" \
        '{
            tenant: $tenant,
            url: $url,
            status: $st,
            latency_ms: ($latency | tonumber),
            service_status: $svc,
            tesseract_version: $tess,
            uptime_secs: ($uptime | tonumber),
            vl_available: $vl,
            issues: $issues
        }')
    instances_json=$(jq -c --argjson i "$inst" '. + [$i]' <<<"$instances_json")
done

[ -z "$primary_url" ] && primary_url="$DEFAULT_SINGLE_NODE_URL"

issues_json="[]"
if [ ${#issues[@]} -gt 0 ]; then
    issues_json=$(printf '%s\n' "${issues[@]}" | jq -R . | jq -s .)
fi

capabilities="ocr"
if [ "$vl_available" = "true" ] || [ "$vision_requests" -gt 0 ] 2>/dev/null; then
    capabilities="ocr,vl"
fi

discovery="single"
if [ -n "${HEALTH_LUNARVISION_URL:-}" ]; then
    discovery="override"
elif [ "${#TARGETS[@]}" -gt 0 ] && [ "${TARGETS[0]#*|}" != "$DEFAULT_SINGLE_NODE_URL" ] || [ "${#TARGETS[@]}" -gt 1 ]; then
    # registry-derived if not pure default single entry
    if [ "${TARGETS[0]}" = "default|${DEFAULT_SINGLE_NODE_URL}" ] && [ "${#TARGETS[@]}" -eq 1 ]; then
        discovery="single"
    else
        discovery="ports-registry"
    fi
fi
# cleaner discovery flag
if [ -n "${HEALTH_LUNARVISION_URL:-}" ]; then
    discovery="override"
else
    case "${TARGETS[0]%%|*}" in
        default) discovery="single" ;;
        *) discovery="ports-registry" ;;
    esac
    [ "${#TARGETS[@]}" -gt 1 ] && discovery="ports-registry"
fi

jq -n \
    --arg component "lunarvision" \
    --arg status "$status" \
    --arg timestamp "$TIMESTAMP" \
    --arg latency "$latency_ms" \
    --arg url "${primary_url}/health" \
    --arg svc_status "$svc_status" \
    --arg tesseract_version "$tesseract_version" \
    --arg uptime_secs "$uptime_secs" \
    --arg vl_available "$vl_available" \
    --arg capabilities "$capabilities" \
    --arg total_requests "$total_requests" \
    --arg ocr_requests "$ocr_requests" \
    --arg vision_requests "$vision_requests" \
    --arg cache_hits "$cache_hits" \
    --arg cache_misses "$cache_misses" \
    --arg rate_limited "$rate_limited" \
    --arg cache_hit_rate "$cache_hit_rate" \
    --arg avg_latency_ms "$avg_latency_ms" \
    --arg discovery "$discovery" \
    --arg target_count "${#TARGETS[@]}" \
    --argjson instances "$instances_json" \
    --argjson issues "$issues_json" \
    '{
        component: $component,
        status: $status,
        timestamp: $timestamp,
        metrics: {
            enabled: true,
            url: $url,
            discovery: $discovery,
            target_count: ($target_count | tonumber),
            instances: $instances,
            latency_ms: ($latency | tonumber),
            service_status: $svc_status,
            tesseract_version: $tesseract_version,
            uptime_secs: ($uptime_secs | tonumber),
            vl_available: $vl_available,
            capabilities: $capabilities,
            total_requests: ($total_requests | tonumber),
            ocr_requests: ($ocr_requests | tonumber),
            vision_requests: ($vision_requests | tonumber),
            cache: {
                hits: ($cache_hits | tonumber),
                misses: ($cache_misses | tonumber),
                hit_rate: ($cache_hit_rate | tonumber)
            },
            rate_limited: ($rate_limited | tonumber),
            avg_latency_ms: ($avg_latency_ms | tonumber)
        },
        issues: $issues
    }'

exit $exit_code
