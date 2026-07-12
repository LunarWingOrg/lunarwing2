# B-1: Skills as Self-Improving Procedural Memory

Implement the Hermes-style "skill patching" loop: when a thread fails and the
failure traces back to an active skill, diagnose it, patch the skill's
prompt/code, bump its version, and track the patch — closing the loop
**extracted → used → failed → improved**.

## Key finding: ~80% of the substrate already exists

This is NOT greenfield. Verified in-tree:

- **Data model is ready** (`ic/crates/lunarwing_skills/src/v2.rs`): `V2SkillMetadata`
  already has `version`, `parent_version` (for rollback), `content_hash`,
  `code_snippets`, and `SkillMetrics{usage/success/failure_count, last_used}`
  with a `confidence()` method.
- **Patch mechanics are implemented + tested** (`ic/crates/lunarwing_engine/src/memory/skill_tracker.rs`):
  `SkillTracker::update_skill()` (bumps version, sets parent_version for
  rollback), `rollback_skill()`, and `record_usage(doc_id, success)`.
- **Usage tracking is already wired**: the orchestrator calls
  `record_usage` via `__record_skill_usage__`, and threads emit
  `EventKind::SkillActivated { skill_names }` (orchestrator.rs:1504).
- **The failure trigger already fires**: the mission event listener
  (`runtime/mission.rs:478`) fires `thread_completed_with_issues` on any failed
  thread, with a payload of issues + error_messages, routed to the
  self-improvement mission.

**The two concrete gaps:**

1. `SkillTracker::update_skill()` / `rollback_skill()` have **zero callers**
   outside their own tests — the patch loop is built but never invoked.
2. The self-improvement mission (`prompts/mission_self_improvement.md`) patches
   **prompts / orchestrator / config only** — it has no skill-patching branch,
   and its trigger payload does **not** carry which skills were active in the
   failed thread. So it literally cannot attribute a failure to a skill.

## Design

