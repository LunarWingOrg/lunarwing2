#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
zero_sha="0000000000000000000000000000000000000000"
base_sha="${CI_MERGE_REQUEST_DIFF_BASE_SHA:-${CI_COMMIT_BEFORE_SHA:-}}"

python3 "$repo_root/ic/scripts/check_no_panics.py" --self-test

if [[ -z "$base_sha" || "$base_sha" == "$zero_sha" ]]; then
    # Local runs lack CI_DEFAULT_BRANCH; derive the default branch the same
    # way ic/scripts/check-version-bumps.sh does (origin/HEAD, then master/main).
    default_branch="${CI_DEFAULT_BRANCH:-}"
    if [[ -z "$default_branch" ]]; then
        remote_head=$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null || true)
        default_branch="${remote_head#origin/}"
    fi
    if [[ -z "$default_branch" ]]; then
        if git rev-parse --verify origin/master >/dev/null 2>&1; then
            default_branch=master
        else
            default_branch=main
        fi
    fi
    default_ref="origin/${default_branch}"
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
