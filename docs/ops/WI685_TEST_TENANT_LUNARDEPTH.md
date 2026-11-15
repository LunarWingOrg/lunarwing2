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

The daemon release build finished in **9m18s** and the XMPP bridge release
build finished in **7m49s**. The LunarDepth image phase is in progress: its
pinned source checkout, pnpm 11.7.0 frozen install, runtime-closure check (4
agent presets / 182 workspace packages), native host build, and TypeScript
build have run. pnpm printed its expected unsupported-platform, cyclic-workspace,
and not-yet-built `bin.js` warnings; none stopped the build.

The fat candidate image was produced as
`269216fa5ab8658cda7afe2dcd5256ae5395ac66d4f9b7a1fe8e4a5bbaa4587c`.
Its real `RUN --network=none` split-role smoke regenerated the task
configuration, started the non-root bridge, brought the task harness and
runner to ready, and shut both roles down cleanly. This proves the image's
fake-model composition/readiness path, not real TensorZero inference.

The first full build then exited **1** during out-of-band verification:
`17-lunardepth.sh:383: setpriv: command not found`. This is a host prerequisite
failure, not a failed image policy assertion. The candidate was not promoted to
`:latest`. Gentoo's installed `sys-apps/util-linux-2.42.3` has `caps` disabled,
and the repository's `docs/guides/GENTOO_PACKAGE_LIST.md` §1b states that this
USE flag builds `setpriv`. Following that guide, I added
`sys-apps/util-linux caps` as `/etc/portage/package.use/lunarwing-setpriv` and
started the one-package emerge. That succeeded and installed
`/usr/bin/setpriv`; no verifier privilege check was bypassed.

The mt-admin retry returned **0**. It rebuilt the runtime layers, repeated the
network-isolated split-role smoke, ran the out-of-band verifier as `nobody`,
and promoted verified image
`3285688081822c91597ca60defe18abd7dc1038821fa2ecbe03f2f2efdbc39c9`
(2,126,553,198 bytes) to `lunarwing-worker-lunardepth:latest`. Its source label
still names the original pin `da06c33b2091546dc38c61b95452ce27a4ce32d5`;
WI#692 was not silently applied. The image is the fat `--with-toolchains`
build, and the daemon/bridge stages completed in 1.22 s / 1.00 s on this retry.

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

Live startup and audit results:

- `start-tenant lunardepthtester` returned **0**. The daemon, PostgreSQL and
  its supervisor, XMPP bridge, WeeChat plus adapter, LunarVision plus
  supervisor, and the LunarDepth bridge/task pair are running under OpenRC.
  The proxy and LunarVoice are intentionally not running; the other worker
  lanes were never selected.
- Gateway `:10010/api/health` is healthy. LunarDepth `:52011/health` reports
  `lunardepth-worker-1.0.0` and the original source pin; `/ready` returns
  `ready: true`, with WebSocket, task runner, and task gate all ready.
- `worker-listeners lunardepthtester` is **clean**. Bridge publications are
  host-loopback-only, its expected container binds are 52010/tcp and 8443/tcp,
  and the task has no non-loopback listener.
- `worker-egress lunardepthtester` shows both roles exclusively attached to
  `lw-egress-lunardepthtester` (10.89.12.0/24), DNS pinned and static hosts
  current. The live forward/input reject rules are verified, and the only
  derived model target is `100.88.232.146` TCP 3000.
- `worker-smoke lunardepthtester lunardepth` returned **0** (with no stdout).
  As noted above, this covers live protocol/readiness/authentication/listener
  and confined-bash checks, not real model inference.
- Admin and tenant stores carry the same image ID
  `3285688081822c91597ca60defe18abd7dc1038821fa2ecbe03f2f2efdbc39c9`;
  the toolchain-profile label is `true`. In the task container,
  `python3 --version` = 3.11.2, `g++` = Debian 12.2.0-14+deb12u1, and
  `node --version` = 22.23.4.

Real TensorZero job evidence: pending.

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

### Job 1 — Python log pipeline: PASS

- Job `213f3217-2f06-4b05-b06f-2c139a05d10d`, mode
  `external:lunardepth`, created 09:25:53Z and completed 09:33:41Z
  (**468 s** job elapsed; 500.6 s including gateway discovery/approval).
  Approval request `3e91a95b-4bb0-41d8-97cd-86286a67f181`; 23 persisted job
  events; zero repair attempts and no stuck flag.
- The worker emitted real bash/write activity and produced `generator.py`,
  `access.log`, `log_pipeline.py`, `test_log_pipeline.py`, four reports, and
  `SUMMARY.md` under `/workspace/wi685_python_log`.
- **Independent in-container rerun:** all 16 tests passed; text and JSON report
  diffs were empty; regenerating the fixture produced a byte-identical file.
  The copied fixture's SHA-256 is
  `d413598d8a486703af49121e5b5a30c07f2680c2c9a4afb47270c47388caf4b1`.
- **Independent parser (not the worker's module):** 300 total, 298 parsed,
  exactly two malformed rows at 61/131; `/index` 161, `/search` 91, `/cart` 46;
  status-500 8; 39 errors at status >=400, rate 0.130872; nearest-rank p50=55
  and p95=436. Every value matches the worker's report.

### Job 2 — C++17 weighted interval scheduling: PASS

- Job `04044cd8-c2a0-4e1b-a055-57b91fd7ed81`, created 09:36:07Z and
  completed 09:41:39Z (**331 s** job elapsed; 350.4 s wall including
  dispatch). It used the previous `always` approval, emitted 21 events, and
  had zero repairs/stuck state.
- `/workspace/wi685_cpp_wis` contains `wis.hpp`, `main.cpp`, `Makefile`, the
  binary/object, transcript, and `SUMMARY.md`. The test driver has an actual
  exhaustive-subset oracle independent of the DP recurrence.
- Reported and independently reproduced results: 8/8 directed cases; 240
  seeded random instances with zero optimum/reconstruction mismatches; N=2000
  optimum 463293 with 761 selected and matching independent DP; empty,
  touching-endpoint, heavier-equal-overlap, duplicate, and negative-weight
  edge cases all passed.
- **Independent forced rebuild and rerun:** `make -B` succeeded with
  `g++ -O2 -std=c++17 -Wall -Wextra -Werror`; two new executions matched each
  other and the original transcript byte for byte (SHA-256
  `301cc5e1090fb57583164cd58e4599fde8a023dc99a2cace8ec7a7dbce0a15c4`).
- Minor environment hiccup: the worker noted that the optional `file` utility
  is absent. It did not affect compilation or execution; the actual binary ran
  successfully with exit 0.

Job 3: pending.

## Findings and hand-off

Pending.
