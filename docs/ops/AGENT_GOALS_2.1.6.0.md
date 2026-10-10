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

3. [x] audit all crates and ensure there are no security vulnerabilities
   - DONE (2026-10-10, branch `sweetiebot/kimi-3-20261010-0605`): audit doc `docs/ops/v2.1.6.0_CARGO_DENY_AUDIT.md`; RustSec advisory-db `7eebec6` (2026-10-09); v2.1.5.0 audit archived to `docs/ops/history/`
   - Main cargo-deny gate clean (advisories/bans/licenses/sources, `yanked = "deny"` verifiably enforced); satellite sweep 26/26 manifest roots ok (1 manifest-less lock gated by verify_upstream_lock.py); lockfile-only advisory guard ok (pyo3 absent, atty no-dev)
   - New this cycle: cargo-audit 0.22.2 full-lockfile sweep over all 27 committed locks (no resolve-graph filtering) — 25 fully clean; `ic` + `ic/bridges/xmpp-bridge` report exactly the accepted set, nothing hiding in uncompiled pins beyond guarded pyo3/atty
   - Accepted advisories (6 vuln + 1 unsound + 2 unmaintained: rustls-webpki 0.102.8 x4 and h2 0.3.27 via the libsql 0.6.0 legacy chain, rsa 0.10.0-rc.18 via russh, failure x2 + rental via the vendored libsignal OMEMO wrapper) re-verified upstream 2026-10-10 — libsql 0.9.30/0.10.0-pre.4 still pull hyper-rustls ^0.25, RUSTSEC-2023-0071 still `patched = []` (rc.19 also affected), russh 0.64.1 still pins rsa =0.10.0-rc.18, failure 0.1.8 final, rental 0.5.6 newest; deny.toml re-verification stamps updated
   - Advisory-db delta 2026-10-03 → 2026-10-09: 8 new advisories (all crates unpinned here), 1 modified (wasmtime RUSTSEC-2026-0223, unaffected <46.0.0; locked 36.0.17)
   - `.gitlab` tests 60/60 OK (need exec-able TMPDIR + PyYAML 6.0.3 on this VM); `validate_config.py` green
4. [x] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/298 - Brightdawn confirmed close now
5. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/324
6. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/325
7. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/326
8. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/328
9. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/329
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
