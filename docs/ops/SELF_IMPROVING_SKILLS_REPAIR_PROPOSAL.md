# Self-Improving Skills Runtime Repair

- **Date:** 2026-07-29
- **Release target:** v2.0.3.0
- **Goal:** Repair the runtime path described in [`SIS.md`](SIS.md) without
  weakening skill proposal approval, tenant isolation, or feature gates.

## Status

Implemented on branch `g-sis-m-2/2.0.3.0`.

This repair addresses the observed missing trigger payload, restores automatic
learning-mission delivery, and closes the capability-lease leak reported as 582
active leases with no active threads.

## Source-Verified Findings

### 1. Trigger payload was not runtime state

`MissionManager::fire_mission()` accepted a trigger payload, saved it as
`Mission.last_trigger_payload`, and rendered a truncated JSON copy in the mission
thread's user prompt. It did not initialize the Python orchestrator's persisted
`state` object.

The mission prompts correctly documented `state["trigger_payload"]`, but fresh
threads started with an empty `runtime_checkpoint.persisted_state`. Mission code
therefore saw a missing key even while the same payload appeared as prompt text
and in mission history.

### 2. Automatic learning listener could miss completion

`MissionManager::start_event_listener()` subscribed to `ThreadManager` terminal
events. Normal completion used direct `Thread::transition_to()` calls, which
recorded events on the thread but did not publish the final event on the manager's
broadcast channel. The listener therefore had no reliable automatic completion
signal.

Publishing before final persistence would create another race: the listener
loads the source thread from the Store and could observe its previous state.

### 3. Terminal capability leases stayed valid

Threads received non-expiring capability leases at spawn. Terminal processing
did not revoke them. Startup cleanup removed only leases already revoked or
expired, so valid leases belonging to completed threads accumulated indefinitely.

This explains the `SIS.md` observation of 582 active leases with zero active
threads. The leak affected runtime accounting and memory. Leases remained scoped
to dead thread IDs, so it was not a cross-thread authorization bypass, but stale
authority should still be closed.

### 4. `profile-builder` is not a current Engine V2 mission

No `profile-builder` implementation or seed exists in current source.
`conversation-insights` is the active successor for extracting user preferences,
domain knowledge, and workflow patterns every five successful threads. No second
profile mission should be added under this repair.

## Proposed Method

### A. Seed payload atomically before execution

Add optional initial persisted state to the thread spawn path. Mission fires pass:

```json
{
  "trigger_payload": { "...": "event-specific data" }
}
```

Thread creation stores that object under both
`metadata.initial_persisted_state` and
`metadata.runtime_checkpoint.persisted_state` before the thread is saved or
started. The initial copy restores required mission inputs after an
approval/authentication resume intentionally discards the interrupt checkpoint.
Benefits:

- `state["trigger_payload"]` is available on the first orchestrator and REPL step.
- No post-spawn metadata race exists.
- Gate pause and process-restart recovery preserve the payload.
- Normal terminal cleanup removes both runtime state keys; mission history keeps
  `last_trigger_payload` for diagnosis.
- Non-object initial state is rejected because Python mission state must be a
  mapping.

### B. Publish terminal events after persistence

At terminal processing:

1. Finalize `Completed` as `Done`, or transition execution/finalization errors
   to `Failed`.
2. Record skill feedback.
3. Revoke terminal leases.
4. Persist events and final thread state.
5. Broadcast the terminal `StateChanged` event.

Ordering guarantees that learning listeners loading the source thread observe
the final state and complete event history. Failed threads feed error diagnosis
only; skill extraction and conversation insights remain success-only. Mission
threads remain excluded to prevent recursive learning loops.

Conversation-insight counters are keyed by project and user. Threads belonging
to another user cannot consume or delay a user's five-thread cadence.

### C. Close current and historical lease lifetimes

For newly terminal threads, revoke every attached lease in both `LeaseManager`
and the Store, recording reason `owning thread reached terminal state`. Do not
revoke leases for waiting or suspended threads because they may resume.
Spawn and resume failures roll back leases granted during the failed attempt.
Per-thread resume serialization prevents duplicate approval/authentication
callbacks from launching the same waiting thread twice. Approval events are
broadcast only after the resumed thread state is persisted.

For historical data, startup cleanup finds valid leases whose owner thread is
`Done`, `Failed`, `Completed`, missing, or does not reference the lease ID,
persists revocation, then evicts those leases from the in-memory active set.
Lease files remain on disk as audit history.
Thread and lease writes used by this ordering propagate workspace persistence
errors rather than reporting success after only updating memory. Failed lease
reconciliation remains active in memory and retries on the next startup.

## Security and Compatibility

- `SKILL_SELF_IMPROVEMENT` default and behavior are unchanged.
- Skill patches and prunes remain propose-only until explicit user approval.
- Mission ownership, shared-owner visibility, webhook authentication, and
  capability planning are unchanged.
- Trigger data was already persisted in mission state and included in the model
  prompt. Runtime-state injection adds no new external data source.
- Raw source-thread transcripts are not copied into extraction payloads. Current
  bounded event summary avoids a new automatic disclosure path.
- No database schema migration is required; thread and mission metadata remain
  serde-compatible JSON.

## Verification

Focused regressions cover:

- A mission REPL reading the event value from `state["trigger_payload"]`.
- A terminal event being broadcast only after the stored thread is `Done`.
- Infrastructure failure after execution becoming `Failed`, emitting a terminal
  event, and releasing its leases.
- Gate-paused mission state retaining `trigger_payload` for resume.
- Conversation-insight cadence remaining isolated between users in one project.
- Spawn persistence failure rolling back newly granted leases.
- Concurrent resume attempts starting one execution, and failed resume
  persistence rolling back leases without broadcasting an uncommitted approval.
- Terminal in-memory leases becoming inactive.
- Startup cleanup revoking still-valid historical leases owned by terminal or
  missing threads and preserving terminal revocation after workspace reload.

Required commands:

```bash
taskset -c 0-5 cargo test -j6 -p lunarwing_engine --lib -- --test-threads=6
taskset -c 0-5 cargo test -j6 --lib bridge::store_adapter::tests:: -- --test-threads=6
taskset -c 0-5 cargo fmt --all -- --check
taskset -c 0-5 cargo check -j6
```

Results on 2026-07-29:

- All 420 `lunarwing_engine` library tests passed, including end-to-end terminal
  event delivery, gate-pause state retention, failure finalization, lease cleanup,
  spawn/resume rollback, concurrent-resume serialization, and multi-user cadence
  isolation.
- All eleven HybridStore adapter tests passed, including terminal, missing-owner,
  and unreferenced lease cleanup plus persistence across workspace reload.
- Workspace `cargo check`, `cargo fmt --check`, Engine library clippy, and root
  library clippy passed with warnings denied.
- Tenant `alpha` was inspected through `lunarwing-mt-admin.sh status`; all tenant
  services and PostgreSQL were inactive. Deployment was not attempted because
  the implementation did not yet have a committed ref consumable by the clean
  `upgrade-tenant` path.

Live validation should upgrade one test tenant through
`scripts/lunarwing-mt-admin.sh`, confirm service health, and verify startup state
no longer reports historical active leases for terminal threads.

## Residual Limits

- Daily mission budgets can intentionally suppress a trigger after quota is
  exhausted.
- Skill extraction receives bounded event facts (goal, action names, counts, and
  source thread ID), not the raw source transcript. Expanding that payload needs
  a separate redaction and privacy design.
- Skill quality, registry updates, and patch/prune policy are separate lifecycle
  concerns covered by existing self-improving-skills documents.
