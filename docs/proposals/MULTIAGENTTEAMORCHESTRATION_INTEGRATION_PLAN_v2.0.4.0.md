# Multi-Agent Team Orchestration — Integration Plan for v2.0.4.0

**Created:** 2026-07-28
**Author:** Rarity
**Status:** Implemented for v2.0.4.0; retained as the release-scoped design record
**Source material:** `docs/proposals/MULTIAGENTTEAMORCHESTRATION/` (6 files)

All four phases shipped on 2026-07-30, with ISSUE-001…014 review fixes applied
on 2026-07-31. Use
[`MULTIAGENTTEAMORCHESTRATION/DEPLOYMENT.md`](MULTIAGENTTEAMORCHESTRATION/DEPLOYMENT.md)
for the current configuration and operational contract. Important final design
differences are recorded alongside the acceptance criteria below: tag validation
runs once at `transformResponse`, the gate is owned by `ic/src/gate/`, coalescing
is transport-independent, strike-two alerting is a structured warning log, and
first-responder claims remain process-local.

---

## Purpose

Item #33 from `AGENT_GOALS_2.0.3.0.md` asks: "write up proper plan document in
docs/proposals for integrating docs/proposals/MULTIAGENTTEAMORCHESTRATION into
the next release, 2.0.4.0."

The `MULTIAGENTTEAMORCHESTRATION` directory contains design work for a
hook-based coordination pipeline that enforces structured, machine-readable
multi-agent communication in group-chat environments (XMPP MUC, IRC). The
design was produced by Captain Ruffles and source-verified by Rarity against
v2.0.2.0. This document consolidates that work into a single release-scoped
integration plan for v2.0.4.0.

---

## Background

### What the Framework Does

The coordination pipeline enforces a "pager system / ops room" discipline on
designated coordination channels:

- **Message schema:** messages must start with a tag (`STATUS`, `ALERT`,
  `COORD`, `ANNOUNCE`, `QUERY`), be single-line, and ≤420 characters.
- **Secret detection:** inbound and outbound messages are scanned for API keys,
  private keys, JWTs, and high-entropy strings.
- **Rate limiting:** 2 messages per agent per configured window by default
  (`COORD_RATE_WINDOW_SECS=3600`).
- **Engagement policy:** within configured coordination scopes, messages pass
  when sent by the local agent/admin or when their text contains a configured
  `COORD_AGENT_ALIASES` value. Direct-reply metadata is not inspected; DMs are
  unaffected unless explicitly listed in `COORD_CHANNELS`.
- **First-responder correlation:** opportunistically suppresses duplicate claims
  among hooks sharing one daemon; separate tenant daemons can still both respond.
- **Three-strike enforcement:** repeated violations auto-disable an agent for
  24 hours.
- **Coalesce window:** a 2-second pre-send buffer by default
  (`COALESCE_WINDOW_SEC`) merges rapid-fire messages from the same agent.

### Existing Design Documents

| File | Contents |
|------|----------|
| `OriginalAgentFrameworkFromMidApril.md` | Original Kageho/Baud framework spec: mental model, pipeline, hooks, engagement policy, message schema, rate limits, three-strike, config reference |
| `coordination-pipeline-plan.md` | Captain Ruffles' 4-phase implementation plan mapping framework onto LunarWing hooks + gates, with pseudocode for all 6 components and 4 phases |
| `coordination-phase1-plan.md` | Phase 1 deployment plan: declarative hooks only, no Rust changes, includes verification steps |
| `coordination-phase1-hooks.md` | Hook system capabilities summary and Phase 1 scope/limitations |
| `coordination-phase1-bundle.json` | Ready-to-deploy HookBundleConfig JSON for Phase 1 |
| `rarity-source-verification.md` | Line-by-line source verification of all structural claims, 7 concerns identified, overall verdict |

### Source Verification Summary (rarity-source-verification.md)

Rarity verified every structural claim against v2.0.2.0 source. Key findings:

- **Hook system** (`ic/src/hooks/`): HookPoint, HookEvent, HookOutcome,
  HookFailureMode, Hook trait, HookRegistry, RuleHook execution flow, workspace
  hook loading — all confirmed accurate.
