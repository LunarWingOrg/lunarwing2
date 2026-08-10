# PRE-RELEASE CHECKLIST for LunarWing v2.0.7.0

**Open TODOs (v2.0.7.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

---

1. [x] investigate if any crates should be upgraded for security reasons. make a new document in docs/ops detailing findings - you may use existing documentation for reference — DONE (2026-08-10): [`CRATE_SECURITY_AUDIT_v2.0.7.0.md`](CRATE_SECURITY_AUDIT_v2.0.7.0.md) records the live 32-root RustSec audit. No newly fixable security bump was found; the seven accepted shipped-graph advisories remain blocked by libSQL's legacy TLS stack, unpatched RSA via russh, or the vendored Signal wrapper.
2. [x] Bump lunarwing crate versions and wasm channel crates for lunarwing that have same version. they are at 2.0.6. we need bump to 2.0.7 - ensure CI/CD is also updated for the new crate versions — lockfiles must be refreshed - all channel registry patch versions must be also be bumped to comply with CI/CD
by CI source-version policy. CI/CD contains no hardcoded LunarWing package-version pin and derives versions from Cargo manifests; verify GitLab config validation passes if possible - cargo update and all that good stuff as usual
3. [ ] investigate cause of agent occasionally getting "stuck" after certain tool calls or approval mechanisms and fix if possible - you may spin up test tenant(s) to live debug - Root Cause Identified — TWO related issues found by Ruffles' self-improvement engine and Kageho's diagnostics: (1) `__save_memory_doc__` host-side defect — the `__save_memory_doc__` host function (`ic/crates/lunarwing_engine/src/executor/scripting.rs:1884`) returns `False` for ALL non-mission threads (`thread.thread_type != ThreadType::Mission`). This silently blocks orchestrator v1 activation AND Reflex compiler persistence — both systems depend on the memory doc save primitive to persist state but never receive an error, just `Bool(false)`. Kageho confirmed this same defect affects both systems. Fix: either allow orchestrator v1/reflex threads to use the save path, or have callers treat `False` as an error instead of silently ignoring it. (2) `missing_tool_output` — orchestrator v0 swallows `None`/error tool results. The engine V2 path (`resolve_tool_completion` at scripting.rs:1697) handles errors correctly — pushes to `action_results` with `is_error: true` and raises Python exception. But the legacy v0 path may handle results differently, causing tool results to vanish from the model's context. This is what the trace detector at `executor/trace.rs:288-297` flags as `missing_tool_output`. Fixing `__save_memory_doc__` should unblock orchestrator v1 activation, Reflex compiler persistence, and reduce agent "stuck" incidents simultaneously
4. [ ] improve coordination protocol - any negative things preventing full capability should be addressed
5. [ ] find gaps/issues with mcp part 2 implementations if any. document findings in new document in docs/ops - feel free to use existing documentation for reference
6. [ ] Additional Opencode External Worker Polishing, including update of opencode version (latest is 1.18.9 )
7. [ ] LunarVision polishing, improving stability across both systemd/gentoo setups
8. [ ] LunarVoice polishing, improving stability across both systemd/gentoo setups
9. [ ] come up with codename ideas for release. put into new doc in docs/ops - base the name on all changes made since 2.0.6.0
10. [ ] go over items in work items ( hint: can use gitlab api to access work items without auth ). make note/document of any that are already confirmed fix and should be closed - put note/doc in docs/ops - check when done - you may use existing documentation for reference
11. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.7.0 explaining all relevant changes since v2.0.6.0 as well as revising and including an ACCURATE VERSION OF `known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. Keep it brief. The codename for this release is: `UNKNOWN` — The file you write will be `RELEASE-v2.0.7.0.md` and should be written to the ROOT of the repo
12. [ ] update any outdated documents in docs/proposals
13. [ ] update any outdated documents in docs/guides
14. [ ] update README.md in repo root (if necessary)
15. [ ] review first draft of release notes from prev step and update any inconsistencies
16. [ ] update readme from prev step and update any inconsistencies
17. [ ] Run cargo update and lockfile reconciliation before tag if necessary
18. [ ] check cicd of master for failures before beginning release process (after testing phase completed)

---
