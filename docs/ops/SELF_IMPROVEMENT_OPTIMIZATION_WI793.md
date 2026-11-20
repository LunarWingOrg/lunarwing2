# WI#793 — Self-Improvement Optimization Opportunities

**Date:** 2026-11-20
**Source revision:** `9c96220bb2c3711774612a71a23e094d49b788ab`
**Scope:** Engine V2 self-improvement, skill extraction/feedback/patching, prompt overlays, and runtime orchestrator patching.

## Scope and confidence

This is a **static source audit**. It did not inspect live tenant data, run a tenant workload, reproduce a race, or claim that any recommendation below is already implemented. Line numbers locate the current branch; the function names are the stable references after later edits.

The useful next step is not another generation path. It is closing the loop from **observation → evidence → candidate change → controlled activation → measured outcome → retention or rollback**. Items marked **verified** follow directly from the current source. **Design** means an optimization idea whose benefit still requires a replay or live comparison.

Existing B-1/B-2/B-3 design documents and the 2026-07-15 audit describe older states. In particular, automatic demotion recovery after a patch, quarantine-based pruning, registry content replacement, registry provenance, proposal scope checks, shared-owner authorization, and the daily registry update sweep are now present. Do not reopen them from the old audit without reproducing a current defect.

## Executive summary

| Priority | Finding | Kind | Main benefit |
|---|---|---|---|
| P0 | Prompt-overlay writes and reads do not share one identity/update contract | Verified | Stop teaching rules that can be lost, overwritten by UUID order, or left dormant |
| P0 | Orchestrator fallback records the wrong version and lacks per-version counters | Verified | Prevent a bad patch from poisoning a healthy fallback and produce real rollback evidence |
| P0 | Skill injection claims credential-bearing authority in prompt prose instead of stating an explicit, verified contract | Verified design gap | Make skills composable, measurable instructions instead of capability claims |
| P1 | Every activated skill receives the whole thread's outcome | Existing approximation | Reduce false patching/demotion; establish a no-skill and per-skill baseline |
| P1 | Skill changes cannot be restored as content, and patches omit snippets/activation | Verified | Make an approved experiment reversible and cover the behavior that can actually change |
| P1 | The core loop has no experiment ledger, budget metrics, or negative/rejected controls | Verified product gap | Demonstrate improvement rather than equating activity with learning |
| P1 | Extraction turns a compact summary into a durable procedure without transcript fidelity | Verified | Eliminate fabricated or one-off skills and make evidence auditable |
| P1 | Trigger identity, evidence previews, and concurrent writes can lose or repeat learning work | Verified | Preserve evidence and reduce duplicate token/DB cost |
| P2 | Broad selectors, repeated scans, non-scheduled maintenance, and silent collection failures bound throughput and observability | Verified / design | Make the subsystem cheaper and diagnosable at library scale |

## Keep these current controls

These are useful foundations, not reasons to rewrite the subsystem:

- Deterministic skill selection and injection are separate from ordinary memory retrieval (`orchestrator/default.py:624-736`; `memory/retrieval.rs:37-104`). Retrieval excludes skills and engine-control records.
- Terminal feedback has stable document IDs, project/user visibility checks, per-pass deduplication, a 64-ID cap, and no observation for stops, gate pauses, or budget exhaustion (`runtime/skill_feedback.rs:15-112`).
- `MaxIterations` currently counts as a skill failure (`skill_feedback.rs:63-73`). Treat it as ambiguous in future evaluations: budget exhaustion was already excluded, but iteration exhaustion can also reflect an underspecified task or an unrelated model/runtime problem.
- Skill patch/prune/update operations remain gated and propose-then-approve. Patches use a prompt content hash, preserve bounded pre-patch metrics, and clear only automatic demotion on apply (`memory/skill_tracker.rs:154-291`; `lib.rs:26-44`; bridge `router.rs:5777-5907`).
- Role-token neutralization and untrusted-memory labeling remain necessary. None of the recommendations below should bypass approvals, visibility checks, the execution gate, or secret scanning.

## P0 — Repair the representation and versioning foundations

