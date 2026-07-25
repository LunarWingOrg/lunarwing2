#!/usr/bin/env bash
# Opt-in real WeeChat stop/start cycle. Default CI run skips this host test.
set -euo pipefail

if [[ "${RUN_LIVE_WEECHAT_RESTORE:-0}" != 1 ]]; then
    echo "SKIP: set RUN_LIVE_WEECHAT_RESTORE=1 to test real session restoration"
    exit 0
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
START_HELPER="$SCRIPT_DIR/../lunarwing-weechat-start.sh"
STOP_HELPER="$SCRIPT_DIR/../lunarwing-weechat-stop.sh"
TMUX_BIN="$(command -v tmux 2>/dev/null || true)"
WEECHAT_BIN="$(command -v weechat 2>/dev/null || true)"
SOCKET="lw-weechat-restore-$$"
SESSION="weechat"

[[ -n "$TMUX_BIN" ]] || { echo "FAIL: tmux not installed" >&2; exit 1; }
[[ -n "$WEECHAT_BIN" ]] || { echo "FAIL: WeeChat not installed" >&2; exit 1; }

mkdir -p /tmp/opencode
WEECHAT_HOME="$(mktemp -d /tmp/opencode/weechat-restore.XXXXXX)"

cleanup() {
    local attempt
    "$TMUX_BIN" -L "$SOCKET" kill-server 2>/dev/null || true
    for attempt in 1 2 3; do
        rm -rf "$WEECHAT_HOME" 2>/dev/null && return 0
        sleep 1
    done
    rm -rf "$WEECHAT_HOME" 2>/dev/null || true
}
trap cleanup EXIT

wait_for_fifo() {
    local attempt candidate
    for ((attempt = 0; attempt < 30; attempt++)); do
        for candidate in "$WEECHAT_HOME"/weechat_fifo_*; do
            if [[ -p "$candidate" ]]; then
                printf '%s\n' "$candidate"
                return 0
            fi
        done
        sleep 1
    done
    return 1
}

wait_for_restored_buffer() {
    local attempt output
    for ((attempt = 0; attempt < 30; attempt++)); do
        output="$("$TMUX_BIN" -L "$SOCKET" capture-pane -p -t "$SESSION" 2>/dev/null || true)"
        if [[ "$output" == *"irc/restoretest"* ]]; then
            return 0
        fi
        sleep 1
    done
    return 1
}

start_weechat() {
    "$TMUX_BIN" -L "$SOCKET" new-session -d -s "$SESSION" \
        "$START_HELPER --weechat-bin $WEECHAT_BIN --weechat-home $WEECHAT_HOME"
}

start_weechat
FIFO="$(wait_for_fifo)" || { echo "FAIL: WeeChat FIFO not created" >&2; exit 1; }
printf '%s\n' \
    '*/server add restoretest 127.0.0.1/9 -notls' \
    '*/connect restoretest' >"$FIFO"
wait_for_restored_buffer || { echo "FAIL: fixture buffer not created" >&2; exit 1; }

"$STOP_HELPER" \
    --weechat-home "$WEECHAT_HOME" \
    --tmux-socket "$SOCKET" \
    --session "$SESSION" \
    --timeout 20 \
    --write-timeout 3
[[ -f "$WEECHAT_HOME/weechat.upgrade" ]] \
    || { echo "FAIL: graceful stop did not create weechat.upgrade" >&2; exit 1; }

start_weechat
wait_for_restored_buffer || { echo "FAIL: buffer did not return after --upgrade" >&2; exit 1; }
[[ ! -e "$WEECHAT_HOME/weechat.upgrade" ]] \
    || { echo "FAIL: --upgrade did not consume weechat.upgrade" >&2; exit 1; }
for upgrade_file in "$WEECHAT_HOME"/*.upgrade; do
    [[ ! -e "$upgrade_file" ]] \
        || { echo "FAIL: --upgrade left session file: $upgrade_file" >&2; exit 1; }
done

"$STOP_HELPER" \
    --weechat-home "$WEECHAT_HOME" \
    --tmux-socket "$SOCKET" \
    --session "$SESSION" \
    --timeout 20 \
    --write-timeout 3

echo "PASS: real WeeChat session restored irc/restoretest buffer"
