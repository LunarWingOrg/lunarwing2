# Self-Improving Skills (SIS) — Fix Proposal

- **Date:** 2026-07-29
- **Release target:** v2.0.3.0
- **Goal item:** 41
- **Status:** Implemented. Grafted from two independent attempts,
  `k-sis-m-2/2.0.3.0` and `g-sis-m-2/2.0.3.0`; see
  [Provenance](#provenance-and-graft-decisions).
- **Source notes:** [`SIS.md`](SIS.md) — field observations from the
  2026-07-28/29 mission test runs.
- **Scope:** Engine V2 (`ic/crates/lunarwing_engine/`) + bridge store adapter
  (`ic/src/bridge/store_adapter.rs`). No schema migration; thread and mission
  metadata remain serde-compatible JSON.

## TL;DR

The self-improvement machinery is architecturally sound, but four concrete breaks
made it inert in production. Each was verified by reading source, not inferred
from the field notes:

1. **Terminal thread events are never broadcast**, so event-driven learning
   missions never fire on their triggers — they only fire on manual/API calls.
2. **`state["trigger_payload"]` is never injected** into mission thread state.
   Every mission prompt tells the agent to read it; it did not exist.
3. **Capability leases leak.** Every thread mints permanent leases that are never
   revoked, reload at every startup, and count as "active" forever (observed: 582
   active leases with 0 active threads).
4. **Missions cannot persist their output.** The prompts instruct missions to save
   Skill / fix-pattern / orchestrator docs via `memory_write(title=…,
   doc_type=…, metadata=…)`, but the v1 `memory_write` tool accepts only
   `content/target/append/layer/force`. The extra arguments were silently
   dropped, so extracted skills survived only as prose and never reached
   `__list_skills__`.

Cause 4 is the gating one. Fixing 1–3 gets a mission thread its payload and lets
it do the analysis — and then its output is discarded. Any fix that stops short
of 4 leaves item 41 unsolved.

`profile-builder` (from the SIS.md notes) **never existed in code** — zero matches
in tree and git history. `conversation-insights` is the profile-analog mission
(goal: "Extract user preferences, domain knowledge, and workflow patterns"). No
code change needed; any `profile-builder` mission in the field was created
out-of-band via `mission_create` and can simply be deleted.

---

## Fix 1 — Broadcast terminal `StateChanged`, but only after persistence

**Root cause of "missions fire blind."**

The intended chain was already correct end to end except for one missing link:

1. A thread completes; the finisher task transitions `Completed → Done`.
2. `MissionManager::start_event_listener` (`runtime/mission.rs:871`) subscribes to
   `thread_manager.subscribe_events()` and waits for a terminal `StateChanged`,
   then builds the payload and calls `fire_on_system_event(...)`.
3. `fire_mission_inner` embeds the payload in the meta-prompt and persists it as
   `mission.last_trigger_payload`.

**The break:** `Thread::transition_to` only *appends* the event to
`thread.events`. An exhaustive audit of `.send(` call sites shows only
waiting/approval, approval-received, response-delta, code-step/action, and
rollback events reach the broadcast channel — never `StateChanged`. The
listener's filter therefore never matched: no learning mission had ever fired
from an engine event in production, and `last_trigger_payload` stayed `null`,
exactly the SIS.md symptom. Existing tests call `fire_on_system_event` directly,
which is why this went unnoticed.

**The fix, and the ordering subtlety that matters:** the finisher now broadcasts
the terminal event — but only *after* `save_thread` succeeds, and only if it
succeeded. Broadcasting earlier reintroduces the same bug in a subtler form: the
listener responds by calling `store.load_thread(event.thread_id)`, and skill
extraction requires `thread.state == ThreadState::Done`
(`runtime/mission.rs:1002`). Between an early broadcast and the eventual save
there are several store round-trips (lease revocations, skill feedback, event
append), so a listener that wins that race loads a pre-`Done` thread and
**silently skips extraction**. Persist first, then publish.

The terminal event is located by scanning `thread.events` in reverse for
`StateChanged { to: Done | Failed }`, so both outcomes reach subscribers and
SSE/status consumers stay consistent. `Failed` threads feed error diagnosis only;
extraction and conversation insights remain success-only, and Mission threads
stay excluded to prevent recursive learning loops.

Two lifecycle gaps are closed alongside it:

- `ExecutionLoop` can return an infrastructure error *before* it makes the thread
  terminal. The finisher now transitions those to `Failed`, so leases and
  learning events cannot be stranded on a dead background task.
- The loop engine broadcasts `Failed` on the orchestrator-error path via a
  `transition_to_broadcast` helper that validates the transition.

## Fix 2 — Seed `trigger_payload` into mission thread state

**The break:** `fire_mission`'s doc comment (`runtime/mission.rs:460-461`) and all
five `prompts/mission_*.md` files promise the payload as
`state["trigger_payload"]`. Nothing injected it. `fire_mission_inner` spawned with
no initial state, and the orchestrator's `state` comes from the runtime
checkpoint, which is empty for a fresh thread. The repl `state` dict is otherwise
built solely from prior-step results (`executor/scripting.rs:208-212`). The
payload's only real delivery channel was a prompt-text block truncated at 1000
chars — mid-JSON, so large error-diagnosis payloads arrived corrupt.

**The fix:** an `initial_state: Option<serde_json::Value>` threads through the
spawn chain (`spawn_thread_with_history{,_and_message}`) and is written into
thread metadata before the thread is saved or started, so there is no post-spawn
metadata race. `fire_mission_inner` passes `json!({"trigger_payload": payload})`.
Non-object initial state is rejected, because Python mission state must be a
mapping.

The payload is written to **two** metadata keys, which is what makes it survive
interruption:

- `runtime_checkpoint.persisted_state` — what
  `ExecutionLoop::load_runtime_checkpoint` reads on the first step.
- `initial_persisted_state` — a durable copy, merged in on load without
  overwriting live checkpoint values.

The second key exists because of a pre-existing bug this fix also closes:
`GatePaused` returns through the `Ok(orch_result)` arm of the run loop, which
called `clear_runtime_checkpoint()` unconditionally — discarding the state the
orchestrator had just written via `__save_checkpoint__`. A mission thread that
paused for approval therefore lost its payload on resume. The clear is now
skipped on `GatePaused`, and both keys are removed together once the thread is
genuinely terminal. Mission history retains `last_trigger_payload` for
diagnosis either way.

Meta-prompt truncation is raised 1000 → 4000 chars so large issue payloads
survive in the prompt copy as well.

## Fix 3 — Lease lifecycle: revoke on terminal, roll back on failure, sweep history

**The break:** `ThreadManager` granted leases with `(None, None)` — no TTL, no use
cap — at spawn and resume, and persisted them. Nothing ever released them:

- `LeaseManager::revoke` and `Store::revoke_lease` had **no production callers**.
- The finisher never touched `thread.capability_leases`, despite recording exactly
  which leases belonged to the thread.
- `LeaseManager::expire_stale` was never called in production.
- Startup `cleanup_terminal_state` dropped only leases already
  `revoked || !is_valid()` — i.e. none, ever — and lease files were never
  reconciled, so they reloaded at every startup.

Hence "582 active leases, 0 active threads": every thread and every mission run
minted permanent leases that accumulated monotonically. The leases stayed scoped
to dead thread IDs, so this was not a cross-thread authorization bypass, but
stale authority should still be closed.

**The fix, in three parts:**

1. **Revoke on terminal.** The finisher revokes every attached lease in both
   `LeaseManager` and the Store with reason `owning thread reached terminal
   state`, then calls `expire_stale()`. All exit paths funnel through this task,
   so coverage is uniform. Leases for `Waiting`/`Suspended` threads are left
   alone — they may resume, and `refresh_resumed_thread_leases` re-grants on
   resume regardless.
2. **Roll back on failure.** Spawn and resume failures revoke any leases granted
   during the failed attempt, so a thread that never started leaves no authority
   behind. Per-thread resume serialization prevents duplicate
   approval/authentication callbacks from launching the same waiting thread
   twice, and approval events are broadcast only after the resumed state is
   persisted. Thread and lease writes propagate workspace persistence errors
   rather than reporting success after a memory-only update.
3. **Sweep history.** Startup cleanup finds still-valid leases whose owning thread
   is terminal, missing, or does not reference the lease ID, persists the
   revocation, then evicts them from the in-memory active set. Lease **files stay
   on disk as audit history** — because the revocation is durable, they reload as
   revoked and are filtered out rather than counted active again. Failed
   reconciliation stays in memory and retries on the next startup.

No TTL is added to lease grants: revoke-on-terminal plus the sweep closes the
observed leak with less behavior change. TTLs remain an option if long-lived
`Waiting` threads ever need hard expiry.

> **Invariant.** The sweep treats a lease whose thread is absent from the
> in-memory map as dead. That is sound only because the sole caller
> (`ic/src/bridge/router.rs`) runs it immediately after
> `load_state_from_workspace()` has eagerly loaded every thread. Do not move that
> call after lazy eviction, or live `Waiting`/`Suspended` leases will be swept.
> A comment at the function pins this.

## Fix 4 — Give missions a real memory-doc write path

**This is the fix without which nothing else matters.**

**The break:** the extraction, self-improvement, insights, and expected-behavior
prompts all told missions to save output with `memory_write(target="memory",
title=…, doc_type=…, metadata=…)`. The v1 `memory_write` tool
(`ic/src/tools/builtin/memory.rs:387-452`) accepts only
`content/target/append/layer/force`; the extra parameters were silently ignored,
so skill output landed as prose in MEMORY.md. No other action or host function
created `DocType::Skill` docs either — the orchestrator exposed
`__list_skills__`, `__propose_skill_patch__`, `__propose_skill_prune__`, and
`__record_skill_usage__`, all of which read or propose, none of which construct.
The only Skill-doc constructor in the tree is the v1→v2 migration, which
hardcodes `V2SkillSource::Migrated`.

That resolves a contradiction inside SIS.md itself: run #1 reports extracting
`skill:external-worker-dispatch`, while the later audit reports zero docs with
`source: "extracted"` and 225 all `migrated`. Both are true — the skill was
written as prose and never became a skill.

**The fix:** a host function available inside mission code blocks:

```python
doc_id = __save_memory_doc__(title, content, doc_type, tags, metadata, shared=False)
```

It constructs a `MemoryDoc` with the given type/tags/metadata, sets
`source_thread_id` to the calling thread, and returns the new doc id string (or
`False` on refusal/error). Arguments resolve by keyword first, then position, so
both call styles work — the prompts use `shared=True`, and a positional-only
handler would silently drop it and write shared docs user-scoped where the
engine's shared-space loaders could never see them.

`shared=True` writes under the shared owner, which is **required** for the three
doc kinds the engine reads via `list_shared_memory_docs`: the prompt overlay
(`executor/prompt.rs:172`), orchestrator versions (`executor/orchestrator.rs:148`,
`:241`, `:292`), and the fix-pattern database (`runtime/mission.rs:1154`). Skill
and insight docs stay user-scoped by default so they remain attributable to the
mission's user; `__list_skills__` reads `list_memory_docs_with_shared`, so they
are still found.

**Gated to `ThreadType::Mission`.** The name is not registered for other thread
types and the handler re-checks, so ordinary chat threads cannot mint
`trust: "trusted"` skill docs. This is a trust-escalation guard, not a
convenience.

All five `prompts/mission_*.md` files now call this function, and the extraction
prompt explicitly warns against `memory_write` for skill docs. The consumption
side needed no changes: `__list_skills__` has no source filter, the scorer already
applies the extracted-confidence factor, and `SkillTracker` already feeds the
demote/prune lifecycle — extracted skills become usable the moment they can be
created.

## Also fixed: conversation-insights cadence was cross-tenant

The insights counter was keyed by `project_id` alone, so in a shared project one
user's completed threads consumed or delayed another user's five-thread cadence.
It is now keyed `project_id:user_id`.

## Security and compatibility

- `SKILL_SELF_IMPROVEMENT` and `ORCHESTRATOR_SELF_MODIFY` defaults and behavior
  are unchanged. This work makes the machinery functional when enabled; it does
  not enable it. Note that the SIS.md observation "self-improvement ran as a
  healthy no-op" is partly this policy gate
  (`runtime/mission.rs:1846-1859`), not only breakage.
- Skill patches and prunes remain propose-only until explicit user approval.
- Mission ownership, shared-owner visibility, webhook authentication, and
  capability planning are unchanged.
- `__save_memory_doc__` is mission-gated; it is documented only in the mission
  prompts and exposed through no public API.
- Trigger data was already persisted in mission state and included in the model
  prompt, so runtime-state injection adds no new external data source.
- Raw source-thread transcripts are not copied into extraction payloads — the
  payload stays a bounded event summary (goal, action names, counts, source
  thread ID), avoiding a new automatic disclosure path.
- No database schema migration is required.

## Verification

Unit and integration coverage added or extended:

- A mission REPL reading the event value from `state["trigger_payload"]`.
- `fire_mission` with a payload seeds both metadata keys.
- Terminal event broadcast happens only after the stored thread is `Done`.
- Infrastructure failure after execution becomes `Failed`, emits a terminal
  event, and releases its leases.
- Gate-paused mission state retains `trigger_payload` for resume.
- Conversation-insight cadence stays isolated between users in one project.
- Finisher revokes the thread's leases; terminal in-memory leases become
  inactive.
- Spawn persistence failure rolls back newly granted leases; concurrent resume
  attempts start one execution; failed resume persistence rolls back leases
  without broadcasting an uncommitted approval.
- Startup cleanup revokes still-valid historical leases owned by terminal,
  missing, or unreferencing threads, retains their files, and preserves terminal
  revocation across a workspace reload.
- `__save_memory_doc__` creates a Skill doc for Mission threads, refuses
  non-Mission threads, honors `shared=True` for shared-space docs, and honors
  keyword arguments for every parameter.

Commands (this machine allows 18 threads):

```bash
taskset -c 0-17 cargo fmt --all -- --check
taskset -c 0-17 cargo check -j18
taskset -c 0-17 cargo test -j18 -p lunarwing_engine --lib -- --test-threads=18
taskset -c 0-17 cargo test -j18 --lib bridge::store_adapter::tests:: -- --test-threads=18
taskset -c 0-17 cargo clippy -j18 --all --benches --tests --examples -- -D warnings
```

**Live validation still owed.** The check that actually proves item 41 is
end-to-end: complete a 5+ step thread, then confirm both that `skill-extraction`
fires with a non-null `last_trigger_payload` **and** that a `DocType::Skill` doc
with `source: "extracted"` appears. Only the second half distinguishes a working
pipeline from the pre-existing prose-only behavior. Also worth confirming that
startup no longer reports historical active leases for terminal threads across a
restart.

## Provenance and graft decisions

Two branches attempted item 41 independently. Both correctly diagnosed causes
1–3; only `k-sis` found cause 4. The merged result takes:

| From `k-sis-m-2/2.0.3.0` | From `g-sis-m-2/2.0.3.0` |
|---|---|
| `__save_memory_doc__` host function + all five prompt rewrites (cause 4) | Persist-before-broadcast ordering |
| `trigger_payload` seeding through the spawn chain | Gate-pause checkpoint retention + `initial_persisted_state` |
| Meta-prompt truncation 1000 → 4000 | Terminal `Failed` transition for pre-terminal infra errors |
| Lease revoke-on-terminal + startup orphan sweep | Lease rollback on spawn/resume failure; resume serialization |
| Live-thread predicate in the sweep | Per-user insights cadence; lease files retained as audit history |

Fixed during the graft, present in neither branch: `__save_memory_doc__` ignored
`call.kwargs`, so the `shared=True` its own prompts pass was always dropped —
silently writing prompt-overlay, orchestrator-version, and fix-pattern docs
user-scoped, where the engine never reads them. Arguments now resolve by keyword
then position, with a regression test.

Superseded: `SELF_IMPROVING_SKILLS_REPAIR_PROPOSAL.md` (the `g-sis` write-up).
Its ordering, rollback, and tenant-isolation reasoning is folded into Fixes 1–3
above.

## Residual limits

- Daily mission budgets can intentionally suppress a trigger once quota is
  exhausted.
- Skill extraction receives bounded event facts, not the raw source transcript.
  Expanding that payload needs a separate redaction and privacy design.
- Event delivery still depends on the Engine V2 `ThreadManager` broadcast
  (capacity 256, lag only debug-logged). Work performed by the v1 harness, main
  assistant sessions, or external workers emits no such events and therefore
  produces no learning triggers. Widening the event sources is out of scope here.
- Skill quality, registry updates, and patch/prune policy remain separate
  lifecycle concerns covered by the existing self-improving-skills documents.
