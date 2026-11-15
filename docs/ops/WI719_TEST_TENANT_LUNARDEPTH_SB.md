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
