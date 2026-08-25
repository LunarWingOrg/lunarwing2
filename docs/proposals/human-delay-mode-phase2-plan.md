# Human Delay Mode - Phase 2 Plan

> **Current status (source-reverified 2026-08-25): OPEN / PARTIALLY OVERTAKEN.**
> Phase 1 is usable and has focused adapter regressions: supervised mode gates
> `Never` tools, overrides auto-approve, and a resolved approval bypasses it once.
> `PendingGate` persistence and a background expiry sweep also landed, with a
> DB-backed global `settings.agent.pending_gate_expiry_secs` default of 1,800
> seconds. However, the per-thread `supervised_timeout_secs` field (default 300)
> is only propagated through `ThreadConfig`, `ThreadExecutionContext`, and
> `GateContext`; pending-gate creation still ignores it and uses the global
> expiry. The host `ApprovalGate` and engine `GatePipeline` exist but production
> approval remains inline in `ic/src/bridge/effect_adapter.rs`. Parameter
> modification and supervision levels have not landed.

**Status:** Phase 2 remains planned; Phase 1 `--supervised`/`AGENT_SUPERVISED_MODE`
is shipped.
**Goal:** Complete supervised mode with per-thread expiry semantics, one
production gate path, parameter modification, and current-path tests.

## Phase 2 Scope

Phase 1 introduced the data model and basic inline approval gating. Phase 2
removes the split-brain timeout/gate implementation and adds a safe modification
flow rather than inventing standalone CLI commands that do not exist.

## 1. Wire `supervised_timeout_secs` to Pending Gate Expiration

### Problem

`PendingGate` already has `expires_at`, persistence, fail-closed expiry, and a
background sweep. The mismatch is at creation: both pending-gate construction
sites in `ic/src/bridge/router.rs` use `EngineState::pending_gate_expiry`, loaded
from `settings.agent.pending_gate_expiry_secs` (default 1,800 seconds).
`ThreadExecutionContext.supervised_timeout_secs` reaches the effect adapter but
is not included in `EngineError::GatePaused`, so the router cannot choose the
thread's 300-second value.

### Implementation

Define the precedence explicitly:

- supervised approval pause → validated per-thread `supervised_timeout_secs`;
- ordinary approval/authentication pause → configured global pending-gate expiry;
- zero or invalid values → fail closed to a documented bounded default, never an
  unbounded gate.

Carry the selected expiry or timeout through the typed pause result rather than
re-reading mutable thread state after the effect returns. Use one helper for both
initial and resumed-action pending-gate construction.

### Files to modify

- `ic/crates/lunarwing_engine/src/types/error.rs` / resume metadata — carry the
  typed timeout selected by the execution context.
- `ic/src/bridge/effect_adapter.rs` — attach the supervised timeout when pausing.
- `ic/src/bridge/router.rs` — compute `PendingGate.expires_at` through one helper.
- `ic/src/gate/pending.rs`, `store.rs`, and persistence tests — verify expiry
  round-trips and sweep behavior.

## 2. Gate Pipeline Full Integration

### Problem

The reusable pipeline lives in `ic/crates/lunarwing_engine/src/gate/pipeline.rs`
and host `ApprovalGate` in `ic/src/gate/approval.rs`, but production does not
construct/evaluate that pipeline. `EffectBridgeAdapter::execute_action_internal`
still performs coordination, shared-room approval, supervision, normal approval,
authentication, rate-limit, and hook checks inline.

### Implementation

Make one production pre-execution pipeline authoritative. Do **not** retain a
second inline approval path as an indefinitely divergent fallback: either migrate
one check at a time behind parity tests or keep a single audited safety check with
clearly narrower purpose. Preserve ordering (authorization/coordination before
side effects, approval before execution, auth without double execution),
one-shot resumed approval, lease refunds, shared-room restrictions, and
parameter-sensitive `allow_always` behavior.

### Files to modify

- `ic/src/bridge/effect_adapter.rs` — production execution boundary.
- `ic/src/gate/approval.rs` and other host gates — parity with current inline
  semantics.
- `ic/crates/lunarwing_engine/src/gate/pipeline.rs` / `gate/mod.rs` — only if
  typed context or decisions need extension.
- focused adapter and router regressions for ordering, resume, auth, shared
  conversations, and supervised mode.

## 3. UI "Modify" Option for Supervised Threads

### Problem

Current HTTP and WebSocket approval surfaces accept only `approve`, `always`, or
`deny` and submit `Submission::ExecApproval`. There is no standalone
`lunarwing gate ...` CLI command. A modification feature must not turn approval
into a parameter-tampering bypass.

### Implementation

**Goal:** add a typed `modify` resolution to the existing authenticated web/WS
approval surfaces (and channel submission path if parity is required).

