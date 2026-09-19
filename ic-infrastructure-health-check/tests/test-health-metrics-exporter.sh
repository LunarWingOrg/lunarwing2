#!/usr/bin/env bash
# Test suite for health-metrics-exporter.sh (Prometheus textfile exporter).
#
# Uses the same harness discipline as the other suites: synthetic reports in a
# throwaway sandbox, no real services, no network. The exporter is read-only,
# so nothing here can mutate a service.

set -uo pipefail

LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXPORTER="${HEALTH_METRICS_EXPORTER_SCRIPT:-$LIB_DIR/../health-metrics-exporter.sh}"

fail=0; pass=0
ok()  { printf 'PASS: %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL: %s\n      %s\n' "$1" "$2"; fail=$((fail + 1)); }
assert_contains() { if grep -qF -- "$2" <<<"$1"; then ok "$3"; else bad "$3" "expected substring: $2"; fi; }
assert_absent()   { if grep -qF -- "$2" <<<"$1"; then bad "$3" "unexpected substring: $2"; else ok "$3"; fi; }
assert_eq()       { if [[ "$1" == "$2" ]]; then ok "$3"; else bad "$3" "got [$1] want [$2]"; fi; }
assert_ne()       { if [[ "$1" != "$2" ]]; then ok "$3"; else bad "$3" "got [$1], expected it to differ"; fi; }
finish() { printf '\n%d passed, %d failed\n' "$pass" "$fail"; [[ "$fail" -eq 0 ]]; }

[[ -x "$EXPORTER" ]] || { echo "FATAL: exporter not found at $EXPORTER"; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "FATAL: jq required"; exit 1; }

ROOT="$(mktemp -d "${TMPDIR:-/tmp}/hmx-tests.XXXXXX")"
trap 'rm -rf "$ROOT"' EXIT

# run <sb> [ENV=VAL ...] — invoke the exporter against sandbox <sb>; sets OUT.
# EXPORTER is resolved per call so tests can shadow it (e.g. the notify tests
# copy it next to a fake send-notification.sh).
run() {
    local sb="$1"; shift
    local exporter="${HEALTH_METRICS_EXPORTER_SCRIPT:-$EXPORTER}"
    OUT="$(env LUNARWING_BASE_DIR="$sb" HEALTH_REPORT_DIR="$sb/reports/health" \
        HEALTH_METRICS_SELFHEAL_STATE="$sb/self-heal/state.json" \
        HEALTH_METRICS_OUTPUT="$sb/reports/health/metrics.prom" \
        "$@" "$exporter" 2>"$ROOT/stderr.log")"
    RC=$?
    PROM="$(cat "$sb/reports/health/metrics.prom" 2>/dev/null || true)"
}

# mkreport <sb> <filename> — capture a JSON object on stdin as a report
mkreport() { mkdir -p "$1/reports/health" "$1/self-heal"; cat > "$1/reports/health/$2"; }
mksb() { local d; d="$(mktemp -d "$ROOT/sb.XXXXXX")"; printf '%s' "$d"; }

# ── 1. Valid exposition format from a synthetic report ──────────────────────
sb="$(mksb)"
mkreport "$sb" "2026-08-15T10:00:00Z.json" <<'EOF'
{"timestamp":"2026-08-15T10:00:00Z","overall_status":"healthy",
 "components":[
  {"component":"tensorzero","status":"healthy","duration_seconds":3,
   "metrics":{"p50_ms":"120","p95_ms":812,"error_rate":0.004,"queue_depth":0}},
  {"component":"xmpp","status":"healthy","metrics":{"connected":true,"latency_ms":7,"error_count":0}}
 ],
 "alerts":[]}
EOF
run "$sb"
assert_eq "$RC" "0" "exporter exits 0 on a valid report"
assert_contains "$PROM" 'lunarwing_health_overall_status 0' "overall status healthy -> 0"
assert_contains "$PROM" 'lunarwing_health_tensorzero_p95_ms 812' "numeric metric flattened"
assert_contains "$PROM" 'lunarwing_health_tensorzero_p50_ms 120' "numeric STRING metric coerced"
assert_contains "$PROM" 'lunarwing_health_xmpp_connected 1' "boolean true -> 1"
assert_contains "$PROM" 'lunarwing_health_check_duration_seconds{component="tensorzero"} 3' "duration exported"
assert_contains "$PROM" 'lunarwing_health_report_timestamp_seconds ' "report timestamp exported"
assert_contains "$PROM" 'lunarwing_health_alerts_total 0' "alerts count exported"

