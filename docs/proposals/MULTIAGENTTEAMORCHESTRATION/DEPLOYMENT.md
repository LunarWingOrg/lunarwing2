# Multi-Agent Coordination Pipeline — Deployment & Configuration

**Status:** Implemented for v2.0.4.0 (all 4 phases; ISSUE-001…014 review fixes applied).
**Plan:** `docs/proposals/MULTIAGENTTEAMORCHESTRATION_INTEGRATION_PLAN_v2.0.4.0.md`

The coordination pipeline enforces "pager system / ops room" discipline on
designated coordination channels (XMPP MUC, IRC): tagged single-line messages,
secret detection, per-agent rate limits, engagement policy, first-responder
correlation, three-strike auto-disable, and an outbound coalesce window.

Everything is **off by default**. Nothing activates unless `COORD_ENABLED=true`
plus the required identity/channel variables are set.

---

## Policy scope: what `COORD_CHANNELS` matches against

Hook events carry two channel identities (ISSUE-001):

- `channel` — the transport/adapter name (`xmpp`, `web`, …), used for dispatch.
- `policy_scope` — the canonical destination from routing metadata
  (`xmpp_target`/`xmpp_room`, `chat_id`, `signal_target`, `target`), falling
  back to the transport name when no metadata exists.

All coordination hooks match `COORD_CHANNELS` against the **policy scope**, so
populate it with room JIDs (e.g. `coord@conference.example`). Native and
installable (WASM) XMPP paths produce the same scope; XMPP DMs and unrelated
rooms are unaffected. Matching is case-insensitive exact equality.

Transient XMPP status output is also isolated at the shared channel boundary.
When `COORD_ENABLED=true`, thinking indicators, tool progress/results, and safe
reasoning summaries are suppressed for exact `COORD_CHANNELS` destinations
before either the native or installable WASM XMPP adapter sees them. Final agent
responses still use the normal outbound hook pipeline. Set
`COORD_XMPP_STATUS_UPDATES=true` only when status chatter is intentionally wanted
in coordination rooms; XMPP DMs and unrelated rooms are always unaffected.

## Phase 1 — Declarative hook bundle (no Rust needed)

Ships as `ic/config/hooks/coordination.hook.json`.

Deploy: copy it into the agent workspace as `hooks/coordination.hook.json` and
restart the daemon. `register_workspace_bundles()` auto-loads
`hooks/*.hook.json` at startup.

Enforces: API-key/PEM/JWT/high-entropy secret rejection (inbound + outbound),
inbound newline stripping, outbound single-line and 420-char caps.

The managed workspace path follows `COORD_ENABLED` (ISSUE-010). When enabled,
startup replaces every rule's inert shipped guard with one exact,
case-insensitive regex generated from escaped `COORD_CHANNELS` values. Other
MUCs, CLI, web, and DMs remain unaffected; no hand-editing is required.
Generic workspace hook files keep their configured guards unchanged.

## Phase 2 — Stateful Rust hooks

`ic/src/hooks/coordination.rs`, registered from `main.rs` when
`COORD_ENABLED=true`:

| Hook | Priority | Point | Mode |
|------|----------|-------|------|
| `coordination.secret_entropy` | 5 | beforeInbound, beforeOutbound | fail-closed |
| `coordination.engagement_policy` | 15 | beforeInbound | fail-open |
| `coordination.first_responder` | 16 | beforeInbound | fail-open |
| `coordination.tag_schema` | 20 | transformResponse only | fail-closed |
| `coordination.rate_limit` | 25 | beforeOutbound | fail-closed |

Notes:

- Engagement policy uses Option B from the plan: the thread may spawn but the
  agent exits without responding when the message isn't targeted at it.
  Inbound hook rejections now produce **typed suppression** (empty response,
  logged at debug) instead of synthetic `[Message rejected: …]` channel text
  that re-entered outbound hooks and mis-scored the local agent (ISSUE-004).
- `TagSchemaHook` enforces at `transformResponse` **only** (ISSUE-004): legacy,
  approval-resume, Engine V2 final responses, and mission notifications pass it exactly once,
  so one malformed response costs one strike. Rejection is typed suppression:
  no synthetic policy text is persisted, re-hooked, rate-limited, or delivered.
- `SecretEntropyHook` attributes violations by direction (ISSUE-004): inbound
  secrets strike the remote sender; outbound secrets strike the configured
  local agent (`COORD_AGENT_ID`). The event's `user_id` is the conversation
  peer and is never used for outbound attribution.
- First-responder claims are **process-local** (ISSUE-013): each daemon keeps
  its own in-memory claim map, so separate tenant daemons watching the same
  MUC can both claim and both respond. Treat it as opportunistic
  self-deduplication; a shared compare-and-insert backend is deferred (below).
- Entropy detection is supplementary to the Phase 1 regexes. Allowlisted:
  40-char hex git SHAs, file paths, URLs.

## Phase 3 — Three-strike enforcement + admin endpoints

- `ic/src/gate/violation_store.rs` — persistent violations at
  `$LUNARWING_BASE_DIR/coordination-violations.json` (atomic temp+rename).
  Escalation: strike 1 silent halt, strike 2 ops alert (`tracing::warn!` —
  wire your log alerting to it), strike 3 auto-disable (24h). An expired
  auto-disable re-arms on the next violation (ISSUE-008) — the policy is
  "re-arm on new violation", not on stale strikes alone.
- `ic/src/gate/coordination.rs` — `CoordinationGate` (`ExecutionGate`,
  priority 5) is **registered in production** (ISSUE-003): `main.rs` builds it
  from the same `Arc<ViolationStore>` as the hooks, `AgentDeps` carries it,
  and the Engine V2 effect adapter checks it first in
  `execute_action_internal` — before any tool side effect. Denials become typed
  `ExecutionStopped` control flow: executors refund pre-execution lease use,
  emit no action failure, stop the thread, and expose no reason to model or
  channel. `COORD_PERMISSIVE=true` disables it for testing.
