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
9. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/329
10. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/330
11. [x] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/331
    - DONE (2026-10-10, branch `aj/kimi-11-20261010-1200`): all three WI#331 asks completed.
    - libsql legacy chain (RUSTSEC-2026-0049/-0098/-0099/-0104, RUSTSEC-2026-0258): cleared from default builds by mitigation — libsql dep now uses only `core`; Turso remote replicas moved behind new non-default cargo feature `libsql-remote-replica` (ic/Cargo.toml, ic/src/db/libsql/mod.rs, ic/src/db/mod.rs, ic/src/setup/wizard.rs). Verified: `cargo tree -i rustls-webpki@0.102.8` and `-i h2@0.3.27` report "did not match any packages" with default features.
    - rsa (RUSTSEC-2023-0071): keep accepted, track upstream — advisory still `patched = []` (checked 2026-10-10); rsa 0.10.0-rc.19 published but not marked patched; russh 0.64.1 still pins `rsa =0.10.0-rc.18`, so a russh bump clears nothing.
    - failure/rental (RUSTSEC-2019-0036/2020-0036/2021-0134): keep accepted; porting vendored libsignal-protocol off failure (52 uses / 23 files) documented as dedicated follow-up work.
    - Every `reason` string in ic/deny.toml now references WI#331 with re-verified 2026-10-10 exposure notes; decision record at docs/ops/v2.1.6.0_WI331_ADVISORY_CLEARING.md; setup README updated.
    - Verified: cargo check default / libsql-only(+tests) / all-features pass; clippy -D warnings (libsql-only + all-features --lib) clean; cargo-deny CI wrapper green (advisories/bans/licenses/sources ok, advisory-db 2026-10-09); lockfile-only advisory guard ok; cargo fmt clean.
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
