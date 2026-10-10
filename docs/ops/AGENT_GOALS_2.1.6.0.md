# PRE-RELEASE CHECKLIST for LunarWing v2.1.6.0

## Codename: Tsuno (角) - "Horns"

**Open TODOs (v2.1.6.0) — To be done before release**

~~Old parity~~
~~Rares = odd (1, 3, 5, 7, ...)~~
~~AJ = even (2, 4, 6, 8, ...)~~
### mod-3 lanes (2026-10-04): Rares = 1 mod 3, AJ = 2 mod 3, Sweetiebot = 0 mod 3


#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status
#### DONE-trail convention: record evidence as indented sub-bullets under the item — never inline on the checkbox line.

---

## Phase 1 — Foundation (engine, crates & architecture)

1. [ ] Update all external crates that need updating for the entire project - See previous release notes for examples of such crates. Ensure CI/CD is also updated for the new crate versions. Exit criteria: cargo check clean on all feature combos, cargo deny check clean, CI green on a scratch branch. worker pins, external crates, ci/cd must all be included in this. LunarWing crates must be bumped from 2.1.5 to 2.1.6 and all crates which require a version bump must also be bumped such as channel wasms. There should not be any 2.1.5 LunarWing crates left. Refer to goals doc from aragents/kimil00p/v2.1.4.0 in order to be thorough. all LunarWing-owned release packages, workspace crates, XMPP bridge, and WASM channels must be updated and lockfiles refreshed. All channel registry packages need bump. A tracked-manifest/lock audit must confirm no project-owned package or path dependency remains at 2.1.4 - External crates audit and update must be completed - ic/vendor/ must be regenerated - Worker/CI audit must be completed - cargo deny check and all associated things usually done with each release should be done. there may be docs guidance that you need to update. See older releases for more info on this if any clarification is needed. External worker upgrades such as kimi, opencode2, pebble, etc may be done in a seperate goal item
2. [x] Due to MULTIPLE various security concerns, including ethics of non-self-hosted relays being added into the project, opencode 2 external worker WILL NOT BE UPDATED - A WORK ITEM HAS BEEN CREATED PROVIDING FULL EXPLANATION AS TO WHY THIS IS BEING GATED - WE WILL REVISIT THIS CONCERN AFTER THE RELEASE OF 2.1.5.0 - CHECKING THIS BOX OFF SO AGENTS STOP ATTEMPTING THIS - Update external workers - opencode2 to 2.0.26 - NEW VERSION - update from current version - refer to previous commits related to opencode2 since the lunarwing 2.1.4.0 release for details - last MR was https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1490 - see work item https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/357

## Phase 2 — Features & Polish (for goals referencing work items, read comments on work items as well)

3. [ ] audit all crates and ensure there are no security vulnerabilities
4. [x] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/298 - Brightdawn confirmed close now
5. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/324
6. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/325
7. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/326
8. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/328
9. [x] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/329
  - Completed 2026-10-10 on branch `sweetiebot/kimi-9-20261010-1205`. Status: 100% — all three WI#329 fix items are now satisfied in-tree.
  - Fix item 1 (call-path correction) was already landed: `opencode2.4lunarwing/scripts/opencode_task_executor.ts:497` calls top-level `sdk.message.list` with `.data` unwrap, server-contract paging (order on first page only, short-page end signal, 5-page bound), and a mock mirroring the real client shape (commits f3fdc0437 / 28b75debe, in branch).
  - Fix item 2 (single list driving build gate + test mock) implemented: new `opencode2.4lunarwing/scripts/sdk_surface.ts` is the one committed list of bridge SDK method paths (`session.create/prompt/interrupt`, `permission.reply`, `event.subscribe`, `message.list`) plus known-stale paths (`session.message.list`, `session.permission.reply`); new `scripts/sdk_surface_assert.ts` constructs the pinned promise client against a dummy base URL and asserts `typeof` per path (fails the image build on drift); the Dockerfile RUN now executes that script instead of the inline `bun -e` copy; the executor self-test asserts the injected mock implements EXACTLY the shared list (both directions) and no stale paths.
  - Fix item 3 (docs reword): dated WI#329 corrections added to the stale v2.0.11 / v2.0.14 / v2.0.20 bump entries in `docs/ops/WORKER-CONTAINERS.md` (v2.0.21 and v2.0.23/2.0.24 entries already carried corrections); the RELEASE-v2.1.5.0 sentence the WI quoted no longer exists (already reworded — verified by repo-wide grep); `opencode2.4lunarwing/AGENTS.md` Key Files now lists `sdk_surface.ts`.
  - CI provenance: `.gitlab/scripts/build-worker-image.sh` opencode2 `input_files` manifest now records both new scripts (hashed into worker-inputs.sha256, verified at promotion).
  - Verification (host-side, bun 1.3.1): all 4 TS self-tests pass (`opencode_task_executor_test.ts` incl. new parity assertions, path_expand, opencode_result, task_prompt); `sdk_surface_assert.ts` passes against the REAL pinned v2.0.24 client source (sparse clone at OPENCODE_REF `e7a34f09bfd9134dfade5a8ddb843f7030bc9a69`) and fails closed (exit 1, FATAL message) on all drift classes — missing `message.list` on both stub and real source, stale `session.message.list` appearing, module unresolvable; mock-parity negative control fires; `validate_config.py` green; 15/15 `.gitlab/tests/test_goal1_ci.py` green; `bash -n` green. Whole-image candidate build remains a protected-pipeline gate as already recorded in WORKER-CONTAINERS.md.
  - Note: opencode2 stays pinned at v2.0.24 in this lane — the v2.0.26 bump is gated off per checklist item #2 (WI#357) and was not pulled in.
10. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/330
11. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/331
12. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/332

## Phase 4 — Carried Known-Issues from RELEASE-v2.1.3.0.md (for goals referencing work items, read comments on work items as well)

13. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/334
14. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/337
15. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/338
16. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/339
17. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/341
18. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/351
19. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/352
20. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/356
21. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/357
22. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/359
23. [x] work on x
24. [x] (S) (H MR) come back to item 2 at some point after manual review is completed (NOTE FOR LATER)
25. [x] (S) (H MR) release alpha of droid and desk repo projs
26. [x] (S) RESERVED
27. [x] (S) RESERVED
28. [ ] fix issues in CI/CD (if any) - See Pipeline number 5397 for latest master pipeline run and see if anything is not GREEN
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

## Phase 5 - Documentation and Misc goal items (for goals referencing work items, read comments on work items as well)

39. [ ] update all documentation under docs/architecture
40. [ ] Create release notes draft at the root of the repo (as we usually do with each release; see previous release notes in docs/releases for references)

---
