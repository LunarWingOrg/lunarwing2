#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
zero_sha="0000000000000000000000000000000000000000"
base_sha="${CI_MERGE_REQUEST_DIFF_BASE_SHA:-${CI_COMMIT_BEFORE_SHA:-}}"

python3 "$repo_root/ic/scripts/check_no_panics.py" --self-test

if [[ -z "$base_sha" || "$base_sha" == "$zero_sha" ]]; then
    default_ref="origin/${CI_DEFAULT_BRANCH:?CI_DEFAULT_BRANCH is required}"
    if git rev-parse --verify "$default_ref^{commit}" >/dev/null 2>&1; then
        base_sha=$(git merge-base HEAD "$default_ref")
    elif git rev-parse --verify HEAD^ >/dev/null 2>&1; then
        base_sha=$(git rev-parse HEAD^)
    else
        echo "ERROR: cannot determine panic-policy base commit" >&2
        exit 1
    fi
fi

python3 "$repo_root/ic/scripts/check_no_panics.py" --base "$base_sha" --head "${CI_COMMIT_SHA:-HEAD}"
