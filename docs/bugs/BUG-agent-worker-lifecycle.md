# Agent and worker lifecycle bugs

> **Overall status: PARTIALLY-FIXED (verified against current source on
> 2026-08-28).** The fire-and-forget completion race is fixed, one earlier
> notification-task report is invalidated, priority interrupts are handled
> during active turns, and distinct scoped Engine V2 gateway threads no longer
> head-of-line block each other. A synchronous `wait=true` worker call still
> occupies its originating thread. `wait=true` remains the default only for the
> built-in Docker sandbox; external workers have defaulted to `wait=false` since
> 2026-07-27.

This document consolidates `BUG-agent-loop-blocks-on-worker-jobs.md`,
`BUG-subagent-worker-hang.md`, and `BUG-unbounded-mpsc-recv-in-spawned-tasks.md`.
The original reports were written against pre-v2 paths and are retained here
with their distinct reproducers and current evidence.

## 1. Synchronous worker jobs (built-in and external)

**Status: STILL-OPEN (architectural limitation).**

### Symptoms and reproduction

1. Call the `create_job` tool with the built-in Docker sandbox (`mode: worker`)
   or an external worker (`opencode`, `kimi`, or `pebble`) and `wait=true`.
2. Use a task that takes several minutes, such as a repository analysis.
3. The originating turn does not return until the worker result is available.
   Exact `/interrupt` and `/stop` submissions are handled while it is active.
   A later message on that same thread remains deferred; a distinct scoped
   Engine V2 gateway thread for the same authenticated user may run in parallel.

The old report's claim that no parallel tool work is possible is too broad: the
dispatcher can run independent tools already emitted in one turn in a `JoinSet`
(`ic/src/agent/dispatcher.rs:783-846`). Its stronger claim that the merged
channel stream stops being polled is also stale. `Agent::run` continuously
selects from `message_stream` and a supervised task set; exact interrupts are
dispatched immediately (`ic/src/agent/agent_loop.rs:1137-1216`). The bounded
scheduler permits up to four distinct scoped Engine V2 gateway threads for one
`(channel, user)` group while serializing the same thread and all non-gateway or
control paths (`ic/src/agent/dispatch.rs:6-198`). Queue overflow receives an
explicit busy response. This removes daemon-wide ingestion stalls and the
cross-thread head-of-line blocking observed in v2.0.9.0 stress finding F3.

### Current implementation evidence

- `create_job` defaults `wait` to true only for the built-in Docker sandbox
  (schema text at `ic/src/tools/builtin/job.rs:1311`, parameter parsing at
  `:1531`). External workers default to `wait=false` (`job.rs:167-173`,
  applied at `:1489`; schema note at `:1333-1337`).
- The built-in sandbox path (`execute_sandbox`, `job.rs:557-871`) waits for
  the container/polling result when `wait` is true (`job.rs:705-870`); the
  external path (`execute_external`, `job.rs:872-1119`) awaits the worker
  result directly (`:994-1055`). The originating scheduling lane remains active
  until completion, timeout, or interruption.
- The default agent message timeout is 630 seconds
  (`ic/src/settings.rs:660-662,723-725`), so a long worker can consume most of
  its originating turn.
- The `wait=false` path dispatches immediately and starts a completion monitor
  (`ic/src/tools/builtin/job.rs:672-702` for the sandbox path, `:1086-1105`
  for external workers).
- Regression tests cover cross-thread progress, same-thread FIFO replay,
  active-turn interrupts, scoped interrupt rejection, bounded-queue overflow,
  soft-timeout detach, and shutdown cancellation
  (`ic/tests/engine_v2_interrupt_ingress.rs:156-459`).

### Impact

The user receives no result on the originating thread until completion unless
they interrupt it. The agent keeps ingesting messages; same-thread and exclusive
work remains serialized, while eligible independent gateway threads can use the
other bounded lanes. After 256 waiting messages, further messages receive a busy
response. This is now a thread-local interaction limitation, not a daemon-wide
throughput stall or evidence that the worker protocol itself is broken.

### Existing workaround and possible fix

Use `wait=false`, then inspect `job_events` or job status from a later turn. The
priority-interrupt path can cancel a matching active thread, and an independent
Engine V2 gateway thread can proceed concurrently. Same-thread progress still
requires an asynchronous job or a future steer mechanism that injects new input
at a safe execution boundary. The default remains `wait=true` for the built-in
Docker sandbox only; external workers default to `wait=false`.

