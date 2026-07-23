#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

mapfile -d '' tracked < <(git ls-files -z --cached --others --exclude-standard -- '*.sh')
scripts=()
for script in "${tracked[@]}"; do
    [[ -f "$script" ]] || continue
    case "$script" in
        gotify-wasm/reference/gotify.sh)
            continue
            ;;
    esac
    scripts+=("$script")
done

shellcheck --severity=warning \
    "${scripts[@]}" \
    .githooks/pre-commit \
    .githooks/pre-push \
    .githooks/commit-msg