### 1. Give the prompt overlay one identity, a typed revision history, and an atomic update path

**Verified current behavior**

`process_self_improvement_output` appends to the first matching overlay document (`runtime/mission.rs:5136-5211`). Both mission prompts instead tell the model to read and save an updated overlay through `__save_memory_doc__` (`prompts/mission_self_improvement.md:20-28`; `prompts/mission_expected_behavior.md:26-36`).

That host function creates a new document with a new ID (`scripting.rs:2294-2414`). The bridge maps every such title to the same workspace path, but records the path under each distinct ID (`store_adapter.rs:1390-1443,1466-1489`). Until reload, the reader retains both in-memory docs; after reload, both IDs share one file, so only the last writer's content survives and earlier appended rules are lost. Before that, shared listing sorts by UUID and prompt extraction takes the first matching document (`traits/store.rs:161-171`; `executor/prompt.rs:182-201`), which may be the stale copy.

**Impact:** the JSON append path is safe when enabled, but repeated interface-driven overlay saves can select an old/stale document arbitrarily, accumulate invisible memory records, and overwrite the same durable path. After a restart, a rule that appeared active can silently disappear. This undermines every subsequent prompt experiment.

**Recommendation**

- Resolve the canonical overlay by explicit title + tag identity. Return its existing `DocId` on a protected host-function update, or implement a compare-and-swap store method over `(project_id, stable_path, expected_revision)`.
- Store a small, bounded revision record: rule ID, source mission/failure fingerprint, source thread, previous content hash/security revision, creation timestamp, status, and outcome counts. Keep the existing 4,000-character prompt injection cap.
- Coalesce exact duplicate rules instead of appending them. Load an explicit active revision, not whichever document UUID sorts first. Migrate legacy duplicates by an audited mapping; never “guess newest” from random UUIDs.
- Until this is fixed, prefer the host-owned JSON append path over asking a mission to create another overlay document. Do not claim that reading with `memory_search` fixes identity; explicit retrieval can return multiple candidates.

**Acceptance:** two overlay saves through both interfaces leave one active identity; a restart chooses the same rules; simultaneous saves cannot silently discard one; a superseded rule can be restored without reconstructing it from Git or transcript text.

### 2. Make orchestrator fallback version-specific and truthful

**Verified current behavior**

`load_orchestrator_from_docs` chooses the newest non-blocked version (`executor/orchestrator.rs:204-268`). The failure store nevertheless has only one `{version,count}` value, and reads only `count`, not its recorded `version` (`orchestrator.rs:271-350`). `ExecutionLoop::run` calls fallback accounting after an orchestrator error, then emits `from_version: current, to_version: current-1` even when selection can skip several versions or fall back directly to v0 (`loop_engine.rs:461-552`).

A successful, budget-limited, or max-iteration run under v0 also resets the entire failure counter (`loop_engine.rs:559-575`), even if that counter belongs to another runtime version. Only an execution `Err` or an outcome explicitly typed `Orchestrator` increments it. The reset/failure read-modify-write operations have no compare-and-set or revision guarantee.

**Impact:** fallback is not a reliable per-version experiment ledger. A healthy fallback can erase evidence that condemned a patch; unrelated load/runtime failures can demote a patch; the rollback event can name a version that was never selected.

**Recommendation**

Keep this optimization independent of skill machinery:

- Key failure state by the exact candidate `DocId`/content hash, not by a single numeric count. Decide narrowly which failure classes are causal evidence; external/user/budget outcomes should not count against a patch.
- Do not clear a rejected candidate's counter because v0 or a different version succeeded.
- Resolve fallback before execution and record it in a typed event: candidate, attempted versions, selected version, failure class, count threshold, and actual destination. Distinguish “rejected” from “rolled back during a run.”
- Serialize counter updates with a store revision/CAS. Keep v0 as a final safe floor; do not automatically execute unbounded historical versions just because the latest failed.

**Acceptance:** v2 fails three times, v1 runs successfully, and a later load still skips v2 without losing the evidence; choosing v0 reports v0, not v2-1. Three unrelated tool/LLM failures do not remove a candidate. Concurrent counters cannot lose a failure. A restart-after-failure test covers the same decision as the normal path.

