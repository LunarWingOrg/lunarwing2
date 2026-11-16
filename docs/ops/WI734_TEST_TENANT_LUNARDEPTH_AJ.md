# WI#734 — LunarDepth external-worker test (`lunardepthtester`, AJ edition, second attempt)

> Test started 2026-11-16 by Applejack on branch
> `aj/kimi-wi734-20261116-0731`, whose head contains the target
> `aragents/kimil00p/v2.2.0.0` at `f0d47a882`.
> This is the second attempt at the WI#720 test; !2487 was closed without
> merging and its review (MR !2487 notes 17864 and 17866) is the input for this
> report. The goals file is deliberately unchanged; this is a work-item run.

## Scope

Work item #734 asks for a tenant named `lunardepthtester` with the LunarDepth
external worker and `--with-toolchains` where available, jobs of medium to high
complexity, and a record of the results and every hiccup. This attempt differs
from !2487 in four ways:

1. The test runs on the host's **own** service manager — no
   `LUNARWING_SERVICE_MANAGER` override of any kind.
2. The state in which the tenant name is found is recorded before anything is
   done, and nothing is removed or reused without the maintainer's word.
3. The run is finished — build, runtime and jobs — before the MR is opened.
4. Each hiccup is recorded as what it was: a finding about the call, the
   product, or the host.

## Host and preflight

Recorded before the first `mt-admin` command, on the host this run used:

| Fact | Value |
| --- | --- |
| Distribution | Arch Linux (rolling), per `/etc/os-release` |
| PID 1 | `systemd` (`/proc/1/comm`) |
| `rc-service` | absent (`command -v rc-service` → not found) |
| `systemctl` | `/usr/sbin/systemctl` |
| Kernel | `6.18.54-2-lts` (`uname -a`, `6.18.54-2-lts #1 SMP PREEMPT_DYNAMIC`) |
| Podman | `6.2.0`, rootful for mt-admin (`podman info`: rootless=false, `overlay`); each tenant's containers run in that tenant's rootless store |
| CPUs | 12 (`nproc`); every build was confined with `taskset -c 0-11` |
| RAM / disk at start | 17 GiB available of 23 GiB; 173 GiB free on `/` |
| LSM | `capability,landlock,lockdown,yama,bpf` — Landlock present |
| `setpriv`, `tmux`, `taskset`, `jq` | present |
| Health pipeline | `/etc/lunarwing/health.env` reads `LUNARWING_SERVICE_MANAGER=systemd` |
| Existing tenants | `hermes` (base 10040) and `antelope` (base 10000) |

No command of this run sets `LUNARWING_SERVICE_MANAGER`. The service manager is
whatever `ic/scripts/mt-admin-lib/01-detect.sh` finds on this host, which is
systemd.

### State in which the tenant name was found

Checked before the first `mt-admin` command:

- `getent passwd lunardepthtester` → no such account, so no home directory
  existed under that name either.
- `/etc/lunarwing/ports.json` → `tenants` holds `hermes` and `antelope` only; no
  `lunardepthtester` entry, so the block that the next allocation chose
  (10010–10019 and 52010/52011) was free.
- `/etc/lunarwing/health.env` reads `LUNARWING_SERVICE_MANAGER=systemd`, and a
  `health.env.pre-fix-20261115` backup is present from the host's own recovery.

So the account and registry entry that the previous attempt left on this host
(`/tmp/wi720-*` logs at 22:02–22:09Z on 2026-11-15 are from it) are gone. I
removed nothing and reused nothing; the tenant was created fresh under its own
name. No maintainer decision was needed on this item.

### Model route

- TensorZero `http://100.88.232.146:3000/status` returned HTTP 200, version
  `2026.6.0`.
- An unauthenticated OpenAI-compatible request with
  `tensorzero::function_name::lunarwing` returned HTTP 200 with `finish_reason=stop`
  and the exact marker `WI734_ROUTE_OK` (43 completion tokens; the first probe
  at `max_tokens=40` stopped on `length` mid-reasoning, which is a budget
  artifact, not a route failure).
