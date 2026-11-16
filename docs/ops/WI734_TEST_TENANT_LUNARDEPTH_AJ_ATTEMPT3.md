# WI#734 — LunarDepth external-worker test, third attempt (`lunardepthtester`, AJ edition)

> Run started 2026-11-16 20:36Z by Applejack on branch
> `aj/kimi-wi734-20261116-2031`, from the target
> `aragents/kimil00p/v2.2.0.0` at `4473ae365`.
> This is the third attempt at the WI#720 exercise. The first (!2487) was closed
> unmerged; the second (!2497, `5ac73beaf`) recorded a blocked run in
> `docs/ops/WI734_TEST_TENANT_LUNARDEPTH_AJ.md` and left boxes 3 and 4 of this
> item open. That document is kept as it was; this one records the attempt that
> set out to close those two boxes. The goals file is deliberately unchanged;
> this is a work-item run.

## Scope

Work item #734 asks for a tenant named `lunardepthtester` with the LunarDepth
external worker and `--with-toolchains`, jobs of medium to high complexity, and
a record of the results and every hiccup. Boxes 1, 2, 5 and 6 were met by
!2497. This attempt has to meet the two that were left:

3. the tenant has the LunarDepth worker **running** with `--with-toolchains`,
   with the build result and the runtime check in this report;
4. jobs of medium to high complexity ran, with their results.

The constraint that shaped the run is §3 of the item: the run has to be
finished before the MR is opened. The work order was therefore host record →
worker image → runtime audit → jobs → this document.

## Host and preflight

Recorded before the first `mt-admin` command of this attempt, on the host this
run used:

| Fact | Value |
| --- | --- |
| Distribution | Arch Linux (rolling), per `/etc/os-release` |
| PID 1 | `systemd` (`/proc/1/comm`) |
| `rc-service` | absent (`command -v rc-service` → not found) |
| `systemctl` | `/usr/sbin/systemctl` |
| Kernel | `6.18.54-2-lts` |
| Podman | `podman version 6.2.0` |
| CPUs | 12 (`nproc`); every build confined with `taskset -c 0-11` |
| Health pipeline | `/etc/lunarwing/health.env` reads `LUNARWING_SERVICE_MANAGER=systemd` |
| Tenants in the registry | `hermes`, `lunardepthtester` |

No command of this run sets `LUNARWING_SERVICE_MANAGER`. The tenant's units are
systemd **user** units under the tenant account, created by the previous attempt
and found in place; this run created no unit.

### State in which the tenant name was found

Read-only, before anything was done:

- `mt-admin status lunardepthtester`: tenant present; gateway `10010`
  listening (`{"status":"healthy","channel":"gateway"}`); PostgreSQL `running`
  (`lunarwing-pg-lunardepthtester`); `LunarDepth worker: not created`;
  `lunarwing-worker-lunardepth:latest: missing`; LunarVision healthy
  (20015/20016); user unit `lunarwing-lunardepthtester.service` active.
- `/etc/lunarwing/ports.json` holds `lunardepth_base 52010` with
  `lunardepth_wss 52010` and `lunardepth_health 52011`.
- The tenant clone is at `d872e9632`, the second attempt's report commit.
- No `lunarwing-worker-lunardepth` image in the root store or in the tenant's
  store.

The tenant was **reused as it stands**. Nothing was removed, no account or
registry entry was recreated, and no new tenant was made. The maintainer was
asked on this item in note 18277 (20:02Z) with three options; no answer had come
by 20:39Z, so the run took the recommended option 2 (reuse, worker work only),
which removes nothing and adds no workaround on the host. The decision is
recorded in note 18295.

## Worker image

The second attempt could not build the image: its own out-of-band export
verifier rejected the archive because the closure carried hard links. That is
WI#739, whose fix is MR !2501. The fix changes only
`lunardepth4lunarwing/scripts/verify_exposure_out_of_band.py` (the link rule) and
its test; neither the Dockerfile nor the pinned lane source changes, so the
image content is the same as the one the failed builds produced.

