#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
zero_sha="0000000000000000000000000000000000000000"

export BASE_BRANCH="${CI_MERGE_REQUEST_TARGET_BRANCH_NAME:-${CI_DEFAULT_BRANCH:-}}"
export BASE_SHA="${CI_MERGE_REQUEST_DIFF_BASE_SHA:-${CI_COMMIT_BEFORE_SHA:-}}"
export CHANGE_LABELS="${CI_MERGE_REQUEST_LABELS:-}"

if [[ "$BASE_SHA" == "$zero_sha" ]]; then
    BASE_SHA=""
    export BASE_SHA
fi

bash "$repo_root/ic/scripts/check-version-bumps.sh"
