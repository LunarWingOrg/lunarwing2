#!/usr/bin/env bash
# Source this after a bootstrap failure so the compiler cache cannot fail the job.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    echo "ERROR: disable-sccache.sh must be sourced by the Rust job shell" >&2
    exit 1
fi

echo "WARNING: sccache is unavailable; continuing without compiler cache (RUSTC_WRAPPER and SCCACHE_* unset)" >&2
unset RUSTC_WRAPPER
while IFS= read -r variable; do
    unset "$variable"
done < <(compgen -e SCCACHE_)
return 0