For this run the two files of MR !2501 (`d3205e1c8`) were copied into the
worktree so `mt-admin build-lunardepth-worker` would verify the image it built.
They are not committed here: the patch belongs to !2501, and it was still open
when this run needed it. The image label therefore reads
`org.opencontainers.image.revision = 4473ae365…-dirty`; the `-dirty` is these
two uncommitted lane files, not a change to the worker's runtime code.

The build ran through the standard path under tmux, confined to 12 CPUs, with no
service-manager override:

```bash
sudo taskset -c 0-11 bash ic/scripts/lunarwing-mt-admin.sh \
  build-lunardepth-worker --with-toolchains
```

`WORKER_BUILD_EXIT=0` after 28m56s (20:39:08Z→21:08:04Z). Its stages:

| Stage | Result | Evidence |
| --- | --- | --- |
| Pinned lane clone and guards | **PASS** | `Detected Node v22.23.4 and pnpm 11.7.0. Version guard passed`; revision `0472d115e5297093f249c222639431ba8ff0bb78` |
| Focused worker tests | **PASS** | `Test Files 5 passed (5)`, `Tests 35 passed (35)`, 13.82 s |
| Deploy / runtime closure | **PASS** | `verify-runtime-closure: 4 agent presets and 182 workspace packages form a closed runtime dependency graph` |
| Split-role image smoke | **PASS** | `LunarDepth split-role image smoke passed` (bridge and task roles started and stopped cleanly) |
| Out-of-band export verifier | **PASS** | silent, exit 0 — the WI#739 fix accepted the shared-inode hard link |

The image was committed as candidate `lunarwing-worker-lunardepth:candidate-2908479`
and promoted after the verifier passed:

