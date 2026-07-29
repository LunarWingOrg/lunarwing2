#!/usr/bin/env bash
# Install sccache (pinned, sha256-verified) for the Rust CI jobs.
# Nothing is installed on the runner host — the binary lives in the
# project-local .ci-cache and the job containers download it fresh
# (cache-hit makes this a no-op on repeat runs).
set -euo pipefail

SCCACHE_VERSION="0.17.0"
SCCACHE_SHA256="67c4a96dd237c1f518f6b36083f270f9976d516f1e57fce891755ea782e50006"
SCCACHE_URL="https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl.tar.gz"

bin_dir="$CI_PROJECT_DIR/.ci-cache/bin"
sccache_bin="$bin_dir/sccache"

if [[ -x "$sccache_bin" ]] && [[ "$("$sccache_bin" --version)" == "sccache $SCCACHE_VERSION" ]]; then
    exit 0
fi

mkdir -p "$bin_dir"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

curl -fsSL --retry 3 -o "$tmp/sccache.tar.gz" "$SCCACHE_URL"
echo "$SCCACHE_SHA256  $tmp/sccache.tar.gz" | sha256sum --check --status
tar -xzf "$tmp/sccache.tar.gz" -C "$tmp"
install -m 0755 "$tmp/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl/sccache" "$sccache_bin"
"$sccache_bin" --version
