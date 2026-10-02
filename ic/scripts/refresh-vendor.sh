#!/usr/bin/env bash
# Regenerate the committed crates.io vendor dir (ic/vendor/), assert the
# result still matches the committed lockfiles set-wise, and re-verify the
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
#   - vendor/rig-core carries a LOCAL PATCH (WI#292 finish_reason
#     pass-through, see vendor/rig-core/LUNARWING-PATCH.md). cargo vendor
#     regenerates it from crates.io, which would silently drop the patch; the
#     post-vendor patch check below fails loudly if that happens.
#   - The post-vendor set check compares vendor/ against the committed
#     Cargo.lock files (the approved pins). It must only report drift, never
#     re-resolve versions: any stray means the lockfiles drifted, so fix the
#     locks first and re-run.
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
  tools-src/cavepony/Cargo.toml
  tools-src/gitlab/Cargo.toml
  tools-src/gotify/Cargo.toml
  tools-src/llm-context/Cargo.toml
  tools-src/multica-bridge/Cargo.toml
  tools-src/ssh/Cargo.toml
  tools-src/vision-analyze/Cargo.toml
  tools-src/voice-transcribe/Cargo.toml
  tools-src/web-search/Cargo.toml
  unix-socket-client/Cargo.toml
)

sync_args=()
lockfiles=(Cargo.lock)
for manifest in "${SYNC_MANIFESTS[@]}"; do
  lock="${manifest%/Cargo.toml}/Cargo.lock"
  if [[ ! -f "$lock" ]]; then
    echo "refresh-vendor: skipping $manifest (no Cargo.lock present)" >&2
    continue
  fi
  sync_args+=(--sync "$manifest")
  lockfiles+=("$lock")
done

echo "refresh-vendor: cargo vendor --locked --no-delete $VENDOR_DIR ${sync_args[*]} $*" >&2
cargo vendor --locked --no-delete "$VENDOR_DIR" "${sync_args[@]}" "$@"

# WI#292 tripwire: vendor/rig-core carries a local patch (finish_reason
# pass-through) that cargo vendor silently overwrites with pristine crates.io
# sources. Fail loudly instead of losing the patch in a "refresh".
if ! grep -q "LUNARWING_WI292_FINISH_REASON_PATCH" \
  "$VENDOR_DIR/rig-core/src/providers/internal/openai_chat_completions_compatible.rs" 2>/dev/null; then
  echo "refresh-vendor: ERROR rig-core local patch (WI#292 finish_reason pass-through) is gone after the vendor run." >&2
  echo "  Re-apply it (see vendor/rig-core/LUNARWING-PATCH.md and git history), refresh .cargo-checksum.json, then re-run." >&2
  exit 1
fi

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

# Set check against the committed lockfiles (the approved pins): every
# vendored crate dir must come from one of the synced Cargo.lock files. A
# stray means a lockfile drifted between vendor runs — fix the lock instead
# of letting cargo re-resolve. Duplicate version dirs (e.g. hashbrown x5) are
# expected: each synced lockfile may pin a different version of a shared
# crate, and the vendor set is their union.
python3 - "$VENDOR_DIR" "${lockfiles[@]}" <<'PYEOF'
import os, sys, tomllib

vendor_dir = sys.argv[1]
lockfiles = sys.argv[2:]

# Every non-workspace package in a lockfile carries a `source` (registry+ or
# git+); cargo vendor vendored both families into the vendor dir. Workspace-
# local packages have no source and are never vendored.
lock_names = set()
for lock in lockfiles:
    with open(lock, "rb") as fh:
        for pkg in tomllib.load(fh).get("package", []):
            if pkg.get("source"):
                lock_names.add(pkg["name"])


def base_candidates(d):
    # cargo vendor uses bare crate names for the newest pinned version and
    # name-<version> dirs for collided versions of the same crate. Reverse it
    # by cutting at any dash that is followed by a version digit — this also
    # handles real names ending in digits (md-5-0.10.6 -> md-5).
    yield d
    for i, ch in enumerate(d):
        if ch == "-" and i + 1 < len(d) and d[i + 1].isdigit():
            yield d[:i]


strays, joints = [], {}
for d in sorted(os.listdir(vendor_dir)):
    if not os.path.isdir(os.path.join(vendor_dir, d)):
        continue
    if d in ("libsignal-protocol", "libsignal-protocol-sys"):
        continue  # pre-vendored path deps, not from crates.io
    base = next((c for c in base_candidates(d) if c in lock_names), d)
    joints[base] = joints.get(base, 0) + 1
    if base == d and d not in lock_names:
        strays.append(d)

top = sorted(joints.items(), key=lambda kv: -kv[1])[:5]
print(
    f"refresh-vendor: {sum(joints.values())} crate dirs, {len(joints)} unique names; "
    + ", ".join(f"{n}x{c}" for n, c in top)
)
if strays:
    print(
        "refresh-vendor: ERROR vendored dirs outside every synced Cargo.lock "
        + f"({len(strays)}): {strays[:10]} — lockfiles drifted; fix locks, do not re-resolve",
        file=sys.stderr,
    )
    sys.exit(1)
print("refresh-vendor: vendor set matches synced Cargo.lock files")
PYEOF

# The unvendored cargo-deny wrapper (../.gitlab/scripts/cargo-deny-check.sh)
# enforces yanked = "deny" against the real crates.io index; running the check
# from inside ic/ would source-replace the registry away and silently skip it.
echo "refresh-vendor: cargo-deny advisories gate (unvendored context)" >&2
bash ../.gitlab/scripts/cargo-deny-check.sh
echo "refresh-vendor: OK - review the $VENDOR_DIR/ diff, then commit" >&2
