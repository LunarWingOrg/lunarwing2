#!/usr/bin/env bash
# Tests graceful WeeChat session preservation and bounded destructive fallback.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STOP_HELPER="$SCRIPT_DIR/../lunarwing-weechat-stop.sh"
FAILURES=0
WORKDIR="$(mktemp -d)"
FAKE_BIN="$WORKDIR/bin"
FAKE_TMUX_STATE="$WORKDIR/tmux.state"
FAKE_TMUX_ACTIONS="$WORKDIR/tmux.actions"
BACKGROUND_PIDS=()

cleanup() {
    local pid
    for pid in "${BACKGROUND_PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
    rm -rf "$WORKDIR"
}
trap cleanup EXIT

mkdir -p "$FAKE_BIN"
cat >"$FAKE_BIN/tmux" <<'TMUX'
#!/usr/bin/env bash
case " $* " in
    *" has-session "*)
        if [[ "${FAKE_TMUX_PROBE_ERROR:-0}" == 1 ]]; then
            printf 'permission denied\n' >&2
            exit 1
        fi
        if [[ -f "$FAKE_TMUX_STATE" ]]; then
            exit 0
        fi
        if [[ "${FAKE_TMUX_NO_SERVER:-0}" == 1 ]]; then
            printf 'error connecting to /tmp/tmux-test/socket (No such file or directory)\n' >&2
        else
            printf "can't find session: weechat\n" >&2
        fi
        exit 1
        ;;
    *" display-message "*)
        [[ -n "${FAKE_TMUX_PANE_PID:-}" ]] || exit 1
        printf '%s\n' "$FAKE_TMUX_PANE_PID"
        ;;
    *" kill-session "*)
        printf 'kill-session\n' >>"$FAKE_TMUX_ACTIONS"
        if [[ "${FAKE_TMUX_KILL_FAIL:-0}" == 1 ]]; then
            exit 1
        fi
        rm -f "$FAKE_TMUX_STATE"
        ;;
    *) exit 2 ;;
esac
TMUX
chmod +x "$FAKE_BIN/tmux"
export FAKE_TMUX_STATE FAKE_TMUX_ACTIONS
export PATH="$FAKE_BIN:$PATH"

pass() { echo "  PASS: $1"; }
fail() { echo "  FAIL: $1"; FAILURES=$((FAILURES + 1)); }

reset_tmux() {
    rm -f "$FAKE_TMUX_STATE" "$FAKE_TMUX_ACTIONS"
    unset FAKE_TMUX_KILL_FAIL FAKE_TMUX_NO_SERVER FAKE_TMUX_PANE_PID FAKE_TMUX_PROBE_ERROR
}

echo "=== no session: exit 0 ==="
reset_tmux
out="$("$STOP_HELPER" \
    --weechat-home "$WORKDIR/missing-home" \
    --tmux-socket no-session \
    --session weechat 2>&1)"
rc=$?
if [[ "$rc" -eq 0 ]]; then
    pass "returns 0 when no session exists"
else
    fail "expected exit 0, got $rc: $out"
fi

echo "=== absent tmux server: exit 0 ==="
reset_tmux
export FAKE_TMUX_NO_SERVER=1
if "$STOP_HELPER" \
    --weechat-home "$WORKDIR/missing-home" \
    --tmux-socket absent-server \
    --session weechat >/dev/null 2>&1; then
    pass "treats missing tmux server as already stopped"
else
    fail "missing tmux server should not be a stop failure"
fi

echo "=== tmux probe error: nonzero ==="
reset_tmux
export FAKE_TMUX_PROBE_ERROR=1
if "$STOP_HELPER" \
    --weechat-home "$WORKDIR/missing-home" \
    --tmux-socket denied \
    --session weechat >/dev/null 2>&1; then
    fail "should not treat tmux probe error as stopped"
else
    pass "reports tmux probe error"
fi

echo "=== required and validated arguments ==="
if "$STOP_HELPER" --tmux-socket foo 2>/dev/null; then
    fail "should require --weechat-home"
else
    pass "rejects missing --weechat-home"
fi
if "$STOP_HELPER" --weechat-home /tmp/foo 2>/dev/null; then
    fail "should require --tmux-socket"
else
    pass "rejects missing --tmux-socket"
fi
if "$STOP_HELPER" --weechat-home /tmp/foo --tmux-socket foo --timeout nope 2>/dev/null; then
    fail "should reject non-numeric timeout"
else
    pass "rejects non-numeric timeout"
fi
if "$STOP_HELPER" --weechat-home /tmp/foo --tmux-socket foo --bogus 2>/dev/null; then
    fail "should reject unknown argument"
else
    pass "rejects unknown argument"
fi

