# PRE-RELEASE CHECKLIST for LunarWing v2.0.7.0

**Open TODOs (v2.0.7.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

---

1. [x] investigate if any crates should be upgraded for security reasons. make a new document in docs/ops detailing findings - you may use existing documentation for reference — DONE (2026-08-10): [`CRATE_SECURITY_AUDIT_v2.0.7.0.md`](CRATE_SECURITY_AUDIT_v2.0.7.0.md) records the live 32-root RustSec audit. No newly fixable security bump was found; the seven accepted shipped-graph advisories remain blocked by libSQL's legacy TLS stack, unpatched RSA via russh, or the vendored Signal wrapper.
2. [x] Bump lunarwing crate versions and wasm channel crates for lunarwing that have same version. they are at 2.0.6. we need bump to 2.0.7 - ensure CI/CD is also updated for the new crate versions — lockfiles must be refreshed - all channel registry patch versions must be also be bumped to comply with CI/CD
by CI source-version policy. CI/CD contains no hardcoded LunarWing package-version pin and derives versions from Cargo manifests; verify GitLab config validation passes if possible - cargo update and all that good stuff as usual
3. [x] investigate cause of agent occasionally getting "stuck" after certain tool calls or approval mechanisms and fix if possible - you may spin up test tenant(s) to live debug — DONE (2026-08-10): verified that mission-only `__save_memory_doc__` access is an intentional trust boundary, not a Reflex dependency; fixed its real failure mode by raising explicit errors instead of returning silent `False`. Fixed orchestrator v0 dropping null/missing/interrupted structured results, made denied approvals resolve the original call with an `ActionResult`, and corrected `missing_tool_output` analysis to pair call IDs in the authoritative inference transcript. Added embedded-Monty regressions plus approval/auth matrix coverage. Verified `493/493` engine tests, `engine_v2_channel_delivery_matrix`, formatting, `cargo check`, and zero-warning clippy for both touched crates
4. [x] improve coordination protocol - any negative things preventing full capability should be addressed — DONE (2026-08-10): removed activation and execution parity blockers (derived safe aliases, Engine V2 prompt contract, uniform legacy/Engine auto-disable, complete master switch); added deterministic cross-daemon responder election with explicit-routing and replay safety plus tenant roster persistence; auto-loaded the managed seven-rule protocol bundle; reconciled strike-aware explicit/entropy secret detection and coalescer config safety; refreshed operator/design docs. Verified coordination/bootstrap/gate/coalescer/boundary/Engine tests, tenant shell harness, JSON parity, rustfmt, all-feature cargo check, and all-feature lib clippy
5. [ ] fix cicd.  secret detection is failing now
6. [x] find gaps/issues with mcp part 2 implementations if any. document findings in new document in docs/ops - feel free to use existing documentation for reference — DONE (2026-08-10): [`MCP_PART_2_GAP_AUDIT_v2.0.7.0.md`](MCP_PART_2_GAP_AUDIT_v2.0.7.0.md) verifies the shipped runtime slices and records three P1 plus five P2 gaps, including unsupported advertised client capabilities, worker recovery circuit defects, stdio provenance TOCTOU, incomplete discovery/version/event handling, unverified full MCP-over-WASM interoperability, a production-closed adapter registry, and contradictory status docs. Focused MCP suite passed 368/368.
7. [ ] Additional Opencode External Worker Polishing, including update of opencode version (latest is 1.18.9 )
8. [ ] LunarVision polishing, improving stability across both systemd/gentoo setups
9. [ ] LunarVoice polishing, improving stability across both systemd/gentoo setups
10. [ ] come up with codename ideas for release. put into new doc in docs/ops - base the name on all changes made since 2.0.6.0
11. [ ] go over items in work items ( hint: can use gitlab api to access work items without auth ). make note/document of any that are already confirmed fix and should be closed - put note/doc in docs/ops - check when done - you may use existing documentation for reference
12. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.7.0 explaining all relevant changes since v2.0.6.0 as well as revising and including an ACCURATE VERSION OF `known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. Keep it brief. The codename for this release is: `UNKNOWN` — The file you write will be `RELEASE-v2.0.7.0.md` and should be written to the ROOT of the repo
13. [ ] update any outdated documents in docs/proposals
14. [ ] update any outdated documents in docs/guides
15. [ ] update README.md in repo root (if necessary)
16. [ ] review first draft of release notes from prev step and update any inconsistencies
17. [ ] update readme from prev step and update any inconsistencies
18. [ ] Run cargo update and lockfile reconciliation before tag if necessary
---