# Strict line-format check: every non-comment line is a valid sample.
format_bad="$(grep -vE '^(#|$)' "$sb/reports/health/metrics.prom" \
    | grep -cvE '^[a-zA-Z_:][a-zA-Z0-9_:]*(\{[^"}]*"[^"}*"]*"[^}]*\})? -?[0-9]+(\.[0-9]+)?[[:space:]]*$')"
assert_eq "$format_bad" "0" "every sample line matches the exposition format"

# Naming schema check: every metric starts with lunarwing_health_.
naming_bad="$(grep -vE '^(#|$)' "$sb/reports/health/metrics.prom" \
    | awk '{print $1}' | sed 's/{.*//' | grep -cv '^lunarwing_health_')"
assert_eq "$naming_bad" "0" "every metric name starts with lunarwing_health_"

# TYPE lines appear before their family's samples.
type_first="$(awk '/^# TYPE lunarwing_health_overall_status gauge/{t=NR}
    /^lunarwing_health_overall_status /{s=NR} END{print (t<s) ? 1 : 0}' "$sb/reports/health/metrics.prom")"
assert_eq "$type_first" "1" "TYPE line precedes samples"
mode="$(stat -c '%a' "$sb/reports/health/metrics.prom")"
assert_eq "$mode" "644" ".prom written mode 0644 (textfile-collector convention; non-secret series)"

# No secret material: URLs and strings from metrics must never appear.
assert_absent "$PROM" "http://" "no URL leaks into exposition"

# ── 2. Status mapping 0/1/2/3 incl. unknown ─────────────────────────────────
sb="$(mksb)"
mkreport "$sb" "2026-08-15T10:00:00Z.json" <<'EOF'
{"timestamp":"2026-08-15T10:00:00Z","overall_status":"critical",
 "components":[
  {"component":"gateway","status":"degraded","metrics":{}},
  {"component":"clickhouse","status":"critical","metrics":{}},
  {"component":"omemo","status":"unknown","metrics":{}}
 ], "alerts":[{"component":"clickhouse","severity":"critical","message":"x"}]}
EOF
run "$sb"
assert_contains "$PROM" 'lunarwing_health_overall_status 2' "overall status critical -> 2"
assert_contains "$PROM" 'lunarwing_health_status{component="gateway"} 1' "degraded -> 1"
assert_contains "$PROM" 'lunarwing_health_status{component="clickhouse"} 2' "critical -> 2"
assert_contains "$PROM" 'lunarwing_health_status{component="omemo"} 3' "unknown -> 3"
assert_contains "$PROM" 'lunarwing_health_alerts_total 1' "alerts counted"

# ── 3. Disabled components: enabled 0, no status series ────────────────────
sb="$(mksb)"
mkreport "$sb" "2026-08-15T10:00:00Z.json" <<'EOF'
{"timestamp":"2026-08-15T10:00:00Z","overall_status":"healthy",
 "components":[{"component":"netbird","status":"healthy","metrics":{"enabled":false}}],
 "alerts":[]}
EOF
run "$sb"
assert_contains "$PROM" 'lunarwing_health_component_enabled{component="netbird"} 0' "disabled -> component_enabled 0"
assert_absent  "$PROM" 'lunarwing_health_status{component="netbird"}' "disabled -> no status series"
assert_contains "$PROM" 'lunarwing_health_component_enabled' "component_enabled family present"

# ── 4. MT per-tenant instance labels + nested metric flattening ─────────────
sb="$(mksb)"
mkreport "$sb" "2026-08-15T10:00:00Z.json" <<'EOF'
{"timestamp":"2026-08-15T10:00:00Z","overall_status":"degraded",
 "components":[
  {"component":"lunarvision","status":"degraded","metrics":{
     "enabled":true,"latency_ms":42,"cache":{"hits":10,"misses":2,"hit_rate":0.83},
     "instances":[
       {"tenant":"lunarium","status":"healthy","latency_ms":42,"uptime_secs":100,"vl_available":"true"},
       {"tenant":"antelope","status":"critical","latency_ms":0,"uptime_secs":0,"vl_available":"false"}]}},
  {"component":"lunarvoice","status":"healthy","metrics":{
     "enabled":true,"instances":[
       {"tenant":"lunarium","status":"healthy","ready":true,"latency_ms":12,"errors":0}]}}
 ],
 "alerts":[]}
