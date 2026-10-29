# GOAL-117 SALVAGE NOTES — WI#511 smoke guard aimed at the wrong netns

Status: item #117 of `docs/ops/AGENT_GOALS_2.1.7.0.md`, re-attempt of the
**closed** MR !2055 (run-267 verdict **REQUEST CHANGES**).

## The defect

`opencode2.4lunarwing/scripts/runtime_smoke.py`'s listener check (WI#424
control 6) reads `/proc/net/tcp{,6}` wholesale. mt-admin builds every worker
with `podman build --network=host` (`ic/scripts/mt-admin-lib/05-tenant-build.sh`
— the host netns is the only one that can reach the network on those hosts), so
the candidate-build `RUN` of `runtime_smoke_entrypoint.sh` read the **HOST's**
socket table and flagged host sshd/NetBird listeners as worker exposure.

The check logic was always correct; only its **scope** was wrong. Rootless
BuildKit (CI) gives each RUN its own netns, which is why the candidate builds
never reproduced it.

## What !2055 did, and why it was rejected

MR !2055 (head `ed088c90c`, branch `aj/wi511-smoke-listener-attribution`)
tried **uid attribution**: map listening-socket inodes to owning uids by
scanning `/proc/<pid>/fd`, judge only sockets owned by the runtime uid (10003)
or of unknown provenance. Run-267 found:

- **RR-1 (High):** the inode was read from the **wrong column**. It used
  `fields[12]`, which is the retransmit-timeout value (`99` on every row); the
  real inode is `fields[9]` and the owning uid is `fields[7]`. So
  `owners.get(99)` never matched and the change was a **no-op on real
  kernels**. Its own fixture wrote the inode at index 12 too, so 53/53 passed
  while mirroring the misreading.
- **RR-2 (High):** even with the right column, `/proc/<pid>/fd` attribution
  **cannot see the host** — `podman build` RUN steps have a private PID
  namespace, so host processes (and their sockets) are absent, stay
  unattributed, and are judged anyway. The build would fail unchanged.
- **RR-3 (Medium):** "ignore sockets owned by other uids" **narrows the gate**
  — a listener a future entrypoint opens as another in-container uid would
  escape judgment.
- **RR-4 (Medium):** no test could catch RR-1; needs a real `/proc/net/tcp`
  row fixture.
- **RR-5 (Low):** "Closes WI#511" premature; the WI also asks for a regression
  test that fails when the guard inspects host netns and a sibling-call-site
  sweep.
- **RR-6 (Info):** CI never runs `runtime_smoke_test.py` (only the protected
  candidate build does).

## What was NOT salvageable from !2055

Nothing was cherry-picked. The uid-attribution design is unsound for this
problem (RR-2 is structural: private PID namespace), and fixing RR-1's column
index would still leave a no-op. Both `socket_owners()` implementations, the
`--smoke-uid` plumbing and the `_OwnersTransport` fixture were **dropped
entirely** — `runtime_smoke.py`'s check semantics are byte-identical to
pre-!2055, so there is no RR-3 narrowing to unwind either.

## What this pass does instead (reviewer shape (a))

Aim the guard at the right namespace, as the WI title says:

```
RUN --network=none bash /app/scripts/runtime_smoke_entrypoint.sh
```

`--network=none` gives the RUN step its **own private netns with loopback UP**,
under both builders, with **no capability requirement**. The original branch
recorded the following probes on its Podman 5.8.2/Gentoo reproduction host;
the BuildKit loopback row cites upstream evidence, not an entrypoint run:

