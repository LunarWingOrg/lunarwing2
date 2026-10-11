#!/usr/bin/env bash
# Guard for the WI#331 mitigation: the libsql 0.6.0 legacy chain
# (rustls-webpki 0.102.8, h2 0.3.27) must stay OUT of the default-feature
# production graph now that libsql's remote/replication/tls surface lives
# behind the non-default `libsql-remote-replica` cargo feature.
#
# Why a separate guard (MR !1513 review CZ-1): both cargo-deny gates
# (cargo-deny-check.sh, check-satellite-advisories.sh) run
# --workspace --all-features with the same ID-keyed ignores, so a regression
# that pulls the legacy chain back into default builds — restoring libsql's
# "remote" feature, or a new dependency on hyper-rustls 0.25 — stays
# invisible while every gate is green and the deny.toml reason strings
# ("default builds cleared by feature gate") silently become false. Every
# shipped build path uses default features, so this default-graph pin is the
# regression alarm for the whole mitigation.
#
# Contract per entry:
#   * The vulnerable version pin must still exist in ic/Cargo.lock: the
#     all-features graph legitimately contains it behind the opt-in feature.
#     If the pin ever leaves the lock (the chain is really cleared), the
#     guard fails loudly as stale instead of passing vacuously — the entry
#     and the matching ic/deny.toml ignores must then be removed together.
#   * `cargo tree --locked -e normal --target all -i crate@version` over the
#     DEFAULT-feature graph must report the pin absent, at both build roots:
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
    "rustls-webpki|0.102.8|RUSTSEC-2026-0049/0098/0099/0104|libsql 0.6.0 -> hyper-rustls 0.25 -> rustls 0.22 legacy chain; only opt-in libsql-remote-replica builds may compile it"
)

lock_roots=(
    "$repo_root/ic"
    "$repo_root/ic/bridges/xmpp-bridge"
)

# `cargo tree -i crate@version` errors with "did not match any packages"
# when the pin never resolved, and prints "nothing to print" when it matched
# nothing invertible; both mean "not in this graph".
absent_markers="nothing to print|did not match any packages"

failed=0

for entry in "${guarded[@]}"; do
    IFS='|' read -r crate version advisories explanation <<<"$entry"
    pin="$crate@$version"

    if ! grep -A1 "^name = \"$crate\"$" "$repo_root/ic/Cargo.lock" | grep -q "^version = \"$version\"$"; then
        echo "ERROR: default-graph guard entry for '$pin' is stale: not pinned in ic/Cargo.lock." >&2
        echo "       If the libsql legacy chain was really cleared, remove this entry and the matching ic/deny.toml ignores together." >&2
        failed=1
        continue
    fi

    for root in "${lock_roots[@]}"; do
        out=$(taskset -c 0-11 cargo tree --locked -e normal --target all \
            -i "$pin" --manifest-path "$root/Cargo.toml" 2>&1 || true)
        if ! grep -qE "$absent_markers" <<<"$out"; then
            echo "ERROR: $pin is reachable in $root's DEFAULT-feature production graph — $advisories apply ($explanation)." >&2
            echo "       Default builds must not compile the legacy chain; check libsql's enabled features and any new hyper-rustls 0.25 dependents." >&2
            failed=1
        fi
    done
done

if ((failed)); then
    exit 1
fi

echo "default-graph advisory guard: ok"
