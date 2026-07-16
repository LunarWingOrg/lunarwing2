# Engine V2 Skill Usage Feedback Design

**Date:** 2026-07-15
**Status:** Approved for implementation planning
**Scope:** Lifecycle-exactly-once feedback for activated Engine V2 skills

## Problem

The default Engine V2 Python orchestrator selects V2 skills, emits a
`SkillActivated` event, and stores their MemoryDoc UUIDs in
`state["active_skill_ids"]`. It never calls the existing
`__record_skill_usage__` host function.

Consequently, normal Engine V2 execution does not update skill metrics. B-1
patch thresholds, B-2 demotion, and B-2 pruning remain inert even though the
tracker and proposal mechanisms exist.

The runtime must record one observation for every activated skill when the
logical thread reaches an authoritative terminal outcome. A resumable gate
pause must not record or cause duplicate recording after resume.

## Goals

1. Record each activated skill once for a terminal Engine V2 lifecycle.
2. Use stable MemoryDoc UUIDs rather than skill names.
3. Centralize outcome classification in Rust at the authoritative thread
   lifecycle boundary.
4. Preserve activated skill IDs across `GatePaused` and resume.
5. Respect `SKILL_SELF_IMPROVEMENT`, which remains disabled by default.
6. Cover the real default orchestrator path with deterministic, in-process
   tests.
7. Treat orchestrator-emitted UUIDs as untrusted references: only skills visible
   to the executing thread's project/user scope may receive feedback.

## Non-Goals

- Transactional exactly-once behavior across process or host crashes.
- Changing B-1 patch, B-2 demotion/pruning, or B-3 registry semantics.
- Fixing the separate ungated `__propose_skill_prune__` entry point.
- Recording usage for the legacy agent runtime.
- Using live tenants, live LLM providers, or `/tmp` prototype scripts as
  regression coverage.

## Outcome Policy

| `ThreadOutcome` | Metric effect | Reason |
|---|---|---|
| `Completed` | Success | The logical thread produced its intended terminal result. |
| `Failed` | Failure | The runtime reached a terminal execution failure. |
| `MaxIterations` | Failure | The runtime exhausted its work budget without completion. |
| `Stopped` | No observation | User or system cancellation is not a skill-quality signal. |
| `GatePaused` | No observation | The thread is resumable and has not reached a terminal result. |

`Completed` counts as success whether or not its optional response contains
text. Completion is the authoritative signal.

## Approaches Considered

### Python terminal recording

The default orchestrator could call `__record_skill_usage__` before every
terminal return. This is rejected because orchestrator documents are
self-modifiable, the terminal logic has multiple return sites, and a modified
orchestrator could omit recording.

### `ExecutionLoop` checkpoint extraction

Rust could read `active_skill_ids` from the Python checkpoint after execution.
This is rejected because runtime checkpoint cleanup removes the data before a
later gate resume reaches its terminal outcome. It also couples Rust to the
internal shape of Python persisted state.

### `ThreadManager` terminal recording

The selected design copies stable skill UUIDs into namespaced thread metadata
when activation is emitted, then records usage in `ThreadManager` after the
resolved outcome is known. This keeps the policy in Rust, avoids checkpoint
shape coupling, and survives gate pause/resume.

## Architecture

### 1. Emit stable skill IDs

In `ic/crates/lunarwing_engine/orchestrator/default.py`, the step-zero skill
activation block continues to emit skill names for trace and mission behavior.
It additionally passes a comma-separated `skill_doc_ids` argument containing
the selected MemoryDoc UUIDs:

```python
__emit_event__(
    "skill_activated",
    skill_names=skill_names,
    skill_doc_ids=",".join(s.get("doc_id", "") for s in active_skills),
)
```

Empty IDs are ignored by the Rust handler.

### 2. Persist IDs outside the runtime checkpoint

In `handle_emit_event()` in
`ic/crates/lunarwing_engine/src/executor/orchestrator.rs`, the
`skill_activated` branch parses `skill_doc_ids` and stores valid, non-empty
strings in namespaced thread metadata.

The metadata key will be a module-owned constant rather than an unscoped string.
The intended serialized shape is an array of UUID strings:

```json
{
  "lunarwing.skill_feedback.active_doc_ids": ["uuid-1", "uuid-2"]
}
```

The existing `EventKind::SkillActivated { skill_names }` representation remains
unchanged for backward compatibility. Names are never resolved back to IDs.

`clear_runtime_checkpoint()` and `resume_thread()` remove only the runtime
checkpoint key, so the namespaced skill-feedback metadata survives a gate
pause and subsequent resume.

The terminal consumer processes at most 64 unique emitted IDs per pass. This
bounds amplification by a modified orchestrator without constraining the
default selector, which activates at most three skills.

### 3. Record at the authoritative terminal boundary

