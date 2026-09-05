#!/usr/bin/env bash
# Health Check: LLM provider stream integrity telemetry
# Output: JSON to stdout; exit 0=healthy, 1=recent issue or malformed snapshot

set -uo pipefail

BASE_DIR="${LUNARWING_BASE_DIR:-${IRONCLAW_BASE_DIR:-$HOME/.lunarwing}}"
REGISTRY="${SELF_HEAL_TENANTS_FILE:-/etc/lunarwing/ports.json}"
ISSUE_WINDOW_SECS="${HEALTH_LLM_STREAMS_ISSUE_WINDOW_SECS:-900}"
[[ "$ISSUE_WINDOW_SECS" =~ ^[0-9]+$ ]] || ISSUE_WINDOW_SECS=900
now="$(date +%s)"
instances='[]'
providers='[]'
issues='[]'
malformed=0

add_snapshot() {
    local tenant="$1" file="$2" snapshot recent=0
    [[ -f "$file" ]] || return 0
    if ! snapshot="$(jq -c . "$file" 2>/dev/null)"; then
        issues="$(jq -c --arg tenant "$tenant" --arg file "$file" \
            '. + ["invalid stream telemetry snapshot for " + $tenant + ": " + $file]' <<<"$issues")"
        malformed=1
        return 0
    fi

    local last_issue
    last_issue="$(jq -r '[.providers[]?.last_issue_unix_seconds // 0] | max // 0' <<<"$snapshot")"
    if [[ "$last_issue" =~ ^[0-9]+$ ]] \
        && (( last_issue > 0 && now >= last_issue && now - last_issue <= ISSUE_WINDOW_SECS )); then
        recent=1
    fi

    local instance
    instance="$(jq -c \
        --arg tenant "$tenant" \
        --arg status "$([[ "$recent" -eq 1 ]] && printf degraded || printf healthy)" '
        {
          tenant: $tenant,
          status: $status,
          streams_started_total: ([.providers[]?.streams_started_total // 0] | add // 0),
          streams_completed_total: ([.providers[]?.streams_completed_total // 0] | add // 0),
          retries_total: ([.providers[]?.retries_total // 0] | add // 0),
          stream_aborts_total: ([.providers[]?.stream_aborts_total // 0] | add // 0),
          decode_failures_total: ([.providers[]?.decode_failures_total // 0] | add // 0),
          corruption_total: ([.providers[]?.corruption_total // 0] | add // 0),
          last_issue_unix_seconds: ([.providers[]?.last_issue_unix_seconds // 0] | max // 0)
        }' <<<"$snapshot")"
    instances="$(jq -c --argjson item "$instance" '. + [$item]' <<<"$instances")"

    local provider_rows
    provider_rows="$(jq -c --arg tenant "$tenant" '
        [.providers | to_entries[]? | {
          tenant: $tenant,
          provider: .key,
          streams_started_total: (.value.streams_started_total // 0),
          streams_completed_total: (.value.streams_completed_total // 0),
          retries_total: (.value.retries_total // 0),
          stream_aborts_total: (.value.stream_aborts_total // 0),
          decode_failures_total: (.value.decode_failures_total // 0),
          corruption_total: (.value.corruption_total // 0),
          ttfc_samples_total: (.value.ttfc_samples_total // 0),
          ttfc_sum_ms: (.value.ttfc_sum_ms // 0),
          ttfc_max_ms: (.value.ttfc_max_ms // 0),
          ttfc_last_ms: (.value.ttfc_last_ms // 0),
          last_issue_unix_seconds: (.value.last_issue_unix_seconds // 0),
          retry_reason_decode_total: (.value.retry_reasons.decode // 0),
          retry_reason_rate_limited_total: (.value.retry_reasons.rate_limited // 0),
          retry_reason_timeout_total: (.value.retry_reasons.timeout // 0),
          retry_reason_transport_total: (.value.retry_reasons.transport // 0),
          abort_reason_decode_total: (.value.abort_reasons.decode // 0),
          abort_reason_timeout_total: (.value.abort_reasons.timeout // 0),
          abort_reason_transport_total: (.value.abort_reasons.transport // 0),
          abort_reason_empty_total: (.value.abort_reasons.empty // 0),
          abort_reason_missing_done_total: (.value.abort_reasons.missing_done // 0),
          corruption_repeated_phrase_total: (.value.corruption_signatures.repeated_phrase // 0),
          corruption_role_delimiter_total: (.value.corruption_signatures.role_delimiter // 0)
        }]' <<<"$snapshot")"
    providers="$(jq -c --argjson rows "$provider_rows" '. + $rows' <<<"$providers")"
}

if [[ -n "${HEALTH_LLM_STREAMS_FILE:-}" ]]; then
    add_snapshot "${HEALTH_LLM_STREAMS_TENANT:-default}" "$HEALTH_LLM_STREAMS_FILE"
elif [[ -r "$REGISTRY" ]] && jq -e '.tenants | type == "object" and length > 0' "$REGISTRY" >/dev/null 2>&1; then
    while IFS=$'\t' read -r tenant user; do
        [[ -n "$tenant" ]] || continue
        user="${user:-$tenant}"
        add_snapshot "$tenant" "/home/$user/lunarwing/state/runtime/llm-stream-metrics.json"
    done < <(jq -r '.tenants | to_entries[] | [.key, (.value.user // .key)] | @tsv' "$REGISTRY")
else
    add_snapshot "default" "$BASE_DIR/runtime/llm-stream-metrics.json"
fi

recent_count="$(jq '[.[] | select(.status == "degraded")] | length' <<<"$instances")"
status="healthy"
exit_code=0
if (( malformed > 0 || recent_count > 0 )); then
    status="degraded"
    exit_code=1
fi

jq -n \
    --arg status "$status" \
    --arg timestamp "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --argjson instances "$instances" \
    --argjson providers "$providers" \
    --argjson issues "$issues" \
    --argjson issue_window_secs "$ISSUE_WINDOW_SECS" \
    '{
      component: "llm_streams",
      status: $status,
      timestamp: $timestamp,
      metrics: {
        enabled: true,
        tenant_count: ($instances | length),
        provider_count: ($providers | length),
        recent_issue_tenants: ([$instances[] | select(.status == "degraded")] | length),
        issue_window_seconds: $issue_window_secs,
        instances: $instances,
        providers: $providers
      },
      issues: $issues
    }'

exit "$exit_code"