| probe | result |
| --- | --- |
| `podman build --network=host` + plain `RUN` | netns `net:[4026531833]` = **host**, 101 `/proc/net/tcp` rows |
| `podman build --network=host` + `RUN --network=none` | **private** netns, `/proc/net/tcp` = header only, `lo` **UP** |
| `unshare -n` inside a plain RUN (rootless **and** rootful) | **EPERM** — no CAP_SYS_ADMIN; `--cap-add=SYS_ADMIN` required (build-invocation change, rejected) |
| `RUN --network=none` loopback bind + connect | OK |
| BuildKit v0.34.1 `RUN --network=none` lo state | `lo: <LOOPBACK,UP,LOWER_UP>` (upstream PR #1141) |
| Podman RUN step PID namespace | **private** (`HOST_PIDS=4`) — confirms RR-2; CI BuildKit instead shares buildkitd's PID namespace with `--oci-worker-no-process-sandbox`, not the host's |
| rootless build: host socket uid column | `65534` (overflow), never `10003` — confirms shape (b) alone would not attribute either |

Why shape (b) (kernel uid column, `fields[7]`) was not used on its own: it
*does* distinguish host sockets (rootful: uid 0; rootless: 65534) from the
worker's, but it still narrows the gate exactly as RR-3 objects, and it depends
on the uid mapping of whichever builder is in use. Private-netns scoping needs
no attribution at all and keeps the check **as strict as WI#424 wrote it**.

## Original parser-only namespace evidence (not entrypoint acceptance)

Ran the **real** `runtime_smoke.py` parser inside `podman build --network=host`
in both shapes (`/tmp/wi511-proof`):

```
BEFORE (plain RUN, host netns):  rows=43  offending=['<host-listeners-redacted>']  -> FAIL
AFTER  (RUN --network=none):     rows=0   offending=[]           -> PASS
```

`BEFORE` reproduces the reported incident signature (host relay/DNS and SSH
listeners), and `AFTER` shows the fix on the same host and build shape. Host
addresses and service-port inventory are redacted from this evidence.
The empty `AFTER` socket table proves namespace selection only: it does not
start the entrypoint, serve or bridge, and does not prove their offline startup.

Also green in the original branch verification: `runtime_smoke_test.py`
**50/50** (was 47/47; 3 pre-existing `CliWiringTests` failures on that
reproduction host were caused by `/tmp` being mounted
`noexec` — they pass with an exec-capable `TMPDIR`, identical before and
after); BuildKit-parser gate `worker exposure policy (BuildKit parser): ok`
plus `go test` in the CI-pinned `golang:1.26.10` image; Python syntax; bash
syntax and unchanged ShellCheck findings for the mt-admin comment.

## Sibling call-site sweep (RR-5)

Only **one** call site shared the host netns — the opencode2 candidate-build
smoke RUN. Verified clean:

- `mt-admin worker-smoke` → `worker_runtime_smoke`
  (`ic/scripts/mt-admin-lib/09-worker-config.sh:3292`) execs
  `--profile container` **inside** the running tenant container → the
  container's own netns. Correct already.
- `mt-admin worker-listeners` → `_worker_listeners_read` (same file, `:3133`)
  reads `/proc/<pid>/net/{tcp,tcp6,udp,udp6}` **host-side** for the container
  pid → the container's netns, by construction. Correct already.
- No other worker Dockerfile (`pebble4lunarwing`, `opencode4lunarwing`,
  `kimicode4lunarwing`, `gotify-wasm`) runs a listener guard in a RUN step.
- Repo-wide `/proc/net` readers outside `ic/vendor/` are only the two above.

## !2058 run-270 follow-up

- **RU-1 (Medium):** the blocking Go BuildKit-parser policy now pins the exact
  `RUN --network=none bash /app/scripts/runtime_smoke_entrypoint.sh` instruction.
  It must appear exactly once in the final assertion stage, after dependency
  modes and before the wrapper anchor. Go fixtures reject a missing flag,
  host/default network, a missing/commented/duplicate smoke, an extra unsafe
  smoke and wrong stage/order. The Python exposure-policy fixture includes
  the pin too and tests removal of the network flag through the real guard.
  This is not dependent on the checkout-only Python test running in an image.
- **RU-2 (Medium):** `build_opencode2_worker` now preflights Podman >= 4.5.0
  (its `go.mod` first vendors Buildah 1.30.0, which supports inline `RUN
  --network`; Podman 4.4.4 still vendors 1.29.0). Unreadable versions fail
  closed. Docker requires BuildKit/buildx; an explicit legacy-builder choice
  is refused and the build command explicitly enables BuildKit. Preflight
  refusals happen before pruning, building or promoting images. This does not
  change the existing-image lifecycle or the older-Podman Quadlet fallback.
- **RU-4 (Low, pre-merge blocker):** removed host addresses and host service
  inventory from both evidence records by amending the commit that introduced
  them, not by leaving the originals in an ancestor. The other three original
  MR commits are unchanged. All four commits on the rewritten local branch
  were checked for the disclosed inventory. The remote branch is not rewritten
  until a maintainer publishes this local history; no force-push was performed.
- **RU-5 (Info, accuracy):** kernel-row fixtures pin address/port/state parsing,
  not inode attribution (`parse_listeners` never reads uid/inode). PID namespace
  claims now distinguish Podman from CI's no-process-sandbox BuildKit setup;
  the shared build comment describes the affected host class. The stale
  deferred-smoke control in `WORKER-CONTAINERS.md` now describes the candidate
  and live suites and the enforced network scope.

### Review-fix verification (Arch/systemd host, tmux)

- Checksum-verified Go **1.26.10**: `gofmt`, `go test ./...` and the real
  Dockerfile parser command pass with `GOTOOLCHAIN=local` and
  `GOFLAGS=-mod=readonly`; no Go dependency files changed.
- Builder compatibility **27 PASS / 0 FAIL**, runtime-smoke unit tests
  **50/50**, worker-image contract **168 / 0**, and opencode2 mt-admin
  regressions **109 / 0**. The Docker matrix covers unset, `1` and `true`
  BuildKit selections, explicit legacy selection and missing buildx.
- **Ten mutations rejected** without modifying tracked sources: lowering or
  bypassing the Podman floor, allowing Docker legacy mode, skipping buildx,
  removing forced BuildKit, and five real-Dockerfile parser inputs with a
  dropped/host/default network flag, missing smoke or duplicate smoke.
- Full `.gitlab/tests` discovery: **487 tests, zero failures, six skips**.
  The exposure-policy guard, 8 CI YAML files and Python syntax in 143 tracked
  files pass (2 documented experimental copies excluded). Scoped mt-admin
  and final repository ShellCheck pass; final Bash syntax covers **272
  scripts and 3 hooks**, and `git diff --check` is clean. An overlapping lint
  attempt raced with temporary-fixture cleanup; the final repository lint
  ran after those tests finished and exited 0. No Rust code or dependencies
  changed, and no Cargo lane was needed.

## Not claimed (RU-3 acceptance remains pending)

- **No full `mt-admin build-tenant`** was run for a live tenant (needs a
  provisioned tenant and a multi-minute image build from source). RR-5's
  acceptance for WI#511/WI#515 is a `build-tenant` that passes the smoke step
  on an affected host; the reproduction above is the same build shape, the same
  host and the real parser, but it is **not** a full tenant build.
- **WI#511 and WI#515 stay open** pending operator/maintainer acceptance.
- No real started-entrypoint smoke under `--network=none` is claimed by the
  original parser-only probe. The review-fix host has Podman 6.1.4 / Buildah
  1.45.2, but neither its rootless nor rootful store had a cached opencode2
  worker image. A full rootless candidate build was attempted from this tree
  with `--network=host`, the checked-in Dockerfile and wrapper digest, and a
  private temporary tag. It failed before the smoke at builder step 9/20:
  `bun install --frozen-lockfile` (Bun 1.4.4) could not apply hunk #10 of the
  `@silvia-odwyer/photon-node@0.3.4` patch (`photon_rs.js`, expected line 4509).
  This reproduces the !2057/WI#514 dependency-install blocker; no lockfile,
  dependency pin, MCP setting or guard was changed to work around it. No
  existing tenant was rebuilt or modified; build-cache layers were created,
  but no worker `:latest` image was promoted.
- The blocking parser gate now enforces the namespace instruction on this MR.
  Real offline entrypoint acceptance still needs an affected-host
  `build-tenant` and the first protected candidate after !2057/WI#514 unblocks
  that build. Keep the configured remote MCP servers, readiness checks and
  listener assertions intact for that acceptance; do not disable them just
  to obtain a passing smoke.
- The reproducibility gate (candidate vs clean-rebuild digest) is unaffected in
  principle — `--network=none` removes network access, it adds no per-build
  state — but it was **not** re-proven here, since that needs two full builds.