### 3. Replace skill capability folklore with a typed, bounded contract

**Verified current behavior**

The scorer sorts only by descending score, with ties inherited from the unsorted `HybridStore` map (`orchestrator/default.py:669-696`; `store_adapter.rs:2371-2383`). A positive substring match is enough for activation, including generic tags, and the declared `max_context_tokens` is charged rather than the actual body size. There is no explicit minimum relevance threshold.

Injection then asserts for every active skill that it has “API access with automatic credential injection,” tells the model not to search for tools, and directs it to HTTP (`default.py:699-736`). The advertised snippet section lists only name/description, not snippet source (`default.py:718-724`). The engine passes only `skill_snippet_names` into `execute_code_with_skills` (`scripting.rs:467-580`); those names become `known_actions` and can then fall through to ordinary tool dispatch (`scripting.rs:607-610,787-807,1088-1105`). There is no snippet-body injection in that path.

**Impact:** a learned markdown procedure can make a false capability claim, a supposedly callable function can really be an unavailable tool, and a stable-ID collision can alter the record used for feedback. These effects contaminate both user outcomes and the metrics that should explain them. Labeling installed-skill content as suggestions does not remove the unconditional credential claim that follows it. Host-side credential injection, tool leases, and approval gates remain the actual authority boundary; the defect is that the prompt misrepresents that boundary and steers the model down the wrong path.

**Recommendation**

- Model distinct contracts: instructional guidance; a catalog-provided callable tool; an approved credential/domain mapping. Ordinary prose must not imply either of the latter two. Never present a callable unless it resolves to a real sandboxed function or an existing leased tool.
- Represent untrusted skill text as reference instructions. Do not convert it into network, credential, or approval authority; preserve host-side allowlists and approval policies.
- Validate snippet syntax and execution against Monty's actual binding contract before advertising it, or remove unsupported snippets from the advertised callable surface. Never route an unresolved SKILL.md helper silently through `http`/credentials.
- Rank by minimum relevance, then deterministic score/doc-ID tie-break. Measure injected chars/tokens, not just a self-declared estimate; retain the top-three and 4,000-token ceilings as conservative starting limits.
- Handle same-name/different-content collisions as explicit conflicts. A UUID is not a skill semantic identity, and two unrelated tools must not inherit each other's guidance.

**Acceptance:** injected text for a plain-text extracted skill makes no credential claim; an unbound helper is never advertised as callable; an explicitly registered helper executes in the intended sandbox; 100 unrelated and three relevant candidate skills deterministic-select only the latter without blowing the injected budget. Add a name-collision test and an untrusted-skill/prompt-injection test.

## P1 — Turn metrics into evidence, not just counters

### 4. Separate activation, usage, attributable failure, and per-version outcome

**Verified current behavior / approximation**

At a terminal thread, every unique activated ID visible to the user gets the same success/failure boolean (`runtime/skill_feedback.rs:45-112`; `memory/skill_tracker.rs:30-107`). Failure patch discovery instead reconstructs identity from display names and returns the *current* doc/version/metrics (`runtime/mission.rs:4295-4377`), not the content version and content hash actually injected.

Activation from `__emit_event__` replaces the metadata ID set rather than accumulating it (`executor/orchestrator.rs:1954-2035`; `default.py:1380-1384`). `__record_skill_usage__` remains separately callable with any visible ID (`orchestrator.rs:2338-2379`). The default Python does not call it, but custom orchestration must not use both paths to credit the same activation. Read-back, loss of one metric write, and concurrent read-modify-write are not covered by a transaction.

**Recommendation**

