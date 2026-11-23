#!/usr/bin/env bash
set -euo pipefail

if ! command -v curl >/dev/null 2>&1; then
  echo "error: curl is required" >&2
  exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq is required" >&2
  exit 1
fi

BASE="${BASE:-http://127.0.0.1:8787}"
XMPP_BRIDGE_TOKEN="${XMPP_BRIDGE_TOKEN:-}"

usage() {
  cat <<'EOF'
Usage:
  scripts/xmpp-drain-queue.sh status
  scripts/xmpp-drain-queue.sh drain [older_than_secs]

Environment:
  BASE               default: http://127.0.0.1:8787
  XMPP_BRIDGE_TOKEN  required

Commands:
  status             show queued inbound message count and the oldest queue age
  drain [n]          discard queued inbound messages (all, or only those that
                     have waited at least n seconds)

Notes:
  The drain verb is pre-hop hygiene (WI#854): the bridge queue is in memory, so
  a backlog left over from a daemon outage would otherwise be served as fresh
  turns right after the restart. A drain is explicit, counted, and logged by
  the bridge -- it never discards silently.

  Run `status` first and drain only when the queued count is unexpected or the
  oldest age shows messages that can no longer be current. To bound this
  automatically, enable the opt-in stale-queue policy with
  XMPP_BRIDGE_STALE_QUEUE_SECS on xmpp-bridge.

  This drains the BRIDGE queue only. Messages the daemon already polled sit in
  its own in-memory queue and are not affected.
EOF
}

require_token() {
  if [[ -z "$XMPP_BRIDGE_TOKEN" ]]; then
    echo "error: XMPP_BRIDGE_TOKEN is not set" >&2
    exit 1
  fi
}

print_json_or_fail() {
  local response="$1"
  local jq_filter="${2:-.}"

  if jq -e . >/dev/null 2>&1 <<<"$response"; then
    jq "$jq_filter" <<<"$response"
  else
    echo "error: bridge returned a non-JSON response:" >&2
    printf '%s\n' "$response" >&2
    return 1
  fi
}

show_status() {
  local response
  response="$(curl -sS "$BASE/v1/status" \
    -H "Authorization: Bearer $XMPP_BRIDGE_TOKEN")"
  print_json_or_fail "$response" '{
      configured,
      running,
      queued_messages,
      oldest_queued_age_secs
    }'
}

drain_queue() {
  local older_than_secs="${1:-}"
  local payload
  if [[ -z "$older_than_secs" ]]; then
    payload='{}'
  else
    if ! [[ "$older_than_secs" =~ ^[0-9]+$ ]]; then
      echo "error: older_than_secs must be a non-negative integer" >&2
      exit 1
    fi
    payload="$(jq -cn --argjson n "$older_than_secs" '{older_than_secs: $n}')"
  fi

  local response
  response="$(curl -sS -X POST "$BASE/v1/messages/drain" \
    -H "Authorization: Bearer $XMPP_BRIDGE_TOKEN" \
    -H 'Content-Type: application/json' \
    -d "$payload")"
  print_json_or_fail "$response"
}

require_token

if [[ "$#" -eq 0 ]]; then
  usage >&2
  exit 1
fi

command_name="$1"
shift

case "$command_name" in
  status)
    if [[ "$#" -ne 0 ]]; then
      echo "error: status takes no extra arguments" >&2
      exit 1
    fi
    show_status
    ;;
  drain)
    if [[ "$#" -gt 1 ]]; then
      echo "error: drain takes at most one argument" >&2
      exit 1
    fi
    drain_queue "${1:-}"
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    echo "error: unknown command: $command_name" >&2
    usage >&2
    exit 1
    ;;
esac
