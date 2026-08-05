# PRE-RELEASE CHECKLIST for LunarWing v2.0.5.0

## Codename: Undecided

**Open TODOs (v2.0.5.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

---

1. [ ] update all external crates that need updating for entire project. see previous release notes for some examples of such crates - ensure CI/CD is also updated for the new crate versions
2. [ ] ensure crates are updated
3. [ ] identify crates which have not been updated which should be - put in a new doc
4. [ ] Bump lunarwing crate versions and wasm channel crates for lunarwing that have same version. they are at 2.0.4. we need bump to 2.0.5 - ensure CI/CD is also updated for the new crate versions
5. [ ] ensure EXTERNAL crates are updated
6. [ ] take look at Advisory Status (`deny.toml`) in docs/ops/CRATE_AUDIT_v2.0.3.0.md and install cargo-deny using rustup, cargo, then remove items marked as`now unblocked`
7. [ ] DarkIRC secure staged key exchange. H1/H2 fixed and verified; H3 partially fixed, with live isolation and leak evidence still required. Full boundary, audit ledger, validation, blockers, and next steps: [`DARKIRC-KEYEXCHANGE-ITEM15-STATUS.md`](DARKIRC-KEYEXCHANGE-ITEM15-STATUS.md). Reference branch: darkirckeyexch8/cmc/v2.0.4.0
8. [ ] self healing capability expansion - see prev attempt for reference: shexpansion/cmc/v2.0.4.0
9. [ ] Implement additional UI fixes in document in docs/ops — `docs/ops/UI_FIX_SUGGESTIONS.md`
10. [ ] review darkirc key exchange implementation (only if completed)
11. [ ] work on coordination improvements (there is doc in docs/ops with info and list of known issues which has been updated recently)
12. [ ] come up with codename ideas for release. put into new doc
13. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.4.0 explaining all relevant changes since v2.0.3.0 as well as revising and including an ACCURATE VERSION OF`known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. The codename for this release is:`UNKNOWN` — The file you write will be`RELEASE-v2.0.4.0.md` and should be written to the ROOT of the repo. When complete with draft, move on to another task, which is: Improve accuracy of the first draft of release notes (from previous task) -`RELEASE-v2.0.4.0.md`
14. [ ] update README.md in repo root (if necessary)

---
