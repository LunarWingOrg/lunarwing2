#!/usr/bin/env bash
# NetBird readiness probe tests. All CLI calls hit a PATH-shadowed mock.

set -uo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$DIR/../health-netbird.sh"
command -v jq >/dev/null 2>&1 || { echo "FATAL: jq required"; exit 1; }

ROOT="$(mktemp -d "${TMPDIR:-/tmp}/netbird-health.XXXXXX")"
trap 'rm -rf "$ROOT"' EXIT
mkdir -p "$ROOT/bin"

cat > "$ROOT/bin/netbird" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" > "${MOCK_NETBIRD_ARGS:?}"
[[ -n "${MOCK_NETBIRD_SLEEP:-}" ]] && sleep "$MOCK_NETBIRD_SLEEP"
exit "${MOCK_NETBIRD_RC:-0}"
EOF
chmod +x "$ROOT/bin/netbird"

pass=0; fail=0; OUT=""; RC=0
ok() { printf 'PASS: %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL: %s\n      %s\n' "$1" "$2"; fail=$((fail + 1)); }
assert_eq() { if [[ "$1" == "$2" ]]; then ok "$3"; else bad "$3" "got [$1] want [$2]"; fi; }
assert_contains() { if grep -qF -- "$2" <<<"$1"; then ok "$3"; else bad "$3" "expected substring: $2"; fi; }

run_check() {
    rm -f "$ROOT/args"
    OUT="$(env PATH="$ROOT/bin:$PATH" MOCK_NETBIRD_ARGS="$ROOT/args" "$@" "$CHECK" 2>/dev/null)"
    RC=$?
}

echo "=== NetBird readiness probe ==="
run_check
assert_eq "$RC" "0" "N1: disabled by default exits 0"
assert_eq "$(jq -r '.status' <<<"$OUT")" "healthy" "N1: disabled status healthy"
assert_eq "$(jq -r '.metrics.enabled' <<<"$OUT")" "false" "N1: disabled metric explicit"
if [[ ! -e "$ROOT/args" ]]; then
    ok "N1: disabled probe does not invoke CLI"
else
    bad "N1: disabled probe does not invoke CLI" "mock was invoked"
fi

run_check HEALTH_NETBIRD_ENABLED=true
assert_eq "$RC" "0" "N2: ready peer exits 0"
assert_eq "$(jq -r '.status' <<<"$OUT")" "healthy" "N2: ready peer healthy"
assert_eq "$(jq -r '.metrics.ready' <<<"$OUT")" "true" "N2: ready metric true"
assert_eq "$(cat "$ROOT/args")" "status --check startup" "N2: documented startup readiness command used"

run_check HEALTH_NETBIRD_ENABLED=true MOCK_NETBIRD_RC=1
assert_eq "$RC" "2" "N3: unready peer exits 2"
assert_eq "$(jq -r '.status' <<<"$OUT")" "critical" "N3: unready peer critical"
assert_eq "$(jq -r '.metrics.probe_exit' <<<"$OUT")" "1" "N3: CLI exit exposed"

run_check HEALTH_NETBIRD_ENABLED=true NETBIRD_BINARY=definitely-not-a-netbird-binary
assert_eq "$RC" "2" "N4: enabled but missing binary exits 2"
assert_contains "$(jq -r '.issues[0]' <<<"$OUT")" "binary is unavailable" "N4: missing binary explained"

if command -v timeout >/dev/null 2>&1; then
    run_check HEALTH_NETBIRD_ENABLED=true HEALTH_NETBIRD_TIMEOUT=1 MOCK_NETBIRD_SLEEP=3
    assert_eq "$RC" "2" "N5: timed-out probe exits 2"
    assert_contains "$(jq -r '.issues[0]' <<<"$OUT")" "timed out" "N5: timeout explained"
else
    ok "N5: skipped (timeout unavailable)"
    ok "N5: skipped (timeout unavailable)"
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
