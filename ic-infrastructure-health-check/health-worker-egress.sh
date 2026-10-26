#!/usr/bin/env bash
# WI#424: read-only, host-side worker egress audit for the health pipeline.
set -uo pipefail

TENANTS_FILE="${SELF_HEAL_TENANTS_FILE:-${LUNARWING_TENANTS_FILE:-/etc/lunarwing/ports.json}}"
AUDIT_TIMEOUT="${HEALTH_WORKER_EGRESS_TIMEOUT:-8}"
instances=()
issues=()

json_array() {
  if [[ $# -eq 0 ]]; then
    printf '[]\n'
  else
    printf '%s\n' "$@" | jq -s .
  fi
}

json_issues() {
  if [[ $# -eq 0 ]]; then
    printf '[]\n'
  else
    printf '%s\n' "$@" | jq -R . | jq -s .
  fi
}

report() {
  local status="$1" rc="$2"
  jq -n \
    --arg component "worker_egress" \
    --arg status "$status" \
    --arg timestamp "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" \
    --argjson instances "$(json_array "${instances[@]}")" \
    --argjson issues "$(json_issues "${issues[@]}")" \
    '{component: $component, status: $status, timestamp: $timestamp,
      metrics: {instances: $instances}, issues: $issues}'
  exit "$rc"
}

if ! command -v jq >/dev/null 2>&1; then
  printf '{"component":"worker_egress","status":"degraded","metrics":{"instances":[]},"issues":["jq is unavailable"]}\n'
  exit 1
fi

MT_ADMIN="${LUNARWING_MT_ADMIN:-}"
if [[ -z "$MT_ADMIN" ]]; then
  if command -v lunarwing-mt-admin >/dev/null 2>&1; then
    MT_ADMIN="$(command -v lunarwing-mt-admin)"
  elif [[ -x /usr/local/bin/lunarwing-mt-admin.sh ]]; then
    MT_ADMIN=/usr/local/bin/lunarwing-mt-admin.sh
  else
    issues+=("lunarwing-mt-admin is unavailable; worker egress cannot be audited")
    report degraded 1
  fi
fi
if [[ ! -x "$MT_ADMIN" ]]; then
  issues+=("worker egress audit command is not executable: $MT_ADMIN")
  report degraded 1
fi

if [[ ! -r "$TENANTS_FILE" ]]; then
  report healthy 0
fi
if ! tenants="$(jq -er '
  .tenants // {} | to_entries[]
  | select((.value.workers.pebble == true) or (.value.workers.opencode == true)
      or (.value.workers.opencode2 == true) or (.value.workers.kimi == true))
  | .key
' "$TENANTS_FILE" 2>/dev/null)"; then
  issues+=("tenant registry is invalid; worker egress coverage is unknown")
  report degraded 1
fi

while IFS= read -r tenant; do
  [[ -n "$tenant" ]] || continue
  audit_rc=0
  if command -v timeout >/dev/null 2>&1; then
    audit_output="$(timeout -k 2 "$AUDIT_TIMEOUT" "$MT_ADMIN" worker-egress "$tenant" 2>&1)" || audit_rc=$?
  else
    audit_output="$("$MT_ADMIN" worker-egress "$tenant" 2>&1)" || audit_rc=$?
  fi
  if [[ "$audit_rc" == "0" ]]; then
    instances+=("$(jq -n --arg tenant "$tenant" '{tenant: $tenant, status: "healthy"}')")
  else
    printf '%s\n' "$audit_output" >&2
    instances+=("$(jq -n --arg tenant "$tenant" '{tenant: $tenant, status: "critical"}')")
    issues+=("worker egress is missing or has a stale contract for $tenant; run $MT_ADMIN worker-egress $tenant")
  fi
done <<<"$tenants"

for instance in "${instances[@]}"; do
  [[ "$(jq -r '.status' <<<"$instance")" != "critical" ]] || report critical 2
done
report healthy 0
