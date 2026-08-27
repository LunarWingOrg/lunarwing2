> **Resolved 2026-07-29:** See [`SIS_FIX_PROPOSAL.md`](../SIS_FIX_PROPOSAL.md) for
> source findings, implementation, tests, and remaining limits. A summary is at
> the end of this file.

[02:31:03] [02:30:00] ## Skill Extraction Mission — Thread #2

Status: No trigger payload propagated to state. last_trigger_payload in mission.json is null. This is thread #2 of 3 allowed today (2026-07-29).

What happened: Mission was triggered (likely by a thread_completed_with_learnings event), but no trigger_payload was available in the thread's state dict. Without knowing which thread completed, I cannot extract a skill from it.

Action: Documented the issue. Need to check the engine's event emission code to understand why payloads aren't being propagated to the mission thread state.

Existing extracted skills: None found with source: "extracted". All existing skills have source: "migrated". [02:31:27] ## Skill Extraction Run #1 (2026-07-28)

Mission: skill-extraction, Thread #1 Status: Completed — extracted skill from reflex compiler fix task pattern

Extracted skill: skill:external-worker-dispatch

    Based on 7+ job attempts managing the opencode worker for the reflex compiler fix task
    Covers: pre-dispatch checks, job creation, worker environment, monitoring, timeout awareness, double-dispatch prevention
    Keywords: opencode, worker, dispatch, job, external worker, sandbox job, create_job
    Domain tags: worker, dispatch, job-management, opencode, external-worker, sandbox

Issue: Could not access state["trigger_payload"] — repl blocks were not executing in this environment. State/context variables may not be properly injected for mission threads. Had to work from daily log context instead.

Next focus:

    Fix repl/state access for future extraction runs
    Watch for other patterns: TensorZero config tuning, memory file management, SSH/git workflows, doc generation
    Consider extracting: "TensorZero timeout diagnosis" and "Rust cargo environment setup" as separate skills

[02:35:55] ## Self-Improvement Machinery Test — 2026-07-28

Three missions tested via mission_fire():
1. self-improvement — ✅ Working as designed

    Ran 2-3 no-op cycles. Orchestrator still v0 (unpatched).
    0 consecutive failures (no auto-rollback risk).
    Fix pattern database has 5 patterns loaded (tool-not-found, str-indices TypeError, NameError, byte-boundary slicing, model-call failure).
    Verdict: Healthy system, nothing broken to fix. Correct no-op behavior.

2. skill-extraction — ⚠️ Partial success, hit event bug

    Thread #1: ✅ Extracted real skill skill:external-worker-dispatch (based on 7+ reflex compiler fix attempts). Covers pre-dispatch checks, job creation, worker env, monitoring, double-dispatch prevention.
    Thread #2: ❌ Bug — trigger_payload was null in thread state. Mission didn't know which thread to extract from, couldn't act.
    Verdict: Extraction logic works when it gets data. Event propagation is broken.

3. profile-builder — ❌ No output

    0 results in memory. Current engine shows conversation-insights instead, which may be the successor.

Engine State at test time
Metric 	Value
Orchestrator version 	v0 (unpatched)
Skills extracted 	225
Lessons 	0
Summaries 	0
Issues 	0
Active threads 	0
Active leases 	582 (possible leak — old leases not cleaned up)
Active missions 	3 (expected-behavior, skill-extraction, conversation-insights)
Key Bug Found

Trigger payloads not propagating to mission thread state. Most mission threads spin up blind — they fire but don't receive the event data that triggered them, so they can't act on specific threads/events. This is the root cause of mission threads doing nothing.
Action Items

    Fix trigger_payload propagation in engine (event → mission thread state)
    Investigate 582 active leases leak (0 active threads but leases not cleaned)
    Confirm whether profile-builder was replaced by conversation-insights

---

## Resolution — 2026-07-29

All action items are addressed by `docs/ops/SIS_FIX_PROPOSAL.md` (item 41),
grafted from branches `k-sis-m-2/2.0.3.0` and `g-sis-m-2/2.0.3.0`:

1. **Trigger payload / blind missions — fixed, two root causes:**
   - Terminal `StateChanged` events were never broadcast on the event bus, so
     the mission event listener never fired in production. The thread finisher
     now broadcasts the terminal event, but only *after* the final thread state
     is persisted — otherwise the listener reloads a pre-`Done` thread and
     silently skips extraction.
   - `state["trigger_payload"]` was never injected despite all mission prompts
     referencing it. Mission fires now seed it into the thread's initial
     orchestrator state, kept in a durable `initial_persisted_state` key so it
     survives a gate-pause resume and process restart.
2. **Lease leak — fixed:** leases are revoked on terminal transition in the
   finisher and rolled back if a spawn or resume fails partway. The startup
   sweep durably revokes leases owned by terminal or missing threads and evicts
   them from memory; files stay on disk as audit history and reload as revoked.
3. **profile-builder — never existed in code** (zero matches in tree and git
   history). `conversation-insights` is the profile-analog mission. Any
   `profile-builder` mission in the field was created out-of-band via
   `mission_create` and can be deleted.
4. **The gating fix — extraction write path:** mission prompts instructed saves
   via a `memory_write(title=…, doc_type=…, metadata=…)` signature the v1 tool
   does not have; those arguments were silently dropped, which is why run #1
   reported an extracted skill while the audit found zero `source: "extracted"`
   docs — it was written as prose. Missions now have a
   `__save_memory_doc__(title, content, doc_type, tags, metadata, shared=False)`
   host function (mission threads only), and the prompts use it.

Also fixed in passing: the conversation-insights counter was keyed by project
only, so one user's completed threads consumed another's five-thread cadence in
a shared project. It is now keyed `project:user`.