- Preserve `SkillActivation {doc_id, injected_content_hash, injected_version, snippet_ids/hashes}` at selection time. Use that immutable identity in the terminal ledger and failure payload. Do not use names as a transport identity.
- Count **activation**, **explicit use**, **attributed contribution**, and **uncertain** separately. A thread with a relevant skill but no used procedure should not be reported as a success caused by that skill.
- Start conservatively: co-activated skills are *candidates*, not automatically responsible for a failure. Require tool/error/step evidence before a patch or demotion decision. The existing `Used` tri-state for stopped/budget cases is a useful beginning, not causal attribution.
- Deduplicate by activation ID across default and custom recorders, put observations behind an idempotency key `(thread_id, activation_id, outcome_epoch)`, and define resume/retry semantics before adding more writes.
- Record counter writes with revision/retry information. A partial metric save should be visible as incomplete and never turn a healthy fallback into a demonstrated failure.

**Acceptance:** two co-activated skills can receive different evidence classes; a new version cannot inherit outcomes from the old version; an unknown name cannot redirect feedback; gate resume and both recording paths produce exactly one observation; injected failure after one write cannot be silently presented as complete.

### 5. Make every skill change a reversible full-content proposal

**Verified current behavior**

`propose_patch` covers only `doc.content` (`memory/skill_tracker.rs:154-211`). Approval replaces that content and resets live metrics, but snippets and activation are not included in the patch's optimistic-concurrency hash (`skill_tracker.rs:220-291`). In contrast, registry updates now replace prompt, activation, and snippets together (`skill_tracker.rs:457-512`).

`rollback_skill` decrements only a version number and explicitly leaves content restoration to a caller (`skill_tracker.rs:596-631`). The bounded patch history stores reason and metrics, not the prior executable content (`skill_tracker.rs:252-280`; `lunarwing_skills/src/v2.rs:246-294`). Sequential proposal is accepted broadly when a document is non-Installed; it is not itself proof that source/name/version/approval still matches the originally implicated skill.

**Recommendation**

- Use a canonical, bounded full-content snapshot for proposals: prompt, activation, snippets, trust/provenance, version, and base hash. Exclude generated metric timestamps from semantic identity.
- Include activation and snippet changes in the diff, leak/policy review, optimistic concurrency check, approval, and evaluation epoch. No automatic trust or capability expansion may ride along with a content fix.
- Retain immutable prior content by document/version, not only an integer `parent_version`. An explicit approved rollback is a new version pointing to an older hash, not a counter decrement with stale body text.
- Conditional/atomic writes are required when several metadata fields, history, or evidence records change together.

**Acceptance:** patch a failing snippet, reject drift in any covered field, approve and then roll back, and observe the exact previous content restored. Concurrent approval or an external edit leaves a pending proposal or a conflict, never a partially applied result. Validate legacy records through serde defaults.

### 6. Add a minimal experiment ledger before adding more autonomy

**Verified product gap**

Current counters answer “what happened when a skill was present?” They do not answer “was the new rule/code an improvement over the previous behavior?” Replay tests verify mechanics, not causal benefit. There is no linked before/after record for overlays, no structured experiment score for extraction acceptance, and no automatic rejection/negative-control concept shared across all three learning surfaces.

**Design: smallest useful ledger**

| Field | Reason |
|---|---|
| Candidate ID, base hash/security revision, source event/activation | Bind the experiment to what was reviewed and executed |
| Failure signature and task cohort | Compare like with like |
| Baseline/candidate outcome: success, failure signature unchanged, neutral | Distinguish learned correction from an unrelated finish |
| Steps, tokens, wall time, cost, approval count | Optimize user cost, not only raw success ratio |
| Acceptance/rejection reason and actual activated version | Close the review-to-runtime loop |
| Observation completeness and sample count | Do not infer from a single success or partial ledger |

**Design: evaluation protocol**

1. Use sanitized persisted traces in an offline replay harness with mock effects; do not put tenant secrets into corpus fixtures. Freeze a task cohort before changing behavior.
2. Compare baseline against the exact candidate on the same cohort, including **no-skill** and **suppressed-rule/overlay-off** controls. Run repeated noisy cases rather than one happy path.
3. Start with conservative gates: primary task success must not regress; target signature must improve; token/step/cost ceilings must hold; security/approvals must be identical or stricter.
4. For live evaluation, canary activation is opt-in, bounded, and separately approved. Do not silently use low-confidence skill activation as a training data generator.
5. Persist superseded/rejected candidates and negative outcomes. A rejected patch is a useful pattern record, not a reason to propose the same patch on every new failure.

