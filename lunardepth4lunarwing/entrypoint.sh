#!/bin/bash
set -euo pipefail

export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

case "${LUNARWING_CONTAINER_ROLE:-}" in
  split-bridge|split-task)
    exec /app/entrypoint-split.sh "$@"
    ;;
  *)
    printf '%s\n' 'LUNARWING_CONTAINER_ROLE=split-bridge or split-task is required' >&2
    exit 1
    ;;
esac
