# PRE-RELEASE CHECKLIST for LunarWing v2.1.5.0

## Codename: UNKNOWN

**Open TODOs (v2.1.5.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status
#### DONE-trail convention: record evidence as indented sub-bullets under the item — never inline on the checkbox line.

---

## Phase 1 — Foundation (engine, crates & architecture)

1. [ ] Last attempt was merged in but has multiple CI/CD failures.... See pipeline #4679 for all failing jobs... there are multiple.... please address and fix these....Update all external crates that need updating for the entire project - See previous release notes for examples of such crates. Ensure CI/CD is also updated for the new crate versions. Exit criteria: cargo check clean on all feature combos, cargo deny check clean, CI green on a scratch branch. worker pins, external crates, ci/cd must all be included in this. LunarWing crates must be bumped from 2.1.4 to 2.1.5 and all crates which require a version bump must also be bumped such as channel wasms. There should not be any 2.1.4 LunarWing crates left. Refer to goals doc from aragents/kimil00p/v2.1.4.0 in order to be thorough. all LunarWing-owned release packages, workspace crates, XMPP bridge, and WASM channels must be updated and lockfiles refreshed. All channel registry packages need bump. A tracked-manifest/lock audit must confirm no project-owned package or path dependency remains at 2.1.4 - External crates audit and update must be completed - ic/vendor/ must be regenerated - Worker/CI audit must be completed - cargo deny check and all associated things usually done with each release should be done. there may be docs guidance that you need to update. See older releases for more info on this if any clarification is needed. External worker upgrades such as kimi, opencode2, pebble, etc may be done in a seperate goal item
2. [ ] update external workers - opencode2 to 2.0.21 - NEW VERSION - update from 2.0.20

## Phase 2 — Features & Polish

3. [ ] audit all crates and ensure there are no security vulnerabilities

## Phase 4 — Carried Known-Issues from RELEASE-v2.1.3.0.md (for goals referencing work items, read comments on work items as well)

4. [x] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/302
  - DONE 2026-10-03 (aj/kimi-4-20261003-0600): WI#302 "tasks are rarely hallucinated completed although not completed". Implemented the three cheap fix shapes from the KB audit `sweetiebot/terminal-completed-evidence-audit-wi302` §5 (items 1–3), which had source-verified the defect mechanisms at HEAD 0ece377c1:
  - Fix 1 (`6bb01a410`): v1 job lane fall-through — `ic/src/worker/job.rs`: `execution_loop` now returns the `LoopOutcome`; the worker wrapper no longer falls through to `mark_completed()` ("Job completed successfully") when the loop ended via `LoopOutcome::Stopped`/`NeedApproval` without a terminal state (session teardown, daemon shutdown, channel removal). Mirrors the container lane's `Stopped → success: false`. New `loop_ended_via_stop` helper + unit test.
  - Fix 2 (`2c96610a5`): completion sniffer — `ic/src/util.rs` `llm_signals_completion` was substring-contains, so "I have completed step 1 of 10, moving on" fired completion. A positive phrase now only counts when the rest of its line has no progress qualifiers (enumerated "step 1"/"phase two", numeric "3 of 7", hedges "but"/"moving on"/"remains"/"more to do"/"still need"). Genuine claims unchanged; 2 new regression tests pin both directions.
  - Fix 3 (`034cacfeb`): engine-v2 `parse_outcome` (`ic/crates/lunarwing_engine/src/executor/orchestrator.rs`) fail-loud — missing/non-string/unknown `outcome` key maps to `Failed{kind: Orchestrator}` instead of `Completed{None}` (which had also suppressed the channel reply and mission history). Runtime manager already transitions the thread to Failed; missions record/notify/bounded-retry on that path. 3 new tests.
  - Fix 4 (`3737e617b`): engine-v2 default orchestrator (`ic/crates/lunarwing_engine/orchestrator/default.py`) — token/time/cost budget exhaustion no longer transitions the thread to Completed with the budget notice as the final answer; it now transitions to Failed and returns outcome "failed" with an explicit "exhausted before the task completed" error (`failure_kind: budget_exhausted`).
  - Verified: `cargo check` clean (lunarwing lib 4m47s; lunarwing_engine); targeted tests green — worker:: 102/102, util:: 34/34 (2 new), engine executor:: 266/266 (incl. 3 new parse_outcome); clippy `-D warnings` clean on `lunarwing` (lib+tests) and `lunarwing_engine` (lib+tests); rustfmt clean on touched files; `py_compile` clean on default.py.
  - Deliberately out of scope (audit §5 items 4–5, larger): mission goal-claim verification tier (post-`declares_goal_achieved` evidence pass), and unifying thread-state vs outcome semantics docs. Noted on the WI.
5. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/301
6. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/300
7. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/299
8. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/298
9. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/297
10. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/296
11. [x] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/296 (dupe)
12. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/295
13. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/294
14. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/291
15. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/290
16. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/289
17. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/288
18. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/287
19. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/278
20. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/276

## Phase 5 - Miscellaneous (for goals referencing work items, read comments on work items as well)

21. [ ] take a look at any documentation that should be updated and update necessary documentation
22. [ ] create release notes draft at the root of the repo (as we usually do with each release; see previous release notes in docs/releases for reference)
23. [ ] review all currently open work items - create audit document for 2.1.5.0 in docs/ops - there are similar docs in docs/ops for 2.1.4.0 and earlier releases so you can use those as references
24. [ ] update all docs in docs/guides
25. [ ] update all docs in docs/architecture
26. [ ] fix issues in CI/CD (if any) - See Pipeline number 4975 for latest master pipeline run and see if anything is not GREEN (cargo deny has issue now at the least)
27. [ ] come up with ideas for names for 2.1.5.0 - write in a document in docs/ops
28. [ ] Repo housecleaning: 1) get rid of old stuff in docs/ops and move it to history 2) remove docs/ov directory
29. [x] (S) RESERVED
30. [x] (S) RESERVED
31. [x] (S) RESERVED
32. [x] (S) RESERVED
33. [x] (S) RESERVED
34. [x] (S) RESERVED
35. [x] (S) RESERVED
36. [x] (S) RESERVED
37. [x] (S) RESERVED
38. [x] (S) RESERVED
39. [x] (S) RESERVED
40. [x] (S) RESERVED

---
