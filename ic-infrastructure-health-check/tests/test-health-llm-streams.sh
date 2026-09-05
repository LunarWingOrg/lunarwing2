#!/usr/bin/env bash
# LLM stream integrity healthpoint and Prometheus export tests.

set -uo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$DIR/../health-llm-streams.sh"
EXPORTER="$DIR/../health-metrics-exporter.sh"
command -v jq >/dev/null 2>&1 || { echo "FATAL: jq required"; exit 1; }

ROOT="$(mktemp -d "${TMPDIR:-/tmp}/llm-stream-health.XXXXXX")"
trap 'rm -rf "$ROOT"' EXIT

pass=0; fail=0; OUT=""; RC=0
ok() { printf 'PASS: %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL: %s\n      %s\n' "$1" "$2"; fail=$((fail + 1)); }
assert_eq() { if [[ "$1" == "$2" ]]; then ok "$3"; else bad "$3" "got [$1] want [$2]"; fi; }
assert_contains() { if grep -qF -- "$2" <<<"$1"; then ok "$3"; else bad "$3" "expected substring: $2"; fi; }

write_snapshot() {
    local path="$1" last_issue="$2"
    mkdir -p "$(dirname "$path")"
    cat > "$path" <<JSON
{
  "schema_version": 1,
  "updated_at_unix_seconds": $last_issue,
  "providers": {
    "tensorzero::function_name::FrontierTEE/P": {
      "streams_started_total": 8,
      "streams_completed_total": 6,
      "retries_total": 3,
      "stream_aborts_total": 2,
      "decode_failures_total": 2,
      "corruption_total": 1,
      "ttfc_samples_total": 6,
      "ttfc_sum_ms": 900,
      "ttfc_max_ms": 400,
      "ttfc_last_ms": 125,
      "last_issue_unix_seconds": $last_issue,
      "retry_reasons": {"decode": 2, "rate_limited": 1},
      "abort_reasons": {"decode": 2},
      "corruption_signatures": {"repeated_phrase": 1}
    }
  }
}
JSON
}

run_check() {
    OUT="$(env "$@" "$CHECK" 2>/dev/null)"
    RC=$?
}

echo "=== LLM stream integrity healthpoint ==="
missing="$ROOT/missing.json"
run_check HEALTH_LLM_STREAMS_FILE="$missing"
assert_eq "$RC" "0" "S1: absent snapshot is non-failing"
assert_eq "$(jq -r '.metrics.provider_count' <<<"$OUT")" "0" "S1: absent snapshot has no providers"

old=$(( $(date +%s) - 3600 ))
snapshot="$ROOT/metrics.json"
write_snapshot "$snapshot" "$old"
run_check HEALTH_LLM_STREAMS_FILE="$snapshot" HEALTH_LLM_STREAMS_TENANT=alpha HEALTH_LLM_STREAMS_ISSUE_WINDOW_SECS=900
assert_eq "$RC" "0" "S2: old issue exits healthy"
assert_eq "$(jq -r '.metrics.providers[0].provider' <<<"$OUT")" "tensorzero::function_name::FrontierTEE/P" "S2: provider retained as data"
assert_eq "$(jq -r '.metrics.providers[0].retry_reason_decode_total' <<<"$OUT")" "2" "S2: retry reason exposed"
assert_eq "$(jq -r '.metrics.providers[0].ttfc_last_ms' <<<"$OUT")" "125" "S2: time-to-first-chunk exposed"

recent="$(date +%s)"
write_snapshot "$snapshot" "$recent"
run_check HEALTH_LLM_STREAMS_FILE="$snapshot" HEALTH_LLM_STREAMS_TENANT=alpha HEALTH_LLM_STREAMS_ISSUE_WINDOW_SECS=900
assert_eq "$RC" "1" "S3: recent provider issue exits degraded"
assert_eq "$(jq -r '.status' <<<"$OUT")" "degraded" "S3: recent issue status degraded"
assert_eq "$(jq -r '.metrics.instances[0].corruption_total' <<<"$OUT")" "1" "S3: tenant corruption aggregate exposed"

printf '{not-json' > "$snapshot"
run_check HEALTH_LLM_STREAMS_FILE="$snapshot" HEALTH_LLM_STREAMS_TENANT=alpha
assert_eq "$RC" "1" "S4: malformed snapshot exits degraded"
assert_contains "$(jq -r '.issues[0]' <<<"$OUT")" "invalid stream telemetry snapshot" "S4: malformed snapshot explained"

printf '{"schema_version":99,"providers":{}}\n' > "$snapshot"
run_check HEALTH_LLM_STREAMS_FILE="$snapshot" HEALTH_LLM_STREAMS_TENANT=alpha
assert_eq "$RC" "1" "S4: unknown snapshot schema exits degraded"

write_snapshot "$snapshot" "$old"
report_dir="$ROOT/reports"
mkdir -p "$report_dir"
env HEALTH_LLM_STREAMS_FILE="$snapshot" HEALTH_LLM_STREAMS_TENANT=alpha \
    "$CHECK" > "$ROOT/component.json"
jq -n --arg timestamp "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --slurpfile component "$ROOT/component.json" \
    '{timestamp:$timestamp,overall_status:"healthy",components:$component,alerts:[]}' \
    > "$report_dir/2026-09-05T00:00:00Z.json"
HEALTH_REPORT_DIR="$report_dir" HEALTH_METRICS_OUTPUT="$ROOT/metrics.prom" \
    "$EXPORTER" >/dev/null 2>&1
exposition="$(cat "$ROOT/metrics.prom")"
assert_contains "$exposition" 'lunarwing_health_llm_streams_retries_total{provider="tensorzero::function_name::FrontierTEE/P",tenant="alpha"} 3' "S5: retries export with provider and tenant labels"
assert_contains "$exposition" 'lunarwing_health_llm_streams_decode_failures_total{provider="tensorzero::function_name::FrontierTEE/P",tenant="alpha"} 2' "S5: decode failures export"
assert_contains "$exposition" 'lunarwing_health_llm_streams_corruption_repeated_phrase_total{provider="tensorzero::function_name::FrontierTEE/P",tenant="alpha"} 1' "S5: corruption signature exports"
assert_contains "$exposition" 'lunarwing_health_llm_streams_ttfc_max_ms{provider="tensorzero::function_name::FrontierTEE/P",tenant="alpha"} 400' "S5: TTFC exports"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
