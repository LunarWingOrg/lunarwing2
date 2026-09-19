#!/usr/bin/env bash
# Health Check: Tenant Runtime Diagnostics (WI#180 F7)
#
# Probes the CURRENT runtime contracts instead of legacy filesystem artifacts:
#   - gateway  /api/gateway/status        (uptime, connections, channel health)
#   - gateway  /api/chat/pending-gates    (durable execution gates)
#   - bridge   /v1/status                 (queue, rate limit, OMEMO diagnostics)
#
# Discovery is ports-registry-aware (same contract as health-lunarvision.sh):
# on multi-tenant hosts every tenant's gateway/bridge port is discovered from
# the registry; single-node falls back to the default ports and the base-dir
# env file. Bearer tokens are read from the tenant env files and never logged
# or exported. An absent legacy agents directory is NOT proof of health; an
# unreachable gateway is a real runtime failure.
#
# Status semantics:
#   - gateway unreachable or not running          -> critical (daemon down)
#   - bridge unreachable/not running when XMPP is configured -> degraded
#   - bridge delivery backlog (queued messages)    -> degraded/critical
#   - gateway channels reporting unhealthy         -> degraded (real failure)
#   - pending gates (approval/auth waits)          -> metric only, never a
#     failure: normal interactive waits are not stalled execution; but a
#     reachable gateway whose /api/chat/pending-gates endpoint cannot be
#     observed is degraded (observability gap)
#   - discovery unavailable on an MT host          -> degraded (monitoring gap)
#
# Bounded sweep: per-request curl timeouts are capped by the remaining sweep
# budget (HEALTH_RUNTIME_SWEEP_DEADLINE_SECS), so slow or hanging endpoints
# cannot push the sweep past the driver's outer timeout before the report is
# emitted.
#
# Output: JSON to stdout; exit 0=healthy, 1=degraded, 2=critical.
#
# Env vars:
#   HEALTH_RUNTIME_ENABLED           (default: auto)  — false disables
#   HEALTH_RUNTIME_TIMEOUT           (default: 5)     — curl timeout seconds
#   HEALTH_RUNTIME_QUEUE_DEGRADED    (default: 10)    — bridge queue backlog
#   HEALTH_RUNTIME_QUEUE_CRITICAL    (default: 50)
#   SELF_HEAL_TENANTS_FILE / LUNARWING_TENANTS_FILE   — ports registry path
#   LUNARWING_BASE_DIR / IRONCLAW_BASE_DIR            — single-node base dir
set -uo pipefail

TIMEOUT_SECS="${HEALTH_RUNTIME_TIMEOUT:-5}"
[[ "$TIMEOUT_SECS" =~ ^[0-9]+$ ]] || TIMEOUT_SECS=5
QUEUE_DEGRADED="${HEALTH_RUNTIME_QUEUE_DEGRADED:-10}"
[[ "$QUEUE_DEGRADED" =~ ^[0-9]+$ ]] || QUEUE_DEGRADED=10
QUEUE_CRITICAL="${HEALTH_RUNTIME_QUEUE_CRITICAL:-50}"
[[ "$QUEUE_CRITICAL" =~ ^[0-9]+$ ]] || QUEUE_CRITICAL=50
ENABLED="${HEALTH_RUNTIME_ENABLED:-auto}"
TIMESTAMP="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
DEFAULT_GATEWAY_PORT=3000
DEFAULT_BRIDGE_PORT=8787

if [[ "$ENABLED" != true && "$ENABLED" != auto ]]; then
  printf '{"component":"runtime","status":"healthy","timestamp":"%s","metrics":{"enabled":false,"reason":"disabled_by_config"},"issues":[]}\n' "$TIMESTAMP"
  exit 0
fi

