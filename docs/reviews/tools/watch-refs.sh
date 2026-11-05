#!/bin/bash
# watch-refs.sh: bounded watcher for new or moved MR heads and branch heads on origin.
#
# Usage: watch-refs.sh [-m MINUTES] [-i SECONDS] [-r REPO] BASELINE
#   -m  window length in minutes (default 20)
#   -i  poll interval in seconds (default 60)
#   -r  repository to run `git ls-remote origin` in (default: the repo this script lives in)
#   BASELINE  snapshot file; created on the first run, compared against on every poll
#
# Exit 0  = a ref was added or moved; the new `<sha>\t<ref>` lines are printed after CHANGE
# Exit 124 = the window elapsed with no change
# Exit 2  = usage or repository error
#
# Deletions are ignored. The ignore list (WATCH_IGNORE_REGEX, grep -E over "<sha>\t<ref>")
# defaults to the testing/* build branches, the maintainer's goal<N>cmc / cmcgoal<N> and
# vibetui* branches, and the long-lived wip MRs !1792 and !1799.
set -u

usage() { sed -n '2,15p' "$0" >&2; exit 2; }
ts() { date -u +%Y-%m-%dT%H:%M:%SZ; }

minutes=20; interval=60; repo=""
while getopts "m:i:r:h" opt; do
  case $opt in
    m) minutes=$OPTARG ;;
    i) interval=$OPTARG ;;
    r) repo=$OPTARG ;;
    *) usage ;;
  esac
done
shift $((OPTIND - 1))
[ $# -eq 1 ] || usage

base=$(readlink -f -- "$1")   # absolute: we cd into the repo below
if [ -z "$repo" ]; then
  repo=$(git -C "$(dirname -- "$(readlink -f -- "$0")")" rev-parse --show-toplevel 2>/dev/null) || repo=""
fi
if [ -z "$repo" ] || ! cd -- "$repo"; then
  echo "cannot find the repository (use -r)" >&2
  exit 2
fi

tab=$'\t'
ignore=${WATCH_IGNORE_REGEX:-"${tab}refs/heads/(testing/|goal[0-9]+cmc\$|cmcgoal[0-9]+\$|vibetui[0-9]*\$)|${tab}refs/merge-requests/(1792|1799)/head\$"}

snap() {
  git ls-remote origin 'refs/merge-requests/*/head' 'refs/heads/*' 2>/dev/null | grep -v -E -e "$ignore" | sort
}

deadline=$(( $(date +%s) + minutes * 60 ))
if [ ! -s "$base" ]; then
  snap > "$base"
  [ -s "$base" ] || { echo "$(ts) baseline snapshot failed (network?)" >&2; rm -f -- "$base"; exit 2; }
  echo "$(ts) BASELINE $(wc -l < "$base") refs -> $base"
fi

while :; do
  cur=$(snap)
  if [ -n "$cur" ]; then
    add=$(comm -13 "$base" <(printf '%s\n' "$cur"))
    if [ -n "$add" ]; then
      echo "$(ts) CHANGE"
      printf '%s\n' "$add"
      exit 0
    fi
  fi
  now=$(date +%s)
  if [ "$now" -ge "$deadline" ]; then
    echo "$(ts) TIMEOUT after $minutes min, no change"
    exit 124
  fi
  remaining=$(( deadline - now ))
  sleep $(( remaining < interval ? remaining : interval ))
done
