# Agent Framework: Kageho & Baud

A hook-based pipeline and policy framework for multi-agent coordination in a group chat environment (XMPP/OMEMO on Prosody, with IRC as a legacy reference point). Covers OpenClaw (IRC-facing) and IronClaw (TEE-focused) agent behavior.

---

## Table of contents

1. [Mental model](#mental-model)
2. [The pipeline](#the-pipeline)
3. [Lifecycle hooks](#lifecycle-hooks)
4. [XMPP/OMEMO adaptations](#xmppomemo-adaptations)
5. [Engagement policy — when to speak](#engagement-policy--when-to-speak)
6. [Coordination-channel posture](#coordination-channel-posture)
7. [Allowed message schema](#allowed-message-schema)
8. [Rate limiting and admin control](#rate-limiting-and-admin-control)
9. [Enforcement protocol (three-strike)](#enforcement-protocol-three-strike)
10. [Secret detection](#secret-detection)
11. [Length caps](#length-caps)
12. [Config reference](#config-reference)
13. [Pipeline summary](#pipeline-summary)

---

## Mental model

The coordination MUC is a coordination channel, not a conversation channel. The goal is to have a highly structured, machine-readable message flow for multi-agent coordination and status updates.

A common mental model is that of a pager system or an ops room — where only essential, structured messages are allowed. It is not a place for casual chat, idle questions, or back-and-forth dialogue.

This is a design constraint and a feature: it ensures that every message has purpose, and agents do not get lost in conversational drift.

## The pipeline

1. Message received (inbound)
2. `on_inbound_message`
3. `on_should_engage`
4. `on_response_filter`
5. Message sent (outbound)

Each hook is a "gate" — if any returns `Refuse` or `Veto`, the pipeline halts silently. No message is sent, and no log is written to the coordination MUC.

## Lifecycle hooks

- `on_inbound_message` — validates message structure (e.g. tags, schema), detects secrets, enforces rate limits
- `on_should_engage` — returns `Engage`, `Pass`, or `Veto` based on message content, history, and policy
- `on_response_filter` — normalizes, reformats, or redacts outgoing messages to match allowed schema

All hooks have access to shared context: KV store, clock, log, etc.

## XMPP/OMEMO adaptations

- Messages are sent in a single line (no newlines)
- Maximum message length: 420 characters
- OMEMO encryption is used by default
- Users/agents are addressed by JID
- MUCs are used for coordination
- Pre-message coalescing window: 1.5–3 seconds to allow edits

## Engagement policy — when to speak

- Only respond if:
  - Message is tagged with `@agent`
  - Message contains `@agent` in body
  - Message is in direct reply to agent (MUC or DM)
- First responder wins: use correlation IDs to prevent double responses
- Silent halts are preferred: if no response is correct, do nothing

## Coordination-channel posture

- Coordination MUC is not a conversation — only structured messages allowed
- Messages are concise and formatted
- Use tags: `STATUS`, `ALERT`, `COORD`, `ANNOUNCE`, `QUERY`
- No idle chatter, no casual questions, no "hey" or "what's up"
- Agents may be present in casual channel, but coordination is separate

## Allowed message schema

- Tags: `STATUS`, `ALERT`, `COORD`, `ANNOUNCE`, `QUERY`
- Format:
  - One-line only
  - 420 characters max
  - Tags at start: `STATUS: Node A online`
- Examples:
  - `STATUS: System online`
  - `ALERT: High CPU on node B`
  - `COORD: Starting deployment`
  - `ANNOUNCE: Update complete`
  - `QUERY: What is the status of node C?`

## Rate limiting and admin control

- Admin commands sent to localhost-only HTTP endpoint for privileged control
- Rate limiting: 2 messages per hour in group channels, per agent
- Admins can manually trigger actions via secure channel

## Enforcement protocol (three-strike)

- 1st violation: silent halt (no message, no log)
- 2nd violation: ops alert (Gotify or DM)
- 3rd violation: silent halt + ops alert + auto-disable (24h)
- Violations include:
  - Message structure invalid
  - Secret detected
  - Rate limit exceeded
  - Response to non-targeted message
  - Incorrect schema
- Silent halts are preferred — do not explain the rule to the channel

## Secret detection

- Use regex and entropy checks to detect potential secrets
- If secret is detected, halt silently
- Do not attempt redaction — refusal is the correct response

## Length caps

- 420 characters max per message
- Enforced at `on_response_filter` stage

## Config reference

- `max_message_length`: 420
- `rate_limit_per_hour`: 2
- `allowed_tags`: ["STATUS", "ALERT", "COORD", "ANNOUNCE", "QUERY"]
- `coalesce_window_sec`: 3.0
- `admin_http_bind`: "127.0.0.1:8080"

## Pipeline summary

1. Inbound message arrives
2. `on_inbound_message` — validate structure, check for secrets, enforce rate limit
3. `on_should_engage` — decide: Engage, Pass, Veto
4. If Engage:
   - Generate response
   - Coalesce for 1.5–3s (allow edits)
   - `on_response_filter` — format, cap length, enforce schema
   - Send
5. If Pass or Veto: halt silently

---

This framework ensures clean, secure, and purposeful agent coordination.
