# Self-Improving Skills (SIS) — Fix Proposal

Status: implemented on branch `k-sis-m-2/2.0.3.0` (2026-07-29). See per-fix status notes.
Source notes: `docs/ops/SIS.md` (field observations from the 2026-07-28/29 mission test runs).
Scope: Engine V2 (`ic/crates/lunarwing_engine/`) + bridge store adapter (`ic/src/bridge/store_adapter.rs`).

## TL;DR

The self-improvement machinery is architecturally sound, but four concrete breaks
make it inert in production:

1. **Terminal thread events are never broadcast**, so event-driven learning
   missions never fire on their triggers (they only fire on manual/API calls).
2. **`state["trigger_payload"]` is never injected** into mission thread state —
   every mission prompt tells the agent to read it; it does not exist.
3. **Capability leases leak**: every thread mints permanent leases that are never
   revoked on completion, reloaded at every startup, and counted as "active"
   forever (observed: 582 active leases with 0 active threads).
4. **Missions cannot persist their output**: the prompts instruct missions to
   save Skill/fix-pattern/orchestrator docs via `memory_write(title=…, doc_type=…,
   metadata=…)`, but the v1 `memory_write` tool accepts only
   `content/target/append/layer/force` — so extracted skills survive only as
   prose in ordinary memory and never reach `__list_skills__`.

`profile-builder` (from the SIS.md notes) **never existed in code** — zero matches
in tree and git history. `conversation-insights` is the profile-analog mission
(goal: "Extract user preferences, domain knowledge, and workflow patterns"). No
code change needed; any `profile-builder` mission in the field was created
out-of-band via `mission_create` and can simply be deleted.

---

## Fix 1 — Broadcast terminal `StateChanged` events (root cause of "missions fire blind")

**Intended chain**

1. Thread completes; the finisher task transitions `Completed → Done`
   (`ic/crates/lunarwing_engine/src/runtime/manager.rs:461-468`).
2. `MissionManager::start_event_listener` (`runtime/mission.rs:871`) subscribes to
   `thread_manager.subscribe_events()` and waits for `StateChanged { to: Done }`,
   then builds the payload and calls `fire_on_system_event(...)` with it
   (`mission.rs:977-984, 1035-1042, 1086-1093`).
3. `fire_mission_inner` embeds the payload in the meta-prompt and persists it as
   `mission.last_trigger_payload` (`mission.rs:544, 563`). This part is correct.

**Break**: `Thread::transition_to` only *appends* the event to `thread.events`
(`types/thread.rs:291-320`); nothing ever calls `event_tx.send(...)` for state
changes. An exhaustive audit of `.send(` call sites shows only waiting/approval,
response-delta, code-step/action, and rollback events are broadcast — never
`StateChanged`. So the listener's filter never matches, no learning mission ever
fires from an event in production, and `last_trigger_payload` stays `null`
(exactly the SIS.md symptom). All existing tests call `fire_on_system_event`
directly, which is why this went unnoticed.

**Fix**: in the finisher task (`runtime/manager.rs:449-509`), after the run
completes, find the terminal `StateChanged` event (`to: Done` or `to: Failed`,
the latter from `executor/loop_engine.rs:390`) in `exec.thread.events` and
broadcast it on a cloned `event_tx`. One emission unblocks the whole downstream
chain, which is already correct. Broadcasting `Failed` too keeps SSE/status
subscribers consistent.

## Fix 2 — Seed `trigger_payload` into mission thread state

**Break**: `fire_mission`'s doc comment (`mission.rs:460-461`) and all five
`prompts/mission_*.md` files promise the payload as `state["trigger_payload"]`.
Nothing injects it: `fire_mission_inner` spawns with no initial state
(`mission.rs:566-578`), and the orchestrator's `state` comes from the runtime
checkpoint (`executor/loop_engine.rs:158-168`), which is empty for a fresh
thread. Result: `KeyError` in REPL blocks — the SIS.md "repl blocks were not
executing / trigger_payload null in thread state" symptom. (REPL execution
itself works fine for mission threads; the seed is what's missing.)

**Fix**: thread an `initial_state: Option<serde_json::Value>` through the spawn
chain (`spawn_thread_with_history_and_message`), written into
`thread.metadata["runtime_checkpoint"]["persisted_state"]` before the thread is
saved/started — exactly where `ExecutionLoop::load_runtime_checkpoint` reads it.
`fire_mission_inner` passes `json!({"trigger_payload": payload})`. Also raise the
meta-prompt payload truncation from 1000 → 4000 chars (`mission.rs:1672`) so
large issue payloads survive.

## Fix 3 — Lease lifecycle: revoke on terminal transition + orphan sweep

