#!/usr/bin/env bash
# Health Check: LunarVoice ONNX sidecar
# Output JSON; exit 0=healthy, 1=degraded, 2=critical.

set -uo pipefail

TIMEOUT_SECS="${HEALTH_LUNARVOICE_TIMEOUT:-5}"
FETCH_METRICS="${HEALTH_LUNARVOICE_FETCH_METRICS:-true}"
ENABLED="${HEALTH_LUNARVOICE_ENABLED:-auto}"
DEFAULT_SINGLE_NODE_URL="http://127.0.0.1:8091"
TIMESTAMP="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

if [[ "$ENABLED" != true && "$ENABLED" != auto ]]; then
  printf '{"component":"lunarvoice","status":"healthy","timestamp":"%s","metrics":{"enabled":false,"reason":"disabled_by_config"},"issues":[]}\n' "$TIMESTAMP"
  exit 0
fi

resolve_registry() {
  local candidate base
  for candidate in \
    "${SELF_HEAL_TENANTS_FILE:-}" \
    "${LUNARWING_TENANTS_FILE:-}" \
    /etc/lunarwing/ports.json; do
    [[ -n "$candidate" && -f "$candidate" ]] || continue
    printf '%s\n' "$candidate"
    return 0
  done
  base="${LUNARWING_BASE_DIR:-${IRONCLAW_BASE_DIR:-}}"
  if [[ -n "$base" && -f "$base/tenants/ports.json" ]]; then
    printf '%s\n' "$base/tenants/ports.json"
    return 0
  fi
  return 1
}

targets=()
discovery=single
if [[ -n "${HEALTH_LUNARVOICE_URL:-}" ]]; then
  targets+=("override|${HEALTH_LUNARVOICE_URL%/}")
  discovery=override
else
  registry=""
  if registry="$(resolve_registry)"; then
    discovery=ports-registry
    while IFS=$'\t' read -r tenant port; do
      [[ -n "$tenant" && "$port" =~ ^[0-9]+$ ]] || continue
      targets+=("$tenant|http://127.0.0.1:$port")
    done < <(jq -r '
      (.tenants // {}) | to_entries[]
      | select(.value.enable_voice == true)
      | (.value.voice_ports.voice_health // empty) as $port
      | select($port != "")
      | "\(.key)\t\($port)"
    ' "$registry" 2>/dev/null)
    if [[ ${#targets[@]} -eq 0 ]]; then
      jq -n \
        --arg timestamp "$TIMESTAMP" \
        '{component:"lunarvoice",status:"healthy",timestamp:$timestamp,
          metrics:{enabled:false,reason:"no_enabled_tenants",discovery:"ports-registry",target_count:0,instances:[]},issues:[]}'
      exit 0
    fi
  else
    if [[ "$ENABLED" == true ]]; then
      targets+=("default|$DEFAULT_SINGLE_NODE_URL")
    else
      jq -n \
        --arg timestamp "$TIMESTAMP" \
        '{component:"lunarvoice",status:"healthy",timestamp:$timestamp,
          metrics:{enabled:false,reason:"not_configured",discovery:"single",target_count:0,instances:[]},issues:[]}'
      exit 0
    fi
  fi
fi

rank() {
  case "$1" in critical) printf 3 ;; degraded) printf 2 ;; healthy) printf 1 ;; *) printf 0 ;; esac
}