1. Load the pending gate by request ID plus authenticated user/thread/source
   constraints; reject missing, expired, already-consumed, or wrong-owner gates.
2. Accept a bounded JSON object of replacement parameters. Keep action name,
   call ID, lease/thread identity, and resume kind server-owned.
3. Re-run canonical parameter preparation/schema checks, sensitive-field
   redaction, policy/lease checks, rate limiting, hooks, authentication, and the
   tool's parameter-sensitive approval requirement. A change that becomes more
   dangerous must pause again or be denied; `modify` is never equivalent to an
   unqualified one-shot approval.
4. Atomically consume/replace the pending gate so concurrent approve/deny/modify
   requests cannot execute twice. Persist only redacted before/after parameters
   in history/logs.
5. Execute the validated modified call once, then resume the thread through the
   same result/event path as ordinary approval.

### Files to modify

- `ic/src/agent/submission.rs` — typed modified-approval payload.
- `ic/src/channels/web/handlers/chat.rs`, `server.rs`, `ws.rs`, static UI —
  authenticated request and editable JSON surface.
- `ic/src/bridge/router.rs` and `ic/src/gate/{pending,store,persistence}.rs` —
  ownership, expiry, atomic consume/replace, and resume.
- `ic/src/bridge/effect_adapter.rs` — re-evaluate modified parameters without a
  blanket `approval_already_granted` bypass.

A CLI command is separate scope: none exists today, so do not document
`lunarwing gate modify` unless a complete pending-gate CLI surface is added.

## 4. Supervised Mode Configuration Per-Thread

### Problem

`ThreadConfig` already persists a per-thread boolean and timeout, but the normal
operator surface enables the boolean globally for newly created foreground
threads through `--supervised` / `AGENT_SUPERVISED_MODE`. There is no typed
supervision level or per-thread management UI.

### Enhancement (optional, may be Phase 3)

Replace—not overload—the boolean with a serde-compatible level after defining
migration/default behavior:

```rust
pub enum SupervisionLevel {
    Disabled,
    Tools,       // all tool actions
    Commands,    // explicitly classified command-execution tools
    Always,      // every effect that can be paused
}
```

Define classification centrally; unknown/new actions must fail toward more
supervision, not silently bypass it. Preserve old `supervised_mode=true` as
`Always` during deserialization and pending-thread restore.

## 5. Testing

### Existing coverage (do not duplicate)

- `ic/src/bridge/effect_adapter.rs`: supervised mode gates a `Never` tool,
  overrides auto-approve, preserves disabled behavior, and lets one resolved
  approval execute once.
- `ic/src/gate/approval.rs`: gate-level supervised and normal approval policy.
- `ic/src/gate/pending.rs`, `store.rs`, and router tests: expiry/storage and
  pending-gate lifecycle foundations.
- CLI snapshots cover `--supervised`; web/WS tests cover approve/always/deny.

### New regressions required

- per-thread 300-second supervised expiry versus the independently configured
  ordinary 1,800-second expiry, including zero/overflow validation;
- restart persistence and background sweep auto-denial at the exact boundary;
- gate-pipeline/inline parity for every `ApprovalRequirement`, shared rooms,
  auto-approve, auth pause, and one-shot resume;
- modify happy path, invalid JSON/schema, sensitive redaction, increased-risk
  re-pause, expired/wrong-owner request, and concurrent resolution executes at
  most once;
- backward-compatible deserialization if supervision levels replace the bool.

### Current test locations

- `ic/src/bridge/effect_adapter.rs` and `router.rs`
- `ic/src/gate/{approval,pending,store,persistence}.rs`
- `ic/src/channels/web/{handlers/chat,ws}.rs` and web tests
- `ic/crates/lunarwing_engine/src/{gate,types,executor}/` unit tests

## Phase 2 Timeline (Estimated)

| Task | Effort | Dependencies |
|------|--------|-------------|
| 1. Timeout wiring | Medium | Typed pause metadata + both pending creation paths |
| 2. Gate pipeline integration | Medium-Large | Parity/ordering tests before removing inline checks |
| 3. Modify UI option | Medium-Large | Atomic resolution + full policy re-evaluation |
| 4. Supervision levels | Medium | Serde migration and centralized classification |
| 5. Tests | Medium-Large | Crosses engine, host gate, router, persistence, HTTP, and WS |

## Phase 3 (Future)

- Agent-to-agent supervised coordination (supervised thread with agent-to-agent tools)
- Supervision dashboard (UI to view all pending approvals across threads)
- Supervision audit logging (who approved what, when)
- Sweetiebot integration (multi-agent supervised workflow)

---

**Priority Order**: 1 → 2 → 3 → 5 (Task 4 optional)
**Blocker**: Cleared — Phase 1 compiles and its supervised-mode regressions pass.
