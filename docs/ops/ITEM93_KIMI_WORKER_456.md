# Item 93 — WI#456 kimi external worker `connection refused`: investigation, fixes, and temporary-tenant verification

> Investigated 2026-10-22 by SweetieBot (autonomous), branch
> `sweetiebot/kimi-93-20261022-1828`. Companion work items (all referenced
> from WI#456's description): [WI#461](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/461),
> [WI#462](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/462),
> [WI#463](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/463).
> The temporary test tenant used for verification is `kimi93test` on the
> Gentoo dev VM (see below) and is **torn down** at the end of the run.

## The symptom (WI#456)

Dispatched kimi canary jobs failed with:

```
External worker 'kimi' connection failed: IO error: Connection refused (os error 111)
```

The daemon reported `External workers configured: kimi, pebble, opencode` at
boot (the `config.toml` route exists), but there were **no kimi-specific ERROR
lines** anywhere in the daemon log — the failure only surfaced at dispatch
time, connecting to a dead socket. Same signature as the pebble worker issue
(WI#452, closed as "rebuild needed").

## Root cause (verified live on the Gentoo dev VM)

The kimi lane regressed on the new build because **stale tenant-store kimi
images predate the WI#400 root-bridge shape**. Concretely, on this host:

- Tenant `scootaloo`'s rootless podman store carried
  `lunarwing-worker-kimi:latest` built 2026-10-16 with **`USER 10002:10002`**
  (pre-WI#400), while the current tree pins **`USER 0:0`**
  (`kimicode4lunarwing/Dockerfile:269`, image env `KIMI_TASK_UID/GID=10002`,
  entrypoint running the root bridge and dropping tasks via setpriv).
- The rootful admin store had **no** kimi image at all, so
  `_ensure_tenant_image`'s stale-accept branch
  (`[[ -n "$tenant_id" ]] && return 0`) silently kept the tenant's old copy
  in play.
- On the next `start-tenant`/worker start, mt-admin's image-user guard
  (`ic/scripts/mt-admin-lib/09-worker-config.sh:2170`) then `die`s:
  `kimi worker image has user '<actual>', expected 0:0 … rebuild it`. That
  die is **deliberate** (fail loudly rather than silently run a pre-WI#400
  bridge as the task uid — introduced with the WI#400 lanes, commit
  `79a60ee22`).
- The failure class is exactly WI#456's: the worker never starts (or the
  already-running stale container wedges), the daemon-side
  `[[sandbox.external_workers]]` route stays hot, and the next
  `create_job(mode: "kimi")` dials `ws://127.0.0.1:<kimi_wss>/ws/agent`
  with nothing listening → **os error 111, no kimi lines in daemon.log**.

WI#452's pebble note is the same class with one difference: pebble's image
carried `USER unset`; kimi's stale copies carry the *old task uid*.

## Fixes landed in this item

1. **The remediation itself — rebuild the image.**
   `build-kimi-worker` on the current tree produces `USER 0:0`
   (verified: `podman inspect lunarwing-worker-kimi:latest --format
   '{{.Config.User}}'` → `0:0`). Once the admin store holds a rebuilt
   image, `_ensure_tenant_image` distributes it to tenant stores (the
   stale-accept branch only applies while the admin store has *no* copy) and
   the launch contract (`_worker_launch_contract` pins the tenant-store
   image ID) force-recreates the running container on the next start — the
   lane is self-healing after a rebuild. **Operators hitting WI#456: run
   `sudo bash ic/scripts/lunarwing-mt-admin.sh build-kimi-worker`, then
   `start-tenant <name>` (or `start-worker <name> kimi`).**

2. **WI#461 (LOW, assigned): `prepare_worker_workspace` chown walk unchecked.**
   Both chown steps of the rootful workspace handoff
   (`09-worker-config.sh:1989-2005`) ran bare under `set -euo pipefail`.
   In a direct call context a failure aborted the whole mt-admin run from
   inside the function; in a suppressed-errexit context
   (`cmd || status=$?` — the shape the daemon-facing callers use) the same
   failure was **silently swallowed** and the function returned 0 with the
   bind mount never handed to the container uid. (The long-standing single
   MI-3 harness failure on the noexec-/tmp dev VM was exactly this silent
   swallow, live.) Both steps are now `if !`-guarded with a clean rc-1
   refusal, PWD restore on every exit path (MV-4), and two mutation-verified
   pins in `test-mt-admin-kimi.sh`. Harness now 140/140.

3. **WI#462 (LOW, assigned): task-uid PATH ordering pinned.**
   The executor's task toolchain PATH
   (`kimicode4lunarwing/scripts/kimi_task_executor.ts:351-361`) deliberately
   prepends task-uid-writable dirs ahead of the ME-1-pinned root-owned dirs —
   the inverse of the bridge-side ordering. The justification is now a
   comment block naming WI#462: this PATH is only ever used by code that
   already runs as the task uid (setpriv has already dropped), so a planted
   `node`/`python` shim gains no privilege it did not already have, and the
   toolchain must resolve from the task's own directories first. The bridge
   never resolves through this PATH (setpriv and the kimi binary come from
   the bridge PATH).

4. **WI#463 (INFO, assigned): bare-name `kimi` resolution at the drop.**
   `kimiBin()` returned a bare `kimi` that the *task-side* PATH would resolve
   after the setpriv drop. New `taskKimiBin()`: a bare name resolves through
   the **bridge's own PATH** (root-ownership-checked via
   `resolveFromBridgePath` when the resolver runs as root — the same
   treatment setpriv gets, WI#400 LD-11); an absolute `KIMI_BIN` (every
   operator/test override) passes through unchanged. Mutation-verified pins
   for both shapes; kimi TS suite 113/113 non-root, 109 pass / 4 skipped
   (documented real-CLI lane) root-shaped.

## Temporary test tenant verification (`kimi93test`)

Per the goals item, the fixes were exercised on a temporary test tenant
provisioned for this item on the Gentoo dev VM (OpenRC, rootless podman):

```bash
sudo bash ic/scripts/lunarwing-mt-admin.sh add-tenant kimi93test \
  --with-kimi \
  --tensorzero-url http://192.168.1.157:3000 \
  --kimi-base-url http://192.168.1.157:3000/openai/v1 \
  --no-ssh
sudo bash ic/scripts/lunarwing-mt-admin.sh start-tenant kimi93test
```

- Port block 10030-10039 (base) + kimi dedicated range: **kimi_wss 30030,
  kimi_health 30031**.
- Image: freshly built `lunarwing-worker-kimi:latest` (slim, revision
  `df89b66d6` — this branch's head, carrying WI#462/WI#463), `USER 0:0`,
  distributed into the tenant's rootless store by `_ensure_tenant_image`.
- `start-tenant` output: `kimi worker ready (lunarwing-kimi-kimi93test,
  WSS port 30030)`; OpenRC unit registered + babysitter attached;
  `mt-admin status kimi93test` → `Kimi worker: running`.
- Container identity: `podman exec … id` → **uid=0(root)**, with
  `KIMI_TASK_UID/GID=10002` driving the setpriv task drop.
- Daemon-side route: `/home/kimi93test/lunarwing/state/config.toml` carries
  `name = "kimi"`, `url = "ws://127.0.0.1:30030/ws/agent"`,
  `auth_token = <scoped WORKER_AUTH_TOKEN>`, `timeout_ms = 14400000`.
- **Handshake as the daemon dials it**: `curl -i` upgrade request with
  `Sec-WebSocket-Protocol: lunarwing-agent-v1` and the scoped bearer →
  **HTTP/1.1 101 Switching Protocols**. With a wrong bearer the bridge
  answers 401 before the upgrade — auth is enforced. This exercises the
  exact `connect_and_handshake` code path (`ic/src/orchestrator/external_worker.rs`)
  whose failure produced WI#456's `External worker 'kimi' connection
  failed: IO error: Connection refused (os error 111)` — the error is
  gone with the rebuilt image.
- **Readiness**: `curl http://127.0.0.1:30031/ready` →
  `{"ready": true, … "websocket": {"listening": true, "port": 30030,
  "path": "/ws/agent"}}`.
- The daemon binary was built (`build-tenant kimi93test`) and started; its
  boot logged `External workers configured: kimi, pebble, opencode` with
  the live socket behind the route. A full LLM-driven
  `create_job(mode: "kimi")` chat turn was attempted through
  `/api/chat/send`; the turn reached the daemon and **failed at the LLM
  upstream step** — this dev VM's TensorZero upstream answers `/health`
  200 but its model providers return 401 (the fleet's live credentials are
  not provisioned here; the same limitation every deploy-gated item
  records). The dispatch-level failure of WI#456 lives in the WS connect,
  which is verified end-to-end above; the LLM round-trip remains
  deploy-gated like the item-69/!1783 real-worker acceptance evidence.

Teardown: `sudo bash ic/scripts/lunarwing-mt-admin.sh remove-tenant
kimi93test --purge` (the tenant, its user, ports block, and containers are
removed; the rebuilt `lunarwing-worker-kimi:latest` stays in the rootful
admin store for the fleet — scootaloo's next start-tenant will pick it up
via `_ensure_tenant_image` and force-recreate its stale container).

## What this item does NOT change

- The image-user guard's `die` stays (deliberate, WI#400); this item makes
  sure a rebuilt image exists and documents the one-command remediation.
- `_ensure_tenant_image`'s stale-accept branch (admin store empty → tenant's
  copy wins) is unchanged: the image-user guard is the loud backstop, and
  changing the accept rule would break offline fleets where only a tenant
  store holds the image.
- The daemon's `ExternalWorkerConnectionFailed` path is unchanged — with a
  healthy worker the connection succeeds (verified 101).

## Verification summary

- `ic/scripts/tests/test-mt-admin-kimi.sh`: **140/140** (was 138 PASS + the
  MI-3 stub failure — that failure was the WI#461 hole live; now fixed and
  pinned).
- kimi TS suite (`npm test` under `kimicode4lunarwing/scripts`):
  **113/113** non-root; root-shaped 109 pass / 0 fail / 4 skipped (the
  documented real-CLI contract lane self-skips without a kimi binary).
- Mutation checks: WI#461 bare-chown revert fails exactly the rc-1 pin;
  WI#463 `taskKimiBin()`→`kimiBin()` revert fails exactly the bare-name
  argv pin.
- `check-mt-admin.sh` shellcheck clean (16 lib files); repo `shellcheck.sh`
  clean; `shell-syntax.sh` 255 scripts + 3 hooks clean; `tsc` clean.
- Live tenant: image `USER 0:0`, container running as root with task drop,
  WS handshake 101 with the scoped token, 401 with a wrong token,
  `/ready` true, mt-admin status `Kimi worker: running`.