- The lane therefore uses `--lunardepth-allow-unauthenticated-gateway` and the
  private-network cleartext exception, as WI#719 and WI#685 did.

## Provisioning

Provisioning began at 07:32Z with no service-manager override:

```bash
sudo taskset -c 0-11 bash ic/scripts/lunarwing-mt-admin.sh add-tenant lunardepthtester \
  --no-ssh --no-weechat-bootstrap \
  --llm-base-url http://100.88.232.146:3000/openai/v1 \
  --with-lunardepth \
  --lunardepth-base-url http://100.88.232.146:3000/openai/v1 \
  --lunardepth-model tensorzero::function_name::lunarwing \
  --lunardepth-allow-insecure-http \
  --lunardepth-allow-unauthenticated-gateway
```

`ADD_EXIT=0`. What it did, from `/tmp/wi734-add.log`:

- Allocated block 10010–10019 and LunarDepth 52010/52011; registry entry
  `workers.lunardepth=true`, all other lanes false.
- Created the account, enabled linger, and rendered **systemd user units** into
  `/home/lunardepthtester/.config/systemd/user`
  (`lunarwing-lunardepthtester.service`, `xmpp-bridge-lunardepthtester.service`,
  the WeeChat pair, `lunarwing-build.slice`, `lunarwing-workloads.slice`). This
  is the host's own manager, chosen by detection, not by an override.
- Started PostgreSQL as the user quadlet `lunarwing-pg-lunardepthtester` on
  10013.
- Distributed the generic sandbox image into the tenant's rootless store.
- Left `/etc/lunarwing/health.env` alone (`already exists (preserving)`) and
  scheduled the host pipeline through its **systemd timer**.
- Wrote the LunarDepth external-worker block with the explicit `tensorzero::`
  route and the unauthenticated-gateway exception; no key stored.
- Tenant clone at `d872e9632` on branch `aj/kimi-wi734-20261116-0731`, whose
  history contains `f0d47a882` (the target `aragents/kimil00p/v2.2.0.0`).

One hiccup at provisioning, non-blocking and already a tracked bug:

- **H1 (host, cosmetic):** `WARNING: could not pre-warm the rootless podman
  userns for lunardepthtester; the first podman call will create it in its own
  context (see docs/bugs/BUG-workload-scope-podman-userns.md)`. Image
  distribution and the PostgreSQL quadlet both succeeded afterwards, so the
  deferred userns creation cost one slow first call and nothing else.

### First mt-admin command output (abridged)

```text
=== Adding tenant: lunardepthtester ===
allocated port block 10010-10019 for tenant 'lunardepthtester'
created user: lunardepthtester
```

## Build

The build ran through the standard path, under tmux, confined to 12 CPUs and
with the same environment (no service-manager override):

```bash
sudo taskset -c 0-11 bash ic/scripts/lunarwing-mt-admin.sh \
  build-tenant lunardepthtester --with-lunardepth --with-toolchains
```

`BUILD_EXIT=1` after about an hour. Two stages passed and the third failed:

| Stage | Result | Evidence |
| --- | --- | --- |
| Daemon (`lunarwing`) | **PASS** | `Finished \`release\` profile [optimized] target(s) in 19m 09s`; `ic/target/release/lunarwing` present |
| XMPP bridge | **PASS** | `Finished \`release\` profile [optimized] target(s) in 16m 19s`; `last-build-stage` returns `bridge` |
| LunarDepth worker image (`--with-toolchains`) | **FAIL** | out-of-band export verification, below |

The LunarDepth phase itself got far enough to build the whole image:

- the pinned source was cloned and the pnpm 11.7.0/toolchain guards passed;
- the focused worker tests passed (`35 passed`, 10.59 s);
- the split-role smoke passed
  (`[08:34:58Z] … regenerated`, `[08:34:59Z] … bridge started`,
  `[08:35:01Z] LunarDepth task harness and runner are ready`,
  `LunarDepth split-role image smoke passed`);
