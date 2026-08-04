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
- **Rate limiting:** 2 messages per hour per agent in coordination channels.
- **Engagement policy:** agents only respond to @mentions, direct replies, or
  DMs — not to every message in the channel.
- **First-responder correlation:** prevents duplicate responses when multiple
  agents could answer.
- **Three-strike enforcement:** repeated violations auto-disable an agent for
  24 hours.
- **Coalesce window:** 1.5–3 second pre-send buffer to merge rapid-fire
  messages from the same agent.

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
- **Gate system** (`ic/crates/lunarwing_engine/src/gate/`): ExecutionGate
  trait, GateDecision, GateContext, GatePipeline — confirmed. No `silent` field
  on `Deny` (Phase 3 proposes adding one or using a convention prefix).
- **Hook timeout default:** 5 seconds, not 30 as the plan states. Minor
  discrepancy; coordination hooks are sub-millisecond anyway.
- **TransformResponse** has no `channel` field — identified as a blocker for
  tag-schema enforcement on transform responses.

**Overall verdict:** Architecturally sound. Phases are feasible. Total
estimated ~1,480 LOC across all 4 phases.

---

## Release Scoping for v2.0.4.0

### What Ships in 2.0.4.0

| Phase | Scope | Rust changes | Est. LOC | Target milestone |
|-------|-------|-------------|----------|-----------------|
| Phase 1 | Declarative hook bundle (secret detection, newline enforcement, length caps) | None | ~50 JSON | 2.0.4.0 alpha |
| Phase 2 | Stateful Rust hooks (rate limiting, tag schema, engagement policy, first-responder, secret entropy) | Yes — new `ic/src/hooks/coordination.rs` | ~900 | 2.0.4.0 beta |
| Phase 3 | Three-strike gate + auto-disable + admin HTTP | Yes — new gate + ViolationStore | ~320 | 2.0.4.0 RC |
| Phase 4 | Coalesce window in channel adapter | Yes — new `ic/src/channels/coalesce.rs` | ~210 | 2.0.4.0 RC (stretch) |

**Phase 1 can ship immediately** — it requires zero Rust changes and uses the
existing `HookBundleConfig` system. The bundle JSON is already written and
ready for deployment.

**Phase 4 is stretch** — if the coalesce window proves too invasive for the
channel adapter layer, it can slip to 2.0.5.0 without blocking the other
phases.

### What Does NOT Ship in 2.0.4.0

- **Cross-host first-responder store** (Redis/NFS-based): per-host is
  sufficient for 2.0.4.0. Cross-host coordination deferred to 2.0.5.0+.
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

- [ ] Hook bundle loads without errors on daemon startup
- [ ] Secret patterns (API keys, PEM headers, JWTs) are silently rejected
- [ ] Inbound newlines are replaced with spaces
- [ ] Outbound messages with newlines are silently rejected
- [ ] Outbound messages >420 chars are silently rejected
- [ ] Normal coordination messages pass through unaffected

---

## Phase 2: Stateful Rust Hooks

### New Files

| File | Purpose | Est. LOC |
|------|---------|----------|
| `ic/src/hooks/coordination.rs` | CoordinationState, 5 hooks, config, helpers | ~500–600 |
| `ic/src/hooks/coordination_test.rs` | Unit tests for all hooks | ~300 |
| `ic/src/hooks/bundled.rs` (modified) | `register_coordination_hooks()` function | +40 |
| `ic/src/hooks/bootstrap.rs` (modified) | Call site when `COORD_ENABLED=true` | +10 |

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
   responses via correlation ID (content hash). Per-host store with TTL =
   coalesce window. Claims expire after 2 seconds by default.

5. **TagSchemaHook** (priority 20, BeforeOutbound + TransformResponse) —
   rejects messages in coordination channels not starting with an allowed tag.
   FailClosed.

6. **RateLimitHook** (priority 25, BeforeOutbound) — 2 messages/hour per agent
   per coordination channel. Sliding window. FailClosed.

### Configuration

Environment variables (loaded by `CoordinationHookConfig::from_env()`):

| Variable | Default | Description |
|----------|---------|-------------|
| `COORD_ENABLED` | `false` | Enable coordination hooks |
| `COORD_AGENT_ID` | (required) | This agent's unique identifier |
| `COORD_AGENT_ALIASES` | (required) | Comma-separated @mentions |
| `COORD_CHANNELS` | (required) | Comma-separated coordination channel JIDs |
| `COORD_MAX_MESSAGES` | `2` | Max messages per hour per agent |
| `COORD_COALESCE_MS` | `2000` | First-responder coalesce window (ms) |
| `COORD_ENTROPY_THRESHOLD` | `4.8` | Shannon entropy threshold |
| `COORD_MIN_SECRET_LENGTH` | `32` | Min token length for entropy check |
| `COORD_ADMIN_USERS` | (empty) | Comma-separated admin user IDs |

