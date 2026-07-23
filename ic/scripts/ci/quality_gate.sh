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

echo "==> clippy (correctness)"
run_cargo clippy -j6 --locked --all-targets -- -D clippy::correctness

# LUNARWING_PREPUSH_TEST is primary; IRONCLAW_PREPUSH_TEST is a legacy alias.
if [ "${LUNARWING_PREPUSH_TEST:-${IRONCLAW_PREPUSH_TEST:-1}}" = "1" ]; then
    echo "==> tests (skip with LUNARWING_PREPUSH_TEST=0)"
    run_cargo test -j6 --locked --lib -- --test-threads=1
fi
