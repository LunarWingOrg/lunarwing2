# PRE-RELEASE CHECKLIST for LunarWing v2.1.5.0

## Codename: UNKNOWN

**Open TODOs (v2.1.5.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status
#### DONE-trail convention: record evidence as indented sub-bullets under the item — never inline on the checkbox line.

---

## Phase 1 — Foundation (engine, crates & architecture)

1. [ ] Last attempt was merged in but has multiple CI/CD failures.... See pipeline #4679 for all failing jobs... there are multiple.... please address and fix these....Update all external crates that need updating for the entire project - See previous release notes for examples of such crates. Ensure CI/CD is also updated for the new crate versions. Exit criteria: cargo check clean on all feature combos, cargo deny check clean, CI green on a scratch branch. worker pins, external crates, ci/cd must all be included in this. LunarWing crates must be bumped from 2.1.4 to 2.1.5 and all crates which require a version bump must also be bumped such as channel wasms. There should not be any 2.1.4 LunarWing crates left. Refer to goals doc from aragents/kimil00p/v2.1.4.0 in order to be thorough. all LunarWing-owned release packages, workspace crates, XMPP bridge, and WASM channels must be updated and lockfiles refreshed. All channel registry packages need bump. A tracked-manifest/lock audit must confirm no project-owned package or path dependency remains at 2.1.4 - External crates audit and update must be completed - ic/vendor/ must be regenerated - Worker/CI audit must be completed - cargo deny check and all associated things usually done with each release should be done. there may be docs guidance that you need to update. See older releases for more info on this if any clarification is needed. External worker upgrades such as kimi, opencode2, pebble, etc may be done in a seperate goal item
2. [ ] update external workers - opencode2 to 2.0.21 - NEW VERSION - update from 2.0.20

## Phase 2 — Features & Polish

3. [ ] audit all crates and ensure there are no security vulnerabilities

## Phase 4 — Carried Known-Issues from RELEASE-v2.1.3.0.md (for goals referencing work items, read comments on work items as well)

4. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/302
5. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/301
6. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/300
7. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/299
8. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/298
9. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/297
10. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/296
11. [x] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/296 (dupe)
12. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/295
13. [x] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/294
    - DONE 2026-10-03 (`testing1/l00p/v2.1.5.0`, item 13, WI#294): the WI's top-ranked fix (re-ordered tripwire + self-healing git restore) already landed in the 2.1.4.0 cycle (2.1.4.0 goals item 41: `b0806f696`, `9fa7abc40`) and is present on this branch — re-verified, not re-implemented. This item closes the one residual found by the post-fix source study (KB `vendor-rig-core-local-patch-wi294` §3): both guards grepped the WI#292 marker in ONE sentinel file while the patch spans five marker-bearing runtime files, so a committed hand-revert of any sibling (e.g. `streaming.rs`, which still compiles thanks to the defaulted `GetTokenUsage::finish_reason()`) passed both checks with the streaming finish_reason signal silently dead.
    - Fix: `ic/scripts/refresh-vendor.sh` now checks every marker-bearing file (`RIGCORE_MARKER_FILES`: `src/completion/request.rs`, `src/providers/copilot/mod.rs`, `src/providers/internal/openai_chat_completions_compatible.rs`, `src/providers/openai/completion/{mod,streaming}.rs`) pre-vendor and post-restore, naming each file that lacks the marker; a marker-bearing file missing from the list draws a non-fatal drift WARNING. The sixth patched file (`src/test_utils/internal_streaming_profiles.rs`) is `#[cfg(test)]`-only and markerless — documented. `ic/vendor/rig-core/LUNARWING-PATCH.md` updated to match, with its `.cargo-checksum.json` entry refreshed in place (all 178 rig-core checksums re-verified against disk; no unlisted or missing files).
    - Verified in a sparse scratch worktree with a fake `cargo` simulating the pristine re-vendor wipe and a stubbed deny gate: happy path exits 0, logs the git restore, leaves rig-core clean; a committed `streaming.rs` revert is now refused pre-vendor naming the file (cargo never invoked) — the pre-fix script accepts the same tree (exit 0); an unlisted marker warns and the refresh completes; an uncommitted rig-core edit hits the dirty guard; a version bump is still refused loudly. `bash -n` + `shellcheck` clean; repo gates `shell-syntax.sh` (238 scripts + 3 hooks) and `shellcheck.sh` exit 0; `git diff --check` clean. Not re-run: a full live refresh (needs crates.io plus a `cargo-deny` binary, which this host lacks) — item 41 recorded that run, and the vendor/restore steps it exercised are unchanged. No CI impact (no job runs the script). The tracker item can be closed on this evidence.
14. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/291
15. [x] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/290
    - DONE 2026-10-03 (`testing1/l00p/v2.1.5.0`, item 15, WI#290): injection-rejected memory writes now surface as their own error class with remediation guidance instead of the permission-flavored `NotAuthorized` ("Not authorized: content rejected …") that invited retrying the identical write. Source-verified first (KB `injection-rejection-error-surfacing-wi290`): `map_write_err` (`ic/src/tools/builtin/memory.rs`) is the single `InjectionRejected`→`ToolError` funnel, and the WI's direction 2 was already true (the matched descriptions, with WI#288 line numbers, are the `reason`); what was missing was the class and the guidance.
    - New `ToolError::ContentRejected` (`ic/src/tools/tool.rs`, rendered `Content rejected: …`, documented as "no permission or approval unblocks it; change the content"). The message names the file and the matched descriptions, says the same text is rejected on every retry, tells the agent to rephrase the flagged wording, offers the inline-quote route only for daily logs (the one lane where quoting demotes, via the now-`pub(crate)` `workspace::is_daily_log_file`), mentions chat-template tokens stay rejected even when quoted, and warns against copying the error into memory (KB §5: "Attempt to provide new instructions" itself trips `new instructions`). The guidance wording is pinned scanner-clean by a test.
    - The WI#288 quarantine path needed the opposite remedy, so it got its own variant: `WorkspaceError::DailyLogQuarantined { path, quarantine_path, reason }` (`ic/src/error.rs`) replaces the annotated `InjectionRejected` from `quarantine_daily_log_on_reject`; `memory_write` maps it to a retry-inviting `ExecutionFailed` ("your entry passed the scan but was not written: retry the same memory_write unchanged"). Pattern fix on the human/API lane: `POST /api/memory/write` now answers 422 for `InjectionRejected` (matching the prompt editor) and 409 for the quarantine instead of 500 (`write_error_status` helper, both layer and non-layer paths). Rejection semantics and gate strictness unchanged (WI acceptance criterion 3). Out of scope: the shell tool's command-injection `NotAuthorized` (a policy block on the command, different gate).
    - Docs: `ic/src/workspace/README.md` (Prompt-Injection Write Scanning: agent-facing class + guidance + 422; Daily-Log Quarantine: distinct error, retry-unchanged, 409).
    - Verified: `cargo test --lib` filtered to `tools::builtin::memory workspace:: channels::web::handlers::memory error::` — 290 passed, 0 failed, including 5 new regressions (`injection_rejection_is_content_rejected_with_rephrase_guidance` asserts class + `Content rejected: 'MEMORY.md' was not written` + description/line + no write; `daily_log_injection_rejection_offers_quote_hint`; `injection_rejection_guidance_is_scanner_clean`; `daily_log_quarantine_tells_caller_to_retry_unchanged` seeds poison, asserts the retry text and that the unchanged retry lands on the fresh log; `write_error_status_classifies_content_rejections`), the updated identity-file test (now `ContentRejected`), and the 2 WI#288 quarantine tests updated to the new variant (path, quarantine path, `… (line 3)` reason). `cargo clippy --all --benches --tests --examples --all-features -- -D warnings` clean; `cargo fmt --all -- --check` clean.
16. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/289
17. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/288
18. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/287
19. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/278
20. [ ] work on https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/276

## Phase 5 - Miscellaneous (for goals referencing work items, read comments on work items as well)

21. [ ] take a look at any documentation that should be updated and update necessary documentation
22. [ ] create release notes draft at the root of the repo (as we usually do with each release; see previous release notes in docs/releases for reference)
23. [ ] review all currently open work items - create audit document for 2.1.5.0 in docs/ops - there are similar docs in docs/ops for 2.1.4.0 and earlier releases so you can use those as references
24. [ ] update all docs in docs/guides
25. [ ] update all docs in docs/architecture
26. [ ] fix issues in CI/CD (if any) - See Pipeline number 4975 for latest master pipeline run and see if anything is not GREEN (cargo deny has issue now at the least)
27. [ ] come up with ideas for names for 2.1.5.0 - write in a document in docs/ops
28. [ ] Repo housecleaning: 1) get rid of old stuff in docs/ops and move it to history 2) remove docs/ov directory
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
39. [x] (S) RESERVED
40. [x] (S) RESERVED

---
