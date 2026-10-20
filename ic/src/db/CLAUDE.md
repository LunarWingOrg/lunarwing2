# Database Module

Shared dual-backend database abstraction for LunarWing.

## Backends

- **PostgreSQL** (primary): `ic/src/db/postgres.rs`
- **libSQL**: `ic/src/db/libsql/` (split into per-domain files)

## Trait

`Database` trait is defined in `ic/src/db/mod.rs`. It declares ~140 async
methods spanning conversations, jobs, sandbox jobs, routines, tool failures,
settings, workspace/memory, identities, reflex, and users.

### Multi-scope methods

Four `_multi` methods have **default trait implementations** that delegate to
single-scope versions:

- `hybrid_search_multi` — Postgres overrides with a combined RRF query; libSQL
  uses the default per-scope merge.
- `list_all_paths_multi`
- `get_document_by_path_multi`
- `list_directory_multi`

Both backends must implement the single-scope versions; the `_multi` variants
are optional overrides.

## libSQL feature surface (verified 2026-07-26)

- `cargo check --no-default-features --features libsql` passes.
- No `todo!()` or `unimplemented!()` stubs in any DB module file.
- libSQL modules: `conversations`, `identities`, `jobs`, `mod`, `reflex`,
  `routines`, `sandbox`, `settings`, `tool_failures`, `users`, `workspace`.
- Migrations: `ic/src/db/libsql_migrations.rs`.

## libSQL remote replicas / Turso sync (WI#331, v2.1.6.0)

- The `libsql` dependency is compiled with `features = ["core"]` only.
  Turso remote replicas live behind the non-default
  **`libsql-remote-replica`** cargo feature (`libsql/remote +
  libsql/replication + libsql/tls`): the libsql 0.6.0 remote chain pins
  rustls-webpki 0.102.8 / h2 0.3.27 (accepted RustSec advisories,
  `ic/deny.toml`), so it stays out of default builds. The default-feature
  graph is pinned clean by
  `.gitlab/scripts/check-default-graph-advisories.sh`.
- Connection-mode selection is centralized in
  `LibSqlBackend::from_config_parts(path, url, token)`
  (`ic/src/db/libsql/mod.rs`); all `db/mod.rs` connect paths and the setup
  wizard's libSQL connection test route through it. A URL without an auth
  token is rejected in every build before a local database is opened.
  Without the feature, a configured remote replica fails closed with an
  error naming the feature — never a silent local-only fallback (a local
  replica file without sync diverges from the Turso primary).
- `new_remote_replica` exists only under `#[cfg(feature =
  "libsql-remote-replica")]`; keep new remote-replica call sites behind the
  same cfg and route config-driven connections through `from_config_parts`.

## Rules

- Add new DB operations to the shared `Database` trait first, then implement
  both backends.
- If libSQL work would be disproportionate, PostgreSQL-only is acceptable with
  a note here.
- Do not regress existing libSQL coverage.

## Cron tick retention primitives (WI#370, v2.1.6.0)

`SandboxStore` carries two methods backing the cron supervisor's per-mission
retention sweep. Both backends implement them; no schema change is involved.

- `list_cron_mission_ticks(user_id, job_mode, title_prefix)` — the mission's
  tick rows, newest first. Matching is a prefix test on the bound parameter
  (PostgreSQL `starts_with` on bytes; libSQL `substr`/`length`, which counts
  TEXT characters — the same result for this comparison) — never LIKE, so no
  wildcard escaping exists to get wrong. Tick rows are identified by the
  supervisor-authored prompt-title prefix (`cron_worker::
  cron_tick_title_prefix`), scoped to the mission worker's `job_mode` and the
  owner; ordinary jobs, other missions' ticks, and other owners' rows are
  never returned. `create_job` refuses an ordinary task starting with that
  reserved opening, so no caller can forge the marker.
- `delete_sandbox_job_cascade(job_id)` — deletes the row's `job_events` first,
  then the `agent_jobs` row (`AND source = 'sandbox'`), because
  `job_events.job_id` references `agent_jobs(id)` with **no** `ON DELETE
  CASCADE`. A row-first delete would violate the FK. Returns whether the job
  row existed. `job_actions`/`llm_calls`/`estimation_snapshots` cascade on the
  row delete themselves. Two operational notes (LW-10): the delete is two
  autocommit statements, not one transaction — a row delete failing after its
  event delete leaves the row without events until the next sweep; and
  because `llm_calls` cascade with the row, `user_usage_stats`/
  `user_summary_stats` aggregates (built from `llm_calls`) shrink if tick jobs
  ever record LLM calls — usage reporting does not preserve swept ticks.

The PostgreSQL FK semantics (event-for-missing-row refused; delete order;
re-delete idempotency; listing scoping) are pinned by
`ic/tests/postgres_cron_retention.rs`, which runs in the
`rust-test-postgres-integration` CI lane — the generic
`external_worker_integration` target is auto-discovered only by the libSQL
lane.

## Permission upgrade (V28 / WI#307)

Both backends remove `tool_permissions.secret_delete` rows whose JSON value
is `"always_allow"`, across every user scope. Disabled/ask settings and other
tools are retained. Execution rejects these legacy grants independently of the
migration. This data cleanup is intentionally irreversible: downgrading does
not restore blanket credential-deletion grants.

libSQL checks `json_valid` before extraction so malformed legacy TEXT rows do
not abort startup. Live data selectivity and repeat execution are covered by
the libSQL migration regression and `tests/postgres_settings_migrations.rs`,
which runs in the PostgreSQL integration CI job against a session-local table.

## Study-Notes knowledge store (v2.0.6)

The `WorkspaceStore` sub-trait carries three multi-scope knowledge methods
(`knowledge_search`, `knowledge_topic_versions`, `knowledge_list_notes`) for
the study-notes knowledge layer. Study notes are ordinary `memory_documents`
rows tagged `metadata->>'provenance' = 'study-note'`; FTS runs on the existing
`memory_chunks` tsvector (PostgreSQL) / `memory_chunks_fts` FTS5 (libSQL).
Both backends are implemented; migration V26 adds the partial metadata index
on each side. `knowledge_list_notes` takes a `(updated_at, path, user_id)`
keyset cursor so `Workspace::knowledge_scan_all` can page through the whole
store instead of a fixed most-recent window (WI#121).
