# Multi-Agent Coordination Pipeline — Deployment & Configuration

**Status:** Implemented for v2.0.4.0 (all 4 phases).
**Plan:** `docs/proposals/MULTIAGENTTEAMORCHESTRATION_INTEGRATION_PLAN_v2.0.4.0.md`

The coordination pipeline enforces "pager system / ops room" discipline on
designated coordination channels (XMPP MUC, IRC): tagged single-line messages,
secret detection, per-agent rate limits, engagement policy, first-responder
correlation, three-strike auto-disable, and an outbound coalesce window.

Everything is **off by default**. Nothing activates unless `COORD_ENABLED=true`
plus the required identity/channel variables are set.

---

## Phase 1 — Declarative hook bundle (no Rust needed)

Ships as `ic/config/hooks/coordination.hook.json`.

Deploy: copy it into the agent workspace as `hooks/coordination.hook.json` and
restart the daemon. `register_workspace_bundles()` auto-loads
`hooks/*.hook.json` at startup.

Enforces: API-key/PEM/JWT/high-entropy secret rejection (inbound + outbound),
inbound newline stripping, outbound single-line and 420-char caps.

## Phase 2 — Stateful Rust hooks

`ic/src/hooks/coordination.rs`, registered from `main.rs` when
`COORD_ENABLED=true`:

| Hook | Priority | Point | Mode |
|------|----------|-------|------|
| `coordination.secret_entropy` | 5 | beforeInbound, beforeOutbound | fail-closed |
| `coordination.engagement_policy` | 15 | beforeInbound | fail-open |
| `coordination.first_responder` | 16 | beforeInbound | fail-open |
| `coordination.tag_schema` | 20 | beforeOutbound, transformResponse | fail-closed |
| `coordination.rate_limit` | 25 | beforeOutbound | fail-closed |

Notes:

- Engagement policy uses Option B from the plan: the thread may spawn but the
  agent exits without responding when the message isn't targeted at it.
- `TagSchemaHook` uses the new `channel` field on `ResponseTransform` events;
  transforms without a channel are not tag-enforced.
- Entropy detection is supplementary to the Phase 1 regexes. Allowlisted:
  40-char hex git SHAs, file paths, URLs.

## Phase 3 — Three-strike enforcement + admin endpoints

- `ic/src/gate/violation_store.rs` — persistent violations at
  `$LUNARWING_BASE_DIR/coordination-violations.json` (atomic temp+rename).
  Escalation: strike 1 silent halt, strike 2 ops alert (`tracing::warn!` —
  wire your log alerting to it), strike 3 auto-disable (24h).
- `ic/src/gate/coordination.rs` — `CoordinationGate` (`ExecutionGate`,
  priority 5). Denies tool execution for disabled agents with the
  `"[SILENT] "` reason convention (surfacing paths should suppress channel
  messages for silent denies). **Note:** like the existing
  approval/authentication/hook gates in `ic/src/gate/approval.rs`, the gate
  pipeline is assembled by the Engine V2 bridge work; the gate ships ready to
  wire. `COORD_PERMISSIVE=true` disables it for testing.
- Admin endpoints (`ic/src/channels/web/handlers/coord.rs`): dedicated axum
  server bound to `127.0.0.1` only — **not** routed through the authenticated
  web gateway.

  | Endpoint | Action |
  |----------|--------|
  | `GET /coord/status` | all tracked agents |
  | `GET /coord/status/{agent}` | single agent |
  | `POST /coord/clear/{agent}` | reset violations |
  | `POST /coord/disable/{agent}` | manual disable |
  | `POST /coord/enable/{agent}` | manual enable |

## Phase 4 — Coalesce window

`ic/src/channels/coalesce.rs`, wired into the XMPP adapter's `respond()`
(after BeforeOutbound hooks, before the hourly rate limiter + outbound queue).

- 2s window (default), max 3 merges; merged same-tag messages join with
  `" | "`, first tag wins on mixed tags, exact duplicates stripped.
- `ALERT:` messages bypass coalescing entirely.
- Overflow beyond 420 chars flushes the pending message first, then buffers
  the new one (sequential delivery).
- `shutdown()` flushes all pending messages.
- Coalesced flushes pass through the existing XMPP hourly rate limiter
  (`OutboundRateLimiter`, see also `scripts/xmpp-rate-limit.sh` for the
  bridge-level control) — coalescing reduces volume, it doesn't bypass limits.

## Configuration reference

| Variable | Default | Description |
|----------|---------|-------------|
| `COORD_ENABLED` | `false` | Master switch for coordination hooks + admin server |
| `COORD_AGENT_ID` | (required) | This agent's unique identifier |
| `COORD_AGENT_ALIASES` | (required for engagement) | Comma-separated @mentions targeting this agent |
| `COORD_CHANNELS` | (required) | Comma-separated coordination channel JIDs/names |
| `COORD_MAX_MESSAGES` | `2` | Max messages per rate window per agent |
| `COORD_RATE_WINDOW_SECS` | `3600` | Rate limit window |
| `COORD_COALESCE_MS` | `2000` | First-responder claim TTL |
| `COORD_ENTROPY_THRESHOLD` | `4.8` | Shannon entropy threshold (bits/char) |
| `COORD_MIN_SECRET_LENGTH` | `32` | Min token length for entropy checks |
| `COORD_ADMIN_USERS` | (empty) | Comma-separated admin user IDs |
| `COORD_VIOLATION_MAX` | `3` | Strikes before auto-disable |
| `COORD_VIOLATION_DISABLE_DURATION` | `86400` | Auto-disable seconds (24h) |
| `COORD_VIOLATION_DECAY` | `604800` | Violation decay seconds (7d) |
| `COORD_PERMISSIVE` | `false` | Disable CoordinationGate (testing) |
| `COORD_ADMIN_PORT` | `18777` | Admin server port (always 127.0.0.1) |
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

- Cross-host first-responder store (per-host is current behavior).
- Router-level engagement gating (Option A: suppress thread spawning on
  BeforeInbound rejections).
- Gate pipeline assembly for the app-level gates (Engine V2 bridge work).
- WASM-channel coalesce integration (terminal responses only; skipped).
