#!/usr/bin/env bash

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$SCRIPT_DIR/../health-lunarvoice.sh"
command -v jq >/dev/null 2>&1 || { echo "FATAL: jq required"; exit 1; }
ROOT="$(mktemp -d "${TMPDIR:-/tmp}/lunarvoice-health-test.XXXXXX")"
trap 'rm -rf "$ROOT"' EXIT
mkdir -p "$ROOT/bin"
fail=0; pass=0
ok() { printf 'PASS: %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL: %s (%s)\n' "$1" "$2"; fail=$((fail + 1)); }
assert_eq() { [[ "$1" == "$2" ]] && ok "$3" || bad "$3" "got '$1', want '$2'"; }
assert_has() { grep -qF -- "$2" <<<"$1" && ok "$3" || bad "$3" "missing '$2'"; }

cat >"$ROOT/bin/curl" <<'MOCK'
#!/usr/bin/env bash
url=""; write_code=false
for arg in "$@"; do
  case "$arg" in http://*|https://*) url="$arg" ;; *%\{http_code\}*) write_code=true ;; esac
done
[[ -z "${MOCK_CALLS:-}" ]] || printf '%s\n' "$url" >>"$MOCK_CALLS"
[[ "${MOCK_CURL_FAIL:-false}" != true ]] || exit 7
[[ -z "${MOCK_CURL_FAIL_MATCH:-}" || "$url" != *"$MOCK_CURL_FAIL_MATCH"* ]] || exit 7
case "$url" in
  */health) body="${MOCK_HEALTH_BODY:-}"; [[ -n "$body" ]] || body='{"status":"ok","uptime_secs":12}'; code="${MOCK_HEALTH_CODE:-200}" ;;
  */ready) body="${MOCK_READY_BODY:-}"; [[ -n "$body" ]] || body='{"ready":true}'; code="${MOCK_READY_CODE:-200}" ;;
  */metrics) body="${MOCK_METRICS_BODY:-}"; [[ -n "$body" ]] || body='{"total_requests":9,"transcription_requests":4}'; code=200 ;;
  *) body='{}'; code=404 ;;
esac
printf '%s' "$body"
$write_code && printf '\n%s' "$code"
exit 0
MOCK
chmod +x "$ROOT/bin/curl"

run_check() { PATH="$ROOT/bin:$PATH" bash "$CHECK" 2>/dev/null; }

out="$(HEALTH_LUNARVOICE_ENABLED=false run_check)"; rc=$?
assert_eq "$rc" 0 "disabled exits healthy"
assert_eq "$(jq -r '.metrics.enabled' <<<"$out")" false "disabled reports enabled=false"

out="$(HEALTH_LUNARVOICE_URL=http://127.0.0.1:8091 run_check)"; rc=$?
assert_eq "$rc" 0 "healthy explicit target exits zero"
assert_eq "$(jq -r '.status' <<<"$out")" healthy "healthy explicit target status"
assert_eq "$(jq -r '.metrics.instances[0].ready' <<<"$out")" true "readiness parsed"
assert_eq "$(jq -r '.metrics.instances[0].transcription_requests' <<<"$out")" 4 "metrics parsed"

out="$(MOCK_READY_CODE=503 MOCK_READY_BODY='{"ready":false}' HEALTH_LUNARVOICE_URL=http://127.0.0.1:8091 run_check)"; rc=$?
assert_eq "$rc" 2 "unready STT exits critical"
assert_eq "$(jq -r '.status' <<<"$out")" critical "unready STT status"
assert_has "$(jq -r '.issues[]' <<<"$out")" "readiness" "unready issue explains failure"

registry="$ROOT/ports.json"
cat >"$registry" <<'JSON'
{"version":15,"tenants":{"on":{"enable_voice":true,"voice_ports":{"voice_health":41001}},"off":{"enable_voice":false,"voice_ports":{"voice_health":42001}}}}
JSON
calls="$ROOT/calls"; : >"$calls"
out="$(MOCK_CALLS="$calls" SELF_HEAL_TENANTS_FILE="$registry" run_check)"; rc=$?
assert_eq "$rc" 0 "registry enabled tenant healthy"
assert_eq "$(jq -r '.metrics.discovery' <<<"$out")" ports-registry "registry discovery reported"
assert_eq "$(jq -r '.metrics.target_count' <<<"$out")" 1 "disabled tenant omitted"
assert_has "$(cat "$calls")" "41001" "enabled tenant probed"
if grep -q 42001 "$calls"; then bad "disabled tenant not probed" "found 42001"; else ok "disabled tenant not probed"; fi

