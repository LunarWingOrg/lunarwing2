#!/usr/bin/env bash
# Create or reuse a git worktree for one feature slug.
# Does NOT switch the main checkout branch.
# usage: feature-worktree.sh <slug> [base-ref]
set -euo pipefail

SLUG="${1:?usage: feature-worktree.sh <slug> [base-ref]}"
BASE="${2:-}"
ROOT="$(git rev-parse --show-toplevel)"
SAFE="$(printf '%s' "$SLUG" | tr '[:upper:]' '[:lower:]' | sed -E 's/[^a-z0-9]+/-/g; s/^-+|-+$//g')"
if [[ -z "$SAFE" ]]; then
  echo "ERROR: slug produced empty safe name from: $SLUG" >&2
  exit 2
fi

BRANCH="feat/${SAFE}"
WT_ROOT="${ROOT}/.worktrees"
WT="${WT_ROOT}/${SAFE}"

mkdir -p "$WT_ROOT"

if [[ -d "$WT" ]] && { [[ -d "$WT/.git" ]] || [[ -f "$WT/.git" ]]; }; then
  echo "WORKTREE=$WT"
  echo "BRANCH=$(git -C "$WT" branch --show-current 2>/dev/null || echo "$BRANCH")"
  echo "STATUS=reused"
  exit 0
fi

if [[ -z "$BASE" ]]; then
  if git rev-parse --verify -q origin/main >/dev/null; then
    BASE=origin/main
  elif git rev-parse --verify -q origin/master >/dev/null; then
    BASE=origin/master
  else
    BASE="$(git rev-parse HEAD)"
  fi
fi

# Fetch is best-effort; offline / no remote still works from local refs.
git fetch --quiet origin 2>/dev/null || true

if git show-ref --verify --quiet "refs/heads/${BRANCH}"; then
  git worktree add "$WT" "$BRANCH"
else
  git worktree add -b "$BRANCH" "$WT" "$BASE"
fi

echo "WORKTREE=$WT"
echo "BRANCH=$BRANCH"
echo "STATUS=created"
echo "BASE=$BASE"
