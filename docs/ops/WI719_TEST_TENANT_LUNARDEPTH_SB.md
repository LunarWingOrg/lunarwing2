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
  created 22:29:44Z and completed 22:44:18Z (**874 s** job elapsed), with 19
  persisted events, `stuck=false`, zero repair attempts.
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

### Job 3 — Parallel statistics with independent cross-check: PASS

- Job `6a6555e9-90d5-45c5-a414-14b37ec2aae2`, mode `external:lunardepth`,
  created 22:48:28Z and completed 23:09:24Z (**1256 s** job elapsed), with 48
  persisted events, `stuck=false`, zero repair attempts.
- `/workspace/wi719_parallel_stats` contains `gen_data.py`, `data.csv`,
  `sha256_data.txt`, `stats.cpp`, the built `stats`, `check_independent.py`,
  `compare_runs.py`, `out_p1.txt` / `out_p4.txt` / `out_p4_repeat.txt` /
  `out_p8.txt`, both checker outputs, and `SUMMARY.md`.
- The worker's checker reports **84 PASS, 0 FAIL** (4 categories × 7 fields ×
  3 partition counts) against its own `math.fsum`/`statistics.pstdev`
  recomputation, and `compare_runs.py` reports **28 PASS, 0 FAIL** across the
  three partition runs.
- **Independent host rebuild and rerun:**
  `g++ -O2 -std=c++17 -pthread -Wall -Wextra -Werror` succeeded, and a fresh
  4-partition run was **byte-identical** to the worker's `out_p4.txt`.
