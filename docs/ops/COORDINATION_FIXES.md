# Coordination fixes

## Update this file when new issues with coordination arise

## Issue #1: untagged inbound traffic disabled receiving agent

Observed on XMPP coordination room: short untagged messages reached model, model
returned normal persona prose, and `coordination.tag_schema` assigned all three
outbound failures to local agent. Local agent then auto-disabled for 24 hours.
Tag matcher itself was correct; it requires case-insensitive `TAG:` at start.

Root causes:

- tag schema ran only at `transformResponse`, not `beforeInbound`;
- installable XMPP bridge collapsed MUC sender and room target into `user_id`;
- model received no coordination-specific response contract, so persona prose
  commonly preceded required tag.

Fix:

- validate inbound schema before model execution at priority 14;
- attribute inbound violations to `sender_id`, while model-output violations
  remain attributed once to local agent at `transformResponse`;
- preserve bridge `sender_id` with backward-compatible payload decoding;
- inject exact room-scoped system instruction requiring one-line tagged output.

Operator recovery after deploying fix (run with tenant environment loaded; never
print token):

```bash
curl -fsS -X POST \
  -H "Authorization: Bearer ${COORD_ADMIN_TOKEN:?}" \
  "http://127.0.0.1:${COORD_ADMIN_PORT:?}/coord/clear/alpha"
```

Then test with valid input such as `QUERY: @derpy report status`.
