#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

# ic/vendor/ is pinned third-party crates.io source (ic/.cargo/config.toml) —
# its scripts are not our lint surface, so exclude it from the scan.
mapfile -d '' tracked < <(git ls-files -z --cached --others --exclude-standard -- '*.sh' | grep -zv '^ic/vendor/')
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