**Acceptance:** a fixture that fails at the target signature and a control fixture that previously passed both execute baseline/candidate; no improvement blocks promotion; a safety change cannot be traded for additional successes; a report states token, step, and evidence-coverage changes. Prove replay behavior first; live canary numbers are a later decision.

## P1 — Make extraction and learning inputs high-fidelity and bounded

### 7. Extract from evidence, not merely from counts

**Verified current behavior**

The extraction trigger carries goal, counts, tool names, and tokens for chunks with 5+ steps and 3+ action events (`runtime/mission.rs:1956-1959,2153-2213`). The prompt explicitly warns that this is a bounded summary and forbids inventing tool arguments, outputs, and snippets (`prompts/mission_skill_extraction.md:73-80`). It then asks the mission to search by goal to recover the source context (`mission_skill_extraction.md:55-64`). Skill docs are deliberately excluded from generic recall, so `memory_search(query="skill:")` is not a deterministic complete duplicate check.

A successful thread can still contain earlier failures. Counts alone do not show which sequence was actually responsible for the result, whether the user confirmed it, whether it is reusable, or whether it should never become a durable instruction.

**Recommendation**

- Create an authenticated, scope-checked trace reader for the source thread. Give it redacted, bounded arguments/results/error categories, event/step references, and pagination; do not solve this by pasting the full transcript into an unrelated memory document.
- Start from the successful suffix after the last correction. Mark unresolved warnings and intermediate failures as exclusions, not as recipe steps. A successful tool-return status is not automatically user-approved task success.
- Check for existing exact activation/task fingerprints before an LLM extraction run. Put near-duplicate or materially different same-name matches into one reviewed merge proposal; do not overwrite the current skill through the create API.
- Require repeated evidence or an explicit operator pin before promoting a general-purpose rule. One successful occurrence may remain a cited, user-private lesson instead of a skill.
- Preserve provenance to exact source threads/events, applicable preconditions, exclusions, cost, and whether the procedure has ever been verified independently. Cap snippet count/size and require the callable contract in item 3.

**Acceptance:** a fixture containing a failed intermediate command produces a recipe without that command; a repeated common pattern merges deterministically; secret/redaction boundaries hold after restart and pagination; source-insufficient fixtures create no skill; the same source event does not cause duplicate durable docs.

### 8. Give every event and aggregate an explicit identity

**Verified current behavior**

Trigger deduplication uses only `source_thread_id` and optional `content_hash` (`types/mission.rs:431-452`), and the payload carries no event-type or candidate-set discriminator. The dedup list is per mission (`runtime/mission.rs:1397-1407,1479-1484`), so different missions do not suppress each other. Within one mission, however, two semantically different occurrences that name the same source thread are treated as the same occurrence. The skill-maintenance event embeds a current-time candidate list but is keyed only by the completing thread ID, not by which candidates were due (`runtime/mission.rs:2113-2144`); a re-delivery after the candidate list changes is indistinguishable from the original. Expected-behavior events likewise have `thread_id` but no request discriminator (`prompts/mission_expected_behavior.md:5-13`).

**Impact:** evidence can be discarded or conflated precisely when the aggregate has changed, and the daily dispatch count / token spend may show activity without a corresponding new observation. This is a correctness gap in the trigger identity, not just cosmetic logging.

Non-insight payloads are rendered into the mission prompt using `serde_json::to_string_pretty(...).chars().take(4000)` (`runtime/mission.rs:4523-4532`). That is unbounded by item count and can cut off later evidence in mid-string.

**Recommendation**

- Give every occurrence a host-generated `event_id`. Keep delivery-attempt ID separate from the semantic finding fingerprint. Include event type, source thread, stable evidence/finding hash, and the candidate/version IDs that define an aggregate.
- Store bounded pending aggregates when an event is budget/coalescing suppressed. Do not reduce a growing skill-maintenance backlog to the last named source thread.
- Use a root schema version and typed payload readers; include `request_id` for expected-behavior events rather than aliasing arbitrary user feedback events that happen to describe the same thread.
- Bind one mission decision to a bounded set of source event IDs, not merely the most recent payload, so the complete learning outcome can be replayed.

