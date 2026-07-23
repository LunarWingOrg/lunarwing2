#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

mapfile -d '' candidates < <(git ls-files -z --cached --others --exclude-standard -- '*.sh')
scripts=()
for script in "${candidates[@]}"; do
    [[ -f "$script" ]] && scripts+=("$script")
done
hooks=(.githooks/pre-commit .githooks/pre-push .githooks/commit-msg)

for script in "${scripts[@]}"; do
    bash -n "$script"
done
for hook in "${hooks[@]}"; do
    bash -n "$hook"
done

echo "Validated Bash syntax in ${#scripts[@]} scripts and ${#hooks[@]} hooks."
