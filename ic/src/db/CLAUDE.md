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