### Concerns to Resolve Before Phase 2

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

#### C2: TransformResponse Has No Channel Field

**Problem:** `HookEvent::ResponseTransform { user_id, thread_id, response }`
has no `channel` field. TagSchemaHook cannot know if a transform is destined
for a coordination channel.

**Resolution for 2.0.4.0:** Add an optional `channel: Option<String>` to the
`ResponseTransform` variant. This is an additive change — existing match arms
use `..` or named fields, so they won't break. The field is `None` for
non-channel-scoped transforms.

**Files affected:** `ic/src/hooks/hook.rs` (enum variant), all match sites on
`ResponseTransform` (grep for `ResponseTransform` in `ic/src/`).

#### C3: High-Entropy False Positives

**Problem:** Threshold 4.5 bits/char catches base64 data, git SHAs, file paths,
URLs with long query strings.

**Resolution for 2.0.4.0:** Start with threshold 4.8 (catches base64, allows
hex SHAs which are ~4.0 bits/char). Add an allowlist for known-safe patterns:
git commit SHAs (`[0-9a-f]{40}`), file paths (`/` containing), URLs (`http`).
Phase 1 regex-based detection remains the primary defense; entropy is
supplementary.

### Acceptance Criteria

- [ ] CoordinationState unit tests pass (rate limiting, first-responder claims, violation tracking)
- [ ] TagSchemaHook rejects untagged messages in coord channels, allows in casual channels
- [ ] RateLimitHook allows under limit, rejects over limit
- [ ] EngagementPolicyHook passes targeted messages, rejects untargeted
- [ ] FirstResponderHook: first caller wins, second caller rejected
- [ ] SecretEntropyHook detects high-entropy strings, allows normal text
- [ ] Hooks are no-ops on non-coordination channels
- [ ] `COORD_ENABLED=false` disables all coordination hooks (zero overhead)

---

## Phase 3: Three-Strike Enforcement Gate

### New Files

| File | Purpose | Est. LOC |
|------|---------|----------|
| `ic/src/gate/violation_store.rs` | ViolationStore — persistent per-agent violation tracking | ~150 |
| `ic/crates/lunarwing_engine/src/gate/coordination.rs` | CoordinationGate — ExecutionGate impl | ~80 |
| `ic/crates/lunarwing_engine/src/gate/mod.rs` (modified) | Add `pub mod coordination;` | +1 |
| `ic/src/gate/mod.rs` (modified) | Add `pub mod violation_store;` | +1 |
| `ic/src/channels/web/handlers/coord.rs` (new) | Admin HTTP endpoints (localhost-only) | ~50 |

### Components

1. **ViolationStore** — persistent per-agent violation tracking.
   - File: `$LUNARWING_BASE_DIR/coordination-violations.json` (atomic write:
     temp + rename, same pattern as `PendingGateStore`)
   - Single `Mutex`, same pattern as existing gate stores
   - `ViolationStatus`: `Active(0)` → `Warning(1)` → `FinalWarning(2)` →
     `Disabled(3+, 24h)`
   - `ViolationRecord`: timestamp, reason, hook, channel
   - Methods: `record_violation`, `get_status`, `clear_violations`,
     `manual_disable`, `manual_enable`, `expire_check`
   - Violation decay: 7 days (configurable via `COORD_VIOLATION_DECAY`)

2. **CoordinationGate** — `ExecutionGate` impl at priority 5 (above LeaseGate
   at 10). Denies all tool execution when agent is auto-disabled. Auto-clears
   when disable period expires.

3. **Admin HTTP endpoints** (localhost-only, bound to `127.0.0.1`):
   - `GET /coord/status` — all agents
   - `GET /coord/status/:agent` — single agent
   - `POST /coord/clear/:agent` — reset violations
   - `POST /coord/disable/:agent` — manual disable
   - `POST /coord/enable/:agent` — manual enable

4. **Phase 2 integration** — `CoordinationState` gains
   `Arc<ViolationStore>`. Hooks call `record_violation` on detection.
   - Strike 1: silent halt (hook already rejects)
   - Strike 2: ops alert via Gotify/DM (webhook hook or direct send)
   - Strike 3: auto-disable set by store, CoordinationGate blocks all activity

### Concern to Resolve Before Phase 3

#### C4: GateDecision::Deny Silent Field

**Problem:** Phase 3 wants silent denies for disabled agents (no channel
message, no error surfaced). Current `GateDecision::Deny { reason: String }`
has no `silent` field. Adding one touches every match arm.

**Resolution for 2.0.4.0:** Use a convention prefix `"[SILENT] "` in the reason
string. The gate pipeline and bridge already handle `Deny` by surfacing the
reason — we add a check: if reason starts with `"[SILENT] "`, suppress the
channel message and log at `debug` level only. This avoids an enum change that
would touch all match sites.

