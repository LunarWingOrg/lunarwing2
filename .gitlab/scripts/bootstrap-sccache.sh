#!/usr/bin/env bash
# Install sccache (pinned, sha256-verified) for the Rust CI jobs.
# Nothing is installed on the runner host — the binary lives in the
# project-local .ci-cache and the job containers download it fresh
# (cache-hit makes this a no-op on repeat runs).
# --show-stats verifies the cached binary and reports telemetry without downloading.
set -euo pipefail

SCCACHE_VERSION="0.17.0"
SCCACHE_SHA256="67c4a96dd237c1f518f6b36083f270f9976d516f1e57fce891755ea782e50006"
SCCACHE_BIN_SHA256="066c5a84c85044c8f48b3ab571ac114293ea717c3d36985db022af8206e21e63"
SCCACHE_ARCHIVE="sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl.tar.gz"
SCCACHE_URL="${SCCACHE_URL:-https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/${SCCACHE_ARCHIVE}}"
if [[ -n "${SCCACHE_REGISTRY_URL:-}" ]]; then
    registry_url="$SCCACHE_REGISTRY_URL"
    registry_auth=()
else
    registry_url="${CI_API_V4_URL:-https://git.lunarwing.org/api/v4}/projects/${CI_PROJECT_ID:-35}/packages/generic/lunarwing-ci-tools/${SCCACHE_VERSION}/${SCCACHE_ARCHIVE}"
    registry_auth=()
    if [[ -n "${CI_JOB_TOKEN:-}" ]]; then
        registry_auth=(--header "JOB-TOKEN: $CI_JOB_TOKEN")
    fi
fi

bin_dir="$CI_PROJECT_DIR/.ci-cache/bin"
sccache_bin="$bin_dir/sccache"

# IU-2: cache contents are untrusted; asking for --version executes them.
# Verify bytes before any execution, even on a cache hit.
if [[ -x "$sccache_bin" ]] && echo "$SCCACHE_BIN_SHA256  $sccache_bin" | sha256sum --check --status; then
    if [[ "${1:-}" == "--show-stats" ]]; then
        exec "$sccache_bin" --show-stats
    fi
    exit 0
fi

# after_script still runs when bootstrap fails. Remove a rejected binary
# before downloading so a network/checksum failure cannot leave it executable.
rm -f -- "$sccache_bin"
# IZ-1: after_script can run before bootstrap ever reaches this helper.
# Verify independently, and skip telemetry for missing/rejected binaries.
if [[ "${1:-}" == "--show-stats" ]]; then
    exit 0
fi
mkdir -p "$bin_dir"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

download() {
    local source_url="$1"
    shift
    rm -f -- "$tmp/sccache.tar.gz"
    curl -fsSL --retry 5 --retry-delay 5 --retry-all-errors "$@" -o "$tmp/sccache.tar.gz" "$source_url"
}

download_source="$registry_url"
if ! download "$registry_url" "${registry_auth[@]}"; then
    echo "WARNING: sccache project registry download failed; trying GitHub fallback" >&2
    download_source="$SCCACHE_URL"
    if ! download "$SCCACHE_URL"; then
        echo "ERROR: sccache downloads failed from project registry ($registry_url) and GitHub ($SCCACHE_URL)" >&2
        exit 1
    fi
fi
if ! echo "$SCCACHE_SHA256  $tmp/sccache.tar.gz" | sha256sum --check --status; then
    echo "ERROR: sccache archive checksum mismatch from $download_source" >&2
    exit 1
fi
tar -xzf "$tmp/sccache.tar.gz" -C "$tmp"
install -m 0755 "$tmp/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl/sccache" "$sccache_bin"
if ! echo "$SCCACHE_BIN_SHA256  $sccache_bin" | sha256sum --check --status; then
    rm -f -- "$sccache_bin"
    exit 1
fi
"$sccache_bin" --version