- **A separate host-side implementation** (my own `csv`/`math.fsum`/
  `statistics.pstdev` script, not the worker's checker) reproduced every
  aggregate at 6 decimals:

| Category | Count | Sum | Mean | Min | Max | Population stddev |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| A | 6000 | 210661.170000 | 35.110195 | -9997.610000 | 9995.890000 | 5757.338318 |
| B | 6000 | 766623.590000 | 127.770598 | -9999.440000 | 9998.730000 | 5755.740371 |
| C | 6000 | 73112.700000 | 12.185450 | -9993.110000 | 9997.040000 | 5757.283212 |
| D | 6000 | 456821.160000 | 76.136860 | -9990.650000 | 9995.230000 | 5801.860529 |

- **FNV-1a checksums** were reproduced independently from the documented
  canonical text (`%.6f` of each value in cents-then-position order, newline
  separated): A `cf760272`, B `0841428d`, C `f1564075`, D `5de07e4d` — all four
  match the worker's output.
- **Fixture determinism:** re-running `gen_data.py` on the host reproduced
  `data.csv` byte for byte, including row order (SHA-256
  `1276b18785fd51e48ab678bf8d562ec739280264146fea43601bbb9b6dc922fe`).

### Job inventory

| Job | ID | State | Job elapsed | Independent result |
| --- | --- | --- | ---: | --- |
| Python access-log pipeline | `10372b43-58e3-4da2-8c71-9723a2bd0eb8` | completed | 1284 s | 18 tests + deterministic reruns + independent metrics/percentiles |
| C++ weighted interval scheduling | `f43d93a1-d0dd-486b-b37b-e2002f4a5ec2` | completed | 874 s | forced rebuild, identical reruns, independent DP + brute force (120 cases), scaling check |
| Parallel statistics | `6a6555e9-90d5-45c5-a414-14b37ec2aae2` | completed | 1256 s | fresh rebuild, identical p4 run, independent aggregates and FNV-1a |

Final `GET /api/jobs?mode=external:lunardepth` contains exactly these three
jobs, all `completed` and all attributed to `external:lunardepth`.

## Findings and hand-off

### Findings

1. **F1 (provisioning: no WASM channels).** I provisioned and built with only
   `--with-lunardepth --with-toolchains`, omitting `--with-wasm`. The daemon
   therefore logged `Native XMPP config detected, but the installable xmpp
   channel could not be installed … Channel 'xmpp' WASM not found` at boot, and
   WeeChat cannot work either because it is also a WASM channel. Gateway job
   dispatch is unaffected, so the lane test is valid, but a normal tenant needs
   `--with-wasm` (the `add-tenant` "next steps" line says so). This is my
   omission, not a lane defect.
2. **F2 (orchestrator warns about Docker on a podman-only host).** At boot the
   daemon logged `Docker is not installed -- sandbox disabled for this session`
   even though the tenant runs rootless Podman and `SANDBOX_RUNTIME=podman` is
   set. `setup_orchestrator` only probes Docker
   (`ic/src/orchestrator/mod.rs:121-135`); that status gates only the container
   job backend, and the external-worker manager starts independently
   (`needs_event_tx` includes `has_external_workers`). The tool-level sandbox
   still used Podman — a sandbox proxy started and the tenant's own `shell` tool
   worked. The message is therefore misleading on every podman-only MT host,
   but it did not block this lane.
3. **F3 (approval semantics for the harness).** The first `create_job` gate was
   answered with action `always`. Job 2 then ran with **no** gate at all, so a
   client that waits for one blocks forever; my first dispatch loop did exactly
   that and had to be stopped (the worker job kept running and finished
   normally). Job 3 produced a `shell` gate first, because the tenant agent ran a
   preparatory shell command before calling `create_job`. A robust external
   client must poll `/api/chat/pending-gates` repeatedly, answer whichever tool
   is gated, and never assume the next gate is `create_job`.
4. **F4 (API ergonomics for external jobs, reproduces WI#685 F4 and adds one).**
   Each job detail has an empty `description` and puts the full task text after
   a newline in `title`; unique title-prefix matching is the practical
   correlation key. New in this run: `browse_url` is
   `/projects/<job-id>/` while `project_dir` is empty for external jobs, so the
   `/api/jobs/{id}/files/*` endpoints (which join on `project_dir`) cannot list
   an external job's artifacts. The worker's `/workspace` on the tenant host is
   the only way to inspect them. `job_kind` is also reported as `sandbox`,
   which mislabels an external-worker job.
5. **F5 (`mt-admin status` omits the LunarDepth units).** `status` reports
   `LunarDepth worker: running (…, WSS port 52000)` but its OpenRC unit listing
   never includes `lunarwing-lunardepth-lunardepthtester` or
   `lunarwing-lunardepth-task-lunardepthtester`; it lists only the other lanes,
   as `unknown`. Worker health, `/ready`, `worker-listeners` and `worker-smoke`
   all confirm the pair, so this is a display gap in the status summary.
6. **F6 (liveness probe counts unselected lanes).** Before the lane came up the
   daemon logged `External worker liveness probe: 0 reachable, 3 unreachable`
   for `lunardepth`, `opencode` and `pebble`, and later `1 reachable, 2
   unreachable`, even though only `lunardepth` is configured for this tenant.
   Informational only, but it looks alarming in a fresh podman tenant's log.
7. **F7 (environment, not a product bug).** `/tmp` on this host is mounted
   `noexec`, so a copied worker binary cannot be run from there; independent
   C++ rebuilds were executed from `/var/tmp`.
8. **F8 (latency pattern, not a hang).** Job 1 had a quiet interval between the
   initial `todo_write`/`bash` events and the first artifacts, and job 3 spent
   its first several minutes on toolchain checks and data generation. Every job
   progressed with `stuck=false` and zero repair attempts, so operators should
   inspect job events rather than cancelling during a silent stretch.

### Final state and evidence

- **`lunardepthtester` is deliberately left running** for operator inspection;
  no remove/purge was run. Final `status` shows the daemon, PostgreSQL and its
  supervisor, XMPP bridge, WeeChat/adapter, LunarVision, and the LunarDepth
  bridge/task pair running under OpenRC.
- Final `/ready` is green (`websocket`, `task_runner`, `task_gate` all true,
  uptime ~66 min). `worker-listeners` is still clean, `worker-egress` still
  verifies both roles and the live allowlist, and a second `worker-smoke`
  returns 0 after all jobs.
- Ports remain allocated: gateway 10000, LunarVision 20005/20006, LunarDepth
  52000/52001. Both image stores hold
  `1c16fb498d30d712e8504424fb779628d03e4374ee47f39fdcf46ae2159758bd`.
- Worker artifacts live in
  `/home/lunardepthtester/lunarwing/lunardepth-workspace/` (`wi719_python_log`,
  `wi719_cpp_wis`, `wi719_parallel_stats`). Raw job details/events and copied
  artifacts are under `/tmp/wi719-results/`; logs are `/tmp/wi719-add.log`,
  `/tmp/wi719-build.log` and `/tmp/wi719-dispatch.log`. No credential appears in
  the report or the evidence.
- Root filesystem usage went from 297 GiB free to 288 GiB free after the daemon
  and worker image builds.
- No daemon, lane, mt-admin, CI, or goals-file source was changed. The only
  repository deliverable is this report.

### Verification limits

This is live OpenRC/rootless-Podman/TensorZero evidence on a dev host. It is not
a protected CI candidate, clean-rebuild digest comparison, CRITICAL scan,
manual image promotion, or a systemd run. The lane used the explicit
unauthenticated-gateway and private-network cleartext exceptions for this herd
endpoint. Cron, steering, tool proxy, resume, and credential injection remain
unsupported by this lane and were not exercised. No WASM channels were
installed (F1), so XMPP and WeeChat behaviour was not validated.
