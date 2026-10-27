#!/usr/bin/env bash
# WI#424 worker egress health component and orchestration pins.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CHECK="$ROOT/health-worker-egress.sh"
FIXTURE="$(mktemp -d)"
trap 'rm -rf "$FIXTURE"' EXIT
failures=0
passes=0
pass() { printf 'PASS: %s\n' "$1"; passes=$((passes + 1)); }
fail() { printf 'FAIL: %s\n' "$1" >&2; failures=$((failures + 1)); }
assert_eq() {
  if [[ "$1" == "$2" ]]; then pass "$3"; else fail "$3 (got '$1', want '$2')"; fi
}
assert_contains() {
  if [[ "$1" == *"$2"* ]]; then pass "$3"; else fail "$3 (missing: $2)"; fi
}
assert_absent() {
  if [[ "$1" != *"$2"* ]]; then pass "$3"; else fail "$3 (unexpected: $2)"; fi
}

ADMIN="$FIXTURE/mt-admin"
CALL_LOG="$FIXTURE/calls.log"
cat >"$ADMIN" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$CALL_LOG"
case "${2:-}" in
  alpha) exit "${ALPHA_RC:-0}" ;;
  beta) exit "${BETA_RC:-0}" ;;
  *) exit 2 ;;
esac
STUB
chmod +x "$ADMIN"
cat >"$FIXTURE/ports.json" <<'JSON'
{"tenants":{
  "alpha":{"workers":{"opencode2":true}},
  "beta":{"workers":{"pebble":true}},
  "gamma":{"workers":{"opencode":false}}
}}
JSON

run_check() {
  set +e
  OUTPUT="$(env CALL_LOG="$CALL_LOG" LUNARWING_MT_ADMIN="$ADMIN" \
    SELF_HEAL_TENANTS_FILE="$FIXTURE/ports.json" "$CHECK" 2>&1)"
  RC=$?
  set -e
}

: >"$CALL_LOG"
ALPHA_RC=0 BETA_RC=0 run_check
assert_eq "$RC" "0" "healthy audits exit zero"
assert_eq "$(jq -r '.status' <<<"$OUTPUT")" "healthy" "healthy audits report healthy"
assert_eq "$(jq '.metrics.instances | length' <<<"$OUTPUT")" "2" "only selected worker tenants are audited"
assert_absent "$OUTPUT" 'gamma' "unselected tenants are not audited"
assert_eq "$(wc -l <"$CALL_LOG" | tr -d ' ')" "2" "mt-admin is called once per selected tenant"

: >"$CALL_LOG"
ALPHA_RC=0 BETA_RC=1 run_check
assert_eq "$RC" "2" "a failed tenant audit is critical"
assert_eq "$(jq -r '.status' <<<"$OUTPUT")" "critical" "failed audit drives component critical"
assert_eq "$(jq -r '.metrics.instances[] | select(.tenant=="alpha") | .status' <<<"$OUTPUT")" "healthy" "healthy sibling remains healthy"
assert_eq "$(jq -r '.metrics.instances[] | select(.tenant=="beta") | .status' <<<"$OUTPUT")" "critical" "failed tenant is attributed critical"
assert_contains "$OUTPUT" 'worker-egress beta' "issue names the read-only audit command"

printf '{not json\n' >"$FIXTURE/ports.json"
run_check
assert_eq "$RC" "1" "invalid registry degrades coverage instead of passing"
assert_eq "$(jq -r '.status' <<<"$OUTPUT")" "degraded" "invalid registry reports degraded"

rm -f "$FIXTURE/ports.json"
run_check
assert_eq "$RC" "0" "no registry means no multi-tenant workers to audit"
assert_eq "$(jq '.metrics.instances | length' <<<"$OUTPUT")" "0" "no-registry result is explicitly empty"

set +e
OUTPUT="$(LUNARWING_MT_ADMIN="$FIXTURE/missing" SELF_HEAL_TENANTS_FILE=/dev/null "$CHECK" 2>&1)"
RC=$?
set -e
assert_eq "$RC" "1" "missing mt-admin degrades the audit"
assert_contains "$OUTPUT" 'not executable' "missing mt-admin names the coverage gap"

main_report="$(cat "$ROOT/infrastructure-health-check.sh")"
assert_contains "$main_report" 'run_check "health-worker-egress.sh" "worker_egress"' "main health check runs the worker egress component"
# shellcheck disable=SC2016  # literal shell variable reference in the generated report.
assert_contains "$main_report" '${check_worker_egress:-}' "main report includes the worker egress component"

printf '%d passed, %d failed\n' "$passes" "$failures"
if ((failures > 0)); then
  exit 1
fi
