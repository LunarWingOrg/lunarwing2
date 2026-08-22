#!/usr/bin/env bash
# Tests for ic/scripts/lunarwing-watchdog.sh (systemd watchdog oneshot).
#
# Self-contained mock (systemctl/curl) driven by a state directory — no real
# systemd bus and no real services are touched. Validates:
#   - active service -> no action
#   - inactive service -> reset-failed (when failed) + restart + verify
#   - restart command failure / stuck-inactive -> exit 1
#   - gateway deep check restarts on unhealthy channels only
#   - LUNARWING_WATCHDOG_SYSTEMD_SCOPE=user routes every probe to the user bus
#   - invalid scope warns and falls back to the system bus
#   - a held lock aborts the run cleanly (exit 0, no systemctl calls)
#
# State layout under <sb>/wdstate:
#   <svc>            -> "active" | "inactive" | "failed"
#   fail/<svc>       -> marker: `restart` returns non-zero
#   stuck/<svc>      -> marker: `restart` succeeds but the service stays down
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/lib.sh
source "$DIR/lib.sh"
WD="$DIR/../../ic/scripts/lunarwing-watchdog.sh"

[[ -x "$WD" ]] || { echo "FATAL: watchdog script not found/executable at $WD"; exit 1; }

mk_wd_mock() {
    local sb="$1" bin="$1/bin"
    mkdir -p "$bin" "$1/wdstate/fail" "$1/wdstate/stuck"

    cat > "$bin/systemctl" <<'EOF'
#!/usr/bin/env bash
SS="${WD_STATE_DIR:?}"
scope=system
while [ $# -gt 0 ]; do
  case "$1" in
    --user) scope=user; shift ;;
    *) break ;;
  esac
done
sub="${1:-}"; shift || true
[ -n "${WD_CALL_LOG:-}" ] && printf '%s %s %s\n' "$scope" "$sub" "$*" >> "$WD_CALL_LOG"
svc=""
case "$sub" in
  is-active|is-failed)
    for a in "$@"; do case "$a" in --quiet) ;; *) svc="$a" ;; esac; done
    state="$(cat "$SS/$svc" 2>/dev/null || echo unknown)"
    [ "$sub" = is-active ] && [ "$state" = active ] && exit 0
    [ "$sub" = is-failed ] && [ "$state" = failed ] && exit 0
    exit 1 ;;
  reset-failed)
    svc="${1:-}"
    [ "$(cat "$SS/$svc" 2>/dev/null)" = failed ] && echo inactive > "$SS/$svc"
    exit 0 ;;
  restart)
    svc="${1:-}"
    [ -f "$SS/fail/$svc" ] && { echo "mock systemctl: restart $svc failed" >&2; exit 1; }
    [ -f "$SS/stuck/$svc" ] || echo active > "$SS/$svc"
    exit 0 ;;
  show)
    unit=""; props=()
    while [ $# -gt 0 ]; do
      case "$1" in
        -p) props+=("$2"); shift 2 ;;
        --no-pager|--value) shift ;;
        *) unit="$1"; shift ;;
      esac
    done
    state="$(cat "$SS/$unit" 2>/dev/null || echo inactive)"
    for p in "${props[@]}"; do
      case "$p" in
        ActiveState) echo "ActiveState=$state" ;;
        SubState) [ "$state" = active ] && echo "SubState=running" || echo "SubState=dead" ;;
        NRestarts) echo "NRestarts=0" ;;
        MainPID) echo "MainPID=0" ;;
        Result) echo "Result=success" ;;
        ExecMainCode) echo "ExecMainCode=0" ;;
        ExecMainStatus) echo "ExecMainStatus=0" ;;
        *) echo "$p=" ;;
      esac
    done
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

run_wd() {  # <sb> [ENV=VALUE ...] -> stdout+stderr; rc via $@
    local sb="$1"; shift
    env PATH="$sb/bin:$PATH" WD_STATE_DIR="$sb/wdstate" \
        LUNARWING_WATCHDOG_SERVICE=lunarwing.service \
        LUNARWING_WATCHDOG_LOG="$sb/watchdog.log" \
        LUNARWING_WATCHDOG_LOCK="$sb/watchdog.lock" \
        LUNARWING_WATCHDOG_POST_RESTART_SLEEP_SECONDS=0 \
        "$@" bash "$WD" 2>&1
}