**Alternative (if clean enough):** Add `silent: bool` to `Deny` with a default
of `false` via a constructor method. Audit all match sites (grep
`GateDecision::Deny` in `ic/`). If the count is small (<20), this is cleaner.

### Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `COORD_VIOLATION_MAX` | `3` | Strikes before auto-disable |
| `COORD_VIOLATION_DISABLE_DURATION` | `86400` | Disable duration in seconds (24h) |
| `COORD_VIOLATION_DECAY` | `604800` | Violation decay in seconds (7 days) |
| `COORD_PERMISSIVE` | `false` | Disable gate for testing |

### Acceptance Criteria

- [ ] ViolationStore persists to disk and survives restarts
- [ ] Three-strike escalation: 1st silent halt, 2nd ops alert, 3rd auto-disable
- [ ] CoordinationGate denies all tool execution for disabled agents
- [ ] Auto-disable expires after configurable duration
- [ ] Admin HTTP endpoints work from localhost only
- [ ] `COORD_PERMISSIVE=true` disables the gate for testing

---

## Phase 4: Coalesce Window (Stretch)

### New Files

| File | Purpose | Est. LOC |
|------|---------|----------|
| `ic/src/channels/coalesce.rs` | CoalesceBuffer + merge logic + timer | ~150 |
| Channel adapter trait (modified) | Optional coalesce passthrough | ~20 |
| XMPP adapter (modified) | Integration | ~15 |
| WeeChat adapter (modified) | Integration | ~15 |

### Design

**CoalesceBuffer** lives in the channel adapter layer — AFTER BeforeOutbound
hooks, BEFORE `channel.send()`. It batches rapid-fire messages from the same
agent in the same channel.

- `pending: HashMap<ChannelKey, PendingMessage>`
- `ChannelKey` = `(agent_id, channel_id, conversation_id)`
- Window: 2.0 seconds (configurable via `COALESCE_WINDOW_SEC`)
- Max merges: 3 (flush immediately if hit)

### Merge Strategy

- Same tag prefix: concatenate with `" | "` separator
- Different tags: preserve both, keep first message's tag
- Merged content >420 chars: split into two messages, send sequentially
- Exact duplicate content within merge: stripped

### Edge Cases

- **ALERT messages:** bypass coalesce entirely (priority flush)
- **Thread termination:** flush all pending for that thread before exit
- **Engine shutdown:** flush all pending before graceful shutdown
- **Timer drift:** use `tokio::Instant`, not `Duration` arithmetic

### What Phase 4 Depends On

- Phase 1 (TagSchemaHook for post-merge validation)
- Phase 2 (only coalesce messages that passed engagement check)
- Does NOT depend on Phase 3 (coalesce runs before violation detection)

### Acceptance Criteria

- [ ] Rapid-fire messages from same agent are merged
- [ ] ALERT messages bypass coalesce
- [ ] Thread termination flushes pending messages
- [ ] Engine shutdown flushes pending messages
- [ ] Merged content passes through BeforeOutbound hooks again
- [ ] Non-coordination channels are unaffected (coalesce only on coord channels)

---

## Dependencies

| Dependency | Status | Notes |
|-----------|--------|-------|
| HookRegistry and Hook system | Ready | `ic/src/hooks/` — verified |
| GatePipeline and ExecutionGate | Ready | `ic/crates/lunarwing_engine/src/gate/` — verified |
| HookBundleConfig (workspace hooks) | Ready | `ic/src/hooks/bundled.rs`, `bootstrap.rs` — verified |
| XMPP channel adapter | Ready | Needed for Phase 4 coalesce |
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

## Implementation Order

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

4. **Phase 4 — Coalesce window (stretch)** — implement if time permits:
   1. `CoalesceBuffer` (buffer + merge + timer)
   2. Channel adapter trait extension
   3. XMPP adapter integration
   4. WeeChat adapter integration

---

## Open Questions

1. **Coordination channel identification:** How do we know which channels are
   coordination MUCs? Config list of JIDs (`COORD_CHANNELS`)? Convention-based
   naming (`coord@conference.*`)? **Current plan:** explicit config list.

2. **Admin commands transport:** The framework mentions localhost-only HTTP.
   Should this go through the existing web gateway router or a new dedicated
   endpoint? **Current plan:** dedicated localhost-only endpoints in a new
   handler module, not through the authenticated web gateway.

3. **Violation persistence:** Should violations survive agent restarts?
   **Current plan:** yes — persisted to `$LUNARWING_BASE_DIR/coordination-violations.json`
   via atomic write.

4. **Ops alert delivery (strike 2):** Gotify? DM? Webhook? **Current plan:**
   webhook hook (existing OutboundWebhookHook system) for maximum flexibility.
   Specific alert destination configured via existing webhook hook config.

5. **GateDecision silent approach:** Convention prefix `"[SILENT] "` or
   `silent: bool` field? **Current plan:** evaluate match site count; if <20,
   add the field. Otherwise use the prefix convention.

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
