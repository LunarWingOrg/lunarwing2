# WI#720 — LunarDepth external-worker test (`lunardepthtester`, AJ edition)

> Test started 2026-11-15 by Applejack on branch
> `aj/kimi-wi720-20261115-2200`, at tree `85543ba8999e`.
> This report is updated as each live stage completes; unverified results are
> not claimed.

## Scope

Work item #720 asks for a new tenant named `lunardepthtester`, with the
LunarDepth external worker and `--with-toolchains` enabled where supported,
several medium- to high-complexity jobs, and a record of the results and any
hiccups. The goals file is deliberately unchanged; this is a work-item run.

The exercise uses the real mt-admin and daemon paths:

1. Provision only the LunarDepth lane with an explicit TensorZero route.
2. Build the tenant daemon and fat LunarDepth worker image through mt-admin.
3. Check status, readiness, listeners, egress, and confined execution.
4. Dispatch focused jobs through fresh tenant gateway threads, approve the
   owning Engine V2 gate, and wait for terminal states rather than treating
   `in_progress` as success.
5. Independently run or inspect the produced artifacts and record failures.

## Host and preflight

- Gentoo, OpenRC, rootless Podman 6.2.0, kernel 6.18.54-2-lts; 124 GiB free on
  `/` and 18 GiB available RAM before provisioning.
- `lunardepthtester` was absent from the mt-admin registry, and no matching
  account or home directory existed. No prior tenant was removed or reused.
- `/usr/sbin/setpriv` and tmux were available before the build.
- This checkout supports `build-tenant ... --with-lunardepth
  --with-toolchains`; for this lane the option installs the Python/C++ job
  toolchain. The pinned LunarDepth source is
  `0472d115e5297093f249c222639431ba8ff0bb78` (profile schema 1).
- TensorZero `http://100.88.232.146:3000/status` returned HTTP 200 (version
  2026.6.0). An unauthenticated OpenAI-compatible request using
  `tensorzero::function_name::lunarwing` returned HTTP 200, the exact marker
  `WI720_ROUTE_OK`, `finish_reason=stop`, and 53 completion tokens.
- The lane therefore uses its explicit unauthenticated-gateway mode. The
  private-network cleartext route separately requires the insecure-HTTP
  exception. No credential was copied from another tenant.

## Provisioning

Provisioning began at 22:02:20Z with:

```bash
sudo LUNARWING_SERVICE_MANAGER=openrc LUNARWING_CONTAINER_RUNTIME=podman \
  taskset -c 0-11 bash ic/scripts/lunarwing-mt-admin.sh \
  add-tenant lunardepthtester \
  --no-ssh --no-weechat-bootstrap \
  --llm-base-url http://100.88.232.146:3000/openai/v1 \
  --with-lunardepth \
  --lunardepth-base-url http://100.88.232.146:3000/openai/v1 \
  --lunardepth-model tensorzero::function_name::lunarwing \
  --lunardepth-allow-insecure-http \
  --lunardepth-allow-unauthenticated-gateway
```

The initial allocation is gateway/HTTP/bridge block `10010–10019` and
LunarDepth WebSocket `52010`. The tenant clone tracks this branch. Only the
LunarDepth lane was selected. SSH provisioning and WeeChat relay bootstrap
were intentionally skipped as unrelated to this worker test.

One early warning appeared: mt-admin could not pre-warm the rootless Podman
user namespace and deferred its creation to the first Podman call. Provisioning
continued through repository cloning and explicit LunarDepth configuration.
The remaining provisioning, build, runtime, and job results are recorded below
once observed.

## Build and runtime results

Pending live completion.

## Job exercise

Pending live completion. Planned artifacts use unique `wi720_aj_*` directories:

1. **Medium — Python log analytics:** 360 deterministic rows with exactly
   three malformed rows, text/JSON reports, nearest-rank percentiles, at least
   12 unit tests, and byte-identical regeneration/report diffs.
2. **Medium/high — C++17 weighted interval scheduling:** DP plus reconstruction,
   at least 320 seeded exhaustive-oracle instances, directed edge cases, an
   N=2500 scaling case, warnings-as-errors, and deterministic reruns.
3. **High — parallel statistics:** a deterministic 24,000-row CSV, C++17
   `std::async` partition runs, an independent Python numerical oracle,
   checksums, and cross-partition determinism checks.

## Findings and hand-off

Pending live completion. Raw route output, mt-admin logs, dispatch records,
job details/events, and independent verification output are retained under
`/tmp/wi720-*`; no credentials are included in this report.
