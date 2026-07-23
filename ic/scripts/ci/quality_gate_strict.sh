#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
PROJECT_ROOT=$(cd "$SCRIPT_DIR/../.." && pwd)
cd "$PROJECT_ROOT"

run_cargo() {
    if command -v taskset >/dev/null 2>&1; then
        taskset -c 0-5 cargo "$@"
    else
        cargo "$@"
    fi
}

echo "==> fmt check"
run_cargo fmt --all -- --check

echo "==> clippy (all warnings)"
run_cargo clippy -j6 --locked --all --benches --tests --examples --all-features -- -D warnings

echo "==> cargo deny"
if ! command -v cargo-deny &>/dev/null; then
    echo "ERROR: cargo-deny not installed (install with: cargo install cargo-deny)"
    exit 1
fi
run_cargo deny check

echo "==> tests"
run_cargo test -j6 --locked -- --test-threads=1
