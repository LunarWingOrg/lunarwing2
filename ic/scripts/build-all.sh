#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

echo "Building LunarWing..."
# Clean vendored crates patched in place since the last build (cargo never
# re-checks them); see scripts/vendor-artifact-guard.sh.
bash scripts/vendor-artifact-guard.sh --profile release
cargo build --release
bash scripts/vendor-artifact-guard.sh --profile release --record || true

echo ""
echo "Done. Binary: target/release/lunarwing"