- **Gate system** (`ic/crates/lunarwing_engine/src/gate/`): `ExecutionGate`,
  `GateDecision`, `GateContext`, and `GatePipeline` remain the engine contract;
  the application-owned coordination gate lives in `ic/src/gate/coordination.rs`
  and uses the reviewed silent-reason convention.
- **Hook timeout default:** 5 seconds, not 30 as the original source assessment
  stated. Coordination hooks remain bounded and local.
- **ResponseTransform scope:** `HookEvent::ResponseTransform` now carries optional
  `channel` and canonical `policy_scope` fields for destination-aware policy.

**Overall verdict:** All four phases shipped. The original estimate was ~1,480
LOC; the hardened implementation is larger because it includes bounded
persistence, authenticated admin routes, delivery backpressure, shutdown
barriers, and review regression tests.

---

## Release Scoping for v2.0.4.0

### What Ships in 2.0.4.0

| Phase | Shipped scope | Primary implementation | Status |
|-------|---------------|------------------------|--------|
| Phase 1 | Declarative secret, newline, and length rules scoped to configured rooms | `ic/config/hooks/coordination.hook.json` | Shipped |
| Phase 2 | Stateful rate limit, tag schema, engagement, first-responder, and entropy hooks | `ic/src/hooks/coordination.rs` | Shipped |
| Phase 3 | Three-strike gate, bounded persistence, and authenticated localhost admin API | `ic/src/gate/`, `ic/src/channels/web/handlers/coord.rs` | Shipped |
| Phase 4 | Transport-independent coalesce window with bounded delivery and shutdown drain | `ic/src/channels/coalesce.rs`, `ic/src/agent/agent_loop.rs` | Shipped |

Phase 1 uses the existing `HookBundleConfig` system. Phase 4 moved to the agent
outbound boundary during review, avoiding per-adapter implementations and
covering native and installable channels uniformly.

### What Does NOT Ship in 2.0.4.0

- **Shared first-responder store** (host-local broker/file CAS or remote broker):
  current claims are process-local, so separate tenant daemons on the same host
  or different hosts can both respond. Shared coordination is deferred.
- **Router-level engagement gating** (Option A from verification doc): the
  router refactor to respect BeforeInbound rejections for thread spawning is
  invasive and deferred. 2.0.4.0 uses Option B (thread spawns but agent
  returns without responding).
- **Worker-local MCP coordination**: out of scope — this is host-local agent
  coordination only.

---

## Phase 1: Declarative Hooks (No Rust Changes)

### Deliverables

1. Ship `coordination-phase1-bundle.json` as a workspace hook bundle
2. Document deployment: place as `hooks/coordination.hook.json` in workspace,
   restart daemon, hooks auto-load via `register_workspace_bundles()`
3. Verification: 7 test cases from `coordination-phase1-plan.md` §Verification

### What Phase 1 Enforces

| Rule | Priority | Hook points | Failure mode |
|------|----------|-------------|-------------|
| API key/token detection (inbound) | 10 | beforeInbound | fail_closed |
| API key/token detection (outbound) | 10 | beforeOutbound, transformResponse | fail_closed |
| Private key material detection | 11 | beforeInbound, beforeOutbound, transformResponse | fail_closed |
| JWT token detection | 12 | beforeInbound, beforeOutbound, transformResponse | fail_closed |
| High-entropy base64 detection | 13 | beforeInbound, beforeOutbound, transformResponse | fail_closed |
| Newline strip (inbound) | 50 | beforeInbound | fail_open |
| Newline reject (outbound) | 50 | beforeOutbound, transformResponse | fail_closed |
| Max length 420 chars (outbound) | 51 | beforeOutbound, transformResponse | fail_closed |

### Limitations (Addressed in Phase 2)

