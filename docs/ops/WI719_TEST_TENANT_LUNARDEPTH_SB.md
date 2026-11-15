# WI#719 — LunarDepth external-worker test, SweetieBot edition (`lunardepthtester`)

> Test started 2026-11-15 by SweetieBot on branch
> `sweetiebot/kimi-wi719-20261115-2135`, at tree `85543ba899`.
> Work item #719 is a repeat of the WI#685 exercise: provision a fresh
> `lunardepthtester` tenant with the LunarDepth worker and `--with-toolchains`,
> run medium/high-complexity jobs, and record results plus hiccups. The goals
> file is deliberately unchanged; this is a work-item run.

## Scope

The exercise uses the real mt-admin and daemon paths:

1. Provision only the LunarDepth lane, with an explicit TensorZero route.
2. Build the tenant daemon and the fat (`--with-toolchains`) LunarDepth image.
3. Audit startup: status, host-loopback listeners, egress, readiness, and
   `worker-smoke`.
4. Dispatch focused medium/high-complexity jobs through the tenant gateway and
   independently verify their artifacts.
5. Record failures honestly; an upstream/routing/infrastructure failure is not
   reported as a worker success.

## Host and preflight

- Gentoo, OpenRC, rootless Podman, 16 usable CPUs (cgroup limit; the host view
  is 18 CPUs, commands ran under `taskset -c 0-17`).
- `/sys/kernel/security/lsm` includes `landlock`, satisfying the lane's
  Linux 6.10 / Landlock ABI 5 floor.
