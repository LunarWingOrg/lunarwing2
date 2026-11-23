#!/usr/bin/env bash
set -euo pipefail

script="$(dirname "$(realpath "$0")")/../xmpp-drain-queue.sh"
fixture="$(mktemp -d)"
trap 'rm -rf -- "$fixture"' EXIT
export PATH="$fixture:$PATH" CALLS="$fixture/calls"

cat >"$fixture/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ -z "${XMPP_BRIDGE_TOKEN+x}" ]]
[[ "$*" != *fixture-token* ]]
[[ "$*" == *'--fail-with-body --connect-timeout 5 --max-time 30'* ]]
[[ "$*" == *'-H @-'* ]]
IFS= read -r header
[[ "$header" == 'Authorization: Bearer fixture-token' ]]
printf '%s\n' "$@" >"$CALLS"
if [[ "${MOCK_HTTP_ERROR:-0}" == 1 ]]; then
  echo '{"error":"invalid bridge token"}'
  exit 22
fi
echo '{"queued_messages":5,"pending_messages":2,"oldest_queued_age_secs":20,"drained":2,"remaining":0}'
EOF
chmod +x "$fixture/curl"
export XMPP_BRIDGE_TOKEN=fixture-token
unset BASE

assert_exit() {
  local expected="$1" rc=0
  shift
  bash "$script" "$@" >"$fixture/output" 2>"$fixture/error" || rc=$?
  if [[ "$rc" != "$expected" ]]; then
    printf 'Expected exit %s for %s, got %s\n' "$expected" "$*" "$rc" >&2
    cat "$fixture/error" >&2
    exit 1
  fi
}

assert_exit 1 status
export BASE=http://127.0.0.1:18787
assert_exit 1 drain
assert_exit 1 drain -1
[[ ! -e "$CALLS" ]]

assert_exit 0 status
jq -e '.queued_messages == 5 and .pending_messages == 2' "$fixture/output" >/dev/null
grep -Fx "$BASE/v1/status" "$CALLS"
assert_exit 0 drain --all
grep -Fx '{"all":true}' "$CALLS"
assert_exit 0 drain 3600
grep -Fx '{"older_than_secs":3600}' "$CALLS"

export MOCK_HTTP_ERROR=1
assert_exit 22 status
assert_exit 22 drain --all
echo "XMPP drain CLI tests passed"
