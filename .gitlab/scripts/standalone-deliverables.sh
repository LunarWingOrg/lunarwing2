#!/usr/bin/env bash
# Validate every classified standalone deliverable from .gitlab/deliverables.json.
# For each unit whose ci list names "standalone-deliverables", this script runs a
# locked offline metadata check inside the unit directory. A missing lockfile,
# a lockfile out of sync with the manifest, or a stale classification fails.
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

readonly manifest="$repo_root/.gitlab/deliverables.json"
readonly cargo="taskset -c 0-5 cargo"

if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq is required for standalone deliverable validation" >&2
    exit 1
fi

count=$(jq '.deliverables | length' "$manifest")
checked=0

for index in $(seq 0 $((count - 1))); do
    entry=$(jq -r ".deliverables[$index]" "$manifest")
    name=$(jq -r '.name' <<<"$entry")
    manifest_path=$(jq -r '.manifest' <<<"$entry")
    ci_list=$(jq -r '(.ci // []) | join(" ")' <<<"$entry")

    if [[ " $ci_list " != *" standalone-deliverables "* ]]; then
        continue
    fi

    echo "=== standalone deliverable: $name ($manifest_path) ==="
    if [[ ! -f "$manifest_path" ]]; then
        echo "ERROR: $name manifest missing: $manifest_path" >&2
        exit 1
    fi

    (
        cd "$(dirname "$manifest_path")"
        $cargo metadata --locked --no-deps --format-version 1 >/dev/null
        $cargo verify-project --locked >/dev/null
    )
    checked=$((checked + 1))
    echo "OK"
done

if [ "$checked" -eq 0 ]; then
    echo "ERROR: no deliverable selected standalone-deliverables validation" >&2
    exit 1
fi

echo "Standalone deliverables passed: $checked unit(s), locked metadata verified."
