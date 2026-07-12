# Fire-and-forget sandbox completion race

> **Status: FIXED (current source verified 2026-07-12).** This is the archived
> completion-state portion of the former `BUG-subagent-worker-hang.md`. The
> stronger claim that a single worker exit made every channel unresponsive was
> never isolated and is now cross-referenced from
> [`BUG-agent-worker-lifecycle.md`](../BUG-agent-worker-lifecycle.md).

## Historical symptom and root cause

A sandbox worker could exit successfully while the main agent's in-memory job
remained `InProgress`. The monitor could miss a broadcast completion event, so
the stale job consumed a `ContextManager` slot until restart cleared memory.
The old report cited `src/...` paths; all current paths are under `ic/`.

The historical repro was: start a tenant with the sandbox worker image, submit
a fire-and-forget subagent job, let the container exit, then observe that job
status never leaves `InProgress`. A daemon restart cleared the stale slot. One
stale job did not by itself prove that the whole daemon or every channel was
blocked; that stronger symptom remains unverified.

## Current resolution

- `/worker/{job_id}/complete` now persists the result, broadcasts a `JobResult`,
  and transitions the in-memory context directly
  (`ic/src/orchestrator/api.rs:247-319`).
- Container-exit handling updates the context and emits a failure event when a
  worker exits without reporting completion
  (`ic/src/orchestrator/job_manager.rs:537-563`).
- Monitors subscribe before dispatch and handle completion, closed channels, and
  timeout (`ic/src/tools/builtin/job.rs:472-523`,
  `ic/src/agent/job_monitor.rs:83-204,206-282`).
- Regression tests cover these transitions
  (`ic/src/agent/job_monitor.rs:463-711`).

`complete_job` remains responsible for stopping/removing the container
(`job_manager.rs:629-684`); it is not the sole owner of context-state repair.

## Verification record

Source and in-tree regression-test inspection verified the resolution. No Cargo
or live container run was performed.