- 297 GiB free on `/`, 25 GiB available RAM. `/usr/bin/setpriv` is present
  (the WI#685 `util-linux caps` fix persists).
- Registry before provisioning contained `scootaloo` (10010) and a leftover
  `y` (10030); no `lunardepthtester` account existed.
- TensorZero at `http://100.88.232.146:3000/status` returned HTTP 200. A live
  `tensorzero::function_name::lunarwing` request returned exactly
  `WI719_ROUTE_OK`, `finish_reason=stop`, 39 completion tokens. This proves the
  model path before the lane enters it.
- Lane source pin in this tree is the newer
  `0472d115e5297093f249c222639431ba8ff0bb78` (the WI#692 pin bump), unlike the
  `da06c33b…` pin used for WI#685.

## Provisioning

Provisioned with:

```bash
sudo LUNARWING_SERVICE_MANAGER=openrc LUNARWING_CONTAINER_RUNTIME=podman \
  bash ic/scripts/lunarwing-mt-admin.sh add-tenant lunardepthtester \
  --no-ssh --no-weechat-bootstrap \
  --llm-base-url http://100.88.232.146:3000/openai/v1 \
  --with-lunardepth \
  --lunardepth-base-url http://100.88.232.146:3000/openai/v1 \
  --lunardepth-model tensorzero::function_name::lunarwing \
  --lunardepth-allow-insecure-http \
  --lunardepth-allow-unauthenticated-gateway
```

The registry allocated base 10000 (gateway 10000, HTTP 10001, bridge 10002,
PostgreSQL 10003), extended base 20000 (LunarVision 20005/20006), and LunarDepth
52000/52001. Only `workers.lunardepth` is selected. SSH provisioning and WeeChat
relay bootstrap were skipped as unrelated to this lane test.

`ADD_EXIT=0` after about four minutes. The tenant clone is at
`85543ba8999edec0d2fe30a97c121b49228b1774` — the same tree as this report.
The generic sandbox image (`lunarwing-sandbox:latest`) and the LunarVision
sidecar were built and distributed successfully, and PostgreSQL is running.

## Build

The fat build ran through mt-admin under tmux, constrained to CPUs 0–17:

```bash
sudo LUNARWING_SERVICE_MANAGER=openrc LUNARWING_CONTAINER_RUNTIME=podman \
  taskset -c 0-17 bash ic/scripts/lunarwing-mt-admin.sh \
  build-tenant lunardepthtester --with-lunardepth --with-toolchains
```

`BUILD_EXIT=0`. The daemon release build finished in **9m17s** and the XMPP
bridge release build in **7m52s**; both were compiled inside the tenant account
with its own `1.96.1` toolchain. The LunarDepth phase cloned the pinned source
(`0472d115e5297093f249c222639431ba8ff0bb78`), verified both ancestry markers and
the pnpm 11.7.0 package manager, ran the focused Node tests
(`effective_config_assert`, `task_runner`, `bridge`, `runtime`,
`verify_deployed_closure`, `workspace_path`), and materialized the runtime
closure with the shared-lockfile deploy. The `RUN --network=none` split-role
smoke started the non-root bridge and reached ready before the image was
committed as candidate `1c16fb498d30`.

The image was promoted to `lunarwing-worker-lunardepth:latest`:

| Field | Value |
| --- | --- |
| Image ID | `1c16fb498d30d712e8504424fb779628d03e4374ee47f39fdcf46ae2159758bd` |
| Size (admin store) | 2,141,243,412 bytes |
| `org.opencontainers.image.revision` | `85543ba8999edec0d2fe30a97c121b49228b1774` |
| `org.opencontainers.image.toolchain-profile` | `true` |
| `org.lunarwing.lunardepth.revision` | `0472d115e5297093f249c222639431ba8ff0bb78` |
| `org.lunarwing.lunardepth.profile-schema` | `1` |

The out-of-band verifier (`setpriv` as `nobody`, isolated interpreter, archive +
config + lane snapshot) passed silently; it did not fail on `setpriv` this time
because the WI#685 host fix persists.

Provisioning hiccups, neither blocking:

- `rustup` printed that Rust was already installed and continued (benign
  idempotence).
- `add-tenant` compiled the sandbox sidecars and vision sidecar from source,
  which is expected on a cold host but adds several minutes.

## Runtime audits

- `start-tenant` returned **0**. OpenRC started PostgreSQL and its supervisor,
  the daemon, the XMPP bridge, WeeChat plus adapter, LunarVision plus supervisor,
  and both LunarDepth roles. The proxy and the other worker lanes are not
  selected and were not started.
- Gateway `:10000/api/health` returns `{"status":"healthy","channel":"gateway"}`.
- LunarDepth `:52001/health` reports `lunardepth-worker-1.0.0`,
  `source_revision=0472d115…` and `worker_id=worker-lunardepth-lunardepthtester`.
  `/ready` returns `ready: true` with `websocket`, `task_runner` and `task_gate`
  all true.
- `worker-listeners lunardepthtester` is **clean**: both bridge and task
  publications are host-loopback only, the expected non-loopback container binds
  are 52000/tcp and 8443/tcp (published on loopback), and the task has no
  non-loopback listener.
- `worker-egress lunardepthtester` shows both containers exclusively attached to
  `lw-egress-lunardepthtester` (10.89.10.0/24) with DNS pinned and static hosts
  current. The live forward/input reject rules are verified and the only derived
  model target is `100.88.232.146` TCP 3000.
- `worker-smoke lunardepthtester lunardepth` returned **0** with no stdout
  (protocol/readiness/authentication/listener plus confined-bash checks).
- The task container carries the fat toolchain: `python3` 3.11.2,
  `g++` 12.2.0 (Debian 12.2.0-14+deb12u1), `node` v22.23.4, `/usr/bin/setpriv`.
- Hiccup (cosmetic, non-blocking): `mt-admin status` reports
  `LunarDepth worker: running`, but its OpenRC unit listing does not include
  `lunarwing-lunardepth-lunardepthtester` or
  `lunarwing-lunardepth-task-lunardepthtester` — the list only enumerates the
  other lanes (as `unknown`). The worker's own health and `/ready` endpoints,
  `worker-listeners` and `worker-smoke` all confirm the pair is running.
