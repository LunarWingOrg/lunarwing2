#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET="$SCRIPT_DIR/../lunarwing-mt-provision-openrc.sh"
failures=0

assert_contains() {
  local label="$1" pattern="$2"
  if grep -qF "$pattern" "$TARGET"; then
    printf '  PASS: %s\n' "$label"
  else
    printf '  FAIL: %s\n' "$label"
    failures=$((failures + 1))
  fi
}

assert_contains "bootstrap defaults enabled" 'ENABLE_WEECHAT_BOOTSTRAP=true'
assert_contains "disabled setting forwards opt-out" '[[ "$ENABLE_WEECHAT_BOOTSTRAP" == false ]] && flags+=(--no-weechat-bootstrap)'

(( failures == 0 )) || exit 1
printf 'ALL TESTS PASSED\n'
