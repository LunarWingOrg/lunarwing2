#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
scan_root=$(mktemp -d)
trap 'rm -rf "$scan_root"' EXIT

cd "$repo_root"
git archive --format=tar "${CI_COMMIT_SHA:-HEAD}" | tar -xf - -C "$scan_root"
# ic/vendor/ is pinned third-party crates.io source (ic/.cargo/config.toml,
# managed by ic/scripts/refresh-vendor.sh). Its test fixtures legitimately
# contain key/cert-shaped material, so it is excluded from secret scanning;
# its contents are checksummed by cargo and never carry repo secrets.
rm -rf "$scan_root/ic/vendor"
cd "$scan_root"
gitleaks dir --redact=100 --no-banner --no-color .
