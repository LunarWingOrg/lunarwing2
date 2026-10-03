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

4. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/302
5. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/301
6. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/300
7. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/299
8. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/298
9. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/297
10. [x] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/296
  - DONE (review-corrected): both model-visible exits of the `rlm_query` child loop in `ic/crates/lunarwing_engine/src/executor/scripting.rs` are now built from typed failure kinds only. The `Err(e)` arm maps through the shared `safe_model_failure_message` vocabulary; note that at this tree it cannot carry a provider error (`ExecutionLoop::run` converts every orchestrator error into `Ok(ThreadOutcome::Failed { kind: Orchestrator, .. })` and the child loop runs with no store), so that half is defence in depth. The `Ok(Failed { kind, error })` arm — the exit a child provider failure actually takes — no longer forwards the orchestrator's `error` string (engine-internal diagnostics for `Orchestrator`, free text under `ORCHESTRATOR_SELF_MODIFY`); `rlm_query_child_response` renders only the kind plus the fixed recovery instruction and logs the raw text host-side at debug.
  - Verified: `cargo test -p lunarwing_engine --lib executor::scripting::` green including `rlm_query_failure_message_redacts_provider_diagnostics`, `rlm_query_child_failed_outcome_renders_kind_not_error_text` (every `ThreadFailureKind` with a raw provider sentinel in `error`) and `rlm_query_child_provider_failure_stays_redacted` (end-to-end child loop with a failing provider); `cargo fmt --check` clean.
  - Commits: a1a856a1a + the review fix (branch aj/kimi-10-20261003-1000). If the FrontierTEEC text that motivated WI#296 is observed again in a mission outcome, it is not on the rlm_query path and needs its own triage.
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
