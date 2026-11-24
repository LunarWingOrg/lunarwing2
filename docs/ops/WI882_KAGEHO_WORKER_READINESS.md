# WI#882 — Kageho external workers unreachable after 2.2.1.0 upgrade

**Status:** root cause classified; readiness diagnostics landed (2.2.2.0).
**Run:** 2026-11-24, SweetieBot. **Branch:** `sweetiebot/kimi-wi882-20261124-1805`.

## Symptom (from the work item)

After Kageho's tenant was upgraded 2.1.9.0 → 2.2.1.0, both configured
external workers refused loopback connections:

- `opencode` `127.0.0.1:20037` → `ECONNREFUSED`
- `pebble` `127.0.0.1:10038` → `ECONNREFUSED`

The daemon started, connected to PostgreSQL, and applied migrations. A search
of the agent-visible scrubbed daemon log for worker-liveness entries on those
ports returned nothing.

## Classification: incomplete upgrade deployment, amplified by a diagnostic gap

**Not a 2.2.1.0 product regression in the worker code.** The `v2.2.1.0` tag
predates every part of the WI#796 worker-lifecycle work:

```
a19d47d93  wi-796: upgrade-tenant rebuilds external worker containers onto the promoted images   NOT in v2.2.1.0
6841e6eb8  wi-796: a dead worker lane must not report a green start or upgrade                    NOT in v2.2.1.0
e6321b0c2  wi-796: document the upgrade container rebuild and the loud lane refusals              NOT in v2.2.1.0
b1dc2adad  wi-796: fix worker startup fixtures and OpenRC refresh                                NOT in v2.2.1.0
```

`v2.2.1.0:ic/scripts/upgrade-tenant.sh` promotes worker candidates but never
calls `refresh-worker-containers`, and `v2.2.1.0`'s `mt-admin` has no such
verb (`git show v2.2.1.0:ic/scripts/mt-admin-lib/05-tenant-build.sh` has no
`upgrade_refresh_worker_containers`). So an upgrade on that tag retagged
`:latest` in the **admin** store only; a tenant that kept its pre-upgrade
bridge container went on serving the old image, and — because
`start_tenant_<lane>` short-circuits on `.State.Running` — every start and
upgrade stayed green while the lane's socket was dead.

That is exactly the failure mode WI#796 documented and fixed (landed after
2.2.1.0, present in this tree). The work item's own hypothesis — "if the
rootfs rebuild/recreate steps were omitted, this may be expected failed
deployment state" — is the correct one for a tenant upgraded on 2.2.1.0.

**The diagnostic gap is real and product-side.** On the 2.2.1.0 tag, and still
on this tree before this change:

- `mt-admin status <tenant>` printed `Pebble worker: running (...)` from
  `.State.Running` alone. Nothing compared that against a loopback dial.
- The only host-side listener audit was `worker-listeners`, a WI#357
  **exposure** scan (bind addresses / published mappings), not a readiness
  check.
- `mt-admin` had no verb that surfaced a lane's container logs.
- The daemon's boot probe (`WI#464`, present in 2.2.1.0) logs unreachable
  workers at `warn!`/`error!`, but only for lanes that are **in
  `config.toml`**. A tenant whose config carries no
  `[[sandbox.external_workers]]` block for a lane logs nothing at all — and
  `ExternalWorkerManager::new` said nothing about an empty worker set either.

## What landed

1. `mt-admin status` per-lane WSS readiness verdict
   (`listening` / `dead (no listener on 127.0.0.1)` / `unknown (no WSS port
   allocated)`), worker image revision, and a registry-vs-`config.toml`
   lane consistency report.
2. `mt-admin worker-readiness <tenant>` — strict gate, exit non-zero on a
   dead enabled lane. Poll window `LUNARWING_MT_WORKER_READY_WAIT_SECS`
   (default `0`).
3. `mt-admin worker-failure-log <tenant> <lane> [lines]` — container state
   plus a bounded, sanitized tail of the bridge and task container logs.
4. `_start_tenant_workers` runs the readiness pass and warns on a dead lane
   (advisory; the container start already succeeded).
5. Daemon: `ExternalWorkerManager::new` logs an explicit
   "No external workers configured" line when the set is empty, and
   `spawn_boot_liveness_probe` logs the resolved `host:port` endpoints it
   will dial at boot — so a log that reports a lane unreachable also shows
   the address the daemon resolved.

## Operator steps for Kageho (2.2.1.0 → 2.2.2.0)

`upgrade-tenant.sh` on this tree performs the WI#796 recreate automatically.
If the tenant was upgraded on 2.2.1.0 and is already running, the equivalent
repair without a full re-upgrade is:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh refresh-worker-containers kageho
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant kageho
sudo ic/scripts/lunarwing-mt-admin.sh worker-readiness kageho    # exit 0 == both lanes listening
```

Then a small task through each backend (`create_job(mode: "pebble")` and
`mode: "opencode"`) completes the acceptance check. If a lane is still dead,
`worker-failure-log kageho <lane>` names the container and its first failure.

## Not verified live

Kageho's host was not reachable from this run; the classification rests on
the tag contents plus the work item's evidence. The diagnostics were
smoke-tested against local tenants (`status camel`, `worker-readiness camel`,
`worker-failure-log camel pebble`) and the shell harnesses.
