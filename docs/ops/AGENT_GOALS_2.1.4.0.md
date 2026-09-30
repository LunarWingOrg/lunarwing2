# PRE-RELEASE CHECKLIST for LunarWing v2.1.4.0

## Codename: UNKNOWN

**Open TODOs (v2.1.4.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status
#### DONE-trail convention: record evidence as indented sub-bullets under the item — never inline on the checkbox line.

---

## Phase 1 — Foundation (engine, crates & architecture)

1. [ ] Update all external crates that need updating for the entire project. See previous release notes for examples of such crates. Ensure CI/CD is also updated for the new crate versions. Exit criteria: cargo check clean on all feature combos, cargo deny check clean, CI green on a scratch branch. worker pins, external crates, ci/cd must all be included in this. LunarWing crates must be bumped from 2.1.3 to 2.1.4 and all crates which require a version bump must also be bumped such as channel wasms. There should not be any 2.1.3 LunarWing crates left. Refer to goals doc from aragents/kimil00p/v2.1.3.0 in order to be thorough. all LunarWing-owned release packages, workspace crates, XMPP bridge, and WASM channels must be updated and lockfiles refreshed. All channel registry packages need bump. A tracked-manifest/lock audit must confirm no project-owned package or path dependency remains at 2.1.3 - External crates audit and update must be completed - ic/vendor/ must be regenerated - Worker/CI audit must be completed - cargo deny check and all associated things usually done with each release should be done. See older releases for more info on this if any clarification is needed. External worker upgrades such as kimi, opencode2, pebble, etc may be done in a seperate goal item
2. [x] update external workers - opencode2 to 2.0.20 - NEW VERSION - update from 2.0.18
    - DONE 2026-09-30 (Applejack, item-2): bumped opencode2 worker v2.0.18 → v2.0.20 at reviewed full commit `84c9be93a56304a108f1a22df0c5d62c26d5b6ca` (tag SHA verified live against the anomalyco/opencode remote).
    - Files: `opencode2.4lunarwing/Dockerfile` (OPENCODE_VERSION/OPENCODE_REF), `.gitlab/scripts/build-worker-image.sh` (CI source_ref + build args), `opencode2.4lunarwing/scripts/prepare_reproducible_build_test.py` (fixture → 2.0.20), `opencode2.4lunarwing/AGENTS.md` + `README.md` (pin refs), `docs/ops/WORKER-CONTAINERS.md` (pin table + bump changelog entry), `docs/guides/CONTRIBUTOR-ONBOARDING.md` (pin mention).
    - Verified: upstream `packages/cli/script/build.ts` at the new SHA still has exactly one `bytecode: true` / `splitting: true` (patch contract holds); bridge surface unchanged (`OpenCode.make`, `session.create` w/ `permissions`, `session.message.list`, `session.permission.reply`, `session.interrupt`, `event.subscribe`, all matched event names; diff is additive — new `credential.list/create` endpoints unused). Host-side `prepare_reproducible_build_test.py` 3/3 OK; shellcheck clean on the CI script. 100% except: whole-image reproducibility must still pass the CI candidate-vs-clean-rebuild digest gate before promotion (CI-only, as with prior bumps).

## Phase 2 — Features & Polish

3. [ ] audit all crates and ensure there are no security vulnerabilities

## Phase 4 — Carried Known-Issues from RELEASE-v2.1.2.0.md (candidates, weigh each)

4. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/268
5. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/265
6. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/264
7. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/244
8. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/243
9. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/220
10. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/191
11. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/184

## Phase 5 - Miscellaneous

12. [ ] take a look at any documentation that should be updated and update necessary documentation
13. [ ] create release notes draft at the root of the repo (as we usually do with each release; see previous release notes in docs/releases for reference)
14. [ ] review all currently open work items - create audit document for 2.1.3.0
15. [x] (S) (COMEBACKTOTHISLATER) UPDATE RELEASE-v2.1.2.0.md WITH CHANGES MADE SINCE FIRST DRAFT WAS CREATED
16. [ ] update all docs in docs/guides
17. [ ] update all docs in docs/architecture
18. [x] (S) perform write up on october experiment
19. [x] (S) compile proper list of changes wanted for web gateway itself
20. [x] fix issues in CI/CD
21. [x] (S) (HUMAN) explore wireguard tun for tunnelling system for podman sandbox
22. [x] (S) (H) Consider web gateway additional functionality (seperate from goal 38)
23. [x] (S) new reconcile method for MR review process
24. [x] (S) record deferred items somewhere in one place
25. [ ] come up with ideas for names for 2.1.4.0
26. [x] (S) (HUMAN) start thinking about scope of 2.1.6.0 next real feature set
27. [x] (S) (HUMAN) Findings from sb and kumo
28. [ ] Repo housecleaning - get rid of old stuff in docs/ops and move it to history

---