- the image was committed as `lunarwing-worker-lunardepth:candidate-310336`
  (`ab7357d580cc`, first run) and `candidate-506835` (`284204ff0270`, second
  run).

Then its own out-of-band verifier rejected the exported filesystem:

```text
Traceback (most recent call last):
  File "/var/tmp/lw-lunardepth-oob.hsOJyr/lane/scripts/verify_exposure_out_of_band.py", line 100, in validate_archive
    raise ValueError(f"unexpected link in application closure: {name}")
ValueError: unexpected link in application closure: opt/lunardepth/node_modules/@agentclientprotocol/sdk/dist/connection.test.d.ts
ERROR: out-of-band LunarDepth export verification failed
error: out-of-band LunarDepth image verification failed for sha256:ab7357d580ccfb40e706b477892f1e21d2047b38873183bc05b6d4b559b5fcf3
BUILD_EXIT=1
```

The second run of the same phase alone (`build-lunardepth-worker
--with-toolchains`, 08:41:34Z→09:06:57Z, 25m23s) reproduced it exactly with a
new image ID:

```text
ValueError: unexpected link in application closure: opt/lunardepth/node_modules/@agentclientprotocol/sdk/dist/connection.test.d.ts
error: out-of-band LunarDepth image verification failed for sha256:284204ff0270175fc39b1cc11be16343c9a059eaea61a7e067b56b205c92dbe5
LANE_BUILD_EXIT=1
```

### Why it fails (F1, blocking)

The archive member is a **hardlink**, not a symlink, and the hardlink is
benign content deduplication inside the image:

- `lunardepth4lunarwing/scripts/verify_exposure_out_of_band.py:99-100` rejects
  any member for which `member.issym() or member.islnk()` is true, under
  `app/` and `opt/lunardepth`.
- From the failed build's cached intermediate image
  (`1604e9870dc6`, `/opt/lunardepth` present):

  ```text
  /opt/lunardepth/node_modules/@agentclientprotocol/sdk/dist/connection.test.d.ts
  type=regular file mode=644 links=275
  ```

  All 275 paths that share that inode are 11-byte `.d.ts` files
  (`find / -xdev -inum 8322670` lists `gaxios/build/esm/test/*.d.ts`,
  `gaxios/build/cjs/test/*.d.ts`, …). pnpm's content-addressed store links
  identical files to one inode; nothing here is a symlink or an escape.
- The host's container store is `overlay`, and `podman 6.2.0 export` writes
  hardlinked files as tar hardlink entries. A two-file probe on this host:

  ```text
  /a.txt links=2
  /b.txt links=2
  -rw-r--r-- 0/0               3 2026-11-16 08:40 a.txt
  hrw-r--r-- 0/0               0 2026-11-16 08:40 b.txt link to a.txt
  ```

  `podman export --help` offers no dereference option.
- CI takes its export from BuildKit (`type=tar` in
  `.gitlab/scripts/build-worker-image.sh:579`), not from `podman export`; I
  did not run that path, so whether it flattens these links is unverified.
  On this host and through mt-admin it does not.
- The verifier's own unit tests
  (`lunardepth4lunarwing/scripts/verify_exposure_out_of_band_test.py`) cover no
  link case at all.

The lane therefore cannot produce `lunarwing-worker-lunardepth:latest` on this
host through mt-admin, and everything downstream of the image (worker start,
readiness, jobs) is blocked by it. I did not weaken the verifier, patch the
lane, or bypass the gate with a hand-built image: that would change the
artifact under test and the security contract of the lane. The product fix
belongs in its own work item (see *Hand-off*).

## Runtime audits

`start-tenant` returned **0** with no override:

```bash
sudo taskset -c 0-11 bash ic/scripts/lunarwing-mt-admin.sh start-tenant lunardepthtester
```

The tenant's services run as systemd **user** units under the tenant account,
which is the host's own manager:

```text
lunarwing-lunardepthtester.service                 loaded active running
lunarwing-pg-lunardepthtester.service              loaded active running
lunarwing-vision-lunardepthtester.service          loaded active running
lunarwing-weechat-adapter-lunardepthtester.service loaded active running
lunarwing-weechat-lunardepthtester.service         loaded active running
xmpp-bridge-lunardepthtester.service               loaded active running
```

