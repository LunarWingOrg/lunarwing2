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

Pending.

## Runtime audits

Pending.

## Job exercise

Pending; jobs will be listed here with their real IDs, outcomes, timings, and
independent verification.

## Findings and hand-off

Pending.
