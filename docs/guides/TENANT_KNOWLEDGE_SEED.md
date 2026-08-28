# Seeding a New Tenant with the Study Notes Knowledge Base

This guide documents how to provision a new multi-tenant LunarWing instance so
that it starts with the **entire contents** of the project's shared study-notes
knowledge base — the [lw2studymcp](https://git.lunarwing.org/cmc/lw2studymcp.git)
study-notes MCP checkout — imported into the tenant's own, private knowledge
store on first boot.

Seeding is designed for **new tenants only**. It runs exactly once, never
overwrites anything, and never re-populates a store that was already seeded.
Existing tenants upgraded to a seed-capable binary are never seeded.

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
synced checkout into the tenant's private knowledge store. No further operator
action is required.

## What `--with-study-mcp` actually does

`add-tenant --with-study-mcp` runs `provision_tenant_study_mcp`
(`ic/scripts/mt-admin-lib/09-worker-config.sh:871`), which performs three steps:

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

Failure of the study-notes provisioning is **non-fatal**: the base tenant is
still provisioned, and you can recover later with:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-study-mcp <name> --sync
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

## Safety invariants

- **New tenants only.** The env flag is written exclusively on the
  `add-tenant --with-study-mcp` path. `configure-study-mcp` (existing tenants)
  wires the shared MCP read-only but never arms the seed; `upgrade-tenant`
  never touches env, so an existing tenant upgraded onto a seed-capable
  binary is never populated; env regeneration only carries an existing value
  forward (including an explicit operator `=0` opt-out).
- **Runs exactly once per tenant.** A state file
  (`knowledge_seed_state.json`, see below) is written once the pass has walked
  every discovered note; later startups skip the pass entirely, so the store
  is never repopulated — even if the shared checkout gains new notes later.
- **Never overwrites.** A note whose workspace path already exists is skipped
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
  shared read-only MCP only. Use it to recover a tenant whose auto-provisioning
  step failed: `configure-study-mcp <name> --sync`.
- **Opting out:** remove the `KNOWLEDGE_SEED_FROM_STUDY_MCP=1` line (or set it
  to `0`) from the tenant env before the first boot. An explicit `=0` written
  by an operator is preserved by `arm_tenant_knowledge_seed` and by later env
  regeneration.
- **Seeding an existing, long-lived tenant** is possible but unusual: re-run
  `add-tenant <name> --with-study-mcp` to arm the flag. The pass is safe — it
  runs at most once and never overwrites an existing note — but curated stores
  keep their operator-authored notes untouched; seeding only adds what is
  missing.
- **Forcing a full re-seed:** delete
  `/home/<tenant>/lunarwing/state/knowledge_seed_state.json` and restart the
  tenant. This is safe precisely because existing notes are never overwritten:
  only notes missing from the store are imported, and a previous pass's own
  stranded rows are repaired.

## Troubleshooting

| Symptom | Meaning | Action |
|---------|---------|--------|
| No flag in tenant env | Provisioned without `--with-study-mcp`, or it failed non-fatally | Re-run `add-tenant <name> --with-study-mcp`, or `configure-study-mcp <name> --sync` plus manual flag |
| `KNOWLEDGE_SEED_FROM_STUDY_MCP=1` present but no seed log | `LW2STUDYMCP_HOME` unset, or checkout missing (double-check warning in daemon log) | `configure-study-mcp <name> --sync` |
| `knowledge seed: ... not found; will retry on next startup` | Checkout directory missing at startup | Re-sync via `configure-study-mcp <name> --sync`, then restart the tenant |
| `N note(s) failed ... seed is complete and will not rerun` | Upstream junk notes (empty bodies, invalid slugs) counted in the state file | Nothing; expected for known upstream junk |
| Daemon restarted mid-import | No marker written; import resumes from scratch | Nothing; already-imported notes are skipped |
| Notes stale in tenant store relative to updated shared checkout | Seed is intentionally one-shot | Delete `knowledge_seed_state.json` and restart to force a safe re-seed of missing notes |

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
- Implementation: `ic/src/workspace/knowledge_seed.rs`,
  `ic/src/config/knowledge.rs`, `ic/scripts/mt-admin-lib/09-worker-config.sh`
  (`provision_tenant_study_mcp`, `arm_tenant_knowledge_seed`)