restarts() { grep -c ' restart ' "$1/calls" 2>/dev/null || true; }

# ── A: active service -> no action ────────────────────────────────────────────
echo "=== Section A: active service ==="
a="$(sb)"; mk_wd_mock "$a"; wd_set "$a" lunarwing.service active
: > "$a/calls"
oA="$(run_wd "$a" WD_CALL_LOG="$a/calls")"; rcA=$?
assert_eq "$rcA" "0" "A1: active service exits 0"
assert_contains "$oA" "no action" "A2: active service reports no action"
assert_eq "$(restarts "$a")" "0" "A3: active service is never restarted"

# ── B: inactive service -> restart + verify success ───────────────────────────
echo "=== Section B: inactive service restart ==="
b="$(sb)"; mk_wd_mock "$b"; wd_set "$b" lunarwing.service inactive
: > "$b/calls"
oB="$(run_wd "$b" WD_CALL_LOG="$b/calls")"; rcB=$?
assert_eq "$rcB" "0" "B1: inactive service restart exits 0"
assert_contains "$oB" "restart succeeded" "B2: restart success logged"
assert_eq "$(restarts "$b")" "1" "B3: exactly one restart issued"
assert_absent "$oB" "reset-failed" "B4: no reset-failed for a merely-inactive unit"

# ── C: failed (StartLimitBurst) service -> reset-failed before restart ────────
echo "=== Section C: failed service ==="
c="$(sb)"; mk_wd_mock "$c"; wd_set "$c" lunarwing.service failed
: > "$c/calls"
oC="$(run_wd "$c" WD_CALL_LOG="$c/calls")"; rcC=$?
assert_eq "$rcC" "0" "C1: failed service recovery exits 0"
assert_contains "$oC" "reset-failed" "C2: reset-failed attempted for failed unit"
assert_contains "$oC" "restart succeeded" "C3: restart after reset-failed succeeds"
if grep -q 'reset-failed lunarwing.service' "$c/calls" && grep -q 'restart lunarwing.service' "$c/calls"; then
    ok "C4: reset-failed and restart both issued"
else
    bad "C4: reset-failed and restart both issued" "$(cat "$c/calls")"
fi

# ── D: restart command fails -> exit 1 ────────────────────────────────────────
echo "=== Section D: restart command failure ==="
d="$(sb)"; mk_wd_mock "$d"; wd_set "$d" lunarwing.service inactive; wd_fail "$d" lunarwing.service
oD="$(run_wd "$d")"; rcD=$?
assert_eq "$rcD" "1" "D1: failed restart exits 1"
assert_contains "$oD" "restart command failed" "D2: restart failure logged"

# ── E: restart succeeds but service stays down -> exit 1 ─────────────────────
echo "=== Section E: stuck service ==="
e="$(sb)"; mk_wd_mock "$e"; wd_set "$e" lunarwing.service inactive; wd_stuck "$e" lunarwing.service
oE="$(run_wd "$e")"; rcE=$?
assert_eq "$rcE" "1" "E1: stuck service exits 1"
assert_contains "$oE" "still not active" "E2: stuck service reported"

# ── F: gateway deep check — unhealthy channels restart an active service ──────
echo "=== Section F: deep health check ==="
f="$(sb)"; mk_wd_mock "$f"; wd_set "$f" lunarwing.service active
printf '%s\n' '{"channel_health":{"xmpp":{"healthy":false},"web":{"healthy":true}}}' > "$f/gw.json"
: > "$f/calls"
oF="$(run_wd "$f" WD_CALL_LOG="$f/calls" WD_GATEWAY_JSON="$f/gw.json" \
    LUNARWING_WATCHDOG_GATEWAY_URL=http://127.0.0.1:1 LUNARWING_WATCHDOG_GATEWAY_TOKEN=t)"; rcF=$?
assert_eq "$rcF" "0" "F1: deep-check restart exits 0"
assert_contains "$oF" "channels unhealthy: xmpp" "F2: unhealthy channel named"
assert_eq "$(restarts "$f")" "1" "F3: deep check restarts the active service"

