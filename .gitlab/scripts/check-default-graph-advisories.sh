#!/usr/bin/env bash
# Guard for the WI#331 mitigation: the libsql 0.6.0 legacy chain
# (rustls 0.22.4, rustls-webpki 0.102.8, h2 0.3.27) must stay OUT of the
# default-feature production graph now that libsql's remote/replication/tls surface lives
# behind the non-default `libsql-remote-replica` cargo feature.
#
# Why a separate guard (MR !1513 review CZ-1): both cargo-deny gates
# (cargo-deny-check.sh, check-satellite-advisories.sh) run
# --workspace --all-features with the same ID-keyed ignores, so a regression
# that pulls the legacy chain back into default builds — restoring libsql's
# "remote" feature — is invisible to the ID-keyed ignores and the deny.toml
# reason strings ("default builds cleared by feature gate") become false.
# WI#418 bans now restrict other versions/parents, while this guard checks
# the accepted pins' default-feature reachability and shipped build flags.
#
# Contract per entry:
#   * The vulnerable version pin must still exist in ic/Cargo.lock: the
#     all-features graph legitimately contains it behind the opt-in feature.
#     If the pin ever leaves the lock (the chain is really cleared), the
#     guard fails loudly as stale instead of passing vacuously — the entry
#     and the matching ic/deny.toml ignores must then be removed together.
#   * A successful `cargo tree --locked -e normal --target all`
#     over the DEFAULT-feature graph must not contain the pin at either root:
#     the ic workspace and the xmpp-bridge satellite (the bridge builds
#     lunarwing with default features, so its graph is exactly what ships in
#     tenant bridge binaries).
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)

if ! command -v taskset >/dev/null 2>&1; then
    echo "ERROR: taskset is required for bounded Cargo execution" >&2
    exit 1
fi

# crate|version|advisories|explanation — keep alphabetized by crate name.
guarded=(
    "h2|0.3.27|RUSTSEC-2026-0258|libsql 0.6.0 -> tonic 0.11 -> hyper 0.14 legacy chain; only opt-in libsql-remote-replica builds may compile it"
    "rustls|0.22.4|RUSTSEC-2026-0346/0349|libsql 0.6.0 -> hyper-rustls 0.25 legacy chain; only opt-in libsql-remote-replica builds may compile it"
    "rustls-webpki|0.102.8|RUSTSEC-2026-0049/0098/0099/0104|libsql 0.6.0 -> hyper-rustls 0.25 -> rustls 0.22 legacy chain; only opt-in libsql-remote-replica builds may compile it"
)

lock_roots=(
    "$repo_root/ic"
    "$repo_root/ic/bridges/xmpp-bridge"
)

failed=0

for entry in "${guarded[@]}"; do
    IFS='|' read -r crate version advisories explanation <<<"$entry"
    pin="$crate@$version"

    if ! grep -A1 "^name = \"$crate\"$" "$repo_root/ic/Cargo.lock" | grep -q "^version = \"$version\"$"; then
        echo "ERROR: default-graph guard entry for '$pin' is stale: not pinned in ic/Cargo.lock." >&2
        echo "       If the libsql legacy chain was really cleared, remove this entry and the matching ic/deny.toml ignores together." >&2
        failed=1
    fi
done

for root in "${lock_roots[@]}"; do
    # Cargo discovers .cargo/config.toml from the working directory, not
    # --manifest-path. Enter each root to honor ic/vendor source replacement.
    # Git sources are not replaced; allow fetching pinned revisions on cold CI caches.
    # Inspect a successful forward graph: resolver errors must never count
    # as evidence that a vulnerable package is absent.
    if ! out=$(cd "$root" && taskset -c 0-11 cargo tree --locked \
        -e normal --target all --prefix none --format '{p}' \
        --manifest-path "$root/Cargo.toml" 2>&1); then
        echo "ERROR: failed to inspect $root's DEFAULT-feature production graph." >&2
        printf '%s\n' "$out" >&2
        failed=1
        continue
    fi
    for entry in "${guarded[@]}"; do
        IFS='|' read -r crate version advisories explanation <<<"$entry"
        pin="$crate@$version"
        if awk -v crate="$crate" -v version="v$version" \
            '$1 == crate && $2 == version { found = 1 } END { exit !found }' <<<"$out"; then
            echo "ERROR: $pin is reachable in $root's DEFAULT-feature production graph — $advisories apply ($explanation)." >&2
            echo "       Default builds must not compile the legacy chain; check libsql's enabled features and any new hyper-rustls 0.25 dependents." >&2
            failed=1
        fi
    done
done

# WI#418 condition 1 (IS-2): shipped builds must not enable the opt-in chain.
# This textual tripwire cannot resolve variable indirection; the build paths
# and their effective flags must also be reviewed at every release.
shipped_builds=(
    "ic/Dockerfile"
    "ic/Dockerfile.worker"
    "ic/scripts/mt-admin-lib/05-tenant-build.sh"
    "scripts/build-lunarwing.sh"
)
for build in "${shipped_builds[@]}"; do
    if [[ ! -f "$repo_root/$build" ]]; then
        echo "ERROR: shipped build path $build is missing; update the WI#418 condition-1 list." >&2
        failed=1
    elif grep -n -E 'libsql-remote-replica|--all-features' "$repo_root/$build" >&2; then
        echo "ERROR: $build enables the opt-in libsql-remote-replica chain (WI#418 condition 1)." >&2
        failed=1
    fi
done

if ((failed)); then
    exit 1
fi

echo "default-graph advisory guard: ok"
