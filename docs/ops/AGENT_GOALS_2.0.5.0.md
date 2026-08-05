# PRE-RELEASE CHECKLIST for LunarWing v2.0.5.0

## Codename: Undecided

**Open TODOs (v2.0.5.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

---

1. [ ] update all crates that need updating for entire project. see previous release notes for some examples of such crates - ensure CI/CD is also updated for the new crate versions
1. [ ] Bump lunarwing crate versions and wasm channel crates for lunarwing that have same version. they are at 2.0.4. we need bump to 2.0.5 - ensure CI/CD is also updated for the new crate versions
15. [x] take look at Advisory Status (`deny.toml`) in docs/ops/CRATE_AUDIT_v2.0.3.0.md and install cargo-deny using rustup, cargo, then remove items marked as`now unblocked`
1. [] DarkIRC secure staged key exchange. H1/H2 fixed and verified; H3 partially fixed, with live isolation and leak evidence still required. Full boundary, audit ledger, validation, blockers, and next steps: [`DARKIRC-KEYEXCHANGE-ITEM15-STATUS.md`](DARKIRC-KEYEXCHANGE-ITEM15-STATUS.md). Reference branch: darkirckeyexch8/cmc/v2.0.4.0
2. [ ] self healing capability expansion - see prev attempt for reference: shexpansion/cmc/v2.0.4.0
26. [ ] Implement additional UI fixes in document in docs/ops — `docs/ops/UI_FIX_SUGGESTIONS.md`
30. [ ] review darkirc key exchange implementation (only if completed)
38. [ ] update README.md at repo root
39. [ ] come up with codename ideas for release. put into new doc
40. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.4.0 explaining all relevant changes since v2.0.3.0 as well as revising and including an ACCURATE VERSION OF`known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. The codename for this release is:`UNKNOWN` — The file you write will be`RELEASE-v2.0.4.0.md` and should be written to the ROOT of the repo. When complete with draft, move on to another task, which is: Improve accuracy of the first draft of release notes (from previous task) -`RELEASE-v2.0.4.0.md`
and lease rollback on spawn/resume failure; profile-builder confirmed nonexistent (conversation-insights is the equivalent)
43. [ ] review release notes from item 40 -else skip and mark this done
44. [ ] Ensure proposals in docs/proposals are up to date
45. [ ] The roadmap document - which can be found at`docs/ops/ROADMAP_2026.md` - needs proper updating - the things we discussed and have documents for in docs/proposals which are not already in there should be added for 2.0.6.0 and 2.0.8.0 and 2.1.0.0 - spread them out if possible
46. [ ] create a document in docs/ops to give a list of all the things that were changed since 2.0.3.0 and what to test thoroughly on upgraded testing tenants
47. [ ] ensure roadmap doc in docs/ops is up to date - this is a followup from previous item and not a duplicate
48. [ ] inspect darkirc adapter code quality and improve if possible - do nothing if already good
49. [ ] inspect weechat service code quality and improve if possible (gentoo)
50. [ ] ensure roadmap doc in docs/ops is up to date and update if necessary - this is a followup from previous item and not a duplicate
51. [ ] inspect code for reflex compiler accuracy again
52. [ ] ensure roadmap doc in docs/ops is up to date and update if necessary - this is a followup from previous item and NOT a duplicate





2. [x] (S) self healing capability expansion - see prev attempt for reference: shexpansion/cmc/v2.0.4.0

10. [ ] update anything outdated in docs/guides - there are a ton of guides in here which have not been updated since v1. many are no longer relevant. they can be placed into archive: docs/guides/history
11. [ ] final pass over release notes and update any inaccuracies and inconsistencies

---
