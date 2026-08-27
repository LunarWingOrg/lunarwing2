# MT-Admin Post-Refactor Upgrade Validation — henry (non-animal tenant)

**Date:** 2026-08-19 15:56–16:05 UTC
**Operator:** Rarity (supervised, interactive)
**Purpose:** First validation that the refactored mt-admin (item #9: root +
14 `mt-admin-lib/` modules) can upgrade an existing tenant running an OLDER
version with accumulated legacy state — a path not covered by any goal item.

## Patient profile (deliberately "dirty")

| Property | Value |
|---|---|
| Tenant | henry (manual/keep tenant — NOT teardown-eligible) |
| Pre-upgrade build | v2.0.7.0-22-g87169d89 (binary reported `lunarwing 2.0.7`) |
| Pre-upgrade branch | `rarity/kimi-1-20260814-0602` (kimi work branch, not a tag) |
| Provisioned | Aug 14 (post-crash reprovision era) |
| Running services | core, pg, pg-sup, vision, vision-sup, weechat-adapter, xmpp-bridge (7) |
| Worker containers | pg + vision only (no pebble/opencode/kimi) |
| Legacy state | runtime env/, logs/, run/, state/, pre-existing config.toml, old rendered units |
| Pre-existing tree drift | `ic/bridges/xmpp-bridge/Cargo.lock` modified (reset before upgrade to pass clean-tree gate) |
| Target | `aragents/kimil00p/v2.0.8.0` @ afe0beec |

## Execution

```
sudo bash ic/scripts/lunarwing-mt-admin.sh upgrade-tenant henry \
  --target aragents/kimil00p/v2.0.8.0
```
(run in tmux per help guidance; full log preserved at /tmp/henry-upgrade.log)

Observed sequence, all through the post-split module path:
1. Postgres backup → `/var/lib/lunarwing-backups/henry/henry-20260819155653.dump` (100K)
2. Fetch from origin (tenant origin points at the operator checkout)
3. Clean-tree gate — caught the dirty xmpp-bridge lockfile BEFORE we reset it
   (gate is functional post-refactor)
4. Stop tenant
5. Checkout afe0beec as tenant user
6. Release rebuild with WASM (~9 min, incremental, 2.9G target cache)
7. Env patch: appended COORD_KNOWLEDGE_* knobs (new coordination feature) to
   `/home/henry/lunarwing/env/lunarwing.env` — additive, preserved existing env
8. External-worker config: correctly skipped (workers not selected for henry)
9. Re-render + start all services; boot persistence re-confirmed for running set

## Results

| Check | Result |
|---|---|
| Upgrade completed | v2.0.7.0-22-g87169d89 → v2.0.7.0-327-gafe0beec |
| New binary version | `lunarwing 2.0.8` ✓ |
| Running process = new binary | pid started 16:05:25, binary mtime 16:04, `/proc/exe` → target/release/lunarwing ✓ |
| All 7 services | started ✓ (core, pg, pg-sup, vision, vision-sup, weechat-adapter, xmpp-bridge) |
| Weechat caps | re-patched (relay 10035 / adapter 10039) ✓ |
| SSH readiness | agent running, 1 key loaded, wasm ssh installed ✓ |
| Backup | taken pre-upgrade, non-empty ✓ |
| PG data | preserved (containers reused, "already running" warnings benign) ✓ |

## Notes & gotchas

- "WARNING: already been started" lines at the start phase are benign: pg +
  xmpp-bridge containers/services came back on their own during rebuild; the
  script's start phase just noted it. No double-start, no port conflict.
- Git-describe on the tenant now reads `v2.0.7.0-327-gafe0beec` because the
  2.0.8.0 tag has not been cut yet; the binary itself reports 2.0.8.
- The one manual intervention: resetting the dirty `xmpp-bridge/Cargo.lock`
  (build-time drift from Aug 14). Without it the clean-tree gate (correctly)
  refuses. Operators upgrading older tenants should expect this class of drift.
- Build ran as tenant user inside cgroup bounds (post-refactor 13-cgroups-openrc
  module); no OOM/throttle events observed; 22G RAM host, nothing starved.

## Conclusion

The refactored mt-admin upgrades a legacy-state tenant from 2.0.7-era code to
current 2.0.8 code cleanly: ordering held, gates enforced, env additive-patched,
all services restored, running binary verified new. Post-refactor upgrade path:
**VALIDATED** on OpenRC/Gentoo for the core+WASM+pg+vision topology.

Not covered here (future evidence wanted): tenants WITH pebble/opencode/kimi
worker containers upgrading across the same span (AJ's host has the full-stack
equivalent), and pre-2.0.7 tenants (pre-ports-v16 registry interplay).