Results that could be taken:

| Check | Result |
| --- | --- |
| Gateway `127.0.0.1:10010/api/health` | `200 {"status":"healthy","channel":"gateway"}` |
| PostgreSQL | running (`lunarwing-pg-lunardepthtester`, port 10013) |
| LunarVision sidecar | healthy (`20015`/`20016`) |
| Worker `:52011/health`, `:52011/ready` | connection refused (no worker container) |
| `worker-listeners lunardepthtester` | exit 0, every lane `not present … skipped`, `worker listener audit: clean` |
| `worker-egress lunardepthtester` | exit 1, `status: not configured (no egress network allocated)` |
| `worker-smoke lunardepthtester lunardepth` | exit 1, `Error: no such container "lunarwing-lunardepth-task-lunardepthtester"` / `LunarDepth task container is not running` |

The worker could not be started because no `:latest` image exists; mt-admin
said so and skipped it:

```text
LunarDepth worker image not available; run 'build-lunardepth-worker' first (skipping)
```

while `start-tenant` still returned 0 (finding F2).

## Job exercise

**Not run.** The dispatch stage needs a running LunarDepth worker, and the
worker image could not be produced (F1). No job was created, no gate was
approved, and no workspace artifact exists; this is a blocker, not a pending
result. The task texts prepared for the three intended jobs (Python log
pipeline, C++17 weighted interval scheduling, parallel statistics with an
independent oracle) are in `/tmp/wi734-tasks/` for the next attempt, but they
were never dispatched.

## Findings and hand-off

### Findings

