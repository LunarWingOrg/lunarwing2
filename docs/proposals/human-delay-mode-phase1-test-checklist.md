# Human Delay Mode - Phase 1 Test Checklist

> **Current status (source-reverified 2026-08-25): IMPLEMENTED.** Phase 1
> shipped and focused supervised-mode regressions pass. The active model lives in
> `ic/crates/lunarwing_engine/src/types/thread.rs` and the host execution/gate
> paths live in `ic/src/bridge/effect_adapter.rs` and `ic/src/gate/approval.rs`.
> Standalone `lunarwing thread ...` and `lunarwing gate ...` commands shown in
> the original manual checklist do not exist; approvals use channel submissions
> or the authenticated HTTP/WebSocket gateway flow. The global pending-gate
> expiry/sweep landed later, but per-thread `supervised_timeout_secs` is still
> ignored when `PendingGate.expires_at` is created.

**Status:** Implemented and verified; retained as the Phase 1 checklist and
historical manual-test record.

## Overview

Phase 1 introduces `supervised_mode` + `supervised_timeout_secs` fields to:
- `ThreadConfig`
- `ThreadExecutionContext` 
- `GateContext`

When `supervised_mode = true`:
- All tools require human approval (overrides auto-approve)
- Tool request pauses at gate with `GatePaused(PendingGate { ... })`
- User must approve via existing approval flow

## What Changed

### 1. Data model

- `ic/crates/lunarwing_engine/src/types/thread.rs` — `ThreadConfig` persists
  `supervised_mode` (default false) and `supervised_timeout_secs` (default 300).
- `ic/crates/lunarwing_engine/src/traits/effect.rs` —
  `ThreadExecutionContext` carries those fields into host effect execution.
- `ic/crates/lunarwing_engine/src/gate/mod.rs` — `GateContext` carries them for
  reusable gates.
- `ic/src/gate/pending.rs` — later work added durable `PendingGate.expires_at`;
  router creation still uses the independent global pending-gate expiry.

### 2. Active approval logic

- **Production inline boundary:** `ic/src/bridge/effect_adapter.rs` pauses every
  tool in supervised mode before normal parameter-sensitive approval handling.
  A resolved pending approval sets `approval_already_granted` for that call only;
  later calls are supervised again.
- **Reusable host gate:** `ic/src/gate/approval.rs` implements equivalent
  supervised behavior over the engine `ExecutionGate` trait, but the production
  effect adapter does not construct/evaluate `GatePipeline` yet.
- Shared conversations, authentication, rate limits, hooks, and leases retain
  their separate restrictions; supervision is not an authorization bypass.

### 3. Configuration and operator flow

- `--supervised` is a global CLI flag (`ic/src/cli/mod.rs`) and sets
  `AGENT_SUPERVISED_MODE=true` in `main.rs`.
- `ic/src/bridge/router.rs` applies that environment setting when creating
  Engine V2 foreground thread configuration.
- The browser receives `approval_needed` SSE and sends authenticated actions to
  `POST /api/chat/approval`; HTTP and WebSocket accept `approve`, conditional
  `always`, or `deny`. Channel submissions use the same typed execution-approval
  path. There is no standalone gate/thread management CLI.

## Verification

Run targeted source regressions from `ic/` under the current local limit:

```bash
taskset -c 0-11 cargo test -j12 --lib supervised_mode -- --nocapture --test-threads=12
taskset -c 0-11 cargo check -j12
```

The adapter tests prove:

- supervised mode gates a tool whose normal requirement is `Never`;
- supervised mode overrides auto-approve;
- disabled supervision preserves normal auto-approve behavior;
- one resolved approval bypasses supervision exactly once.

Manual gateway validation should start the daemon with `--supervised`, request a
normally non-gated tool in a direct foreground conversation, inspect the rendered
parameters, exercise approve and deny, and confirm the tool executes only after
approval. Shared rooms cannot resolve privileged approval prompts.

## Known Phase 2 Dependencies

1. **Per-thread expiration mismatch:** `supervised_timeout_secs` propagates but
   `PendingGate.expires_at` uses global `settings.agent.pending_gate_expiry_secs`
   (default 1,800 seconds), not the thread's 300-second default.
2. **Gate pipeline split:** host `ApprovalGate`/engine `GatePipeline` exist, but
   production approval remains inline in `EffectBridgeAdapter`.
3. **No parameter modification:** approval surfaces expose approve/always/deny
   only.
4. **No typed supervision levels or per-thread management UI:** the persisted
   setting is still boolean.

## Files to Review

- `ic/crates/lunarwing_engine/src/types/thread.rs`
- `ic/crates/lunarwing_engine/src/traits/effect.rs`
- `ic/crates/lunarwing_engine/src/gate/mod.rs`
- `ic/src/bridge/effect_adapter.rs`, `router.rs`
- `ic/src/gate/{approval,pending,store,persistence}.rs`
- `ic/src/channels/web/{handlers/chat,ws}.rs` and `static/app.js`

## Success Criteria

- ✅ Supervised mode pauses every tool action, including normal `Never` tools
- ✅ Supervised mode overrides auto-approve
- ✅ Explicit resolution executes the pending action once and later calls re-gate
- ✅ CLI flag/environment setting reaches new foreground Engine threads
- ⚠️ Per-thread timeout semantics remain Phase 2 work

---

**Next**: Phase 2 will wire `supervised_timeout_secs` to `PendingGate` expiration, add UI modify option, and improve test coverage.
