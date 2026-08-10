# PRE-RELEASE CHECKLIST for LunarWing v2.0.7.0

**Open TODOs (v2.0.7.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

---

1. [ ] investigate if any crates should be upgraded for security reasons. make a new document in docs/ops detailing findings
2. [ ] Bump lunarwing crate versions and wasm channel crates for lunarwing that have same version. they are at 2.0.6. we need bump to 2.0.7 - ensure CI/CD is also updated for the new crate versions — lockfiles must be refreshed - all channel registry patch versions must be also be bumped to comply with CI/CD
by CI source-version policy. CI/CD contains no hardcoded LunarWing package-version pin and derives versions from Cargo manifests; GitLab config validation passes. Verified
default and all-feature workspace checks, bridge and all four channel checks, locked resolution, version policy, and rustfmt under the six-core build cap.
3. [ ] Additional Opencode External Worker Polishing, including update of opencode version (latest is 1.18.9 )
4. [ ] LunarVision polishing, improving stability across both systemd/gentoo setups
5. [ ] LunarVoice polishing, improving stability across both systemd/gentoo setups
6. [ ] come up with codename ideas for release. put into new doc in docs/ops - base the name on all changes made since 2.0.6.0
7. [ ] go over items in work items ( hint: can use gitlab api to access work items without auth ). make note/document of any that are already confirmed fix and should be closed - put note/doc in docs/ops - check when done
8. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.7.0 explaining all relevant changes since v2.0.6.0 as well as revising and including an ACCURATE VERSION OF `known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. Keep it brief. The codename for this release is: `UNKNOWN` — The file you write will be `RELEASE-v2.0.7.0.md` and should be written to the ROOT of the repo
9. [ ] update any outdated documents in docs/proposals
10. [ ] update any outdated documents in docs/guides
11. [ ] update README.md in repo root (if necessary)
12. [ ] review first draft of release notes from prev step and update any inconsistencies
13. [ ] update readme from prev step and update any inconsistencies
14. [ ] Run cargo update and lockfile reconciliation before tag if necessary
15. [ ] check cicd of master for failures before beginning release process (after testing phase completed)

---