1. **F1 (blocking, product): the out-of-band export verifier rejects benign
   hardlinks, so the LunarDepth image cannot be built through mt-admin on this
   host.**
   - Where: `lunardepth4lunarwing/scripts/verify_exposure_out_of_band.py:99-100`
     rejects `member.islnk()`; `ic/scripts/mt-admin-lib/17-lunardepth.sh:339-390`
     feeds it a `podman export` tar.
   - What: pnpm 11.7.0 deduplicates identical files into one inode; on this
     host's `overlay` store `podman export` encodes them as tar hardlink
     entries (`links=275` on an 11-byte `.d.ts`; probe above).
   - Command and output: the second `build-lunardepth-worker --with-toolchains`
     above; `LANE_BUILD_EXIT=1`.
   - Why not fixed here: it is a security-gate change, and this item is a test.
     A safe fix would accept hardlinks whose `linkname` is another archive
     member beneath the checked roots (the checker already validates the
     target member's bytes and ownership) and keep rejecting symlinks, plus a
     unit test. That is a product change on a different work item.
2. **F2 (product, small): `start-tenant` returns 0 when a selected lane is
   skipped.** The run above returned 0 while printing `LunarDepth worker image
   not available … (skipping)`. A caller cannot tell a healthy start from a
   tenant whose only selected worker did not start. `start_tenant_lunardepth`
   returns 0 on the `_ensure_tenant_image` miss
   (`ic/scripts/mt-admin-lib/17-lunardepth.sh:404-406`).
3. **F3 (product, display): `worker-listeners` reports `clean` for a lane with
   no containers.** It printed `lunardepth: not present … skipped` and then
   `worker listener audit: clean`, exit 0. A vacuous pass is indistinguishable
   from a verified one; it should say "not present" or return a distinct code.
4. **F4 (product, display; WI#719 F5/F6 reproduced):**
   - `mt-admin status lunardepthtester` lists no `lunardepth_wss`/`lunardepth_health`
     ports although the registry holds 52010/52011; `add-tenant` printed the
     `lunardepth_wss` line but `status` prints neither.
   - `state/config.toml` carries `pebble` and `opencode` external-worker blocks
     even though only `lunardepth` is selected, and the daemon logs liveness
     probes for all three (as in WI#719 F6).
5. **F5 (product, hygiene): test declaration files are in the runtime
   closure.** `@agentclientprotocol/sdk/dist/connection.test.d.ts` and 274
   sibling `.d.ts` test declarations reach `/opt/lunardepth`; the residue gate
   rejects `_test.ts`, `_test.js` and `_test.py` but not `.test.d.ts`
   (`verify_exposure_out_of_band.py:107-110`). Harmless here, but the closure
   is not as clean as the gate claims.
6. **H1 (host, cosmetic):** `WARNING: could not pre-warm the rootless podman
   userns for lunardepthtester …` at `add-tenant`; the first Podman call
   created it (`docs/bugs/BUG-workload-scope-podman-userns.md`). Image
   distribution and the PostgreSQL quadlet then succeeded.

### Host changes this run made

Every command that changed the host, in order:

1. `add-tenant lunardepthtester …` (no override): created the account and
   subuid/subgid ranges, enabled linger, wrote `/etc/lunarwing/ports.json`, the
   tenant home and env/state files, systemd user units (enabled at
   `default.target.wants`), the PostgreSQL quadlet, distributed the sandbox
   image into the tenant store, synced the health library to
   `/usr/local/lib/lunarwing-health`, wrote `/usr/local/sbin/lunarwing-mt-health`,
   and confirmed the existing systemd health timer (no `health.env` edit).
2. `build-tenant lunardepthtester --with-lunardepth --with-toolchains`:
   release binaries and the XMPP bridge inside the tenant account; container
   build caches and one failed candidate image (deleted by the build).
3. `build-lunardepth-worker --with-toolchains`: a second failed candidate
   image (deleted by the build) and its build caches.
4. `start-tenant lunardepthtester`: enabled and started the tenant's systemd
   user units and the LunarVision sidecar; PostgreSQL was already running.
5. A two-file `podman run`/`export`/`rm` probe for F1 in the root store (no
   persistent container; the probe tar was removed).

No `LUNARWING_SERVICE_MANAGER` appears in any command or log of this run;
`/etc/lunarwing/health.env` still reads `systemd`.

### Final state

- `lunardepthtester` is **left in place and running** for inspection (gateway
  10010, vision 20015/20016, PostgreSQL 10013; no LunarDepth worker). I did
  not remove or reuse anything: the name was free when the run started, so no
  maintainer decision was needed.
- `lunarwing-worker-lunardepth:latest` does **not** exist in any store; the two
  failed candidates were removed by the build itself.
- The tenant clone is at `d872e9632` (`aj/kimi-wi734-20261116-0731`, target
  `f0d47a882` in its history).
- Logs: `/tmp/wi734-add.log`, `/tmp/wi734-build.log`,
  `/tmp/wi734-build-lane.log`, `/tmp/wi734-start.log`,
  `/tmp/wi734-runtime.log`. No credential is quoted in this report.

### Hand-off

The item is **not done**: the tenant exists and runs, but its only selected
worker is not built or running, and no job ran. The next attempt needs either
a product fix for F1 (preferred; a separate work item on the verifier's
hardlink handling) or a host whose `podman export` flattens hardlinks, such as
the Gentoo host that passed the same lane for WI#719. F2 and F3 should be
filed with the same caution as WI#730: they are product behavior, not test
hiccups.

## Acceptance

- [x] The host line is true (Arch Linux, systemd, kernel `6.18.54-2-lts`,
  Podman 6.2.0), and no command of the run forced a service manager the host
  does not run.
- [x] The report says in which state the tenant name was found (free: no
  account, no registry entry) and that nothing was removed or reused.
- [ ] The tenant has the LunarDepth worker **running** with `--with-toolchains`:
  the lane is enabled and `--with-toolchains` was passed, but the image build
  fails at its own out-of-band verifier (F1), so there is a build result and
  no successful runtime check.
- [ ] Jobs of medium to high complexity ran: blocked by F1; none dispatched.
- [x] Every hiccup is recorded with its command and output, classified as a
  finding about the product or the host.
- [x] No section is marked pending; the blocked sections say what stopped them
  and what evidence exists.
