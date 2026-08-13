# PRE-RELEASE CHECKLIST for LunarWing v2.0.8.0

## Codename: Unknown

**Open TODOs (v2.0.7.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)

### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

---

 1. [ ] investigate if any crates should be upgraded for security reasons. make a new document in docs/ops detailing findings - you may use existing documentation for reference
 2. [ ] clean up repo - remove uncessary files - organize files in repo root a bit better - for example - crszsslw2412c09c1-8ea9-46bd-8063-084ccdd2f332.jpg in root of repo - maybe change file name?
 3. [ ] refactor mt admin - you may use this old branch from 2.0.5.0 era for inspiration - in fact, most of this can be reused likely - ref2/sunburst/mtadmin-refactor-v2.0.6.0
 4. [ ] Bump lunarwing crate versions and wasm channel crates for lunarwing that have same version. they are at 2.0.7. we need bump to 2.0.8 - ensure CI/CD is also updated for the new crate versions — lockfiles must be refreshed - all channel registry patch versions must be also be bumped to comply with CI/CD by CI source-version policy. CI/CD contains no hardcoded LunarWing package-version pin and derives versions from Cargo manifests; verify GitLab config validation passes if possible - cargo update and all that good stuff as usual
 5. [ ] mcp part 3 work - see existing documentation for details - pick one thing from extensive list and update this goal with what was done. state any work that was deferred or is incomplete. ensure that the whole project builds after you finish your thing - see roadmap for information as well
 6. [ ] Additional Opencode External Worker Polishing, including updating the version of opencode to 1.18.18
 7. [ ] health service improvements - see recent document in docs/proposals for more information
 8. [ ] Cross-tenant knowledge sharing via coordination pipeline - see existing documentation
 9. [ ] Vector embeddings hybrid ranking - see existing documentation
 10. [ ] update roadmap in docs/ops with state of each goal
 8. [x] (S) fix cicd
 9. [x] (S) LunarVoice/Lunarvision polishing
10. [x] (S) investigate cause of agents getting "stuck" in some kind of loop after accepting that they will perform a tool call and sometimes requiring multiple prompts in order to get them to use that tool (you may spin up a test tenant for this investigation) - fix the underlying issue if possible
11. [ ] come up with codename ideas for release. put into new doc in docs/ops - base the name on all changes made since 2.0.7.0
12. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.8.0 explaining all relevant changes since v2.0.7.0 as well as revising and including an ACCURATE VERSION OF `known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. Keep it brief. The codename for this release is not know yet - The file you write will be `RELEASE-v2.0.7.0.md` and should be written to the ROOT of the repo
14. [ ] update any architecture docs in docs/architecture that may contain outdated information. verify against code rather than docs
15. [ ] update any bugs docs in docs/bugs that may contain outdated information. verify against code rather than docs
16. [ ] update anything outdated in docs/proposals - verify against code rather than docs
17. [ ] update anything outdated in docs/guides - verify against code rather than docs
18. [x] (S) fix cicd again - confirmed a test-only race between the default 500-million fuel budget and the one-second epoch deadline. Under CI load, Wasmtime could legitimately report epoch interruption before fuel exhaustion, making the fuel-specific assertion flaky even though both runtime limits worked. The fuel regression now uses a deliberately small 10-million fuel budget and a 60-second epoch deadline, isolating the failure mode without changing production limits. The other failed master job (`worker-kimi-image`) passed in the duplicate same-SHA merge-request pipeline, as did `rust-test-lib`; its protected trace returned HTTP 401, so no unsupported image change was made. Verified the exact fully qualified regression, all 13 MCP-over-WASM transport tests, rustfmt, and diff checks; local Cargo required `--ignore-rust-version` because the host has Rust 1.95.0 while the repository MSRV is 1.96.1.
19. [ ] update anything outdated in docs/internal - verify against code rather than docs
21. [ ] create a fourth document in docs/ops similar in nature to docs/ops/guide-for-spin-up-gentoo-tenants.md and the arch guide and the fedora guide but for debian/ubuntu GNU/Linux hosts
24. [ ] add new health stuff to roadmap and add new ideas discussed to roadmap that are not currently present
20. [ ] update release notes draft in root of repo with anything might have missed that may be relevant — DONE (2026-08-13): updated RELEASE-v2.0.7.0.md with everything landed since the 2026-08-12 first draft: LunarVoice supervision parity across systemd/OpenRC (item #9, `8091` health listener, launch-contract force-recreate, `-sup` supervisors), GitLab WASM tool `all_pages` single-approval pagination plus blank/repeating pagination-header loop fix (item #12, registry 0.1.2), conversation-insight recency sort (item #13), opencode 1.18.9 worker update on the canonical `anomalyco/opencode` repo (item #6), and the ShellCheck SC2218 CI fix (item #8). Compatibility table gained the LunarVoice health row; upgrade notes now cover LunarVoice `render-units` and health gating; known issues gained verified-open work items #40 (image tool `/v1/v1` path), #42 (XMPP bridge image attachments), and #47 (WASM channel tool-activity events), and the first-draft "open items" section now records all three as resolved. Docs-only change; no cargo verification required
23. [ ] update README.md in repo root (include information related to sandboxed WASM MCPs and any other relevant information that may be missing)



---