| Field | Value |
| --- | --- |
| Image ID | `d083885d1e0871ed2cb948aa81ce17791d22acdd745f9f31cd60c26e5c2b38df` |
| Size (admin store) | 2,096,584,209 bytes |
| `org.opencontainers.image.revision` | `4473ae3655822a8641e49b0d0d0ca41725a35f87-dirty` (WI#739 patch in the lane scripts) |
| `org.opencontainers.image.toolchain-profile` | `true` |
| `org.lunarwing.lunardepth.revision` | `0472d115e5297093f249c222639431ba8ff0bb78` |
| `org.lunarwing.lunardepth.profile-schema` | `1` |
| `Config.User` | `10005:10005` |

`build-lunardepth-worker` does not build the daemon, so unlike the second
attempt's `build-tenant` this run did not recompile `lunarwing`; the tenant
keeps the daemon binary the previous attempt built (`ic/target/release/lunarwing`,
unit active throughout).

## Runtime audit

`start-tenant` returned **0** with no override:

```bash
sudo taskset -c 0-11 bash ic/scripts/lunarwing-mt-admin.sh start-tenant lunardepthtester
```

The tenant's units are systemd user units under the tenant account — the host's
own manager, found in place, not restarted by an override. What the start did to
the LunarDepth lane:

```text
distributing image lunarwing-worker-lunardepth:latest into lunardepthtester's rootless store (save|load — minutes for large images) ...
image lunarwing-worker-lunardepth:latest available in lunardepthtester's store
LunarDepth model backend 100.88.232.146:3000 is derived into lunardepthtester's worker egress allowlist
worker egress filter enforced for lunardepthtester (lunardepth)
config changed for lunarwing-lunardepth-task-lunardepthtester; force-recreating
config changed for lunarwing-lunardepth-lunardepthtester; force-recreating
LunarDepth worker ready via quadlet (lunarwing-lunardepth-lunardepthtester.service, WSS port 52010)
START_EXIT=0
```

| Check | Result |
| --- | --- |
| `mt-admin status lunardepthtester` | `LunarDepth worker: running (lunarwing-lunardepth-lunardepthtester, WSS port 52010)`; `lunarwing-worker-lunardepth:latest: present` |
| `GET 127.0.0.1:52011/health` | `{"status":"ok","mode":"websocket","version":"lunardepth-worker-1.0.0","worker_id":"worker-lunardepth-lunardepthtester","source_revision":"0472d115e5297093f249c222639431ba8ff0bb78"}` |
| `GET 127.0.0.1:52011/ready` | `{"ready":true,"websocket":{"ready":true},"task_runner":{"ready":true},"task_gate":{"ready":true}}` |
| Tenant gateway `127.0.0.1:10010/api/health` | `200 {"status":"healthy","channel":"gateway"}` |
| `worker-listeners lunardepthtester` | exit 0, `worker listener audit: clean`; bridge published on host loopback only, task `no non-loopback listeners` |
| `worker-egress lunardepthtester` | exit 0, `filter: live`; both containers exclusive on `lw-egress-lunardepthtester` (10.89.10.0/24), single allowlist target `100.88.232.146:3000` |
| `worker-smoke lunardepthtester lunardepth` | exit **0**, no stdout |
| Task container toolchain | `python3` 3.11.2, `g++ (Debian 12.2.0-14+deb12u1) 12.2.0`, `node` v22.23.4, `/usr/bin/setpriv`; the task process runs as the non-root container user (`lunarde+` on the host) |

## Job exercise

Jobs ran through fresh gateway threads on the tenant's own gateway
(`127.0.0.1:10010`). Each prompt asked the tenant agent for exactly one
`create_job(mode="lunardepth", wait=false)` call; the harness polled the
thread's pending gate, approved it with the owning `thread_id` and `always`,
found the job by a unique title marker, and saved the job detail and events. The
gateway bearer token was read inside the script through `mt-admin tokens` and
never printed.

The tasks were chosen for the lane's fat Python/C++ toolchain:

1. **Medium — Python access-log pipeline:** deterministic 360-row fixture with
   three malformed rows at fixed line numbers, strict parser with text/JSON
   reports, nearest-rank percentiles, at least 12 unit tests, and
   byte-identical rerun checks.
2. **Medium/high — C++17 weighted interval scheduling:** DP plus
   reconstruction, an independent exhaustive-subset oracle, edge cases, and an
   N=2500 scaling case, compiled with warnings as errors.
3. **High — parallel statistics with an independent oracle:** deterministic
   24,000-row CSV, C++17 `std::async` aggregation over 1/2/4/8 partitions with
   an FNV-1a checksum, and an independent Python recomputation.

### Job 1 — Python access-log pipeline: PASS

- Job `59919468-d069-4a32-aed9-6721dc9aacf4`, mode `external:lunardepth`,
  created 21:49:08Z and completed 21:58:39Z (**571 s**). The worker wrote
  `generate_logs.py`, `log_parser.py`, `test_log_pipeline.py`, `access.log`,
  `report.txt`, `report.json`, a second pair of reports, three empty diff
  proofs and `SUMMARY.md` under `/workspace/wi734_aj_log_pipeline`.
- **Independent host re-run of the worker's tests:** `Ran 20 tests ... OK`.
- **Independent host parser** (written for this report, not the worker's
  module) reproduced every value in `report.json`:

| Metric | Worker | Independent |
| --- | ---: | ---: |
| total lines / valid | 360 / 357 | 360 / 357 |
| malformed line numbers | 42, 181, 307 | 42, 181, 307 |
| `/api` / `/search` / `/checkout` / `/health` | 90 / 89 / 89 / 89 | 90 / 89 / 89 / 89 |
| 2xx / 4xx / 5xx | 286 / 50 / 21 | 286 / 50 / 21 |
| errors (status ≥ 400), rate | 71, 0.1989 | 71, 0.1989 |
| nearest-rank p50 / p95 / p99 (ms) | 285 / 1328 / 2135 | 285 / 1328 / 2135 |

- **Determinism:** all three `diff` files are 0 bytes (text report pair, JSON
  report pair, regenerated fixture), and the SHA-256 pairs match
  (`access.log` = `7f8e0303…e45aa27`, `report.json` = `f59fed2f…b3aefdd`).

### Job 2 — C++17 weighted interval scheduling: PASS

- Job `c6c15f0b-92ca-415a-b3df-b020f06b3649`, mode `external:lunardepth`,
  created 22:01:29Z and completed 22:15:16Z (**827 s**). The worker left
  `wis.hpp`, `main.cpp`, `tests.cpp`, `Makefile`, three transcripts and
  `SUMMARY.md` under `/workspace/wi734_aj_cpp_wis`.
- The worker's driver reports **15/15 directed cases**, an exhaustive-subset
  oracle over **320 seeded instances** (optimum weight, reconstructed-set
  weight, and non-overlap on each), and a scaling case at **N=2500** with
  `optimum=1080 selected=62`.
- **Independent forced rebuild on the host** with
  `g++ -O2 -std=c++17 -Wall -Wextra -Werror` succeeded, and the full suite
  passed again: `Directed: 15 passed`, `Oracle: 320 passed`,
  `Scaling: 1 passed`, `FINAL: ALL TESTS PASSED`.
- **Determinism:** `transcript2.txt` and `transcript3.txt` are byte-identical.
- **Independent oracle of the algorithm itself** (a separate Python DP using
  `bisect` over finish-sorted intervals, not the worker's driver): 60 fresh
  seeded instances, including empty input and negative weights, **0
  mismatches** against `wis_main`'s reported optimum.

### Job 3 — parallel statistics with independent cross-check: PASS

- Job `b0d7cdd1-e689-4adb-a1e7-2293a4e5636c`, mode `external:lunardepth`,
  created 22:17:43Z and completed 22:45:42Z (**1679 s**). The worker left
  `generate_data.py`, `data.csv`, `aggregator.cpp`, `check.py`,
  `run_analysis.sh`, per-partition transcripts (1/2/4/8 plus repeats), diff
  proofs and `SUMMARY.md` under `/workspace/wi734_aj_parallel_stats`.
- The worker's checker reports `TOTAL: 140 fields, 0 mismatches` across the
  four partition counts against its own `math.fsum`/`statistics.pstdev`
  recomputation; the partition-4 repeat transcripts are byte-identical.
- **Independent host rebuild and rerun:**
  `g++ -O2 -std=c++17 -pthread -Wall -Wextra -Werror` succeeded, and a fresh
  4-partition run was **byte-identical** to the worker's `transcript_p4.txt`.
- **A separate host-side implementation** (my own parser, not the worker's
  `check.py`) reproduced every aggregate at 6 decimals:

| Group | Count | Sum | Mean | Min | Max | Population stddev |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| A | 6000 | -90632.005644 | -15.105334 | -999.986051 | 999.904089 | 579.872686 |
| B | 6000 | -63801.033643 | -10.633506 | -999.768931 | 999.702694 | 574.014895 |
| C | 6000 | -103022.937076 | -17.170490 | -999.976848 | 999.913292 | 584.094751 |
| D | 6000 | -132997.020243 | -22.166170 | -999.674795 | 999.877539 | 572.493239 |
| ALL | 24000 | -390452.996606 | -16.268875 | -999.986051 | 999.913292 | 577.652431 |

### Job inventory

| Job | ID | State | Job elapsed | Independent result |
| --- | --- | --- | ---: | --- |
| Python access-log pipeline | `59919468-d069-4a32-aed9-6721dc9aacf4` | completed | 571 s | 20 tests + independent metrics/percentiles |
| C++17 weighted interval scheduling | `c6c15f0b-92ca-415a-b3df-b020f06b3649` | completed | 827 s | host rebuild + 320-instance oracle + separate DP oracle |
| Parallel statistics | `b0d7cdd1-e689-4adb-a1e7-2293a4e5636c` | completed | 1679 s | host rebuild + byte-identical p4 run + separate oracle |
| Python access-log pipeline (first dispatch) | `20d31ff7-1318-4393-a28a-777d6fd33da8` | failed | 1816 s | H2: six idle model-stream timeouts |

### The first dispatch of job 1 failed (H2)

The first attempt at job 1, `20d31ff7-1318-4393-a28a-777d6fd33da8`, ran
21:13:46Z→21:44:02Z and **failed** with
`pi-ai stream idle timeout after 300000ms`. Its worker session shows six
consecutive stream attempts (initial plus retries 1–5), each idle for five
minutes and producing no chunk, then `turn/end` with `code: TIMEOUT`. The
receipts are beside the successful run:

- The same route answered from the host in 25.7 s for a 900-token request and
  in 2–4 s for short streaming probes.
- From the **worker's own network namespace**, a replay of the failed session's
  **exact request body** (16,420 bytes, 17 tools) streamed its first chunk in
  1.7 s and completed in 215.5 s.
- The `pi-ai` client called directly inside the task container streamed its
  first event in 1.5 s.

The retry, with no change to the tenant, the image or the route, completed
normally. It is recorded as a host/infra or model-side stall (H2), not a worker
defect, and it is the one thing in this run that could not be reproduced on
demand.


## Findings and hiccups

Each item is classified as a finding about the **product**, the **call** (how the
test harness called it), or the **host**.

1. **F1 (product, resolved by WI#739): the out-of-band verifier rejected the
   image's intra-archive hard links.** In the previous attempt this blocked the
   whole lane. MR !2501 accepts a hard-link member only when its target is an
   earlier regular file beneath the checked roots, and keeps rejecting
   symlinks, links leaving the roots, links to non-regular members, and links
   to unseen paths. Built with that patch, the image verified on the first try
   (`README`-level evidence above). Nothing in this run weakens the gate: the
   verifier is the patched one from !2501, and `worker-lunardepth-ts` and
   `worker-lunardepth-exposure-verify` are green on pipeline 8000.
   - Note for the reviewer: the image label carries `…-dirty` because the two
     !2501 files were applied to the worktree rather than merged. The verifier
     is the only thing that changed; the worker runtime is unaffected.
2. **F2 (product, API shape): `GET /api/chat/pending-gates` returns
   `parameters` as a JSON string, and the action name is `tool_name`.** The
   harness copied from the WI#719 run read `parameters["title"]` and
   `action_name`, and crashed:
   ```text
   File "/tmp/wi734_dispatch.py", line 113, in main
       title = str(parameters.get("title") or "")
   AttributeError: 'str' object has no attribute 'get'
   ```
   The live payload is
   `{"request_id": "945e9dd1-…", "thread_id": "fec915d4-…", "gate_name": "approval", "tool_name": "create_job", "description": "Tool 'create_job' requires approval (gate: approval)", "parameters": "{\n  \"description\": \"Medium-complexity Python …"}`.
   This is a documented-interface question, not a test failure: a client written
   against the WI#719 shape silently breaks. Either the endpoint changed shape
   or the old harness was written against a different route; the report cannot
   tell which. A tolerant client (parse when it is a string, accept
   `tool_name` or `action_name`) works.
3. **F3 (product, small): `create_job` accepted a ~1.8 KB title and an empty
   description.** The model put the whole task into `title` and left
   `description` empty for all four dispatches, despite the instruction to use
   the marker alone:
   ```text
   b0d7cdd1-… completed title= 'wi734-aj3b-parstats\n\nHigh-complexity parallel numerical task' len 1864
   59919468-… completed title= 'wi734-aj3b-pylog\n\nMedium-complexity Python validation task. ' len 1792
   ```
   The job then renders with `"description": ""` and the full text as a title.
   The tasks still ran correctly because the daemon hands the worker the task
   text, so this is display and validation hygiene, not a blocker. It is worth
   a length check or a "title is one line" rule in the tool schema.
4. **H1 (host, model-side): the first job dispatch's LLM stream stalled six
   times.** `20d31ff7-…` failed with `pi-ai stream idle timeout after 300000ms`
   after 30 minutes; the retry of the same task with no change finished in
   571 s. The evidence that the route and worker were healthy is in *Job
   exercise* above. Classified as host/infra: the worker's own network
   namespace streamed the exact request body in 1.7 s.
5. **H2 (host, cosmetic): `mt-admin status` prints `inactive` twice for lanes
   that are not selected.** The service list renders
   ```text
     lunarwing-proxy-lunardepthtester.service: inactive
   inactive
   ```
   — the second `inactive` is the unit's `ActiveState` echoed on its own line.
   The same shape appeared for the pebble/opencode/opencode2 units. Cosmetic;
   the same display is pinned by `test-mt-admin-*` elsewhere, so changing it is
   a design decision.
6. **H3 (call, cosmetic): `podman logs --since 21:13` is rejected**
   (`Error: parsing --since "21:13": unable to interpret time value`). Podman
   6.2 requires a date-qualified or duration value. Not a product issue; noted
   because it slowed the log reading during the run.

## Host changes this run made

In order:

1. **Worker image build** (`build-lunardepth-worker --with-toolchains`): the
   rootless build store gained the pinned base images and the layer cache, and
   the root store gained `lunarwing-worker-lunardepth:latest`
   (`d083885d1e08…`). The failed candidate from the second attempt's state was
   not present; nothing was deleted.
2. **`start-tenant lunardepthtester`**: distributed the image into the tenant's
   rootless store, regenerated the tenant's LunarDepth overlay and task
   environment, created the `lw-egress-lunardepthtester` network and its
   nftables chains, and force-recreated the two LunarDepth quadlet units
   (`lunarwing-lunardepth-lunardepthtester`, `lunarwing-lunardepth-task-lunardepthtester`).
   The daemon, PostgreSQL, vision sidecar and XMPP bridge units were already
   running and were not recreated.
3. **Three job dispatches plus one failed first dispatch**: the tenant's
   `/workspace/wi734_aj_*` directories (inside the task container), the job
   rows in the tenant database, and the tenant's gateway conversations. No
   project outside the tenant was touched.
4. **Read-only probes**: `podman image inspect`, `nsenter` reads of the task
   container's filesystem and `/proc`, `journalctl --user`, `ss`, and four
   model-route probes from the worker's network namespace. The probes created
   no lasting container or network object.

No `LUNARWING_SERVICE_MANAGER` appears in any command or log of this run;
`/etc/lunarwing/health.env` still reads `systemd`, and the host's own tenants
(`hermes`, `antelope`) were not touched.

## Acceptance

- [x] The report's host line is true (Arch Linux, systemd, kernel
  `6.18.54-2-lts`, Podman 6.2.0), and no command of the run forced a service
  manager the host does not run.
- [x] The report says in which state the tenant name was found (present and
  running, without a worker) and what was done about it (reused as it stands,
  nothing removed, no new tenant), with the maintainer asked in note 18277 and
  the decision recorded in note 18295.
- [x] The tenant has the LunarDepth external worker **running** with
  `--with-toolchains`: `build-lunardepth-worker --with-toolchains` produced
  `d083885d1e08…` with `toolchain-profile=true`, and the runtime audit shows
  health/ready true and `worker-smoke` exit 0.
- [x] Jobs of medium to high complexity ran and their results are in this
  report: three jobs completed and each was re-verified on the host by an
  independent implementation; a fourth dispatch failed on a model-stream stall
  (H1) and is recorded as such.
- [x] Every hiccup is recorded with its command and output, classified as a
  finding about the product, the call or the host.
- [x] No section of this report is pending.

