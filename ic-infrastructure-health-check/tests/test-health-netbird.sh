#!/usr/bin/env bash
# Tests for health-netbird.sh
#
# Mocks the `netbird` binary to simulate daemon states. No real NetBird
# install needed — all probe output is faked via PATH shadowing.
#
# Run:  bash tests/test-health-netbird.sh   (exit 0 = pass, 1 = fail)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$SCRIPT_DIR/../health-netbird.sh"
[[ -x "$CHECK" ]] || { echo "FATAL: $CHECK not found/executable"; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "FATAL: jq required"; exit 1; }

ROOT="$(mktemp -d "${TMPDIR:-/tmp}/nb-health-test.XXXXXX")"
trap 'rm -rf "$ROOT"' EXIT

fail=0; pass=0
ok()  { printf 'PASS: %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL: %s\n      %s\n' "$1" "$2"; fail=$((fail + 1)); }
assert_eq()       { [[ "$1" == "$2" ]] && ok "$3" || bad "$3" "got [$1] want [$2]"; }
assert_contains() { grep -qF -- "$2" <<<"$1" && ok "$3" || bad "$3" "expected: $2"; }

# ── Mock netbird binary ──────────────────────────────────────────────────────
# Reads MOCK_NB_EXIT (default 0), MOCK_NB_MGMT (Connected|Disconnected|<empty>),
# MOCK_NB_SLEEP (seconds to sleep before answering, for the timeout case).

MOCK_BIN="$ROOT/bin"
mkdir -p "$MOCK_BIN"

cat > "$MOCK_BIN/netbird" <<'MOCKNB'
#!/usr/bin/env bash
[[ -n "${MOCK_NB_SLEEP:-}" ]] && sleep "$MOCK_NB_SLEEP"
if [[ "${1:-}" == status ]]; then
    if [[ "${MOCK_NB_EXIT:-0}" -ne 0 ]]; then
        echo "Error: daemon not running" >&2
        exit "${MOCK_NB_EXIT:-1}"
    fi
    echo "Daemon status: Running"
    [[ -n "${MOCK_NB_MGMT:-}" ]] && echo "Management: ${MOCK_NB_MGMT}"
    echo "Signal: Connected"
    exit 0
fi
exit 2
MOCKNB
chmod +x "$MOCK_BIN/netbird"

# run_check [ENV=VAL ...] — invoke the check with the mock on PATH; captures
# stdout (JSON) and the real exit code into the globals OUT / RC.
OUT=""; RC=0
run_check() {
    OUT="$(env PATH="$MOCK_BIN:$PATH" "$@" "$CHECK" 2>/dev/null)"; RC=$?
}

echo "=== N1: HEALTH_NETBIRD_ENABLED=false → healthy/disabled ==="
run_check HEALTH_NETBIRD_ENABLED=false
assert_eq "$RC" "0" "N1: exit 0"
assert_eq "$(jq -r '.status' <<<"$OUT")" "healthy" "N1: healthy"
assert_eq "$(jq -r '.metrics.enabled' <<<"$OUT")" "false" "N1: enabled=false"

echo "=== N2: netbird binary absent → healthy/disabled (not the tunnel provider) ==="
run_check NETBIRD_BINARY=definitely-no-such-netbird-binary
assert_eq "$RC" "0" "N2: exit 0"
assert_eq "$(jq -r '.status' <<<"$OUT")" "healthy" "N2: healthy"
assert_eq "$(jq -r '.metrics.enabled' <<<"$OUT")" "false" "N2: enabled=false"

echo "=== N3: daemon up, Management: Connected → healthy ==="
run_check MOCK_NB_MGMT=Connected
assert_eq "$RC" "0" "N3: exit 0"
assert_eq "$(jq -r '.status' <<<"$OUT")" "healthy" "N3: healthy"
assert_eq "$(jq -r '.metrics.daemon' <<<"$OUT")" "up" "N3: daemon=up"
assert_eq "$(jq -r '.metrics.management' <<<"$OUT")" "connected" "N3: management=connected"
assert_eq "$(jq -r '.metrics.enabled' <<<"$OUT")" "true" "N3: enabled=true"
assert_eq "$(jq -r '.component' <<<"$OUT")" "netbird" "N3: component=netbird"

echo "=== N4: daemon up, Management: Disconnected → degraded ==="
run_check MOCK_NB_MGMT=Disconnected
assert_eq "$RC" "1" "N4: exit 1"
assert_eq "$(jq -r '.status' <<<"$OUT")" "degraded" "N4: degraded"
assert_eq "$(jq -r '.metrics.daemon' <<<"$OUT")" "up" "N4: daemon still up"
assert_eq "$(jq -r '.metrics.management' <<<"$OUT")" "disconnected" "N4: 'Disconnected' not misread as connected (substring guard)"

echo "=== N5: daemon down (status exits non-zero) → critical ==="
run_check MOCK_NB_EXIT=1
assert_eq "$RC" "2" "N5: exit 2"
assert_eq "$(jq -r '.status' <<<"$OUT")" "critical" "N5: critical"
assert_eq "$(jq -r '.metrics.daemon' <<<"$OUT")" "down" "N5: daemon=down"
assert_contains "$(jq -r '.issues[0]' <<<"$OUT")" "not responding" "N5: issue explains the outage"

echo "=== N6: exit 0 with no Management line → healthy, management=unknown ==="
run_check
assert_eq "$RC" "0" "N6: exit 0"
assert_eq "$(jq -r '.status' <<<"$OUT")" "healthy" "N6: healthy"
assert_eq "$(jq -r '.metrics.management' <<<"$OUT")" "unknown" "N6: management=unknown when unparsable"

echo "=== N7: probe exceeding HEALTH_NETBIRD_TIMEOUT → critical ==="
if command -v timeout >/dev/null 2>&1; then
    run_check MOCK_NB_SLEEP=3 HEALTH_NETBIRD_TIMEOUT=1
    assert_eq "$RC" "2" "N7: exit 2 on probe timeout"
    assert_eq "$(jq -r '.status' <<<"$OUT")" "critical" "N7: critical on probe timeout"
    assert_eq "$(jq -r '.metrics.daemon' <<<"$OUT")" "down" "N7: daemon=down on timeout"
else
    ok "N7: skipped (GNU timeout unavailable)"
    ok "N7: skipped (GNU timeout unavailable)"
    ok "N7: skipped (GNU timeout unavailable)"
fi

echo "=== N8: NETBIRD_BINARY override is honored ==="
cp "$MOCK_BIN/netbird" "$ROOT/netbird-custom"; chmod +x "$ROOT/netbird-custom"
OUT="$(env MOCK_NB_MGMT=Connected NETBIRD_BINARY="$ROOT/netbird-custom" "$CHECK" 2>/dev/null)"; RC=$?
assert_eq "$RC" "0" "N8: exit 0 via override binary"
assert_eq "$(jq -r '.metrics.management' <<<"$OUT")" "connected" "N8: override binary probed"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