## 2. Fire-and-forget worker completion race

**Status: FIXED (static verification; the original live outage symptom is not
reproduced in this documentation-only pass).**

The original report described a cleanly exiting sandbox container leaving an
`InProgress` job in memory, followed by a supposedly unresponsive daemon. The
stale in-memory job state was real; the stronger claim that one worker exit
stopped all XMPP/gateway responses was not established and was conflated with
the separate polling issue in `BUG-xmpp-polling-and-backpressure.md`.

### Original failure chain

The worker could report `/complete` after a monitor subscribed too late. The
completion event could be missed, leaving a job slot occupied in the in-memory
`ContextManager`. Restarting the daemon cleared that state.

### Current fix evidence

- `report_complete` persists the result, broadcasts `SseEvent::JobResult`, and
  directly transitions `ContextManager` out of `InProgress`
  (`ic/src/orchestrator/api.rs:236-366`).
- The container watcher also transitions the context when a container exits
  without a completion report and broadcasts a failure event
  (`ic/src/orchestrator/job_manager.rs:765-876`).
- Fire-and-forget monitors subscribe before dispatch, transition on completion
  or channel close, and have a 630-second deadline
  (`ic/src/tools/builtin/job.rs:672-690`,
  `ic/src/agent/job_monitor.rs:83-207,209-283`).
- Regression coverage exercises the context transition and timeout paths
  (`ic/src/agent/job_monitor.rs:469-712`).

`complete_job` still primarily owns container cleanup
(`ic/src/orchestrator/job_manager.rs:935-992`); the context transition is now
deliberately handled by the API/watcher paths above. The old reference to
`job_manager.rs:529` was therefore stale.

## 3. Notification receiver report

**Status: FIXED / INVALIDATED (not a current bug).**

The old report treated every `while let Some(response) = recv().await` as an
unbounded leak. In the current tree the channels are bounded and normal Tokio
`mpsc` semantics end the loop when all senders are dropped:

- Heartbeat uses `mpsc::channel(16)` at `ic/src/agent/agent_loop.rs:798`
  and the forwarder receive loop is at `:812`.
- Routine notifications use `mpsc::channel(32)` at
  `ic/src/agent/agent_loop.rs:953` and the forwarder receive loop is at
  `:980`.
- The owning heartbeat/routine tasks retain the sender while active
  (`ic/src/agent/heartbeat.rs:217`,
  `ic/src/agent/routine_engine.rs:115,146-158`) and the agent aborts their
  handles during shutdown (`ic/src/agent/agent_loop.rs:1216-1236`).
- The previously cited relay channel no longer exists in v2.

Waiting while a live producer remains active is expected notification-forwarder
behavior; there is no evidence of a task leak from a dropped sender. If a new
forwarder is added, it should still close its sender on shutdown and log a
closed-channel exit.

## Verification record

All statuses above were established by source inspection, in-tree regression
inspection, and local Git history; no Cargo command or full build was run. The
worker behavior was cross-checked against the current tool schema, active-turn
dispatcher, deferred queue, monitor, and API paths. The report's original live
outage was not re-run. Re-verified on 2026-08-13 (revision `7e813db4`): line
references refreshed after post-2026-08-03 churn in `agent_loop.rs`,
`job.rs`, `api.rs`, `job_manager.rs`, `heartbeat.rs`, and `routine_engine.rs`
(coordination hardening, MCP worker grants, and orchestrator auth work moved
the cited regions; no behavior described here changed). Re-verified on
2026-08-20 (revision `2d10832a`): no status changed; the only drift was the
630-second timeout default moving to `settings.rs:660-662,723-725`.
Re-verified on 2026-08-25 (revision `57e8a36f`): no status changed; line
references refreshed after the work-item #73 self-repair changes inserted
~28 lines into `agent_loop.rs` (repair-counter reset and terminal
`ManualRequired` surfacing) and ~13 lines into `dispatcher.rs` (skill
attenuation info logging). The cited regions moved to
`agent_loop.rs:1106-1213` (deferred FIFO loop), `:1144-1213` (active task),
`:798`/`:812` (heartbeat channel/forwarder), `:953`/`:980` (routine
channel/forwarder), `:1216-1236` (shutdown aborts), and
`dispatcher.rs:783-846` (tool `JoinSet`).