echo "=== live FIFO: exact upgrade command preserves session ==="
reset_tmux
home="$WORKDIR/live-home"
capture="$WORKDIR/fifo-command"
mkdir -p "$home"
touch "$FAKE_TMUX_STATE"
sleep 300 &
owner_pid=$!
BACKGROUND_PIDS+=("$owner_pid")
export FAKE_TMUX_PANE_PID="$owner_pid"
fifo="$home/weechat_fifo_$owner_pid"
mkfifo "$fifo"
(
    IFS= read -r command <"$fifo"
    printf '%s\n' "$command" >"$capture"
    rm -f "$FAKE_TMUX_STATE"
) &
reader_pid=$!
BACKGROUND_PIDS+=("$reader_pid")

if "$STOP_HELPER" \
    --weechat-home "$home" \
    --tmux-socket live \
    --session weechat \
    --timeout 3 \
    --write-timeout 1; then
    wait "$reader_pid" 2>/dev/null || true
    if [[ "$(<"$capture")" == '*/upgrade -quit' ]]; then
        pass "writes exact WeeChat FIFO upgrade command"
    else
        fail "unexpected FIFO command: $(<"$capture")"
    fi
    if [[ ! -e "$FAKE_TMUX_ACTIONS" ]]; then
        pass "graceful exit avoids kill-session fallback"
    else
        fail "graceful exit unexpectedly used kill-session"
    fi
else
    fail "graceful FIFO stop returned nonzero"
fi

echo "=== stale live PID: selects FIFO associated with tmux pane ==="
reset_tmux
associated_home="$WORKDIR/associated-home"
associated_capture="$WORKDIR/associated-command"
mkdir -p "$associated_home"
touch "$FAKE_TMUX_STATE"
sleep 300 &
unrelated_pid=$!
BACKGROUND_PIDS+=("$unrelated_pid")
sleep 300 &
associated_pid=$!
BACKGROUND_PIDS+=("$associated_pid")
export FAKE_TMUX_PANE_PID="$associated_pid"
mkfifo "$associated_home/weechat_fifo_$unrelated_pid"
associated_fifo="$associated_home/weechat_fifo_$associated_pid"
mkfifo "$associated_fifo"
(
    IFS= read -r command <"$associated_fifo"
    printf '%s\n' "$command" >"$associated_capture"
    rm -f "$FAKE_TMUX_STATE"
) &
associated_reader_pid=$!
BACKGROUND_PIDS+=("$associated_reader_pid")
if "$STOP_HELPER" \
    --weechat-home "$associated_home" \
    --tmux-socket associated \
    --session weechat \
    --timeout 3 \
    --write-timeout 1; then
    wait "$associated_reader_pid" 2>/dev/null || true
    if [[ "$(<"$associated_capture")" == '*/upgrade -quit' ]]; then
        pass "ignores unrelated live-PID FIFO"
    else
        fail "did not write associated FIFO"
    fi
else
    fail "associated FIFO stop returned nonzero"
fi

echo "=== blocked FIFO: bounded fallback ==="
reset_tmux
blocked_home="$WORKDIR/blocked-home"
mkdir -p "$blocked_home"
touch "$FAKE_TMUX_STATE"
sleep 300 &
blocked_owner_pid=$!
BACKGROUND_PIDS+=("$blocked_owner_pid")
export FAKE_TMUX_PANE_PID="$blocked_owner_pid"
mkfifo "$blocked_home/weechat_fifo_$blocked_owner_pid"
SECONDS=0
if "$STOP_HELPER" \
    --weechat-home "$blocked_home" \
    --tmux-socket blocked \
    --session weechat \
    --timeout 1 \
    --write-timeout 1 >/dev/null 2>&1; then
    if (( SECONDS <= 4 )) && grep -q '^kill-session$' "$FAKE_TMUX_ACTIONS"; then
        pass "blocked FIFO falls back without hanging"
    else
        fail "blocked FIFO fallback exceeded bound or skipped kill-session"
    fi
else
    fail "bounded fallback should stop session successfully"
fi

echo "=== failed fallback: nonzero ==="
reset_tmux
touch "$FAKE_TMUX_STATE"
export FAKE_TMUX_KILL_FAIL=1
if "$STOP_HELPER" \
    --weechat-home "$WORKDIR/no-fifo" \
    --tmux-socket failed-kill \
    --session weechat >/dev/null 2>&1; then
    fail "should fail when tmux session survives fallback"
else
    pass "reports surviving tmux session"
fi

echo ""
if [[ "$FAILURES" -eq 0 ]]; then
    echo "ALL TESTS PASSED"
    exit 0
fi
echo "$FAILURES TEST(S) FAILED"
exit 1
