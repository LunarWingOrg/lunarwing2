#!/usr/bin/env bash
# Start WeeChat normally, or restore session files created by /upgrade -quit.
set -euo pipefail

WEECHAT_BIN=""
WEECHAT_HOME=""

die() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --weechat-bin)
            [[ $# -ge 2 ]] || die "--weechat-bin requires a value"
            WEECHAT_BIN="$2"
            shift 2
            ;;
        --weechat-home)
            [[ $# -ge 2 ]] || die "--weechat-home requires a value"
            WEECHAT_HOME="$2"
            shift 2
            ;;
        *) die "unknown argument: $1" ;;
    esac
done

[[ -n "$WEECHAT_BIN" ]] || die "--weechat-bin is required"
[[ -x "$WEECHAT_BIN" ]] || die "WeeChat binary is not executable: $WEECHAT_BIN"
[[ -n "$WEECHAT_HOME" ]] || die "--weechat-home is required"

args=(--dir "$WEECHAT_HOME")
if [[ -f "$WEECHAT_HOME/weechat.upgrade" ]]; then
    args=(--upgrade "${args[@]}")
fi

exec "$WEECHAT_BIN" "${args[@]}"