**Acceptance:** duplicate event delivery runs once, new evidence for an old source runs once, changed candidate sets do not overwrite or suppress each other, and restart/retry preserves the same semantic identity without blocking a legitimate new cause.

### 9. Bound and validate learning payloads and every state update

**Verified current behavior**

Issues, observations, and action names do not share one total evidence budget, although error strings are capped at ten (`runtime/mission.rs:2030-2097`). The 4,000-character preview can omit critical fields; the complete payload remains available to `state["trigger_payload"]` at mission fire (`mission.rs:1491-1509`).

Overlapping self-improvement and expected-behavior missions can each read, reconstruct, and append to the same overlay/fix-pattern record without an expected revision or lock (`mission.rs:5136-5265`). Instruction-produced JSON is not a required structured schema: `FINAL` prose alone silently takes the no-JSON early return. `next focus` parsing accepts the first line containing that substring (`mission.rs:4802-4816`), and the latest values become guidance to the next run.

**Recommendation**

- Set explicit limits per item, per category, and total serialized bytes. Include the complete error/finding signature and source reference before optional observations; paginate rather than truncate JSON at an arbitrary character.
- Keep mission `next_focus` a bounded, line-anchored typed field. Carry forward durable hypotheses/evidence, not a generated agenda that itself could have been poisoned by recent input.
- Use atomic append/upsert for overlay and pattern stores, keyed on expected content/revision and deduplicated by finding fingerprint. Cross-mission writes must not rely on being ordered by chance.
- Use a validated final report schema for learning outcomes. A no-op is a legitimate typed outcome; an empty success response must not be indistinguishable from a patch, a skipped diagnosis, or a memory-write failure.

**Acceptance:** large malicious payloads do not crowd out the primary error; concurrent same-finding writes create one rule; a malformed focus cannot inject a section heading; no-op, proposal, applied-write, rejection, and write-failure each have distinct machine-readable outcomes.

## P2 — Reduce operating cost and expose blind spots

### 10. Consolidate reads and avoid unbounded candidate work

**Verified current behavior**

One terminal event can separately list memory documents for patch candidates, prune candidates, and eventually extraction; each skill's usage update is a separate document read-modify-write (`runtime/mission.rs:2030-2209`; `skill_feedback.rs:78-111`). `visible_skill_ids` collects IDs from all memory doc types even though only skills are useful (`skill_feedback.rs:22-43`). Selection scores all skills and, where budget allows, can inject full text (`default.py:669-736`).

**Recommendation**

Take one scoped, immutable snapshot of skill activation docs and metadata per terminal pass, then derive patch/prune/selection inputs with explicit rules. In the default path, preferably reuse the IDs already captured at activation instead of reconstructing identity later. Add projection queries only if profiling justifies them, and benchmark candidate latency and store calls at representative library sizes. Unparseable skill metadata should emit a visible skipped-record metric, not silently act as a healthy candidate or silently vanish depending on the consumer.

**Acceptance:** the number of store reads per terminal pass is bounded and reported; identical candidate snapshots cannot produce mutually inconsistent visibility decisions; oversized libraries stay within measured latency and memory budgets.

### 11. Run maintenance independently of arbitrary user thread completions

**Verified current behavior**

Prune quarantine is now time-based and defaults to 30 days (`lib.rs:37-44`; `lunarwing_skills/src/v2.rs:181-206`). Discovery nevertheless occurs on a subsequent non-mission thread terminal event, only while the gate is on (`runtime/mission.rs:2113-2144`). A dormant tenant can therefore cross quarantine without receiving a sweep. An already staged proposal creates a fresh candidate only after another terminal event, so repeated aggregate proposals may also be lost as described in item 8.

**Recommendation**

