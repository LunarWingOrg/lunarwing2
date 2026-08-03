# PRE-RELEASE CHECKLIST for LunarWing v2.0.4.0

## Codename: Odoshi (威し)

**Open TODOs (v2.0.4.0) — To be done before release**

### Rares = odd (1, 3, 5, 7, ...)
### AJ = even (2, 4, 6, 8, ...)

#### Please check off item before the final commit/push. You may add a note to the item to say if it was completed 100%, failed, or other kind of status

###### (S) is usually noted to denote tasks that can be temporarily deferred

---

1. [x] (test kimidevroutine) This is a test. Create a markdown file called R1.md in docs/ops - Populate it with a list of every file's name that exists in docs/architecture - do not delete it. commit the file. check off item from box as usual
2. [x] (test kimidevroutine) This is a test. Create a markdown file called A1.md in docs/ops - Populate it with list of every file's name that exists in docs/proposals - do not delete it. commit the file. check off item from box as usual
3. [x] Go thru the document: docs/ops/SUNBURST_BUG_LIST.md - you may edit the document - look at each bug in the list and if it was already fixed, mark it as fixed. if it wasnt, mark accordingly — Completed 2026-08-02: audited all 24 bugs, both feature gaps, and the watch item against current source, regression tests, related docs, and Git history; deployment-only uncertainty is marked for live revalidation.
4. [x] take a look at the document here: docs/ops/BUG-005_DWARFSTAR_MEMORY_INCIDENT.md - cross-verify with any other relevant documents/code - make edits to this doc if necessary — DONE 2026-08-02: cross-verified against current source, repository history, focused tests, kernel/Tokio/systemd semantics, and related ops docs. Corrected the unsupported sandbox-escape/fail-closed classification; documented `job_mode=worker` as a direct-job schema default rather than backend proof; qualified generated-unit limits versus inherited/manager controls; corrected the local-worker timeout path, shell byte-capture bound, lazy historical ActionResult repair, and size-first output-safety ordering; disambiguated this report from Sunburst's unrelated BUG-005; and preserved host-only evidence as unreproduced provenance. Process-tree cleanup and per-build resource controls remain open.
5. [x] go thru docs/ops and see if anything needs updating — reconciled active operations docs and the docs index with current commands, service behavior, worker lifecycle, and release-test requirements on 2026-08-02
6. [x] FIX ALL HIGH PRIORITY ISSUES IN DOC HERE: docs/ops/P1_ISSUES.md — completed and verified all eight findings on 2026-08-02
7. [x] Reflex compiler fixes needed: 1) Reflex semantic matching adds network request before normal LLM processing. Commit 69d3ffc. Every unique exact/fuzzy miss calls embedding provider; code checks empty pattern-embedding cache only afterward ( ic/src/agent/reflex.rs:342-360 ). Applies when reflex is enabled with embeddings. Check cache first and add separate default-off semantic switch. 2) Reflex fast path synchronously updates one hot DB row per hit. Response waits for PostgreSQL UPDATE ( ic/src/agent/dispatcher.rs:200-217,  ic/src/db/postgres.rs:1355-1367 ) - The proposed fix is bounded write-behind accumulator - Replace per-request DB update with per-pattern in-memory counters flushed periodically. - Request path records hit without awaiting DB - record() briefly locks a HashMap<(user_id, pattern), PendingHit> and merges: - No task per hit. No async work. Unique keys remain naturally bounded by active reflex patterns. - Background worker flushes every 2–5 seconds. It swaps out current map, releases lock, then sends one batch DB update. -  PostgreSQL updates each pattern once per batch - Multiple LunarWing instances remain safe because increments are additive under PostgreSQL row locking. - Failed flushes merge deltas back into accumulator with exponential backoff. Enforce hard unique-key cap to prevent malformed inputs growing memory. - Graceful shutdown attempts bounded final flush. Losing a few seconds of hit statistics during crash is acceptable because counters only drive telemetry/eviction, not correctness. - Add shared DB trait method such as: async fn bump_reflex_pattern_matches( &self, hits: &[ReflexPatternHit], ) -> Result<(), DatabaseError>; PostgreSQL uses UNNEST; libSQL uses one transaction with repeated prepared updates. Avoid: wrapping current update in tokio::spawn. That removes response latency but preserves row-lock storm and creates potentially unbounded tasks. Result: one row lock per pattern per flush interval instead of one per request, while response path becomes DB-independent.
8. [x] Importing via kawarimi adapter leaves PostgreSQL running on failures. Commit 51a3393. After start-pg, populate failure or final start failure returns without stop-pg (hermes_kawarimi/loader.py:177-198 ). Repeated failed imports accumulate persistent PostgreSQL services. Add ownership-based cleanup for all error paths.
9. [x] FIX: Execution stop waits for all parallel siblings. Commit d0d0a04. First ExecutionStopped only sets a flag; all other action/LLM futures are awaited ( ic/crates/lunarwing_engine/src/executor/orchestrator.rs:1487-1512 ,  structured.rs:258-283 ,  scripting.rs:663-729 ). Slow sibling prolongs stop and retains thread/VM state. Abort and drain remaining work immediately.
10. [x] Kimi provider API token is optional; bridge bearer authentication is not. `KIMI_API_KEY` may be absent for OAuth-backed setups, while `AGENT_AUTH_TOKEN` remains required in server mode. Compose now rejects a missing bridge token before container launch instead of entering a restart loop. Regression tests cover missing provider API key and missing bridge bearer token.
11. [x] Execution stop waits for all parallel siblings. Commit d0d0a04. First ExecutionStopped only sets a flag; all other action/LLM futures are awaited ( ic/crates/lunarwing_engine/src/executor/orchestrator.rs:1487-1512 ,  structured.rs:258-283 ,  scripting.rs:663-729 ). Slow sibling prolongs stop and retains thread/VM state. Abort and drain remaining work immediately. — Duplicate of item 9; resolved by the same merged stop-aware sibling-abort implementation.
12. [x] Work on Gitlab issues: Every GitLab call scans all workspace paths. Commit 2870e35. WASM preload calls workspace.list_all() across every read scope, then filters for one config file ( ic/src/tools/wasm/wrapper.rs:177-210 , 1450-1461). Large workspaces create severe per-call DB latency outside tool timeout. Read  config/gitlab.json directly. Additionally, Workspace append adds full-document memory copies on common log paths. Commit caa13d4. append() constructs a full candidate even when prompt validation is a no-op, then retains returned full content while reindex fetches another copy (
ic/src/workspace/mod.rs:848-871 , 984-1017, 1791-1795). Large growing files can hold several complete copies. Build candidate only for prompt-visible paths and drop buffers before reindex. previous salvaged branch: rarity/kimi-11-20260802-0301 (POSSIBLE THIS WAS ALREADY FIXED BUT MUST CHECK AND MAKE SURE) — Completed 2026-08-03: bundled GitLab tools now read `config/gitlab.json` directly while other WASM prefix grants retain existing scan semantics; append paths keep combined prompt-visible validation while releasing full-document buffers before reindex. Focused WASM, append concurrency, runtime indexing, format, and compile checks passed.
13. [x] check for issues in CI/CD and fix anything. One issue currently is shown in here docs/ops/CICD_FAILURE_INVESTIGATION_ITEM13.md
14. [x] MCP additions — host-local MCP lifecycle Part 1 completed 2026-08-03. ExtensionManager now provides whole-runtime stdio crash recovery with fresh owner-scoped secret resolution, atomic client/tool replacement, bounded backoff, visible recovery state, generation-safe lifecycle serialization, and supervisor-first shutdown. Package installation/provenance, host sandboxing, worker-local placement, general runtime adapters, MCP-over-WASM, and dedicated/shared gateway architecture remain deferred. See [`MCP_OVERALL_STATUS.md`](MCP_OVERALL_STATUS.md). - see note at bottom too 
15. [ ] **DarkIRC secure staged key exchange.** H1/H2 fixed and verified; H3 partially fixed, with live isolation and leak evidence still required. Full boundary, audit ledger, validation, blockers, and next steps: [`DARKIRC-KEYEXCHANGE-ITEM15-STATUS.md`](DARKIRC-KEYEXCHANGE-ITEM15-STATUS.md).
16. [x] continue to work on external workers, in particular for kimicode - work on this branch seems SOLID - kimicode-toolchain-
17. [ ] self healing capability expansion - see prev attempt for reference: aj/kimi-24-20260801-1203
18. [ ] come up with proper plan for testing coordination stuff added since 2.0.3.0
19. [x] (S) sweetielist
20. [ ] Suggest additional UI fixes a new document in docs/ops — update`docs/ops/UI_FIX_SUGGESTIONS.md` with any additional findings
21 [ ] update any architecture docs in docs/architecture that may contain outdated information. verify against code rather than docs - prev attempt that may be referenced as it was fairly accurate at time of creation: rarity/kimi-31-20260801-0601
22. [ ] update any bugs docs in docs/bugs that may contain outdated information. verify against code rather than docs
23. [ ] update README.md at repo root
24. [x] come up with codename ideas for release. put into new doc - decided Odoshi (威し)
25. [ ] Write up FIRST DRAFT release notes (at root of repo) for v2.0.4.0 explaining all relevant changes since v2.0.3.0 as well as revising and including an ACCURATE VERSION OF`known issues list`. Use previous release notes in docs/releases for reference as to how to write up this document. The codename for this release is:`UNKNOWN` — The file you write will be`RELEASE-v2.0.4.0.md` and should be written to the ROOT of the repo. When complete with draft, move on to another task, which is: Improve accuracy of the first draft of release notes (from previous task) -`RELEASE-v2.0.4.0.md`
26. [ ] update anything outdated in docs/proposals
27. [ ] update anything outdated that is in docs/ops
28. [ ] update anything outdated in docs/guides - there are a ton of guides in here which have not been updated since v1. many are no longer relevant. they can be placed into archive: docs/guides/history
29. [ ] review release notes draft and update anything inaccurate or changed since time of writing first draft (referencing item #21)


___ 


Item 14 completion hardening (2026-08-03): host-local stdio recovery now runs one
cancellable task per server, permits five unstable restart attempts with jittered
bounded backoff, resets attempts after 30 healthy seconds, and opens a visible
circuit instead of restart-storming. Manual lifecycle work cancels stale recovery;
activation rollback, persistence-before-teardown, atomic tool replacement, fresh
secret resolution, and deadline-bounded error-path shutdown close resurrection and
resource-leak races. Scope remains Part 1 host-local lifecycle only; package
installation/provenance, host OS sandboxing, worker placement, generic runtime
adapters, MCP-over-WASM, and dedicated/shared gateway work remain deferred. See
[`MCP_OVERALL_STATUS.md`](MCP_OVERALL_STATUS.md) for concise current status,
remaining validation, and deferred work.

---
