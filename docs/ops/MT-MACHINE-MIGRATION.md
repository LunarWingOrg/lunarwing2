# Multi-Tenant Machine Migration (old host → fresh v2 host)

**When to use this instead of an in-place upgrade:** when moving a tenant to a
different host or deliberately rebuilding on a clean v2 host. Each tenant goes
through the normal `add-tenant` path, while the old host remains available for
rollback. This is the recommended cross-host path for important tenants.

**Tools:** `ic/scripts/export-tenant.sh` (old host) and `ic/scripts/import-tenant.sh`
(new host).

**Cross-init:** both scripts work for **systemd and OpenRC**. Export only needs to
stop two per-tenant units (auto-detecting the init); import delegates all
init/runtime-specific work to `lunarwing-mt-admin.sh`, which auto-detects systemd vs
OpenRC and rootless-podman vs rootful-docker. The new host's init system need not
match the old host's.

> **PostgreSQL only.** Machine migration supports the postgres backend; both scripts
> refuse a libSQL tenant rather than risk a silent empty import (migrate a libSQL DB
> file by hand if needed).

> **Export is the start of cutover, not a read-only snapshot.** To capture a
> consistent *and final* picture, `export-tenant.sh` **stops the tenant's daemon +
> xmpp-bridge** before dumping (PostgreSQL stays up just for `pg_dump`). The agent is
> down from export until you start it on the new host — plan a per-tenant maintenance
> window. This is deliberate: it prevents a torn OMEMO store and prevents losing
> anything written between snapshot and cutover.

> **Validation status.** A historical plaintext path was live-validated in 2026.
> The current encrypted path has automated production-script coverage for export,
> wrong-password rejection, decrypt/import dry-run, manifest restrictions, owner-scope
> planning, and passphrase safety. A real tenant cutover must still be rehearsed and
> verified for database rows, encrypted secrets, workspace state, and OMEMO continuity.

---

## What moves, and why

