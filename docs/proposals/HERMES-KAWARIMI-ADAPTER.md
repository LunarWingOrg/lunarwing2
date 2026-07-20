# Hermes → LunarWing Kawarimi Adapter — Proposal & Implementation

**Status:** Phase 1 implemented (v0.1.0) · **Package:** `hermes_kawarimi/` ·
**Date:** 2026-07-20

> A Kawarimi adapter that imports a Hermes agent into a LunarWing tenant.
> Read-only path (`inspect`, dry-run `import`) is implemented, unit-tested, and
> smoke-tested. The live `--apply` path is implemented and awaits rehearsal on a
> LunarWing host (root + container runtime), mirroring the KAWARIMI validation.

---

## 1. Problem

Hermes agents store their state mostly as markdown under `$HERMES_HOME`
(`SOUL.md`, `memories/MEMORY.md`, `memories/USER.md`) plus a SQLite `state.db`
(sessions/messages), `config.yaml`, and credentials in `.env` / `auth.json`.
LunarWing stores an agent ("tenant") in a much richer stack: a PostgreSQL DB
(`conversations`, `conversation_messages`, `memory_documents`, `settings`,
`secrets`), AES-256-GCM-encrypted secret rows keyed by `SECRETS_MASTER_KEY`, a
state dir, and env config.

We already have **Kawarimi** — LunarWing's machine-to-machine tenant migration
(`ic/scripts/export-tenant.sh` → bundle → `ic/scripts/import-tenant.sh`, wrapped
by `lunarwing_mt_onboard`). We want an analogous, **safe** path for
`hermes → lunarwingv2`.

## 2. Design decision — provision-then-populate

```
hermes agent dir ($HERMES_HOME)
   │  extract.py   (read-only)
   ▼
HermesAgentSnapshot   (neutral IR)
   │  mapper.py     (pure, deterministic uuid5)
   ▼
MappedAgent   (owner-agnostic LunarWing rows)
   │  loader.py     (--apply only)
   ▼
mt-admin add-tenant + build-tenant + install-wasm   (empty tenant)
   → start-tenant (daemon runs migrations = SCHEMA) → stop-tenant (PG stays up)
   → populate live DB + encrypt secrets → start-tenant (if --start) else STAGED
```

**Rejected alternative — "Kawarimi bundle producer"** (emit a `meta.txt` +
`db.dump` + manifests + `state.tar.gz` for `import-tenant.sh`): it would require
generating a valid `pg_dump -Fc` binary archive from markdown without a live
Postgres — impractical. Provision-then-populate reuses the battle-tested mt-admin
provisioning and the exact secrets crypto while writing rows against a known,
live schema.

**Why start-then-stop to create the schema:** LunarWing creates its schema via
the daemon's refinery runner at first boot (`ic/src/app.rs` `run_migrations` +
auto-onboard). `V1__initial.sql` uses bare `CREATE TABLE` (not `IF NOT EXISTS`),
so hand-applying the SQL would collide with refinery's own run. The loader boots
the tenant once (schema created), stops the daemon (PostgreSQL container stays up,
per the export model), then populates with no daemon racing.

**Key safety win over the bundle path:** the entire
`KAWARIMI-OWNER-SCOPE-CONTINUITY` silent-empty hazard comes from `pg_restore`
reloading foreign `user_id` scopes. Here there is **no restore** — every row is
written under the freshly-provisioned tenant's `LUNARWING_OWNER_ID` from the
start. No owner-scope rekey, no silent amnesia.

## 3. Mapping

| Hermes source | LunarWing target | Notes |
|---|---|---|
| `SOUL.md` | `memory_documents` `SOUL.md` | persona as workspace doc |
| `memories/MEMORY.md` / `USER.md` | `memory_documents` `MEMORY.md` / `USER.md` | ~1:1 |
| other `memories/*.md` | `memory_documents` `memories/<name>.md` | tree preserved |
| `config.yaml` (raw) | `memory_documents` `imported/hermes-config.json` | non-destructive reference copy |
| `config.yaml` (top-level scalars) | `settings` `hermes.<key>` | namespaced so it never clobbers LunarWing keys (`agent.*`, `sandbox.*`) |
| `state.db` `sessions` | `conversations` | `id=uuid5(session)`; `channel←source`; `user_id←owner`; `thread_id←thread_id/chat_id`; `metadata` stashes source/title/model/profile/original id |
| `state.db` `messages` | `conversation_messages` | `id=uuid5(session,msg)`; NULL/tool-call content coalesced (`content` is NOT NULL) |
| `.env` + `auth.json` | `secrets` (AES-256-GCM) | via `secrets_ops.insert_secret`; `auth.json` carried whole as `hermes_auth_json` |
| `kanban.db`, `skills/`, embeddings | — | Phase 2 |

