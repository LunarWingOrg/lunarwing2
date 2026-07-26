#!/usr/bin/env bash
#
# Shellcheck gate for lunarwing-mt-admin.sh and its mt-admin-lib/ libraries (T7).
# Fails if any warning-or-above issue is found. Run manually, in CI, or as a
# pre-commit hook. Install shellcheck separately (e.g. `pacman -S shellcheck`).
#
# The composition root is linted with -x so shellcheck follows the `source`
# directives into mt-admin-lib/ — that pass sees the real symbol set and is the
# authoritative gate. Each library is then linted standalone as a scoping aid:
# out of context it cannot know which globals the root defines, so SC2034
# (unused, i.e. only read by a heredoc in a sibling library) and SC2154
# (referenced but not assigned) are advisory there and reported, not fatal.
#
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
TARGET="$SCRIPT_DIR/lunarwing-mt-admin.sh"
LIB_DIR="$SCRIPT_DIR/mt-admin-lib"

command -v shellcheck >/dev/null 2>&1 || {
  echo "error: shellcheck is required (install: pacman -S shellcheck / apt install shellcheck)" >&2
  exit 2
}

rc=0

echo "--- composition root (with -x, follows mt-admin-lib/) ---"
if shellcheck -S warning -x "$TARGET"; then
  echo "✓ shellcheck clean: $TARGET (+ sourced libraries)"
else
  echo "✗ shellcheck issues found in $TARGET (fix or document in .shellcheckrc)" >&2
  rc=1
fi

echo
echo "--- per-library scoped lint (advisory: SC2034/SC2154 excluded) ---"
shopt -s nullglob
for lib in "$LIB_DIR"/*.sh; do
  if shellcheck -S warning -e SC2034,SC2154 "$lib"; then
    printf '✓ %s\n' "${lib#"$SCRIPT_DIR"/}"
  else
    printf '✗ %s\n' "${lib#"$SCRIPT_DIR"/}" >&2
    rc=1
  fi
done

echo
echo "--- symbol selfcheck (library load order / completeness) ---"
if "$TARGET" selfcheck; then
  echo "✓ selfcheck passed"
else
  echo "✗ selfcheck failed" >&2
  rc=1
fi

exit "$rc"