| Item | Where it lives | Carried? |
|------|----------------|----------|
| Conversations, memory, routines, reflexes, settings, **encrypted secret rows** | PostgreSQL | **Yes** — `db.dump` (`pg_dump -Fc` → `pg_restore`) |
| **`SECRETS_MASTER_KEY`** | env | **Yes (must match)** — the AES-256-GCM key; without it every encrypted secret row is unrecoverable. Injected + verified verbatim before the daemon starts. |
| **OMEMO store** (encrypted-XMPP device identity/sessions) | `state/xmpp/` on disk | **Yes** — `state.tar.gz`, snapshotted with the bridge stopped (consistent) |
| Workspace identity/memory files, WASM tool storage | `state/` on disk | **Yes** — `state.tar.gz` |
| `XMPP_JID` + `XMPP_PASSWORD` | env | **Yes** — same account login |
| XMPP rooms / allowlist / encrypted-rooms / plaintext-fallback, OMEMO device id, `LLM_API_KEY`/`MODEL` | env (+ bridge `*_JSON`) | **Yes** — operator config |
| `LLM_BASE_URL`, `NANOCODE_BASE_URL`, `OPENCODE_BASE_URL` | env | **Only when non-loopback/custom** — obsolete host-local proxy URLs are not carried |
| `NANOCODE_MODEL`, `OPENCODE_MODEL` | env | **Yes** — worker model overrides |
| Vision sidecar VL backend/model + auth token | `env/vision.env` | **Yes, when present** — `VL_URL`, `VL_MODEL`, and `LUNARWING_AUTH_TOKEN` are carried in `manifest-vision.env`; vision ports remain host-specific and regenerated |
| Gateway / HTTP **bind address** (`GATEWAY_HOST`, `HTTP_HOST`) | env | **Yes** — operator config (e.g. `0.0.0.0` for remote access vs the `127.0.0.1` default), so your binding choice survives the migration. The **ports** (`GATEWAY_PORT`/`HTTP_PORT`) are NOT carried — they're host-specific and regenerated. (If you bound to a host-specific NIC IP rather than `0.0.0.0`/`127.0.0.1`, review it on the new host.) |
| Gateway / bridge / webhook / relay tokens | env | **No** — the new host mints fresh, self-consistent ones (keeps `config.toml`'s worker auth aligned). **Gateway-UI and external-webhook clients re-authenticate after cutover.** |
| `state/config.toml` (external-worker ports/tokens) | `state/` | **No** — host-specific; the new host's `add-tenant` writes the correct one |
| WASM `.wasm` artifacts | `state/tools`, `state/channels` | **No** — rebuilt on the new host (`build-tenant --with-wasm` + `install-wasm`) |
| Ports, paths, obsolete host-local proxy URLs, docker/podman group membership | `ports.json` / env / OS groups | **No** — regenerated or chosen on the target host. Use import's `--docker-group` when the target host needs explicit group membership. |

---

## Owner-scope (`user_id`) continuity — the v1.1.7 data migration

**There is no new SQL/schema migration on this path.** Refinery migrations `V1..V21` are
byte-identical from v1.1.0 through v1.1.7, and a fully-migrated source DB restores already
at `V21`, so the daemon's refinery run on first start applies nothing and does not abort.

**The migration that *does* matter is `migrate_owner_scope`** — a shell-level *data*
migration in `lunarwing-mt-admin.sh`. It rekeys the `user_id` ("owner scope") column from
the source scope to the tenant name across 13 tables (`settings`, `conversations`,
`memory_documents`, `secrets`, `routines`, `agent_jobs`, `reflex_patterns`,
`user_identities`, `api_tokens`, `heartbeat_state`, `tool_rate_limit_state`,
`secret_usage_log`, `wasm_channels`). Current `import-tenant.sh` performs this reconciliation
immediately after restore, while PostgreSQL is running and before state restoration or start.

**Why it is make-or-break:** `pg_restore` reloads each row's `user_id` *verbatim*, while
the new daemon is hard-scoped to the new tenant name. If the two don't line up after start,
the daemon **silently sees an empty dataset** — it looks like total history/memory loss,
but the rows are intact under the old scope. Success depends on owner-scope continuity, not
on any schema step.

**Measure the source scope before you export** (read-only, on the old host — `docker`
shown for a rootful source; use the tenant's `podman` on a rootless source):

```bash
sudo docker exec lunarwing-pg-<tenant> \
  psql -U lunarwing -d lunarwing -c \
  "SELECT user_id, count(*) FROM settings GROUP BY 1 ORDER BY 2 DESC;"
```

Then pick the branch and, when needed, provide the source scope to import:

| Source `user_id` | What happens / what you do |
|------------------|----------------------------|
| `default` (typical older tenant) | Keep the same tenant name. Import detects and rekeys `default → <tenant>` automatically. |
| already `<tenant>` | Keep the same name. Import verifies that no non-target scope needs rekeying. |
| some other value `S` | Pass `--owner-scope S`. Import verifies the restored scope matches, rekeys it, and fails closed if it does not. A rename via `--name` normally needs this flag. |
| multiple non-target scopes | Import aborts. Inspect and clean the source database before retrying; it will not guess. |

Still verify the rekey after import. The automated flow inspects restored scopes, rejects
ambiguity, runs the migration, and rechecks for remaining non-target scopes. An independent
query remains useful evidence that rows are under `<tenant>` with zero `default` remaining.

If independent verification later finds a stale source scope, keep the target tenant stopped
and investigate before running a manual `migrate-owner-scope`. Current import should fail
before start when reconciliation is ambiguous or incomplete; do not bypass that gate.

The source measurement is still recommended because it tells you whether `--owner-scope` is
required before the maintenance window begins; it is no longer a substitute for import-side
validation.

---

## Prerequisites on the new (standalone) host

A current v2 host: PostgreSQL via rootless Podman or rootful Docker, reachability to the
configured LLM endpoint and the same XMPP server, Gotify when used for escalation, and a
current `lunarwing-mt-admin.sh` with `restore-tenant` and owner-scope inspection. See
`docs/ops/MULTITENANCY-PRODUCTION.md` and `docs/guides/MT-ADMIN-QUICKSTART.md`. Ensure
`jq`, `tar`, `7z`/p7zip, and the container runtime are present on both hosts.

> **Rootless-podman + docker-CLI gotcha.** `mt-admin` selects the container runtime by which
> binary is present: if the `docker` CLI is installed — **even with no running docker
> daemon** — it picks **docker**, and every per-tenant container op (PG provisioning, restore,
> rekey, worker build) fails against a dead socket. On a rootless-podman host that also has the
> docker CLI, force podman explicitly: `LUNARWING_CONTAINER_RUNTIME=podman` (with
> `LUNARWING_MT_ROOTLESS=true`). It **must survive `sudo`**, which strips your shell env — pass
> it inline on every mt-admin call:
> `sudo env LUNARWING_CONTAINER_RUNTIME=podman LUNARWING_MT_ROOTLESS=true ic/scripts/lunarwing-mt-admin.sh …`
> Cleaner long-term: remove the unused docker CLI (detection then lands on podman on its own),
> or add the var to the admin's `sudoers` `env_keep`.

---

## Procedure (per tenant — one maintenance window each, canary first)

### 1. Export on the OLD host (begins the cutover)
> **First, measure the source owner scope** (read-only) — it decides whether you keep the
> tenant name and whether import needs `--owner-scope <S>`. See *Owner-scope continuity*
> above.
```bash
sudo ic/scripts/export-tenant.sh <tenant> --dry-run  # preview; no passphrase needed
sudo ic/scripts/export-tenant.sh <tenant>            # prompts before stopping services
# stops the tenant's daemon + bridge, dumps the DB, snapshots state, writes
# /var/lib/lunarwing-migrate/<tenant>-migrate-<stamp>.7z  (AES-256, 0600, root)
```
The tenant is now stopped on the old host (PostgreSQL left up for the dump; you may
stop it too). Use `--no-quiesce` only if you've already stopped the daemon+bridge
yourself.

### 2. Transfer the bundle securely
```bash
rsync -av -e ssh old-host:/var/lib/lunarwing-migrate/<tenant>-migrate-<stamp>.7z /var/lib/lunarwing-migrate/
```
The bundle contains `SECRETS_MASTER_KEY` and the XMPP password — treat it like a key,
keep mode `0600`, and delete it from both hosts once the import is verified.

### 3. Import + cut over on the NEW host
```bash
sudo ic/scripts/import-tenant.sh /var/lib/lunarwing-migrate/<tenant>-migrate-<stamp>.7z \
     --start --old-stopped \
     [--with-nanocode] [--with-pebble] [--with-opencode] [--with-toolchains] \
     [--with-vision] [--docker-group]   # --dry-run first
```
Runs `add-tenant --no-health` → `build-tenant` → injects the carried secrets (incl.
`SECRETS_MASTER_KEY`, verified verbatim) → `restore-tenant` (DB) → restores `state/`
(OMEMO) → `install-wasm` → runs the read-only WeeChat migration preflight → starts the
daemon. `--with-opencode` and `--with-toolchains` are passed through to
`build-tenant`. `--with-vision` builds the LunarVision sidecar image before start; if
the bundle contains `manifest-vision.env`, import injects the carried `VL_URL`,
`VL_MODEL`, and sidecar auth token into the target `vision.env`. Docker/podman
group membership is a target-host choice and is not exported from the source host.
`--old-stopped` confirms the old side is down (export already stopped it);
omit `--start` to **stage** without starting, or omit `--old-stopped` to be prompted
interactively. The double-login confirmation is **not** satisfied by `--yes` alone.

The WeeChat preflight checks the restored tenant env against the target port
registry and installed capabilities before cutover. A preflight FAIL blocks import
so a migrated tenant does not start with wrong relay/adapter ports; warnings are
reported as the usual backfill guidance.

> **Owner-scope flag:** if the source `user_id` was neither `default` nor the target tenant
> name, add `--owner-scope <S>` to the import command. Import performs and verifies the rekey
> before it can reach the start phase.

> **Migrating into a host with self-heal already armed?** If the target runs the fleet health
> pipeline (`lunarwing-mt-health.timer`), it will **restart the tenant daemon** as soon as
> `add-tenant` renders its unit — reviving it mid-import and re-contaminating the DB between
> `restore-tenant` and the rekey (the revived daemon bootstraps fresh `<tenant>`-scoped rows
> that then collide with the restored `default` rows). Pause it for the whole migration window:
> `sudo systemctl stop lunarwing-mt-health.timer lunarwing-mt-health.service` (system-level, not
> `--user`), do restore → rekey → start with the daemon staying down, verify, then re-arm:
> `sudo systemctl start lunarwing-mt-health.timer`. It's host-global, so nothing else is
> self-healed while paused — re-arm promptly. (Same pipeline step 5 arms fleet-wide.)

### 4. Verify
**Owner scope landed:** `SELECT user_id, count(*) FROM conversations GROUP BY 1;` shows
rows under `<tenant>` with **zero `default`** remaining (the rekey swallows errors and
only re-counts 5 of 13 tables, so check this yourself). Then: a message round-trips;
conversation history is present; routines and channels load; **OMEMO encrypted chat
decrypts** (may take a few messages after first start — known behavior); a **stored secret
still decrypts** (proves `SECRETS_MASTER_KEY` carried). Re-authenticate the gateway UI (its
token was regenerated). Soak the canary before migrating the rest.

### 5. After all tenants are migrated
```bash
sudo ic/scripts/enable-health-fleet.sh --gotify-url <url> --gotify-token-file <path>
```

---

## Dry run & rehearsal

Validate before you touch a real agent. There are two levels:

### Level 1 — `--dry-run` (cheap, no changes)
Every script accepts `--dry-run`: it prints the exact command plan and makes **no**
changes (services are not stopped, no bundle is written, nothing is restored). It
validates the *plan and arguments*, **not** the outcome — it does not execute
`mt-admin`, so it cannot prove the migration succeeds.

```bash
sudo ic/scripts/export-tenant.sh <agent> --dry-run     # old host: preview (no stop, no bundle)
sudo ic/scripts/import-tenant.sh <bundle>.7z --dry-run # prompts, decrypts, validates, prints plan
```

Asymmetry to know: `export --dry-run` writes **no** bundle, but `import --dry-run`
**needs** a real bundle (it unpacks it and runs the real preconditions — postgres-only,
`SECRETS_MASTER_KEY` present, tenant-not-already-registered). So to dry-run the import
you need a real export first.

### Level 2 — throwaway-tenant rehearsal (the real confidence)
`--dry-run` can't prove correctness because it doesn't run `mt-admin`. Rehearse the
**whole** flow on a disposable tenant first:

```bash
# old host — create + seed a throwaway tenant (see the helper note below)
sudo ic/scripts/rehearse-testbot.sh
# real export -> transfer -> real import -> start
sudo ic/scripts/export-tenant.sh testbot
rsync -av -e ssh old:/var/lib/lunarwing-migrate/testbot-migrate-*.7z /var/lib/lunarwing-migrate/   # to new host
sudo ic/scripts/import-tenant.sh /var/lib/lunarwing-migrate/testbot-migrate-*.7z --dry-run         # plan check
sudo ic/scripts/import-tenant.sh /var/lib/lunarwing-migrate/testbot-migrate-*.7z --start --old-stopped
# verify the seeded marker survived (new host); then tear down
sudo ic/scripts/rehearse-testbot.sh --cleanup
```

Then do the lowest-stakes real agent as the **canary** (real export → import → verify →
**soak a day**) before migrating the other agents.

**What to verify** after a rehearsal/canary (what `--dry-run` can't tell you):
conversation history is present (DB restored), encrypted secrets decrypt
(`SECRETS_MASTER_KEY` carried correctly — e.g. a stored API key still works), and OMEMO
chat decrypts (may take a few messages after first start).

> ⚠️ **`ic/scripts/rehearse-testbot.sh` is generated but NOT yet tested.** It is a
> convenience helper to create + seed a throwaway tenant for the rehearsal above. Review
> it before running, and treat the import side as still unproven (it has only been
> syntax/shellcheck-validated plus a single source-side run on 2026-06-18; the full
> export → import → verify round-trip was never completed). It seeds a DB marker that proves DB
> round-trip only — it does not exercise OMEMO or encrypted-secret continuity, so still
> send a real message + OMEMO chat through the testbot for a full test.

---

## Cutover & rollback

- **Cutover window:** the agent is offline from export (step 1) until start (step 3).
  Because the old daemon is stopped before the snapshot, there is no same-JID double
  login and nothing is written after the snapshot. Repoint DNS / reverse-proxy /
  tunnel from the old gateway to the new host's gateway port at this point.
- **Rollback is trivial:** the old host is intact (export stopped the services but
  changed no data). To abort, restart the tenant on the old host.
- Don't decommission the old host until every migrated tenant has soaked and you've
  confirmed encrypted secrets decrypt (i.e. `SECRETS_MASTER_KEY` carried correctly)
  and OMEMO chat works.

---

## Security notes

- The encrypted bundle and both manifests are mode `0600`, root-owned, in a `0700`
  directory. Secrets (`SECRETS_MASTER_KEY`, XMPP password, bundle passphrase) never
  enter command argv or logs. The scripts feed `7z` through stdin and log key names
  only. For unattended use, prefer a root-owned mode-`0600` `KAWARIMI_PASS_FILE`;
  the browser console instead uses an anonymous per-job descriptor.
- Import inspects member paths, types, counts, and declared sizes before extraction;
  rejects links and special files; and defaults to 100 GiB unpacked limits for both
  the outer bundle and nested state. Override deliberately with
  `KAWARIMI_MAX_BUNDLE_BYTES`, `KAWARIMI_MAX_STATE_BYTES`, and
  `KAWARIMI_MAX_STATE_ENTRIES` for larger trusted migrations.
- Transfer over ssh; delete bundles from both hosts after verification.
