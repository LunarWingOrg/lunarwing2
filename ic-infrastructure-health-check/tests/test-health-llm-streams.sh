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
      "repl_argument_corruption_total": 4,
      "ttfc_samples_total": 6,
      "ttfc_sum_ms": 900,
      "ttfc_max_ms": 400,
      "ttfc_last_ms": 125,
      "last_issue_unix_seconds": $last_issue,
      "retry_reasons": {"decode": 2, "rate_limited": 1, "empty": 5},
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
assert_eq "$(jq -r '.metrics.providers[0].retry_reason_empty_total' <<<"$OUT")" "5" "S2: empty retry reason exposed (WI#297)"
assert_eq "$(jq -r '.metrics.providers[0].ttfc_last_ms' <<<"$OUT")" "125" "S2: time-to-first-chunk exposed"

recent="$(date +%s)"
write_snapshot "$snapshot" "$recent"
run_check HEALTH_LLM_STREAMS_FILE="$snapshot" HEALTH_LLM_STREAMS_TENANT=alpha HEALTH_LLM_STREAMS_ISSUE_WINDOW_SECS=900
assert_eq "$RC" "1" "S3: recent provider issue exits degraded"
assert_eq "$(jq -r '.status' <<<"$OUT")" "degraded" "S3: recent issue status degraded"
assert_eq "$(jq -r '.metrics.instances[0].corruption_total' <<<"$OUT")" "1" "S3: tenant corruption aggregate exposed"
assert_eq "$(jq -r '.metrics.instances[0].repl_argument_corruption_total' <<<"$OUT")" "4" "S3: tenant REPL corruption aggregate exposed"

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
assert_contains "$exposition" 'lunarwing_health_llm_streams_retry_reason_empty_total{provider="tensorzero::function_name::FrontierTEE/P",tenant="alpha"} 5' "S5: empty retry reason exports (WI#297)"
assert_contains "$exposition" 'lunarwing_health_llm_streams_decode_failures_total{provider="tensorzero::function_name::FrontierTEE/P",tenant="alpha"} 2' "S5: decode failures export"
assert_contains "$exposition" 'lunarwing_health_llm_streams_corruption_repeated_phrase_total{provider="tensorzero::function_name::FrontierTEE/P",tenant="alpha"} 1' "S5: corruption signature exports"
assert_contains "$exposition" 'lunarwing_health_llm_streams_repl_argument_corruption_total{provider="tensorzero::function_name::FrontierTEE/P",tenant="alpha"} 4' "S5: REPL corruption exports with tenant and provider labels"
assert_contains "$exposition" 'lunarwing_health_llm_streams_ttfc_max_ms{provider="tensorzero::function_name::FrontierTEE/P",tenant="alpha"} 400' "S5: TTFC exports"

# ── S6: collector → driver → saved report → exporter (WI#180 F2) ─────────────
# The driver previously dropped check_llm_streams from the final serialized
# components array: the report degraded with an llm_streams alert but no stream
# component, and the exporter emitted zero llm_streams metric lines.
driver_dir="$ROOT/driver"
mkdir -p "$driver_dir"
cp "$DIR/../infrastructure-health-check.sh" "$driver_dir/"
cp "$CHECK" "$driver_dir/health-llm-streams.sh"
for stub in gateway xmpp omemo ratelimit clickhouse tensorzero models lunarvision lunarvoice netbird; do
    cat >"$driver_dir/health-$stub.sh" <<EOF
