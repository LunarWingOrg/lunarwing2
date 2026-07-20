# Engine tests and Rust integration harness failures

> **Status: FIXED (static verification against `51ae5a8` on 2026-07-20;
> historical test-run counts were not re-run).** This consolidates the former
> `BUG-engine-crate-test-failures.md` and
> `BUG-rust-integration-test-harness-failures.md`.

## Engine crate: five stale assertions

The v1.1.0 report described five test failures after the execution-loop
refactor. They were test expectation bugs, not production failures.

The shared root cause for the first three was that Monty/Python orchestration
keeps action calls, results, and nudges in `Thread::internal_messages`, while
the user-visible `messages` collection contains the system prompt and final
response. The trace failure assumed the old event ordering, and the mission
failure used the legacy shared owner id even though shared missions intentionally
skip the engine-level per-user ownership guard.

1. `executor::loop_engine::tests::action_then_text` expected three visible
   messages. The current test checks visible messages (system + final) and a
   non-empty internal transcript
   (`ic/crates/lunarwing_engine/src/executor/loop_engine.rs:909-936`).
2. `codeact_multi_step` now searches `internal_messages` for `x = 30`
   (`loop_engine.rs:1172-1197`).
3. `tool_intent_nudge_injected` now searches the internal transcript for the
   nudge (`loop_engine.rs:1032-1059`).
4. `trace_serializes_approval_request_payload` finds the event by
   `EventKind::ApprovalRequested` instead of assuming it is event zero
   (`ic/crates/lunarwing_engine/src/executor/trace.rs:818-875`).
5. `system_mission_requires_system_user_to_manage` creates a non-shared
   `admin-user` mission and asserts that `alice` is denied
   (`ic/crates/lunarwing_engine/src/runtime/mission.rs:2737-2770`).

The old line references and the historical "271 passed" count are retained
only as provenance. Current source confirms all five corrected expectations;
the documentation pass did not run Cargo.

## Integration harness: `Agent::run` ownership change

`Agent::run` now requires `self: Arc<Self>`
(`ic/src/agent/agent_loop.rs:461-462`). Both surviving harness call sites wrap
the agent correctly:

- `ic/tests/support/gateway_workflow_harness.rs:292-294`
- `ic/tests/support/test_rig.rs:946-951`

The old report also claimed a third fix at historical line 231 of
`ic/tests/e2e_telegram_message_routing.rs`. That file is absent from the current
tree and reachable history, so the historical claim cannot be independently
verified. A repository-wide search found no remaining plain `agent.run()` site.
Treat the missing third test as retired/obsolete rather than as evidence of a
current compile failure.

## Verification record

Fixed status is supported by current assertions, ownership signatures, and a
search for stale call sites. No Cargo/build command was run, as required for
this documentation task.