resolve_registry() {
  local candidate base
  for candidate in \
    "${SELF_HEAL_TENANTS_FILE:-}" \
    "${LUNARWING_TENANTS_FILE:-}" \
    "${HEALTH_RUNTIME_TENANTS_FILE:-/etc/lunarwing/ports.json}"; do
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

# Read a KEY=VALUE from a tenant env file without exposing it in logs.
env_value() {  # <env-file> <key> -> value (empty when absent/unreadable)
  local file="$1" key="$2"
  [[ -n "$file" && -r "$file" ]] || return 0
  sed -n "s/^${key}=//p" "$file" 2>/dev/null | tail -1
}

# Bounded authenticated GET into the global HTTP_BODY. <url> <token>.
#
# MUST be called without command substitution: a $(...) subshell would
# silently discard p_probe_exhausted, which is exactly how the "budget
# exhausted mid-probe" signal used to get lost.
#
# Per-request timeout is capped by the REMAINING sweep budget (MR !1041 review
# finding 3): a tenant whose endpoints hang must not be able to spend three
# full HEALTH_RUNTIME_TIMEOUT waits and push the sweep past the driver's outer
# timeout before any JSON is emitted. When the budget is exhausted mid-probe
# the call is skipped and p_probe_exhausted is raised so the caller can mark
# the instance unprobed instead of fabricating an "unreachable" verdict for a
# request that was never allowed to complete.
http_get() {
  local url="$1" token="$2" t rem
  HTTP_BODY=""
  t="$TIMEOUT_SECS"
  if [[ "$SWEEP_DEADLINE_SECS" =~ ^[0-9]+$ ]] && [[ "$SWEEP_DEADLINE_SECS" -gt 0 ]]; then
    rem=$(( SWEEP_DEADLINE_SECS - SECONDS ))
    if (( rem < 1 )); then
      p_probe_exhausted=true
      return 0
    fi
    (( t > rem )) && t=$rem
  fi
  if [[ -n "$token" ]]; then
    HTTP_BODY="$(curl -s --connect-timeout "$t" --max-time "$t" \
      -H "Authorization: Bearer $token" "$url" 2>/dev/null)" || true
  else
    HTTP_BODY="$(curl -s --connect-timeout "$t" --max-time "$t" \
      "$url" 2>/dev/null)" || true
  fi
}

# Probe one tenant's runtime. Sets globals: p_gw_reachable
# p_gw_uptime p_gw_connections p_gw_channels_unhealthy p_pending_gates
# p_bridge_reachable p_bridge_running p_bridge_queued p_bridge_outbound_hour
# p_bridge_omemo p_bridge_prekeys p_issues (array).
probe_one() {  # <tenant> <gateway-port> <bridge-port> <env-file>
  local tenant="$1" gw_port="$2" bridge_port="$3" env_file="$4"
  local gw_token bridge_token gw_body bridge_body
  p_gw_reachable=false; p_gw_uptime=0; p_gw_connections=0
  p_gw_channels_unhealthy=0; p_pending_gates=-1
  p_bridge_reachable=false; p_bridge_running=false; p_bridge_queued=-1
  p_bridge_outbound_hour=-1; p_bridge_omemo=false; p_bridge_prekeys=-1
  p_probe_exhausted=false
  p_issues=()

  gw_token="$(env_value "$env_file" GATEWAY_AUTH_TOKEN)"
  bridge_token="$(env_value "$env_file" XMPP_BRIDGE_TOKEN)"

  http_get "http://127.0.0.1:${gw_port}/api/gateway/status" "$gw_token"
  gw_body="$HTTP_BODY"
  if [[ -n "$gw_body" ]] && jq -e . >/dev/null 2>&1 <<<"$gw_body"; then
    p_gw_reachable=true
    p_gw_uptime="$(jq -r '.uptime_secs // 0' <<<"$gw_body" 2>/dev/null || echo 0)"
    p_gw_connections="$(jq -r '.total_connections // 0' <<<"$gw_body" 2>/dev/null || echo 0)"
    p_gw_channels_unhealthy="$(jq -r '[.channel_health // {} | to_entries[] | select(.value.healthy == false)] | length' <<<"$gw_body" 2>/dev/null || echo 0)"
  else
    p_issues+=("gateway unreachable at 127.0.0.1:${gw_port}")
  fi

  local gates_body
  http_get "http://127.0.0.1:${gw_port}/api/chat/pending-gates" "$gw_token"
  gates_body="$HTTP_BODY"
  if [[ -n "$gates_body" ]] && jq -e . >/dev/null 2>&1 <<<"$gates_body"; then
    p_pending_gates="$(jq -r '(.gates // []) | length' <<<"$gates_body" 2>/dev/null || echo 0)"
  fi

  http_get "http://127.0.0.1:${bridge_port}/v1/status" "$bridge_token"
  bridge_body="$HTTP_BODY"
  if [[ -n "$bridge_body" ]] && jq -e . >/dev/null 2>&1 <<<"$bridge_body"; then
    p_bridge_reachable=true
    p_bridge_running="$(jq -r '.running // false' <<<"$bridge_body" 2>/dev/null || echo false)"
    p_bridge_queued="$(jq -r '.queued_messages // 0' <<<"$bridge_body" 2>/dev/null || echo 0)"
    p_bridge_outbound_hour="$(jq -r '.outbound_messages_last_hour // 0' <<<"$bridge_body" 2>/dev/null || echo 0)"
    p_bridge_omemo="$(jq -r '.omemo_enabled // false' <<<"$bridge_body" 2>/dev/null || echo false)"
    p_bridge_prekeys="$(jq -r '.prekeys_available // 0' <<<"$bridge_body" 2>/dev/null || echo 0)"
    if [[ "$p_bridge_running" != "true" ]]; then
      p_issues+=("xmpp bridge configured but not running")
    fi
  else
    p_issues+=("xmpp bridge unreachable at 127.0.0.1:${bridge_port}")
  fi
}

# Sweep deadline (WI#180 F4 contract): keep completed observations, mark the
# rest unprobed, degrade for lost coverage. 0 disables. Per-request budgets
# (see http_get) bound a single tenant's probe so the sweep lands inside this
# deadline instead of racing the driver's outer timeout (MR !1041 review
# finding 3).
SWEEP_DEADLINE_SECS="${HEALTH_RUNTIME_SWEEP_DEADLINE_SECS:-22}"
[[ "$SWEEP_DEADLINE_SECS" =~ ^[0-9]+$ ]] || SWEEP_DEADLINE_SECS=22
sweep_incomplete=false
unprobed_count=0

instances='[]'
issues='[]'
overall="healthy"
exit_code=0
discovery="single"
targets=()

if registry="$(resolve_registry)"; then
  discovery="ports-registry"
  if ! jq -e 'type == "object" and ((.tenants // {}) | type == "object")' \
      "$registry" >/dev/null 2>&1; then
    printf '{"component":"runtime","status":"degraded","timestamp":"%s","metrics":{"enabled":true,"discovery":"ports-registry","target_count":0,"instances":[]},"issues":["ports registry is not valid tenant JSON"]}\n' "$TIMESTAMP"
    exit 1
  fi
  while IFS=$'\t' read -r tenant user gw_port bridge_port; do
    [[ -n "$tenant" ]] || continue
    [[ "$gw_port" =~ ^[0-9]+$ ]] || continue
    targets+=("$tenant|$gw_port|$bridge_port|/home/$user/lunarwing/env/lunarwing.env")
  done < <(jq -r '
    (.tenants // {}) | to_entries[]
    | "\(.key)\t\(.value.user // .key)\t\(.value.ports.gateway // 0)\t\(.value.ports.bridge // 0)"
  ' "$registry" 2>/dev/null)
  if [[ ${#targets[@]} -eq 0 ]]; then
    printf '{"component":"runtime","status":"degraded","timestamp":"%s","metrics":{"enabled":true,"discovery":"ports-registry","target_count":0,"instances":[]},"issues":["tenant registry lists no tenants with a gateway port; runtime diagnostics unavailable"]}\n' "$TIMESTAMP"
    exit 1
  fi
else
  base="${LUNARWING_BASE_DIR:-${IRONCLAW_BASE_DIR:-$HOME/.lunarwing}}"
  env_file="$base/.env"
  if [[ "$ENABLED" == true || -r "$env_file" ]]; then
    targets+=("default|$DEFAULT_GATEWAY_PORT|$DEFAULT_BRIDGE_PORT|$env_file")
  else
    printf '{"component":"runtime","status":"healthy","timestamp":"%s","metrics":{"enabled":false,"reason":"not_configured","discovery":"single","target_count":0,"instances":[]},"issues":[]}\n' "$TIMESTAMP"
    exit 0
  fi
fi

for target in "${targets[@]}"; do
  if [[ "$SWEEP_DEADLINE_SECS" != "0" ]] && [[ "$SECONDS" -ge "$SWEEP_DEADLINE_SECS" ]]; then
    sweep_incomplete=true
    unprobed_count=$((unprobed_count + 1))
    instances="$(jq -c --arg tenant "${target%%|*}" \
      '. + [{tenant:$tenant,status:"unprobed",issues:["probe skipped: sweep deadline"]}]' \
      <<<"$instances")"
    continue
  fi
  tenant="${target%%|*}"; rest="${target#*|}"
  gw_port="${rest%%|*}"; rest="${rest#*|}"
  bridge_port="${rest%%|*}"; env_file="${rest#*|}"
  probe_one "$tenant" "$gw_port" "$bridge_port" "$env_file"

  # Budget exhausted mid-probe: the instance is UNPROBED, not unhealthy — a
  # skipped request must not fabricate an "unreachable" verdict (MR !1041
  # review finding 3).
  if [[ "$p_probe_exhausted" == "true" ]]; then
    sweep_incomplete=true
    unprobed_count=$((unprobed_count + 1))
    instances="$(jq -c --arg tenant "$tenant" \
      '. + [{tenant:$tenant,status:"unprobed",issues:["probe skipped: sweep deadline"]}]' \
      <<<"$instances")"
    continue
  fi

  inst_status="healthy"; inst_exit=0
  if [[ "$p_gw_reachable" != "true" ]]; then
    inst_status="critical"; inst_exit=2
  elif [[ "$p_bridge_reachable" != "true" || "$p_bridge_running" != "true" ]]; then
    inst_status="degraded"; inst_exit=1
  elif [[ "$p_bridge_queued" =~ ^[0-9]+$ ]] && [[ "$p_bridge_queued" -gt "$QUEUE_CRITICAL" ]]; then
    inst_status="critical"; inst_exit=2
    p_issues+=("xmpp bridge delivery backlog critical: ${p_bridge_queued} queued")
  elif [[ "$p_gw_channels_unhealthy" =~ ^[0-9]+$ ]] && [[ "$p_gw_channels_unhealthy" -gt 0 ]]; then
    # MR !1041 review finding 5: channels the gateway itself reports unhealthy
    # must never produce a healthy verdict. A normal approval wait stays
    # metric-only (pending_gates), but failed channel health is a real
    # delivery failure.
    inst_status="degraded"; inst_exit=1
    p_issues+=("gateway channels unhealthy: ${p_gw_channels_unhealthy} channel(s) reporting unhealthy")
  elif [[ "$p_pending_gates" == "-1" ]]; then
    # The pending-gates endpoint is part of the current gateway contract; a
    # reachable gateway that cannot answer it is an observability gap, not a
    # normal approval wait (MR !1041 review finding 5).
    inst_status="degraded"; inst_exit=1
    p_issues+=("pending-gates diagnostics unavailable (gateway reachable at 127.0.0.1:${gw_port} but /api/chat/pending-gates did not answer)")
  elif [[ "$p_bridge_queued" =~ ^[0-9]+$ ]] && [[ "$p_bridge_queued" -gt "$QUEUE_DEGRADED" ]]; then
    inst_status="degraded"; inst_exit=1
    p_issues+=("xmpp bridge delivery backlog elevated: ${p_bridge_queued} queued")
  fi
  if [[ "$inst_exit" -gt "$exit_code" ]]; then
    exit_code="$inst_exit"
    overall="$([[ "$inst_exit" -eq 2 ]] && echo critical || echo degraded)"
  fi

  iss_json="[]"
  [[ ${#p_issues[@]} -eq 0 ]] || iss_json="$(printf '%s\n' "${p_issues[@]}" | jq -R . | jq -s .)"
  inst="$(jq -n \
    --arg tenant "$tenant" \
    --arg status "$inst_status" \
    --arg gw_reachable "$p_gw_reachable" \
    --arg bridge_reachable "$p_bridge_reachable" \
    --arg bridge_running "$p_bridge_running" \
    --arg bridge_omemo "$p_bridge_omemo" \
    --argjson gw_uptime "$([[ "$p_gw_uptime" =~ ^[0-9]+$ ]] && echo "$p_gw_uptime" || echo 0)" \
    --argjson gw_connections "$([[ "$p_gw_connections" =~ ^[0-9]+$ ]] && echo "$p_gw_connections" || echo 0)" \
    --argjson channels_unhealthy "$([[ "$p_gw_channels_unhealthy" =~ ^[0-9]+$ ]] && echo "$p_gw_channels_unhealthy" || echo 0)" \
    --argjson pending_gates "$([[ "$p_pending_gates" =~ ^[0-9]+$ ]] && echo "$p_pending_gates" || echo -1)" \
    --argjson bridge_queued "$([[ "$p_bridge_queued" =~ ^[0-9]+$ ]] && echo "$p_bridge_queued" || echo -1)" \
    --argjson bridge_outbound_hour "$([[ "$p_bridge_outbound_hour" =~ ^[0-9]+$ ]] && echo "$p_bridge_outbound_hour" || echo -1)" \
    --argjson bridge_prekeys "$([[ "$p_bridge_prekeys" =~ ^[0-9]+$ ]] && echo "$p_bridge_prekeys" || echo -1)" \
    --argjson issues "$iss_json" \
    '{tenant:$tenant,status:$status,gateway_reachable:($gw_reachable=="true"),bridge_reachable:($bridge_reachable=="true"),
      bridge_running:($bridge_running=="true"),bridge_omemo_enabled:($bridge_omemo=="true"),
      gateway_uptime_secs:$gw_uptime,gateway_connections:$gw_connections,
      channels_unhealthy:$channels_unhealthy,pending_gates:$pending_gates,
      bridge_queued_messages:$bridge_queued,bridge_outbound_last_hour:$bridge_outbound_hour,
      bridge_prekeys_available:$bridge_prekeys,issues:$issues}')"
  instances="$(jq -c --argjson item "$inst" '. + [$item]' <<<"$instances")"
  issues="$(jq -c --argjson more "$iss_json" '. + $more' <<<"$issues")"
done

if [[ "$sweep_incomplete" == "true" ]]; then
  issues="$(jq -c --arg msg "target sweep incomplete: $unprobed_count target(s) unprobed (deadline ${SWEEP_DEADLINE_SECS}s); results partial" \
    '. + [$msg]' <<<"$issues")"
  if [[ "$exit_code" -lt 1 ]]; then
    overall="degraded"; exit_code=1
  fi
fi

jq -n \
  --arg status "$overall" \
  --arg timestamp "$TIMESTAMP" \
  --arg discovery "$discovery" \
  --argjson instances "$instances" \
  --argjson issues "$issues" \
  '{component:"runtime",status:$status,timestamp:$timestamp,
    metrics:{enabled:true,discovery:$discovery,target_count:($instances|length),instances:$instances},issues:$issues}'
exit "$exit_code"
