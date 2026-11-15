# WI#685 — LunarDepth external-worker test (`lunardepthtester`)

> Test started 2026-11-15 by Rarity on branch
> `rarity/kimi-wi685-20261115-0830`, at tree `028404983`.
> This report is a work in progress while the live tenant exercise runs.

## Scope

Work item #685 asks for a new tenant named `lunardepthtester`, with the
LunarDepth external worker and `--with-toolchains` enabled, several medium- to
high-complexity jobs, and a record of the results and any hiccups. The goals
file is deliberately unchanged; this is a work-item run.

The exercise uses the real mt-admin and daemon paths:

1. Provision only the LunarDepth lane, with an explicit TensorZero route.
2. Build the tenant daemon and the fat LunarDepth worker image.
3. Check status, host-loopback listeners, egress, readiness, confined writes,
   and denied escapes with `worker-smoke`.
4. Dispatch focused medium/high-complexity jobs through the tenant gateway and
   independently inspect or re-run their artifacts.
5. Record failures without converting an upstream, routing, or infrastructure
   failure into a claimed worker success.

## Host and preflight

- Gentoo, OpenRC, rootless Podman 6.1.4, kernel 6.18.35-gentoo-dist-bin.
- `/sys/kernel/security/lsm` includes `landlock`; the running kernel exceeds the
  lane's Linux 6.10 / Landlock ABI 5 floor.
- 58 GiB free on `/`, 15 GiB available RAM before provisioning.
- Existing registry tenant: `dash`. No `lunardepthtester` account and no admin
  `lunarwing-worker-lunardepth:latest` image existed at preflight.
- TensorZero at `http://100.88.232.146:3000` returned HTTP 200 on `/status`
  (version 2026.6.0). An unauthenticated
  `/openai/v1/chat/completions` request using
  `tensorzero::function_name::lunarwing` returned HTTP 200. The deliberately
  tiny 16-token cap produced an empty completion with `finish_reason=length`;
  this probe establishes route reachability and authentication behavior, not a
  successful content test.
- The lane therefore uses its explicit
  `--lunardepth-allow-unauthenticated-gateway` mode, not a copied sibling key.
  The private-network cleartext route also requires the separate
  `--lunardepth-allow-insecure-http` exception.

## Known route-risk before the build

WI#691's lane-side TensorZero renderer is present in this checkout. WI#692 is
still open, however: this tree pins LunarDepth source
`da06c33b2091546dc38c61b95452ce27a4ce32d5`, while #692 requests
`0472d115e5297093f249c222639431ba8ff0bb78` for upstream TensorZero routing and
profile schema v1. This test does not silently absorb that separate pin bump.
Any failure attributable to the old pin will be reported as such.

## Provisioning and build

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

`ADD_EXIT=0` after about 220 seconds. The tenant clone is at commit
`9afc94d78283fd3d85d865c0418b93846fa43c90`, so it includes this report's
initial commit but no later test-only edits. Only `workers.lunardepth` is
selected. The generic sandbox image was built and distributed successfully,
and PostgreSQL is running. SSH provisioning and WeeChat relay bootstrap were
intentionally skipped as unrelated to this lane test; WeeChat itself remains
enabled by the normal tenant defaults.

| Purpose | Port |
| --- | ---: |
| gateway / HTTP / bridge | 10010 / 10011 / 10012 |
| PostgreSQL | 10013 |
| LunarVision API / health | 20015 / 20016 |
| LunarDepth WebSocket / health | 52010 / 52011 |

Two pre-existing/host-side hiccups appeared during provisioning, neither of
which blocked tenant creation:

- rustup reported that Rust was already installed and continued; this is a
  benign idempotence warning.
- The host health-pipeline installer quarantined an older
  `lunarwing-health-metrics` binary because its installed library path failed
  the root-owned/non-writable check (WI#687). It requested a trusted
  reinstallation before the optional metrics listener is restarted. This test
  does not alter that listener or claim metrics endpoint health. The routine
  `fcron has already been started` warning was also harmless.

The fat build is running through mt-admin under tmux, constrained to CPUs
0–17:

```bash
sudo LUNARWING_SERVICE_MANAGER=openrc LUNARWING_CONTAINER_RUNTIME=podman \
  taskset -c 0-17 bash ic/scripts/lunarwing-mt-admin.sh \
  build-tenant lunardepthtester --with-lunardepth --with-toolchains
```

Build result pending.

## Runtime audits

Pending.

## Job exercise

Pending; jobs will be listed here with their real IDs, outcomes, timings, and
independent verification.

## Findings and hand-off

Pending.
