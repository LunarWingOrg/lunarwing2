# PRE-RELEASE CHECKLIST for LunarWing v2.0.8.0

## Codename: Unknown

**Open TODOs (v2.0.7.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)

### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

---

1. [x] update kimi dev loop if havent already - Rares
2. [x] update kimi dev loop if havent already - AJ
3. [ ] work on the following work item - https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/54 — PARTIAL 2026-08-14: OpenRC service cgroup v2 ceilings and non-systemd build memory cgroups are implemented, the 35-check render harness passes, and MR !469's 17-job pipeline passed. Review found correctness gaps and no real OpenRC runtime coverage; keep unchecked until fixes and the host validation below pass.
4. [x] verify that the following work item was completed - https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/50 verify if done — DONE (verified 2026-08-14 on aj/kimi-4-20260814-0001): fix cf4b1164 excludes `ThreadType::Mission` from the conversation-insights `thread_goals` payload + event listener skips Mission threads; e999db6d sorts goals by recency; regression tests `conversation_insight_goals_exclude_mission_threads` and `conversation_insight_goals_use_recency_not_store_order` pass (ic/crates/lunarwing_engine/src/runtime/mission.rs). Independent review found no correctness gaps; all 67 mission tests and MR !468 pipeline 1739 passed. Residual gap: no single integration test asserts the exact generated conversation-insights payload end to end.
5. [ ] verify that the following work item was completed - https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/47 verify if done
6. [ ] investigate if any crates should be upgraded for security reasons. make a new document in docs/ops detailing findings - you may use existing documentation for reference as well as the following work item https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/53 
7. [ ] clean up repo - remove uncessary files - organize files in repo root a bit better - for example - crszsslw2412c09c1-8ea9-46bd-8063-084ccdd2f332.jpg in root of repo - maybe change file name? and move very old documentation to historical archive directory. dont just delete it
8. [ ] implement Publisher signatures dependency locks, marketplace revocation distribution, mandatory sandbox profiles, persistent external-worker placement, a bundled gateway control-plane service, and generic external runtime descriptors for MCP part 2
9. [ ] refactor mt admin - you may use this old branch from 2.0.5.0 era for inspiration - in fact, most of this can be reused likely - ref2/sunburst/mtadmin-refactor-v2.0.6.0
10. [ ] Bump lunarwing crate versions and wasm channel crates for lunarwing that have same version. they are at 2.0.7. we need bump to 2.0.8 - ensure CI/CD is also updated for the new crate versions — lockfiles must be refreshed - all channel registry patch versions must be also be bumped to comply with CI/CD by CI source-version policy. CI/CD contains no hardcoded LunarWing package-version pin and derives versions from Cargo manifests; verify GitLab config validation passes if possible - cargo update and all that good stuff as usual
11. [ ] mcp part 3 work - see existing documentation for details - pick one thing from extensive list and update this goal with what was done. state any work that was deferred or is incomplete. ensure that the whole project builds after you finish your thing - see roadmap for information as well
12. [ ] Additional Opencode External Worker Polishing, including updating the version of opencode to 1.18.18
13. [ ] health service improvements - see recent documents in docs/proposals for more information
14. [ ] Cross-tenant knowledge sharing via coordination pipeline - see existing documentation
15. [ ] Vector embeddings hybrid ranking - see existing documentation
16. [ ] update roadmap in docs/ops with state of each goal
17. [x] (S) fix cicd again - RESERVED
18. [ ] mcp part 3 work - see existing documentation for details - pick one thing from extensive list and update this goal with what was done. state any work that was deferred or is incomplete. ensure that the whole project builds after you finish your thing - see roadmap for information as well - this is NOT a duplicate
19. [ ] come up with codename ideas for release. put into new doc in docs/ops - base the name on all changes made since 2.0.7.0
20. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.8.0 explaining all relevant changes since v2.0.7.0 as well as revising and including an ACCURATE VERSION OF `known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. Keep it brief. The codename for this release is not know yet - The file you write will be `RELEASE-v2.0.8.0.md` and should be written to the ROOT of the repo
21. [ ] update any architecture docs in docs/architecture that may contain outdated information. verify against code rather than docs
22. [ ] update any bugs docs in docs/bugs that may contain outdated information. verify against code rather than docs
23. [ ] update anything outdated in docs/proposals - verify against code rather than docs
24. [ ] update anything outdated in docs/guides - verify against code rather than docs
25. [ ] update anything outdated in docs/internal - verify against code rather than docs
26. [ ] create a fourth document in docs/ops similar in nature to docs/ops/guide-for-spin-up-gentoo-tenants.md and the arch guide and the fedora guide but for debian/ubuntu GNU/Linux hosts
27. [ ] add new health stuff to roadmap and add new ideas discussed to roadmap that are not currently present
28. [ ] update release notes draft in root of repo with anything might have missed that may be relevant
29. [ ] update README.md in repo root (include information related to sandboxed WASM MCPs and any other relevant information that may be missing)

---

