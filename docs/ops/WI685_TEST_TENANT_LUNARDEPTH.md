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
  (version 2026.6.0). Unauthenticated `/openai/v1/chat/completions` requests
  using `tensorzero::function_name::lunarwing` also returned HTTP 200. Deliberately
  small 16- and 64-token caps were consumed before visible content and ended
  with `finish_reason=length`; an adequately sized 512-token cap returned the
  exact marker `WI685_ROUTE_OK`, `finish_reason=stop`, and usage of 47
  completion tokens. A separate streaming request reconstructed
  `WI685_STREAM_OK`, ended with `finish_reason=stop`, and received `[DONE]`.
  This proves the baseline gateway/model path in both response modes before
  the LunarDepth lane enters it.
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

Source-side prechecks on the exact lane checkout passed:

- `python3 scripts/render_overlay_test.py`: 10 tests passed, including the
  TensorZero/TensorMoon route renderer cases.
- `python3 health_server_test.py`: 4 tests passed.
- `python3 scripts/exposure_policy_assert.py --source .`: passed.
- `python3 -m unittest discover -s scripts -p '*_test.py'`: all 31 lane Python
  tests passed (a superset of the two focused suites above).
- `bash scripts/check-mt-admin.sh` from `ic/`: ShellCheck passed for mt-admin
  and every numbered module, including `17-lunardepth.sh`.
- `bash scripts/tests/test-mt-admin-lunardepth.sh`: passed its registry v18
  migration, lifecycle, bridge flags, overlay, gateway-dialect, verifier
  isolation, fleet-key, and archive checks.
- `bash scripts/tests/test-worker-llm-entrypoints.sh`: passed.
- `bash scripts/tests/test-worker-egress.sh`: all fixture checks passed,
  including cold apply/first start/post-start verification in one rootless
  namespace, fail-closed guard behavior, and bridge/task launch contracts. Its
  optional live arm was not used as a substitute for the real tenant audit.

These do not establish live inference or container confinement. In particular,
mt-admin's remote `worker-smoke` does not enable the smoke client's task
inference path; real inference must come from the gateway jobs below.

Live build/start/listener/egress/smoke results: pending.

## Job exercise

Jobs will run sequentially through fresh gateway threads. Each prompt asks the
tenant agent for exactly one `create_job(mode="lunardepth", wait=false)` call;
the harness discovers that thread's Engine V2 pending gate, approves it with
the owning `thread_id`, finds the job by a unique title prefix, and saves the
job detail/events without printing the gateway credential.

Planned focused tasks, chosen for the lane's Python/C++ toolchain rather than
the Rust/Go sets used by other workers:

1. **Medium — Python log pipeline:** deterministic 300-line fixture with two
   malformed rows, parser plus text/JSON reports, nearest-rank percentiles, at
   least ten unit tests, and byte-identical rerun diffs.
2. **Medium/high — C++17 weighted interval scheduling:** DP plus reconstruction,
   an independent exhaustive oracle over at least 240 seeded instances, edge
   cases, and an N=2000 scaling case, compiled with warnings as errors.
3. **High — parallel statistics with an independent oracle:** deterministic
   20,000-row CSV, C++17 `std::async` aggregation and checksum, repeated 1/4/8
   partition runs, and an independent Python recomputation of every aggregate.

Real IDs, outcomes, timings, and independent verification: pending.

## Findings and hand-off

Pending.
