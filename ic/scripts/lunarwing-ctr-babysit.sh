#!/bin/sh
# lunarwing-ctr-babysit — rootless container supervision for OpenRC.
# Usage: lunarwing-ctr-babysit <container> [egress-guard ruleset chain]
set -eu

if [ "$#" -ne 1 ] && [ "$#" -ne 4 ]; then
    echo "usage: lunarwing-ctr-babysit <container> [egress-guard ruleset chain]" >&2
    exit 1
fi
ctr="$1"
guard="${2:-}"
ruleset="${3:-}"
chain="${4:-}"

if [ "$#" -eq 4 ]; then
    [ -x "$guard" ] || {
        echo "worker egress guard is missing or not executable: $guard" >&2
        exit 1
    }
    "$guard" "$ruleset" rootless "$chain"
fi

# Non-worker callers retain the historical idempotent best-effort start. Worker
# starts fail closed: a guard was required above, and a failed start must not be
# followed by a successful-looking podman wait/respawn cycle.
if ! podman start "$ctr" >/dev/null 2>&1; then
    [ "$#" -eq 1 ] || exit 1
fi

if [ "$#" -eq 4 ]; then
    if ! "$guard" --verify "$ruleset" rootless "$chain"; then
        podman stop "$ctr" >/dev/null 2>&1 || true
        echo "worker egress filter vanished after $ctr started; stopped it" >&2
        exit 1
    fi
fi

exec podman wait "$ctr"
