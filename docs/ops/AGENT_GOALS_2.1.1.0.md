# PRE-RELEASE CHECKLIST for LunarWing v2.1.1.0

## Codename: Zanshin (残心)

**Open TODOs (v2.1.1.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status
#### DONE-trail convention: record evidence as indented sub-bullets under the item — never inline on the checkbox line.

---

## Phase 1 — Foundation (engine & architecture)

1. [ ] Update all external crates that need updating for the entire project. See previous release notes for examples of such crates. Ensure CI/CD is also updated for the new crate versions. Exit criteria: cargo check clean on all feature combos, cargo deny check clean, CI green on a scratch branch. worker pins, external crates, ci/cd must all be included in this. LunarWing crates must be bumped from 2.1.0 to 2.1.1 and all crates which require a version bump must also be bumped such as channel wasms. There should not be any 2.1.0 crates left. Refer to goals doc from aragents/kimil00p/v2.1.0.0 in order to be thorough

## Phase 2 — Polish

2. [ ] Address the work item #128 — mt-admin restart parity: pebble worker unit stopped but never restarted** `[bug,mt-admin,2.1.0.0]` - NEW since ship. Any tenant bounce leaves the tenant without its Pebble worker. Small, sharp, operator-visible. Good early quick-win.

3. [ ] **#127 — Web gateway "sometimes output doesn't load" (SSE delivery-loss + Done-wipes-stream)** `[bug,2.1.0.0]` - Client-side mitigation merged (history reload on Done/missing-final). REMAINING per #48 relnotes pass: **server-side gap signaling** and **stream-text reload** limitation. Audit reference: `references/sse-stream-loss-audit.md` (WI#127 composite bug: Lagged swallow server-side + client Done-guard). See previous goal file from aragents/kimil00p/v2.1.0.0 for more reference

4. [ ] **#126 — Investigate default mission behavior for thread start** - No goal item from aragents/kimil00p/v2.1.0.0 ever addressed it. Needs scoping before it's executable (investigation-shaped, not directive).

5. [x] (S) **#122 — New human supervised LunarVision test** - Unaddressed. Human-in-the-loop acceptance testing for LunarVision.

6. [ ] **#121 — Knowledge seeder produces a static snapshot; no refresh path from hosted study MCP** `[knowledge,low]` - Goal #44 from aragents/kimil00p/v2.1.0.0 DESIGNED the fix (cadence-runner topology, revisioned hosted delta feed preferred, 2.1 fallback = atomically-published checkout generation + additive scans) and explicitly deferred implementation to 2.1.1.0. **This is the headline design-to-build item.** Report: `docs/ops/v2.1.0.0_KNOWLEDGE_STORE_RESYNC_OPTIONS.md`

7. [x] (S) **#114 — feature: generic sandbox — podman impl** `[low]` - Never addressed. Sandbox abstraction has docker; podman impl missing - ( wait for 2.1.2 )

8. [x] (S) **#113 — studynotesfindings — go over all** `[low]` - Never addressed. Sweep/verify all study-notes findings

9. [x] **#109 — sanitize-memory mission registered Active but not firing on thread_start** `[bug,mission,sanitize-memory]` - Related scheduler work shipped (#94 warmup) but this firing bug itself is unaddressed. Real defect, needs repro.
    - Reproduced with an active `engine/thread_start` sanitize-memory mission; the regression test now proves one foreground start fires it exactly once without Mission-thread recursion.
    - Fixed by broadcasting the initial `Created` → `Running` transition and routing it through the mission event listener with a stable `source_thread_id` payload.
    - Verified with targeted thread-start and terminal-event tests, `cargo fmt --all -- --check`, and warning-clean `cargo clippy -p lunarwing_engine --all-targets -- -D warnings`.

10. [ ] **#97 — Coordination knowledge sharing (production residual)** `[coordination,knowledge-sharing]` - Repo fix + 4/4 live matrix done (goals #13/#27/#37). REMAINING: (a) production sunburst/ruffles operator actions — set `XMPP_ALLOW_ROOMS_JSON`, `COORD_KNOWLEDGE_PUBLISHERS` full-occupant JIDs, restart both engines, re-test `KNOW:` wire path; (b) tracker body still carries the live-disproved user-bare recommendation — correct/close; (c) suggested hardening from #37: init-agnostic read-only mt-admin coordination doctor, bounded reason counters + empty-publisher startup warning, automated 4-case live matrix for release-sensitive changes, optional server-attested real-JID policy for non-anonymous MUCs

11. [ ] **#86 — rm unsupported v1 websocket protocol** - Explicitly flagged unaddressed by goal #41 findings. Removal work; check what still speaks v1 before cutting

12. [x] (S) Mascot Design

13. [x] (S) Reserved for potential CI/CD issues

## Phase 3 — Loose Ends (DONE-trail residuals not covered by open WIs)

14. [ ] **WeeChat WASM channel trap on fresh tenants** (goal #43, W1/W2 SKIPPED) - Traps on every poll on a fresh tenant; unique vs derpy/spitfire/dwarfstar; persists with IRC buffers pristine; prime suspect `dm_policy=open` path. Repro recipe in `docs/ops/v2.1.0.0_GOAL43_STEER_INTERRUPT_LIVE_RERUN_KI.md`. Also goal #9: test-only `dm_policy=open` was never made durable. FOLLOW-UP NOTE in doc: re-provision merlin per its Environment section when picking up (~15 min incl. build)

15. [x] (S) **Engine V2 interrupt → mascot stuck in `working` up to 30s** (goal #33 residual)
    `ThreadOutcome::Stopped` maps to the empty-string sentinel by !767 design, so engine-interrupt turns emit no terminal `response` event; mascot falls back to 30s idle timer instead of immediate idle. "Parity with legacy `Interrupted.` emission needs a tracker decision." Small polish, user-visible.

16. [x] (S) **WI#107 mascot: final human-visible browser animation check** (goal #45)
    Repo-contract PASS (27/27 emote tests, live public-surface checks on antelope/kestrel healthy). Remaining: the human acceptance step — fresh tenant, reply containing `*nickers softly* *flicks her tail*`, watch it animate. Cheap; could fold into any live session.

17. [ ] **Mission tracker candidates from goal #30** (recorded in `docs/ops/v2.1.0.0_GOAL30_MISSION_MEMORY_RETRIEVAL.md`)
    (a) orphaned mission_create records after failed turns sharing a webhook path (one fire matched 2 missions); (b) goal-phrase auto-complete trap for user-created missions; (c) `fix_pattern_database` re-seeding duplicates; (d) unbounded `approach_history`.

18. [ ] **Steer-as-tool-preemption contract clarification** (goal #41 finding on WI#105)
    Current steer = "inject after current step" contract; steer-as-tool-preemption is outside it. Needs tracker clarification or an explicit scope decision, not necessarily code.

19. [ ] **Priority-control vs queued-prompt race** (goal #9, documented-not-fixed)
    "Remained expected semantics" — but if 2.1.1.0 touches control-path dispatch (steer/interrupt family), this is adjacent.

20. [ ] **CI/CD validator gaps from GOAL32 review** (partial: goal #28 fixed the P0 set)
    Remaining non-P0 follow-ons from `docs/ops/v2.1.0.0_GOAL32_CI_CD_IMPROVEMENT_REVIEW.md`: validator doesn't check `WORKER_IMAGE_TAG`, OpenCode source ref, or pinned CI tool images; blocking clippy/architecture ratchets; reports/telemetry; classified standalone-crate and WASM matrices; artifact-safe mt-admin prerequisites before production CD. Daemon package publication + tenant deployment CD remain deferred by design.

## Phase 4 — Carried Known-Issues from RELEASE-v2.1.0.0.md (candidates, weigh each)

21. [x] (S) **DarkIRC Stages B–D** — still disabled behind separate acceptance gates (deferred per 2.1 board; do NOT resurrect unasked).

## Phase 5 - Documentation

22. [ ] update all docs in docs/guides
23. [ ] update all docs in docs/architecture
24. [ ] first draft of release notes

---
