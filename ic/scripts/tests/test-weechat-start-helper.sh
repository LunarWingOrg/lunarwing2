#!/usr/bin/env bash
# Tests conditional restoration through WeeChat's --upgrade startup flag.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
START_HELPER="$SCRIPT_DIR/../lunarwing-weechat-start.sh"
FAILURES=0
WORKDIR="$(mktemp -d)"
FAKE_WEECHAT="$WORKDIR/fake-weechat"
FAKE_WEECHAT_ARGS="$WORKDIR/args"
trap 'rm -rf "$WORKDIR"' EXIT

cat >"$FAKE_WEECHAT" <<'WEECHAT'
#!/usr/bin/env bash
printf '%s\n' "$@" >"$FAKE_WEECHAT_ARGS"
WEECHAT
chmod +x "$FAKE_WEECHAT"
export FAKE_WEECHAT_ARGS

pass() { echo "  PASS: $1"; }
fail() { echo "  FAIL: $1"; FAILURES=$((FAILURES + 1)); }

home="$WORKDIR/home with spaces"
mkdir -p "$home"

echo "=== normal start without saved session ==="
if "$START_HELPER" --weechat-bin "$FAKE_WEECHAT" --weechat-home "$home"; then
    expected="$(printf '%s\n' --dir "$home")"
    if [[ "$(<"$FAKE_WEECHAT_ARGS")" == "$expected" ]]; then
        pass "starts normally when no upgrade state exists"
    else
        fail "normal start arguments differ"
    fi
else
    fail "normal start returned nonzero"
fi

echo "=== restore start with saved session ==="
touch "$home/weechat.upgrade"
if "$START_HELPER" --weechat-bin "$FAKE_WEECHAT" --weechat-home "$home"; then
    expected="$(printf '%s\n' --upgrade --dir "$home")"
    if [[ "$(<"$FAKE_WEECHAT_ARGS")" == "$expected" ]]; then
        pass "adds --upgrade when saved session exists"
    else
        fail "restore start arguments differ"
    fi
else
    fail "restore start returned nonzero"
fi

echo "=== invalid arguments ==="
if "$START_HELPER" --weechat-home "$home" 2>/dev/null; then
    fail "should require --weechat-bin"
else
    pass "requires --weechat-bin"
fi
if "$START_HELPER" --weechat-bin "$WORKDIR/missing" --weechat-home "$home" 2>/dev/null; then
    fail "should reject non-executable WeeChat binary"
else
    pass "rejects non-executable WeeChat binary"
fi
if "$START_HELPER" --weechat-bin "$FAKE_WEECHAT" --weechat-home "$home" --bogus 2>/dev/null; then
    fail "should reject unknown argument"
else
    pass "rejects unknown argument"
fi

echo ""
if [[ "$FAILURES" -eq 0 ]]; then
    echo "ALL TESTS PASSED"
    exit 0
fi
echo "$FAILURES TEST(S) FAILED"
exit 1
