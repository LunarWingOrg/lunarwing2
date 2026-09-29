#!/usr/bin/env bash
# Guard against lockfile-only pins of crates with active advisories that
# cargo-deny cannot see because the crate is never resolved into the built
# dependency graph (cargo-deny filters such crates out entirely).
#
# Concrete case (work item 255, RUSTSEC-2026-0176): jiter's optional `python`
# feature pulls pyo3 0.28.3, which is inside the advisory's affected range
# (>=0.24.0, <0.29.0). monty does not enable that feature, so pyo3 lands in
# Cargo.lock but never compiles — and `cargo deny check advisories` stays
# green. If a future monty/jiter change flips the feature on, the advisory
# goes live silently. This script makes that flip a loud CI failure instead.
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)

if ! command -v taskset >/dev/null 2>&1; then
    echo "ERROR: taskset is required for bounded Cargo execution" >&2
    exit 1
fi

# crate|advisory|explanation — extend when another lockfile-only pin with a
# live advisory shows up. Keep entries alphabetized by crate name.
guarded=(
    "pyo3|RUSTSEC-2026-0176|jiter's optional python feature would compile an OOB-read-affected pyo3 (<0.29.0)"
)

# Every workspace root whose Cargo.lock is committed to this repo.
lock_roots=(
    "$repo_root/ic"
    "$repo_root/ic/bridges/xmpp-bridge"
)

failed=0
for root in "${lock_roots[@]}"; do
    for entry in "${guarded[@]}"; do
        IFS='|' read -r crate advisory explanation <<<"$entry"
        # `cargo tree -i <crate>` exits 0 even when the crate is absent from
        # the graph (it just prints "warning: nothing to print"), so the
        # stdout must be inspected rather than the exit status.
        out=$(taskset -c 0-17 cargo tree -i "$crate" --target all \
            --manifest-path "$root/Cargo.toml" 2>&1 || true)
        if ! grep -q "nothing to print" <<<"$out"; then
            echo "ERROR: $crate is now built by $root — $advisory applies ($explanation)." >&2
            echo "       Resolve by upgrading past the advisory's patched range before merging." >&2
            failed=1
        fi
    done
done

if ((failed)); then
    exit 1
fi

echo "lockfile-only advisory guard: ok"
