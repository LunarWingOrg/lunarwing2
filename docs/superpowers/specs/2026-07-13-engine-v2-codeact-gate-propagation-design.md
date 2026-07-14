# Engine V2 CodeAct Gate Propagation Design

**Date:** 2026-07-13
**Status:** Approved

## Problem

Engine V2 correctly pauses direct and structured tool calls when an effect
returns `EngineError::GatePaused`. Nested CodeAct tool calls do not preserve
that control flow.

The CodeAct executor spawns tool calls as asynchronous futures. When one of
those futures returns `EngineError::GatePaused`, `resolve_tool_future` records
an `ApprovalRequested` event but converts the gate into a Monty
`RuntimeError`. The model can then observe the exception, describe it as if an
approval were pending, and finish the thread normally.

The resulting thread is `Completed`, not `Waiting`. The bridge therefore never
creates a durable `PendingGate` and never emits the actionable
`ApprovalNeeded` status used by the web UI.

This was reproduced on Brightdawn with an HTTP POST. The request was prevented
from executing, but the gateway thread completed with the text
`execution paused by gate 'approval'` and no HTTP gate was persisted.

## Scope

Repair execute-time gate propagation for every tool invoked through nested
CodeAct execution. The behavior applies to all `ResumeKind` variants:

- approval;
- authentication; and
- external confirmation.

This change does not make additional tools require approval. It only preserves
gates already returned by the effect adapter.

## Existing Data Flow

1. CodeAct starts an asynchronous tool future.
2. The effect adapter applies parameter-sensitive permissions, persisted
   `AlwaysAllow` settings, authentication checks, and supervised mode.
3. A gated invocation returns `EngineError::GatePaused` with its complete
   structured payload.
4. `resolve_tool_future` currently turns that payload into a Monty
   `RuntimeError`.
5. The model continues and the thread completes, so the bridge has no
   actionable gate to persist or deliver.

The rest of the required pipeline already exists. `CodeExecutionResult` has a
`need_approval` field, `handle_execute_code_step` serializes it as
`pending_gate`, the Python orchestrator maps that to a `gate_paused` outcome,
and the bridge persists and delivers that outcome.

## Considered Approaches

### 1. Typed future resolution (selected)

Return a Rust enum from async tool resolution that distinguishes a normal
Monty result from a structured `ThreadOutcome::GatePaused`. The CodeAct loop
returns the gate through `CodeExecutionResult.need_approval` without resuming
Monty.

This reuses the existing gate pipeline, preserves all structured data, and
keeps the gate outside model-visible exception handling.

### 2. Special Monty exception

Encode the gate in a dedicated exception and recover it after Monty returns.
This still exposes control flow to user code and requires a second channel for
the structured gate payload. It is easier to catch accidentally and is not
selected.

### 3. Static policy preflight

Advertise every potentially gated tool as approval-required in its
`ActionDef`. This bypasses the failing execute-time path, but loses
parameter-sensitive behavior such as HTTP GET versus POST and bypasses
persisted permission checks owned by the effect adapter. It is not selected.

## Selected Design

Introduce a private result enum in `executor/scripting.rs`:

```rust
enum ToolFutureResolution {
    Monty(ExtFunctionResult),
    GatePaused(ThreadOutcome),
}
```

`resolve_tool_future` will return this enum.

For a successful action or an ordinary error, it returns
`ToolFutureResolution::Monty` with the current behavior. For
`EngineError::GatePaused`, it will:

1. preserve the complete gate payload in `ThreadOutcome::GatePaused`;
2. refund the lease only for a pre-execution gate with no `resume_output`;
3. retain the `ApprovalRequested` event and its parameters, gate name, and
   `allow_always` value; and
4. return `ToolFutureResolution::GatePaused` instead of a Monty exception.

During `ResolveFutures`, the CodeAct executor will resolve every future that
has already been spawned, preserving successful sibling results and matching
the structured executor's parallel-batch behavior. It records the first gate
in pending-call order as the actionable interruption. If a gate exists after
resolution, it returns `CodeExecutionResult` immediately with
`need_approval: Some(gate)` and does not call `resolve.resume`.

Not resuming Monty is the critical invariant: neither user code nor the model
can catch, rewrite, or narrate the gate.

## Safety And State Invariants

- The original action name, call ID, parameters, gate name, resume kind, and
  optional resume output must reach the bridge unchanged.
- Pre-execution gates refund their consumed lease use.
- Post-execution authentication gates carry `resume_output` and do not refund
  the lease.
- Ordinary tool failures remain Monty `RuntimeError` values.
- Successful sibling actions retain their results and events.
- Only the first gate in a parallel batch controls thread suspension, matching
  `executor/structured.rs`.
- A gated CodeAct thread transitions to `Waiting`, never `Completed`.
- The bridge remains the sole owner of durable pending-gate storage and
  channel delivery.

## Tests

Add focused engine regression coverage using an action definition that does
not request static preflight approval and a mock effect executor that returns
an execute-time gate.

The tests must prove:

1. an execute-time approval gate populates `need_approval` with the original
   structured payload;
2. the gate is not exposed in stdout as `execution paused by gate`;
3. an `ApprovalRequested` event is retained;
4. a pre-execution gate refunds its lease;
5. a post-execution gate preserves `resume_output` without refunding its lease;
6. ordinary effect errors remain script-visible errors; and
7. successful sibling results survive when another future gates.

The first regression test must fail against the current implementation before
production code changes are made.

## Live Acceptance

After automated verification and a Brightdawn release rebuild:

1. Send a gateway request that performs an approval-gated HTTP POST.
2. Confirm the thread becomes `Waiting` and exactly one pending HTTP gate is
   persisted.
3. Confirm the web UI renders one approval card with Approve and Deny controls.
4. Approve once and confirm the HTTP action executes once.
5. Confirm one result card and one terminal response are shown.
6. Refresh and switch threads, then confirm the resolved tool card persists.

## Non-Goals

- Changing which tools require approval.
- Moving dynamic permission checks out of the effect adapter.
- Redesigning the approval UI.
- Cleaning up pre-existing stale pending gates.
- Changing the global serialized dispatcher or its deferred queue.
