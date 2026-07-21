# hermes_kawarimi

A **Kawarimi adapter**: import a [Hermes](../hermes-agent) agent into a LunarWing
tenant. Hermes keeps its state mostly in markdown under `$HERMES_HOME`; LunarWing
stores an agent as PostgreSQL rows + encrypted secrets + a state dir. This package
reads the former and writes the latter via **provision-then-populate**.

It is the Hermes analogue of `lunarwing_mt_onboard`'s Kawarimi machine migration
(`ic/scripts/export-tenant.sh` / `import-tenant.sh`), and reuses that package's
secrets crypto and mt-admin provisioning rather than reinventing them.

## Architecture

```
hermes agent dir ($HERMES_HOME)
   │  extract.py   (read-only)
   ▼
HermesAgentSnapshot            neutral in-memory IR
   │  mapper.py    (pure, deterministic uuid5)
   ▼
MappedAgent                    owner-agnostic LunarWing rows
   │  loader.py    (--apply only)
   ▼
mt-admin add/build/start → schema → stop → populate live DB + secrets
```

**Why start-then-stop for schema:** LunarWing creates its schema via the daemon's
refinery runner at first boot (`ic/src/app.rs`), and `V1__initial.sql` uses bare
`CREATE TABLE`, so hand-applying SQL would collide. The loader boots the tenant
once to create the schema, stops the daemon (PostgreSQL stays up), then populates
with no daemon racing.

**Owner-scope safety:** every row is written under the freshly-provisioned
tenant's `LUNARWING_OWNER_ID`, so the `KAWARIMI-OWNER-SCOPE-CONTINUITY`
silent-empty hazard (which comes from `pg_restore` reloading foreign scopes)
structurally cannot occur here — there is no restore and no rekey.

## Mapping

| Hermes source | LunarWing target |
|---|---|
| `SOUL.md`, `memories/MEMORY.md`, `memories/USER.md`, other `memories/*.md` | `memory_documents` (path + content) |
| `config.yaml` (raw) | `memory_documents` `imported/hermes-config.json` (reference copy) |
| `config.yaml` (top-level scalars) | `settings` keys `hermes.<key>` (namespaced, never clobbers LunarWing's own keys) |
| `state.db` `sessions` / `messages` | `conversations` / `conversation_messages` (deterministic `uuid5` ids) |
| `.env` + `auth.json` (credentials) | `secrets` (AES-256-GCM via `secrets_ops.insert_secret`) |

## Usage

```bash
pip install -r hermes_kawarimi/requirements.txt

# Read-only: report what would migrate (no root, no DB)
python3 -m hermes_kawarimi inspect --source /path/to/hermes/home

# Dry-run import (default — prints the plan, writes nothing)
python3 -m hermes_kawarimi import --source /path/to/hermes/home --tenant kageho

# Execute (root on a LunarWing host): provision + populate, leave STAGED
sudo python3 -m hermes_kawarimi import --source /path/to/hermes/home \
  --tenant kageho --apply

# Execute and start the tenant
sudo python3 -m hermes_kawarimi import --source /path/to/hermes/home \
  --tenant kageho --apply --start
```

`--source` defaults to `$HERMES_HOME`, then `~/.hermes` (matching Hermes' own
resolution). Import flags: `--apply`, `--start`, `--force` (reuse an existing
tenant name), `--with-nanocode/--with-pebble/--with-opencode`, `--docker-group`,
`--tensorzero-url`, `--llm-model`, `--save FILE` / `--resume FILE`.

## Module layout

```
hermes_kawarimi/
├── model.py        # HermesAgentSnapshot IR + Mapped* row dataclasses
├── extract.py      # $HERMES_HOME -> snapshot (read-only)
├── mapper.py       # snapshot -> MappedAgent (pure, deterministic)
├── loader.py       # --apply: provision + populate live DB + secrets
├── config.py       # ImportPlan (JSON save/resume)
├── cli.py          # `inspect` + `import`
├── model_tests.py / extract_tests.py / mapper_tests.py
└── requirements.txt
```

## Testing

```bash
# Unit tests (no root, no DB)
python3 -m unittest hermes_kawarimi.model_tests \
  hermes_kawarimi.extract_tests hermes_kawarimi.mapper_tests
```

The live `--apply` path is validated on a LunarWing host with root + container
runtime (mirrors the KAWARIMI rehearsal): after import, rows exist under
`LUNARWING_OWNER_ID` and a stored secret decrypts. Rollback:
`sudo lunarwing-mt-admin.sh remove-tenant <tenant>`.

## Scope

**v1 (Phase 1):** memory markdown, conversations/messages, credentials, curated
settings; PostgreSQL only (fails closed on libsql, matching Kawarimi).

**Phase 2:** `kanban.db` → `agent_jobs`; Hermes `skills/` → LunarWing wasm tools;
`memory_chunks` embeddings (regenerated in-runtime); libsql tenants; an
interactive wizard. See `docs/proposals/HERMES-KAWARIMI-ADAPTER.md`.