In `ic/crates/lunarwing_engine/src/runtime/manager.rs`, the spawned execution
task already converges every execution result into `ThreadOutcome`. The outcome
resolution is moved ahead of the final thread save so feedback and metadata
cleanup happen before that existing persistence point.

For `Completed`, `Failed`, and `MaxIterations`, it:

1. Reads the namespaced UUID array from thread metadata.
2. Parses each UUID as a `DocId`.
3. Builds the allowed ID set with
   `Store::list_memory_docs_with_shared(thread.project_id, thread.user_id)`.
4. Rejects IDs outside the thread's project/user/shared-owner visibility scope.
5. Calls `SkillTracker::record_usage(doc_id, success)` once per unique allowed
   ID.
6. Logs malformed, out-of-scope, and tracker failures without changing the
   thread result.
7. Removes the namespaced metadata after the recording pass.
8. Persists the updated thread metadata through the existing store path.

For `Stopped`, it does not record but clears the IDs because the lifecycle has
ended without a quality observation. For `GatePaused`, it neither records nor
clears the IDs. Preserving IDs on `GatePaused` allows resumed execution to
record at its eventual terminal outcome.

### 4. Lifecycle-exactly-once boundary

The guarantee is structural within normal runtime operation:

- Initial execution selects skills once at step zero.
- `GatePaused` records nothing.
- Resume creates another execution task for the same logical thread while the
  namespaced IDs remain in thread metadata.
- The first terminal outcome records once and removes the IDs.
- A stopped lifecycle removes the IDs without recording.
- A later accidental terminal-processing pass finds no IDs and cannot record
  again.

This is not a transactional crash guarantee. A process failure between metric
storage and metadata cleanup can lose or duplicate an observation after
recovery. Eliminating that window would require a durable idempotency receipt
and atomic persistence spanning thread and skill records, which is explicitly
outside this P0.

## Error Handling

- Missing metadata or an empty selected-skill set is a no-op.
- Malformed UUIDs produce a warning and are skipped.
- Scope lookup failures fail closed and record no observations.
- UUIDs outside the thread's project/user/shared-owner visibility are skipped.
- Failure to update one skill produces a warning and does not block attempts for
  the remaining skills.
- A terminal recording pass clears the IDs after all updates are attempted,
  even when one update fails; retry semantics are outside this P0.
- Feedback failures never convert a completed thread into a failed thread.
- The feature gate is checked before performing feedback work. The existing
  guard inside `SkillTracker::record_usage` remains defense-in-depth.
- Duplicate IDs are deduplicated before tracker calls so a malformed
  orchestrator emission cannot increment one skill twice.

## Testing Strategy

Tests use the real bundled `default.py`, deterministic mock LLM/effect
implementations, and `InMemoryStore`. They do not depend on network services or
natural-language failure induction.

Required cases:

1. **Completed:** one selected extracted skill receives exactly one usage and
   one success.
2. **Failed:** one selected extracted skill receives exactly one usage and one
   failure.
3. **MaxIterations:** one selected extracted skill receives one failure.
4. **Stopped:** metrics remain unchanged.
5. **GatePaused:** metrics remain unchanged before approval/resume.
6. **Gate resume:** pause followed by completion produces exactly one success
   total.
7. **Feature gate disabled:** a completed thread leaves metrics unchanged.
8. **No selected skills:** no metadata is emitted and no tracker call occurs.
9. **Duplicate emitted ID:** the skill receives one observation, not two.
10. **Metadata cleanup:** terminal recording removes the namespaced IDs.
11. **Wrong project/user:** an emitted UUID outside thread visibility does not
    mutate metrics.
12. **Shared owner:** a same-project shared skill remains eligible for feedback.
13. **Amplification bound:** at most 64 unique emitted IDs are processed.
14. **Direct host function:** `__record_skill_usage__` applies the same scoped
    visibility and feature-gate checks as terminal feedback.

Tests that mutate `SKILL_SELF_IMPROVEMENT` must use the repository's serialized,
scoped environment guard pattern and restore the original value.

## Verification

Run from `ic/`, respecting the six-thread Gentoo constraint:

```bash
taskset -c 0-5 cargo fmt --all -- --check
taskset -c 0-5 cargo test -j6 -p lunarwing_engine -- --test-threads=6
taskset -c 0-5 cargo check -j6 --all-features
```

No full debug build is required or permitted for verification.

## Expected Files

- `ic/crates/lunarwing_engine/orchestrator/default.py`
- `ic/crates/lunarwing_engine/src/executor/orchestrator.rs`
- `ic/crates/lunarwing_engine/src/runtime/manager.rs`
- Narrow engine test modules adjacent to the behavior they cover

The implementation must not modify B-2/B-3 proposal behavior, web handlers,
tenant scripts, or the legacy runtime.
