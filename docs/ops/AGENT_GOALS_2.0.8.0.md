# PRE-RELEASE CHECKLIST for LunarWing v2.0.8.0

## Codename: Unknown

**Open TODOs (v2.0.7.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)

### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

---

1. [ ] investigate if any crates should be upgraded for security reasons. make a new document in docs/ops detailing findings - you may use existing documentation for reference
2. [ ] clean up repo - remove uncessary files - organize files in repo root a bit better - for example - crszsslw2412c09c1-8ea9-46bd-8063-084ccdd2f332.jpg in root of repo - maybe change file name? and move very old documentation to historical archive directory. dont just delete it
3. [ ] refactor mt admin - you may use this old branch from 2.0.5.0 era for inspiration - in fact, most of this can be reused likely - ref2/sunburst/mtadmin-refactor-v2.0.6.0
4. [ ] Bump lunarwing crate versions and wasm channel crates for lunarwing that have same version. they are at 2.0.7. we need bump to 2.0.8 - ensure CI/CD is also updated for the new crate versions — lockfiles must be refreshed - all channel registry patch versions must be also be bumped to comply with CI/CD by CI source-version policy. CI/CD contains no hardcoded LunarWing package-version pin and derives versions from Cargo manifests; verify GitLab config validation passes if possible - cargo update and all that good stuff as usual
5. [ ] mcp part 3 work - see existing documentation for details - pick one thing from extensive list and update this goal with what was done. state any work that was deferred or is incomplete. ensure that the whole project builds after you finish your thing - see roadmap for information as well
6. [ ] Additional Opencode External Worker Polishing, including updating the version of opencode to 1.18.18
7. [ ] health service improvements - see recent document in docs/proposals for more information
8. [ ] Cross-tenant knowledge sharing via coordination pipeline - see existing documentation
9. [ ] Vector embeddings hybrid ranking - see existing documentation
10. [ ] update roadmap in docs/ops with state of each goal
11. [x] (S) fix cicd again - RESERVED
12. [x] (S) LunarVoice/Lunarvision polishing
13. [x] (S) investigate cause of agents getting "stuck" in some kind of loop after accepting that they will perform a tool call and sometimes requiring multiple prompts in order to get them to use that tool (you may spin up a test tenant for this investigation) - fix the underlying issue if possible
14. [ ] mcp part 3 work - see existing documentation for details - pick one thing from extensive list and update this goal with what was done. state any work that was deferred or is incomplete. ensure that the whole project builds after you finish your thing - see roadmap for information as well - this is NOT a duplicate
15. [ ] come up with codename ideas for release. put into new doc in docs/ops - base the name on all changes made since 2.0.7.0
16. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.8.0 explaining all relevant changes since v2.0.7.0 as well as revising and including an ACCURATE VERSION OF `known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. Keep it brief. The codename for this release is not know yet - The file you write will be `RELEASE-v2.0.8.0.md` and should be written to the ROOT of the repo
17. [ ] update any architecture docs in docs/architecture that may contain outdated information. verify against code rather than docs
18. [ ] update any bugs docs in docs/bugs that may contain outdated information. verify against code rather than docs
19. [ ] update anything outdated in docs/proposals - verify against code rather than docs
20. [ ] update anything outdated in docs/guides - verify against code rather than docs
21. [ ] update anything outdated in docs/internal - verify against code rather than docs
22. [ ] create a fourth document in docs/ops similar in nature to docs/ops/guide-for-spin-up-gentoo-tenants.md and the arch guide and the fedora guide but for debian/ubuntu GNU/Linux hosts
23. [ ] add new health stuff to roadmap and add new ideas discussed to roadmap that are not currently present
24. [ ] update release notes draft in root of repo with anything might have missed that may be relevant
25. [ ] update README.md in repo root (include information related to sandboxed WASM MCPs and any other relevant information that may be missing)

---