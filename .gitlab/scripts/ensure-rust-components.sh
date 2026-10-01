#!/usr/bin/env bash
set -euo pipefail

toolchain=${1:?usage: ensure-rust-components.sh <pinned-toolchain>}

# Rustup installs proxy binaries even with --profile minimal. Probe the actual
# pinned toolchain, not PATH, before entering ic/ (whose toolchain file would
# otherwise trigger an implicit, un-retried download during the Cargo command).
for component in rustfmt clippy; do
    binary=$component
    [[ "$component" != clippy ]] || binary=cargo-clippy
    if rustup run "$toolchain" "$binary" --version >/dev/null 2>&1; then
        continue
    fi

    for attempt in 1 2 3; do
        if rustup component add --toolchain "$toolchain" "$component" \
            && rustup run "$toolchain" "$binary" --version; then
            break
        fi
        if (( attempt == 3 )); then
            echo "ERROR: $component unavailable for Rust $toolchain after 3 attempts; check runner DNS/HTTPS access to static.rust-lang.org" >&2
            exit 1
        fi
        echo "Retrying Rust $toolchain $component installation ($attempt/3 failed)" >&2
        sleep "$((attempt * 5))"
    done
done
