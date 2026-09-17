#!/usr/bin/env bash
#
# Shellcheck gate for lunarwing-mt-admin.sh and its mt-admin-lib/ modules (T7).
# Fails if any warning-or-above issue is found. Run manually, in CI, or as a
# pre-commit hook. Install shellcheck separately — the project floor is 0.11.0
# (Debian bookworm's apt 0.9.0 is too old; CI installs a checksum-pinned
# 0.11.0 via .gitlab/scripts/install-shellcheck.sh).
#
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
TARGETS=("$SCRIPT_DIR/lunarwing-mt-admin.sh")
if [[ -d "$SCRIPT_DIR/mt-admin-lib" ]]; then
  TARGETS+=("$SCRIPT_DIR"/mt-admin-lib/*.sh)
fi

command -v shellcheck >/dev/null 2>&1 || {
  echo "error: shellcheck >= 0.11.0 is required (install: pacman -S shellcheck / apt install shellcheck / .gitlab/scripts/install-shellcheck.sh)" >&2
  exit 2
}

if shellcheck -S warning "${TARGETS[@]}"; then
  echo "✓ shellcheck clean: ${TARGETS[*]}"
  exit 0
else
  echo "✗ shellcheck issues found (fix or document in .shellcheckrc)" >&2
  exit 1
fi
