# PRE-RELEASE CHECKLIST for LunarWing v2.1.1.0

## Codename: Zanshin (残心)

**Open TODOs (v2.1.1.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status
#### DONE-trail convention: record evidence as indented sub-bullets under the item — never inline on the checkbox line.

---

## Phase 1 — Foundation (engine & architecture)

1. [x] Update all external crates that need updating for the entire project. See previous release notes for examples of such crates. Ensure CI/CD is also updated for the new crate versions. Exit criteria: cargo check clean on all feature combos, cargo deny check clean, CI green on a scratch branch. worker pins, external crates, ci/cd must all be included in this. LunarWing crates must be bumped from 2.1.0 to 2.1.1 and all crates which require a version bump must also be bumped such as channel wasms. There should not be any 2.1.0 crates left. Refer to goals doc from aragents/kimil00p/v2.1.0.0 in order to be thorough
    - DONE repo-side 2026-09-08 (`rarity/kimi-1-20260908-0601`): bumped all 11 LunarWing-owned release packages from 2.1.0 to 2.1.1 (daemon, five workspace crates, XMPP bridge, and four WASM channels), refreshed their lockfiles, and bumped all four channel registry packages (DarkIRC 0.1.7, Multica 0.1.10, WeeChat 0.2.8, XMPP 0.1.13). A tracked-manifest/lock audit confirms no project-owned package or path dependency remains at 2.1.0.
    - External crates: all 26 tracked lockfile roots report `Locking 0 packages` in final `cargo update --workspace --dry-run` sweeps. Updated Monty from the pre-release 0.0.9 revision to the v0.0.17 release commit, the newest release that preserves LunarWing's per-VM allocation/memory limits and approved Astral Ruff source; 0.0.18 switches to a non-allowlisted Ruff fork and 0.0.19+ requires a separate process-global allocator migration. Updated yanked `wnaf` 0.14.0 to 0.14.1 in daemon and bridge graphs. Regenerated `ic/vendor/`; its 1,053 directories match every synced lockfile.
    - Worker/CI audit: Pebble remains at upstream HEAD/v0.5.0, Kimi Code remains at npm `latest` 0.41.0, and OpenCode remains deliberately pinned to proven v1.18.25 because current v1.18.29 has the documented live config/stream regressions. Dockerfile pins, immutable refs, CI build arguments, and the Kimi npm lock agree. Rust 1.96.1 remains the declared MSRV and CI pin; checksum-pinned cargo-deny 0.20.2 remains current. No CI pin edit was required; `validate_config.py`, version policy, and the Kimi mt-admin harness pass. A remote scratch-branch pipeline was not triggered in-session because this task did not authorize a push.
    - Verified clean: `cargo check --locked` for default, PostgreSQL-only, libSQL-only, and all-features; XMPP bridge native check; all four channel crates on `wasm32-wasip2`; `cargo deny check`; rustfmt; touched-crate clippy with `-D warnings`; and all 554 `lunarwing_engine` tests.

## Phase 2 — Polish

2. [x] Address the work item #128 — mt-admin restart parity: pebble worker unit stopped but never restarted** `[bug,mt-admin,2.1.0.0]` - NEW since ship. Any tenant bounce leaves the tenant without its Pebble worker. Small, sharp, operator-visible. Good early quick-win.
    - Pebble Quadlet shutdown now stops the owning systemd user unit first, with a container-stop fallback, and startup clears any `StartLimitBurst` lockout before restarting.
    - Regression coverage exercises the stop/start bounce ordering and fallback; `scripts/check-mt-admin.sh` plus `scripts/tests/test-mt-admin-worker-lifecycle.sh` passed.

3. [x] Last attempt on branch: rarity/kimi-3-20260908-1201 - Original Task: **#127 — Web gateway "sometimes output doesn't load" (SSE delivery-loss + Done-wipes-stream)** `[bug,2.1.0.0]` - Client-side mitigation merged (history reload on Done/missing-final). REMAINING per #48 relnotes pass: **server-side gap signaling** and **stream-text reload** limitation. Audit reference: `references/sse-stream-loss-audit.md` (WI#127 composite bug: Lagged swallow server-side + client Done-guard). See previous goal file from aragents/kimil00p/v2.1.0.0 for more reference
    - Server-side SSE and WebSocket subscriptions now convert broadcast lag into an explicit `stream_gap` event, log the missed-event count, and continue with retained events instead of silently swallowing the gap.
    - The browser clears partial live-stream state and reloads persisted history on `stream_gap`; `Done` also reconciles after its response grace period whenever no terminal response rendered, including turns that did receive stream text.
    - Added Rust serialization/lag-continuation coverage and browser scenarios for explicit gaps, streamed `Done` without a response, and the healthy response path. Verified rustfmt, 9 SSE manager tests plus the gap serialization test, touched-crate clippy with `-D warnings`, JavaScript/Python syntax, and E2E collection.

4. [ ] Last attempt on branch: aj/kimi-4-20260908-1201 - Original Task: **#126 — Investigate default mission behavior for thread start** - No goal item from aragents/kimil00p/v2.1.0.0 ever addressed it. Needs scoping before it's executable (investigation-shaped, not directive)

5. [x] (S) **#122 — New human supervised LunarVision test** - Unaddressed. Human-in-the-loop acceptance testing for LunarVision

6. [ ] **#121 — Knowledge seeder produces a static snapshot; no refresh path from hosted study MCP** `[knowledge,low]` - Goal #44 from aragents/kimil00p/v2.1.0.0 DESIGNED the fix (cadence-runner topology, revisioned hosted delta feed preferred, 2.1 fallback = atomically-published checkout generation + additive scans) and explicitly deferred implementation to 2.1.1.0. **This is the headline design-to-build item.** Report: `docs/ops/v2.1.0.0_KNOWLEDGE_STORE_RESYNC_OPTIONS.md`

7. [x] (S) **#114 — feature: generic sandbox — podman impl** `[low]` - Never addressed. Sandbox abstraction has docker; podman impl missing - ( wait for 2.1.2 )

8. [x] (S) **#113 — studynotesfindings — go over all** `[low]` - Never addressed. Sweep/verify all study-notes findings

9. [ ] Last attempt on branch: rarity/kimi-9-20260908-1802 - Original Task: **#109 — sanitize-memory mission registered Active but not firing on thread_start** `[bug,mission,sanitize-memory]` - Related scheduler work shipped (#94 warmup) but this firing bug itself is unaddressed. Real defect, needs repro

10. [ ] Work item #97 — Coordination knowledge sharing (production residual)** `[coordination,knowledge-sharing]` - Repo fix + 4/4 live matrix done (goals #13/#27/#37). REMAINING: (a) production sunburst/ruffles operator actions — set `XMPP_ALLOW_ROOMS_JSON`, `COORD_KNOWLEDGE_PUBLISHERS` full-occupant JIDs, restart both engines, re-test `KNOW:` wire path; (b) tracker body still carries the live-disproved user-bare recommendation — correct/close; (c) suggested hardening from #37: init-agnostic read-only mt-admin coordination doctor, bounded reason counters + empty-publisher startup warning, automated 4-case live matrix for release-sensitive changes, optional server-attested real-JID policy for non-anonymous MUCs - You may create multiple temporary testing tenants in order to test this properly. If you are successful in getting this whole thing to work, you must also write a guide in docs/guides on getting this set up correctly so it can be reproduced

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
