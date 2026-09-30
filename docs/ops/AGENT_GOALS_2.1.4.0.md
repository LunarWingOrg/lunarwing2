# PRE-RELEASE CHECKLIST for LunarWing v2.1.4.0

## Codename: UNKNOWN

**Open TODOs (v2.1.4.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status
#### DONE-trail convention: record evidence as indented sub-bullets under the item — never inline on the checkbox line.

---

## Phase 1 — Foundation (engine, crates & architecture)

1. [x] Last attempt was merged in but has multiple CI/CD failures.... See pipeline #4679 for all failing jobs... there are multiple.... please address and fix these....Update all external crates that need updating for the entire project - See previous release notes for examples of such crates. Ensure CI/CD is also updated for the new crate versions. Exit criteria: cargo check clean on all feature combos, cargo deny check clean, CI green on a scratch branch. worker pins, external crates, ci/cd must all be included in this. LunarWing crates must be bumped from 2.1.3 to 2.1.4 and all crates which require a version bump must also be bumped such as channel wasms. There should not be any 2.1.3 LunarWing crates left. Refer to goals doc from aragents/kimil00p/v2.1.3.0 in order to be thorough. all LunarWing-owned release packages, workspace crates, XMPP bridge, and WASM channels must be updated and lockfiles refreshed. All channel registry packages need bump. A tracked-manifest/lock audit must confirm no project-owned package or path dependency remains at 2.1.3 - External crates audit and update must be completed - ic/vendor/ must be regenerated - Worker/CI audit must be completed - cargo deny check and all associated things usually done with each release should be done. there may be docs guidance that you need to update. See older releases for more info on this if any clarification is needed. External worker upgrades such as kimi, opencode2, pebble, etc may be done in a seperate goal item
    - IN PROGRESS 2026-09-30 (`cmc/g1/v2.1.4.0`, !1267): inspected all #4679 jobs; the two failures are Cavepony release metadata drift and the orphan-mission test's direct libSQL import. Fixes retain locked builds, the fuel budget, required artifacts, and the unchanged boundary ratchet. Full validation now also covers integration-target MRs, with opt-in coverage for scratch branches. Evidence and remaining acceptance checks: [CI recovery record](v2.1.4.0_GOAL1_CI_RECOVERY.md). Keep this item open until the corrected head's full remote pipeline passes.
    - FOLLOW-UP 2026-09-30: #4685 failed `version-policy` because `172e4bb5c` reused Cavepony's existing registry version `0.4.3`. Advanced all five version fields to `0.4.4`; the exact CI wrapper passes against MR base `9fad7de50`, and a new shell regression covers metadata-only alignment and MR-base precedence. No policy bypass or third-party dependency changes; full corrected-head remote CI is still required.
2. [x] update external workers - opencode2 to 2.0.20 - NEW VERSION - update from 2.0.18
    - DONE 2026-09-30 (Applejack, item-2): bumped opencode2 worker v2.0.18 → v2.0.20 at reviewed full commit `84c9be93a56304a108f1a22df0c5d62c26d5b6ca` (tag SHA verified live against the anomalyco/opencode remote).
    - Files: `opencode2.4lunarwing/Dockerfile` (OPENCODE_VERSION/OPENCODE_REF), `.gitlab/scripts/build-worker-image.sh` (CI source_ref + build args), `opencode2.4lunarwing/scripts/prepare_reproducible_build_test.py` (fixture → 2.0.20), `opencode2.4lunarwing/AGENTS.md` + `README.md` (pin refs), `docs/ops/WORKER-CONTAINERS.md` (pin table + bump changelog entry), `docs/guides/CONTRIBUTOR-ONBOARDING.md` (pin mention).
    - Verified: upstream `packages/cli/script/build.ts` at the new SHA still has exactly one `bytecode: true` / `splitting: true` (patch contract holds); bridge surface unchanged (`OpenCode.make`, `session.create` w/ `permissions`, `session.message.list`, `session.permission.reply`, `session.interrupt`, `event.subscribe`, all matched event names; diff is additive — new `credential.list/create` endpoints unused). Host-side `prepare_reproducible_build_test.py` 3/3 OK; shellcheck clean on the CI script. 100% except: whole-image reproducibility must still pass the CI candidate-vs-clean-rebuild digest gate before promotion (CI-only, as with prior bumps)

## Phase 2 — Features & Polish

3. [ ] audit all crates and ensure there are no security vulnerabilities - Last attempt was rejected. See comments on previous closed MR for details on what needs to be addressed: https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1264

## Phase 4 — Carried Known-Issues from RELEASE-v2.1.3.0.md (for goals referencing work items, read comments on work items as well)

4. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/268 - Last attempt was rejected. See comments on previous closed MR for details on what needs to be addressed: https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1261
5. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/265 - Last attempt was rejected. See comments on previous closed MR for details on what needs to be addressed: https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1256
6. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/264 - Last attempt was rejected. See comments on previous closed MR for details on what needs to be addressed: https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1263
7. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/244 - Last attempt was rejected. See comments on previous closed MR for details on what needs to be addressed: https://git.lunarwing.org/lunarwing/lunarwing2/-/merge_requests/1258
8. [x] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/243
    - DONE 2026-09-30 (`aj/kimi-8-20260930-1200`, item 8): the goal-54 rollover fix was correct on the engine side, but the recurrence root cause sat in the host store. `HybridStore` never overrode `list_all_missions_global`/`list_all_active_missions_global`, so both inherited the trait default that derives from `list_all_projects()`. Missions load by directory scan (`engine/projects/*/missions/*/mission.json`) while projects load from `project.json`, so a mission whose project record was missing/unparseable loaded into the missions map but was invisible to every global listing — never bootstrapped into the active index, so no startup/tick rollover, no event/webhook dispatch, and `threads_today_date` pinned at its creation date (matches the dwarfstar `expected-behavior`/`canary-delta-m4t` evidence). Both globals now enumerate the missions map directly (same semantics the engine test store already had)
    - Verified: new libsql regression `global_mission_listings_see_missions_whose_project_file_is_missing` (missions persisted without their project record remain globally enumerable; only Active ones reach the active-only listing) passes; full engine mission suite and lib clippy with `--all-features -D warnings` pass; `docs/architecture/ENGINE-V2.md` scheduling contract updated. Live dwarfstar acceptance (inspecting persisted `mission.json` after a day rollover) remains an operator step
9. [x] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/220
    - DONE 2026-09-30 (`cmc/g9/v2.1.4.0`, WI#220): Settings → System Prompt provides primary-scope source editing, missing-file creation, draft preservation and a saved workspace preview with group/timezone controls. Authenticated `/api/prompts` routes retain workspace write validation/indexing and scope isolation. [Operator guide](../guides/TENANT-SYSTEM-PROMPTS.md) documents the Engine V2 initialization snapshot: activation requires `restart-tenant` followed by a new conversation; existing threads retain their System message.
    - Verified: 5 authenticated router regressions pass (libSQL-backed storage, tenant/read-scope isolation, rejected writes, group/timezone composition and initialized owner bootstrap state); 5 Chromium browser regressions pass (save/create, failed saves and reload/discard, preview races/errors, auth-session clearing). Desktop/mobile layouts inspected; JavaScript syntax, Rust formatting and scoped diff whitespace checks pass.
    - Rust verification ran in tmux with `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`: `taskset -c 0-11 cargo test --locked --offline -j4 --lib --no-default-features --features postgres,libsql channels::web::tests::multi_tenant::prompt_handlers -- --test-threads=4` and `taskset -c 0-11 cargo clippy --locked --offline -j4 --no-default-features --features postgres,libsql --lib -- -D warnings` both pass. Browser command from `ic/`: `pytest -c tests/e2e/pyproject.toml tests/e2e/scenarios/test_prompt_editor.py -q` (routed API; no live tenant deployment).
10. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/191
11. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/184

## Phase 5 - Miscellaneous (for goals referencing work items, read comments on work items as well)

12. [ ] take a look at any documentation that should be updated and update necessary documentation
13. [ ] create release notes draft at the root of the repo (as we usually do with each release; see previous release notes in docs/releases for reference)
14. [ ] review all currently open work items - create audit document for 2.1.3.0
15. [x] (S) (COMEBACKTOTHISLATER) UPDATE RELEASE-v2.1.2.0.md WITH CHANGES MADE SINCE FIRST DRAFT WAS CREATED
16. [ ] update all docs in docs/guides
17. [ ] update all docs in docs/architecture
18. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/269
19. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/270
20. [ ] fix issues in CI/CD (if any)
21. [x] (S) (HUMAN) explore wireguard tun for tunnelling system for podman sandbox
22. [x] (S) (H) Consider web gateway additional functionality (seperate from goal 38)
23. [x] (S) new reconcile routine method improvement for MR review process
24. [x] (S) record deferred items somewhere in one place
25. [ ] come up with ideas for names for 2.1.4.0 - write in a document in docs/ops
26. [x] (S) (HUMAN) start thinking about scope of 2.1.6.0 next real feature set
27. [x] (S) (HUMAN) Findings from sb and kumo
28. [ ] Repo housecleaning - get rid of old stuff in docs/ops and move it to history

---
