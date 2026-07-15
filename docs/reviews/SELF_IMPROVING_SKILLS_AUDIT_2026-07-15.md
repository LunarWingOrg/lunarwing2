# Self-Improving Skills Audit (B-1, B-2, B-3)

**Date:** 2026-07-15
**Branch:** `project-overview-explore`
**Scope:** B-1 patching, B-2 demotion/pruning, B-3 publishing/updates, and the shared Engine V2 skill-usage feedback foundation
**Method:** Static source inspection of Rust crates (`lunarwing_engine`, `lunarwing_skills`), bridge router, web handlers, and orchestrator Python. The audit itself did not execute builds or tests. It incorporates live E2E evidence from a separately executed test against the `epicmoose` tenant (provisioned, tested, and purged on this branch prior to the audit).

---

## Table of Contents

1. [Executive Verdict](#1-executive-verdict)
2. [Live E2E Evidence (epicmoose tenant)](#2-live-e2e-evidence-epicmoose-tenant)
3. [Shared Foundation: Terminal Usage Feedback](#3-shared-foundation-terminal-usage-feedback)
4. [B-1: Confidence-Driven Patching](#4-b-1-confidence-driven-patching)
5. [B-2: Demotion and Pruning](#5-b-2-demotion-and-pruning)
6. [B-3: Cross-Agent Sharing](#6-b-3-cross-agent-sharing)
7. [Proposal Gate and Scope Findings](#7-proposal-gate-and-scope-findings)
8. [Test Environment Contamination](#8-test-environment-contamination)
9. [Shared Authorization Concern](#9-shared-authorization-concern)
10. [Observability and Crash-Window Notes](#10-observability-and-crash-window-notes)
11. [Corrected Stale Findings](#11-corrected-stale-findings)
12. [Recommendations](#12-recommendations)
13. [Default-Disabled Decision](#13-default-disabled-decision)

---

## 1. Executive Verdict

| Track | Status | Summary |
|-------|--------|---------|
| **Feedback foundation** | Working (Rust-side) | Terminal usage recording is wired, scoped, and lifecycle exactly-once in the tested terminal flow. Live E2E confirmed. The default Python orchestrator does not call `__record_skill_usage__` itself, but the Rust terminal path captures it instead. Crash-transactional exactly-once is not provided (see section 10). |
| **B-1** | Structurally sound, partial gaps | Propose-then-approve, optimistic concurrency, epoch reset, and audit trail are well-designed. Missing: `apply_pending_patch` does not clear `deprecated_at`/`deprecation_reason`, so an auto-demoted skill stays demoted after its patch is applied. |
| **B-2** | Defective lifecycle | Inline demotion fires on `record_usage`. But `is_prune_candidate` requires `deprecated_at.is_none()`, and demoted skills are excluded from activation so their usage freezes. The full state machine (demote-at-5 then prune-at-10) is unreachable: removing the exclusion alone is insufficient because the usage count can never reach 10. |
| **B-3** | Publishing partially functional; update workflow nonfunctional | Publishing is reachable and enforces eligibility and leak-scans, but does not stamp local provenance after a successful push. The registry update workflow is nonfunctional end-to-end: update detection has no production caller, and `apply_update` stamps metadata without fetching or replacing content. Code snippets are outside the patch content/hash scope. Registry contract is untested. |
| **Gate** | Mostly correct, one leak | `__propose_skill_prune__` host function is not gated by `skill_self_improvement_enabled()`, unlike `__propose_skill_patch__` and `__record_skill_usage__`. |
| **Default** | Keep disabled | The subsystem should remain default-off (`SKILL_SELF_IMPROVEMENT` unset) until B-2 lifecycle defects are resolved. |

---

## 2. Live E2E Evidence (epicmoose tenant)

A temporary tenant was provisioned from this branch, exercised through one Engine V2 thread with an activated skill, then verified and purged. No credentials or secrets are reproduced here.

**Observed results:**

| Metric | Before | After | Expected |
|--------|--------|-------|----------|
| `usage_count` | 0 | 1 | 1 |
| `success_count` | 0 | 1 | 1 |
| `failure_count` | 0 | 0 | 0 |
| Thread outcome | — | Done (Completed) | Completed |
| Feedback metadata (`lunarwing.skill_feedback.active_doc_ids`) | Set during thread | Cleared after terminal pass | Cleared |
| Duplicate recording | — | None (lifecycle exactly-once in tested flow) | Lifecycle exactly-once |
| Tenant after test | — | Purged | Purged |

**What this proves:** The Rust-side terminal feedback path (`record_terminal_skill_usage` in `lunarwing_engine/src/runtime/skill_feedback.rs`) is wired into the runtime manager, correctly extracts skill doc IDs from thread metadata, validates scope, records each activated skill exactly once within the tested terminal flow, and clears the metadata key after processing. This confirms lifecycle exactly-once under normal (non-crash) operation. It does not prove crash-transactional exactly-once; see section 10 for the crash-window analysis.

**What this does not prove:** B-2 demotion/pruning and B-3 publishing/update flows were not exercised live. The live test covered terminal usage feedback only.

---

## 3. Shared Foundation: Terminal Usage Feedback

**Source:** `ic/crates/lunarwing_engine/src/runtime/skill_feedback.rs`

The terminal feedback path is the authoritative recording mechanism. It fires after a thread reaches a terminal outcome (`Completed`, `Failed`, `MaxIterations`) in the runtime manager (`ic/crates/lunarwing_engine/src/runtime/manager.rs`).

**Verified strengths:**

- **Lifecycle exactly-once (tested terminal flow):** The `ACTIVE_SKILL_DOC_IDS_METADATA_KEY` is removed from thread metadata via `metadata.remove()` before processing, preventing re-recording on retry or re-entry within the same process lifetime. This was confirmed in the live E2E test. However, this removal happens in memory only; the metric mutations and thread persistence are separate store operations without a transaction (see section 10).
- **Scope enforcement:** `visible_skill_ids` loads all docs visible to the thread's project and user (including shared-owner docs) and rejects doc IDs outside that set. Fails closed on lookup error.
- **Amplification cap:** `MAX_EMITTED_SKILL_IDS = 64` bounds the number of skill IDs processed per terminal pass.
- **GatePaused excluded:** `GatePaused` threads return early without recording, avoiding premature metrics on paused/approval flows.
- **Deduplication:** A local `HashSet` prevents recording the same doc ID twice within one pass.
- **Enabled flag:** The `skill_feedback_enabled` flag (set from `skill_self_improvement_enabled()` at manager construction) gates whether `success`/`failure` is recorded. `Stopped` and `GatePaused` outcomes produce `None` and skip recording entirely.

**Default Python orchestrator:** `ic/crates/lunarwing_engine/orchestrator/default.py` stores `state["active_skill_ids"]` with the selected skill doc IDs but does not call `__record_skill_usage__`. This is not a defect because the Rust terminal path supersedes it. The host function `__record_skill_usage__` remains available for custom orchestrators, but it must be treated as mutually exclusive with terminal feedback for the same activation; combining both paths can double-count.

---

## 4. B-1: Confidence-Driven Patching

**Sources:** `ic/crates/lunarwing_skills/src/v2.rs`, `ic/crates/lunarwing_engine/src/memory/skill_tracker.rs`

### Strengths

- **Propose-then-approve model:** `propose_patch` stages a `PendingSkillPatch` without touching live content or version. The skill continues working until the user approves.
- **Optimistic concurrency:** `base_content_hash` (SHA-256 of content at propose time) is re-checked in `apply_pending_patch`. If the skill changed out-of-band, the apply is refused.
- **Epoch reset:** On apply, live metrics zero out so the patched skill gets a fresh evaluation window. Pre-patch metrics are preserved in `patch_history[].metrics_before` for auditability.
- **Bounded history:** `MAX_PATCH_HISTORY = 20` entries, newest kept, overflow drained from the front.
- **Installed protection:** `propose_patch` refuses `SkillTrust::Installed` (read-only external) skills.
- **Threshold gates:** `is_patch_candidate` requires `usage_count >= 5` and `confidence < 0.5`, preventing premature reactions.

### Gaps

- **Patch does not clear demotion state.** `apply_pending_patch` resets metrics to zero but does not clear `deprecated_at` or `deprecation_reason`. If a skill was auto-demoted (B-2) and then patched, it stays excluded from activation even though its metrics were reset. The user must manually call `undeprecate_skill` to restore it. This breaks the intended recovery loop: demote, patch, epoch-reset, re-activate.

- **Patch content scope excludes code snippets.** `propose_patch` takes `proposed_content` (the prompt body) and a diff, but `code_snippets` in `V2SkillMetadata` are outside the patch payload. A patch cannot fix a broken Python snippet, only the prompt text. The `content_hash` covers `doc.content` (the prompt), not the snippets.

---

## 5. B-2: Demotion and Pruning

**Sources:** `ic/crates/lunarwing_skills/src/v2.rs`, `ic/crates/lunarwing_engine/src/memory/skill_tracker.rs`

### Unreachable State Machine

The B-2 lifecycle has a structural defect that makes the full demote-then-prune progression unreachable for auto-demoted skills.

**The chain of conditions:**

1. **Auto-demotion** fires inline in `record_usage` when `is_demote_candidate` returns true: confidence below 0.3, usage >= 5, source is `Extracted`, not Installed, not authored, `deprecated_at.is_none()`. After firing, `deprecated_at` is set to `Some(now)`.

2. **Prune staging** requires `is_prune_candidate` to return true: confidence at or below 0.0, usage >= 10, source is `Extracted`, not Installed, not authored, `archived_at.is_none()`, and **`deprecated_at.is_none()`**.

3. **Contradiction:** A skill that was auto-demoted at usage 5+ (step 1) has `deprecated_at = Some(...)`. It can never satisfy `deprecated_at.is_none()` in `is_prune_candidate` (step 2), and exclusion from activation prevents it from accumulating further failures. It therefore cannot progress to prune staging through normal runtime behavior.

4. **The only path to pruning** is a skill that somehow reaches 0.0 confidence and 10+ usage without ever triggering the demotion floor at 0.3 / 5+. Given the thresholds (demotion fires at 0.3/5, pruning at 0.0/10), a monotonically worsening skill would hit demotion first, then be excluded from pruning.

**Net effect:** Pruning is effectively dead code for any skill that passes through auto-demotion. It only works for skills that jump straight to 0.0 confidence without crossing 0.3 first (impossible with incremental failure accumulation), or for skills demoted manually via `demote_skill` (which also sets `deprecated_at`, same problem).

### Why Removing the Exclusion Alone Is Insufficient

Simply removing `deprecated_at.is_none()` from `is_prune_candidate` does not fix the lifecycle. Demoted skills are excluded from activation (`is_deprecated` returns true, and the selector skips them). A demoted skill can never be activated in a thread, so its `usage_count` freezes at the point of demotion. The prune threshold requires `usage_count >= 10`, but a skill demoted at 5 uses will never reach 10 through normal operation. The additional exclusion is redundant because the usage gate already blocks progression.

### Recommended Fix Approaches

Three options, alone or in combination:

- **Option A (time-based quarantine / grace period):** Add a configurable grace period since automatic demotion. A demoted skill becomes prune-eligible after the grace period expires without a successful patch or manual reactivation, regardless of usage count. This does not require the demoted skill to accumulate further usage (which it cannot, since it is excluded from activation). Track `deprecated_at` timestamp and compare against the grace period in the prune sweep.

- **Option B (low-rate canary activation):** Retain the 10-use threshold but allow demoted skills to be activated at a low rate (e.g., 1 in N threads) as canaries. If the canary succeeds, the skill earns reactivation; if canaries continue to fail, usage accumulates toward the prune threshold. This is more complex but preserves evidence-based pruning.

- **Option C (patch-outcome model):** Make demotion conditional on patch failure rather than raw confidence. A skill is only demoted if a patch was proposed, applied, and still failed to recover. Prune eligibility then follows from repeated patch-and-fail cycles tracked in `patch_history`, not from raw usage count. This ties the entire lifecycle to the B-1 feedback loop.

### Additional B-2 Notes

- `propose_prune` and `demote_skill` both refuse authored and Installed skills (correct).
- `apply_prune` sets `archived_at` (soft delete, MemoryDoc retained). Never hard-deletes.
- `undeprecate_skill` clears demotion and is available for manual recovery, but nothing calls it automatically after a successful patch (see B-1 gap above).

---

## 6. B-3: Cross-Agent Sharing

**Sources:** `ic/src/bridge/router.rs`, `ic/src/bridge/skill_migration.rs`, `ic/src/channels/web/handlers/skills.rs`, `ic/crates/lunarwing_engine/src/memory/skill_tracker.rs`

B-3 has two sub-workflows: publishing (pushing a local skill to a registry) and updating (pulling a newer registry version). Publishing is reachable from the web API and partially functional. The update workflow is nonfunctional end-to-end. The gaps below are organized by sub-workflow.

### B-3a: Update Detection Has No Production Caller

`propose_update` exists on `SkillTracker` and stages a `PendingSkillUpdate` on the skill metadata. However, no production code path detects that a newer registry version exists and calls `propose_update`. The types and staging logic are present, but the detection sweep (comparing installed `registry_version` / `registry_content_hash` against the live registry) has no caller. A `PendingSkillUpdate` can never be created in production without manual API invocation.

### B-3b: Apply Update Does Not Replace Content

`apply_update` (`skill_tracker.rs`) stamps `registry_version`, `registry_content_hash`, and `pulled_at` on the metadata, bumps the version, and epoch-resets metrics. But it never fetches new content from the registry and never updates `doc.content`. The doc body stays the same. The bridge comment says "the actual pull + content swap is performed by the caller," but no caller in the bridge router performs the pull before calling `apply_update`. The user approves an "update" that changes metadata without changing what the skill actually does.

### B-3c: Publish Does Not Stamp Local Provenance

`publish_skill` in the bridge router loads the skill, checks eligibility, leak-scans, builds a `PublishRequest`, and POSTs to the registry via the catalog client. On success, it returns a `SkillPublishResult` with `skill_id` and `version_id`. But it does not write anything back to the local skill metadata. After publishing, the skill's `registry_url`, `registry_publisher`, `registry_version`, and `registry_content_hash` fields remain whatever they were before. The local doc has no record that it was published, where, or under what slug. A skill published under slug `alice/foo` at version `1.0` has no local provenance linking it to that registry entry.

### B-3d: Registry Contract Untested

No integration test exercises the publish or pull flow against a real or mock registry endpoint. The `SkillCatalog::publish` method's HTTP contract, response parsing, and error handling are untested beyond unit-level serialization tests.

### B-3e: Code Snippets Outside Patch Scope

As noted in B-1, code snippets are not part of the patch content/hash. They are also not covered by the update flow. If a registry update changes a snippet, `apply_update` cannot reflect that change since it does not touch `code_snippets` or `doc.content`.

---

## 7. Proposal Gate and Scope Findings

### `__propose_skill_prune__` Is Ungated

In the orchestrator host function dispatch (`ic/crates/lunarwing_engine/src/executor/orchestrator.rs`), three skill-related ext functions are registered:

| Function | Gated by `skill_self_improvement_enabled()`? |
|----------|-----|
| `__record_skill_usage__` | Yes (passes `enabled` flag) |
| `__propose_skill_patch__` | Yes (early return `false` if disabled) |
| `__propose_skill_prune__` | **No** |

`handle_propose_skill_prune` does not check `skill_self_improvement_enabled()`. When the gate is off, an orchestrator could still stage prune proposals. The downstream `SkillTracker::propose_prune` does enforce authored/Installed protection, but the feature gate itself is bypassed.

### Patch/Prune Host Proposal Scope Checks

Both `handle_propose_skill_patch` and `handle_propose_skill_prune` accept a `doc_id` argument from the orchestrator and pass it to `SkillTracker` without checking whether the doc is visible to the thread's project/user scope. This differs from `handle_record_skill_usage`, which validates the doc ID against `visible_skill_ids` before acting. A malicious or buggy orchestrator could propose patches or prunes for skills outside its scope. The tracker methods do not perform ownership checks themselves: `propose_patch` refuses Installed skills, while `propose_prune` refuses authored and Installed skills.

### Bridge-Level Scope (Correct)

The bridge router's `approve_skill_proposal` and `reject_skill_proposal` correctly use `resolve_owned_skill`, which checks `doc.user_id != user_id && !is_shared_owner(&doc.user_id)`. The web handler layer enforces user identity. This gap is limited to the orchestrator ext-function path.

---

## 8. Test Environment Contamination

Four test sites mutate the `SKILL_SELF_IMPROVEMENT` environment variable globally via `unsafe { std::env::set_var("SKILL_SELF_IMPROVEMENT", "true"); }`:

- `ic/crates/lunarwing_engine/src/memory/skill_tracker.rs` (1 site)
- `ic/crates/lunarwing_engine/src/runtime/mission.rs` (3 sites)

None of these restore the variable to its prior value after the test. Since Rust's default test runner executes tests in parallel within the same process, a test that sets the env var can leak the `true` state to concurrent tests that expect the default `false`. This can cause tests that assert "no demotion when disabled" to fail intermittently or, worse, pass when they should fail. A scoped env-var guard (set on setup, restore on drop) does not fully solve this because the restore can still race with a concurrent test's read under parallel execution.

**Recommendation:** Inject the gate state as a test parameter (pass `enabled: bool` into the functions under test instead of reading the env var directly). This eliminates the shared mutable state entirely. Serial execution (`#[serial]` or `--test-threads=1`) is a fallback if injection is too invasive, but it slows the test suite and does not scale.

---

## 9. Shared Authorization Concern

The shared-skill owner check in `resolve_owned_skill` (`ic/src/bridge/router.rs`) allows any authenticated user to act on a skill whose `user_id` matches `SHARED_OWNER_ID` (`"__shared__"`) or `LEGACY_SHARED_OWNER_ID` (`"system"`).

This means: within a single LunarWing instance and shared project space, any user who can authenticate to the web gateway can approve, reject, publish, or apply updates to any shared skill. There is no per-user ACL on shared-skill mutations. The `is_shared_owner` check confirms the skill is shared, not that the acting user has admin privileges or is the user who staged the proposal.

This concern applies to users sharing one LunarWing instance and project space. It does not imply that one OS-level multi-tenant tenant can mutate another tenant's skills; host-level tenants run in separate processes with separate workspaces and stores. The concern is scoped to the multi-user-within-one-instance model where shared skills are visible across authenticated users.

The publish path requires `CLAWHUB_TOKEN`, which adds a credential gate. The approve/reject/apply paths do not require an additional credential beyond web-gateway authentication.

**Recommendation:** For shared-skill mutations (approve/reject/publish/apply), require admin role or restrict approval to the user who originally staged the proposal.

---

## 10. Observability and Crash-Window Notes

### Tracing coverage

The feedback path logs at `warn!` level for failures (visibility lookup failure, malformed skill IDs, out-of-scope IDs, record_usage errors). The inline demotion in `record_usage` logs at `info!`. This is adequate for diagnosing feedback issues.

### Crash window (not transactionally safe)

`record_terminal_skill_usage` removes the `ACTIVE_SKILL_DOC_IDS_METADATA_KEY` from the in-memory thread metadata, then persists metric mutations via `SkillTracker::record_usage` (separate `save_memory_doc` calls per skill), and the `ThreadManager` persists the thread (with the cleared metadata) afterward. These are independent store operations with no wrapping transaction.

Two crash windows exist:

1. **Crash after metric persistence but before thread persistence:** The skill metrics have been incremented and saved, but the thread still holds the old metadata (with the active doc IDs key) on disk. On recovery, if the thread is retried or resumed to terminal, the terminal sweep will find the key again and re-record usage for the same skills. This can cause double-counting.

2. **Crash before metric persistence:** If the process dies after removing the key in memory but before any `save_memory_doc` call completes, the thread on disk still has the old metadata. Depending on the recovery path, the thread may be retried (re-recording correctly) or abandoned (losing the observation entirely).

This is not crash-safe. The lifecycle exactly-once property confirmed in the live E2E test holds only when the process completes the full terminal pass without crashing. Crash-transactional exactly-once would require an atomic store transaction or a durable observation ID/idempotency ledger spanning the metric updates and thread checkpoint.

### No metrics on proposal staging

There is no tracing or metrics emission when `propose_patch` or `propose_prune` stages a proposal. The user discovers pending proposals only through the web API (`GET /api/skills/proposals`). If the self-improvement mission is staging proposals that the user never sees, there is no log trail.

---

## 11. Corrected Stale Findings

Two claims from earlier analysis have been corrected based on source inspection:

### Runtime recording IS wired

**Earlier claim:** "The default Python orchestrator selects skills and stores `state["active_skill_ids"]` but never calls `__record_skill_usage__`; therefore normal runtime metrics do not progress."

**Corrected:** The Python orchestrator indeed does not call `__record_skill_usage__`. But the Rust runtime manager calls `record_terminal_skill_usage` at thread terminal outcomes, which reads the active skill doc IDs from thread metadata and records usage through `SkillTracker::record_usage`. The doc IDs are placed into thread metadata by the feedback wiring (not by the Python orchestrator's `state["active_skill_ids"]`). Live E2E confirmed usage recording works. The `__record_skill_usage__` host function exists as an alternative per-step path for orchestrators that want it, but the default path does not depend on it.

### Default orchestrator does not double-count

**Earlier claim (implicit):** Having both the host function and the terminal sweep could cause double-counting.

**Corrected:** The default orchestrator never calls `__record_skill_usage__`, so there is only one recording path active in the default production flow: the Rust terminal sweep. No double-counting from competing paths occurs there. A custom orchestrator that invokes the host function and also emits IDs for terminal feedback could double-count unless it makes those paths mutually exclusive. The terminal sweep is lifecycle-once within a single process because it removes the metadata key before processing. Crash-window replay risk is analyzed separately in section 10.

---

## 12. Recommendations

### P0: Fix before enabling

1. **Fix the B-2 unreachable lifecycle.** Removing `deprecated_at.is_none()` from `is_prune_candidate` is insufficient because demoted skills are excluded from activation and cannot accumulate further usage. Implement a time-based quarantine model: a demoted skill becomes prune-eligible after a configurable grace period since automatic demotion with no successful patch or reactivation, without requiring more usage. Alternatively, adopt a patch-outcome model (Option C in section 5) or a low-rate canary activation (Option B) if evidence-based pruning is desired.

2. **Clear automatic demotion on patch apply.** `apply_pending_patch` should clear `deprecated_at` and `deprecation_reason` when the deprecation was created by automatic confidence demotion, since the epoch reset gives the skill a fresh start. Explicit operator demotion should remain intact until explicitly reversed.

3. **Gate `__propose_skill_prune__`.** Add `if !crate::skill_self_improvement_enabled()` to `handle_propose_skill_prune`, matching the pattern in `handle_propose_skill_patch`.

4. **Add scope checks to host proposal functions.** `handle_propose_skill_patch` and `handle_propose_skill_prune` should validate the doc ID against `visible_skill_ids` before calling `SkillTracker`, matching the pattern in `handle_record_skill_usage`.

### P1: Fix before B-3 is usable

5. **Wire update detection.** Add a periodic sweep (mission or cron) that compares installed `registry_version` / `registry_content_hash` against the live registry and calls `propose_update` when a newer version is found.

6. **Fetch and replace content on update apply.** `apply_update` (or its bridge caller) must pull the new content from the registry and update `doc.content` before stamping metadata. Without this, the "update" is cosmetic.

7. **Stamp local provenance after publish.** After a successful `publish_skill` call, write `registry_url`, `registry_publisher`, `registry_version`, and `registry_content_hash` back to the skill metadata so the local doc records where it was published.

8. **Add scope to code snippets in patches.** Extend `PendingSkillPatch` and the hash to cover code snippets, or document that patches are prompt-only and snippets require a separate update path.

9. **Test the registry contract.** Add integration tests for publish and pull against a mock registry endpoint.

### P1: Safety

10. **Fix test env contamination.** Inject the gate state as a test parameter instead of mutating the process environment variable. This eliminates the race entirely. Serial execution is a fallback if injection is too invasive.

11. **Restrict shared-skill mutations.** Require admin role for approve/reject/publish/apply on shared skills, or record the staging user and restrict approval to that user plus admins.

### P2: Polish

12. **Add tracing to proposal staging.** Emit an `info!` log when `propose_patch` or `propose_prune` stages a proposal, including skill name and reason.

13. **Document the default-disabled decision** (this audit serves as that record).

---

## 13. Default-Disabled Decision

**Decision: `SKILL_SELF_IMPROVEMENT` stays default-off (unset).**

Rationale:

- B-2 has a structural defect (unreachable prune lifecycle) that means the safety valve for dead skills does not work as intended. Auto-demoted skills remain retained indefinitely with no path to archival.
- B-1 has a recovery-loop break (patch does not clear demotion) that can permanently sideline skills after a single bad patch cycle.
- The prune proposal host function is ungated, meaning the feature gate does not fully contain B-2 mutations.
- B-3's update workflow is nonfunctional end-to-end (no update detection caller, no content replacement). Publishing is partially functional but lacks provenance stamping.

The terminal usage feedback foundation is sound and verified, but there is no supported configuration that enables only metric feedback while disabling B-1/B-2 automation. The single `SKILL_SELF_IMPROVEMENT` gate controls feedback recording, inline demotion, and the host function hooks together. Enabling it to get feedback metrics also activates the defective B-2 auto-demotion path. The combined gate should stay off until the P0 items are resolved.

If finer control is desired in the future, consider splitting the gate into two flags: one for feedback recording (safe to enable independently) and one for automatic mutation (demotion, patch/prune staging). This would allow metric collection without triggering defective lifecycle paths.

New tenants default `ENGINE_V2=true` but do not default `SKILL_SELF_IMPROVEMENT` on. This is correct and should not change.