EOF
run "$sb"
assert_contains "$PROM" 'lunarwing_health_lunarvision_latency_ms{tenant="lunarium"} 42' "tenant-labeled vision latency"
assert_contains "$PROM" 'lunarwing_health_lunarvision_instance_status{tenant="lunarium"} 0' "instance status healthy"
assert_contains "$PROM" 'lunarwing_health_lunarvision_instance_status{tenant="antelope"} 2' "instance status critical"
assert_contains "$PROM" 'lunarwing_health_lunarvision_vl_available{tenant="lunarium"} 1' "string true -> 1"
assert_contains "$PROM" 'lunarwing_health_lunarvision_vl_available{tenant="antelope"} 0' "string false -> 0"
assert_contains "$PROM" 'lunarwing_health_lunarvision_cache_hits 10' "nested metric flattened (cache.hits)"
assert_contains "$PROM" 'lunarwing_health_lunarvision_cache_hit_rate 0.83' "nested metric flattened (cache.hit_rate)"
assert_contains "$PROM" 'lunarwing_health_lunarvoice_ready{tenant="lunarium"} 1' "voice ready boolean"

# ── 5. Init-agnostic unit series (systemd + openrc shapes) ──────────────────
sb="$(mksb)"
mkreport "$sb" "2026-08-15T10:00:00Z.json" <<'EOF'
{"timestamp":"2026-08-15T10:00:00Z","overall_status":"critical",
 "components":[{"component":"systemd","status":"critical","metrics":{"units":[
   {"name":"lunarwing-lunarium.service","active_state":"failed","sub_state":"auto-restart",
    "restart_count":3,"status":"critical","extra":{"tenant_user":"lunarium"}},
   {"name":"xmpp-bridge.service","active_state":"active","sub_state":"running",
    "restart_count":0,"status":"healthy","extra":{}}]}},
  {"component":"openrc","status":"healthy","metrics":{"services":[
   {"name":"lunarwing-antelope","state":"started","status":"healthy"}]}}],
 "alerts":[]}
EOF
run "$sb"
assert_contains "$PROM" 'lunarwing_health_unit_up{init="systemd",unit="lunarwing-lunarium.service",tenant="lunarium"} 0' "systemd failed unit up=0"
assert_contains "$PROM" 'lunarwing_health_unit_status{init="systemd",unit="lunarwing-lunarium.service",tenant="lunarium"} 2' "systemd unit status critical"
assert_contains "$PROM" 'lunarwing_health_unit_restarts_total{init="systemd",unit="lunarwing-lunarium.service",tenant="lunarium"} 3' "unit restart count"
assert_contains "$PROM" 'lunarwing_health_unit_up{init="systemd",unit="xmpp-bridge.service"} 1' "systemd active unit up=1 (no tenant label)"
assert_contains "$PROM" 'lunarwing_health_unit_up{init="openrc",unit="lunarwing-antelope"} 1' "openrc started service up=1"

# ── 6. Staleness gauge matches the report timestamp ─────────────────────────
sb="$(mksb)"
now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
mkreport "$sb" "$now.json" <<EOF
{"timestamp":"$now","overall_status":"healthy","components":[],"alerts":[]}
EOF
run "$sb"
ts="$(jq -r '.timestamp | fromdateiso8601' "$sb/reports/health/$now.json")"
age="$(grep -oE '^lunarwing_health_report_age_seconds [0-9]+' "$sb/reports/health/metrics.prom" | awk '{print $2}')"
if [[ "$age" =~ ^[0-9]+$ ]] && (( age <= 5 )); then
    ok "staleness gauge within 5s of now ($age)"
else
    bad "staleness gauge within 5s of now" "got age=$age (report ts=$ts)"
fi

