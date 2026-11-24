# WI#882 — Kageho external workers unreachable after 2.2.1.0 upgrade

**Status:** root cause is a hypothesis (incomplete upgrade deployment), not
yet confirmed on the host; readiness diagnostics landed (2.2.2.0).
**Run:** 2026-11-24, SweetieBot. **Branch:** `sweetiebot/kimi-wi882-20261124-1805`.

## Symptom (from the work item)

After Kageho's tenant was upgraded 2.1.9.0 → 2.2.1.0, the daemon's liveness
probes reported both configured external workers unreachable ("0 reachable /
2 unreachable"), and that stayed so past the startup grace. The item also
reports a direct re-test from the tenant sandbox:

- `opencode` `127.0.0.1:20037` → `ECONNREFUSED`
- `pebble` `127.0.0.1:10038` → `ECONNREFUSED`

The daemon started, connected to PostgreSQL, and applied migrations. A search
of the agent-visible scrubbed daemon log for worker-liveness entries on those
ports returned nothing.

### What this evidence shows, and what it does not

- **The daemon's probe result is the evidence that counts.** The probe dials
  the configured endpoint from the daemon, on the host's loopback, and
  completes the WebSocket handshake. It says that both lanes were
  unreachable; it does not say why.
- **The direct re-test does not show the host ports.** A sandbox container
  has its own network namespace (`--network none` in the `none` and
  `proxy-enforced` modes), so `127.0.0.1` there is the sandbox's own
  loopback. A dial to it is refused whether or not the worker is healthy.
- **Nobody inspected the host.** The container states, the published ports
  and the `status` output of Kageho's tenant are not known.
- **What a refused connection means on a rootless host.** A lane's port is
  published through podman's forwarder, which accepts on `127.0.0.1` for as
  long as the container runs. A refused connection therefore means that no
  forwarder holds that port: the lane's container is not running, or it
  publishes another port than the one dialed. A running container whose
  bridge never bound its port looks different: the connection is accepted
  and then dropped at once.

## Hypothesis: incomplete upgrade deployment, made worse by a diagnostic gap

**The tag facts (verified).** The `v2.2.1.0` tag predates every part of the
WI#796 worker-lifecycle work:

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
`:latest` in the **admin** store only and did not recreate a tenant's worker
containers onto the promoted images. `start_tenant_<lane>` short-circuits on
`.State.Running`, so a start or an upgrade could finish green whatever the
lane's port did.

**What follows, and what does not.** The work item's own hypothesis — "if
the rootfs rebuild/recreate steps were omitted, this may be expected failed
deployment state" — fits these facts for a tenant upgraded on 2.2.1.0. It is
not confirmed: which state the upgrade left on Kageho's host is not known,
and the tag facts alone do not explain an unreachable lane (a container that
still runs its pre-upgrade image normally still answers on its port). Until
the host is inspected (see "What to collect on the host"), this is not a
finding, and a product regression is not excluded.

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

1. `mt-admin status` prints a verdict beside each running lane's published
   WSS port, the worker image revision, and a registry-vs-`config.toml` lane
   consistency report. The verdicts:
   - `listening` — the port accepted a loopback connection and kept it open
     for a second (or answered). Something behind the published port holds
     connections. It does **not** show that the bridge completes the
     handshake or that a job can connect.
   - `dead (no listener on 127.0.0.1)` — the connection was refused.
   - `dead (accepted on 127.0.0.1 and dropped at once, ...)` — podman's
     forwarder accepted and nothing inside the container listens. A plain
     connect cannot tell this from a healthy lane, which is why the probe
     waits.
   - `unknown (no WSS port allocated)` — the registry has no port for the
     lane.
2. `mt-admin worker-readiness <tenant>` — strict gate, exit non-zero when an
   enabled lane's port refuses or drops the connection. Poll window
   `LUNARWING_MT_WORKER_READY_WAIT_SECS` (default `0`). Exit 0 means that
   every enabled lane's port kept a connection open; it does not mean that a
   job can connect. The daemon's probe line in its log is the stronger check.
3. `mt-admin worker-failure-log <tenant> <lane> [lines]` — container state
   plus a bounded tail of the bridge and task container logs. Each line is
   redacted, best effort: the values of token, key, secret and password
   fields, bearer and cookie headers, URL user parts and strings with a
   known secret prefix are replaced by `[redacted]`. It is a pattern filter,
   not a guarantee: read the output before pasting it anywhere public.
4. `_start_tenant_workers` runs the readiness pass and warns on a dead lane
   (advisory; the container start already succeeded).
5. Daemon: `ExternalWorkerManager::new` logs an explicit
   "No external workers configured" line when the set is empty, and
   `spawn_boot_liveness_probe` logs the resolved `host:port` endpoints it
   will dial at boot — so a log that reports a lane unreachable also shows
   the address the daemon resolved.

## What to collect on the host

To confirm or reject the hypothesis, on Kageho's host, before any repair:

1. **Container state.** `mt-admin status kageho`: the two lane lines with
   their verdicts, and the worker image revisions.
2. **The ports, three ways.** For each lane: the port its container
   publishes (`mt-admin worker-listeners kageho` lists the listening
   sockets inside each running worker container and checks its published
   mappings), the registry's port (the lane line of `status`), and the port in the `url` of
   the lane's `[[sandbox.external_workers]]` block in the tenant's
   `config.toml`. All three must agree.
   `ic/scripts/lunarwing-port-drift-preflight.sh kageho` compares the env
   file and the Quadlet ports with the registry.
3. **The gate.** `mt-admin worker-readiness kageho` and its exit status.
4. **First failure.** `mt-admin worker-failure-log kageho <lane>` for each
   lane. The output is redacted best effort; read it before pasting it.
5. **The daemon's view.** The probe lines in the tenant's daemon log:
   "External worker liveness probe scheduled for ... at host:port" and the
   unreachable lines with their reasons.

## Operator steps for Kageho (2.2.1.0 → 2.2.2.0)

`upgrade-tenant.sh` on this tree performs the WI#796 recreate automatically.
If the tenant was upgraded on 2.2.1.0 and is already running, the equivalent
repair without a full re-upgrade is:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh refresh-worker-containers kageho
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant kageho
sudo ic/scripts/lunarwing-mt-admin.sh worker-readiness kageho    # exit 0 == every enabled lane's port kept a connection open
```

Then a small task through each backend (`create_job(mode: "pebble")` and
`mode: "opencode"`) completes the acceptance check; that, and the daemon's
probe lines, show that a job can connect, which the gate alone does not. If a
lane is still dead, `worker-failure-log kageho <lane>` names the container
and its first failure.

## Not verified live

Kageho's host was not reachable from this run; the hypothesis rests on the
tag contents plus the work item's evidence. The diagnostics were
smoke-tested against local tenants (`status camel`, `worker-readiness camel`,
`worker-failure-log camel pebble`) and the shell harnesses. The readiness
verdict was changed afterwards (review note 22533 on !2743: a published
rootless port accepts while the container runs, so the probe now also
requires that the connection is kept open). That change was checked by the
shell harness with real listeners: one keeps the connection open, one closes
it at once, one resets it. Any run against a real rootless container is
recorded on the merge request, not here.