### 1. Attribute failures to skills (the missing data)
In the event listener's `thread_completed_with_issues` branch
(`runtime/mission.rs`), enrich the payload with the skills that were active in
the failed thread. Source: the thread's `EventKind::SkillActivated { skill_names }`
events (already emitted) → resolve to skill `DocId`s. Add:
```json
"active_skills": [{"doc_id": "...", "name": "...", "version": N}]
```
Only fire the skill-patching path when `active_skills` is non-empty AND an
Error-severity issue exists (mirror the extraction gate's severity check).

### 2. Trigger: confidence-threshold, not per-failure
**Decision:** patching is considered when a skill's **confidence drops below a
threshold**, using the existing `SkillMetrics.confidence()` (success ratio) —
NOT on individual failures. This avoids over-reacting to one-off/environmental
failures and is statistically grounded in data already tracked.

- `record_usage` already maintains success/failure counts per skill.
- Add a check (on skill-usage record, or a periodic sweep) that flags any
  Extracted/Trusted skill whose `confidence() < SKILL_PATCH_CONFIDENCE_THRESHOLD`
  (default e.g. 0.5) with a minimum sample size (e.g. `usage_count >= 5`, so a
  single early failure doesn't trip it).
- A flagged skill + a failed thread that activated it → fire the skill-patching
  path with `active_skills` in the payload.

### 3. Add a skill-patching branch to self-improvement — PROPOSE ONLY
**Decision:** the mission does NOT auto-apply skill patches. It **proposes** a
patch and stages it for user approval (mirroring the memory-maintenance-routines
proposal's propose-diff-then-confirm safety model — consistent safety posture
across both features). No auto-apply, so no auto-rollback-on-N-failures needed.

Extend `prompts/mission_self_improvement.md` with a skills section. When a
below-confidence skill is implicated in a failure:
1. Diagnose whether the skill's **prompt** or a **code_snippet** caused the
   failure (wrong command, missing step, bad API call).
2. Produce a minimal **proposed** patch as a unified diff (one skill, one fix —
   matching the mission's "one fix per issue" rule). Do NOT mutate the live skill.
3. Stage the proposal via a new Monty ext-function
   `__propose_skill_patch__(doc_id, proposed_content, diff, reason)` that writes
   a **pending patch record** (a MemoryDoc of a new `DocType::SkillPatchProposal`,
   or the skill's metadata `pending_patch` field) — NOT a call to
   `update_skill()`. Mirrors the existing `__record_skill_usage__` /
   `__list_skills__` validated-host-function pattern in orchestrator.rs.
4. Surface the pending proposal to the user (web/API + a notification), showing
   the diff, the confidence that triggered it, and the failing thread.

**On approval** (user action via API/UI): the accept path calls the existing
`SkillTracker::update_skill()` (bumps version, sets parent_version, records
patch history). **On reject:** discard the pending proposal, leave the skill and
its version untouched. Rejection is itself a signal (optionally logged).

`rollback_skill()` stays available as a manual user operation, not an automatic
one — since patches only land with explicit approval, automatic rollback is
unnecessary.

### 4. Patch history + version tracking
`V2SkillMetadata` has version/parent_version but no patch log. Add a bounded
`patch_history: Vec<SkillPatch>` field (serde-default for back-compat), each:
`{version, ts, source_thread_id, reason, level}`. Populate it in `update_skill`
(extend the `updater` closure call site) so every patch is auditable and the
user can see *why* a skill changed. Keep it bounded (e.g. last 20).

### 4. Guardrails (non-negotiable, mirror existing mission rules)
- **Only patch Extracted/Trusted skills the failure is attributed to** — never
  touch `Installed` (external, read-only) skills or ones not in `active_skills`.
- Reuse the self-improvement prompt's hard rules verbatim: never modify tests,
  never touch safety/policy/leak-detection code, one fix per issue.
- Patches are content/prompt/code_snippet edits only — no new capabilities or
  trust escalation. A patched skill keeps its trust level.
- Content-hash check: recompute `content_hash` on patch; refuse if the on-disk
  content changed out from under the mission (optimistic concurrency).

## Decisions locked
- **Patch model: propose + require approval.** No auto-apply. The mission stages
  a diff; the user confirms before it goes live.
- **Trigger: confidence-threshold.** Patching is considered when
  `SkillMetrics.confidence()` drops below `SKILL_PATCH_CONFIDENCE_THRESHOLD`
  (with a minimum `usage_count`), not on individual failures.

## Files touched
- `ic/crates/lunarwing_skills/src/v2.rs` — add `SkillPatch` + `patch_history`
  field (serde-default); add a `pending_patch: Option<PendingSkillPatch>` field
  (proposed content + diff + reason + triggering thread/confidence).
- `ic/crates/lunarwing_engine/src/memory/skill_tracker.rs` — record patch
  history in `update_skill`; add `propose_patch` (stage pending) and
  `apply_pending_patch` / `discard_pending_patch` (approval actions); add a
  confidence-threshold flag helper used by the trigger.
- `ic/crates/lunarwing_engine/src/runtime/mission.rs` — enrich the
  `thread_completed_with_issues` payload with `active_skills`, gated on the
  confidence-threshold check.
- `ic/crates/lunarwing_engine/src/executor/orchestrator.rs` — add
  `__propose_skill_patch__` ext-function (mirror `__record_skill_usage__`);
  it stages a proposal, never mutates the live skill.
- `ic/crates/lunarwing_engine/prompts/mission_self_improvement.md` — add the
  propose-only skill-patching branch.
- **Approval surface** (API/UI): endpoints to list pending skill-patch
  proposals, view the diff, and approve/reject. (Onboard/gateway wiring scoped
  as a follow-up if the API lands first.)

## Tests
- v2.rs: `patch_history` + `pending_patch` serde round-trip + bounded truncation.
- skill_tracker.rs: `propose_patch` stages without version bump; `apply_pending_patch`
  bumps version + sets parent_version + appends patch history + clears pending;
  `discard_pending_patch` leaves version untouched; Installed-skill proposal
  refused; content-hash mismatch refused (optimistic concurrency).
- v2.rs / trigger: confidence-below-threshold with `usage_count >= min` flags;
  a single early failure (below min sample) does NOT flag.
- mission.rs: failed thread activating a below-confidence skill produces
  `active_skills` in the payload; healthy or no-skill threads do not.
- Guard test: mission never proposes a patch for an `Installed` skill.

## Build constraints
`taskset -c 0-5 cargo check -j6` / `cargo test -p lunarwing_engine -p lunarwing_skills`.
No full debug builds.

## Phasing
- **Phase 1 (this proposal):** failure attribution + confidence-threshold
  trigger + `__propose_skill_patch__` + pending-proposal storage + patch history
  + approve/reject path + prompt branch. The core propose→approve loop.
- **Phase 2 (defer):** proactive confidence-based demotion/pruning (B-2, a
  separate proposal) and cross-agent sharing (B-3).

## Open questions
- **Threshold values:** `SKILL_PATCH_CONFIDENCE_THRESHOLD` (0.5?) and minimum
  `usage_count` (5?) — tune with real data; expose as config.
- **Approval surface priority:** land the engine loop + API first (proposals
  visible via API), then wire a GUI review panel? Or block on the GUI?
- **Notification:** reuse the existing Gotify/notification path to alert the
  user when a proposal is pending, or purely pull-based (user checks a list)?
