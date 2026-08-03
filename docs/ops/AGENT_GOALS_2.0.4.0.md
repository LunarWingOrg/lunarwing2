# PRE-RELEASE CHECKLIST for LunarWing v2.0.4.0

## Codename: Odoshi (威し)

**Open TODOs (v2.0.4.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

###### (S) is usually noted to denote tasks that can be temporarily deferred

---

1. [ ] **DarkIRC secure staged key exchange.** H1/H2 fixed and verified; H3 partially fixed, with live isolation and leak evidence still required. Full boundary, audit ledger, validation, blockers, and next steps: [`DARKIRC-KEYEXCHANGE-ITEM15-STATUS.md`](DARKIRC-KEYEXCHANGE-ITEM15-STATUS.md).
2. [ ] self healing capability expansion - see prev attempt for reference: aj/kimi-24-20260801-1203
3. [ ] Suggest additional UI fixes in a new document in docs/ops — update`docs/ops/UI_FIX_SUGGESTIONS.md` with any additional findings
4. [ ] update any architecture docs in docs/architecture that may contain outdated information. verify against code rather than docs - prev attempt that may be referenced as it was fairly accurate at time of creation: rarity/kimi-31-20260801-0601
5. [ ] update any bugs docs in docs/bugs that may contain outdated information. verify against code rather than docs
6. [ ] update README.md at repo root with any missing feature information and other necessary information. remove unnecessary technical information
7. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.4.0 explaining all relevant changes since v2.0.3.0 as well as revising and including an ACCURATE VERSION OF`known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. The codename for this release is:`Odoshi (威し)` — The file you write will be`RELEASE-v2.0.4.0.md` and should be written to the ROOT of the repo. When complete with draft, move on to another task, which is: Improve accuracy of the first draft of release notes (from previous task) -`RELEASE-v2.0.4.0.md`
8. [ ] update anything outdated in docs/proposals
9. [ ] update anything outdated that is in docs/ops
10. [ ] update anything outdated in docs/guides - there are a ton of guides in here which have not been updated since v1. many are no longer relevant. they can be placed into archive: docs/guides/history
11. [ ] final pass over release notes and update any inaccuracies and inconsistencies

---
