#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR/.."

# Ensure Flask is available
python3 -c "import flask" 2>/dev/null || pip3 install flask

PORT="${LUNARWING_WEB_PORT:-7424}"

echo ""
echo "  LunarWing MT Web Onboarding"
echo "  ~~~~~~~~~~~~~~~~~~~~~~~~~~~~"
echo "  Starting on http://127.0.0.1:${PORT}"
echo "  Press Ctrl+C to stop"
echo ""

# Try to open browser (non-blocking)
(command -v xdg-open &>/dev/null && xdg-open "http://127.0.0.1:${PORT}") 2>/dev/null &

exec python3 -m lunarwing_mt_web.server --port "$PORT"
