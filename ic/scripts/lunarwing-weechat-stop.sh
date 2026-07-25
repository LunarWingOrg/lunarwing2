#!/usr/bin/env bash
# lunarwing-weechat-stop.sh - gracefully stop WeeChat, preserving buffer state.
#
# Sends /upgrade -quit via the WeeChat FIFO so WeeChat writes session files and
# exits. The start helper consumes those files with --upgrade. If preservation
# fails, this falls back to tmux kill-session rather than hanging shutdown.
#
# This runs as the tenant user (systemd user unit or OpenRC start-stop-daemon).
set -uo pipefail

WEECHAT_HOME=""
TMUX_SOCKET=""
TMUX_SESSION="weechat"
TIMEOUT=20
WRITE_TIMEOUT=3

die() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --weechat-home)
            [[ $# -ge 2 ]] || die "--weechat-home requires a value"
            WEECHAT_HOME="$2"
            shift 2
            ;;
        --tmux-socket)
            [[ $# -ge 2 ]] || die "--tmux-socket requires a value"
            TMUX_SOCKET="$2"
            shift 2
            ;;
        --session)
            [[ $# -ge 2 ]] || die "--session requires a value"
            TMUX_SESSION="$2"
            shift 2
            ;;
        --timeout)
            [[ $# -ge 2 ]] || die "--timeout requires a value"
            TIMEOUT="$2"
            shift 2
            ;;
        --write-timeout)
            [[ $# -ge 2 ]] || die "--write-timeout requires a value"
            WRITE_TIMEOUT="$2"
            shift 2
            ;;
        *) die "unknown argument: $1" ;;
    esac
done

[[ -n "$WEECHAT_HOME" ]] || die "--weechat-home is required"
[[ -n "$TMUX_SOCKET" ]] || die "--tmux-socket is required"
[[ "$TIMEOUT" =~ ^[1-9][0-9]*$ ]] || die "--timeout must be a positive integer"
[[ "$WRITE_TIMEOUT" =~ ^[1-9][0-9]*$ ]] \
    || die "--write-timeout must be a positive integer"

TMUX_BIN="$(command -v tmux 2>/dev/null || true)"
[[ -n "$TMUX_BIN" ]] || die "tmux not found"

tmux_session_state() {
    local output status
    output="$("$TMUX_BIN" -L "$TMUX_SOCKET" has-session -t "$TMUX_SESSION" 2>&1)"
    status=$?
    if [[ "$status" -eq 0 ]]; then
        return 0
    fi
    case "$output" in
        *"can't find session:"*|*"no server running on "*|*"error connecting to "*"No such file or directory"*) return 1 ;;
    esac
    printf 'error: unable to query WeeChat tmux session: %s\n' "$output" >&2
    return 2
}

tmux_session_state
initial_state=$?
case "$initial_state" in
    0) ;;
    1) exit 0 ;;
    *) exit 1 ;;
esac

process_belongs_to_pane() {
    local pid="$1" pane_pid="$2" stat rest parent hops=0
    while [[ "$pid" =~ ^[1-9][0-9]*$ ]] && (( hops < 32 )); do
        [[ "$pid" == "$pane_pid" ]] && return 0
        [[ -r "/proc/$pid/stat" ]] || return 1
        stat="$(<"/proc/$pid/stat")"
        rest="${stat##*) }"
        read -r _ parent _ <<<"$rest"
        [[ "$parent" != "$pid" ]] || return 1
        pid="$parent"
        hops=$((hops + 1))
    done
    return 1
}

find_live_fifo() {
    local candidate pane_pid pid
    [[ -d "$WEECHAT_HOME" ]] || return 1
    pane_pid="$(
        "$TMUX_BIN" -L "$TMUX_SOCKET" display-message \
            -p -t "$TMUX_SESSION" '#{pane_pid}' 2>/dev/null || true
    )"
    [[ "$pane_pid" =~ ^[1-9][0-9]*$ ]] || return 1
    for candidate in "$WEECHAT_HOME"/weechat_fifo_*; do
        [[ -p "$candidate" ]] || continue
        pid="${candidate##*_}"
        [[ "$pid" =~ ^[1-9][0-9]*$ ]] || continue
        if kill -0 "$pid" 2>/dev/null && process_belongs_to_pane "$pid" "$pane_pid"; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

write_upgrade_command() {
    local fifo="$1" writer_pid watchdog_pid status
    (printf '*/upgrade -quit\n' >"$fifo") 2>/dev/null &
    writer_pid=$!
    (sleep "$WRITE_TIMEOUT"; kill "$writer_pid" 2>/dev/null || true) &
    watchdog_pid=$!
    wait "$writer_pid"
    status=$?
    kill "$watchdog_pid" 2>/dev/null || true
    wait "$watchdog_pid" 2>/dev/null || true
    return "$status"
}

fallback_stop() {
    local state
    "$TMUX_BIN" -L "$TMUX_SOCKET" kill-session -t "$TMUX_SESSION" 2>/dev/null || true
    tmux_session_state
    state=$?
    case "$state" in
        1) return 0 ;;
        0) printf 'error: WeeChat tmux session survived kill-session fallback\n' >&2 ;;
        *) printf 'error: unable to verify WeeChat tmux session stopped\n' >&2 ;;
    esac
    return 1
}

FIFO="$(find_live_fifo || true)"
if [[ -n "$FIFO" ]]; then
    if write_upgrade_command "$FIFO"; then
        i=0
        while [[ "$i" -lt "$TIMEOUT" ]]; do
            tmux_session_state
            state=$?
            if [[ "$state" -eq 1 ]]; then
                exit 0
            fi
            if [[ "$state" -ne 0 ]]; then
                printf 'warning: unable to verify graceful WeeChat exit; falling back to kill-session\n' >&2
                break
            fi
            sleep 1
            i=$((i + 1))
        done
        if [[ "$i" -ge "$TIMEOUT" ]]; then
            printf 'warning: WeeChat did not exit after %ss; falling back to kill-session\n' \
                "$TIMEOUT" >&2
        fi
    else
        printf 'warning: WeeChat FIFO write failed or timed out; falling back to kill-session\n' >&2
    fi
else
    printf 'warning: no live WeeChat FIFO found; falling back to kill-session\n' >&2
fi

fallback_stop