f2="$(sb)"; mk_wd_mock "$f2"; wd_set "$f2" lunarwing.service active
printf '%s\n' '{"channel_health":{"xmpp":{"healthy":true}}}' > "$f2/gw.json"
: > "$f2/calls"
oF2="$(run_wd "$f2" WD_CALL_LOG="$f2/calls" WD_GATEWAY_JSON="$f2/gw.json" \
    LUNARWING_WATCHDOG_GATEWAY_URL=http://127.0.0.1:1 LUNARWING_WATCHDOG_GATEWAY_TOKEN=t)"
assert_contains "$oF2" "no action" "F4: all-healthy channels -> no restart"
assert_eq "$(restarts "$f2")" "0" "F5: all-healthy channels -> zero restarts"

f3="$(sb)"; mk_wd_mock "$f3"; wd_set "$f3" lunarwing.service active
: > "$f3/calls"
oF3="$(run_wd "$f3" WD_CALL_LOG="$f3/calls")"
assert_contains "$oF3" "no action" "F6: no gateway url/token -> deep check skipped"
assert_eq "$(restarts "$f3")" "0" "F7: deep check skipped -> zero restarts"

f4="$(sb)"; mk_wd_mock "$f4"; wd_set "$f4" lunarwing.service active
: > "$f4/calls"
oF4="$(run_wd "$f4" WD_CALL_LOG="$f4/calls" \
    LUNARWING_WATCHDOG_GATEWAY_URL=http://127.0.0.1:1 LUNARWING_WATCHDOG_GATEWAY_TOKEN=t)"
assert_contains "$oF4" "no action" "F8: unreachable gateway (curl fail) -> no spurious restart"
assert_eq "$(restarts "$f4")" "0" "F9: unreachable gateway -> zero restarts"

# ── G: user scope routes every systemctl call to the user bus ─────────────────
echo "=== Section G: systemd user scope ==="
g="$(sb)"; mk_wd_mock "$g"; wd_set "$g" lunarwing.service inactive
: > "$g/calls"
oG="$(run_wd "$g" WD_CALL_LOG="$g/calls" LUNARWING_WATCHDOG_SYSTEMD_SCOPE=user)"; rcG=$?
assert_eq "$rcG" "0" "G1: user-scope restart exits 0"
assert_contains "$oG" "restart succeeded" "G2: user-scope restart success logged"
if [ -s "$g/calls" ] && ! grep -q '^system ' "$g/calls"; then
    ok "G3: every systemctl call is on the user bus"
else
    bad "G3: every systemctl call is on the user bus" "$(cat "$g/calls")"
fi

# ── H: invalid scope warns and falls back to the system bus ──────────────────
echo "=== Section H: invalid scope fallback ==="
h="$(sb)"; mk_wd_mock "$h"; wd_set "$h" lunarwing.service active
: > "$h/calls"
oH="$(run_wd "$h" WD_CALL_LOG="$h/calls" LUNARWING_WATCHDOG_SYSTEMD_SCOPE=bogus)"; rcH=$?
assert_eq "$rcH" "0" "H1: invalid scope still exits 0"
assert_contains "$oH" "invalid LUNARWING_WATCHDOG_SYSTEMD_SCOPE" "H2: invalid scope warning logged"
if ! grep -q '^user ' "$h/calls"; then
    ok "H3: invalid scope falls back to the system bus"
else
    bad "H3: invalid scope falls back to the system bus" "$(cat "$h/calls")"
fi

# ── I: held lock -> clean exit, no systemctl calls ────────────────────────────
echo "=== Section I: lock contention ==="
i="$(sb)"; mk_wd_mock "$i"; wd_set "$i" lunarwing.service inactive
: > "$i/calls"
if command -v flock >/dev/null 2>&1; then
    : > "$i/watchdog.lock"
    flock "$i/watchdog.lock" sleep 3 &
    lockpid=$!
    sleep 0.3
    oI="$(run_wd "$i" WD_CALL_LOG="$i/calls")"; rcI=$?
    wait "$lockpid" 2>/dev/null || true
    assert_eq "$rcI" "0" "I1: contended lock exits 0"
    assert_contains "$oI" "already active" "I2: lock contention reported"
    assert_eq "$(wc -l < "$i/calls")" "0" "I3: contended run never touches systemctl"
else
    ok "I1: SKIP (flock unavailable)"
    ok "I2: SKIP (flock unavailable)"
    ok "I3: SKIP (flock unavailable)"
fi

finish
