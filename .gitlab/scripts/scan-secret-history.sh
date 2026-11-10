#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
ignore_path=${1:-"$repo_root/.gitlab/gitleaks-history-ignore"}
pending_path=${2:-"$repo_root/.gitlab/gitleaks-history-pending"}

if [[ $(git -C "$repo_root" rev-parse --is-shallow-repository) == "true" ]]; then
    printf 'secret-history: refusing to scan a shallow repository\n' >&2
    exit 2
fi
if [[ ! -f $ignore_path ]]; then
    printf 'secret-history: commit-bound ignore file is missing\n' >&2
    exit 2
fi
if [[ ! -f $pending_path ]]; then
    printf 'secret-history: pending-findings file is missing\n' >&2
    exit 2
fi
while IFS= read -r entry; do
    [[ -z $entry || $entry == \#* ]] && continue
    if [[ ! $entry =~ ^[0-9a-f]{40}:.+:[^:]+:[0-9]+$ ]]; then
        printf 'secret-history: ignore entry is not commit-bound\n' >&2
        exit 2
    fi
done <"$ignore_path"
# A finding that still awaits its disposition must stay visible. The refresh
# helper refuses to pin such a row; a hand edit of the baseline must not be
# able to turn this job green either.
while IFS= read -r entry; do
    [[ -z $entry || $entry == \#* ]] && continue
    if [[ ! $entry =~ ^[0-9a-f]{40}:.+:[^:]+:[0-9]+$ ]]; then
        printf 'secret-history: pending entry is not commit-bound\n' >&2
        exit 2
    fi
    if grep -Fxq -- "$entry" "$ignore_path"; then
        printf 'secret-history: a pending finding is pinned in the baseline; remove the pin or record its disposition first\n' >&2
        exit 2
    fi
done <"$pending_path"

scan_root=$(mktemp -d)
trap 'rm -rf "$scan_root"' EXIT
git_dir=$(git -C "$repo_root" rev-parse --absolute-git-dir)
printf 'gitdir: %s\n' "$git_dir" >"$scan_root/.git"

# The current-tree .gitleaksignore deliberately uses path:rule:line pins. Keep
# it out of this worktree so only commit-bound history fingerprints can suppress.
cd "$scan_root"
gitleaks git --redact=100 --no-banner --no-color \
    --gitleaks-ignore-path "$ignore_path" \
    --log-opts="--all" .
