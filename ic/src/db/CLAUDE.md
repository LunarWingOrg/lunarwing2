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

## Rules

- Add new DB operations to the shared `Database` trait first, then implement
  both backends.
- If libSQL work would be disproportionate, PostgreSQL-only is acceptable with
  a note here.
- Do not regress existing libSQL coverage.

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