# ── 7. Self-heal series from fixture state; absent state -> omitted ─────────
sb="$(mksb)"
flap_just_now=$(( $(date +%s) - 60 ))
mkreport "$sb" "2026-08-15T10:00:00Z.json" <<'EOF'
{"timestamp":"2026-08-15T10:00:00Z","overall_status":"healthy","components":[],"alerts":[]}
EOF
cat > "$sb/self-heal/state.json" <<EOF
{"tensorzero-gateway":{"retries":4,"escalated":true,"next_attempt_at":1786620000,
  "consecutive_unhealthy":2,"restart_history":[$flap_just_now,1786610100]},
 "xmpp-bridge":{"retries":1,"escalated":false,"consecutive_unhealthy":0,"restart_history":[]}}
EOF
run "$sb"
assert_contains "$PROM" 'lunarwing_health_selfheal_restarts_total{service="tensorzero-gateway"} 4' "selfheal restarts"
assert_contains "$PROM" 'lunarwing_health_selfheal_escalated{service="tensorzero-gateway"} 1' "selfheal escalated=1"
assert_contains "$PROM" 'lunarwing_health_selfheal_escalated{service="xmpp-bridge"} 0' "selfheal escalated=0"
assert_contains "$PROM" 'lunarwing_health_selfheal_consecutive_unhealthy{service="tensorzero-gateway"} 2' "selfheal streak"
assert_contains "$PROM" 'lunarwing_health_selfheal_flapping{service="tensorzero-gateway"} 1' "selfheal flapping counts in-window restarts"
assert_contains "$PROM" 'lunarwing_health_selfheal_backoff_next_attempt_seconds{service="tensorzero-gateway"} 1786620000' "selfheal backoff"

sb_nostate="$(mksb)"
mkreport "$sb_nostate" "2026-08-15T10:00:00Z.json" <<'EOF'
{"timestamp":"2026-08-15T10:00:00Z","overall_status":"healthy","components":[],"alerts":[]}
EOF
run "$sb_nostate"
assert_eq "$RC" "0" "absent state.json exits 0"
assert_absent "$(grep -vE '^(#|$)' <<<"$PROM")" 'lunarwing_health_selfheal' "absent state.json -> series omitted"

sb_corrupt="$(mksb)"
mkreport "$sb_corrupt" "2026-08-15T10:00:00Z.json" <<'EOF'
{"timestamp":"2026-08-15T10:00:00Z","overall_status":"healthy","components":[],"alerts":[]}
EOF
echo 'not json at all' > "$sb_corrupt/self-heal/state.json"
run "$sb_corrupt"
assert_eq "$RC" "0" "corrupt state.json exits 0 (series omitted)"
assert_contains "$PROM" 'lunarwing_health_overall_status' "report series still exported"

# ── 7b. Skipped units are a distinct code, never a failure (WI#180 F5) ──────
sb="$(mksb)"
mkreport "$sb" "2026-08-15T10:00:00Z.json" <<'EOF'
{"timestamp":"2026-08-15T10:00:00Z","overall_status":"healthy",
 "components":[{"component":"openrc","status":"healthy","metrics":{"services":[
   {"name":"lunarwing-weechat-acme","state":"stopped","status":"skipped"},
   {"name":"lunarwing-opencode2-acme","state":"stopped","status":"skipped"},
   {"name":"lunarwing-pg-acme","state":"unprobed","status":"skipped"},
   {"name":"lunarwing-acme","state":"started","status":"healthy"},
   {"name":"lunarwing-proxy-acme","state":"stopped","status":"critical"}]}}],
 "alerts":[]}
EOF
run "$sb"
assert_contains "$PROM" 'lunarwing_health_unit_status{init="openrc",unit="lunarwing-weechat-acme"} 4' "skipped unit -> status 4 (distinct from unknown)"
assert_contains "$PROM" 'lunarwing_health_unit_status{init="openrc",unit="lunarwing-opencode2-acme"} 4' "skipped worker -> status 4"
assert_contains "$PROM" 'lunarwing_health_unit_status{init="openrc",unit="lunarwing-pg-acme"} 4' "unprobed unit -> status 4 (not a failure)"
assert_contains "$PROM" 'lunarwing_health_unit_status{init="openrc",unit="lunarwing-acme"} 0' "healthy unit -> status 0"
assert_contains "$PROM" 'lunarwing_health_unit_status{init="openrc",unit="lunarwing-proxy-acme"} 2' "critical unit -> status 2"
# The example thresholds use equality (warn:==1 crit:==2), so an all-skipped
# fleet must not raise an early warning (status 4 would have fired under the
# old >0/>1 forms).
cat > "$ROOT/th-skip.conf" <<'EOF'
THRESHOLD_lunarwing_health_unit_status="warn:==1 crit:==2"
EOF
sb_skips="$(mksb)"
mkreport "$sb_skips" "2026-08-15T10:00:00Z.json" <<'EOF'
{"timestamp":"2026-08-15T10:00:00Z","overall_status":"healthy",
 "components":[{"component":"openrc","status":"healthy","metrics":{"services":[
   {"name":"lunarwing-weechat-acme","state":"stopped","status":"skipped"}]}}],
 "alerts":[]}
