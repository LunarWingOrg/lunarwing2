#!/usr/bin/env bash
# Advisory and bans sweep over EVERY manifest root with a committed Cargo.lock.
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
# Scope includes advisories and bans (WI#418 IS-1): the ID-keyed ignores
# require the same version/parent restrictions in every manifest root.
# Licenses/sources policy stays with the ic workspace gate. The manifest-less
# pebble4lunarwing/upstream lock is skipped here; its reviewed override delta
# is gated by
# pebble4lunarwing/scripts/verify_upstream_lock.py. cargo-deny cannot audit a
# lock without a manifest, so the override's own advisory scan is a tracked
# follow-up (WI#626), not part of this sweep.
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

audit_bin="cargo-audit"
if [[ -x "$repo_root/.ci-bin/cargo-audit" ]]; then
    audit_bin="$repo_root/.ci-bin/cargo-audit"
fi

config_file="$repo_root/ic/deny.toml"
override_lock="pebble4lunarwing/upstream/Cargo.lock"

checked=0
skipped=0
failed=0
while IFS= read -r lock; do
    root=$(dirname "$lock")
    if [[ ! -f "$repo_root/$root/Cargo.toml" ]]; then
        if [[ "$lock" == "$override_lock" ]]; then
            echo "SKIP (manifest-less cargo-deny; cargo-audit below): $lock"
        else
            echo "SKIP (manifest-less): $lock"
        fi
        skipped=$((skipped + 1))
        continue
    fi
    out=$(cd "$repo_root" && taskset -c 0-11 "$deny_bin" deny \
        --manifest-path "$repo_root/$root/Cargo.toml" \
        --config "$config_file" \
        --workspace --all-features \
        check advisories bans --hide-inclusion-graph 2>&1) || {
        echo "FAIL $root"
        # WI#610: name each failing advisory up front. cargo-deny puts its
        # error block above the advisory-not-detected warnings for ignore
        # entries that do not apply to this root, so the old tail -n 20 hid
        # the advisory ID and the crate. Print the whole output as well.
        grep -E '^error(\[|: )' <<<"$out" | sed 's/^/  /' || true
        printf '%s\n' "$out"
        failed=1
        continue
    }
    echo "OK   $root"
    checked=$((checked + 1))
done < <(git -C "$repo_root" ls-files '**/Cargo.lock' | grep -v '^ic/vendor/' | sort)

db_dir=$(find "${CARGO_HOME:-$HOME/.cargo}"/advisory-dbs -maxdepth 1 -mindepth 1 \
    -type d -name 'advisory-db-*' 2>/dev/null | head -n 1 || true)
if [[ -z "$db_dir" ]]; then
    echo "ERROR: cargo-deny left no advisory database for the Pebble override audit" >&2
    exit 1
fi
echo "advisory-db commit: $(git -C "$db_dir" rev-parse HEAD) ($(git -C "$db_dir" log -1 --format=%cs))"

if [[ ! -f "$repo_root/$override_lock" ]]; then
    echo "ERROR: missing Pebble override lock: $override_lock" >&2
    exit 1
fi
if ! command -v "$audit_bin" >/dev/null 2>&1; then
    echo "ERROR: cargo-audit is required for the Pebble override advisory audit" >&2
    exit 1
fi

# cargo-deny cannot inspect a lockfile without a manifest. Reuse its exact
# advisory checkout and forbid another fetch; unsound and yanked findings keep
# the same deny policy as ic/deny.toml, while vulnerabilities deny by default.
audit_status=0
audit_out=$(cd "$repo_root" && taskset -c 0-11 "$audit_bin" audit \
    --file "$repo_root/$override_lock" \
    --db "$db_dir" \
    --no-fetch --stale \
    -D unsound -D yanked \
    --color never 2>&1) || audit_status=$?
if ((audit_status != 0)); then
    echo "FAIL $override_lock (cargo-audit)"
    grep -E 'RUSTSEC-|error(\[|:)|vulnerability|unsound|yanked' <<<"$audit_out" | sed 's/^/  /' || true
    printf '%s\n' "$audit_out"
    failed=1
else
    if [[ -n "$audit_out" ]]; then
        printf '%s\n' "$audit_out"
    fi
    echo "OK   $override_lock (cargo-audit)"
fi

echo "satellite advisory sweep: $checked manifest roots ok, $skipped manifest-less cargo-deny skips; Pebble override cargo-audit status above"
if ((failed)); then
    exit 1
fi
