#!/usr/bin/env bash
# Convenience launcher for the LunarWing MT Onboard web edition.
#
#   ./run.sh --demo          # safe simulation, no sudo, no system changes
#   sudo ./run.sh            # real provisioning (drives lunarwing-mt-admin.sh)
#
# Any extra args (--port, --log-dir, ...) are passed through.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
VENV_PY="$SCRIPT_DIR/.venv/bin/python"

if [ ! -x "$VENV_PY" ]; then
  echo "Creating virtualenv at $SCRIPT_DIR/.venv ..." >&2
  python3 -m venv "$SCRIPT_DIR/.venv"
  "$SCRIPT_DIR/.venv/bin/pip" install -q --upgrade pip
  "$SCRIPT_DIR/.venv/bin/pip" install -q -r "$SCRIPT_DIR/requirements.txt"
fi

# Run from the repo root so both `lunarwing_mt_onboard_web` and the reused
# `lunarwing_mt_onboard` package import cleanly.
cd "$REPO_ROOT"
exec "$VENV_PY" -m lunarwing_mt_onboard_web "$@"
