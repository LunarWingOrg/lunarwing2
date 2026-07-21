#!/usr/bin/env bash
# lunarwing-weechat-stop.sh — gracefully stop WeeChat, preserving buffer state.
#
# Sends /upgrade -quit via the WeeChat FIFO pipe so WeeChat saves its full
# session (buffers, lines, IRC connections) and exits. On next start, WeeChat
# restores the saved session. Falls back to tmux kill-session if the FIFO is
# unavailable or the graceful stop times out.
#
# This runs as the tenant user (via systemd user units or OpenRC su drop).
set -uo pipefail

WEECHAT_HOME=""
TMUX_SOCKET=""
TMUX_SESSION="weechat"
TIMEOUT=20

while [[ $# -gt 0 ]]; do
    case "$1" in
        --weechat-home) WEECHAT_HOME="$2"; shift 2 ;;
        --tmux-socket)  TMUX_SOCKET="$2";  shift 2 ;;
        --session)      TMUX_SESSION="$2"; shift 2 ;;
        --timeout)      TIMEOUT="$2";      shift 2 ;;
        *) printf 'error: unknown argument: %s\n' "$1" >&2; exit 1 ;;
    esac
done

[[ -n "$WEECHAT_HOME" ]] || { printf 'error: --weechat-home is required\n' >&2; exit 1; }
[[ -n "$TMUX_SOCKET" ]]  || { printf 'error: --tmux-socket is required\n' >&2; exit 1; }

TMUX_BIN="$(command -v tmux 2>/dev/null || true)"
[[ -n "$TMUX_BIN" ]] || { printf 'error: tmux not found\n' >&2; exit 1; }

if ! "$TMUX_BIN" -L "$TMUX_SOCKET" has-session -t "$TMUX_SESSION" 2>/dev/null; then
    exit 0
fi

FIFO=""
if [[ -d "$WEECHAT_HOME" ]]; then
    FIFO="$(ls "$WEECHAT_HOME"/weechat_fifo_* 2>/dev/null | head -1 || true)"
fi

if [[ -n "$FIFO" && -p "$FIFO" ]]; then
    if printf '*:/upgrade -quit\n' >"$FIFO" 2>/dev/null; then
        i=0
        while "$TMUX_BIN" -L "$TMUX_SOCKET" has-session -t "$TMUX_SESSION" 2>/dev/null; do
            i=$((i + 1))
            if [[ "$i" -ge "$TIMEOUT" ]]; then
                printf 'warning: WeeChat did not exit after %ss; falling back to kill-session\n' \
                    "$TIMEOUT" >&2
                break
            fi
            sleep 1
        done
        if ! "$TMUX_BIN" -L "$TMUX_SOCKET" has-session -t "$TMUX_SESSION" 2>/dev/null; then
            exit 0
        fi
    fi
fi

"$TMUX_BIN" -L "$TMUX_SOCKET" kill-session -t "$TMUX_SESSION" 2>/dev/null || true
exit 0