`memory_documents.agent_id` = NULL (shared for the owner). Because
`UNIQUE(user_id, agent_id, path)` treats NULL as distinct, memory docs are
upserted on their natural key manually (UPDATE-then-INSERT) rather than
`ON CONFLICT`, keeping re-import idempotent.

## 4. Components

```
hermes_kawarimi/
├── model.py     HermesAgentSnapshot IR + Mapped* row dataclasses
├── extract.py   $HERMES_HOME -> snapshot (read-only, schema-drift tolerant)
├── mapper.py    snapshot -> MappedAgent (pure, deterministic uuid5)
├── loader.py    --apply: provision + populate live DB + secrets
├── config.py    ImportPlan (JSON save/resume)
├── cli.py       `inspect` (read-only) + `import` (dry-run default)
└── *_tests.py   model / extract / mapper unit tests
```

**Reused (not reinvented):** `lunarwing_mt_onboard.secrets_ops`
(`insert_secret`, `parse_tenant_env`, AES-256-GCM + HKDF-SHA256 matching the Rust
runtime), `lunarwing_mt_onboard.provisioner` (mt-admin wrappers, `PhaseResult`),
`lunarwing_mt_onboard.config.TenantConfig.validate_name`.

## 5. Safety & guardrails

- **Dry-run default.** Writes only on `--apply`; `--start` is a separate opt-in.
  The live path (psycopg2/root) is imported lazily so dry-run needs neither.
- **Owner-scope continuity is inherent** (all rows minted under the tenant owner).
- **Postgres-only in v1**, fails closed on libsql (Kawarimi precedent,
  `export-tenant.sh:84`).
- **Secrets never on argv**; reuse the runtime-matching crypto; secret names
  validated (`^[a-zA-Z0-9_/-]+$`).
- **Idempotent** re-import: deterministic `uuid5` + `ON CONFLICT DO UPDATE`
  (conversations/messages/settings) and natural-key upsert (memory docs);
  `--force` to reuse a tenant name.
- Hermes markdown is untrusted content, stored as **data**, never executed.
- Refuses if the tenant already exists in `/etc/lunarwing/ports.json` unless
  `--force`.

## 6. Verification

- **Unit (no root/DB) — passing:**
  `python3 -m unittest hermes_kawarimi.model_tests hermes_kawarimi.extract_tests hermes_kawarimi.mapper_tests`
  (12 tests; the config test skips when pyyaml is absent).
- **Dry-run — smoke-tested:** `inspect` and `import` (without `--apply`) render a
  correct migration report against a synthetic `$HERMES_HOME` and write nothing.
- **End-to-end (pending rehearsal on a LunarWing host, root + throwaway tenant):**
  `sudo python3 -m hermes_kawarimi import --source <hermes_home> --tenant testimport --apply`;
  confirm rows exist under `LUNARWING_OWNER_ID` and a stored secret decrypts.
  Rollback: `sudo lunarwing-mt-admin.sh remove-tenant testimport`.

## 7. Phasing

- **Phase 1 — DONE:** package scaffold, extractor, mapper, loader, `ImportPlan`,
  CLI (`inspect` + dry-run/apply `import`), unit tests, docs.
- **Phase 2 — deferred:** `kanban.db` → `agent_jobs`; Hermes `skills/`
  (executable) → LunarWing wasm tools; `memory_chunks` embeddings (regenerate
  in-runtime); libsql tenants; state-dir/workspace file copy; interactive wizard;
  secret decrypt-verify in the loader.

## 8. Open questions

1. **Settings fidelity:** is a namespaced `hermes.<scalar>` subset + a full
   reference doc sufficient, or should we map specific Hermes keys onto
   LunarWing's own settings (`agent.name`, model, …)?
2. **Memory doc paths:** should `MEMORY.md`/`USER.md` land at the workspace root
   (current) or under an `imported/` prefix to avoid colliding with LunarWing's
   own conventions?
3. **Conversation volume:** very large `state.db` histories — import all, or add a
   `--since` / archived-session filter?