#!/usr/bin/env bash
printf '{"component":"$stub","status":"healthy","metrics":{}}'
EOF
done
cat >"$driver_dir/health-systemd.sh" <<'EOF'
#!/usr/bin/env bash
printf '{"component":"systemd","status":"healthy","metrics":{}}'
EOF
cat >"$driver_dir/send-notification.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$driver_dir"/*.sh
recent="$(date +%s)"
write_snapshot "$snapshot" "$recent"
driver_report_dir="$ROOT/driver-reports"
mkdir -p "$driver_report_dir"
env HEALTH_LLM_STREAMS_FILE="$snapshot" HEALTH_LLM_STREAMS_TENANT=alpha \
    LUNARWING_BASE_DIR="$ROOT/driver-base" HEALTHCHECK_NOTIFY=false \
    bash "$driver_dir/infrastructure-health-check.sh" >"$ROOT/driver-out.json" 2>/dev/null
driver_rc=$?
# The driver writes reports under <base>/workspace/reports/health — copy the
# saved report so the exporter reads the REAL driver output (not a hand-built one).
saved_report="$(ls "$ROOT/driver-base/workspace/reports/health"/*.json 2>/dev/null | tail -1)"
assert_eq "$driver_rc" "1" "S6: driver exits degraded with a recent stream issue"
assert_contains "$(jq -r '.components[] | select(.component=="llm_streams") | .status' "$saved_report" 2>/dev/null)" "degraded" \
    "S6: llm_streams component serialized in the saved report"
assert_eq "$(jq -r '[.components[] | select(.component=="llm_streams")] | length' "$saved_report" 2>/dev/null)" "1" \
    "S6: exactly one llm_streams component in the saved report"
assert_contains "$(jq -r '.alerts[].message' "$saved_report" 2>/dev/null)" "llm_streams is degraded" \
    "S6: llm_streams alert preserved"
HEALTH_REPORT_DIR="$ROOT/driver-base/workspace/reports/health" HEALTH_METRICS_OUTPUT="$ROOT/driver-metrics.prom" \
    "$EXPORTER" >/dev/null 2>&1
driver_exposition="$(cat "$ROOT/driver-metrics.prom")"
assert_contains "$driver_exposition" 'lunarwing_health_llm_streams_repl_argument_corruption_total{provider="tensorzero::function_name::FrontierTEE/P",tenant="alpha"} 4' \
    "S6: exporter emits REPL corruption from the driver-saved report"
assert_contains "$driver_exposition" 'lunarwing_health_llm_streams_retries_total{provider="tensorzero::function_name::FrontierTEE/P",tenant="alpha"} 3' \
    "S6: exporter emits stream counters from the driver-saved report"
assert_contains "$driver_exposition" 'lunarwing_health_status{component="llm_streams"} 1' \
    "S6: llm_streams component status exported"

# ── S7: driver keeps partial JSON when a component hits the outer timeout ─────
# (WI#180 F4). A slow openrc component emits its partial report (completed
# observations + unprobed entries) before the driver's outer timeout kills it;
# the driver must keep that JSON instead of replacing it with unknown/timeout.
slow_dir="$ROOT/slow-driver"
mkdir -p "$slow_dir"
cp "$DIR/../infrastructure-health-check.sh" "$slow_dir/"
cat >"$slow_dir/health-openrc.sh" <<'EOF'
#!/usr/bin/env bash
# Emit a valid partial report immediately, then stall past the outer timeout.
printf '%s\n' '{"component":"openrc","status":"degraded","metrics":{"services":[{"name":"lunarwing-acme","state":"started","status":"healthy"},{"name":"lunarwing-pg-acme","state":"unprobed","status":"skipped"}]},"issues":["service sweep incomplete: 1 service(s) unprobed"]}'
sleep 60
EOF
chmod +x "$slow_dir/health-openrc.sh"
for stub in gateway xmpp omemo ratelimit clickhouse tensorzero models llm-streams lunarvision lunarvoice netbird; do
    cat >"$slow_dir/health-$stub.sh" <<EOF
#!/usr/bin/env bash
printf '{"component":"$stub","status":"healthy","metrics":{}}'
EOF
done
cat >"$slow_dir/send-notification.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$slow_dir"/*.sh
slow_report_dir="$ROOT/slow-reports"
mkdir -p "$slow_report_dir"
env LUNARWING_SERVICE_MANAGER=openrc LUNARWING_BASE_DIR="$ROOT/slow-base" \
    HEALTHCHECK_NOTIFY=false HEALTH_CHECK_TIMEOUT_SECS=3 \
    bash "$slow_dir/infrastructure-health-check.sh" >"$ROOT/slow-out.json" 2>/dev/null
slow_rc=$?
slow_saved="$(ls "$ROOT/slow-base/workspace/reports/health"/*.json 2>/dev/null | tail -1)"
assert_eq "$slow_rc" "1" "S7: driver exits degraded on partial timeout"
assert_eq "$(jq -r '.components[] | select(.component=="openrc") | .status' "$slow_saved" 2>/dev/null)" "degraded" \
    "S7: partial openrc JSON retained in the saved report"
assert_eq "$(jq -r '.components[] | select(.component=="openrc") | .metrics.services[0].name' "$slow_saved" 2>/dev/null)" "lunarwing-acme" \
    "S7: completed service observation retained"
assert_eq "$(jq -r '.components[] | select(.component=="openrc") | .metrics.services[1].state' "$slow_saved" 2>/dev/null)" "unprobed" \
    "S7: unprobed service entry retained"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
