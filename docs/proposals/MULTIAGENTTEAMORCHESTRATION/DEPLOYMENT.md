# Multi-Agent Coordination Pipeline — Deployment & Configuration

**Status:** Implemented for v2.0.4.0 (all 4 phases; ISSUE-001…014 review fixes applied).
**Plan:** `docs/proposals/MULTIAGENTTEAMORCHESTRATION_INTEGRATION_PLAN_v2.0.4.0.md`

The coordination pipeline enforces "pager system / ops room" discipline on
designated coordination channels (XMPP MUC, IRC): tagged single-line messages,
secret detection, per-agent rate limits, engagement policy, first-responder
correlation, three-strike auto-disable, an outbound coalesce window, and
optional cross-tenant knowledge sharing via `KNOW:` messages.

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

Ships as `ic/config/hooks/coordination.hook.json` and is compiled into the daemon.
No workspace copy is required. When coordination config is actionable, startup
registers all seven managed rules and replaces each inert shipped guard with one
exact, case-insensitive regex generated from escaped `COORD_CHANNELS` values.
A legacy `hooks/coordination.hook.json` workspace copy is ignored to prevent
duplicate enforcement; generic workspace hooks still load normally.

Enforces: API-key/PEM/JWT rejection (inbound + outbound), inbound newline
stripping, and outbound single-line/420-character caps. High-entropy detection is
owned by the strike-aware Rust hook below so exact SHAs, paths, and HTTP(S) URLs
can pass without the old blanket base64 false positives. Other MUCs, CLI, web,
and DMs remain unaffected.

## Phase 2 — Stateful Rust hooks

`ic/src/hooks/coordination.rs`, registered from `main.rs` when
`COORD_ENABLED=true`:

| Hook | Priority | Point | Mode |
|------|----------|-------|------|
| `coordination.secret_entropy` | 5 | beforeInbound, beforeOutbound, transformResponse | fail-closed |
| `coordination.knowledge_share` | 13 | beforeInbound | fail-open (see below) |
| `coordination.engagement_policy` | 15 | beforeInbound | fail-open |
| `coordination.first_responder` | 16 | beforeInbound | fail-open |
| `coordination.tag_schema` | 14 | beforeInbound, transformResponse | fail-closed |
| `coordination.rate_limit` | 25 | beforeOutbound | fail-closed |

Notes:

- Engagement policy uses Option B from the plan: the outer message task may
  spawn, but the agent exits before session/thread resolution, model generation,
  or tool execution when a message is not targeted. Inbound hook rejections
  produce **typed suppression** (empty response, logged at debug) instead of
  synthetic `[Message rejected: …]` channel text that re-entered outbound hooks
  and mis-scored the local agent (ISSUE-004).
- `TagSchemaHook` rejects malformed inbound room traffic before model execution
  and attributes that violation to the channel sender, not the local agent or
  conversation owner. Outbound enforcement remains `transformResponse` only:
  legacy, approval-resume, Engine V2 final responses, and mission notifications
  pass it exactly once, so one malformed response costs one strike. Rejection is
  typed suppression: no synthetic policy text is persisted, re-hooked,
  rate-limited, or delivered.
- Both legacy and Engine V2 receive a host-authored, coordination-scoped system
  instruction requiring one-line tagged final output. It overrides persona/style
  formatting for configured destinations without affecting DMs, casual rooms,
  or other channels.
- The XMPP bridge contract carries `sender_id` separately from the room/storage
  target. Older payloads remain accepted; installable XMPP now preserves the
  actual MUC actor for engagement and violation attribution.
- `SecretEntropyHook` is the strike-aware authority for credential assignments,
  private-key markers, JWTs, and high-entropy tokens. Inbound detections strike
  the remote sender; outbound/transform detections strike the configured local
  agent (`COORD_AGENT_ID`) exactly once. Exact 40-hex SHAs, explicit paths, and
  absolute HTTP(S) URLs bypass entropy only; explicit secret patterns still win.
- Set the same `COORD_RESPONDER_ROSTER` on every daemon watching a coordination
  room to enable team-wide first-responder behavior for unaddressed `QUERY:`
  messages. Each daemon computes the same rendezvous-hash winner from the stable
  sender/room/content correlation ID, so exactly one listed agent engages without
  a shared service. Explicit mentions still route directly. A process-local TTL
  claim then suppresses transport replays at the elected daemon.

## Phase 3 — Three-strike enforcement + admin endpoints

- `ic/src/gate/violation_store.rs` — persistent violations at
  `$LUNARWING_BASE_DIR/coordination-violations.json` (atomic temp+rename).
  Escalation: strike 1 silent halt, strike 2 ops alert (`tracing::warn!` —
  wire your log alerting to it), strike 3 auto-disable (24h). An expired
  auto-disable re-arms on the next violation (ISSUE-008) — the policy is
  "re-arm on new violation", not on stale strikes alone.
- `ic/src/gate/coordination.rs` — `CoordinationGate` (`ExecutionGate`,
  priority 5) is **registered in production** (ISSUE-003): `main.rs` builds it
  from the same `Arc<ViolationStore>` as the hooks and `AgentDeps` carries it.
  The shared agent boundary silently suppresses external activity before legacy
  chat, reflex, approval-resume, or Engine V2 routing; internal notifications
  remain deliverable. Engine V2 also checks the gate immediately before every
  effect and returns typed `ExecutionStopped` control flow, so no side effect or
  policy reason reaches model/channel. `COORD_PERMISSIVE=true` disables it for
  testing.
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

