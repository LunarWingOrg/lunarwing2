#!/usr/bin/env bash
# Normal-CI entrypoint for synthetic, non-root H3 scanner tests.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover \
  -s "$SCRIPT_DIR" -p 'test_darkirc_h3_analyzer.py'
