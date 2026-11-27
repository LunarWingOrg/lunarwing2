# Seeding a New Tenant with the Study Notes Knowledge Base

This guide documents how to provision a new multi-tenant LunarWing instance so
that it imports eligible notes from the project's shared study-notes
knowledge base — the [lw2studymcp](https://git.lunarwing.org/cmc/lw2studymcp.git)
study-notes MCP checkout — imported into the tenant's own, private knowledge
store on first boot.

Seeding is designed for **new tenants only**. A completed bootstrap pass is
recorded in a state file. Tenants provisioned with `--with-study-mcp` also opt
in to a separate additive refresh cadence that imports newly published paths.
Both passes preserve notes they can read, subject to the repair cases below. An
upgrade alone does not arm either pass.

## What the tenant gets

A seeded tenant's private knowledge store contains the notes successfully imported
from the shared [lw2studymcp](https://git.lunarwing.org/cmc/lw2studymcp.git) checkout (roughly
1,250 curated codebase-study notes as of the August 2026 verification run),
imported into its own workspace as `study-notes/<agent>/<topic>.md` documents
with the canonical study-notes metadata preserved (status, confidence, studied
timestamp, source commit, tags, staleness thresholds, supersedure information).
They are written through the normal workspace pipeline, so chunking, full-text
search, and embeddings behave exactly as for notes the agent creates itself.

A seeded tenant also gets the **shared Study Notes MCP** wired in read-only, so
the live (shared) knowledge store stays queryable too.

## Quickstart

Create the tenant with `--with-study-mcp` and a valid LLM endpoint. Replace the
example URL with your TensorZero upstream. For a tenant without a local
TensorZero proxy, `--llm-base-url` can supply a direct endpoint instead.

```bash
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant mytenant --with-study-mcp \
  --tensorzero-url https://tensorzero.example.com
```

Optional: point at a different study-notes checkout (it is still synced to the
latest upstream before wiring):

```bash
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant mytenant --with-study-mcp \
  --tensorzero-url https://tensorzero.example.com \
  --study-mcp-home /srv/my-lw2studymcp
```

Then build and start as usual:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh build-tenant <name>
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant <name>
```

On the daemon's first startup, a background task imports eligible notes from
the synced checkout into the tenant's private knowledge store. The host source
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
from `ic/src/app.rs`) walks eligible note files under
`$LW2STUDYMCP_HOME/agent-notes-by-agent/<agent>/<topic>.md` and imports them
as private `study-notes/<agent>/<topic>.md` workspace documents. The normal
workspace write path indexes the imported content.

Imported notes keep the canonical lw2studymcp metadata: status, confidence,
studied timestamp (RFC 3339 or bare ISO date), source commit, tags, staleness
thresholds, status reason, and supersedure information.

The import is dynamic: whatever the checkout contains at first-boot time is
what gets seeded (hence step 1 above always syncing to latest upstream first).
As of the August 2026 verification run that was ~1,249 notes out of ~1,267
checkout files; the remainder were skipped for documented reasons
(invalid topic slugs, empty upstream bodies).

The daemon schedules staleness checks independently of heartbeat when
`KNOWLEDGE_SWEEP_ENABLED` is enabled. It scans each selected scope using
500-row keyset pages with a 100,000-row safety limit.

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
   (case-insensitive) enable sync — the same values the daemon accepts. Unset,
   `false`, and `0` are skipped without warnings, and a hand-edited `yes`/`on`
   cannot leave the host refreshing a source the daemon refuses to start with.
   The root scheduler runs the frozen release under `current`. That release
   carries its own OpenRC proxy contract, so it does not source the operator
   checkout or the host health library. Snapshot pruning starts when the count
   exceeds `LUNARWING_MT_KNOWLEDGE_SYNC_SNAPSHOT_KEEP + 1`. The default keep
   value is five, and pruning protects the newly published snapshot.
   `upgrade-tenant` republishes
   an existing snapshot from the upgraded-from checkout (stamped in
   `STAGED-FROM`); when the hourly schedule was removed or disabled, it
   refreshes the staged files and leaves the removal in place — an upgrade
   never creates a scheduler or re-enables a disabled timer (on OpenRC, a
   commented-out entry in the managed cron block counts as disabled). A staging
   that is refused or fails changes neither the schedule nor the staged copy.
   `doctor` reports
   the staged commit and flags a snapshot that has outlived its schedule.
3. Each enabled tenant daemon independently checks on its configured cadence
   (six hours by default, with deterministic startup jitter). When the revision
   changes it scans the checkout to import missing paths and repair eligible
   existing rows.
4. The daemon writes `knowledge_sync_state.json` under its state directory with
   the completed revision, attempt/success timestamps, backoff state, and last
   run counts. A cross-process lock collapses daemon and CLI runs.

Each refresh pass defaults to 500 new notes and 10 MiB of new source files.
Deferred records are picked up in follow-up passes. If the generation marker
changes during import, the pass leaves the completed revision unchanged and
retries with backoff. Writes that already succeeded stay in the workspace.

Additive refresh imports new paths and repairs eligible rows as described
below. Imported notes keep their local content and status across upstream
edits, supersedure, tombstones, and deletions. It
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
  wires the shared MCP read-only but never arms the seed. `upgrade-tenant`
  patches the tenant env but does not set the seed flag. Env regeneration
  only carries an existing value forward (including an explicit `=0` opt-out).
- **Bootstrap completion marker.** After visiting the discovered notes, the
  daemon publishes `knowledge_seed_state.json`. Later startups skip bootstrap
  only if that marker is valid. The separate refresh cadence still adds newly
  published paths.
- **Existing rows and repair.** Both passes skip an existing path with parseable
  knowledge-note metadata regardless of its body. Without that metadata, a body
  that exactly matches the rendered seed note gets a metadata-only repair. A
  whitespace-only body is filled with the seed note and given metadata, including
  an empty row created by an operator. Other bodies are left alone. A failed
  workspace lookup is treated as a missing row. If a subsequent write succeeds,
  it can replace an existing note (WI#932).
- **Wire namespace stays reserved.** Checkout directories under the
  coordination wire prefix (`coord-*`) are never imported, so seeded notes
  cannot collide with coordination wire notes. Dotfiles, non-Markdown files,
  and invalid slugs are also skipped.
- **Per-note failures.** Bootstrapping logs unreadable files and rejected
  bodies, records their count, and continues. Invalid slugs are skipped
  during enumeration.
- **Interrupted bootstrap.** After the pass, the daemon syncs a temporary
  marker and atomically renames it into place. If the daemon stops before that,
  or the marker is invalid or unreadable, the next enabled startup scans again
  and skips or repairs existing paths as above. The daemon warns when it finds
  an invalid or unreadable marker.
- **Missing checkout retries.** If the checkout directory is not present at
  startup, the pass is skipped with a warning and retried on the next startup
  (no marker is written).
- **Tenant-chosen sync homes are trust-gated (WI#445).** The host-side
  scheduler and `refresh-knowledge --sync-source` read `LW2STUDYMCP_HOME`
  from each tenant's own env, so a home is only synced when it is the
  operator default (`STUDY_NOTES_DEFAULT_HOME`) or a path without `.`/`..`
  components whose deepest existing ancestor chain is root-owned,
  symlink-free, and not group/other-writable. An untrusted home skips just
  that tenant with a warning (the hourly run never aborts on one bad value);
  the explicit `--sync-source` form dies instead of syncing under it, and
  the worker containers render no Study Notes mount for it at all (the
  worker starts without Study Notes). An existing checkout is only
  fast-forwarded when its configured `origin` is exactly the operator's
  `STUDY_NOTES_REPO`.

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
     "schema_version": 1,
     "seeded_at": "2026-08-22T17:00:00Z",
     "source_home": "/var/cache/lunarwing/lw2studymcp",
     "notes_seeded": 1249,
     "notes_skipped_existing": 4,
     "notes_failed": 2
   }
   ```

   Complete older markers without `schema_version` still prevent a rerun.
   `notes_failed` counts import failures, such as unreadable source files,
   empty bodies, or workspace write errors. While the marker is valid,
   bootstrap does not retry those notes on each boot.

4. **Store populated:** the tenant's PostgreSQL contains
   `study-notes/<agent>/<topic>.md` documents (verification run: 1,249
   documents, all with `provenance=study-note` and full canonical metadata).

5. **Idempotent restart:** after `restart-tenant`, no new seed log line
   appears and the document count is unchanged.

## Existing tenants, upgrades, and opt-outs

- **An upgrade does not opt a tenant into seeding.** `upgrade-tenant` can patch
  the tenant env, but it does not add `KNOWLEDGE_SEED_FROM_STUDY_MCP`. Upgrading
  an unarmed tenant therefore does not bootstrap `study-notes/` documents.
  The historical verification records below cover systemd and Gentoo/OpenRC.
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
  importer fills missing paths and applies the repair rules to rows it reads.
  Keep the existing seed marker in place.

## Troubleshooting

| Symptom | Meaning | Action |
|---------|---------|--------|
| No flag in tenant env | Provisioned without `--with-study-mcp`, or it failed non-fatally | Use `configure-study-mcp <name> --sync --enable-knowledge-sync`, restart, then `refresh-knowledge <name>` |
| `KNOWLEDGE_SEED_FROM_STUDY_MCP=1` present but no seed log | `LW2STUDYMCP_HOME` unset, or checkout missing (double-check warning in daemon log) | `configure-study-mcp <name> --sync` |
| `knowledge seed: ... not found; will retry on next startup` | Checkout directory missing at startup | Re-sync via `configure-study-mcp <name> --sync`, then restart the tenant |
| `N note(s) failed ... seed is complete and will not rerun` | Import failures counted in the state file | Correct or remove malformed source notes; additive refresh reports failures and retries |
| Daemon restarted before writing the marker | The next bootstrap scans the checkout again | Already-imported notes are skipped or repaired as described above |
| `invalid or unreadable completion marker` | The existing marker cannot prove completion | Bootstrap retries automatically. If the warning repeats, check the checkout and state directory permissions |
| `source_unavailable` refresh report | No validated generation is published, often after a failed/dirty checkout update | Repair the checkout, then run `refresh-knowledge <name> --sync-source` |
| Refresh succeeds but an upstream edit is absent | Additive mode preserves existing notes it can read outside the documented repair cases | Resolve manually if needed |

## Related documents

- [`MT-ADMIN-QUICKSTART.md`](MT-ADMIN-QUICKSTART.md) — tenant provisioning and
  lifecycle quickstart
- [`../ops/MULTITENANCY-PRODUCTION.md`](../ops/MULTITENANCY-PRODUCTION.md) —
  production multi-tenant deployment runbook
- [`../ops/TENANT-CONFIGURATION.md`](../ops/TENANT-CONFIGURATION.md) — tenant
  env file layout and configuration reference
- [`../ops/history/v2.0.9.0_GOAL3_KNOWLEDGE_SEED.md`](../ops/history/v2.0.9.0_GOAL3_KNOWLEDGE_SEED.md) —
  mechanism design record + Gentoo/OpenRC live verification
- [`../ops/history/v2.0.9.0_GOAL4_KNOWLEDGE_SEED_SYSTEMD.md`](../ops/history/v2.0.9.0_GOAL4_KNOWLEDGE_SEED_SYSTEMD.md) —
  systemd live verification (including the upgraded-tenant negative case)
- [`../ops/history/v2.1.0.0_KNOWLEDGE_STORE_RESYNC_OPTIONS.md`](../ops/history/v2.1.0.0_KNOWLEDGE_STORE_RESYNC_OPTIONS.md) —
  refresh architecture, safety invariants, and hosted-delta follow-up
- Implementation: `ic/src/workspace/knowledge_seed.rs`,
  `ic/src/workspace/knowledge_sync.rs`, `ic/src/config/knowledge.rs`, and
  `ic/scripts/mt-admin-lib/{06-env-generation,09-worker-config,14-knowledge-sync}.sh`
