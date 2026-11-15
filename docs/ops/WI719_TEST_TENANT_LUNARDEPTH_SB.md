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

## Job exercise

Jobs ran sequentially through fresh gateway threads. Each prompt asked the
tenant agent for exactly one `create_job(mode="lunardepth", wait=false)` call;
the harness polled that thread's Engine V2 pending gate, approved it with the
owning `thread_id` and `always` so later calls were covered, found the job by a
unique title prefix, and saved job detail/events. The gateway bearer was read
only inside the script and never printed.

The tasks were chosen for the lane's fat Python/C++ toolchain rather than the
Rust/Go sets used by other workers:

1. **Medium — Python log pipeline:** deterministic 360-row fixture with two
   malformed rows at fixed line numbers, strict parser plus text/JSON reports,
   nearest-rank percentiles, at least 12 unit tests, and a byte-identical rerun
   check.
2. **Medium/high — C++17 weighted interval scheduling:** DP plus
   reconstruction, an independent exhaustive-subset oracle over at least 260
   seeded instances, edge cases, and an N=2500 scaling case, compiled with
   warnings as errors.
3. **High — parallel statistics with an independent oracle:** deterministic
   24,000-row CSV, C++17 `std::async` aggregation over 1/4/8 partitions with an
   FNV-1a checksum, and an independent Python recomputation of every aggregate.

### Job 1 — Python access-log pipeline: PASS

- Job `10372b43-58e3-4da2-8c71-9723a2bd0eb8`, mode `external:lunardepth`,
  created 22:08:28Z and completed 22:29:22Z (**1284 s** job elapsed; 1286 s wall
  including discovery/approval). 25 persisted job events, `stuck=false`, zero
  repair attempts.
- The worker produced `generator.py`, `log_pipeline.py`,
  `test_log_pipeline.py`, `access.log`, `report.txt`, `report.json`, `run1/`,
  `run2/` and `SUMMARY.md` under
  `/workspace/wi719_python_log`.
- **Independent host re-run of the tests:** all 18 tests passed.
- **Independent host parser** (not the worker's module) reproduced every value:

| Metric | Worker | Independent |
| --- | ---: | ---: |
| total rows | 360 | 360 |
| well-formed rows | 358 | 358 |
| malformed line numbers | 71, 171 | 71, 171 |
| `/index` / `/search` / `/cart` | 120 / 119 / 119 | 120 / 119 / 119 |
| status ≥ 500 | 58 | 58 |
| status ≥ 400 fraction | 0.311111 | 0.311111 |
| nearest-rank p50 / p95 (ms) | 516 / 951 | 516 / 951 |

- **Determinism:** `diff -r run1 run2` is empty, and regenerating the fixture on
  the host reproduced `access.log` byte for byte (SHA-256
  `a1a1ae8a74ae9232be749c0784175e6cd0d15d99fa58e1642e9d1245d2329d4f`).

### Job 2 — C++17 weighted interval scheduling: PASS

- Job `f43d93a1-d0dd-486b-b37b-e2002f4a5ec2`, mode `external:lunardepth`,
  completed with 19 persisted events, `stuck=false`, zero repair attempts.
  It ran under the `always` approval granted on job 1; no second gate appeared,
  so the harness had no gate to answer (see finding F3).
- `/workspace/wi719_cpp_wis` contains `wis.hpp`, `main.cpp`, `Makefile`,
  `run1.stdout/stderr`, `run2.stdout/stderr`, the built `wis_test` and
  `SUMMARY.md`.
- The worker's driver reports 8/8 directed cases, an exhaustive-subset oracle
  over 300 seeded instances (value + reconstruction non-overlap + index
  consistency), every edge case, and a scaling case with N=2500.
- **Independent forced rebuild** on the host with
  `g++ -O2 -std=c++17 -Wall -Wextra -Werror` succeeded. Two fresh executions
  were byte-identical to each other, and identical to the worker's transcript
  except for one added line the worker prints to stderr
  (`scaling case DP time: …`, added after it captured `run2.stdout`).
  The two worker transcripts are themselves byte-identical (SHA-256
  `2111187a86df892fa026dec878400d0588672034e30e7ab61e19986fc73da8cd`).
- **Independent oracle of the algorithm itself** (not the worker's driver, and
  not the exhaustive oracle inside it): a separate Python DP using
  `bisect` over finish-sorted intervals, cross-checked against brute-force
  subset enumeration, agreed with `wis::solve` on 120 fresh seeded instances
  (including empty input and negative weights): 0 mismatches.
- **Independent scaling check:** the C++ instance generator was regenerated
  from its documented seed (`7192500`) and fed to the independent Python DP,
  which reproduced the reported `optimum=732406` and `selected=1183`.