probe() {
  local tenant="$1" base="$2" raw code body curl_rc=0 start_ns end_ns
  local status=healthy service_status=unknown ready=false uptime=0 latency=0
  local total=0 transcription=0 synthesis=0 vad=0 cache_hits=0 rate_limited=0 errors=0
  local issues=()

  start_ns="$(date +%s%N)"
  raw="$(curl -s --connect-timeout "$TIMEOUT_SECS" --max-time "$TIMEOUT_SECS" \
    -w $'\n%{http_code}' "$base/health" 2>/dev/null)" || curl_rc=$?
  end_ns="$(date +%s%N)"
  latency=$(((end_ns - start_ns) / 1000000))
  code="$(printf '%s' "$raw" | tail -1)"
  body="$(printf '%s' "$raw" | sed '$d')"
  if [[ "$curl_rc" -ne 0 || "$code" == 000 ]]; then
    status=critical; issues+=("service unreachable at $base/health")
  elif [[ "$code" != 200 ]]; then
    status=critical; issues+=("health endpoint returned HTTP $code")
  elif ! jq -e . >/dev/null 2>&1 <<<"$body"; then
    status=critical; issues+=("health endpoint returned invalid JSON")
  else
    service_status="$(jq -r '.status // "unknown"' <<<"$body")"
    uptime="$(jq -r '.uptime_secs // 0' <<<"$body")"
    [[ "$service_status" == ok ]] || { status=critical; issues+=("service reports status='$service_status'"); }
  fi

  if [[ "$status" != critical ]]; then
    curl_rc=0
    raw="$(curl -s --connect-timeout "$TIMEOUT_SECS" --max-time "$TIMEOUT_SECS" \
      -w $'\n%{http_code}' "$base/ready" 2>/dev/null)" || curl_rc=$?
    code="$(printf '%s' "$raw" | tail -1)"
    body="$(printf '%s' "$raw" | sed '$d')"
    if [[ "$curl_rc" -ne 0 || "$code" != 200 ]] || ! jq -e . >/dev/null 2>&1 <<<"$body"; then
      status=critical; issues+=("STT readiness check failed")
    else
      ready="$(jq -r '.ready // false' <<<"$body")"
      [[ "$ready" == true ]] || { status=critical; issues+=("STT model not ready"); }
    fi
  fi

  if [[ "$FETCH_METRICS" == true && "$status" != critical ]]; then
    body="$(curl -fsS --connect-timeout "$TIMEOUT_SECS" --max-time "$TIMEOUT_SECS" "$base/metrics" 2>/dev/null)" || body=""
    if jq -e . >/dev/null 2>&1 <<<"$body"; then
      total="$(jq -r '.total_requests // 0' <<<"$body")"
      transcription="$(jq -r '.transcription_requests // 0' <<<"$body")"
      synthesis="$(jq -r '.synthesis_requests // 0' <<<"$body")"
      vad="$(jq -r '.vad_requests // 0' <<<"$body")"
      cache_hits="$(jq -r '.cache_hits // 0' <<<"$body")"
      rate_limited="$(jq -r '.rate_limited // 0' <<<"$body")"
      errors="$(jq -r '.errors // 0' <<<"$body")"
    fi
  fi

  local numeric
  for numeric in uptime latency total transcription synthesis vad cache_hits rate_limited errors; do
    [[ "${!numeric}" =~ ^[0-9]+$ ]] || printf -v "$numeric" 0
  done

  local issues_json="[]"
  [[ ${#issues[@]} -eq 0 ]] || issues_json="$(printf '%s\n' "${issues[@]}" | jq -R . | jq -s .)"
  jq -n \
    --arg tenant "$tenant" --arg url "$base/health" --arg status "$status" \
    --arg service_status "$service_status" --arg ready "$ready" \
    --argjson uptime "$uptime" --argjson latency "$latency" \
    --argjson total "$total" --argjson transcription "$transcription" \
    --argjson synthesis "$synthesis" --argjson vad "$vad" \
    --argjson cache_hits "$cache_hits" --argjson rate_limited "$rate_limited" \
    --argjson errors "$errors" --argjson issues "$issues_json" \
    '{tenant:$tenant,url:$url,status:$status,service_status:$service_status,ready:($ready=="true"),
      uptime_secs:$uptime,latency_ms:$latency,total_requests:$total,
      transcription_requests:$transcription,synthesis_requests:$synthesis,vad_requests:$vad,
      cache_hits:$cache_hits,rate_limited:$rate_limited,errors:$errors,issues:$issues}'
}

overall=healthy
exit_code=0
instances='[]'
issues='[]'
for target in "${targets[@]}"; do
  tenant="${target%%|*}"
  base="${target#*|}"
  instance="$(probe "$tenant" "$base")"
  instance_status="$(jq -r '.status' <<<"$instance")"
  if (( $(rank "$instance_status") > $(rank "$overall") )); then
    overall="$instance_status"
  fi
  instances="$(jq -c --argjson instance "$instance" '. + [$instance]' <<<"$instances")"
  issues="$(jq -c --argjson more "$(jq '.issues' <<<"$instance")" '. + $more' <<<"$issues")"
done
case "$overall" in critical) exit_code=2 ;; degraded) exit_code=1 ;; esac

jq -n \
  --arg timestamp "$TIMESTAMP" --arg status "$overall" --arg discovery "$discovery" \
  --argjson instances "$instances" --argjson issues "$issues" \
  '{component:"lunarvoice",status:$status,timestamp:$timestamp,
    metrics:{enabled:true,discovery:$discovery,target_count:($instances|length),instances:$instances},issues:$issues}'
exit "$exit_code"
