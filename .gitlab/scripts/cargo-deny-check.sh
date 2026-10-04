#!/usr/bin/env bash
# cargo-deny wrapper for CI and local gate parity.
#
# Running `cargo deny` from inside ic/ silently disables the yanked-crate
# check: ic/.cargo/config.toml source-replaces crates.io with ic/vendor/, so
# cargo-deny cannot query the registry index and downgrades every crate to
# `warning[index-failure]` while still exiting 0. That also floods the job
# trace past GitLab's 4 MB log cap, hiding the check verdict entirely
# (pipeline #4811 cargo-deny trace was truncated mid-diagnostic).
#
# This wrapper instead:
#   1. Runs from the repo root with an explicit --manifest-path, so the
#      vendored source replacement never applies and the crates.io index
#      (allowed by the runner egress contract in .gitlab/README.md) is
#      actually consulted for yanked versions.
#   2. Hides inclusion graphs to keep diagnostics compact; the crates.io
#      index lookup no longer needs them to identify a locked crate.
#   3. Fails the job if any index-failure diagnostic still appears, so the
#      yanked gate can never be silently skipped again.
#   4. Checks the graph CI actually compiles: --workspace --all-features
#      (rust-check-all-features / rust-clippy-all). A root-package default-
#      feature query audits ~889 crates while CI builds ~900 — the extras
#      include openssl/openssl-sys, rental, json5, and the
#      lunarwing_health_metrics workspace member (MR !1371 review A4).
#   5. Prints the advisory-db commit that judged the tree so the CI trace
#      records exactly which RustSec database revision produced the verdict.
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)

if ! command -v taskset >/dev/null 2>&1; then
    echo "ERROR: taskset is required for bounded Cargo execution" >&2
    exit 1
fi

deny_bin="cargo-deny"
if [[ -x "$repo_root/.ci-bin/cargo-deny" ]]; then
    deny_bin="$repo_root/.ci-bin/cargo-deny"
fi

manifest="$repo_root/ic/Cargo.toml"
config_file="$repo_root/ic/deny.toml"

log=$(mktemp)
trap 'rm -f "$log"' EXIT

# Run from the repo root: ic/.cargo/config.toml only applies at or below ic/,
# so this keeps the real registry index in play for the yanked check.
status=0
(cd "$repo_root" && taskset -c 0-11 "$deny_bin" deny \
    --manifest-path "$manifest" \
    --config "$config_file" \
    --workspace --all-features \
    check --hide-inclusion-graph 2>&1) | tee "$log" || status=1

# Record which RustSec advisory-db revision produced the verdict above.
db_dir=$(find "${CARGO_HOME:-$HOME/.cargo}"/advisory-dbs -maxdepth 1 -mindepth 1 \
    -type d -name 'advisory-db-*' 2>/dev/null | head -n 1 || true)
if [[ -n "$db_dir" ]] && command -v git >/dev/null 2>&1; then
    echo "advisory-db commit: $(git -C "$db_dir" rev-parse HEAD) ($(git -C "$db_dir" log -1 --format=%cs))"
fi

# index-failure means cargo-deny could not query a crate's registry index —
# the yanked = "deny" policy was not verifiably enforced for that crate.
if grep -q "index-failure" "$log"; then
    echo "ERROR: cargo-deny reported index-failure diagnostics: the yanked-crate check was skipped for at least one crate. Do not run cargo-deny from inside ic/ (vendored source replacement hides the registry index)." >&2
    exit 1
fi

exit "$status"