**Break**: `ThreadManager` grants leases with `None, None` (no TTL, no use cap)
at spawn (`manager.rs:245-256`) and resume (`manager.rs:392-402`), and persists
them (`store.save_lease`). Nothing ever releases them:

- `LeaseManager::revoke` / `Store::revoke_lease` have **no production callers**.
- The finisher never touches `thread.capability_leases` — even though it records
  exactly which leases belong to the thread.
- `LeaseManager::expire_stale` is never called in production.
- Startup `cleanup_terminal_state` (`ic/src/bridge/store_adapter.rs:237-244`)
  only drops leases that are `revoked || !is_valid()` — i.e. none, ever — and
  lease files are never deleted from disk, so they reload at every startup.

Hence "582 active leases, 0 active threads": every thread and every mission run
mints permanent leases that accumulate monotonically.

**Fix** (two parts):

1. **Revoke on terminal**: in the finisher, iterate
   `exec.thread.capability_leases` and call `leases.revoke(id, "thread terminal")`
   + `store.revoke_lease(...)`. All exit paths (Done, Failed, abort) funnel
   through this task, so coverage is uniform.
2. **Orphan sweep**: extend `cleanup_terminal_state` to also drop leases whose
   `thread_id` belongs to a terminal/missing thread, and actually unlink the
   lease files so restarts don't reload them.

This intentionally does **not** add TTLs to lease grants — revoke-on-terminal +
orphan sweep closes the leak with less behavior change. TTLs remain an option if
long-lived `Waiting` threads ever need hard expiry.

## Fix 4 — Give missions a real memory-doc write path (+ prompt updates)

**Break**: extraction/self-improvement prompts tell missions to save output with
`memory_write(target="memory", title=…, doc_type=…, metadata=…)`. The v1
`memory_write` tool (`ic/src/tools/builtin/memory.rs:387-452`) accepts only
`content/target/append/layer/force`; the extra params are silently ignored, so
skill output lands as prose in MEMORY.md. No other action or host function
creates `DocType::Skill` docs either — every non-test use of
`V2SkillSource::Extracted` is a predicate check, never a constructor. That's why
all 225+ skills are `source: "migrated"` and none `source: "extracted"`. (The
*consumption* side is fully wired — `__list_skills__` has no source filter, the
scorer applies the extracted-confidence factor, and `SkillTracker` feeds the
demote/prune lifecycle — so extracted skills would be used the moment they can
be created.)

**Fix**: add a host function `__save_memory_doc__(title, content, doc_type,
tags, metadata)` alongside `__list_skills__` in
`executor/orchestrator.rs`, backed by `store.save_memory_doc`. It constructs a
`MemoryDoc` with the given type/tags/metadata and `source_thread_id` set to the
calling thread, returning the new doc id. **Gated to `ThreadType::Mission`** —
normal chat threads must not be able to mint `trust: "trusted"` skill docs
(trust-escalation guard). Update the five `prompts/mission_*.md` files to call
`__save_memory_doc__` instead of the fictitious `memory_write` signature. This
one path serves skill docs, fix-pattern DB updates, and orchestrator-version
docs alike.

## Verification plan

- Unit tests:
  - terminal `StateChanged` is broadcast on `subscribe_events()` after a thread
    completes (manager);
  - `fire_mission` with a payload seeds
    `metadata["runtime_checkpoint"]["persisted_state"]["trigger_payload"]`
    (extends the existing `fire_with_trigger_payload` test);
  - finisher revokes the thread's leases; `cleanup_terminal_state` evicts
    orphaned leases and deletes their files (store adapter);
  - `__save_memory_doc__` creates a Skill doc for Mission threads and refuses
    non-Mission threads (orchestrator).
- `taskset -c 0-17 cargo check -j18` + targeted `cargo test` for
  `lunarwing_engine` and the bridge; clippy clean.
- Live smoke (optional, post-deploy): complete a 5+ step thread and observe a
  `skill-extraction` fire with a non-null `last_trigger_payload`.

## Risks / notes

- Broadcasting terminal events adds one broadcast per thread — negligible;
  subscribers already filter by kind.
- Lease revocation on terminal is safe: `refresh_resumed_thread_leases`
  re-grants on resume, so no `Waiting`/`Suspended` thread loses access (they are
  not terminal).
- `__save_memory_doc__` is mission-gated to avoid trusted-skill minting by
  arbitrary threads; prompts are the only documentation of it, no public API.
- `ORCHESTRATOR_SELF_MODIFY` and `SKILL_SELF_IMPROVEMENT` still default off;
  this proposal doesn't change those gates, it just makes the machinery
  functional when enabled.
