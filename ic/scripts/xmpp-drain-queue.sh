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

BASE="${BASE:-}"
XMPP_BRIDGE_TOKEN="${XMPP_BRIDGE_TOKEN:-}"
export -n XMPP_BRIDGE_TOKEN

usage() {
  cat <<'EOF'
Usage:
  scripts/xmpp-drain-queue.sh status
  scripts/xmpp-drain-queue.sh drain --all
  scripts/xmpp-drain-queue.sh drain <older_than_secs>

Environment:
  BASE               required loopback URL, e.g. http://127.0.0.1:8787
                     (use the tenant's registered bridge port on MT hosts)
  XMPP_BRIDGE_TOKEN  required

Commands:
  status             show retained and pending counts and the oldest pending age
  drain --all        discard all pending inbound messages, including a page awaiting retry
  drain n            discard never-served pending messages at least n seconds old

Notes:
  Run status first. Use drain only to intentionally discard unacknowledged
  messages on a surviving bridge, before the daemon resumes polling.
  This does not protect messages already queued inside the daemon.
EOF
}

require_token() {
  if [[ -z "$XMPP_BRIDGE_TOKEN" ]]; then
    echo "error: XMPP_BRIDGE_TOKEN is not set" >&2
    exit 1
  fi
  if [[ "$XMPP_BRIDGE_TOKEN" =~ [[:cntrl:]] ]]; then
    echo "error: XMPP_BRIDGE_TOKEN contains control characters" >&2
    exit 1
  fi
}

bridge_curl() {
  printf 'Authorization: Bearer %s\n' "$XMPP_BRIDGE_TOKEN" |
    curl --disable --silent --show-error --fail-with-body \
      --connect-timeout 5 --max-time 30 --noproxy '*' -H @- "$@"
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
  response="$(bridge_curl "$BASE/v1/status")"
  print_json_or_fail "$response" '{
      configured,
      running,
      queued_messages,
      pending_messages,
      oldest_queued_age_secs
    }'
}

drain_queue() {
  local mode="$1"
  local payload
  if [[ "$mode" == --all ]]; then
    payload='{"all":true}'
  else
    if ! [[ "$mode" =~ ^(0|[1-9][0-9]*)$ ]]; then
      echo "error: older_than_secs must be a non-negative integer" >&2
      exit 1
    fi
    printf -v payload '{"older_than_secs":%s}' "$mode"
  fi

  local response
  response="$(bridge_curl -X POST "$BASE/v1/messages/drain" \
    -H 'Content-Type: application/json' \
    -d "$payload")"
  print_json_or_fail "$response"
}

if [[ "$#" -eq 0 ]]; then
  usage >&2
  exit 1
fi

command_name="$1"
shift

case "$command_name" in
  status|drain)
    if ! [[ "$BASE" =~ ^http://(127\.0\.0\.1|\[::1\]):([1-9][0-9]{0,4})/?$ ]]; then
      echo "error: set BASE to the bridge's explicit loopback HTTP URL and port" >&2
      exit 1
    fi
    BASE="${BASE%/}"
    require_token
    ;;
esac

case "$command_name" in
  status)
    if [[ "$#" -ne 0 ]]; then
      echo "error: status takes no extra arguments" >&2
      exit 1
    fi
    show_status
    ;;
  drain)
    if [[ "$#" -ne 1 ]]; then
      echo "error: drain requires --all or an age in seconds" >&2
      exit 1
    fi
    drain_queue "$1"
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