cat >"$registry" <<'JSON'
{"version":16,"tenants":{"one":{"enable_voice":true,"voice_ports":{"voice_health":41001}},"two":{"enable_voice":true,"voice_ports":{"voice_health":42001}}}}
JSON
out="$(MOCK_CURL_FAIL_MATCH=42001 SELF_HEAL_TENANTS_FILE="$registry" run_check)"; rc=$?
assert_eq "$rc" 2 "one unreachable enabled tenant makes fleet critical"
assert_eq "$(jq -r '.metrics.target_count' <<<"$out")" 2 "all enabled tenants remain in health report"
assert_eq "$(jq -r '[.metrics.instances[] | select(.status=="healthy")] | length' <<<"$out")" 1 "healthy peer remains visible"
assert_eq "$(jq -r '[.metrics.instances[] | select(.status=="critical")] | length' <<<"$out")" 1 "unreachable peer remains visible"
assert_has "$(jq -r '.issues[]' <<<"$out")" "42001" "fleet issue names unreachable peer port"

out="$(HEALTH_LUNARVOICE_FETCH_METRICS=false HEALTH_LUNARVOICE_URL=http://127.0.0.1:8091 run_check)"; rc=$?
assert_eq "$rc" 0 "metrics fetching can be disabled"
assert_eq "$(jq -r '.metrics.instances[0].total_requests' <<<"$out")" 0 "disabled metrics fetch emits zero counters"

cat >"$registry" <<'JSON'
{"version":16,"tenants":{"missing":{"enable_voice":true,"voice_ports":{}}}}
JSON
out="$(SELF_HEAL_TENANTS_FILE="$registry" run_check)"; rc=$?
assert_eq "$rc" 2 "enabled tenant without health port exits critical"
assert_has "$(jq -r '.issues[]' <<<"$out")" "missing" "missing-port issue names tenant"

cat >"$registry" <<'JSON'
{"version":16,"tenants":{"one":{"enable_voice":true,"voice_ports":{"voice_health":41001}},"two":{"enable_voice":true,"voice_ports":{"voice_health":41001}}}}
JSON
out="$(SELF_HEAL_TENANTS_FILE="$registry" run_check)"; rc=$?
assert_eq "$rc" 2 "duplicate enabled health ports exit critical"
assert_has "$(jq -r '.issues[]' <<<"$out")" "41001" "duplicate-port issue names port"

printf '{not-json\n' >"$registry"
out="$(SELF_HEAL_TENANTS_FILE="$registry" run_check)"; rc=$?
assert_eq "$rc" 2 "malformed registry exits critical"
assert_has "$(jq -r '.issues[]' <<<"$out")" "valid tenant JSON" "malformed registry issue is explicit"

cat >"$registry" <<'JSON'
{"version":16,"tenants":{"on":{"enable_voice":true,"voice_ports":{"voice_health":41001}},"off":{"enable_voice":false,"voice_ports":{"voice_health":42001}}}}
JSON
jq '.tenants.on.enable_voice=false' "$registry" >"$registry.tmp" && mv "$registry.tmp" "$registry"
: >"$calls"
out="$(MOCK_CALLS="$calls" SELF_HEAL_TENANTS_FILE="$registry" run_check)"; rc=$?
assert_eq "$rc" 0 "no enabled tenants exits healthy"
assert_eq "$(jq -r '.metrics.enabled' <<<"$out")" false "no enabled tenants reports disabled"
assert_eq "$(wc -l <"$calls")" 0 "no enabled tenants makes no requests"

out="$(MOCK_CURL_FAIL=true HEALTH_LUNARVOICE_URL=http://127.0.0.1:8091 run_check)"; rc=$?
assert_eq "$rc" 2 "unreachable sidecar exits critical"
assert_eq "$(jq -r '.status' <<<"$out")" critical "unreachable sidecar status"

printf '%s passed, %s failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