EOF
run "$sb_skips" HEALTH_METRICS_THRESHOLDS_FILE="$ROOT/th-skip.conf"
assert_absent "$PROM" 'lunarwing_health_early_warning{metric="lunarwing_health_unit_status"' \
    "skipped-only fleet raises no unit-status early warning"

# ── 8. Early-warning thresholds + notify gating ──────────────────────────────sb="$(mksb)"
mkreport "$sb" "2026-08-15T10:00:00Z.json" <<'EOF'
{"timestamp":"2026-08-15T10:00:00Z","overall_status":"degraded",
 "components":[
  {"component":"tensorzero","status":"degraded","metrics":{"p95_ms":5000,"error_rate":0.2}},
  {"component":"lunarvision","status":"healthy","metrics":{"enabled":true,"latency_ms":42}}
 ], "alerts":[]}
EOF
cat > "$ROOT/th.conf" <<'EOF'
THRESHOLD_lunarwing_health_tensorzero_p95_ms="warn:>1500 crit:>4000"
THRESHOLD_lunarwing_health_lunarvision_latency_ms="warn:>=42"
THRESHOLD_lunarwing_health_not_exported="crit:>1"
EOF
run "$sb" HEALTH_METRICS_THRESHOLDS_FILE="$ROOT/th.conf" HEALTH_METRICS_EARLY_WARNING_NOTIFY=false
assert_contains "$PROM" 'lunarwing_health_early_warning{metric="lunarwing_health_tensorzero_p95_ms",severity="warn"} 1' "warn crossing emitted"
assert_contains "$PROM" 'lunarwing_health_early_warning{metric="lunarwing_health_tensorzero_p95_ms",severity="crit"} 1' "crit crossing emitted"
assert_contains "$PROM" 'lunarwing_health_early_warning{metric="lunarwing_health_lunarvision_latency_ms",severity="warn"} 1' "warn:>= equality form"
assert_absent  "$PROM" 'not_exported' "rule for unexported metric skipped"
assert_absent  "$OUT" "GOTIFY" "notify off: no notifier output"

# Default: no thresholds file -> no early_warning series at all.
sb="$(mksb)"
mkreport "$sb" "2026-08-15T10:00:00Z.json" <<'EOF'
{"timestamp":"2026-08-15T10:00:00Z","overall_status":"healthy","components":[],"alerts":[]}
EOF
run "$sb" HEALTH_METRICS_THRESHOLDS_FILE="$ROOT/does-not-exist.conf"
assert_eq "$RC" "0" "missing thresholds file exits 0"
assert_absent "$PROM" 'lunarwing_health_early_warning{' "no early_warning series without config"

# Notify path: dedup + cooldown. A fake send-notification.sh records calls.
# The exporter resolves send-notification.sh relative to its own directory, so
# the tests run a copy of the exporter from a tree that also holds the fake
# notifier; HEALTH_METRICS_EXPORTER_SCRIPT (read per call by run()) redirects.
sb="$(mksb)"
mkreport "$sb" "2026-08-15T10:00:00Z.json" <<'EOF'
{"timestamp":"2026-08-15T10:00:00Z","overall_status":"healthy",
 "components":[{"component":"tensorzero","status":"healthy","metrics":{"p95_ms":5000}}],"alerts":[]}
