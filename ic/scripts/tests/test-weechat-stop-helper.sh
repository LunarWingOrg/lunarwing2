#!/usr/bin/env bash
# Tests for lunarwing-weechat-stop.sh — graceful stop via /upgrade -quit FIFO
# with fallback to tmux kill-session.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STOP_HELPER="$SCRIPT_DIR/../lunarwing-weechat-stop.sh"
TMUX_BIN="$(command -v tmux 2>/dev/null || true)"
FAILURES=0

if [[ -z "$TMUX_BIN" ]]; then
    echo "SKIP: tmux not installed"
    exit 77
fi

pass() { echo "  PASS: $1"; }
fail() { echo "  FAIL: $1"; FAILURES=$((FAILURES + 1)); }

# ── Test 1: no tmux session → exit 0 (already stopped) ─────────────────────
echo "=== no session: exit 0 ==="
out="$("$STOP_HELPER" \
    --weechat-home /tmp/nonexistent-weechat-home \
    --tmux-socket "lw-test-$$-nosession" \
    --session weechat 2>&1)"
rc=$?
if [[ "$rc" -eq 0 ]]; then
    pass "returns 0 when no session exists"
else
    fail "expected exit 0, got $rc: $out"
fi

# ── Test 2: --weechat-home required ─────────────────────────────────────────
echo "=== missing --weechat-home: nonzero ==="
if "$STOP_HELPER" --tmux-socket foo 2>/dev/null; then
    fail "should require --weechat-home"
else
    pass "rejects missing --weechat-home"
fi

# ── Test 3: --tmux-socket required ──────────────────────────────────────────
echo "=== missing --tmux-socket: nonzero ==="
if "$STOP_HELPER" --weechat-home /tmp/foo 2>/dev/null; then
    fail "should require --tmux-socket"
else
    pass "rejects missing --tmux-socket"
fi

# ── Test 4: unknown arg rejected ────────────────────────────────────────────
echo "=== unknown arg: nonzero ==="
if "$STOP_HELPER" --weechat-home /tmp/foo --tmux-socket foo --bogus 2>/dev/null; then
    fail "should reject unknown argument"
else
    pass "rejects unknown argument"
fi

# ── Test 5: no FIFO → falls back to kill-session ────────────────────────────
echo "=== no FIFO: tmux kill-session fallback ==="
SOCKET="lw-test-$$-fallback"
WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"; "$TMUX_BIN" -L "$SOCKET" kill-session -t weechat 2>/dev/null || true' EXIT

"$TMUX_BIN" -L "$SOCKET" new-session -d -s weechat "sleep 300" 2>/dev/null
if ! "$TMUX_BIN" -L "$SOCKET" has-session -t weechat 2>/dev/null; then
    fail "setup: could not create tmux session"
else
    # No FIFO in WORKDIR, so helper should kill the session directly
    "$STOP_HELPER" \
        --weechat-home "$WORKDIR" \
        --tmux-socket "$SOCKET" \
        --session weechat
    if "$TMUX_BIN" -L "$SOCKET" has-session -t weechat 2>/dev/null; then
        fail "tmux session still alive after stop helper (fallback kill-session)"
    else
        pass "tmux session killed via fallback (no FIFO)"
    fi
fi

# ── Summary ─────────────────────────────────────────────────────────────────
echo ""
if [[ "$FAILURES" -eq 0 ]]; then
    echo "ALL TESTS PASSED"
    exit 0
else
    echo "$FAILURES TEST(S) FAILED"
    exit 1
fi
