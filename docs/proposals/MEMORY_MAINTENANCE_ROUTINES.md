# Memory Maintenance Routines for LunarWing

> **Current status (2026-07-20, rev `50c8f99`): OPEN / PLANNED.** The routine
> pack importer, proposal-only capability profile, durable maintenance proposal
> store, reviewed apply path, and three first-party routines described here do
> not exist. Existing routine and workspace primitives are only prerequisites.

> Status: first-pass planning stub. This document proposes architecture and delivery options only; it does not implement routines, a pack installer, or an apply path.

## Decision Summary

LunarWing should start with an importable, declarative routine-pack mechanism and ship the first-party memory-maintenance pack as a bundled catalog entry. The core daemon should provide generic execution, capability restrictions, durable proposal records, diff preview, and explicit apply semantics; the three routines should remain replaceable content rather than becoming product-specific logic in `main.rs` or the routine engine.

The built-in option remains useful as a later promotion path for a small, reviewed default pack. In either model, maintenance routines must run in a read-only analysis profile and produce a durable proposal. A routine must never call `memory_write` (or an equivalent write path) to apply its own answer. Applying a proposal is a separate, authenticated user action with an optimistic content check.

This is the design follow-up for issue #201, which the issue-status document
records as an open stub with no implementation or proposal at its report
snapshot ([`docs/ops/ISSUE_STATUS_REPORT.md`](../ops/ISSUE_STATUS_REPORT.md#L61-L66)).

## Context, Goals, and Non-Goals

The immediate users are existing LunarWing operators who already run conservative memory de-duplication routines on production agents, and new users who should be able to import the same routines without hand-copying prompts or reconstructing scheduler settings. The first release should make the workflow understandable and reversible:

1. Run a manual or scheduled analysis against well-defined workspace documents.
2. Show what would change as a per-file unified diff, with evidence and confidence.
3. Let a human accept or reject each proposal (or an explicitly selected set).
4. Refuse to apply a proposal if the source document changed since analysis.

Goals:

- Reuse LunarWing's existing routine triggers, lightweight execution, workspace reads, notification routing, and run history.
- Support three maintenance classes: de-duplication, cleaning, and correction.
- Make the no-auto-mutation rule a host-enforced capability, not merely a prompt instruction.
- Preserve timestamps, dates, URLs, commands, unique notes, layer/scope ownership, and document history.
- Make packs inspectable, versioned, idempotent, and safe to install per user/tenant.

Non-goals for this stub:

- A general-purpose knowledge graph or automatic truth engine.
- Silent rewriting of `MEMORY.md`, daily logs, or any other workspace document.
- Treating a prompt-only skill or a WASM extension as an executable routine pack.
- Replacing the existing retention hygiene pass with semantic maintenance.
- Making remote third-party downloads trustworthy by default; signed distribution is a later phase.

## Current Implementation (Verified)

### Routine lifecycle

The current routine model already covers most trigger and execution vocabulary needed by the examples:

- [`ic/src/agent/routine.rs`](../../ic/src/agent/routine.rs#L31-L53) defines a persistent, user-owned `Routine` with `enabled`, `trigger`, `action`, guardrails, notification settings, and runtime counters/state.
- [`ic/src/agent/routine.rs`](../../ic/src/agent/routine.rs#L55-L91) supports `Cron`, message `Event`, `SystemEvent`, `Webhook`, and `Manual` triggers. Cron expressions are normalized from five, six, or seven fields and can carry an IANA timezone ([`ic/src/agent/routine.rs`](../../ic/src/agent/routine.rs#L567-L607)).
- [`ic/src/agent/routine.rs`](../../ic/src/agent/routine.rs#L229-L262) defines `RoutineAction::Lightweight` and `FullJob`. Lightweight actions persist `prompt`, `context_paths`, `max_tokens`, `use_tools`, and `max_tool_rounds`; `max_tool_rounds` is clamped to a hard limit of 20 ([`ic/src/agent/routine.rs`](../../ic/src/agent/routine.rs#L272-L283)).
- PostgreSQL stores trigger/action JSON and runtime state in `routines`, and execution audit rows in `routine_runs` ([`ic/migrations/V6__routines.sql`](../../ic/migrations/V6__routines.sql#L7-L73)). The shared `RoutineStore` contract exposes create/list/update/delete, due-cron/event lookup, run completion, concurrency counts, and history ([`ic/src/db/mod.rs`](../../ic/src/db/mod.rs#L607-L679)); PostgreSQL and libSQL both implement it.
- The agent loop constructs one `RoutineEngine`, registers the seven routine tools, loads the event cache, forwards notifications, and starts the cron ticker ([`ic/src/agent/agent_loop.rs`](../../ic/src/agent/agent_loop.rs#L719-L846)). The engine polls due cron rows, matches event/system-event rows, enforces cooldown/concurrency/dedup guardrails, and records runs ([`ic/src/agent/routine_engine.rs`](../../ic/src/agent/routine_engine.rs#L250-L514)).
- The concrete tool-registration boundary is [`ic/src/tools/registry.rs`](../../ic/src/tools/registry.rs#L564-L597) (`register_routine_tools`). A maintenance pack should add a narrowly scoped importer/service beside this boundary rather than teach arbitrary routines to self-register through LLM calls.
- Manual firing is exposed through the engine and authenticated web handler ([`ic/src/agent/routine_engine.rs`](../../ic/src/agent/routine_engine.rs#L824-L902), [`ic/src/channels/web/handlers/routines.rs`](../../ic/src/channels/web/handlers/routines.rs#L146-L173)). The web UI currently lists, inspects, runs, toggles, deletes, and shows history, but has no create/import endpoint ([`ic/src/channels/web/server.rs`](../../ic/src/channels/web/server.rs#L481-L491)). The CLI manages list/create/edit/enable/disable/delete/history, but create is cron-only ([`ic/src/cli/routines.rs`](../../ic/src/cli/routines.rs#L17-L132)).
- Conversational `routine_create` accepts the grouped `request.kind` and `execution` shape, then writes one row immediately ([`ic/src/tools/builtin/routine.rs`](../../ic/src/tools/builtin/routine.rs#L888-L960), [`ic/src/tools/builtin/routine.rs`](../../ic/src/tools/builtin/routine.rs#L1089-L1160)). It hardcodes the lightweight token budget to 4096 in the builder, so a pack importer must either extend this path or instantiate the canonical model directly if it needs a different `max_tokens`.

The existing proposal [`docs/proposals/ROUTINE_ENGINE_IMPROVEMENTS.md`](ROUTINE_ENGINE_IMPROVEMENTS.md#L1-L50) is relevant context: it documents state sanitization, retry behavior, and event content-hash dedup as completed work. Its older claim that no structured routine status query exists is stale; current web, CLI, and tool status/history surfaces are listed above. The proposal should not duplicate those surfaces.

### Memory and workspace semantics

LunarWing's memory is a database-backed virtual filesystem, not local files:

- Path normalization and the `MemoryDocument` shape (`id`, `user_id`, `agent_id`, `path`, `content`, timestamps, metadata) live in [`ic/src/workspace/document.rs`](../../ic/src/workspace/document.rs#L7-L78). The current [`normalize_path` helper](../../ic/src/workspace/mod.rs#L1799-L1814) only trims and collapses slashes; it is not a complete security canonicalizer. Selector expansion and proposal storage must therefore reject traversal, control characters, and ambiguous empty segments, then record a validated path identity before reading or writing. `MEMORY.md` and `daily/` are intentionally special paths.
- `Workspace::read`, `list`, and `list_all` can merge configured read scopes, while writes target the primary scope. `MEMORY.md` and daily logs may span read scopes; identity files do not ([`ic/src/workspace/mod.rs`](../../ic/src/workspace/mod.rs#L626-L658), [`ic/src/workspace/mod.rs`](../../ic/src/workspace/mod.rs#L907-L957)). A maintenance proposal therefore needs an explicit source scope/layer, not just a path.
- `Workspace::read` accepts one normalized path at a time. The routine engine loads each `context_paths` entry literally and logs a missing path ([`ic/src/agent/routine_engine.rs`](../../ic/src/agent/routine_engine.rs#L1516-L1552)). `daily/*.md` is not currently a glob selector. A daily-file routine must enumerate with `memory_tree`/workspace listing and then call `memory_read`, or gain a bounded selector API. The resolver must restrict both the owner/layer and the selector, with limits on file count, total bytes/tokens, and candidate count.
- Lightweight execution also calls `ctx.workspace.system_prompt()` after loading `context_paths` ([`ic/src/agent/routine_engine.rs`](../../ic/src/agent/routine_engine.rs#L1561-L1568)); that prompt can inject broad identity, `MEMORY.md`, and recent daily context. A maintenance selector is not a true information boundary until the maintenance profile supplies a restricted system prompt/snapshot and treats all selected memory text as untrusted data.
- `Workspace::write` atomically updates document content and chunks ([`ic/src/workspace/mod.rs`](../../ic/src/workspace/mod.rs#L660-L688)); generic `append` uses an atomic SQL concatenation ([`ic/src/workspace/mod.rs`](../../ic/src/workspace/mod.rs#L690-L719)). `append_memory` still performs a read-modify-write ([`ic/src/workspace/mod.rs`](../../ic/src/workspace/mod.rs#L1030-L1053)), so a proposal apply path must not rely on it for concurrency safety.
- Hybrid search is FTS plus optional vector search, with chunking and RRF described in [`ic/src/workspace/README.md`](../../ic/src/workspace/README.md#L71-L95) and [`docs/architecture/SEMANTIC-MEMORY-SEARCH.md`](../architecture/SEMANTIC-MEMORY-SEARCH.md#L1-L30). Search results are useful candidate recall, but do not include revision/hash or complete source provenance; maintenance analysis must read the full documents before proposing a patch.
- `memory_read`, `memory_tree`, and `memory_search` are read-oriented tools; `memory_write` can replace or append arbitrary workspace paths ([`ic/src/tools/builtin/memory.rs`](../../ic/src/tools/builtin/memory.rs#L185-L253), [`ic/src/tools/builtin/memory.rs`](../../ic/src/tools/builtin/memory.rs#L465-L548)). Lightweight routine loops run without approval dialogs and derive their tool set from an autonomous denylist ([`ic/src/agent/routine_engine.rs`](../../ic/src/agent/routine_engine.rs#L1823-L1831), [`ic/src/tools/autonomy.rs`](../../ic/src/tools/autonomy.rs#L8-L68)). `memory_write` is not denylisted and its default tool approval is `Never` ([`ic/src/tools/tool.rs`](../../ic/src/tools/tool.rs#L319-L327)). The proposal-only profile must replace that broad denylist-derived set with an explicit read allowlist, a scope-restricted memory-tool wrapper, and a dispatch-time re-check; a prompt saying "never apply" is not a sufficient safety boundary.
- A separate deterministic hygiene pass deletes old `daily/` and `conversations/` documents on a cadence (defaults: 30 days, 7 days, 12 hours) and protects identity basenames ([`ic/src/workspace/hygiene.rs`](../../ic/src/workspace/hygiene.rs#L1-L25), [`ic/src/workspace/hygiene.rs`](../../ic/src/workspace/hygiene.rs#L65-L202)). It is spawned by heartbeat ([`ic/src/agent/heartbeat.rs`](../../ic/src/agent/heartbeat.rs#L259-L280)); it is retention cleanup, not semantic dedupe or correction, and must remain a separate policy.
- A document-version table and DB methods exist ([`ic/migrations/V16__document_versions.sql`](../../ic/migrations/V16__document_versions.sql#L1-L24), [`ic/src/db/mod.rs`](../../ic/src/db/mod.rs#L949-L1007)), but normal `Workspace::write` does not currently expose a compare-and-swap revision or automatically save a version. The proposal apply service must close that gap rather than assume version history is already active.

One additional boundary matters for multi-tenant operation: `RoutineEngine` currently holds one `Arc<Workspace>` while `Routine.user_id` is only placed in the job/tool context. Imported memory routines should be owner-scoped until the engine accepts a per-user `WorkspaceResolver` (or one engine is instantiated per tenant). They must not be seeded globally and assumed to read the authenticated gateway user's workspace.

## Delivery Models

| Model | Shape | Advantages | Costs and risks |
| --- | --- | --- | --- |
| **A. Built-in routines** | Compile reviewed definitions/prompts into LunarWing and seed instances during onboarding or engine startup. | Zero file-format decision for users; versioned with the daemon; easy to test and support; policy can be enforced centrally. | Product releases become coupled to prompt changes; enabling a schedule can surprise existing users; local edits and upgrades need migration rules; third-party routines cannot ship independently. |
| **B. Importable routine packs** | Install a versioned declarative pack that validates and creates user-owned routine instances. | Works for the three production routines now; easy to share and fork; decouples prompt iteration from daemon releases; supports preview, opt-in enablement, provenance, and per-user customization. | Requires schema/version validation, collision and upgrade policy, capability review, and an installer; untrusted prompts are a supply-chain/input risk. |

**Recommendation:** implement Model B first, with a small first-party pack embedded in the catalog so a new user can discover it as easily as a built-in. The daemon owns the safety and proposal/apply machinery; the pack owns prompts, selectors, schedules, and human-readable descriptions. If a routine later proves universally useful, it can be promoted to a built-in pack entry without creating a second execution path.

## Proposed Architecture

### 1. Maintenance routine contract

Keep the existing `Routine`, `Trigger`, and `RoutineAction::Lightweight` contract as the execution envelope. Add a maintenance-specific, versioned policy alongside the action (pack metadata or a dedicated JSON field; do not overload runtime `state`):

```text
maintenance:
  class: dedupe | cleaning | correction | project_review
  input_selectors: bounded workspace paths/globs (optional in phase 1)
  execution_mode: proposal_only
  allowed_tools: memory_tree, memory_read, memory_search
  max_candidates: bounded integer
  proposal_expiry: duration
```

The policy is host-enforced before a run. `proposal_only` removes all write-capable tools, shell/file tools, outbound messaging tools, routine-management tools, and arbitrary extensions from the routine's tool definitions. The routine may still return a textual report, but the durable proposal is created by host code from a structured result, not by trusting an LLM-created path or diff.

### 2. Analysis and proposal flow

1. A manual trigger or cron trigger selects an enabled routine. The existing engine supplies the run ID, owner, notification configuration, and token/iteration limits.
2. A workspace resolver expands only permitted selectors and reads a consistent snapshot. Each input records a validated path identity, scope/layer, document ID, `updated_at`, and a content hash. Missing, cross-layer, or ambiguous paths fail closed and are reported. Expansion is capped by file count, total bytes/tokens, and candidate count.
3. Deterministic analyzers find candidate units (lines, entries, blocks, or facts). They do not edit content. The LLM receives those candidates as untrusted data and is asked only to classify, explain, and choose among candidate changes. Exact and structural checks can produce host-owned findings without an LLM at all.
4. A host-side proposal builder validates that every proposed hunk refers to the captured base, stays within the selected path, preserves protected fields, and is a minimal patch. It stores the bounded full proposed content as the canonical apply payload and derives a normalized unified diff for review/audit, together with the base hash/revision, evidence, confidence, routine/pack provenance, and `pending` status.
5. The ordinary routine run completes with `RunStatus::Attention` when proposals exist. Manual firing returns a run ID before the spawned execution necessarily finishes, so proposal IDs must be persisted against that run and looked up through run history rather than assumed to be available synchronously. The routine conversation and notification contain a short, redacted summary and proposal ID(s), not a diff, memory excerpt, or instruction to write target files. A no-findings run may be `Ok`; `weekly-project-review` is explicitly configured to return a non-empty report instead of the normal `ROUTINE_OK` sentinel.
6. A later authenticated action previews the diff and accepts, rejects, or expires it. Apply re-reads every target and compares the current hash/revision with the captured base. A mismatch atomically marks the proposal stale and requires a fresh analysis; it never silently rebases an LLM patch.
7. On acceptance, one backend transaction locks the pending proposal, re-authorizes its owner and target scope from server-side records (not a client-supplied document UUID alone), validates the final content (including prompt-injection checks for files that can enter system prompts), writes content and search chunks, records the prior content/version and `changed_by`, then marks the proposal applied. Repeated acceptance of an applied proposal is an idempotent success; rejected, expired, or stale proposals cannot apply. Phase 1 can apply one file at a time; an all-files transaction is a later enhancement. No routine execution path calls this service implicitly.

The durable record is preferable to reusing the current in-memory `PendingApproval`: that approval card is tied to an interactive tool call, does not render a diff, and is lost on restart ([`ic/src/channels/web/types.rs`](../../ic/src/channels/web/types.rs#L97-L115)).

#### Structured result boundary

The current lightweight path turns a free-form response into a run summary. It must not treat that prose as a patch. For proposal-producing runs, define a versioned, size-limited result envelope (returned through a dedicated result field or a strictly parsed response block) such as:

```json
{
  "schema_version": 1,
  "summary": "...",
  "findings": [
    {
      "target": {
        "path": "daily/2026-07-12.md",
        "document_id": "...",
        "base_sha256": "..."
      },
      "operations": [{ "start": 120, "end": 145, "replacement": "..." }],
      "evidence": ["..."],
      "confidence": 0.98
    }
  ]
}
```

The host validates the schema, target identity, operation bounds, protected-field rules, finding count, and total result/diff bytes before generating a unified diff. Unknown fields, malformed JSON, missing base hashes, or an over-limit result produce a report with no proposal; they never fall back to applying or guessing from prose. This also gives deterministic exact/cleaning analyzers a common handoff and leaves near-duplicate/correction decisions replaceable.

### 3. Routine classes

#### De-duplication

**Exact pass (phase 1):** split the selected snapshot into stable units, normalize only line endings and trailing whitespace for comparison, and hash the full unit. The comparison index spans all selected documents, so duplicates across daily files can be found, but each proposed hunk still targets one file. Keep the first occurrence and flag later byte-equivalent lines/entries. Do not ignore timestamps, dates, URLs, commands, headings, or code fences during this pass; two lines that differ in those fields are not exact duplicates.

**Near-duplicate pass (phase 2):** create a comparison form that collapses repeated whitespace and harmless punctuation and uses token shingles plus a bounded similarity/edit-distance threshold. Cluster candidates, then ask the LLM to confirm semantic equivalence. Embedding similarity may rank candidates but must not authorize a merge by itself. Keep the original units and source locations in the proposal so the human can see why a pair was grouped.

- For `daily/*.md`, prefer line-level or timestamped-entry hunks. Identical content with different timestamps is a review candidate, not an automatic deletion; preserving the chronology is the default.
- For `MEMORY.md`, prefer entry/block-level hunks. A merge is allowed only when the proposition is clearly the same and the union preserves all dates, URLs, commands, qualifiers, and unique details. Conflicting values become a correction candidate, never a dedupe merge.

#### Cleaning

Cleaning identifies entries that are empty, structurally orphaned, or stale, but treats deletion as a proposal. Examples include whitespace-only files, empty list items, duplicate headings with no body, and project entries with no evidence of activity for a configured window. A stale label must carry its evidence (last relevant daily-log date, document `updated_at`, or a missing project signal) and should normally suggest archive/annotation rather than deletion.

The existing retention hygiene job remains authoritative for age-based deletion of whole documents. A maintenance cleaning routine must not duplicate its retention window or race it; the pack should either exclude documents already selected for retention or report that the source may disappear before review. No cleaner may remove `MEMORY.md`, identity files, timestamps, or an entry merely because it is old.

#### Correction

Correction is the highest-risk class. It detects potentially wrong or contradictory facts by comparing entries across `MEMORY.md`, dated logs, and explicitly named authoritative sources. Every candidate includes both sides of the contradiction, paths/line locations, dates, and an evidence ranking. Suggested ranking is: explicit user correction, current user-authored source, later dated note when it clearly supersedes an earlier one, then weaker inferred evidence.

The routine should preserve the old statement in the diff context and propose a minimal replacement or an explicit unresolved annotation. It must not infer that a newer timestamp is true merely because it is newer, and it must return a non-actionable report when evidence is insufficient. Correction proposals require explicit per-hunk confirmation even if a user previously enabled a pack.

### 4. Diff, confirmation, and apply surfaces

The proposed record should include at least:

```text
proposal_id, owner_id, scope/layer, routine_id, pack@version
path, document_id, base_updated_at, base_sha256, proposed_sha256
proposed_content, derived_unified_diff, findings, evidence, confidence
status: pending | accepted | rejected | stale | expired | applied
created_at, expires_at, applied_at, changed_by
```

`base_sha256` and `proposed_sha256` cover the exact stored UTF-8 content; comparison normalization is only for candidate ranking. Proposal records and APIs must also impose byte limits on full content, diffs, findings, and evidence, with an over-limit analysis degrading to report-only.

Suggested surfaces, reusing existing routing where useful:

- **Web:** add a maintenance/review view linked from the routine detail and recent run. Show one file at a time, the unified diff, evidence, base timestamp/hash, and explicit `Accept`, `Reject`, and `Refresh` actions. Render memory and diff text as escaped text, never raw HTML. Enforce owner/scope authorization on every read and apply, bound the returned diff/evidence size, and redact sensitive content from notifications and logs. Add authenticated proposal endpoints; do not make the existing immediate `/api/memory/write` endpoint stand in for review.
- **CLI:** add a dry-run/preview command that lists pack changes and a proposal review command that prints the exact diff. Applying should require an explicit proposal ID and confirmation flag or interactive prompt; a stale hash exits non-zero with a refresh instruction.
- **Conversation/channels:** keep the current routine notification path (`send_notification` and the routine conversation) for a concise, redacted alert and proposal ID. A later interactive command/tool can fetch and apply a proposal after owner authorization. Do not block a cron routine waiting for a chat approval or send the full diff through an untrusted channel.
- **Existing approval UI:** a dedicated apply tool may use `ApprovalRequirement::Always`, but its approval payload must reference a durable proposal and render the diff. The generic current approval card shows raw parameters, not before/after content, so it is only a transport primitive, not the complete review UX.

## Integration Points in the Codebase

| Area | Existing hook | Proposed planning boundary |
| --- | --- | --- |
| Routine model/validation | `ic/src/agent/routine.rs`; `ic/src/tools/builtin/routine.rs` | Add a validated maintenance policy and pack provenance; expose `max_tokens` and bounded selectors instead of silently hardcoding them. Keep trigger/action serialization compatible. |
| Engine execution | `ic/src/agent/routine_engine.rs`; `ic/src/agent/agent_loop.rs` | Add a read-only capability profile and a host-side structured-result/proposal handoff. Register the generic maintenance service where the engine is constructed; do not embed individual prompts in `main.rs`. |
| Workspace reads | `ic/src/workspace/mod.rs`, `document.rs`, `search.rs` | Add bounded selector expansion and a read snapshot that returns scope, document ID, timestamp, and hash. Resolve per-user workspaces before enabling multi-tenant packs. |
| Proposal/apply persistence | `ic/src/db/mod.rs`, `ic/migrations/V6__routines.sql`, `ic/migrations/V16__document_versions.sql`, both backend implementations | Add a shared proposal store and one transactional compare-and-swap apply operation. Lock/authorize the pending row, save prior content/version, replace chunks, and transition status atomically in PostgreSQL and libSQL. |
| Memory tools | `ic/src/tools/builtin/memory.rs`, `ic/src/tools/autonomy.rs` | Expose only scope-restricted `memory_read/tree/search` to analysis; deny `memory_write` and all other mutation/external tools for `proposal_only` runs, and re-check the allowlist at dispatch. |
| Notifications and review | `ic/src/agent/routine_engine.rs`, `ic/src/channels/web/handlers/routines.rs`, `ic/src/channels/web/server.rs`, `ic/src/channels/web/static/app.js`, `ic/src/channels/web/types.rs` | Add proposal list/detail/accept/reject endpoints and UI diff rendering; extend run detail with proposal IDs. |
| CLI/import | `ic/src/cli/routines.rs`, `ic/src/cli/memory.rs`, `ic/src/cli/import.rs` | Add pack `validate`, `preview`, `install`, `enable`, and proposal `review/apply` commands. Follow OpenClaw's read-all-first `--dry-run` precedent, but make pack installation owner-scoped and idempotent. |
| Existing cleanup | `ic/src/workspace/hygiene.rs`, `ic/src/agent/heartbeat.rs` | Document the boundary: retention cleanup remains deterministic and separate; maintenance routines never bypass or silently alter it. |
| First-party precedent | `ic/src/setup/profile_evolution.rs` | Treat the existing profile-evolution prompt as a prompt-template precedent only. It describes a routine but has no current `create_routine` registration path, so it should not be copied as an implicit seeding mechanism. |

## Importable Routine Pack (Model B)

### Format

Use versioned JSON, matching the repository's registry-manifest style ([`ic/src/registry/manifest.rs`](../../ic/src/registry/manifest.rs#L1-L42)) and the Rust serde vocabulary already used by `RoutineAction`. The importer maps the pack's tagged action to the database's separate `action_type` and `action_config` columns. YAML can be a later authoring format, but JSON makes schema validation, hashing, and signatures unambiguous in the first version.

Illustrative (not an implementation contract yet):

```json
{
  "schema_version": 1,
  "pack": "memory-maintenance",
  "version": "0.1.0",
  "display_name": "Memory maintenance routines",
  "requires": { "lunarwing": ">=1.1.2" },
  "routines": [
    {
      "id": "daily-note-dedupe",
      "name": "daily-note-dedupe",
      "enabled_by_default": false,
      "trigger": { "type": "manual" },
      "action": {
        "type": "lightweight",
        "context_paths": [],
        "max_tokens": 4096,
        "max_tool_rounds": 3,
        "use_tools": true,
        "prompt": "..."
      },
      "maintenance": {
        "class": "dedupe",
        "input_selectors": ["daily/*.md"],
        "execution_mode": "proposal_only",
        "allowed_tools": ["memory_tree", "memory_read", "memory_search"]
      }
    }
  ]
}
```

The `input_selectors` field is deliberately separate from existing literal `context_paths`; until selector expansion exists, the routine can enumerate `daily/` with read tools. Packs must declare no secrets, no executable code, and no write-capable tools. A pack may declare human-readable variables (timezone, project-age threshold, notification target), but installation resolves them explicitly rather than interpolating environment secrets into prompts.

The compatibility value in this illustrative snippet is not a claim that the current importer exists on every 1.1.2 deployment. This checkout reports package version 2.0.0 in [`ic/Cargo.toml`](../../ic/Cargo.toml#L11-L15), while the supplied routines were exercised on 1.1.2; installation must check the daemon's actual maintenance capabilities and reject or select a compatible pack variant when the range is unsupported.

### Validation and provenance

The installer should validate the entire pack before creating any rows:

- schema version, pack/version syntax, unique IDs/names, prompt and count limits;
- supported trigger types, cron/timezone validity, cooldown and tool-round bounds;
- safe relative workspace selectors with no absolute paths, traversal, control characters, or unbounded globs, plus caps for expanded files and total bytes/tokens;
- maintenance class and `proposal_only` policy; reject a pack that requests `memory_write`, shell, file writes, routine management, secrets, or arbitrary external extensions;
- LunarWing compatibility and declared variables;
- notification targets resolved against the installing owner and shown as an explicit outbound side effect during preview;
- optional signature/checksum and source URL policy (local files and embedded first-party packs first).

Persist pack name/version/content hash and the routine definition ID separately from runtime state. Installation is idempotent for the same hash. On an upgrade, replace only an untouched pack-managed routine; if a user changed its prompt, schedule, or enablement, show a three-way diff and require an explicit choice. Uninstall should remove only routines still owned by that pack and leave user-edited instances in place with a warning.

### Discovery and install flow

1. Discover a bundled first-party entry, a local `--file` pack, or (later) a signed catalog entry. Do not execute a downloaded prompt during discovery.
2. Show pack metadata, routine names, triggers/timezones, requested read tools, default enablement, compatibility, and any conflicts.
3. Run `validate`/dry-run without writes. Resolve variables and report the exact rows that would be created or updated.
4. Require an explicit install confirmation. Create all definitions through a pack service that can roll back or clearly report partial state; do not loop over `routine_create` tool calls because the current `RoutineStore` has no bulk transaction contract.
5. Install disabled by default, except when the user explicitly selects `--enable` or checks a per-routine enable box. Enabling a cron routine displays its next fire time and timezone. Manual routines still require an explicit `run` action.
6. Refresh the event cache and expose provenance in `routine_list`, web detail, and history. A notification explains how to run the maintenance routine and where proposals appear.

The existing skill installer is useful for discoverability but is not the pack transport: installed skills are intentionally capability-attenuated and the installer persists only `SKILL.md`. A routine pack needs a dedicated validated importer, not a hidden instruction to make the LLM call `routine_create` repeatedly.

## Mapping the Three Example Routines

- **Daily-note dedupe:** manual trigger, lightweight, read-only tools, selector `daily/*.md`; deterministic exact pass first; near-duplicate candidates later; one proposal per file; never merge different timestamps by default.
- **MEMORY.md dedupe:** manual trigger, lightweight, read-only tools, selector `MEMORY.md`; entry/block comparison; only clearly equivalent propositions may merge; dates, URLs, commands, qualifiers, and unique notes are preserved in the proposed content.
- **weekly-project-review:** cron trigger `0 0 13 * * MON` with timezone `UTC`, lightweight, read-only tools, inputs from `MEMORY.md` and daily logs; classify each active project as Active/Blocked/Stale, include progress and next steps/blockers, mark Stale when no work is evidenced for 72 hours, and use a non-empty fallback report rather than `ROUTINE_OK` when uncertain. Activity timestamps combine the `daily/YYYY-MM-DD.md` filename date with the configured timezone rather than treating a bare time-of-day as UTC.

## Phased Roadmap

### Phase 1 - Minimal viable pack and review loop

- Define and validate the JSON pack schema and provenance fields.
- Ship a bundled first-party pack plus local-file `validate`, `preview`, `install`, and explicit `enable` flow.
- Add a read-only maintenance capability profile and owner-scoped workspace resolution.
- Implement deterministic exact duplicate detection, empty-entry candidates, and the three example prompts as proposal-only routines.
- Persist a proposal with base hash/scope and unified diff; expose CLI and web preview, reject, and per-file apply with stale-checking.
- Add focused tests for pack validation, tool denial, path/scope ownership, exact diff generation, stale proposals, and atomic content/chunk updates. No cargo/build is part of this proposal.

### Phase 2 - Conservative near-duplicate and cleaning heuristics

- Add token/shingle similarity and bounded edit distance with fixture-driven thresholds.
- Add Markdown-aware entry parsing, empty-section detection, stale evidence windows, archive suggestions, and candidate clustering.
- Add proposal expiry, batch review, and richer run summaries without changing the no-auto-apply invariant.

### Phase 3 - Correction and provenance

- Add contradiction candidates with source ranking, dates, citations, and unresolved outcomes.
- Add explicit user corrections as high-confidence evidence and preserve an audit/version trail for every accepted hunk.
- Add per-layer review and conflict resolution; never merge across scopes without an explicit target.

### Phase 4 - Distribution and built-in promotion

- Add signed remote catalog entries, checksum/signature verification, pack update notifications, and rollback tooling.
- Promote only stable, reviewed packs to built-in catalog entries; keep user instances opt-in and migration-safe.

## Open Questions / Stub Items

- Should proposal storage be a new `maintenance_proposals` table, a generalized routine-result artifact, or both? What retention/size limit is appropriate for full diffs?
- Should phase 1 apply one file at a time, or introduce a cross-document transaction immediately for all-or-nothing review?
- What exact Markdown unit should a `MEMORY.md` entry be: heading section, list item, paragraph, or a configurable parser supplied by the pack?
- Which normalization rules are safe for near-duplicate comparison across languages, code blocks, URLs, and timestamps?
- Should stale project status use document `updated_at`, dated log content, explicit project metadata, or a combination with per-pack thresholds?
- How should a correction routine identify an authoritative source without granting it network access or trusting arbitrary URLs?
- How should proposal review work when a document exists in multiple memory layers and the same path is visible in more than one scope?
- What is the correct per-tenant `WorkspaceResolver` lifecycle for routines, and should routines be disabled when the engine cannot resolve the routine owner?
- Should accepted patches always create a document version, and how many versions should hygiene retain once version writes are wired into `Workspace`?
- Which notifications are appropriate for a scheduled review that has no findings, given the existing `on_success` default of false?
- Do third-party packs need signature verification before local installation, or is an explicit local-file confirmation sufficient for the first release?

## Appendix A - Example Routines (Verbatim)

The following requirements are reproduced verbatim from the task brief.

1. **Daily-note dedupe (manual trigger, lightweight, use_tools):** scans `daily/*.md` for exact + near-duplicate lines, proposes minimal `diff -u` patches per file, NEVER auto-applies, asks for confirmation, preserves timestamps/unique notes.

2. **MEMORY.md dedupe (manual trigger, lightweight, use_tools):** scans `MEMORY.md` for duplicate/near-duplicate entries, outputs a unified diff, conservative (only clearly-duplicate merges), preserves dates/URLs/commands, asks before applying.

3. **weekly-project-review (scheduled: Mondays 13:00 UTC, lightweight):** analyzes active projects from memory + daily logs; per project reports Status (Active/Blocked/Stale), progress, next steps/blockers; flags Stale if no work in 72h; always returns non-empty (fallback summary if unsure).

Common shape: `{ context_paths, max_tokens, max_tool_rounds, prompt, type: lightweight, use_tools }`, with a trigger (manual or cron-like schedule). All are conservative: propose diffs, never auto-mutate, human-in-the-loop confirmation.