- Admin endpoints (`ic/src/channels/web/handlers/coord.rs`): dedicated axum
  server bound to `127.0.0.1` only — **not** routed through the authenticated
  web gateway. All routes require `Authorization: Bearer $COORD_ADMIN_TOKEN`
  (constant-time compare; ISSUE-009); the server does not start when the token
  is unset (fail closed). Agent path values are validated (1–128 chars,
  JID/agent-name character set) by the same validator used for hook activation.
  `lunarwing-mt-admin.sh` generates and preserves a per-tenant token; `patch-env`
  backfills existing tenants without printing or rotating it.

  | Endpoint | Action |
  |----------|--------|
  | `GET /coord/status` | all tracked agents |
  | `GET /coord/status/{agent}` | single agent |
  | `POST /coord/clear/{agent}` | reset violations |
  | `POST /coord/disable/{agent}` | manual disable |
  | `POST /coord/enable/{agent}` | manual enable |

  Example: `curl -H "Authorization: Bearer $COORD_ADMIN_TOKEN" http://127.0.0.1:18777/coord/status`

## Phase 4 — Coalesce window

`ic/src/channels/coalesce.rs`, wired at the agent's outbound boundary in
`deliver_outbound_with_hooks` (ISSUE-002): **after** BeforeOutbound hooks,
**before** the channel send (whose transport rate limiter sees only flushed
messages). Because it lives at the boundary rather than inside any one
channel, native and installable WASM XMPP channels coalesce identically.

- 2s window (default), max 3 merges; merged same-tag messages join with
  `" | "`, first tag wins on mixed tags, exact duplicates stripped.
- `ALERT:` messages bypass coalescing entirely.
- Overflow beyond 420 chars flushes the pending message first, then buffers
  the new one (sequential delivery).
- Lookup/merge/insert happen atomically under one lock and timer identity is a
  monotonic generation, so concurrent first submissions merge (never replace)
  and stale timers can't remove reinserted entries (ISSUE-005).
- Buffer keys include agent, transport, destination, and conversation, so two
  adapters targeting the same destination cannot merge or misroute output.
- Agent shutdown flushes all pending messages before channels close and waits
  for timer/early-flush callbacks already in flight.
- Coalesced flushes pass through the existing XMPP hourly rate limiter
  (`OutboundRateLimiter`, see also `scripts/xmpp-rate-limit.sh` for the
  bridge-level control) — coalescing reduces volume, it doesn't bypass limits.

## Configuration reference

| Variable | Default | Description |
|----------|---------|-------------|
| `COORD_ENABLED` | `false` | Master switch for coordination hooks + admin server |
| `COORD_AGENT_ID` | (required) | Unique 1–128-byte ASCII ID using letters, digits, `@.-_/:+~` |
| `COORD_AGENT_ALIASES` | (required for engagement) | Comma-separated @mentions targeting this agent |
| `COORD_CHANNELS` | (required) | Comma-separated coordination destinations (policy scope: room JIDs/names) |
| `COORD_MAX_MESSAGES` | `2` | Max messages per rate window per agent |
| `COORD_RATE_WINDOW_SECS` | `3600` | Rate limit window |
| `COORD_COALESCE_MS` | `2000` | First-responder claim TTL |
| `COORD_ENTROPY_THRESHOLD` | `4.8` | Shannon entropy threshold (bits/char) |
| `COORD_MIN_SECRET_LENGTH` | `32` | Min token length for entropy checks |
| `COORD_XMPP_STATUS_UPDATES` | `false` | Allow transient thinking/tool/reasoning status messages in configured XMPP coordination rooms |
| `COORD_ADMIN_USERS` | (empty) | Comma-separated admin user IDs |
| `COORD_VIOLATION_MAX` | `3` | Strikes before auto-disable |
| `COORD_VIOLATION_DISABLE_DURATION` | `86400` | Auto-disable seconds (24h) |
| `COORD_VIOLATION_DECAY` | `604800` | Violation decay seconds (7d) |
| `COORD_PERMISSIVE` | `false` | Disable CoordinationGate (testing) |
| `COORD_ADMIN_PORT` | `18777` | Admin server port (always 127.0.0.1); MT provisioning assigns `extended_base+9` |
| `COORD_ADMIN_TOKEN` | (required for admin API) | Bearer token; MT provisioning generates/preserves it, otherwise server stays down when unset |
| `COALESCE_ENABLED` | `true` | Coalesce window switch |
| `COALESCE_WINDOW_SEC` | `2.0` | Coalesce window seconds |
| `COALESCE_MAX_MERGES` | `3` | Flush after this many merges |

## XMPP rate limiting (two layers)

1. **Coordination rate limit** (new): `RateLimitHook` caps each agent at
   `COORD_MAX_MESSAGES` per hour *in coordination channels*, pre-send.
2. **Transport rate limit** (existing): the XMPP adapter's
   `OutboundRateLimiter` caps total outbound stanzas per hour; manage the
   bridge-level limit with `scripts/xmpp-rate-limit.sh`
   (`status` / `set <n>` / `off` / `reset`, requires `XMPP_BRIDGE_TOKEN`).

Both apply to coordination traffic; the stricter one wins.

## Deferred to 2.0.5.0+

- Shared first-responder backend (host-local store or broker compare-and-insert
  keyed by correlation id). Current behavior is process-local (ISSUE-013).
- Router-level engagement gating (Option A: suppress thread spawning on
  BeforeInbound rejections).
