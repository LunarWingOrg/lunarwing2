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
under both builders, with **no capability requirement**. Verified empirically
on this host (podman 5.8.2, Gentoo):

| probe | result |
| --- | --- |
| `podman build --network=host` + plain `RUN` | netns `net:[4026531833]` = **host**, 101 `/proc/net/tcp` rows |
| `podman build --network=host` + `RUN --network=none` | **private** netns, `/proc/net/tcp` = header only, `lo` **UP** |
| `unshare -n` inside a plain RUN (rootless **and** rootful) | **EPERM** — no CAP_SYS_ADMIN; `--cap-add=SYS_ADMIN` required (build-invocation change, rejected) |
| `RUN --network=none` loopback bind + connect | OK |
| BuildKit v0.34.1 `RUN --network=none` lo state | `lo: <LOOPBACK,UP,LOWER_UP>` (upstream PR #1141) |
| RUN step PID namespace | **private** (`HOST_PIDS=4`) — confirms RR-2 |
| rootless build: host socket uid column | `65534` (overflow), never `10003` — confirms shape (b) alone would not attribute either |

Why shape (b) (kernel uid column, `fields[7]`) was not used on its own: it
*does* distinguish host sockets (rootful: uid 0; rootless: 65534) from the
worker's, but it still narrows the gate exactly as RR-3 objects, and it depends
on the uid mapping of whichever builder is in use. Private-netns scoping needs
no attribution at all and keeps the check **as strict as WI#424 wrote it**.

## Verification evidence (this host, real build shape)

Ran the **real** `runtime_smoke.py` parser inside `podman build --network=host`
in both shapes (`/tmp/wi511-proof`):

```
BEFORE (plain RUN, host netns):  rows=43  offending=['<host-listeners-redacted>']  -> FAIL
AFTER  (RUN --network=none):     rows=0   offending=[]           -> PASS
```

`BEFORE` reproduces the reported incident signature (host relay/DNS and SSH
listeners), and `AFTER` shows the fix on the same host and build shape. Host
addresses and service-port inventory are redacted from this evidence.

Also green: `runtime_smoke_test.py` **50/50** (was 47/47; 3 pre-existing
`CliWiringTests` failures on this dev box are caused by `/tmp` being mounted
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

## Not claimed

- **No full `mt-admin build-tenant`** was run for a live tenant (needs a
  provisioned tenant and a multi-minute image build from source). RR-5's
  acceptance for WI#511/WI#515 is a `build-tenant` that passes the smoke step
  on an affected host; the reproduction above is the same build shape, the same
  host and the real parser, but it is **not** a full tenant build.
- **WI#511 and WI#515 stay open** pending operator/maintainer acceptance.
- CI will not execute `runtime_smoke_test.py` on this MR (RR-6 stands: only the
  protected candidate build runs it, and that lane is red for the unrelated
  Bun 1.4.4 regression — WI#514).
- The reproducibility gate (candidate vs clean-rebuild digest) is unaffected in
  principle — `--network=none` removes network access, it adds no per-build
  state — but it was **not** re-proven here, since that needs two full builds.
