# Agent and worker lifecycle bugs

> **Overall status: PARTIALLY-FIXED (verified against HEAD 2026-07-12).** The
> fire-and-forget completion race is fixed, and one earlier notification-task
> report is invalidated. A synchronous `wait=true` worker call still occupies
> the current agent turn.

This document consolidates `BUG-agent-loop-blocks-on-worker-jobs.md`,
`BUG-subagent-worker-hang.md`, and `BUG-unbounded-mpsc-recv-in-spawned-tasks.md`.
The original reports were written against pre-v2 paths and are retained here
with their distinct reproducers and current evidence.

## 1. Synchronous worker jobs (built-in and external)

**Status: STILL-OPEN (architectural limitation).**

### Symptoms and reproduction

1. Call the `create_job` tool with the built-in Docker sandbox (`mode: worker`)
   or an external worker (`opencode`, `nanocode`, or `pebble`) and `wait=true`.
2. Use a task that takes several minutes, such as a repository analysis.
3. The current turn does not return until the worker result is available. A
   later message cannot be handled by that agent while the awaited tool call is
   in progress.

The old report's claim that no parallel tool work is possible is too broad: the
dispatcher can run independent tools already emitted in one turn in a `JoinSet`
(`ic/src/agent/dispatcher.rs:620-725`). The daemon-wide message impact is real,
however: `Agent::run` owns the merged `ChannelManager` stream and does not call
`message_stream.next()` again while it awaits the current `handle_message`
task (`ic/src/agent/agent_loop.rs:401-430,889-947`). A long `wait=true` job can
therefore delay later messages from other users/channels as well as the
originating conversation.

### Current implementation evidence

- `create_job` defaults `wait` to true in the tool schema and parameter parsing
  (`ic/src/tools/builtin/job.rs:1084-1091,1200-1249`).
- The built-in sandbox path waits for the container/polling result when `wait`
  is true (`ic/src/tools/builtin/job.rs:412-704,1233-1249`); the external path
  awaits the worker result directly (`ic/src/tools/builtin/job.rs:810-858`).
  The agent run loop awaits the message handler (`ic/src/agent/agent_loop.rs:934-947`).
- The default agent message timeout is 400 seconds
  (`ic/src/settings.rs:659-660`), so a long worker can consume most of a turn.
- The `wait=false` path dispatches immediately and starts a completion monitor
  (`ic/src/tools/builtin/job.rs:773-909`).

### Impact

The user receives no result from that turn until completion, and the agent
cannot make progress on another incoming message during the await. This is a
throughput and interaction issue, not evidence that the worker protocol itself
is broken.

### Existing workaround and possible fix

Use `wait=false`, then inspect `job_events`/job status from a later turn. A
proper fix would add a per-session asynchronous job registry and a way to
surface completion/progress without holding the current turn open. Do not call
this fixed merely because the asynchronous path exists: the default remains
`wait=true`.

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
  (`ic/src/orchestrator/api.rs:247-319`).
- The container watcher also transitions the context when a container exits
  without a completion report and broadcasts a failure event
  (`ic/src/orchestrator/job_manager.rs:537-563`).
- Fire-and-forget monitors subscribe before dispatch, transition on completion
  or channel close, and have a 630-second deadline
  (`ic/src/tools/builtin/job.rs:472-523`,
  `ic/src/agent/job_monitor.rs:83-204,206-282`).
- Regression coverage exercises the context transition and timeout paths
  (`ic/src/agent/job_monitor.rs:463-711`).

`complete_job` still primarily owns container cleanup
(`ic/src/orchestrator/job_manager.rs:629-684`); the context transition is now
deliberately handled by the API/watcher paths above. The old reference to
`job_manager.rs:529` was therefore stale.

## 3. Notification receiver report

**Status: FIXED / INVALIDATED (not a current bug).**

The old report treated every `while let Some(response) = recv().await` as an
unbounded leak. In the current tree the channels are bounded and normal Tokio
`mpsc` semantics end the loop when all senders are dropped:

- Heartbeat uses `mpsc::channel(16)` and the forwarder is at
  `ic/src/agent/agent_loop.rs:609-651`.
- Routine notifications use `mpsc::channel(32)` and the forwarder is at
  `ic/src/agent/agent_loop.rs:724-816`.
- The owning heartbeat/routine tasks retain the sender while active and the
  agent aborts their handles during shutdown
  (`ic/src/agent/heartbeat.rs:490-509`,
  `ic/src/agent/routine_engine.rs:109-168,888-1022`,
  `ic/src/agent/agent_loop.rs:1150-1161`).
- The previously cited relay channel no longer exists in v2.

Waiting while a live producer remains active is expected notification-forwarder
behavior; there is no evidence of a task leak from a dropped sender. If a new
forwarder is added, it should still close its sender on shutdown and log a
closed-channel exit.

## Verification record

All statuses above were established by source inspection and local Git history;
no Cargo command or full build was run. The asynchronous worker behavior was
cross-checked against the current tool schema, dispatcher, monitor, and API
paths. The report's original live outage was not re-run.
