#!/usr/bin/env bash
# Advisory sweep over EVERY manifest root with a committed Cargo.lock.
#
# The main cargo-deny job (.gitlab/scripts/cargo-deny-check.sh) gates the ic
# workspace. This sweep makes the release-audit claim "every committed
# lockfile is clean" reproducible from the tree (MR !1371 review A5): it
# discovers each tracked Cargo.lock via git, and runs the same pinned
# cargo-deny binary with the shared ic/deny.toml policy over every root —
# ic satellites (bridges, channels-src, tools-src, openclaw-ports, fixtures,
# unix-socket-client) and the root-level workers/sidecars alike. A new
# satellite root is picked up automatically; removing one shrinks the sweep.
#
# Scope is `check advisories` only: bans/licenses/sources policy is owned by
# the ic workspace gate. The manifest-less pebble4lunarwing/upstream lock is
# skipped here; its reviewed override delta is gated by
# pebble4lunarwing/scripts/verify_upstream_lock.py.
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

config_file="$repo_root/ic/deny.toml"

checked=0
skipped=0
failed=0
while IFS= read -r lock; do
    root=$(dirname "$lock")
    if [[ ! -f "$repo_root/$root/Cargo.toml" ]]; then
        echo "SKIP (manifest-less, gated by verify_upstream_lock.py): $lock"
        skipped=$((skipped + 1))
        continue
    fi
    out=$(cd "$repo_root" && taskset -c 0-11 "$deny_bin" deny \
        --manifest-path "$repo_root/$root/Cargo.toml" \
        --config "$config_file" \
        --workspace --all-features \
        check advisories --hide-inclusion-graph 2>&1) || {
        echo "FAIL $root"
        echo "$out" | tail -n 20
        failed=1
        continue
    }
    echo "OK   $root"
    checked=$((checked + 1))
done < <(git -C "$repo_root" ls-files '**/Cargo.lock' | grep -v '^ic/vendor/' | sort)

db_dir=$(find "${CARGO_HOME:-$HOME/.cargo}"/advisory-dbs -maxdepth 1 -mindepth 1 \
    -type d -name 'advisory-db-*' 2>/dev/null | head -n 1 || true)
if [[ -n "$db_dir" ]] && command -v git >/dev/null 2>&1; then
    echo "advisory-db commit: $(git -C "$db_dir" rev-parse HEAD) ($(git -C "$db_dir" log -1 --format=%cs))"
fi

echo "satellite advisory sweep: $checked roots ok, $skipped manifest-less skipped"
if ((failed)); then
    exit 1
fi