EOF
mkdir -p "$ROOT/tree"
cp "$LIB_DIR/../health-metrics-exporter.sh" "$ROOT/tree/"
cat > "$ROOT/tree/send-notification.sh" <<'EOF'
#!/usr/bin/env bash
echo "NOTIFY $1 $2" >> "$RECORD"
exit 0
EOF
chmod +x "$ROOT/tree/send-notification.sh"
export RECORD="$ROOT/notify-record"
export HEALTH_METRICS_EXPORTER_SCRIPT="$ROOT/tree/health-metrics-exporter.sh"
: > "$RECORD"
run "$sb" HEALTH_METRICS_THRESHOLDS_FILE="$ROOT/th.conf" HEALTH_METRICS_EARLY_WARNING_NOTIFY=true
first_calls="$(wc -l < "$RECORD" 2>/dev/null || echo 0)"
assert_eq "$first_calls" "2" "notify fires once per severity (warn + crit)"
run "$sb" HEALTH_METRICS_THRESHOLDS_FILE="$ROOT/th.conf" HEALTH_METRICS_EARLY_WARNING_NOTIFY=true
second_calls="$(wc -l < "$RECORD" 2>/dev/null || echo 0)"
assert_eq "$second_calls" "2" "notify deduplicated within cooldown"
run "$sb" HEALTH_METRICS_THRESHOLDS_FILE="$ROOT/th.conf" HEALTH_METRICS_EARLY_WARNING_NOTIFY=true \
    HEALTH_METRICS_NOTIFY_COOLDOWN=0
third_calls="$(wc -l < "$RECORD" 2>/dev/null || echo 0)"
assert_eq "$third_calls" "4" "cooldown=0 re-fires immediately"
unset RECORD HEALTH_METRICS_EXPORTER_SCRIPT

# ── 9. Atomic write: no partial .prom on failure ────────────────────────────
sb="$(mksb)"
mkreport "$sb" "2026-08-15T10:00:00Z.json" <<'EOF'
{"timestamp":"2026-08-15T10:00:00Z","overall_status":"healthy","components":[],"alerts":[]}
EOF
echo "PREVIOUS" > "$sb/reports/health/metrics.prom"
# Unreadable/invalid report must fail without touching the previous file.
echo '{ broken json' > "$sb/reports/health/2026-08-15T11:00:00Z.json"
run "$sb"
assert_ne "$RC" "0" "broken newest report fails"
assert_contains "$PROM" "PREVIOUS" "previous .prom untouched on failure"
assert_eq "$(find "$sb/reports/health" -name '.metrics.prom.*' | wc -l)" "0" "no temp files left behind"

# Missing reports directory entirely -> clean failure, exit non-zero.
sb="$(mksb)"
run "$sb"
assert_ne "$RC" "0" "no report at all -> non-zero exit"

# ── 11. Models per-provider metrics flatten to bounded series ────────────────
sb="$(mksb)"
mkreport "$sb" "2026-08-15T10:00:00Z.json" <<'EOF'
{"timestamp":"2026-08-15T10:00:00Z","overall_status":"degraded","components":[
  {"component":"models","status":"degraded","providers":[
     {"name":"openrouter","status":"unknown","latency_ms":0,"last_error":"API key not set"},
     {"name":"local","status":"healthy","latency_ms":18,"last_error":""}],
   "metrics":{"enabled":true,"provider_count":2,"providers_healthy":1,
     "providers_degraded":0,"providers_critical":0,"latency_max_ms":18,
     "provider_latency_ms":{"openrouter":0,"local":18},
     "provider_status":{"openrouter":3,"local":0}}}],
 "alerts":[]}
EOF
run "$sb"
assert_contains "$PROM" 'lunarwing_health_models_provider_count 2' "models provider count exported"
assert_contains "$PROM" 'lunarwing_health_models_providers_healthy 1' "models healthy count exported"
assert_contains "$PROM" 'lunarwing_health_models_latency_max_ms 18' "models latency_max_ms exported"
assert_contains "$PROM" 'lunarwing_health_models_provider_latency_ms_openrouter 0' "per-provider latency flattened"
assert_contains "$PROM" 'lunarwing_health_models_provider_latency_ms_local 18' "per-provider latency local"
assert_contains "$PROM" 'lunarwing_health_models_provider_status_openrouter 3' "per-provider status unknown -> 3"
# String leaves (names, last_error) must never leak into the exposition.
assert_absent "$PROM" 'API key not set' "models last_error string dropped"
assert_absent "$PROM" 'lunarwing_health_models_providers{' "raw providers array not exported"

