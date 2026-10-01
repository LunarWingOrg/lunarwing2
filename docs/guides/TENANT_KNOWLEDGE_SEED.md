# Seeding a New Tenant with the Study Notes Knowledge Base

This guide documents how to provision a new multi-tenant LunarWing instance so
that it starts with the **entire contents** of the project's shared study-notes
knowledge base — the [lw2studymcp](https://git.lunarwing.org/cmc/lw2studymcp.git)
study-notes MCP checkout — imported into the tenant's own, private knowledge
store on first boot.

Seeding is designed for **new tenants only**. It runs exactly once and never
overwrites anything. Tenants provisioned with `--with-study-mcp` also opt in to
a separate additive refresh cadence that imports newly published paths without
changing existing notes. Existing tenants upgraded to a seed-capable binary are
neither seeded nor opted in automatically.

**Revalidated against source 2026-09-17 (v2.1.2.0); re-checked 2026-09-27 (v2.1.3.0) and 2026-10-01 (v2.1.4.0).** The seeding and refresh
flows are unchanged. Note the stale-marking sweep no longer has the v2.1.1
1,000-note scan cap: `knowledge_list_notes` pages the whole store with a keyset
cursor (500-row pages, 100k safety ceiling), and the sweep also runs on an
independent daemon cadence (`KNOWLEDGE_SWEEP_ENABLED`) so multi-tenant hosts —
where mt-admin renders `HEARTBEAT_ENABLED=false` — still age notes. A seeded
tenant's full corpus (~1,250 notes) is therefore fully sweepable.

## What the tenant gets

A seeded tenant's private knowledge store contains every note from the shared
[lw2studymcp](https://git.lunarwing.org/cmc/lw2studymcp.git) checkout (roughly
1,250 curated codebase-study notes as of the August 2026 verification run),
imported into its own workspace as `study-notes/<agent>/<topic>.md` documents
with the canonical study-notes metadata preserved (status, confidence, studied
timestamp, source commit, tags, staleness thresholds, supersedure information).
They are written through the normal workspace pipeline, so chunking, full-text
search, and embeddings behave exactly as for notes the agent creates itself.

A seeded tenant also gets the **shared Study Notes MCP** wired in read-only, so
the live (shared) knowledge store stays queryable too.

## Quickstart

Create the tenant with the `--with-study-mcp` flag:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant <name> --with-study-mcp
```

Optional: point at a different study-notes checkout (it is still synced to the
latest upstream before wiring):

```bash
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant <name> --with-study-mcp \
  --study-mcp-home /srv/my-lw2studymcp
```

Then build and start as usual:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh build-tenant <name>
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant <name>
```

On the daemon's first startup, a background task imports every note from the
synced checkout into the tenant's private knowledge store. The host source
scheduler and tenant cadence then keep adding newly published paths.

## What `--with-study-mcp` actually does

`add-tenant --with-study-mcp` runs `provision_tenant_study_mcp`
(`ic/scripts/mt-admin-lib/09-worker-config.sh`), which performs four steps:

1. **Syncs the shared lw2studymcp checkout to the latest upstream** — cloned
   when missing, fast-forwarded when present and clean. Defaults:
   checkout `/var/cache/lunarwing/lw2studymcp` from
   `https://git.lunarwing.org/cmc/lw2studymcp.git`. Override the checkout with
   `--study-mcp-home <path>` or `LUNARWING_MT_STUDY_NOTES_HOME`, and the repo
   URL with `LUNARWING_MT_STUDY_NOTES_REPO`.
2. **Wires the shared Study Notes MCP into the tenant read-only**, writing
   `LW2STUDYMCP_HOME` into the tenant env (core writes stay opt-in via
   `configure-study-mcp --allow-write`).
3. **Arms the one-shot seed** by appending
   `KNOWLEDGE_SEED_FROM_STUDY_MCP=1` to the tenant's
   `/home/<tenant>/lunarwing/env/lunarwing.env`. This is idempotent (never
   appended twice) and preserves an operator's explicit `=0` opt-out.
4. **Enables additive refresh** with `KNOWLEDGE_SYNC_ENABLED=true` and bounded
   defaults, then installs one host-global hourly scheduler. systemd uses a
   persistent timer; OpenRC uses the available fcron/crontab implementation.

Failure of the study-notes provisioning is **non-fatal**: the base tenant is
still provisioned. Recover and explicitly opt the existing tenant into refresh:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-study-mcp <name> --sync \
  --enable-knowledge-sync
sudo ic/scripts/lunarwing-mt-admin.sh restart-tenant <name>
```

## What happens on first boot

When the daemon starts with `KNOWLEDGE_SEED_FROM_STUDY_MCP=1` and
`LW2STUDYMCP_HOME` set, a background startup task
(`run_knowledge_seed_once` in `ic/src/workspace/knowledge_seed.rs`, spawned
from `ic/src/app.rs`) walks every note under
`$LW2STUDYMCP_HOME/agent-notes-by-agent/<agent>/<topic>.md` and imports each
into the tenant's private knowledge store as `study-notes/<agent>/<topic>.md`
workspace documents. Writes go through the normal workspace pipeline, so
chunking, full-text search, and embeddings behave exactly as for notes the
agent creates itself.

Imported notes keep the canonical lw2studymcp metadata: status, confidence,
studied timestamp (RFC 3339 or bare ISO date), source commit, tags, staleness
thresholds, status reason, and supersedure information.

The import is dynamic: whatever the checkout contains at first-boot time is
what gets seeded (hence step 1 above always syncing to latest upstream first).
As of the August 2026 verification run that was ~1,249 notes out of ~1,267
checkout files; the remainder were skipped for documented reasons
(invalid topic slugs, empty upstream bodies).

## How automatic refresh works

The hosted Study Notes service does not yet expose an immutable snapshot plus
ordered delta feed. LunarWing therefore uses the documented 2.1 fallback:

1. Once per hour, the host scheduler finds each unique checkout referenced by a
   tenant with `KNOWLEDGE_SYNC_ENABLED=true`.
2. mt-admin removes the old publication marker, fast-forwards the clean checkout
   under its existing `flock`, validates the expected database, notes tree,
   scripts, and skill, then atomically writes
   `.git/lunarwing-study-notes-generation.json` with the Git revision. A
   transient fast-forward failure (e.g. network blip) restores the last-good
   publication when HEAD still matches its recorded revision; a dirty checkout
   or one that moved forward keeps the barrier down. Only `true`/`1`
   (case-insensitive) enable sync — the same values the daemon accepts — so a
   hand-edited `yes`/`on` cannot leave the host refreshing a source the daemon
   refuses to start with. Root-owned scheduler snapshots are pruned to the
   newest five releases (`LUNARWING_MT_KNOWLEDGE_SYNC_SNAPSHOT_KEEP`).
3. Each tenant daemon independently checks on its configured cadence (six hours
   by default, with deterministic startup jitter). When the revision changes it
   scans the checkout and imports only missing paths.
4. The daemon writes `knowledge_sync_state.json` under its state directory with
   the completed revision, attempt/success timestamps, backoff state, and last
   run counts. A cross-process lock collapses daemon and CLI runs.

The default pass caps are 500 new notes and 10 MiB. Deferred records are picked
up in bounded follow-up passes. Failures keep the last good native corpus and
retry with exponential backoff. The source marker is checked before and after a
scan, so a pass crossing a host update is rejected and safely replayed.

This mode is deliberately labeled `additive`: it imports new paths, but does not
apply upstream edits, status changes, supersedure, tombstones, or deletions. It
reports a Git revision rather than claiming exact parity with the hosted MCP.
Those complete semantics require the revisioned hosted delta contract described
in `docs/ops/history/v2.1.0.0_KNOWLEDGE_STORE_RESYNC_OPTIONS.md`.

Run a bounded pass immediately without restarting the daemon:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh refresh-knowledge <name> --sync-source
```

Omit `--sync-source` to consume the currently published generation. The command
prints the same JSON report used by automation. To publish all enabled sources
without importing directly, run `sync-knowledge-sources`.

## Safety invariants

- **New tenants only.** The env flag is written exclusively on the
  `add-tenant --with-study-mcp` path. `configure-study-mcp` (existing tenants)
  wires the shared MCP read-only but never arms the seed; `upgrade-tenant`
  never touches env, so an existing tenant upgraded onto a seed-capable
  binary is never populated; env regeneration only carries an existing value
  forward (including an explicit operator `=0` opt-out).
- **The bootstrap runs exactly once per tenant.** A state file
  (`knowledge_seed_state.json`, see below) is written once the pass has walked
  every discovered note; later startups skip that bootstrap entirely. The
  separate refresh cadence can still add paths published afterward.
- **Never overwrites.** Both bootstrap and refresh skip a note whose workspace path already exists
  byte-for-byte, so operator-authored notes survive even a forced re-seed. The
  only writes to existing paths repair a previous seed pass's own stranded
  rows (a byte-for-byte seeded body missing provenance metadata gets the
  metadata restored; an empty placeholder row from a mid-pass crash gets
  filled). Operator content never matches those shapes byte-for-byte.
- **Wire namespace stays reserved.** Checkout directories under the
  coordination wire prefix (`coord-*`) are never imported, so seeded notes
  cannot collide with coordination wire notes. Dotfiles, non-Markdown files,
  and invalid slugs are also skipped.
- **Failures never block.** Individual note failures (unreadable files, empty
  bodies, invalid slugs) are warned in the log and recorded in the state file,
  but never crash the agent and never turn the one-shot seed into a per-boot
  rescan.
- **Crash-safe.** A crash mid-pass leaves no state marker, so the next startup
  simply resumes (already-imported notes are skipped as existing).
- **Missing checkout retries.** If the checkout directory is not present at
  startup, the pass is skipped with a warning and retried on the next startup
  (no marker is written).

## Verification

After provisioning and the first `start-tenant`:

1. **Flag armed exactly once** in the tenant env:

   ```bash
   grep -c '^KNOWLEDGE_SEED_FROM_STUDY_MCP=1' \
     /home/<tenant>/lunarwing/env/lunarwing.env   # expect: 1
   ```

2. **Import ran:** the daemon log contains the completion line

   ```text
   Seeded tenant knowledge store from lw2studymcp checkout seeded=<n> skipped_existing=<m>
   ```

   (systemd: `journalctl --user -u lunarwing-<tenant>`; OpenRC: tenant log
   directory `/home/<tenant>/lunarwing/logs/`).

3. **State marker written:**

   ```bash
   cat /home/<tenant>/lunarwing/state/knowledge_seed_state.json
   ```

   ```json
   {
     "seeded_at": "...",
     "source_home": "/var/cache/lunarwing/lw2studymcp",
     "notes_seeded": 1249,
     "notes_skipped_existing": 4,
     "notes_failed": 2
   }
   ```

   `notes_failed` entries are upstream junk notes (empty bodies, invalid
   slugs); they are counted and never retried per boot.

4. **Store populated:** the tenant's PostgreSQL contains
   `study-notes/<agent>/<topic>.md` documents (verification run: 1,249
   documents, all with `provenance=study-note` and full canonical metadata).

5. **Idempotent restart:** after `restart-tenant`, no new seed log line
   appears and the document count is unchanged.

## Existing tenants, upgrades, and opt-outs

- **Upgraded tenants are never seeded.** `upgrade-tenant` never touches the
  tenant env, so a tenant created on a pre-seed commit and upgraded onto a
  seed-capable branch keeps an untouched knowledge store with zero
  `study-notes/` documents. Verified live on both systemd and Gentoo/OpenRC
  hosts (see the verification records below).
- **`configure-study-mcp` never arms the seed.** It wires (or re-syncs) the
  shared read-only MCP only unless `--enable-knowledge-sync` is supplied. That
  flag opts an existing tenant into additive refresh without pretending it is a
  new-tenant bootstrap; restart once to start the daemon cadence.
- **Opting out:** set `KNOWLEDGE_SEED_FROM_STUDY_MCP=0` before the first boot to
  suppress bootstrap. Disable ongoing refresh with
  `configure-study-mcp <name> --home <current-home> --disable-knowledge-sync`
  and restart. Env regeneration preserves both choices.
- **Populating an existing, long-lived tenant:** configure and publish the
  checkout, enable sync, then run `refresh-knowledge <name>`. The same additive
  importer fills missing paths while curated/operator-authored paths remain
  untouched; deleting the seed marker or impersonating a new add is unnecessary.

## Troubleshooting

| Symptom | Meaning | Action |
|---------|---------|--------|
| No flag in tenant env | Provisioned without `--with-study-mcp`, or it failed non-fatally | Use `configure-study-mcp <name> --sync --enable-knowledge-sync`, restart, then `refresh-knowledge <name>` |
| `KNOWLEDGE_SEED_FROM_STUDY_MCP=1` present but no seed log | `LW2STUDYMCP_HOME` unset, or checkout missing (double-check warning in daemon log) | `configure-study-mcp <name> --sync` |
| `knowledge seed: ... not found; will retry on next startup` | Checkout directory missing at startup | Re-sync via `configure-study-mcp <name> --sync`, then restart the tenant |
| `N note(s) failed ... seed is complete and will not rerun` | Upstream junk notes (empty bodies, invalid slugs) counted in the state file | Correct or remove malformed source notes; additive refresh reports failures and retries |
| Daemon restarted mid-import | No seed marker written; bootstrap resumes from scratch | Nothing; already-imported notes are skipped |
| `source_unavailable` refresh report | No validated generation is published, often after a failed/dirty checkout update | Repair the checkout, then run `refresh-knowledge <name> --sync-source` |
| Refresh succeeds but an upstream edit is absent | Additive mode never replaces an existing path | Expected; wait for the hosted revisioned-delta implementation or resolve manually |

## Related documents

- [`MT-ADMIN-QUICKSTART.md`](MT-ADMIN-QUICKSTART.md) — tenant provisioning and
  lifecycle quickstart
- [`../ops/MULTITENANCY-PRODUCTION.md`](../ops/MULTITENANCY-PRODUCTION.md) —
  production multi-tenant deployment runbook
- [`../ops/TENANT-CONFIGURATION.md`](../ops/TENANT-CONFIGURATION.md) — tenant
  env file layout and configuration reference
- [`../ops/v2.0.9.0_GOAL3_KNOWLEDGE_SEED.md`](../ops/v2.0.9.0_GOAL3_KNOWLEDGE_SEED.md) —
  mechanism design record + Gentoo/OpenRC live verification
- [`../ops/v2.0.9.0_GOAL4_KNOWLEDGE_SEED_SYSTEMD.md`](../ops/v2.0.9.0_GOAL4_KNOWLEDGE_SEED_SYSTEMD.md) —
  systemd live verification (including the upgraded-tenant negative case)
- [`../ops/history/v2.1.0.0_KNOWLEDGE_STORE_RESYNC_OPTIONS.md`](../ops/history/v2.1.0.0_KNOWLEDGE_STORE_RESYNC_OPTIONS.md) —
  refresh architecture, safety invariants, and hosted-delta follow-up
- Implementation: `ic/src/workspace/knowledge_seed.rs`,
  `ic/src/workspace/knowledge_sync.rs`, `ic/src/config/knowledge.rs`, and
  `ic/scripts/mt-admin-lib/{06-env-generation,09-worker-config,14-knowledge-sync}.sh`
