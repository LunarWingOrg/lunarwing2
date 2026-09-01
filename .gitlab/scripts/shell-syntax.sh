#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

# ic/vendor/ is pinned third-party crates.io source (ic/.cargo/config.toml) —
# its scripts are not our lint surface, so exclude it from the scan.
mapfile -d '' candidates < <(git ls-files -z --cached --others --exclude-standard -- '*.sh' | grep -zv '^ic/vendor/')
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
