#!/usr/bin/env bash
# Install sccache (pinned, sha256-verified) for the Rust CI jobs.
# Nothing is installed on the runner host — the binary lives in the
# project-local .ci-cache and the job containers download it fresh
# (cache-hit makes this a no-op on repeat runs).
set -euo pipefail

SCCACHE_VERSION="0.17.0"
SCCACHE_SHA256="67c4a96dd237c1f518f6b36083f270f9976d516f1e57fce891755ea782e50006"
SCCACHE_BIN_SHA256="066c5a84c85044c8f48b3ab571ac114293ea717c3d36985db022af8206e21e63"
SCCACHE_URL="https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl.tar.gz"

bin_dir="$CI_PROJECT_DIR/.ci-cache/bin"
sccache_bin="$bin_dir/sccache"

# IU-2: cache contents are untrusted; asking for --version executes them.
# Verify bytes before any execution, even on a cache hit.
if [[ -x "$sccache_bin" ]] && echo "$SCCACHE_BIN_SHA256  $sccache_bin" | sha256sum --check --status; then
    exit 0
fi

# after_script still runs when bootstrap fails. Remove a rejected binary
# before downloading so a network/checksum failure cannot leave it executable.
rm -f -- "$sccache_bin"
mkdir -p "$bin_dir"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

curl -fsSL --retry 3 -o "$tmp/sccache.tar.gz" "$SCCACHE_URL"
echo "$SCCACHE_SHA256  $tmp/sccache.tar.gz" | sha256sum --check --status
tar -xzf "$tmp/sccache.tar.gz" -C "$tmp"
install -m 0755 "$tmp/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl/sccache" "$sccache_bin"
if ! echo "$SCCACHE_BIN_SHA256  $sccache_bin" | sha256sum --check --status; then
    rm -f -- "$sccache_bin"
    exit 1
fi
"$sccache_bin" --version