Move maintenance due calculation to the existing mission cron ticker and persist a due cursor/checkpoint. It should enumerate each project/user scope, not assume a thread completion or only the default project. Registry update sweeping already has a daily bridge task and 64-check cap (`bridge/router.rs:6241-6265,6331-6413`), but it starts only with self-improvement enabled at initialization and uses owner/default-project scope. Keep its update auth and approval behavior; make resumption schedule-driven, distinguish checked/skipped/staged, and avoid re-downloading an unchanged approved registry update.

**Acceptance:** no user thread occurs after quarantine expiration, yet the next scheduled tick proposes one eligible candidate; paused/disabled gates do not enqueue work; re-enable schedules missed checks without a fetch storm; per-user/project scope and no-change behavior are covered.

### 12. Add an operator health view and a safe observation-only switch

**Verified current behavior**

Feedback writes and several collection/store failures use `debug!`, so ordinary operator log levels can miss them (`skill_feedback.rs:109-110`; `mission.rs:4332-4337,4403-4408`; `scripting.rs:2409-2411`). The default mission `notify_channels` list is empty (`types/mission.rs:481`; managed mission creation uses the same default). Proposal logging is present, but knowing that a proposal exists still does not prove its content was correct or effective.

**Recommendation**

Expose aggregate counters, never prompt/error bodies: eligible events, dispatch holds/queue depth, accepted/no-op/rejected proposals, pending age, activated version, rollback, missing evidence, stale write conflict, and metric-observation failures. Include tokens/cost and the mission/thread references needed for read-only diagnosis. Add a subset read-only dashboard/API without granting ordinary users shared-owner mutation authority.

An **observation-only** switch should preserve activation IDs, no-op evidence, validation, and audit counts while blocking overlay/orchestrator activation and skill mutation/proposal staging. This is not the existing `SKILL_SELF_IMPROVEMENT` flag renamed: today that flag gates feedback and patch/prune missions, while `ORCHESTRATOR_SELF_MODIFY` separately gates prompt/code activation (`lib.rs:26-35`; `orchestrator.rs:156-170`). Keep that security boundary explicit and do not silently change tenant provisioning defaults as part of an optimization experiment.

**Acceptance:** observation-only mode cannot activate or mutate learned state; the complete event→dispatch→outcome chain has aggregate visibility; no credentials, payloads, or secret environment values appear in the health surface; switch behavior survives restart.

## Recommended sequencing

1. **First correctness slice:** items 1–3. Before comparing behavior, make one version addressable, rollback truthful, and the skill/capability contract explicit.
2. **Instrument before optimizing:** items 4–6 plus the aggregate health fields in item 12. Build the sanitized replay corpus and measure baseline/candidate behavior without granting any new runtime capabilities.
3. **Reduce noise and cost:** items 7–10. Extract from source evidence with deterministic fingerprints, preserve typed event identities, and share reads.
4. **Close the operations loop:** item 11 and the observation-only/health controls in item 12. Start with deterministic tenant-local replay/unit coverage, then a separately authorized disposable-tenant canary; do not learn against arbitrary production conversations.

None of these follow-ups is licensed to modify security/policy/leak detection automatically, bypass the approval/pairing gates, widen network or credential access, or make an unreviewed runtime commit. The correct outcome on an ambiguous finding is **no-op with evidence**, not another learned rule.

## Audit evidence map

The above findings were verified primarily in:

- `ic/crates/lunarwing_engine/src/executor/{orchestrator.rs,loop_engine.rs,prompt.rs,scripting.rs}`
- `ic/crates/lunarwing_engine/src/runtime/{mission.rs,skill_feedback.rs}`
- `ic/crates/lunarwing_engine/src/memory/{skill_tracker.rs,retrieval.rs}`
- `ic/crates/lunarwing_engine/orchestrator/default.py`
- `ic/crates/lunarwing_engine/prompts/mission_{self_improvement,skill_extraction,expected_behavior,skill_maintenance}.md`
- `ic/crates/lunarwing_skills/src/v2.rs`
- `ic/src/bridge/{router.rs,store_adapter.rs}`

No Cargo command was needed for this documentation-only change. The acceptance criteria in this document have intentionally not been marked as implemented or tested.
