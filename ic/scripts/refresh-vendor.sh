#!/usr/bin/env bash
# Regenerate the committed crates.io vendor dir (ic/vendor/) and re-verify the
# vendored set against the RustSec advisory database before merge.
#
# Usage: scripts/refresh-vendor.sh [extra cargo-vendor args...]
#
# Notes:
#   - Vendors the ic workspace plus every tracked satellite lockfile root
#     under ic/ (bridges, channels-src, tools, test fixtures). Root-level
#     satellites outside ic/ (pebble, replv2git, sidecars, ...) are NOT
#     source-replaced and are intentionally out of scope.
#   - --no-delete protects the pre-vendored path dependencies
#     (vendor/libsignal-protocol, vendor/libsignal-protocol-sys) which the ic
#     manifest path-depends on (ic/Cargo.toml); cargo vendor never rewrites
#     path dependencies, and without --no-delete it would delete them.
#   - tools-src/gitlab/Cargo.toml is deliberately not synced: its lockfile is
#     untracked and stale, so it would fail --locked.
set -euo pipefail

cd "$(dirname "$0")/.."

VENDOR_DIR=vendor

SYNC_MANIFESTS=(
  bridges/xmpp-bridge/Cargo.toml
  channels-src/darkirc/Cargo.toml
  channels-src/multica/Cargo.toml
  channels-src/weechat/Cargo.toml
  channels-src/xmpp/Cargo.toml
  lunarwing-gotify-tool/Cargo.toml
  openclaw-ports/xmpp/bridge/Cargo.toml
  tests/fixtures/test-echo-tool/Cargo.toml
  tools-src/gotify/Cargo.toml
  unix-socket-client/Cargo.toml
)

sync_args=()
for manifest in "${SYNC_MANIFESTS[@]}"; do
  if [[ ! -f "${manifest%/Cargo.toml}/Cargo.lock" ]]; then
    echo "refresh-vendor: skipping $manifest (no Cargo.lock present)" >&2
    continue
  fi
  sync_args+=(--sync "$manifest")
done

echo "refresh-vendor: cargo vendor --locked --no-delete $VENDOR_DIR ${sync_args[*]} $*" >&2
cargo vendor --locked --no-delete "$VENDOR_DIR" "${sync_args[@]}" "$@"

# The directory source requires `.cargo-checksum.json` in every vendor/*
# subdir, including the hand-vendored libsignal path deps that cargo vendor
# never rewrites. Regenerate them if missing (checksums for all files).
for path_dep in "$VENDOR_DIR/libsignal-protocol" "$VENDOR_DIR/libsignal-protocol-sys"; do
  if [[ -d "$path_dep" && ! -f "$path_dep/.cargo-checksum.json" ]]; then
    echo "refresh-vendor: generating $path_dep/.cargo-checksum.json" >&2
    python3 - "$path_dep" <<'PYEOF'
import hashlib, json, os, sys
crate = sys.argv[1]
files = {}
for root, dirs, names in os.walk(crate):
    dirs.sort()
    for n in sorted(names):
        p = os.path.join(root, n)
        rel = os.path.relpath(p, crate).replace(os.sep, "/")
        if rel == ".cargo-checksum.json":
            continue
        with open(p, "rb") as fh:
            files[rel] = hashlib.sha256(fh.read()).hexdigest()
with open(os.path.join(crate, ".cargo-checksum.json"), "w") as fh:
    json.dump({"files": files}, fh, indent=1, sort_keys=True)
    fh.write("\n")
PYEOF
  fi
done

# Prefer the repo-pinned cargo-deny binary; fall back to the cargo subcommand.
DENY=(cargo deny)
if [[ -x ../.ci-bin/cargo-deny ]]; then
  DENY=(../.ci-bin/cargo-deny)
fi

echo "refresh-vendor: ${DENY[*]} check advisories" >&2
"${DENY[@]}" check advisories
echo "refresh-vendor: OK - review the $VENDOR_DIR/ diff, then commit" >&2