# Threshold evaluation against a per-provider flattened series.
cat > "$ROOT/th-models.conf" <<'EOF'
THRESHOLD_lunarwing_health_models_latency_max_ms="warn:>10"
THRESHOLD_lunarwing_health_models_provider_latency_ms_local="crit:>15"
EOF
run "$sb" HEALTH_METRICS_THRESHOLDS_FILE="$ROOT/th-models.conf" HEALTH_METRICS_EARLY_WARNING_NOTIFY=false
assert_contains "$PROM" 'lunarwing_health_early_warning{metric="lunarwing_health_models_latency_max_ms",severity="warn"} 1' "models rollup threshold crossing"
assert_contains "$PROM" 'lunarwing_health_early_warning{metric="lunarwing_health_models_provider_latency_ms_local",severity="crit"} 1' "per-provider threshold crossing"

# ── 10. Newest report wins over older ones ──────────────────────────────────
sb="$(mksb)"
mkreport "$sb" "2026-08-15T09:00:00Z.json" <<'EOF'
{"timestamp":"2026-08-15T09:00:00Z","overall_status":"healthy","components":[],"alerts":[]}
EOF
mkreport "$sb" "2026-08-15T10:00:00Z.json" <<'EOF'
{"timestamp":"2026-08-15T10:00:00Z","overall_status":"critical","components":[],"alerts":[]}
EOF
run "$sb"
assert_contains "$PROM" 'lunarwing_health_overall_status 2' "newest report used"
assert_contains "$PROM" 'lunarwing_health_report_timestamp_seconds 1786788000' "timestamp from newest report"

# ── 12. Stopped exporter: staleness must be derivable at scrape time ─────────
# MR !1041 review finding 4: the exported report_age_seconds is a snapshot
# frozen at export time. If scheduling stops, the textfile keeps serving the
# same age forever, so the old alert (age_seconds > 2400) could never fire on
# the default textfile path. The alert rule now derives the age from the
# report timestamp at scrape time (time() - report_timestamp_seconds, as the
# dashboard already does), which keeps advancing on every scrape.
sb="$(mksb)"
stale_ts=$(( $(date +%s) - 1800 ))                 # report produced 30m ago
stale_iso="$(date -u -d "@$stale_ts" +%Y-%m-%dT%H:%M:%SZ)"
mkreport "$sb" "$stale_iso.json" <<EOF
{"timestamp":"$stale_iso","overall_status":"healthy","components":[],"alerts":[]}
EOF
run "$sb"
frozen_age="$(grep -oE '^lunarwing_health_report_age_seconds [0-9]+' \
    "$sb/reports/health/metrics.prom" | awk '{print $2}')"
exported_ts="$(grep -oE '^lunarwing_health_report_timestamp_seconds [0-9]+' \
    "$sb/reports/health/metrics.prom" | awk '{print $2}')"
assert_eq "$exported_ts" "$stale_ts" "12a: report timestamp exported verbatim"
if [[ "$frozen_age" =~ ^[0-9]+$ ]] && (( frozen_age < 2400 )); then
    ok "12b: frozen age gauge ($frozen_age) sits below the 2400s threshold"
else
    bad "12b: frozen age gauge below threshold" "got age=$frozen_age"
fi
# Simulate the exporter having stopped 25 minutes ago: wall time advances to
# ts+2400+60 while the textfile still serves the frozen gauge. The scrape-time
# expression fires; the frozen-gauge comparison never would.
future_now=$(( stale_ts + 2400 + 60 ))
derived_age=$(( future_now - exported_ts ))
if (( derived_age > 2400 )); then
    ok "12c: scrape-time expression (time() - ts = $derived_age) trips the stale threshold"
else
    bad "12c: scrape-time expression trips the threshold" "derived=$derived_age"
fi
# The committed alert rule must use the scrape-time form (guards against
# regressing to the frozen-gauge comparison).
rules_file="$LIB_DIR/../contrib/prometheus-rules-lunarwing-health.yml"
assert_contains "$(cat "$rules_file")" \
    "expr: (time() - lunarwing_health_report_timestamp_seconds) > 2400" \
    "12d: alert rule derives age at evaluation time"

finish
