#!/usr/bin/env bash
# Tests for ic/scripts/lunarwing-watchdog-openrc.sh (OpenRC watchdog wrapper).
#
# Self-contained mock (rc-service/curl) driven by a state directory — no real
# OpenRC and no real services are touched. Mirrors test-watchdog-systemd.sh.
# Validates:
#   - started service -> no action
#   - stopped service -> start + verify
#   - start command failure / stuck-stopped -> exit 1
#   - gateway deep check restarts on unhealthy channels only
#   - every rc-service call is bounded (hung status probe -> failed probe,
#     normal start path, whole run completes inside the timeout budget)
#   - non-numeric probe timeout falls back to the default (no breakage)
#   - a held lock aborts the run cleanly (exit 0, no rc-service calls)
#
# State layout under <sb>/wdstate:
#   <svc>            -> "started" | "stopped"
#   fail/<svc>       -> marker: `start`/`restart` returns non-zero
#   stuck/<svc>      -> marker: `start` succeeds but the service stays stopped
#   hang/<svc>       -> marker: `status` sleeps N seconds (wedged OpenRC)
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/lib.sh
source "$DIR/lib.sh"
WD="$DIR/../../ic/scripts/lunarwing-watchdog-openrc.sh"

[[ -x "$WD" ]] || { echo "FATAL: watchdog script not found/executable at $WD"; exit 1; }

mk_wd_mock() {
    local sb="$1" bin="$1/bin"
    mkdir -p "$bin" "$1/wdstate/fail" "$1/wdstate/stuck" "$1/wdstate/hang"

    cat > "$bin/rc-service" <<'EOF'
#!/usr/bin/env bash
SS="${WD_STATE_DIR:?}"
svc="${1:-}"; action="${2:-}"
[ -n "${WD_CALL_LOG:-}" ] && printf '%s %s\n' "$action" "$svc" >> "$WD_CALL_LOG"
case "$action" in
  status)
    [ -f "$SS/hang/$svc" ] && sleep "$(cat "$SS/hang/$svc")"
    if [ "$(cat "$SS/$svc" 2>/dev/null)" = started ]; then
      echo " * status: started"; exit 0
    fi
    echo " * status: stopped"; exit 3 ;;
  start|restart)
    [ -f "$SS/fail/$svc" ] && { echo "mock rc-service: $action $svc failed" >&2; exit 1; }
    [ -f "$SS/stuck/$svc" ] || echo started > "$SS/$svc"
    exit 0 ;;
  *) exit 0 ;;
esac
EOF

    cat > "$bin/curl" <<'EOF'
