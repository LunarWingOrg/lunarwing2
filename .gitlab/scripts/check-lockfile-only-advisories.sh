#!/usr/bin/env bash
# Guard against lockfile-pinned crates with live advisories that cargo-deny
# cannot see because the crate is never resolved into the built dependency
# graph (cargo-deny filters such crates out entirely).
#
# Concrete case (work item 255, RUSTSEC-2026-0176): jiter's optional `python`
# feature pulls pyo3 0.28.3, which is inside the advisory's affected range
# (>=0.24.0, <0.29.0). monty does not enable that feature, so pyo3 lands in
# Cargo.lock but never compiles — and `cargo deny check advisories` stays
# green. If a future monty/jiter change flips the feature on, the advisory
# goes live silently. This script makes that flip a loud CI failure instead.
#
# Graph model (v2.1.5.0 rework, MR !1371 review):
#   * Queries run `cargo tree --workspace --all-features --target all`, the
#     same graph CI compiles (rust-check-all-features / rust-clippy-all).
#     --workspace matters: the vendored libsignal-protocol{,-sys} path deps
#     are automatic workspace members, so their dev-dependencies (e.g.
#     env_logger 0.6.2 -> atty) DO resolve, and a root-package-only query
#     falsely reports them absent.
#   * Each entry declares a mode:
#       absent — the crate must not appear anywhere in the full resolve
#                graph, dev edges included; compiling it at all (even in
#                tests) is the hazard (pyo3).
#       no-dev — the crate may exist behind dev-dependencies, but must not
#                appear in the production (non-dev) build graph (atty:
#                Windows-only unaligned read, only compiled by the vendored
#                libsignal-protocol's own test harness).
#   * A guard entry is only meaningful while the crate stays pinned in a
#     guarded lockfile. If the pin disappears (upgrade, rename, or a typo in
#     the table below) the guard fails loudly as stale instead of passing
#     vacuously on a "did not match any packages" marker.
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)

if ! command -v taskset >/dev/null 2>&1; then
    echo "ERROR: taskset is required for bounded Cargo execution" >&2
    exit 1
fi

# crate|mode|advisory|explanation — extend when another lockfile-pinned crate
# with a live advisory shows up. Keep entries alphabetized by crate name so
# all advisories for one crate stay adjacent.
guarded=(
    # atty 0.2.14 sits in ic/Cargo.lock via the vendored libsignal-protocol
    # dev-dep env_logger 0.6.2. It IS in the workspace dev graph (CI builds
    # it for libsignal-protocol's tests); the contract is that it never
    # enters a shipped binary.
    "atty|no-dev|RUSTSEC-2021-0145|vendored libsignal-protocol dev-dep env_logger 0.6.2 pulls atty (Windows-only unaligned read, no patched release); it must stay out of every non-dev build graph"
    "pyo3|absent|RUSTSEC-2026-0176|jiter's optional python feature would compile an OOB-read-affected pyo3 (<0.29.0)"
    "pyo3|absent|RUSTSEC-2026-0177|jiter's optional python feature would compile a pyo3 whose PyCFunction::new_closure lacks a Sync bound (<0.29.0)"
)

# Every workspace root whose Cargo.lock is committed to this repo and whose
# build graph this guard watches.
lock_roots=(
    "$repo_root/ic"
    "$repo_root/ic/bridges/xmpp-bridge"
)

# `cargo tree -i <crate>` exits 0 even when the crate is absent from the
# graph (it prints "warning: nothing to print"), and a crate the resolver
# never matched at all errors with "did not match any packages". Both mean
# "not compiled", so stdout must be inspected rather than the exit status.
absent_markers="nothing to print|did not match any packages"

failed=0
prev_crate=""
pinned_roots=()

for entry in "${guarded[@]}"; do
    IFS='|' read -r crate mode advisory explanation <<<"$entry"

    # Lockfile-presence assertion, once per crate (entries are alphabetized,
    # so all advisories for a crate are adjacent): the guard may only accept
    # an absent graph while the pin it watches still exists.
    if [[ "$crate" != "$prev_crate" ]]; then
        pinned_roots=()
        for root in "${lock_roots[@]}"; do
            if grep -q "^name = \"$crate\"$" "$root/Cargo.lock"; then
                pinned_roots+=("$root")
            fi
        done
        if ((${#pinned_roots[@]} == 0)); then
            echo "ERROR: guard entry for '$crate' is stale: not pinned in any guarded lockfile." >&2
            echo "       Remove the entry or fix the crate name — a vacuous guard is a silent gap." >&2
            failed=1
        fi
        prev_crate="$crate"
    fi
    ((${#pinned_roots[@]} > 0)) || continue

    case "$mode" in
        absent) edges=() ;;
        no-dev) edges=(-e no-dev) ;;
        *)
            echo "ERROR: unknown guard mode '$mode' for $crate" >&2
            failed=1
            continue
            ;;
    esac

    for root in "${pinned_roots[@]}"; do
        out=$(taskset -c 0-11 cargo tree --workspace --all-features \
            ${edges[@]+"${edges[@]}"} -i "$crate" --target all \
            --manifest-path "$root/Cargo.toml" 2>&1 || true)
        if ! grep -qE "$absent_markers" <<<"$out"; then
            echo "ERROR: $crate is now reachable in $root's '$mode' build graph — $advisory applies ($explanation)." >&2
            echo "       Resolve by upgrading past the advisory's patched range before merging." >&2
            failed=1
        fi
    done
done

if ((failed)); then
    exit 1
fi

echo "lockfile-only advisory guard: ok"
