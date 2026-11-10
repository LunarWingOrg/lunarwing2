#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
ignore_path=${1:-"$repo_root/.gitlab/gitleaks-history-ignore"}

if [[ $(git -C "$repo_root" rev-parse --is-shallow-repository) == "true" ]]; then
    printf 'secret-history: refusing to scan a shallow repository\n' >&2
    exit 2
fi
if [[ ! -f $ignore_path ]]; then
    printf 'secret-history: commit-bound ignore file is missing\n' >&2
    exit 2
fi

cd "$repo_root"
gitleaks git --redact=100 --no-banner --no-color \
    --gitleaks-ignore-path "$ignore_path" \
    --log-opts="--all" .