## Cross-tenant knowledge sharing (`KNOW:` tag)

`ic/src/hooks/coordination_knowledge.rs`, off by default
(`COORD_KNOWLEDGE_SHARE=true` plus a database-backed workspace to enable).

A coordination room is watched by one daemon per tenant. A `KNOW:` message is
a one-line durable finding that **every watching daemon ingests into its own
tenant's shared ("herd") knowledge scope** as a `needs-verification` study
note — the same store the native `knowledge_search` / `knowledge_get_topic`
tools read. That makes the coordination pipeline a cross-tenant knowledge
propagation bus with no shared database between tenants.

Wire format (single line, subject to all coordination channel discipline):

    KNOW: <topic-slug> | <one-line finding>

- The topic slug follows the knowledge-store contract (lowercase, digits,
  dashes). Invalid or malformed shares are ignored silently — the tag-schema
  hook owns structural enforcement and strikes.
- The sender (JID/nick/agent id) is slugified into the note's `agent` field
  (`study-notes/<sender-slug>/<topic>.md`), so the same sender resending a
  topic updates only their own version, and different tenants' agents keep
  independent versions — identical to normal knowledge-store versioning.
- Trust model: wire-ingested notes are remote claims, never locally
  verified. They are always stored `needs-verification` with
  `confidence: null` and the raw sender recorded in `submitted_by`, tagged
  `coordination`. Agents must verify before relying on them (same policy as
  the study-notes skill).
- Ingestion is fail-open and side-effect-only: the hook never modifies or
  rejects the message, so room routing is unchanged. It runs at priority 13
  — before tag-schema (14) and engagement (15) — so broadcast shares are
  ingested even when this daemon would not engage.
- Rate limit: max notes per sender per hour (`COORD_KNOWLEDGE_MAX_PER_HOUR`,
  default 30) plus a minimum finding length
  (`COORD_KNOWLEDGE_MIN_CHARS`, default 12). Rate-limited shares are
  dropped (never ingested) but the message itself still flows.
- Outbound sharing needs no new machinery: `KNOW` is an allowed coordination
  tag, so agents (and humans) can emit it directly. When sharing is enabled,
  the coordination prompt policy appends the `KNOW:` format so agents share
  durable findings with sibling tenants unprompted.
- The background knowledge sweep treats ingested notes like any other: an
  unverified note stays `needs-verification` until an agent verifies or
  supersedes it.

## Configuration reference

| Variable | Default | Description |
|----------|---------|-------------|
| `COORD_ENABLED` | `false` | Master switch for hooks, gate, prompt policy, coalescer, and admin server |
| `COORD_AGENT_ID` | (required) | Unique 1–128-byte ASCII ID using letters, digits, `@.-_/:+~` |
| `COORD_AGENT_ALIASES` | (derived from agent ID) | Optional comma-separated mention aliases; exact identifier boundaries are enforced |
| `COORD_RESPONDER_ROSTER` | (empty) | Same comma-separated agent ID set on each daemon; enables deterministic team-wide election for unaddressed `QUERY:` messages |
| `COORD_CHANNELS` | (required) | Comma-separated coordination destinations (policy scope: room JIDs/names) |
| `COORD_MAX_MESSAGES` | `2` | Max messages per rate window per agent |
| `COORD_RATE_WINDOW_SECS` | `3600` | Rate limit window |
| `COORD_COALESCE_MS` | `2000` | First-responder claim TTL |
| `COORD_ENTROPY_THRESHOLD` | `4.8` | Shannon entropy threshold (bits/char) |
| `COORD_MIN_SECRET_LENGTH` | `32` | Min token length for entropy checks |
| `COORD_XMPP_STATUS_UPDATES` | `false` | Allow transient thinking/tool/reasoning status messages in configured XMPP coordination rooms |
| `COORD_KNOWLEDGE_SHARE` | `false` | Cross-tenant knowledge sharing: ingest `KNOW:` messages into the shared herd knowledge scope (needs DB-backed workspace) |
| `COORD_KNOWLEDGE_MAX_PER_HOUR` | `30` | Max ingested knowledge notes per sender per hour |
| `COORD_KNOWLEDGE_MIN_CHARS` | `12` | Minimum finding length (characters) to ingest |
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

## Operational boundaries

- Every participating daemon must use the same `COORD_RESPONDER_ROSTER`, and each
  entry must exactly match that daemon's `COORD_AGENT_ID` (comparison is
  case-insensitive). Invalid IDs disable election rather than creating divergent
  winners. Membership changes require updating/restarting the team.
- Deterministic election prevents duplicate cross-daemon responses without a
  shared service, but it is not a liveness oracle: if the elected daemon is
  offline, another daemon does not take over that already-delivered query.
  Explicitly address a live agent when failover is required.
- BeforeInbound rejection happens inside the supervised outer message task. It
  prevents session/thread resolution, model generation, and tool execution, but
  does not avoid creating that short-lived Tokio task.
