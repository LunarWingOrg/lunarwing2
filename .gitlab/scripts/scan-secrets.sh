#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
scan_root=$(mktemp -d)
trap 'rm -rf "$scan_root"' EXIT

cd "$repo_root"
git archive --format=tar "${CI_COMMIT_SHA:-HEAD}" | tar -xf - -C "$scan_root"
cd "$scan_root"
gitleaks dir --redact=100 --no-banner --no-color .
