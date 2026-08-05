# IronClaw libSQL to Multi-Tenant LunarWing

There is currently **no maintained, production-safe cross-backend importer** from an arbitrary IronClaw/libSQL database into LunarWing's multi-tenant PostgreSQL schema.

The previous guide has been archived at [`history/MIGRATE_IRONCLAW_LIBSQL_TO_MT.md`](history/MIGRATE_IRONCLAW_LIBSQL_TO_MT.md). Do not execute its CSV and SQL snippets against a current tenant. They hardcode an old table/column set, expose a fixed PostgreSQL connection pattern, disable constraints, assume the historical V18 schema, and do not preserve newer user, owner-scope, extension, reflex, document-version, approval-policy, and runtime-exclusion semantics.

The helper files `ic/scripts/export-libsql.sh`, `ic/scripts/import-to-pg.sh`, and `ic/scripts/reimport-fixes.sh` are legacy one-off scripts, not an MT-admin import contract. They contain deployment-specific defaults and must not be treated as a supported migrator.

## Supported Choices

### Start a Fresh MT Tenant

This is the recommended path when preserving full conversation/job history is not required:

1. Provision the tenant through `ic/scripts/lunarwing-mt-admin.sh` or the thin onboarding wrapper.
2. Copy only reviewed non-database state, such as custom `workspace-template/` files and intentional project repositories.
3. Re-enter secrets through the target tenant's encrypted setup flows.
4. Recreate required routines and extension configuration through current APIs.
5. Keep the original libSQL database as a protected read-only archive.
6. Validate the new tenant with [`TESTING_GUIDE.md`](TESTING_GUIDE.md).

Do not copy the old database file into the PostgreSQL tenant state directory.

### Commission a Version-Specific ETL

If historical data must be preserved, treat the work as a database migration project tied to the exact source and target commits:

1. Freeze and copy the source database with SQLite's backup mechanism; do not copy a live database file byte-for-byte.
2. Inventory every source table, column, index, foreign key, migration version, row count, and owner/user scope.
3. Create a disposable target tenant and let the deployed LunarWing commit create its current PostgreSQL schema.
4. Define explicit per-table transforms for UUIDs, timestamps, JSON, booleans, arrays, encrypted values, vectors, nullable fields, renamed tables, and new required columns.
5. Import parent tables before children without globally disabling integrity checks. Validate constraints after every batch.
6. Preserve encrypted-secret rows only when the exact source master key is securely provisioned; otherwise re-enter secrets under the target key.
7. Rebuild embeddings from text rather than attempting an unverified BLOB-to-pgvector conversion.
8. Compare row counts, representative records, owner isolation, search behavior, routines, jobs, and conversation histories.
9. Run the migration repeatedly in staging until it is deterministic and rollback is proven.
10. Perform final cutover from a fresh quiesced source snapshot.

A one-off ETL should fail closed on unknown tables or columns and produce a redacted reconciliation report. It should never contain passwords, fixed tenant ports, or `SET session_replication_role = 'replica'` as a blanket workaround.

## Rollback

Keep the source IronClaw service, libSQL snapshot, and base directory untouched during rehearsal. To roll back, stop the target through MT admin, disable its message ingress, and restart the source against its original database and state. Do not purge the target or remove source backups until the rollback window closes.
