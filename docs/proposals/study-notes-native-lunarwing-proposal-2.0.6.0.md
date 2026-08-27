# Native Study-Notes Knowledge Layer

> **Status (reverified 2026-08-25): IMPLEMENTED — Phase 1 (core store),
> Phase 2 (lifecycle integration), and the Phase 3 new-tenant bootstrap are in
> tree.** This document records the as-built design originally tracked as
> v2.0.6.0 release-goals items #1 and #2; that working checklist was retired
> during later operations-doc cleanup. It was written retroactively: the
> implementation landed first (branches
> `rarity/kimi-1-20260806-1802` and `aj/kimi-2-20260807-0001`), so everything
> below describes verified tree state, not planned work.
>
> **v2.0.8.0 addendum (item #14):** the optional coordination hook in
> `ic/src/hooks/coordination_knowledge.rs` propagates one-line
> `KNOW: <topic> | <finding>` records between tenant daemons watching the same
> room. Each daemon writes into its own database's `shared` scope. Wire claims
> use collision-resistant `coord-*` agent paths and always enter as
> `needs-verification`; they never become verified merely by crossing the room.
> See `MULTIAGENTTEAMORCHESTRATION/DEPLOYMENT.md` for the protocol and limits.
>
> **v2.0.8.0 addendum (item #10, 2026-08-20):** the native layer reached full
> parity with the canonical lw2studymcp store (7 tools). Added
> `knowledge_list_topics` (topic coverage survey) and `knowledge_update_status`
> (metadata-only lifecycle patch: only provided fields change, explicit null
> clears nullable fields, `superseded` requires `superseded_by`) — both
> registered and in `PROTECTED_TOOL_NAMES`. `KnowledgeNoteMeta` gained
> `stale_after_days` (default 90) and `status_reason`; stale detection honors
> the per-note threshold when no explicit age limit is passed (the sweep's
> configured limit still overrides). `knowledge_search` gained a
> `min_confidence` filter, and the tags filter is now canonical
> require-every-tag on both backends (Postgres `?&`, libSQL all-of).
>
> **v2.0.9.0 addendum (items #3–4, 2026-08-24):** a newly provisioned
> multi-tenant instance using `add-tenant --with-study-mcp` now imports the
> synced checkout into its private native store once on first startup. The
> opt-in flag is written only by that new-tenant path; `configure-study-mcp`,
> env regeneration, and `upgrade-tenant` never introduce it. Existing note
> paths are not overwritten, `coord-*` wire notes are excluded, malformed
> notes are recorded without making startup fail, and a completion marker
> prevents repopulation on later boots.

## Goal

Replace the external `lw2studymcp` Study Notes MCP with a **native**
knowledge layer inside the LunarWing daemon: durable, verified codebase-study
findings shared between agents, backed by the same PostgreSQL database as the
rest of the workspace — no external server, no second database left behind
when a tenant migrates hosts.

## Data model

Study notes are **ordinary workspace documents**, not a new storage system:

- Path layout: `study-notes/<agent>/<topic>.md` — per-agent versions of one
  canonical topic slug.
- A `study-note` provenance tag in the document `metadata` JSON marks the row
  as a study note; the metadata also carries `topic`, `agent`, `title`,
  `status`, `confidence` (0.0–1.0), `studied_at`, `source_commit`, `tags`,
  `superseded_by`, and the Phase 2 provenance fields `submitted_by` and
  `session_id`.
- Lifecycle status: `active`, `stale`, `superseded`, `needs-verification`
  (`KnowledgeStatus` in `ic/src/workspace/knowledge.rs`).
- Full-text search reuses the existing `memory_chunks.content_tsv` GIN index
  with `websearch_to_tsquery` + `ts_rank_cd` ranking (FTS5 on libSQL).
- Migration **V26** (`ic/migrations/V26__study_notes_knowledge.sql`, both
  backends) adds a partial index on `(user_id, metadata->>'topic')` for
  study-note scans.
- Scopes: the caller's **private** tenant scope plus the well-known
  **`shared`** herd scope readable by every agent. Reads span both by
  default; writes default to private, `scope="shared"` opts into herd
  knowledge.

Because notes live in `memory_documents`, they inherit the whole workspace
pipeline: chunking, FTS, multi-scope reads, versioning — and tenant
export/import (see Kawarimi below).

## Phase 1: Core knowledge store (item #1)

Five native built-in tools in `ic/src/tools/builtin/knowledge.rs` (seven since
the v2.0.8.0 parity pass — see the item #10 addendum above), registered
next to the memory tools for both single-tenant and multi-tenant workspace
resolvers (`ic/src/app.rs`), and protected from dynamic shadowing in
`PROTECTED_TOOL_NAMES`:

- `knowledge_search` — ranked PostgreSQL FTS with topic/agent/status/tags
  filters and scope narrowing; best snippet per note with provenance.
- `knowledge_get_topic` — every per-agent version of one canonical topic with
  full content and provenance, so agents can compare conflicting findings.
- `knowledge_submit_note` — validated write path (slug/title/content/tag
  limits, confidence 0.0–1.0, RFC 3339 timestamps); per-agent versioning with
  `replace=true` required to update your own agent's version.
- `knowledge_list_stale` — notes needing verification by status, age
  (`max_age_days`), or `source_commit` mismatch.
- `knowledge_stats` — totals, distinct topics, per-status and per-scope
  counts.

DB access goes through new `knowledge_search`, `knowledge_topic_versions`,
and `knowledge_list_notes` trait methods implemented for both the PostgreSQL
and libSQL backends.

## Phase 2: Lifecycle integration (item #2)

### Kawarimi travel (no external database left behind)

Notes are ordinary `memory_documents` rows, so they move with the tenant:

- `ic/scripts/export-tenant.sh` dumps the **whole** tenant database
  (`pg_dump -Fc`, no table exclusions) into the kawarimi bundle.
- On import, the owner-scope rekey in
  `ic/scripts/mt-admin-lib/10-owner-scope.sh` (invoked via
  `ic/scripts/lunarwing-mt-admin.sh`)
  covers `memory_documents` (with content salvage on collisions), so study
  notes are rekeyed to the new tenant scope like any other workspace
  document.
- This contract is pinned by regression assertions in
  `ic/scripts/tests/test-kawarimi-import-flags.sh` (whole-database dump, no
  `memory_documents`/`memory_chunks` exclusions, rekey coverage, migration
  doc record) and documented in `docs/ops/MT-MACHINE-MIGRATION.md`.

### Self-improving skills pipeline write-through

The bundled `study-notes` skill (`ic/skills/study-notes/SKILL.md`, v3.0.0)
was rewritten around the native `knowledge_*` tools: the study routine now
searches and submits through the native write path. The `LW2STUDYMCP_HOME`
env gate and `python3` requirement were dropped — the native store needs no
external server. The legacy external MCP remains an optional, read-only by
default integration; its tools are name-prefixed (`study_notes_*`) so they
cannot collide with the native tools, which are primary
(`ic/src/tools/README.md`).

### Background stale-detection sweep

`ic/src/workspace/knowledge_sweep.rs` (modeled on memory hygiene) marks
`active` notes `stale` once `studied_at` (falling back to the document's
last update) ages past the threshold. Notes already `stale`, `superseded`,
or `needs-verification` are untouched. The shared-scope pass also reaps
**stranded coordination wire notes** (`coord-*` writes whose ingest never
finished) once they age past a 24-hour grace period
(`STRANDED_WIRE_GRACE_HOURS`); a note still inside the grace window is never
removed, so an in-flight ingest is safe.

- Config: `ic/src/config/knowledge.rs` —
  `KNOWLEDGE_SWEEP_ENABLED` (default true),
  `KNOWLEDGE_STALE_AFTER_DAYS` (default 30),
  `KNOWLEDGE_SWEEP_CADENCE_HOURS` (default 24),
  `KNOWLEDGE_SWEEP_INCLUDE_SHARED` (default true); all documented in
  `ic/.env.example`.
- Cadence-guarded via a persisted state file
  (`~/.lunarwing/knowledge_sweep_state.json`) plus a global `AtomicBool`
  concurrency guard, so overlapping heartbeat ticks cannot double-mark.
- Sweeps the private scope and (optionally) the shared herd scope with a
  per-scope workspace handle, because metadata writes are primary-scope
  guarded.
- Wired `main.rs` → `Agent::set_knowledge_sweep_config` →
  `spawn_heartbeat_with_sweep`; runs in a background task on each heartbeat
  tick next to memory hygiene. Best-effort: failures are logged, never
  propagated.

### Agent/session provenance attribution

`knowledge_submit_note` stamps every note with the **real caller scope**
(`submitted_by`, from `JobContext::user_id`) and the conversation/job
**session id** (from `JobContext::conversation_id`, falling back to the job
id), so shared-herd notes keep attribution beyond the self-declared `agent`
slug. Metadata roundtrips through JSON; notes written before Phase 2 parse
with the new fields defaulting to absent (serde defaults, covered by tests).

## Phase 3: New-tenant checkout seed (v2.0.9.0)

`ic/src/workspace/knowledge_seed.rs` imports canonical Markdown and metadata
from `$LW2STUDYMCP_HOME/agent-notes-by-agent/<agent>/<topic>.md` when both the
checkout path and `KNOWLEDGE_SEED_FROM_STUDY_MCP=1` are present. `AppBuilder`
starts the pass in a background task after the workspace is available.

The seed is deliberately narrower than routine MCP configuration:

- `provision_tenant_study_mcp()` syncs the checkout, configures it, and then
  calls `arm_tenant_knowledge_seed()` only during a successful new-tenant
  `add-tenant --with-study-mcp` flow.
- `configure-study-mcp` does not arm existing tenants. Environment regeneration
  preserves an existing flag but never invents one, and `upgrade-tenant` does
  not add it.
- Existing workspace paths are skipped. Only exact stranded rows created by a
  crashed prior seed may have missing content or provenance repaired; arbitrary
  operator content is never replaced.
- Invalid files increment the failure count but do not abort startup. Once all
  discovered notes have been walked, `knowledge_seed_state.json` records the
  pass, including failures, so later boots do not rescan. A missing checkout has
  no marker and can retry on a later boot.
- Agent/topic slugs and metadata are bounded with the native knowledge limits,
  pass order is deterministic, and the reserved `coord-*` wire namespace is
  excluded.

The external MCP remains independently configurable for agent tooling. The
one-shot seed uses its checkout only as import input; native notes remain normal
workspace documents afterward.

## Verification

- Phase 1: 19 tests (15 unit + 4 libSQL integration over real migrated DBs),
  live PostgreSQL 18 SQL validation, clippy `-D warnings`, rustfmt, all three
  feature-combo `cargo check`s.
- Phase 2: 7 sweep tests including 3 libSQL integration tests (cross-scope
  marking, non-active statuses untouched, cadence skip), bundled-skill test
  asserting the native write path and the dropped env gate, kawarimi shell
  harness assertions, provenance roundtrip + legacy-parse tests.
- Phase 3: 16 Rust seed tests cover metadata preservation, existing-path and
  stranded-row behavior, malformed files, marker/retry semantics, deterministic
  discovery, and reserved wire namespaces. `test-study-mcp-sync.sh` pins the
  add-only arm, explicit opt-out preservation, non-arming configure path, and
  env-regeneration contract. Live systemd and OpenRC tenant exercises verified
  new-tenant population, restart stability, and upgrade exclusion.

## Trust model

Study Notes is a research index, not authority. Distinct agent versions
remain separate; provenance (`agent`, `submitted_by`, `session_id`) shows
origin, not quality. The sweep marks aging notes, but `active` is not proof
of current correctness — agents are instructed (SKILL.md) to verify stale,
conflicting, or security-sensitive claims against current source before
relying on them.
