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

<!-- RESULTS: findings -->

## Host changes this run made

<!-- RESULTS: host-changes -->

## Acceptance

<!-- RESULTS: acceptance -->