- No tag schema enforcement (needs negative regex match — Rust `regex` crate
  doesn't support lookahead)
- No rate limiting (needs mutable state)
- No first-responder correlation (needs mutable state)
- No engagement policy (needs agent identity awareness)

### Acceptance Criteria

- [x] Hook bundle loads without errors on daemon startup
- [x] Secret patterns (API keys, PEM headers, JWTs) are silently rejected
- [x] Inbound newlines are replaced with spaces
- [x] Outbound messages with newlines are silently rejected
- [x] Outbound messages >420 chars are silently rejected
- [x] Normal coordination messages pass through unaffected

---

## Phase 2: Stateful Rust Hooks

### Implemented Files

| File | Purpose |
|------|---------|
| `ic/src/hooks/coordination.rs` | `CoordinationState`, five hooks, environment config, registration, and inline tests |
| `ic/src/hooks/hook.rs` | Destination-aware `ResponseTransform` fields and canonical policy scope |
| `ic/src/main.rs` | Shared violation-store construction and stateful hook registration |

### Components

1. **CoordinationState** — shared state store (`Arc<Mutex<...>>`) holding rate
   limit windows, first-responder claims, violation counts, disabled agents,
   coordination channel list.

2. **SecretEntropyHook** (priority 5, BeforeInbound + BeforeOutbound) — Shannon
   entropy detection beyond Phase 1 regex. Threshold: 4.8 bits/char (adjusted
   from 4.5 per Rarity's false-positive concern). Min token length: 32.

3. **EngagementPolicyHook** (priority 15, BeforeInbound) — rejects non-targeted
   messages in coordination channels. Uses agent aliases for @mention
   detection. FailOpen (non-coordination channels unaffected).

4. **FirstResponderHook** (priority 16, BeforeInbound) — prevents duplicate
   responses via a deterministic correlation ID. Claims are process-local and
   expire after the configured 2-second TTL by default; they do not coordinate
   separate tenant daemons.

5. **TagSchemaHook** (priority 20, TransformResponse only) — rejects messages
   in coordination channels not starting with an allowed tag. Single-point
   enforcement prevents one malformed response from consuming two strikes.
   FailClosed.

6. **RateLimitHook** (priority 25, BeforeOutbound) — 2 messages/hour per agent
   per coordination channel. Sliding window. FailClosed.

### Configuration

Environment variables (loaded by `CoordinationHookConfig::from_env()`):

| Variable | Default | Description |
|----------|---------|-------------|
| `COORD_ENABLED` | `false` | Enable coordination hooks |
| `COORD_AGENT_ID` | (required) | This agent's unique identifier |
| `COORD_AGENT_ALIASES` | (empty) | Comma-separated @mentions targeting this agent |
| `COORD_CHANNELS` | (required) | Comma-separated coordination policy scopes |
| `COORD_MAX_MESSAGES` | `2` | Max messages per rate window per agent |
| `COORD_RATE_WINDOW_SECS` | `3600` | Sliding rate-window duration |
| `COORD_COALESCE_MS` | `2000` | First-responder claim TTL (ms) |
| `COORD_ENTROPY_THRESHOLD` | `4.8` | Shannon entropy threshold |
| `COORD_MIN_SECRET_LENGTH` | `32` | Min token length for entropy check |
| `COORD_ADMIN_USERS` | (empty) | Comma-separated admin user IDs |

### Historical Concerns and Resolutions

#### C1: EngagementPolicyHook + Router Integration

**Problem:** `HookOutcome::Reject` on `BeforeInbound` blocks message processing
but does NOT prevent the router from spawning a thread. The agent gets woken up
and consumes tokens even though the hook rejected the message.

**Resolution for 2.0.4.0:** Option B — let the thread spawn but have the agent
return immediately without generating a response. The EngagementPolicyHook
reject prevents the message from reaching the conversation, so the agent's loop
exits with no output. Token waste is minimal (system prompt load only).

**Future (2.0.5.0+):** Option A — modify the router to respect BeforeInbound
rejections for thread spawning decisions.

#### C2: ResponseTransform Originally Had No Destination Scope

**Original problem:** `HookEvent::ResponseTransform { user_id, thread_id,
response }` could not identify whether a transform targeted a coordination
channel.

**Implemented resolution:** `ResponseTransform` now carries optional `channel`
and canonical `policy_scope` fields. Policy prefers `policy_scope`, falls back
to `channel`, and remains a no-op when neither destination is known. Match sites
were updated and regression-tested.

#### C3: High-Entropy False Positives

**Problem:** Threshold 4.5 bits/char catches base64 data, git SHAs, file paths,
URLs with long query strings.

**Resolution for 2.0.4.0:** Start with threshold 4.8 (catches base64, allows
hex SHAs which are ~4.0 bits/char). Add an allowlist for known-safe patterns:
git commit SHAs (`[0-9a-f]{40}`), file paths (`/` containing), URLs (`http`).
Phase 1 regex-based detection remains the primary defense; entropy is
supplementary.

### Acceptance Criteria

- [x] CoordinationState unit tests pass (rate limiting, first-responder claims, violation tracking)
- [x] TagSchemaHook rejects untagged messages in coord channels, allows in casual channels
- [x] RateLimitHook allows under limit, rejects over limit
- [x] EngagementPolicyHook passes targeted messages, rejects untargeted
- [x] FirstResponderHook: first caller wins, second caller rejected
- [x] SecretEntropyHook detects high-entropy strings, allows normal text
- [x] Hooks are no-ops on non-coordination channels
- [x] `COORD_ENABLED=false` disables all coordination hooks (zero overhead)

---

## Phase 3: Three-Strike Enforcement Gate

### Implemented Files

| File | Purpose |
|------|---------|
| `ic/src/gate/violation_store.rs` | Bounded, asynchronously persisted per-agent violation tracking |
| `ic/src/gate/coordination.rs` | Priority-5 `ExecutionGate` implementation |
| `ic/src/channels/web/handlers/coord.rs` | Authenticated localhost-only admin server and tests |
| `ic/src/main.rs` / `ic/src/agent/agent_loop.rs` / `ic/src/bridge/effect_adapter.rs` | Gate construction, dependency wiring, and pre-side-effect enforcement |

### Components

1. **ViolationStore** — bounded persistent per-agent violation tracking.
   - File: `$LUNARWING_BASE_DIR/coordination-violations.json`; a bounded,
     debounced writer persists immutable snapshots via atomic temp + rename.
   - In-memory state is protected by one mutex; the writer uses a capacity-one
     notification channel and retries failed persistence without blocking hooks.
   - `ViolationStatus`: `Active(0)` → `Warning(1)` → `FinalWarning(2)` →
     `Disabled(3+, 24h)`
   - `ViolationRecord`: timestamp, reason, hook, channel
   - Public operations: `record_violation`, `status`, `all_statuses`,
     `is_disabled`, `clear`, `manual_disable`, and `manual_enable`
   - Violation decay: 7 days (configurable via `COORD_VIOLATION_DECAY`)

2. **CoordinationGate** — `ExecutionGate` impl at priority 5 (above LeaseGate
   at 10). The production Engine V2 effect adapter checks it before tool side
   effects. Legacy tool-dispatch paths do not currently evaluate this gate.
   Expired automatic disables clear when status is evaluated.

3. **Admin HTTP endpoints** (localhost-only, bound to `127.0.0.1`):
   - `GET /coord/status` — all agents
   - `GET /coord/status/:agent` — single agent
   - `POST /coord/clear/:agent` — reset violations
   - `POST /coord/disable/:agent` — manual disable
   - `POST /coord/enable/:agent` — manual enable

4. **Phase 2 integration** — `CoordinationState` holds an optional shared
   `Arc<ViolationStore>`. Hooks call `record_violation` on detection.
   - Strike 1: silent halt (hook already rejects)
   - Strike 2: structured `tracing::warn!` ops alert for log-based alerting
   - Strike 3: auto-disable set by store; `CoordinationGate` blocks Engine V2 tool side effects

### Historical Concern and Resolution

#### C4: Silent Gate Denial

**Original problem:** Phase 3 needed disabled agents to stop without surfacing a
reason to the model or channel, while `GateDecision::Deny` carried only a reason
string.

**Implemented resolution:** `CoordinationGate` prefixes its reason with
`"[SILENT] "`. The Engine V2 effect adapter recognizes that marker before any
tool side effect and returns typed `ExecutionStopped` control flow. Executor
paths refund pre-execution lease use, emit no action failure, stop the thread,
and log only at debug level.

### Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `COORD_VIOLATION_MAX` | `3` | Strikes before auto-disable |
| `COORD_VIOLATION_DISABLE_DURATION` | `86400` | Disable duration in seconds (24h) |
| `COORD_VIOLATION_DECAY` | `604800` | Violation decay in seconds (7 days) |
| `COORD_PERMISSIVE` | `false` | Disable gate for testing |

### Acceptance Criteria

- [x] ViolationStore persists to disk and survives restarts
- [x] Three-strike escalation: 1st silent halt, 2nd ops alert, 3rd auto-disable
- [x] CoordinationGate denies Engine V2 tool execution for disabled agents before side effects
- [x] Auto-disable expires after configurable duration
- [x] Admin HTTP endpoints work from localhost only
- [x] `COORD_PERMISSIVE=true` disables the gate for testing

---

## Phase 4: Coalesce Window

### Implemented Files

| File | Purpose |
|------|---------|
| `ic/src/channels/coalesce.rs` | CoalesceBuffer, bounded delivery actor, merge logic, timer, shutdown barrier |
| `ic/src/agent/agent_loop.rs` | Transport-independent placement after outbound hooks and before channel dispatch |

### Design

**CoalesceBuffer** lives at the agent's outbound boundary — AFTER
BeforeOutbound hooks, BEFORE `channel.send()`. It therefore applies uniformly
to native and WASM channels rather than requiring adapter-specific wiring.

- `pending: HashMap<ChannelKey, PendingMessage>`
- `ChannelKey` = `(agent_id, transport_id, channel_id, conversation_id)`
- Window: 2.0 seconds (configurable via `COALESCE_WINDOW_SEC`)
- Max merges: 3 (flush immediately if hit)

### Merge Strategy

- Same tag prefix: concatenate with `" | "` separator
- Different tags: preserve content under the first message's tag
- Merged content >420 chars: flush the pending message, then buffer the new one
- Exact duplicate content within merge: stripped

### Edge Cases

- **ALERT messages:** bypass coalesce entirely (priority flush)
- **Conversation isolation:** the key prevents cross-thread or cross-transport merging
- **Agent shutdown:** `flush_all()` drains pending messages and waits for in-flight delivery
- **Timer drift:** uses `tokio::Instant`, not wall-clock arithmetic

### What Phase 4 Depends On

- Phase 1/2 outbound policy hooks run before buffering
- Does NOT depend on Phase 3 (coalescing occurs after violation-detecting hooks)

### Acceptance Criteria

- [x] Rapid-fire messages from same agent are merged
- [x] ALERT messages bypass coalesce
- [x] Buffer keys isolate conversations; final design uses one agent-owned buffer rather than a per-thread flush API
- [x] Agent shutdown flushes pending messages and waits for in-flight delivery
- [x] Each message passes hooks before buffering; merged delivery is not re-hooked, avoiding duplicate strikes and rate accounting
- [x] Non-coordination channels are unaffected (coalesce only on coord channels)

---

## Dependencies

| Dependency | Status | Notes |
|-----------|--------|-------|
| HookRegistry and Hook system | Shipped | `ic/src/hooks/` |
| GatePipeline and ExecutionGate | Shipped | Engine trait in `ic/crates/lunarwing_engine/src/gate/`; application gate in `ic/src/gate/` |
| HookBundleConfig (workspace hooks) | Shipped | `ic/src/hooks/bundled.rs`, `bootstrap.rs` |
| Agent outbound boundary | Shipped | `ic/src/agent/agent_loop.rs` applies hooks then transport-independent coalescing |
| Router thread-spawn integration | Deferred | Phase 2 uses Option B (non-invasive) |

---

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Engagement policy doesn't cleanly integrate with router | Medium | High | Option B for 2.0.4.0 (non-invasive), Option A for 2.0.5.0+ |
| TransformResponse channel field addition breaks match sites | Low | Low | Additive change; grep all `ResponseTransform` match sites |
| High-entropy false positives reject legitimate messages | Medium | Medium | Threshold 4.8 + allowlist for SHAs/paths/URLs; Phase 1 regex is primary |
| First-responder per-host store misses cross-VM agents | Low | Medium | Acceptable for 2.0.4.0; cross-host deferred to 2.0.5.0+ |
| Coalesce window delays critical ALERT messages | Low | Low | ALERT bypasses coalesce entirely |
| ViolationStore file corruption on crash | Low | Medium | Atomic write (temp + rename); same pattern as PendingGateStore |
| GateDecision silent-deny convention leaks reason to channel | Low | Medium | Audit all Deny surfacing paths; prefix check before channel send |

---

## Historical Implementation Order

All steps below are complete; the list records sequencing rather than remaining
work.

1. **Phase 1 deploy + verify** — ship the declarative bundle, run verification
   tests. No Rust changes. Can be done immediately.

2. **Phase 2 — CoordinationState + hooks** — implement in this order:
   1. `CoordinationState` (shared state store, all methods, with tests)
   2. `SecretEntropyHook` (standalone, no state dependency)
   3. `RateLimitHook` (depends on state rate limiting)
   4. `TagSchemaHook` (depends on state channel detection)
   5. `EngagementPolicyHook` (depends on state channel + admin detection)
   6. `FirstResponderHook` (depends on state claim system)
   7. `register_coordination_hooks()` (wire together)
   8. Bootstrap integration (call when `COORD_ENABLED=true`)
   9. Add `channel: Option<String>` to `ResponseTransform`

3. **Phase 3 — Gate + three-strike** — implement in this order:
   1. `ViolationStore` (persistent, atomic writes)
   2. `CoordinationGate` (ExecutionGate impl, priority 5)
   3. Silent-deny convention (`"[SILENT] "` prefix or `silent: bool` field)
   4. Phase 2 integration (hooks call `record_violation`)
   5. Admin HTTP endpoints (localhost-only)

4. **Phase 4 — Coalesce window:**
   1. `CoalesceBuffer` (bounded delivery actor, buffer, merge, timer, shutdown)
   2. Agent outbound-boundary integration after hooks
   3. Transport/destination/conversation-aware keys for native and WASM parity

---

## Resolved Decisions and Deferred Scope

1. **Channel identification:** explicit, case-insensitive exact matches from
   `COORD_CHANNELS`, evaluated against canonical `policy_scope` rather than the
   transport adapter name.
2. **Admin transport:** a dedicated bearer-authenticated server bound only to
   `127.0.0.1`, separate from the web gateway. It stays down without
   `COORD_ADMIN_TOKEN`.
3. **Violation persistence:** bounded records survive restarts in
   `$LUNARWING_BASE_DIR/coordination-violations.json`; writes use a debounced
   background persistence worker and atomic replacement.
4. **Strike-two alerting:** a structured `tracing::warn!` event. Operators must
   route daemon warning logs into their alerting system; no Gotify/DM is sent by
   this module.
5. **Silent gate denial:** `CoordinationGate` uses the `"[SILENT] "` convention,
   and callers translate it into typed execution-stop control flow without
   surfacing the reason to the model or channel.
6. **Deferred:** first-responder claims are process-local. A shared
   compare-and-insert backend is required to deduplicate across tenant daemons.

---

## Total Estimated Effort

| Phase | LOC | Complexity | Risk |
|-------|-----|------------|------|
| Phase 1 | ~50 JSON | Trivial | None |
| Phase 2 | ~900 Rust | Medium | Medium (router integration) |
| Phase 3 | ~320 Rust | Low-Medium | Low |
| Phase 4 | ~210 Rust | Medium | Low |
| **Total** | **~1,480** | | |

---

## References

- `docs/proposals/MULTIAGENTTEAMORCHESTRATION/OriginalAgentFrameworkFromMidApril.md` — original framework spec
- `docs/proposals/MULTIAGENTTEAMORCHESTRATION/coordination-pipeline-plan.md` — 4-phase implementation plan with pseudocode
- `docs/proposals/MULTIAGENTTEAMORCHESTRATION/coordination-phase1-plan.md` — Phase 1 deployment plan
- `docs/proposals/MULTIAGENTTEAMORCHESTRATION/coordination-phase1-hooks.md` — hook system capabilities
- `docs/proposals/MULTIAGENTTEAMORCHESTRATION/coordination-phase1-bundle.json` — ready-to-deploy hook bundle
- `docs/proposals/MULTIAGENTTEAMORCHESTRATION/rarity-source-verification.md` — source verification and concerns
- `ic/src/hooks/hook.rs` — Hook trait, HookEvent, HookOutcome, HookPoint
- `ic/src/hooks/registry.rs` — HookRegistry
- `ic/src/hooks/bundled.rs` — RuleHook, HookBundleConfig
- `ic/src/hooks/bootstrap.rs` — workspace hook loading
- `ic/crates/lunarwing_engine/src/gate/mod.rs` — ExecutionGate, GateDecision, GateContext
- `ic/crates/lunarwing_engine/src/gate/pipeline.rs` — GatePipeline
