#!/usr/bin/env bash
# Report status of a feature worktree.
# usage: feature-status.sh <worktree> [base-ref]
set -euo pipefail

WT="${1:?usage: feature-status.sh <worktree> [base-ref]}"
BASE="${2:-}"

if [[ ! -d "$WT" ]]; then
  echo "ERROR: worktree not found: $WT" >&2
  exit 2
fi

cd "$WT"
ROOT="$(git rev-parse --show-toplevel)"
BRANCH="$(git branch --show-current 2>/dev/null || true)"
HEAD="$(git rev-parse --short HEAD)"
DIRTY="$(git status --porcelain | wc -l | tr -d ' ')"

if [[ -z "$BASE" ]]; then
  if git rev-parse --verify -q origin/main >/dev/null; then
    BASE=origin/main
  elif git rev-parse --verify -q origin/master >/dev/null; then
    BASE=origin/master
  else
    BASE="$(git rev-parse HEAD~0)"
  fi
fi

echo "WORKTREE=$WT"
echo "ROOT=$ROOT"
echo "BRANCH=${BRANCH:-detached}"
echo "HEAD=$HEAD"
echo "DIRTY=$DIRTY"
echo "BASE=$BASE"
echo "CHANGED_FILES:"
if git rev-parse --verify -q "$BASE" >/dev/null; then
  git diff --name-only "${BASE}...HEAD" 2>/dev/null || git diff --name-only
else
  git diff --name-only
fi
echo "STATUS_PORCELAIN:"
git status --porcelain