#!/usr/bin/env bash
# -sf -m 10 -H ... <url>; serve the canned gateway payload or fail like -sf does.
[ -n "${WD_GATEWAY_JSON:-}" ] && [ -f "$WD_GATEWAY_JSON" ] && { cat "$WD_GATEWAY_JSON"; exit 0; }
exit 7
EOF
    chmod +x "$bin"/*
}

wd_set()   { printf '%s\n' "$3" > "$1/wdstate/$2"; }          # <sb> <svc> <state>
wd_fail()  { : > "$1/wdstate/fail/$2"; }
wd_stuck() { : > "$1/wdstate/stuck/$2"; }
wd_hang()  { printf '%s\n' "$3" > "$1/wdstate/hang/$2"; }     # <sb> <svc> <secs>

run_wd() {  # <sb> [ENV=VALUE ...] -> stdout+stderr; rc via $@
    local sb="$1"; shift
    env PATH="$sb/bin:$PATH" WD_STATE_DIR="$sb/wdstate" \
        LUNARWING_WATCHDOG_CONFD="$sb/no-such-confd" \
        LUNARWING_WATCHDOG_SERVICE=lunarwing \
        LUNARWING_WATCHDOG_LOG="$sb/watchdog.log" \
        LUNARWING_WATCHDOG_LOCK="$sb/watchdog.lock" \
        LUNARWING_WATCHDOG_POST_RESTART_SLEEP_SECONDS=0 \
        "$@" bash "$WD" 2>&1
}

starts()   { grep -c '^start '   "$1/calls" 2>/dev/null || true; }
restores() { grep -c '^restart ' "$1/calls" 2>/dev/null || true; }

# ── A: started service -> no action ───────────────────────────────────────────
echo "=== Section A: started service ==="
a="$(sb)"; mk_wd_mock "$a"; wd_set "$a" lunarwing started
: > "$a/calls"
oA="$(run_wd "$a" WD_CALL_LOG="$a/calls")"; rcA=$?
assert_eq "$rcA" "0" "A1: started service exits 0"
assert_contains "$oA" "no action" "A2: started service reports no action"
assert_eq "$(starts "$a")" "0" "A3: started service is never started"

# ── B: stopped service -> start + verify success ──────────────────────────────
echo "=== Section B: stopped service start ==="
b="$(sb)"; mk_wd_mock "$b"; wd_set "$b" lunarwing stopped
: > "$b/calls"
oB="$(run_wd "$b" WD_CALL_LOG="$b/calls")"; rcB=$?
assert_eq "$rcB" "0" "B1: stopped service start exits 0"
assert_contains "$oB" "start succeeded" "B2: start success logged"
assert_eq "$(starts "$b")" "1" "B3: exactly one start issued"

# ── C: start command fails -> exit 1 ──────────────────────────────────────────
echo "=== Section C: start command failure ==="
c="$(sb)"; mk_wd_mock "$c"; wd_set "$c" lunarwing stopped; wd_fail "$c" lunarwing
oC="$(run_wd "$c")"; rcC=$?
assert_eq "$rcC" "1" "C1: failed start exits 1"
assert_contains "$oC" "start command failed" "C2: start failure logged"

# ── D: start succeeds but service stays stopped -> exit 1 ────────────────────
echo "=== Section D: stuck service ==="
d="$(sb)"; mk_wd_mock "$d"; wd_set "$d" lunarwing stopped; wd_stuck "$d" lunarwing
oD="$(run_wd "$d")"; rcD=$?
assert_eq "$rcD" "1" "D1: stuck service exits 1"
assert_contains "$oD" "still not active" "D2: stuck service reported"

# ── E: gateway deep check — unhealthy channels restart a started service ──────
echo "=== Section E: deep health check ==="
e="$(sb)"; mk_wd_mock "$e"; wd_set "$e" lunarwing started
printf '%s\n' '{"channel_health":{"xmpp":{"healthy":false},"web":{"healthy":true}}}' > "$e/gw.json"
: > "$e/calls"
oE="$(run_wd "$e" WD_CALL_LOG="$e/calls" WD_GATEWAY_JSON="$e/gw.json" \
    LUNARWING_WATCHDOG_GATEWAY_URL=http://127.0.0.1:1 LUNARWING_WATCHDOG_GATEWAY_TOKEN=t)"; rcE=$?
assert_eq "$rcE" "0" "E1: deep-check restart exits 0"
assert_contains "$oE" "channels unhealthy: xmpp" "E2: unhealthy channel named"
assert_eq "$(restores "$e")" "1" "E3: deep check restarts the started service"

e2="$(sb)"; mk_wd_mock "$e2"; wd_set "$e2" lunarwing started
printf '%s\n' '{"channel_health":{"xmpp":{"healthy":true}}}' > "$e2/gw.json"
: > "$e2/calls"
oE2="$(run_wd "$e2" WD_CALL_LOG="$e2/calls" WD_GATEWAY_JSON="$e2/gw.json" \
    LUNARWING_WATCHDOG_GATEWAY_URL=http://127.0.0.1:1 LUNARWING_WATCHDOG_GATEWAY_TOKEN=t)"
assert_contains "$oE2" "no action" "E4: all-healthy channels -> no restart"
assert_eq "$(restores "$e2")" "0" "E5: all-healthy channels -> zero restarts"

e3="$(sb)"; mk_wd_mock "$e3"; wd_set "$e3" lunarwing started
: > "$e3/calls"
oE3="$(run_wd "$e3" WD_CALL_LOG="$e3/calls")"
assert_contains "$oE3" "no action" "E6: no gateway url/token -> deep check skipped"
assert_eq "$(restores "$e3")" "0" "E7: deep check skipped -> zero restarts"

e4="$(sb)"; mk_wd_mock "$e4"; wd_set "$e4" lunarwing started
: > "$e4/calls"
oE4="$(run_wd "$e4" WD_CALL_LOG="$e4/calls" \
    LUNARWING_WATCHDOG_GATEWAY_URL=http://127.0.0.1:1 LUNARWING_WATCHDOG_GATEWAY_TOKEN=t)"
assert_contains "$oE4" "no action" "E8: unreachable gateway (curl fail) -> no spurious restart"
assert_eq "$(restores "$e4")" "0" "E9: unreachable gateway -> zero restarts"

# ── F: hung status probe is bounded and follows the normal start path ─────────
echo "=== Section F: wedged rc-service (probe timeout) ==="
if command -v timeout >/dev/null 2>&1; then
    f="$(sb)"; mk_wd_mock "$f"; wd_set "$f" lunarwing started; wd_hang "$f" lunarwing 30
    : > "$f/calls"
    start=$SECONDS
    oF="$(run_wd "$f" WD_CALL_LOG="$f/calls" LUNARWING_WATCHDOG_PROBE_TIMEOUT=1)"; rcF=$?
    dur=$((SECONDS - start))
    # status hangs (killed at 1s) -> reads not-active -> start succeeds -> the
    # verify status hangs too (killed at 1s) -> reported still-not-active.
    assert_eq "$rcF" "1" "F1: hung probes -> exit 1 (bounded failure, not a hang)"
    assert_contains "$oF" "still not active" "F2: hung verify reported as not active"
    assert_eq "$(starts "$f")" "1" "F3: hung status reads down -> exactly one start attempted"
    if [ "$dur" -le 20 ]; then ok "F4: whole run bounded by the probe timeout (${dur}s, not 3x30s)"
    else bad "F4: whole run bounded by the probe timeout" "run took ${dur}s"; fi
else
    ok "F1: SKIP (timeout unavailable)"
    ok "F2: SKIP (timeout unavailable)"
    ok "F3: SKIP (timeout unavailable)"
    ok "F4: SKIP (timeout unavailable)"
fi

# ── G: non-numeric probe timeout falls back to the default ────────────────────
echo "=== Section G: invalid probe timeout fallback ==="
g="$(sb)"; mk_wd_mock "$g"; wd_set "$g" lunarwing started
oG="$(run_wd "$g" LUNARWING_WATCHDOG_PROBE_TIMEOUT=bogus)"; rcG=$?
assert_eq "$rcG" "0" "G1: non-numeric timeout falls back, started service still exits 0"
assert_contains "$oG" "no action" "G2: non-numeric timeout falls back, no spurious action"

# ── H: held lock -> clean exit, no rc-service calls ───────────────────────────
echo "=== Section H: lock contention ==="
h="$(sb)"; mk_wd_mock "$h"; wd_set "$h" lunarwing stopped
: > "$h/calls"
if command -v flock >/dev/null 2>&1; then
    : > "$h/watchdog.lock"
    flock "$h/watchdog.lock" sleep 3 &
    lockpid=$!
    sleep 0.3
    oH="$(run_wd "$h" WD_CALL_LOG="$h/calls")"; rcH=$?
    wait "$lockpid" 2>/dev/null || true
    assert_eq "$rcH" "0" "H1: contended lock exits 0"
    assert_contains "$oH" "already active" "H2: lock contention reported"
    assert_eq "$(wc -l < "$h/calls")" "0" "H3: contended run never touches rc-service"
else
    ok "H1: SKIP (flock unavailable)"
    ok "H2: SKIP (flock unavailable)"
    ok "H3: SKIP (flock unavailable)"
fi

finish
