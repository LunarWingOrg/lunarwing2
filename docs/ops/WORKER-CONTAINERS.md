# Worker Container Images

Several container images provide sandboxed and persistent job execution for LunarWing.

Nanocode is retired and has no active source, build, or runtime image definition.
See [`NANOCODE-REMOVAL.md`](NANOCODE-REMOVAL.md) for upgrade behavior.

## Brotli security refresh (WI#514, 2026-10-29)

Master pipeline 6719's four worker scans each found CVE-2026-111497 in
`libbrotli1`. The four original Dockerfiles and `CI_DEBIAN_SNAPSHOT` now use
`20261105T082236Z`: bookworm selects `1.0.9-2+deb12u1`, trixie
`1.1.0-2+deb13u1`. The validator binds every worker's snapshot to CI and
rejects older snapshots. The three existing scan suppressions were re-checked
against these indexes and Debian's tracker; Brotli is not suppressed.

The bundled decoders need separate pins because an OS-package scan does not
cover them. Kimi's builder/runtime and `CI_NODE_IMAGE` use Node `22.23.4`
(index `sha256:4b150b48b4116e2a22f2c39cc27025fbf4f153e2078fd3b14d159fdb5c7279da`),
whose [security release](https://nodejs.org/en/blog/release/v22.23.4) backports
the Brotli fix; its `process.versions.brotli` still reports `1.1.0`. Both
OpenCode builders/runtimes and `CI_BUN_IMAGE` use Bun `1.4.4`
(index `sha256:93d5032fe06d163427822dc1598f420363759bb18155c9cc90eb1be05a64a19e`).
The image's source revision `38afee6c` pins
[Brotli v1.2.1](https://github.com/oven-sh/bun/blob/38afee6c84fcf48c0edb2466aa4b27d287a59c30/scripts/build/deps/brotli.ts),
replacing Bun 1.4.2's vulnerable v1.1.0. Both OCI indexes include amd64 and arm64.

At the OpenCode v1.18.25 / v2.1.2 pins, Bun 1.4.4 rejected the upstream
photon-node patch's hunk-10 context whitespace, and each builder ran
`python3 /build/fix_photon_patch_context.py /build/opencode` immediately after
checkout and before `bun install --frozen-lockfile`, backporting upstream
`61845b0`'s three-space repair. **That repair has been retired since the
v1.19.5 / v2.2.5 pins (WI#531), including the current v2.2.7 pin (WI#609):**
both trees carry the repair in content (a
content-equivalent backport — neither tag contains the commit itself, so do
not test it with `merge-base --is-ancestor`), so the fixer's exact-match
guard reports the repair as redundant. The fixer files, their
Dockerfile `COPY`/`RUN` and self-test invocations, and the two `input_files`
entries per lane are gone together; Bun was not downgraded and the drift
guard was not weakened. See bump checklist item 7 below before moving either
pin.

Rebuild worker images and recreate tenant containers to deploy these fixes.
Protected candidate reproducibility and all four scan jobs must pass before
promotion; local runtime/package checks do not replace those gates. WI#514
stays open until that protected pipeline acceptance is recorded.

## Snapshot apt time bounds (WI#559, 2026-11-05)

All four worker Dockerfiles bound `apt-get update` with `timeout -k 30 240`
and `apt-get install` with `timeout -k 30 600`; the `WITH_TOOLCHAINS` install
gets 1800 s because `build-essential`/`clang` are far larger than the slim
package sets. The `worker-kimi-ts` provisioning helper uses the same 240/600
limits with `timeout -k 30`. A stall therefore exits 124 — or is KILLed 30 s
after it ignores TERM — and enters the existing three-attempt snapshot retry
ladder instead of running until the runner kills the job. Build-wrapper trace
matching accepts 137 or 124 only when the quoted process command itself names
`snapshot.debian.org` (BuildKit escapes the command's inner double quotes as
`\"`, and the matcher handles them); an earlier snapshot step header does not
make an unrelated compile kill transient. Package pins and snapshot contents
are unchanged, but the Dockerfile command edits re-key their layers.
Protected candidate-versus-clean-rebuild digests, all four scans, and the
opencode2 exposure verifier remain the image acceptance gates.

## APT transport, retry, and client hardening (WI#561, 2026-11-05)

[CVE-2026-109518](https://ubuntu.com/security/CVE-2026-109518) requires an
on-path attacker, redirect handling, and non-zero `Acquire::Retries`; APT's
retry hard-link reuse can otherwise replace an already verified package. The
original 24 worker `deb` source lines, LunarDepth's additional six, and the
three `worker-kimi-ts` lines now use
`https://snapshot.debian.org` with normal certificate verification. There is
no `Verify-Peer=false` or plain-HTTP fallback. Both snapshot hosts were
checked over HTTPS on 2026-11-05: each returned its pinned content after an
HTTPS 302 redirect, with HSTS present.

Every worker APT call sets `Acquire::https::Timeout=60`. In each builder and
runtime, the old base client's update and apt-only bootstrap install use
`Acquire::Retries=0`. A `dpkg --compare-versions` floor (`2.6.2` on Bookworm,
`3.0.4` on Trixie) must pass before the fixed client installs every remaining
package with `Acquire::Retries=3`; toolchain updates and installs likewise use
the fixed client with three retries. Every update uses `--error-on=any`, so a
failed security index cannot pass silently. The WI#559 process timeouts are
unchanged. Protected candidate builds and the CI helper retain their
three-attempt outer ladders and recognize HTTP and HTTPS traces. Direct
mt-admin builds have no outer ladder: the small bootstrap still fails fast,
but transient fetches for all remaining packages get the fixed client's three
bounded attempts instead of making the whole build fragile to one response.

All five worker Dockerfiles and `CI_DEBIAN_SNAPSHOT` use `20261105T082236Z`. Its
Bookworm security archive serves `apt_2.6.2_amd64.deb`; its Trixie security
`InRelease` is byte-identical to the index whose package list was checked for
APT `3.0.4`. Every builder/runtime installs `apt` alone before its other
packages, as does the CI provisioning helper, replacing the vulnerable client
inherited from the base images instead of merely making a fixed version
available. The snapshot floor and package-install shape have regression
coverage; later fixed point releases satisfy the floor without another
Dockerfile edit.

The slim Node and Debian bases can have no system CA bundle at all. Each
builder and runtime therefore copies `/etc/ssl/certs/ca-certificates.crt`
directly from the already-reviewed, digest-pinned Python `3.12-slim-bookworm`
image before its first APT call; no extra Dockerfile stage changes the builder
manifest layout. `worker-kimi-ts` creates `/etc/ssl/certs` and then writes the
bundle from the pinned Node runtime's built-in `tls.rootCertificates`. Both
sources are local trusted inputs, not insecure network fallbacks. After CA
package installation, `update-ca-certificates` regenerates the bundle and
hashed links as one consistent store. The version floors and strict update
errors close the APT behavior where failed index downloads can otherwise
return success. The validator ties every worker snapshot ARG to CI and applies
its robust HTTPS source parser to Kimi; `test_worker_reproducibility` enforces
HTTPS, bootstrap-only retry suppression, version floors, and strict updates on
all five lanes.

The current inventory covers five worker Dockerfiles plus
`.gitlab/ci/validate.yml`; mt-admin renders those same lane Dockerfiles and
has no independent mirror recipe. Independent second-host SHA-256 verification
(WI#561 lane C) is not implemented. Rebuild every worker image and recreate
its containers to deploy this fix. Protected candidate/rebuild digests, all
four scans, and the opencode2 exposure verifier remain mandatory acceptance;
local package-index probes and hermetic tests do not replace them.

## Hub-handshake credential boundary (WI#373)

Only the bridge container receives `AGENT_AUTH_TOKEN`, the tenant-scoped
`WORKER_AUTH_TOKEN` used for the daemon→worker WebSocket handshake. mt-admin
mints and persists that scoped token on the first lifecycle pass (WI#210);
the gateway bearer remains a loudly warned last resort when the worker env
file is unwritable. A leaked token would let a job impersonate the daemon
toward every worker WebSocket port of that tenant.

All five lanes use two non-root containers under different identities
(WI#454). The bridge writes its token to a private `0400` file under its own
home, removes the environment variable, and exports only the file pointer to
the bridge process. The task container has neither that home nor the bridge
PID namespace mounted. mt-admin filters both `AGENT_AUTH_TOKEN` and
`AGENT_AUTH_TOKEN_FILE` from task environment files; the task launcher and
runner refuse a non-empty copy of either before accepting work. Executor
scrubbing remains defence in depth. The bridge's initial PID 1 environment
can still carry its input token, but it belongs to the separate bridge
container and is inaccessible from the task container. See the split
contract below for IPC and shared-workspace limits.

Each lane enforces the strip at its task-process boundary:

- **Kimi** — the executor deletes `AGENT_AUTH_TOKEN` from the assembled
  per-task env after all merges (bridge env spread, task-supplied
  `context.environment`, proxy `extraEnv`), so neither inheritance nor
  task-supplied env can deliver it. Pinned by the executor self-test. The
  one-shot `cli` mode execs under `env -u AGENT_AUTH_TOKEN` (shape pinned by
  the entrypoint config self-test).
- **OpenCode / OpenCode 2** — the entrypoint launches `opencode serve` via
  `env -u AGENT_AUTH_TOKEN`: the server process runs all task code (every
  bash tool child inherits its env), while the separately exec'd bridge keeps
  its own copy. The one-shot `cli` and `acp` modes exec the task process the
  same way. Pinned by the worker entrypoint harness, which asserts both
  directions (serve sees it absent, bridge sees it present) plus every
  one-shot mode. The serve process cannot receive per-task environment updates
  from the bridge, so both executors reject any non-empty `context.environment`
  before creating an SDK client or session (**WI#474**). The task returns an
  explicit error telling the caller to resubmit without credentials or use
  kimi/pebble; grants are never silently accepted as a no-op. The hub additionally
  refuses non-empty `create_job.credentials` for workers configured with
  `supports_credentials = false`, before secret lookup/decryption, job
  registration or dispatch (**!1943 ON-1**). mt-admin writes this flag for both
  OpenCode lanes and converges existing managed entries on its next config
  reconciliation (for example `patch-env <tenant>` / `upgrade-tenant <tenant>`).
  Restart the tenant daemon through mt-admin after changing config to load the
  flag. **Unmanaged/renamed OpenCode entries must set it explicitly** in their
  `[[sandbox.external_workers]]` table; omission defaults to `true` for backwards
  compatibility with credential-capable workers such as kimi/pebble. All
  load-balanced endpoints of an entry must support the configured capability.
  Until the flag is loaded, worker-side rejection still receives the plaintext
  grant and is only defence in depth, not a hub-side secrecy boundary. Downgrading
  to a daemon without this capability check likewise loses the hub-side guard.
  Grants are also **never** merged into the bridge's `process.env` (run-209 NU-1), so no
  agent-chosen grant name (`options.credential_grants[].env_var` at
  `create_job`) can alter shared bridge lazy reads (task identity, serve auth,
  retry/heartbeat/abort knobs, tool-proxy timeout) or inject the hub credential.
  The v1 serve-log scrub covers boot-environment values; rejected task maps
  cannot reach that enrichment path. Executor pins assert zero SDK calls,
  unchanged bridge credentials, no grant values in diagnostics, and closure of
  the v1 follow-up queue on the early return.
- **Pebble** — `spawn_pebble` rebuilds the child env via `env_clear()` +
  `strip_bridge_reserved_env` (everything else preserved: PATH, locale,
  `PEBBLE_*` provider variables) and drops task-supplied re-injections of the
  same four reserved keys (`AGENT_AUTH_TOKEN`, `AGENT_AUTH_TOKEN_FILE` and the
  two tool-proxy keys) before the child env is assembled.
  Pinned by a self-exec regression test that carries the token in the parent
  env and probes the real spawned environment through a stub. The one-shot
  `cli` mode also removes both hub credential names (harness-pinned).

The OpenCode serve password (`OPENCODE_SERVER_PASSWORD`) is present in the
serve process env because the server enforces it itself. Task code already
runs inside that task-container authority domain. LLM provider keys
(`KIMI_API_KEY`, `TENSORZERO_API_KEY`, …) are deliberately task-visible.
Older images without the split exposed their bridge credentials through
same-uid `/proc` reads; deploying a source update without rebuilding and
recreating the worker pair does not repair that old image.

### Non-root bridge and task pairs (WI#454, supersedes WI#400)

Each lane uses the same image for two containers. The image defaults to the
task identity; mt-admin selects the bridge identity explicitly. Both roles
run with `--cap-drop ALL` and `--security-opt no-new-privileges`, without
capability additions or runtime uid switching.

| Lane | Image task uid:gid | Bridge uid:gid | Task home |
| --- | --- | --- | --- |
| opencode | 10001:10001 | 10101:10001 | `/home/opencode` |
| kimi | 10002:10002 | 10102:10002 | `/home/kimi` |
| opencode2 | 10003:10003 | 10103:10003 | `/home/opencode2` |
| pebble | 10004:10004 | 10104:10004 | `/home/pebbletask` |

Container names are `lunarwing-<lane>-<tenant>` for the bridge and
`lunarwing-<lane>-task-<tenant>` for the runner. The explicit
`LUNARWING_CONTAINER_ROLE=split-bridge|split-task` selects the launcher;
wrong, equal or root identities fail closed. Split images refuse an
unassigned websocket role rather than silently starting both halves under
the image's task uid. Host development and one-shot CLI/ACP paths retain
legacy support where no image bridge-identity pin is present.

The bridge runs from root-owned `/app` (Pebble's launcher/binary lives in
`/usr/local/bin`), with a bridge-owned home such as `/home/kimi-bridge`.
Its default token directory is `$HOME/.local/state/lunarwing` (`0700`), with
`agent_auth_token` at `0400`. That directory never enters the shared volume.
The bridge PATH uses image-owned executable locations. Kimi boot files have
explicit `0555` modes; its config, hooks and compiled scripts have `0555`
directories and `0444` files independent of build-context umask. OpenCode2's
parser and out-of-band tar/config checks cover its root-owned boot tree.

**Mounts and environment.** Both roles mount the lane workspace at
`/workspace` and the lane's private named volume at `/run/lunarwing-shared`.
The task role receives the SSH-agent mount (except Kimi) and OpenCode Study
Notes mounts. The bridge receives neither. Generated task runtime and
override files strip both hub credential names; launchers and runners reject
any that remain. Kimi and Pebble executors also scrub inherited and
supplied hub credential keys after env merges. OpenCode bridges reject
non-empty `context.environment` before delegation, since serve cannot apply
per-task env grants. OpenCode clients reject non-empty `extraEnv` rather than
silently dropping it. LLM provider keys remain task-visible because the
agent needs them. The bridge still receives the lane env, including the LLM
key, and runtime/override env files are not independently included in the
container config hash; after changing them, force-recreate the pair through
mt-admin rather than assuming an existing running container refreshed them.

**IPC ownership.** The volume name is
`lunarwing-<lane>-<tenant>-ipc-v2`. A fresh volume copies these image modes:

- `/run/lunarwing-shared`: root:task-group `3770`, setgid plus sticky;
- `runner/`: task:task-group `2711`, containing `task-runner.sock` (`0660`)
  and the heartbeat state (`0640`);
- `tool-proxy/`: bridge:task-group `2711`, containing per-task proxy sockets
  that the task group can connect to.

Sticky ownership prevents task code from replacing the bridge-owned proxy
directory. Relays refuse symlinked, foreign-owned or writable proxy parents;
a missing/refused shared proxy fails a delegated task instead of running
without tools. The runner owns its socket and verifies stale entries before
unlinking them. Clients check type and expected uid/gid; Pebble also compares
the socket device/inode around connect. These checks detect drift and races,
but do not authenticate a same-uid task pretending to be the runner.

**Dispatch, cron and cleanup.** One Unix connection owns one run. It sends
`run`, receives `accepted`, then bounded progress and one terminal result;
cancel travels on that connection. Lines are capped at
`WS_MAX_PAYLOAD_BYTES + 65536`, including a bounded unterminated remainder.
Oversized output is truncated within the serialized envelope cap. Foreign
task ids are ignored. OpenCode validates `progress.done` as a boolean;
v1 follow-ups use the runner's queue without accumulating a second bridge
queue, and `prompt_refused` leaves the run alive. Run-map cleanup checks
connection identity before deleting a newer dispatch of the same job id.
Cron path validation on the OpenCode bridge creates nothing: the task runner
performs the descriptor-relative, no-symlink mkdir walk as the task uid and
passes the prepared absolute path to the executor. Kimi and Pebble also
prepare task workspaces on the task side.

Kimi/OpenCode handshakes are bounded by
`LUNARWING_TASK_RUNNER_TIMEOUT_MS` (default 15 s); their accepted-run deadline
is the task timeout plus 60 s. OpenCode cancellation drains for its
`OPENCODE_SESSION_ABORT_TIMEOUT_MS` (default 5 s, maximum 30 s) plus 5 s.
Pebble bounds connect, writes and acceptance to 2 s, also polling cancellation
while writing the run request; its accepted-run deadline is the task timeout
plus 60 s, followed by at most 12 s cancellation drain. Pebble reports
`timed_out` for executor/bridge deadlines and `cancelled` for hub cancel.
Its executor starts a fresh process group, kills remaining group members at
task end, bounds stderr drain, and the image uses Tini to reap orphans.
Across separate PID namespaces the bridge cannot kill task processes;
runner/container shutdown is responsible for them.

**Health, lifecycle and audits.** Task health connects to the socket and
requires a JSON pong within 2 s and a 1024-byte newline-terminated response;
`test -S` or a heartbeat file alone is insufficient. Bridge HTTP health
remains distinct; Python readiness also checks runner heartbeat freshness.
Systemd user Quadlets start the task before the bridge using `Requires=` and
`After=` (no `BindsTo=` stop-job trap). OpenRC uses paired health/supervisor
units. Stop/kill and tenant export quiescence include both roles. A missing
task unit is harmless, but failure to stop a loaded unit propagates.
`worker-listeners`, `worker-egress`, host egress health and `worker-smoke`
inspect both roles. Both are attached to the tenant egress network and
verified after start; the item-114 namespace keeper remains in force.

`remove-tenant` uses the same per-lane stop order with and without `--purge`.
It then removes every supported bridge/task container and IPC volume, every
OpenRC unit/runlevel link, and the systemd Quadlets. Systemd stops task services
and reloads the user manager before container deletion, so `Restart=on-failure`
cannot resurrect a task into teardown. Egress teardown removes the namespace
keeper and refuses to flush while the runtime reports any attachment to the
tenant network. Stop, inspection, container/volume removal, attachment-query,
and network-removal failures abort the command; they are not warnings. A refused
teardown leaves the filter and rendered state in place for repair and retry.

**Rootful workspace redirects (QP-4/RH-2).** Rootless keep-id maps the task
uid to its tenant account. Rootful runtimes use fixed lane uids, so checking
symlinks only during host preparation is insufficient: the runtime can
re-resolve the bind source after a tenant replaces it. mt-admin records the
verified source's device/inode in `LUNARWING_WORKSPACE_ID` and sets
`LUNARWING_ROOTFUL_WORKSPACE=1` after env overrides on both containers.
Each launcher compares the mounted workspace before task setup or bridge
credential staging, including ordinary runtime auto-restarts. A different
inode, even with the same owner, refuses startup. The `workspace-inode-v1`
launch-contract suffix recreates rootful containers lacking this pin. If an
operator deliberately replaces a workspace directory, stop the pair via
mt-admin, remove the stopped pair from that tenant's runtime store, then
start it through mt-admin to record the new identity. Preserve the IPC-v2
volume and workspace contents; a plain restart keeps the old pin and must
fail. The guard does not restore
per-tenant host uids after a container escape: rootful lanes still share
fixed numeric task ids. Prefer rootless keep-id for that additional boundary.

**Authority limit (QP-9).** Every concurrent and successive task in one task
container shares its uid, loopback serve API, workspace, runner socket and
grants. A task can signal/trace its runner, replace a task-owned socket or
leave a process that deliberately escapes the process group. Process-group
cleanup and inode checks reduce accidental leftovers and races; they do not
isolate jobs from each other. Use separate worker pairs for different users
or grant domains. Splitting protects the bridge credential, not one task
from another task inside the same pair.

**Upgrade and rollback.** Rebuild the lane images, then use mt-admin
`start-tenant`/`upgrade-tenant` to reconcile paired units and containers.
Image-id and split launch contracts recreate old containers; the new
`-ipc-v2` name avoids adopting old writable IPC directories. Never reuse the
old volume by renaming it. Preserve the workspace; old IPC volumes may be
removed only after both old roles are stopped. A rollback must restore
matching image, mt-admin, unit and IPC contracts together, and reinstates the
older credential model. Protected candidate builds, out-of-band OpenCode2
verification, whole-image reproducibility and live tenant audits remain
required before publishing/deploying rebuilt images; local source and runner
tests do not establish those deployment results.

**Local Compose examples.** Each lane's `docker-compose.yml` starts its
non-root task service before its bridge using the same IPC-v2 modes and
bounded task health probe. Only the bridge and the explicit smoke client
receive the hub credential; task env overrides both hub credential names
to empty even when an optional env file supplied them. Bridge ports publish
on loopback. Compose does not install mt-admin's tenant egress firewall,
init units or rootful workspace identity pin; use mt-admin for managed
multi-tenant operation. Before running a bind-workspace example, create
`workspace`, `logs` and `results` owned by the image's mapped task uid/gid
and grant the workspace task-group access (`2770`). On rootless Podman,
perform ownership preparation inside `podman unshare`; on Docker, use the
image's numeric uid/gid. Do not pass the bridge credential to a one-shot CLI
container. Recreate both services after image/env changes; do not reuse old
IPC volumes.

**Host-side workspace prep (WI#453, the WI#446 closeout):** all five lane
workspace directories live inside the
tenant-controlled `~/lunarwing` tree, so the historical root `mkdir -p` +
unbounded `chown -R` is gone. `prepare_worker_workspace`
(`ic/scripts/mt-admin-lib/09-worker-config.sh`) runs the
`darkirc_path_components_safe` walk FIRST (a symlinked ancestor refuses
before anything is created behind the link), creates the directory AS the
tenant, then enforces the leaf contract and, on rootful runtimes — which
need the bind mount handed to the image uid — chowns it, bounded with
`--no-dereference` so the recursive walk never follows a symlink. MI-3 (run-186 review) + NR-1/NR-2 (run-206 review): the rootful branch
`cd`s into the directory to pin `.`, then verifies where it landed (`pwd -P`
must equal `$workspace_dir` and `stat -c %u .` must still be one of the two
accepted owners) before any chown or chmod runs, closing the swap window
between the pre-`cd` path checks and `cd`; it then chowns only the top node
on first creation (not `-R`, since the directory is empty), and when
content exists, walks with `find -mindepth 1` skipping regular files with
a link count above 1 (a tenant-planted hard link to an outside file keeps
its owner). Every Quadlet renderer and imperative workspace caller makes a
refusal fatal.

The leaf-uid gate accepts exactly two owners: the tenant root's uid
(provisioned tenant-owned by `create_tenant_user`, and what every rootless
leaf stays), and — on rootful hosts, where the chown legitimately hands
the leaf to the container uid and never restores tenant ownership — this
lane's own container uid, so a second call is idempotent. Any other owner
is refused before the chown, which is what stops a tenant from renaming
another lane's workspace (or any foreign-owned directory) into this slot:
run-203 review of !1880 showed the !1880 fix had dropped that refusal
entirely. Rootless keep-id maps the tenant identity, so no chown runs
there and a foreign owner is always a spoof signal.

Mode: rootless keeps `chmod 2770` through `tenant_chmod` (root has no
business touching a mode under a tenant-writable parent, WI#390). On
rootful the tenant no longer owns the leaf after the chown, so
`tenant_chmod` would fail `EPERM` on the first call — root sets `2770`
itself, inside the verified `cd`, where `.` is the directory it already
entered, so no symlink can be followed.

Pins: the six workspace-symlink pins, the post-`cd` symlink/owner swap pins
(NR-1), the hard-link-skip pin, and the rootful real-chown block in
`test-mt-admin-kimi.sh` — tenant `nobody`, real `chown`, real uid switch —
which asserts calls 1 and 2 both succeed with the leaf at the container uid
and mode 2770, that a foreign-owned leaf renamed into the slot is refused
with its ownership untouched, and that a tenant symlink or foreign-dir swap
between the pre-`cd` checks and `cd` is refused on enter without touching
the decoy's owner or mode. All mutation-verified (reverting to the old root
`mkdir -p` + `chown -R` shape fails the symlink pins; restoring
`tenant_chmod` on the rootful path, removing the two-owner gate, or
dropping the post-`cd` `pwd -P`/`stat -c %u .` check fails the NR-1 and
real-chown pins). That block needs euid 0 plus a mapped uid 10001 and a
working uid switch, so it self-skips with a named reason elsewhere (the dev
VM runs it under `podman unshare`).

### Baked-config rewrite contract (WI#529)

The worker images make their task-side inputs immutable: `/app/config` (and
the hooks/scripts trees) are root-owned `0555` directories with `0444` files,
and the image build fails if any write bit or non-root owner survives
(`opencode4lunarwing/Dockerfile`, `kimicode4lunarwing/Dockerfile`,
`opencode2.4lunarwing/Dockerfile`). That hardening is the image layer's
inventory guarantee, not a promise about the consumer's workspace copy.
Because `cp` inherits the source mode, every in-container consumer that
rewrites a config got a `0444` file and died `EACCES` on the first write —
WI#523 was that shape for the OpenCode v1 model/baseURL block.

The contract, binding:

- **Baked originals never change.** Consumers write a task-owned workspace
  copy. `opencode4lunarwing/entrypoint-task-setup.sh` owns the two helpers
  that carry the knowledge — `materialize_config_copy` (`cp` then
  `chmod u+w` on the copy) and `ensure_config_rewritable` (an alias or a
  missing target is materialized from the baked source; a non-writable
  regular file is widened from **its own** content, so an earlier
  injector's rewrite is never reverted) — and every consumer uses
  them: the model/baseURL rewrite, the Paseo MCP injector and the Study
  Notes MCP injector. `opencode2.4lunarwing/entrypoint.sh` performs the
  same sequence for `opencode2.json`; the Kimi lane writes
  `$KIMI_CODE_HOME/config.toml` from a temp file and never touches the
  baked source.
- **The no-override path is a symlink to the baked original**, and the
  WI#330 placeholder sentinel reads through it. The task setup asserts the
  alias resolves to the baked config and that it is readable by the task
  uid, so a future mode, ownership or layout change fails loudly instead of
  silently grepping something else. `opencode2.4lunarwing/entrypoint.sh`
  carries the same alias binding for its `$OPENCODE_CONFIG` sentinel.
- **One test binds the three copies of the knowledge.**
  `.gitlab/tests/test_worker_config_rewrite_contract.py` statically pins
  the Dockerfile gate, the helper use at every in-place write, and the Kimi
  lane's write shape, then runs the real task setup as the task uid against
  a `0444` fixture (overrides, Paseo-only, Study-Notes-only, sentinel and
  linked no-override paths). Both OpenCode v1 and Kimi image builds also
  execute the real task setup as their task uid and assert the materialized
  copy is writable while the baked file stays `0444`.

Adding an in-place writer? Route it through the helpers and add a live
fixture; do not widen the baked file's mode to make a consumer work.

### Pebble task sandbox: procps is unsupported (WI#480)

Upstream pebble enables its namespace sandbox by default. Its Bash tool runs
commands through `unshare --user --map-root-user --mount --ipc --pid --uts
--fork sh -lc …` (`pebble/crates/runtime/src/sandbox.rs`) without remounting
`/proc`. The inherited procfs therefore belongs to the container's outer PID
namespace, while `getpid()` in the command returns a PID from the new inner
namespace. procps 4.0.2's `ps` self-select uses that inner PID and cannot
reliably find its own entry in the outer `/proc`; it exits with `fatal library
error, lookup self` when no such entry exists.

This is not a deliberate `hidepid` control, and the WI#400 `setpriv` drop alone
does not cause it. On the image's pinned bookworm runtime with procps
`2:4.0.2-3`, the production keep-id/capability/no-new-privileges launch plus
the production `setpriv` drop ran `ps aux` successfully; adding pebble's exact
`unshare` argv reproduced the fatal error. Adding `--mount-proc` was rejected
with `Operation not permitted` by that same production launch. Do not disable
the PID namespace or relax the container security options merely to make `ps`
available.

For lifecycle checks of a child started by the same task shell, use the shell's
namespace-local PID with `kill` and `wait`, not `/proc/$!`:

```sh
server-command &
pid=$!
kill -0 "$pid" 2>/dev/null  # liveness check
kill -TERM "$pid"
wait "$pid"                 # shutdown status (143 after SIGTERM)
```

If `set -e` is active, put `wait` in an `if` condition when a non-zero exit is
expected. For metadata, `/proc/self/status`, `/proc/self/cmdline` and
`/proc/self/fd` remain usable from the process they describe because the magic
`self` link needs no numeric PID translation. `/proc/self/status` records the
outer PID in `Pid:` and the outer-to-inner mapping in `NSpid:`. To inspect
another process through `/proc/<pid>/status` or `/proc/<pid>/cmdline`, use its
outer-namespace PID (for example, the first value in its `NSpid:` line), never
an inner-namespace `$!`. Host-side diagnostics that already have the outer PID
are unaffected.

## WebSocket message limits

The daemon accepts external-worker messages and individual WebSocket frames
up to **16 MiB**, including envelope overhead. Fragmentation does not bypass
the total-message limit. This applies to every worker, even with an empty
`tool_allowlist`; exceeding the bound fails the job with a WebSocket size
error and drops the connection. Proxied `tool_call` messages additionally
have an independent **256 KiB** limit before dispatch.

Kimi's `WS_MAX_PAYLOAD_BYTES` defaults to 1,048,576 bytes and can be raised to
16,777,216 bytes; its serializer caps the whole envelope. The daemon limit
accommodates that maximum. OpenCode and Pebble bridges do not currently cap
their outgoing envelopes, so outputs above 16 MiB still exceed the transport
contract. Results/progress between 1 and 16 MiB are accepted (MR !1540 DX-1/DX-3).
Note the interplay with the WI#324 tool proxy (MR !1549 EF-9): raising the
hub's tool-output safety cap (`SAFETY_MAX_OUTPUT_LENGTH`, default 100,000
bytes) past the worker bridge's inbound `maxPayload` (Kimi default 1 MiB)
means a large `tool_result` trips the worker's own WS layer — ws 8.21.1
closes the connection with a 1009 (message too big) error, and the bridge
then aborts the task that was running on that socket (MR !1561 EV-5). Keep
the worker's `WS_MAX_PAYLOAD_BYTES` at or above the hub cap when tuning it.

## LLM request latency, heartbeats and timeouts (WI#477)

Large coding turns are not expected to finish at ordinary chat latency. A small
direct request can answer in under a minute, while a large request with a long
context and many tool results can spend several minutes queued before its first
token; the silverzoom evidence recorded **508.8 s** to first token and about
8.5 s of subsequent streaming. Treat a multi-minute request as possible, not as
proof of a dead worker. The heartbeat and deadline below make that wait visible
without turning it into an ETA.

Both workers default to a **120 s** progress heartbeat. While an LLM request is
outstanding, the persisted `task_progress` / `job_events` text carries its
one-based logical request index and elapsed seconds, for example
`[worker] Still waiting for Pebble response; request 2 elapsed 240s; task remains active.`
The existing `[worker] … ; task remains active.` daemon filtering contract is
unchanged. Kimi also reports the following tool-wait phase with the same index
and elapsed fields. Its request index is inferred from assistant/tool stream
boundaries: it starts at 1 and advances after every tool call from the previous
response has a result. Pebble's pinned build emits exact start/finish markers
around each runtime API call through the reviewed
`pebble4lunarwing/patches/wi477-request-events.patch`; those markers are enabled
only by the bridge, so ordinary Pebble CLI JSON output is unchanged. Native
retries stay on the same logical request index.

- **Kimi:** `KIMI_CODE_LLM_HEADERS_TIMEOUT_MS` defaults to **900000 ms**
  (15 minutes, clamped to 1000..86400000). This is the CLI's native per-attempt
  response-headers deadline, covering connection and time to response headers,
  not the entire token stream. `turn.step.retrying` becomes a visible worker
  progress event with the request index, next/max attempt, delay and bounded
  error class; final failure still produces the task error result. The bridge
  applies the operator value after task-supplied env, so a task cannot extend
  it. mt-admin forwards the key from `lunarwing.env` into the protected
  `kimi-runtime.env`. Upstream Kimi does not apply this override through a
  SOCKS proxy: it warns and keeps its own default headers timeout; use a direct
  or HTTP-proxy connection when relying on this deadline.
  `KIMI_SILENCE_TIMEOUT_MS` remains a separate whole-CLI no-output watchdog.
- **Pebble:** `PEBBLE_API_TIMEOUT_SECS` defaults to **600 s** (clamped to
  1..86400). This is the native reqwest per-HTTP-attempt total request timeout.
  Retryable expiry produces the upstream `[llm-retry]` marker plus a visible
  worker event carrying the current request index; exhausted or non-retryable
  failures reach the task error result. The bridge overrides task-supplied
  values with the operator value. `PEBBLE_PROGRESS_HEARTBEAT_MS=0` disables
  heartbeats, not the API deadline. The reqwest setting does not replace
  provider-specific deadlines such as the Grok ACP response timeout.

These are **per-attempt** limits, not a promise for a whole turn: retries can
multiply the wait (Pebble defaults to two retries after the initial attempt),
and each task's existing `timeout_ms` remains the outer bound. Choose values
that accommodate the expected large-request envelope, and inspect the visible
retry/failure event rather than waiting silently for the outer task deadline.

## Worker-side tool proxy bridge (WI#324)

The kimi worker image ships the WI#324 worker-side bridge: a per-task unix
socket relay in `lunarwing_bridge.ts` plus a thin stdio MCP server
(`scripts/lunarwing_mcp_server.ts`) registered in `$KIMI_CODE_HOME/mcp.json`
by the entrypoint (write-if-absent; set `"enabled": false` there to opt out).
The kimi CLI spawns the shim per task session; the bridge injects
`LUNARWING_TASK_ID` / `LUNARWING_TOOL_PROXY_SOCKET` per task, and the shim
translates MCP `tools/list` / `tools/call` into `tool_list` / `tool_call`
frames on the existing hub WebSocket. Tools appear to the agent as
`mcp__lunarwing__<name>`; the hub stays deny-by-default until the worker's
`tool_allowlist` (or `cron.tool_allowlist` for tick jobs — MR !1506 CQ-10)
grants phase-1 names. On multi-tenant hosts the grant is rendered by
`lunarwing-mt-admin.sh configure-worker-tools <tenant> <worker> <tools|none>`
(it validates against the daemon's phase-1 set, refuses `mode = "cron"`
entries, and needs `restart-tenant` to load; editing `config.toml` by hand
works on single-node installs). `LUNARWING_TOOL_PROXY_TIMEOUT_MS` bounds one
proxied call (default 300000, capped 900000).

The opencode worker image ships the same relay in its
`scripts/lunarwing_bridge.ts`, but the in-task half is an opencode plugin
(`scripts/lunarwing_tool_plugin.ts`, registered by absolute path in
`config/opencode.json`) rather than an MCP shim: `opencode serve` owns MCP
servers per project Instance with the serve process's frozen environment,
so a per-task socket env cannot reach an MCP child. The plugin statically
registers the phase-1 tools as `lunarwing_<name>`; each call resolves the
session's task id from its title (`lunarwing-opencode-<task_id>`, with
subagent child sessions resolved through the root session via `parentID`)
through the plugin host's authenticated client (first-title-wins cached) and
relays over the task's socket. The static tools are visible even without a
grant — ungranted calls return the hub's denial as a tool error. The
first-title-wins cache is not an isolation boundary: any process in the
container can rename a session before its first call, create a session with
another task's title or `parentID` over the loopback serve API, or connect
to another task's socket directly — all of these stay inside the
container's single hub-enforced grant and change only which job a call is
attributed to. The opencode2 worker image ships the same relay plus a v2
serve-process plugin (`scripts/lunarwing_tool_plugin/index.ts`, a directory
plugin registered by absolute path in `config/opencode2.json`'s `plugins`
array — v2 drops configured plugin file paths): the v2 promise plugin API
registers the phase-1 tools via `ctx.tool.transform` as direct
(`codemode: false`) tools with plain JSON Schema inputs, each `execute()`
resolves the calling session's task id from its title
(`lunarwing-opencode2-<task_id>`) through the plugin context's in-process
session API, and relays over the task's socket with the same
first-title-wins caching and the same "not an isolation boundary" limits.

The pebble worker ships the same relay in Rust, inside the
`pebble4lunarwing` bridge binary (`src/tool_proxy.rs`): one relay per hub
connection, one per-task unix socket per accepted task, the same NDJSON IPC
shapes, identifier bounds and locally pre-checked 256 KiB `tool_call` frame
cap, and the same EF-2 never-fail lifecycle / EF-9+EV-4
fail-fast-on-hub-close semantics. The in-task half is a thin stdio MCP
server like the kimi lane's, also inside the same binary
(`pebble4lunarwing mcp-shim`, `src/mcp_shim.rs`): pebble discovers
`mcpServers.lunarwing` from the baked
`$PEBBLE_CONFIG_HOME/settings.json` (`/etc/lunarwing/pebble`; root-owned
`0555`/`0444`, and since the WI#400 split that DOES protect it from task
code, which now runs as the task uid) and spawns a
fresh shim for tool discovery and for each tool call, passing its process env through to the MCP child, so the
executor's per-task `LUNARWING_TASK_ID` / `LUNARWING_TOOL_PROXY_SOCKET`
injection (applied after task-supplied env, and stripped from both
inherited and task-supplied env alongside `AGENT_AUTH_TOKEN`) reaches the
shim. Tools appear as `mcp__lunarwing__<name>`; without the proxy env the
shim advertises zero tools. Note pebble merges project-scope
`.pebble/settings.json` from the task workspace, so workspace content can
shadow or disable the `lunarwing` entry — the hub stays authoritative, same
as the kimi loader (EF-10). The flip side of the read-only config home
(run-166 LD-16): pebble's own state writes into
`$PEBBLE_CONFIG_HOME` now fail under the split, so its model state
(`state.json`, `model-catalogs.json`, cached credentials) is not persisted
across runs; some call sites ignore those errors and some surface them.
Seeding a task-owned `~/.pebble` with the baked `settings.json` is the
known follow-up shape if that ever becomes load-bearing. See
[`docs/proposals/EXTERNAL-WORKER-TOOL-PROXY.md`](../proposals/EXTERNAL-WORKER-TOOL-PROXY.md)
for the wire contract.

**One worker container is one authority domain** (MR !1549 EF-1): every
concurrent and successive job in a container runs as the same uid, and any
process in it can connect to another task's relay socket and act with that
job's grant — including a detached leftover process waiting for later tasks.
Per-user isolation therefore requires per-user workers. Because a tick job
could borrow a concurrent interactive job's grant this way, a `mode = "cron"`
worker's worker-level `tool_allowlist` is stripped at manager load (logged
as an error): a cron worker container never holds more than its
`cron.tool_allowlist`, and ordinary dispatches to it get no proxied tools.
The strip is applied per worker entry, but the authority domain is the
container: never point an entry that has a `tool_allowlist` at the same
endpoint as a `mode = "cron"` entry — give a cron mission its own
container. Two cron entries pointing at the same container likewise
share one authority domain (one mission's tick can borrow the other's
`cron.tool_allowlist` through another tick's relay socket, concurrent
or later — a leftover process can wait for it), so the manager now
detects that pairing at load (WI#396): cron entries are grouped by
normalized endpoint key, and when a shared listener's entries carry
*differing* `cron.tool_allowlist` grants the manager strips every cron
grant on that listener with an `error!` (fail closed); identical grants
are the same authority on both sides and stay, with a `warn!`. The
strip is a single pass: an entry stripped for one listener can still,
on another listener it shares with identical-grant entries, borrow a
grant equal to its own configured list — no mission gains authority
beyond its configuration. (The manager also strips the worker-level list of a non-cron
entry whose endpoint shares a cron entry's container. The comparison uses
a normalized endpoint key — loopback spellings folded into one class,
host lowercased, scheme-default port applied, path and query ignored —
so one host:port that path-routes several containers strips all of them
(fails closed). The key names a host:port listener, not the container
itself: a container that is also published on a second host port, or
reached behind a proxy on another port, is not caught; a real host
literally named `loopback` folds into the loopback class and can only
over-strip, which fails closed. Those, and exotic aliases such as
`/etc/hosts` names, remain the operator's responsibility — the rule
stands on its own.) A borrowed
call is recorded in the `job_events` — and counted against the
100-frame budget — of the job whose relay socket was used, not of the
job that issued it.

## Generic shell sandbox network policy (WI#154/155/537)

On Linux the generic shell sandbox (`ic/src/sandbox/`) defaults to
`SANDBOX_NETWORK=proxy-enforced`, in code and in new mt-admin-rendered
multi-tenant envs. It keeps `--network none` and exposes only a per-execution
Unix relay to the public-only allowlist proxy — local Linux runtime and a
Python-3-equipped image are required (the bundled `docker/sandbox.Dockerfile`
image includes the full Python development suite). Other platforms default to
`none` because this relay is unsupported. `none` disables egress, `tunnel`
selects the item-3 overlay sidecar, and `proxy` remains the legacy advisory
mode. mt-admin preserves every operator-set value on re-render.

Rootless podman uses keep-id; rootless docker maps container root onto the
host user; rootful docker maps numeric UIDs and refuses writable workspaces
under a root daemon (never container root). The `--pids-limit` is pinned to
256; the runtime's default seccomp filter is the supported contract. The
cleanup path is the `ContainerCleanupGuard` detached kill/rm plus the
API-independent bounded CLI sweep. See
[`v2.1.2.0_ITEM26_WI179_SANDBOX_HARDENING.md`](history/v2.1.2.0_ITEM26_WI179_SANDBOX_HARDENING.md).

**`/tenant-logs` (WI#287).** The daemon writes a secret-scrubbed copy of its
own log to `<base>/logs/sandbox/daemon.log` (MT: `<tenant root>/state/logs/sandbox/`)
and sandboxed runs see it read-only at `/tenant-logs`. mt-admin renders
`SANDBOX_TENANT_LOGS=info` (INFO and above, whatever the runtime level) and
preserves an operator-set value on re-render; `upgrade-tenant` does not
re-render the env, so existing tenants keep the daemon default, which is also
`info`. `debug` follows the runtime level up to DEBUG (the tool-call lines);
`off` writes no copy and mounts nothing. An invalid value fails every
`lunarwing` command that loads the config. The copy is daemon-owned (`0700`
directory, `0600` file), so it is mounted only where the runtime maps the
sandbox user onto the tenant's uid: docker, podman 5+ (keep-id pinned to
1000), or podman 4.3+ when the tenant uid is 1000. Other podman 4.x tenants
get no mount. The raw `<tenant root>/logs` directory stays protected: it is
neither a valid `workdir` nor visible through a tenant-root mount.

## Worker exposure policy (WI#357)

**LunarWing owns every public exposure path** through `ic/src/tunnel/`
(providers `none` (default) / `netbird` / `custom`; `TUNNEL_URL` or
`TUNNEL_PROVIDER`). **External workers must never self-expose**: no tunnels,
relays, pairing links, or public listeners of their own, and no publishing
session content to third-party share services. A worker that needs a public
path routes it through the daemon's tunnel. The only legitimate instruction
channel into a worker is the daemon's job dispatch → the in-container bridge
→ the worker's agent API on loopback.

This policy originally held the opencode2 lane at **v2.0.24**: v2.0.26
bundles OpenTunnel remote access (`packages/cli/src/services/remote-tunnel.ts`,
`@opentunnel/client` → hosted relay `opentunnel.xyz`, no self-host option,
anonymous device identity in the agent-writable `$XDG_DATA_HOME/opentunnel`),
plus `opencode service set remote true` and `opencode pair` surfaces. !1490
(v2.0.26 bump) was closed. **WI#427 (2026-10-17) records the maintainer's
completed analysis and approval to cross that boundary at v2.0.32**, through
the existing compile-out, wrapper, config and image assertions. See the
v2.0.32 bump entry below for the source review and verification status; the
lane has since advanced to **v2.2.7** (2026-11-12, WI#609) with every gate
re-verified against a fresh clone — see the v2.2.7 bump entry.

**Gate record for !1490 (run-79 EI-2):** the in-image layers below (ruleset,
wrapper, config pins, compile-out) are **naive-use guards, not a boundary**
— all of them miss whitespace/quote-obfuscated command text, the real
binary stays directly executable by the agent uid, and with egress open a
compromised agent can fetch an upstream opencode (tunnel included) or run
`@opentunnel/client` under bun against the loopback server, whatever the
pin. **Only control 1 (the egress allowlist) holds against a compromised
agent**. The maintainer-approved WI#427 bump deliberately crosses on
compile-out plus config alone; it does not turn the in-image guards into
an egress boundary. The mt-admin egress implementation is in-tree across the
worker lanes; the cold rootless first-start follow-up is WI#504, with
fresh-tenant acceptance and operator rollout still tracked separately.

Enforced controls (opencode2 lane unless noted):

1. **Egress allowlist (WI#424, 2.1.7.0; mt-admin implementation):** per-tenant
   rootless/host firewall chains limit workers to the configured LLM endpoint
   and operator allowlist. This remains the only control that also covers
   future phone-home features and `webfetch`/shell exfiltration, and the only
   one that holds against a compromised agent (see the gate record above).

   The allowlist is derived, never guessed: `worker_egress_target_records`
   reads `LLM_BASE_URL`, the three worker `*_BASE_URL` overrides and
   `PEBBLE_BASE_URL` from `lunarwing.env`, plus the pebble lane's own
   `PEBBLE_BASE_URL`/`NANOGPT_BASE_URL` from `pebble.env` (WI#506). A target
   the host cannot resolve is silently omitted from the ruleset, so the
   pebble lane refuses to start when its model backend is undeclared, malformed
   or unresolvable (`worker_egress_require_pebble_backend`) and names the exact
   remediation instead of letting every dispatched job fail with an opaque
   connect error. This is a status-4 lane-local refusal (WI#552), so other
   selected workers still start; see “Shared startup isolation” below.
   `worker-egress <tenant>` prints the derived targets under `allowlist targets:`;
   see `docs/ops/PEBBLE-WORKER.md` for the pebble backend pin.

   **Rootless Podman first-start keeper (WI#504):** when no worker container is
   running, mt-admin creates `lw-egress-keeper-<tenant>` on that tenant's
   `lw-egress-<tenant>` network before applying the filter. Where the tenant's
   local Podman store holds a `localhost/podman-pause:<tag>` image, creation is
   pinned to that exact reference via `--infra-image`, so creation and
   validation agree by construction; on hosts with no local pause image
   (Podman 6.x's image-less native pause, or a 5.x host that has not prebuilt
   one) no pin is passed and the native-pause shape check remains the gate.
   The reference is never invented — a pinned `--infra-image` always names an
   image the store already holds, and the `LUNARWING_WORKER_EGRESS_INFRA_IMAGE`
   override is refused unless it is a `localhost/podman-pause:<tag>` reference
   present locally (WI#516). The derived reference prefers the pause tag built
   by the running Podman client and falls back to the newest local
   `localhost/podman-pause:<tag>` when no tag matches (e.g. after a Podman
   upgrade leaves the previous version's image behind). The root-owned guard
   verifies the keeper's ownership label, single infra-container shape, local
   `localhost/podman-pause:<tag>` image and exclusive network attachment, then
   loads the rules and reads both reject chains in one
   `podman unshare --rootless-netns` session. `--verify` never creates a keeper
   or loads rules; the imperative post-start and the OpenRC/Quadlet paths use
   the same guard. Any infra image that is not a local pause reference with an
   explicit non-empty tag — including the malformed tag-stripped spelling
   `localhost/podman-pause:` — is refused and its keeper pod is
   removed. Keep Podman's `infra_image` at its local default; the host must
   have `catatonit` installed so Podman can build/use its local pause image.
   This prerequisite applies on OpenRC/Gentoo as well as systemd hosts.
   `stop-tenant` releases the keeper only after worker quiescence; cleanup
   failure is a warning, not a reason to report a healthy stop as failed.
2. **No self-exposure commands (naive-use guard).** The bridge session
   ruleset (`opencode2.4lunarwing/scripts/opencode_task_executor.ts`,
   `sessionPermissions()`) denies `shell` on `*opencode service *`,
   `*opencode pair *`, `*opencode serve *` — after the allow-all rule,
   because upstream evaluates with findLast. The lane config
   (`config/opencode2.json` `permissions`) carries the same denies for
   cli/acp sessions the bridge did not create. And
   `scripts/opencode-exposure-guard.sh` is installed as
   `/usr/local/bin/opencode` (real binary at `/usr/local/lib/opencode2/opencode`),
   scanning **every argv token**: `service`/`pair` subcommands, the
   `--service` flag, and every `--hostname` occurrence (loopback only).
   Server-backed commands such as `run` and `session list` can spawn the real
   binary directly via `Service.ensure`, bypassing the wrapper. At each boot
   the entrypoint removes whatever sits at
   `${OPENCODE_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/opencode}/service.json`
   (upstream's resolution order, run-90 FB-6/FB-7) and writes
   `{"disabled": true}` there, so those commands default to private,
   command-scoped servers instead of a shared service. The removal also
   defeats an agent-planted FIFO/directory/symlink at the reset path, which
   would otherwise hang the boot, crash it, or redirect the write. This reset
   discards prior service settings; the bridge's explicit foreground
   `serve` still uses its loopback hostname and per-boot password. These layers
   are naive-use guards: the agent can rewrite its live service config or
   invoke the real binary directly.
3. **Session sharing pinned off; auto-update silenced.** `"share": "disabled"` in both
   `opencode2.4lunarwing/config/opencode2.json` and
   `opencode4lunarwing/config/opencode.json` (v1's `ConfigV1.Info` still
   declares `share` at v1.19.5), plus `"update": "disable"` in the v2 lane config so an upstream
   default flip cannot re-enable publication. **The `update` pin does not
   reach the updater** (run-158 KG-3 / !1699 JE-3): the updater's
   `readPolicy()` reads only the global config dir, never `OPENCODE_CONFIG`.
   `OPENCODE_DISABLE_AUTOUPDATE=1` in the image env is the switch the updater
   actually honours — it silences the background check — and the wrapper
   refuses the manual `upgrade`/`update` command (KG-1/JE-2), so neither the
   background nor the naive-use manual self-update path replaces the pinned
   binary with an upstream build. The config tree and boot files
   (`entrypoint.sh`, `health_server.py`, `agent_comm_protocol.json`) are
   root-owned and read-only in the image, including deployments that restart
   the same container filesystem. The entrypoint re-derives the default
   runtime config from the baked source at every boot. An override copy gets
   owner-write permission before the config writers run; the baked source
   stays read-only. Live runtime/project configs and operator-selected
   `OPENCODE_CONFIG` remain untrusted and mutable. WI#529: that copy
   permission is owned by `entrypoint-task-setup.sh`'s
   `materialize_config_copy`/`ensure_config_rewritable` helpers, which every
   config writer calls — see "Baked-config rewrite contract" above.
4. **CI guards.** `.gitlab/scripts/check-worker-exposure-policy.sh` (job
   `worker-exposure-policy-guard`) checks source/config pins, the every-token
   wrapper, compile-out tests, non-root boot configuration, and bridge ruleset
   ordering without git. Dockerfile wiring is owned by the required
   `worker-buildkit-parser-policy-guard` job (WI#425): the bash grammar is
   retired, and `.gitlab/go/dockerfile-parse` applies the policy to BuildKit
   v0.34.1's own instruction tree (the module declares Go 1.26.8). The parser
   requires the compile-out COPY and RUN before the native build in
   `AS builder`, the wrapper/binary scan, root-owned read-only
   `scripts/`+`config/` and boot files, and the wrapper anchor ARG immediately
   above the final `--image-filesystem` assertion in the same stage. The final
   stage's last USER must be `10003:10003`; after the assertion only `EXPOSE`,
   `HEALTHCHECK`, `USER`, `ENTRYPOINT`, `CMD`, `LABEL` and `STOPSIGNAL` are
   allowed. Parser directives and heredocs are refused, and BuildKit's
   structured ENV parser rejects expanded, quoted, legacy, or protected keys
   while literal keys may retain quoted/expanded values. The
   FQ-3/GD-1/GJ-1/GJ-2/GW-2/GX-1/GZ-1/HD-1 divergence shapes have direct
   policy fixtures alongside the broader Python fixture suite. The CI job uses
   a digest-pinned Go 1.26.10 image with `GOTOOLCHAIN=local`,
   `GOFLAGS=-mod=readonly`, and checksum-verified modules; it runs `gofmt`,
   the Go policy tests, and the real Dockerfile check. `ci-config` installs
   the same pinned Go through `.gitlab/scripts/fetch-go-toolchain.py`, so the
   Python parser fixtures execute instead of skipping on its Python-only
   image. OpenCode 2 promotion hard-needs both this parser job and the
   out-of-band verifier. Out-of-band image verification remains the separate
   control-5 backstop below, not a parser substitute.
5. **Build-time assertion.** The opencode2 Dockerfile runs
   `scripts/exposure_policy_assert.py --binary …`: the image fails to build
   if the config lacks the pins, the entrypoint touches service mode or
   `--service`, `OPENCODE_BIN` bypasses the wrapper, the wrapper regressed
   to a first-token dispatch, the binary contains OpenTunnel client
   markers. A final `--image-filesystem` assertion checks actual ownership
   and modes of the boot files and `scripts/`+`config/` after all image steps;
   the image fails if they are not root-owned read-only or `/app` permits
   non-root replacement of its children. Run-103 GD-2: the v2
   `--image-filesystem` path additionally verifies the *installed wrapper*:
   `/usr/local/bin/opencode` must be a regular non-symlink, root-owned file
   with no group/other write bit and all three execute bits (WI#425 GX-3),
   byte-identical to `scripts/opencode-exposure-guard.sh`; `OPENCODE_BIN`
   may not retarget it, and `shutil.which("opencode")` must resolve to it
   (no earlier `PATH` dir shadows it). The real binary at
   `/usr/local/lib/opencode2/opencode` is re-scanned for tunnel markers and
   must itself be root-owned and non-group/other-writable; every parent of
   both paths is checked through, but never above, the trusted root (GX-2).
   These checks detect accidental drift after the wrapper-install scan,
   provided the checker and its environment remain intact — pinned by unit
   tests with injected paths, since the image build itself is a
   protected-pipeline gate. Run-106 GJ-3 hardens the
   same check: `LUNARWING_REAL_OPENCODE` may not retarget the wrapper at
   build time (only the default binary path passes, and any other value is
   also marker-scanned), a non-executable wrapper is refused with its own
   reason instead of the misleading "shadows" text, and the installed
   digest must match the `LUNARWING_WRAPPER_SHA256` build-arg anchor — the
   byte-identity source ships inside the image, so a root `RUN` could
   rewrite both files identically; the anchor is computed from the checkout
   by `build-worker-image.sh` and mt-admin's `build_opencode2_worker`, and
   a missing or mismatched anchor fails the build fail-closed. The anchor is
   **not unforgeable**: an adversarial Dockerfile edit can still defeat the
   check by replacing its environment or the checker itself (run-109 GW-3).
   The CI guard's ENV/SHELL refusals are an additional drift check.
   **WI#425 out-of-band verification (was the WI#357 follow-up, run-109
   GW-3):** the opencode2 candidate exports its final filesystem through a
   second BuildKit `--output type=tar`, preserving uid/gid/mode headers that
   a local directory export rewrites. The required
   `worker-opencode2-exposure-verify` job safely materializes only regular
   files, checks those tar headers with the checkout's
   `exposure_policy_assert.py --image-root`, compares checkout-owned `/app`
   files byte for byte, and refuses unexpected source-tree entries. BuildKit
   omits the `/` header, so only that root is a structural trust boundary;
   every emitted entry uses its tar-header metadata. Its
   shared `image_config_policy.py` allowlist covers the complete `Env`
   (including PATH order and loader variables), `Entrypoint`, `Cmd`, `User`,
   `WorkingDir`, `Healthcheck`, `ExposedPorts`, and `StopSignal`; `OnBuild`,
   `Volumes`, and `Shell` must be empty. Promotion hard-needs the
   non-optional job and the BuildKit parser guard. The Dockerfile strips
   setuid/setgid and write bits throughout `/app/node_modules` after every
   dependency `COPY` and before the wrapper anchor; the parser requires that
   ordering so `cp -a` cannot carry privileged source-directory modes into
   the published image. The final in-build `--image-filesystem` assertion
   recursively requires every dependency entry to be a root-owned regular
   file or directory with no setuid/setgid or group/other write bits, so a
   later `RUN` that restores those modes fails at the source (run-236 PL-2).
   The out-of-band assertion reads the same dependency metadata directly from
   the tar headers without extracting the dependency tree.
   In mt-admin, `build_opencode2_worker` uses a per-build candidate tag and
   `--iidfile`, then `create --pull=never`, `export`, and full `image inspect`
   feed the same tar/config verifier by immutable image ID. Docker's
   `Config.Healthcheck` and Podman's top-level `Healthcheck` normalize to the
   same policy; neither runtime can silently drop that field. The verifier
   and checkout-owned reference files enter a root-owned snapshot only after
   every source parent resolves beneath the canonical checkout; final links
   and special files are refused. The snapshot Python runs as `nobody` via
   `setpriv --clear-groups --no-new-privs`, `python3 -I`, and an empty
   environment. The exported tar and inspect JSON stay under `/var/tmp` in a
   `0700` `nobody`-owned input directory with `0600` files, and extraction
   uses a separate `0700` work directory. Cleanup is trapped, and only the
   verified ID is tagged `:latest`.
   The mt-admin trust boundary is deliberately narrow: only the wrapper has a
   build-time digest anchor; the other reference and verifier bytes are a
   verify-time snapshot of the live checkout. A hostile tenant can change
   both its checkout and the image that mt-admin builds as root from it, so
   this is a drift/naive-use check, not a tenant-to-host security boundary.
   Note the `PATH` resolution is a *build-time* property:
   at runtime the agent-owned `~/.cargo/bin` and `~/go/bin` (five PATH
   entries — the four toolchain dirs plus `/usr/local/sbin` — precede
   `/usr/local/bin`), so a runtime `PATH` `opencode` need not hit the
   wrapper; the entrypoint is unaffected (absolute `OPENCODE_BIN`) and the
   wrapper remains a naive-use guard.
6. **Runtime smoke (WI#424 control 6, WI#511 namespace scope):**
   `opencode2.4lunarwing/scripts/runtime_smoke_entrypoint.sh` starts the real
   entrypoint, serve and bridge during the candidate build, waits for their
   loopback readiness probes, and runs `runtime_smoke.py --profile container`
   as the image runtime uid. It checks service configuration, tunnel identity
   directories, all visible listener rows, sharing and the wrapper refusal.
   The Dockerfile must use exactly
   `RUN --network=none bash /app/scripts/runtime_smoke_entrypoint.sh`: only
   loopback is needed, and this keeps host listeners out of the check even
   when mt-admin builds with `--network=host`. The blocking BuildKit-parser
   policy requires exactly one such RUN in the final assertion stage, after
   dependency-mode normalization and before the wrapper anchor. Missing,
   host/default-network, duplicate and misplaced smoke steps fail that gate.
   `mt-admin worker-smoke <tenant>` runs the same suite inside live workers;
   `--profile remote` drives it from the host. Control 9 remains the separate
   host-side listener audit. These are naive-use checks, not a substitute for
   the egress boundary. A passing parser-only namespace probe is not acceptance
   of the real entrypoint smoke or of a full affected-host `build-tenant`; the
   acceptance run is recorded below.
   **Affected-host acceptance (WI#511, 2026-11-01):**
   `mt-admin build-opencode2-worker --no-cache` on a Gentoo 2.18 / OpenRC
   0.63.3 host (Podman 5.8.2, above the 4.5 floor) built the
   candidate through `podman build --network=host` and step 38/48
   (`RUN --network=none bash /app/scripts/runtime_smoke_entrypoint.sh`)
   printed `runtime-smoke: 5/5 checks passed` — 3 listeners, only health/WS
   8443 and 19090 non-loopback — before the out-of-band verify and the
   `:latest` tag. The build's own step-34 self-tests include
   `runtime_smoke_test.py`. On the same host and build shape a plain `RUN`
   read the host netns (105 TCP + 3 TCP6 rows) while `RUN --network=none`
   read a private netns (0 rows) with loopback still bindable and
   connectable, which is the premise this step relies on.
7. **Bump checklist (REQUIRED for every worker version bump).** The bump
   entry in this file must record the upstream features that add network
   exposure or outbound publication — tunnels, pairing, sharing, remote MCP,
   auto-update — with the decision for each, not only the bridge call
   surface. For opencode2 bumps past v2.0.24 the compile-out guard (8) plus
   these guards cover the *shipped binary*; against a compromised agent
   only control 1 makes the crossing safe (see the gate record above).
   **Photon-node repair retirement:** when a new OpenCode pin carries the
   `61845b0` whitespace repair in content (a backport; the tag need not
   contain the commit), `fix_photon_patch_context.py` deliberately fails with
   "already carries the fixed whitespace". For each affected lane, remove
   both `scripts/fix_photon_patch_context{,_test}.py` files, the Dockerfile's
   fixer COPY/RUN before `bun install`, its self-test invocation, and the two
   corresponding `input_files` entries in `.gitlab/scripts/build-worker-image.sh`
   in the same bump commit. Do not bypass the guard or downgrade Bun. Run the
   manifest/CI checks, then require protected candidate == clean-rebuild digests,
   both OpenCode scans and `worker-opencode2-exposure-verify` before claiming
   acceptance; getting past install can still expose WI#510-class binary/layer
   nondeterminism. Items 28 and 95 stay unchecked until that evidence exists.
   **WI#369 typecheck compiler:** the gate runs whatever `@typescript/native`
   the upstream lock pins. Check its version on every bump: v2.2.5 moved it
   from `npm:typescript@7.0.2` to a `7.1.0-beta`, and v2.2.6 and v2.2.7 retain
   that version (binary `bin/tsc`); a beta compiler in a build gate is acceptable only while it is
   upstream's own pinned devDependency, resolved through the committed lock,
   and still exposes `tsc` at `bin/tsc`.
   Record the version and the binary path in the bump entry.
8. **Compile the tunnel out.** `scripts/compile_out_remote_tunnel.py` runs
   in the builder stage: the historical v2.0.24 path verifies there is no
   tunnel surface; at the current v2.2.7 pin it replaces `remote-tunnel.ts`
   with a fail-closed stub (re-verified against the real v2.2.7 tree:
   importers still use exactly
   `run`/`ensure`/`hostname`). Drift detection scans
   `packages/{cli,server,schema,core}/src` (`*.ts` and `*.tsx`), matches
   any quoted `@opentunnel/*` specifier (static, dynamic `import()`,
   `require`), and collects `@opentunnel` deps from every
   `packages/*/package.json`; if upstream moves/renames the module or
   extends its surface, the build fails instead of silently shipping the
   tunnel. The runtime-stage binary marker scan is the backstop beyond
   even that.
9. **Live listener audit.** `mt-admin worker-listeners <tenant>` lists
   listening sockets inside each running worker container, read **host-side
   from `/proc/<container pid>/net/{tcp,tcp6,udp,udp6}`** (running `cat`
   inside the untrusted container would let a root-in-container agent forge
   the audit), TCP LISTEN (0A) and UDP UNCONN (07) alike. It fails on any
   non-loopback bind beyond bridge-WS + health — and the expected-port
   exemption is TCP LISTEN only: those are the ports mt-admin publishes, so a
   UDP bind on 8443 or the WS port is unpublished and still a finding, with
   the protocol labelled on every reported line (run-90 FC-2). It fails on
   any published mapping
   off host loopback — an empty/missing `HostIp` counts as all-interfaces
   (Docker's shape; podman reports `0.0.0.0`) — and an inspect failure is a
   finding, never a silent skip. Absence requires a failed inspect with a
   Docker/Podman missing-object/container diagnostic naming that exact
   container; account/runtime-directory errors are findings. Warning lines
   before a valid state line are tolerated, and a failed PortBindings inspect
   is also a finding. Re-run against the `scootaloo` tenant on
   2026-10-13 (rootless podman, after the run-79 EI-6 hardening):
   **clean** — all four lanes showed exactly the expected bridge-WS + 8443
   binds, all mappings host-loopback only.

**!1570 deployment note:** rebuild the worker image and recreate existing
opencode2 containers through mt-admin to receive the protected boot files.
Restarting a container on an older image does not apply the new ownership or
startup logic. !1490 was closed; WI#427 now authorizes the v2.0.32 crossing.
The service-config reset is not an egress boundary.

Control 11 audit of the other lanes (2026-10-12, against pinned sources;
re-checked 2026-10-13):

- **opencode v1 (v1.19.5):** no service mode, no pairing, no tunnel (all
  v2-era surfaces). Session sharing exists and is pinned
  `"share": "disabled"`. v1's `serve` does honour
  `OPENCODE_SERVER_PASSWORD`, but this lane never sets one and its bridge
  sends no credential, so the containment is the loopback bind plus
  host-loopback publishing, verified live under control 9 — the same reason
  the v2-only password invariant in `exposure_policy_assert.py` is scoped
  that way. Adopting the v2 lane's per-boot shared password is an option if
  the in-pod loopback ever needs hardening.
- **kimi (2.1.1):** no server mode, pairing, sharing, or tunnel in the lane;
  `kimi -p` is spawned per task. `telemetry = false` is pinned in
  `config/config.toml`, and so is `max_output_size = 131072` on the baked
  model table — since 2.2.0 the CLI sends no `max_tokens` for an
  `openai`-type model without it (WI#412); the WI#206 fail-closed command
  guard is the only in-CLI enforcement point and stays mandatory.
- **pebble:** no listeners, pairing, or sharing in upstream
  (nanogpt-community/pebble) — the only `TcpListener` is test code.
  **Non-root runtime (WI#454):** the image defaults to `10004:10004`;
  mt-admin launches a separate `10104:10004` bridge and a task runner with
  all capabilities dropped on rootless and rootful hosts. The root-owned
  binary is not writable by either identity. The earlier root-bridge and
  rootful shared-identity models are superseded by the pair contract above.
  **Bump record (WI#425, 2026-10-26; no upstream ref change):** a PATH-visible
  refusal wrapper (`pebble4lunarwing/scripts/pebble-self-update-guard.sh`)
  owns `/usr/local/bin/pebble`; the reviewed binary moves to
  `/usr/local/lib/pebble/pebble`. It refuses naive `self-update` and honours
  no environment override. Direct execution remains possible, but the real
  binary and its parent are root-owned and non-writable by the task uid, so
  file permissions — not the wrapper — prevent replacement under either
  launch identity. The wrapper is only a naive-use guard, and the GitHub fetch
  remains an egress-allowlist concern (WI#424). Keep the pinned
  `PEBBLE_LOCK_SHA256` build contract intact. WI#454 implements the non-root
  container shape requested by WI#425 AC4 without combining bridge and task
  identities. A rebuilt candidate and live `mt-admin worker-listeners` audit
  remain protected-pipeline/deploy evidence; local tests do not attest to
  deployed images.

## LunarWing Worker

The main sandbox worker. Its runtime image includes Git, build-essential, Node.js/npm, Python 3, Rust, and GitHub CLI; it does not install a Claude Code executable. The orchestrator (`ic/src/orchestrator/`) manages container lifecycle, LLM proxying, and credential injection.

**Dockerfile:** `ic/Dockerfile.worker`

```bash
cd ic
docker build -f Dockerfile.worker -t lunarwing-worker:latest .
```

Runs as non-root user `sandbox` (UID 1000) in `/workspace`. Entrypoint is the `lunarwing` binary — the orchestrator passes the full command via Docker cmd.

### Generic sandbox egress default (WI#537)

On Linux the generic podman sandbox (`shell` tool, `ic/src/sandbox/`) defaults
to **`SANDBOX_NETWORK=proxy-enforced`** in code and in new multi-tenant envs;
other platforms default to `none`. mt-admin fills only an unset value and
preserves an operator's `none`, `proxy-enforced`, `tunnel`, or `proxy` on
re-render. The Linux default retains `--network none` and provides standard web
egress only through the WI#155 Unix relay and its public-only, DNS-pinned
allowlist proxy. It installs no credential mappings, so daemon API keys are
never injected in this mode. Advisory `proxy` remains an explicit legacy
choice: a process that ignores its env vars could reach worker WSS/health
ports, tenant PostgreSQL, and other tenants' loopback services, so it must
never become the default. See `ic/src/NETWORK_SECURITY.md` §5 for the tier
table and exact default allowlist.

Existing multi-tenant envs with an explicit `SANDBOX_NETWORK=none` stay on
`none`; to adopt enforced egress, set `proxy-enforced`, run `patch-env
<tenant>`, and restart through mt-admin. A single-node Linux install with no
value set changes from `none` to `proxy-enforced` on upgrade; set `none`
explicitly before upgrading to retain no-egress behavior.

### Generic sandbox image + tenant defaults (WI#223/537/596)

For new multi-tenant tenants, the generic sandbox's image is
**`lunarwing-sandbox:latest`**, built from `ic/docker/sandbox.Dockerfile`
without the lunarwing binary. Alongside the existing build tools it includes
Python headers, venv/pip, setuptools/wheel/build, pytest/pytest-cov/coverage,
mypy, black, isort, flake8, pylint, IPython, debugpy, tox, Twine, pipx, and
Poetry. Every top-level Python package has an explicit version pin. The base
image is digest-pinned and apt uses the fixed Debian snapshot over HTTPS, a
fixed-client-first bootstrap, bounded timeouts, and bounded retries
(WI#559/561). The Podman 5.8.2 verification build grew from 689,767,025 to
922,670,727 bytes (**+232,903,702 bytes**, about 222.1 MiB). Rootless tenants
each receive a copy in their own store, so budget roughly 0.92 GB of image size
per tenant rather than once per host (allocated disk depends on the storage
driver). mt-admin renders these defaults into every tenant's `lunarwing.env`
during `add-tenant` (existing values are preserved if that command is
explicitly re-run):

```bash
SANDBOX_RUNTIME=podman
SANDBOX_IMAGE=lunarwing-sandbox:latest
SANDBOX_AUTO_PULL=false
```

Every managed build carries an `org.lunarwing.sandbox.inputs` label. Its
SHA-256 digest covers the target revision's `sandbox.Dockerfile` and the
resolved `DEBIAN_SNAPSHOT` build argument. The Dockerfile's `ARG` value is the
default; `LUNARWING_MT_SANDBOX_DEBIAN_SNAPSHOT` is an explicit operator
override and is included in the digest. `add-tenant` and `upgrade-tenant`
default to `auto`: they build when the admin image is absent, unlabelled, or
has a different digest, and skip the build when it matches. `always` forces a
build and `never` suppresses it. Set the policy on an upgrade with
`--sandbox-image`, or on either command through
`LUNARWING_MT_SANDBOX_IMAGE_POLICY`.

For both tenant-driven paths, mt-admin verifies the tenant HEAD as the tenant
user and requires it to be known to and on the selected operator release ref.
It then uses `git archive` on the **operator checkout** to extract `ic/docker/`
at that revision into a private root-owned mode-0700 directory. The digest and
build read only that archive. A dirty, unknown, or off-release tenant source is
refused without replacing the current image. Root never reads or builds the
tenant-writable working-tree Dockerfile, including when Podman uses
`--network=host`. The standalone `build-sandbox-image` recovery command
continues to use the operator checkout directly.

For rootless Podman, `start-tenant` distributes the admin image into the
tenant's own store only when its image ID differs; unchanged inputs therefore
cause neither a rebuild nor a save/load redistribution. Docker uses the shared
store directly. On hosts with both CLIs, the generic shell sandbox prefers
Podman even when mt-admin runs its worker containers with Docker; an explicit
`SANDBOX_RUNTIME=docker` still chooses Docker. Provisioning uses the tenant's
selected runtime and prepares its rootless Podman store when needed.
`SANDBOX_AUTO_PULL=false` is deliberate: the tag exists only in local stores,
so an auto-pull would fail against a registry that has never hosted it.
Single-node hosts keep the in-code defaults (`auto` runtime preference,
`lunarwing-worker:latest`, auto-pull on).

During `upgrade-tenant`, the candidate is built while the old daemon still
runs. Only a successful candidate is promoted to `lunarwing-sandbox:latest`,
after the daemon/bridge/WASM build and before `start-tenant` distributes it. A
failed candidate or source-verification refusal leaves the previous tag and
image ID in place, prints a recovery command, and does **not** abort the daemon
or worker upgrade. Promotion records the previous admin image ID in the same
rollback record as the worker candidates; a post-promotion upgrade failure
re-tags that ID before the previous version restarts. The record is cleared
only after the whole upgrade succeeds. Old images can be pruned after that
rollback window with the operator's normal dangling-image maintenance.

**Shared-tag decision:** WI#596 retains `lunarwing-sandbox:latest` rather than
introducing per-release tags. There is one such tag per runtime admin store,
shared by every managed tenant using that runtime on the host. Upgrading one
tenant can therefore update the image for all of them. Docker tenants use that
shared store for subsequent sandbox commands immediately; rootless Podman
tenants receive the new ID at their next `start-tenant`, even if their daemon
was not upgraded. Podman and Docker admin stores remain independent. Operators
who need a fixed image must use a custom `SANDBOX_IMAGE`; custom-image tenants
and legacy tenants without the managed key are never rebuilt or redistributed.
To opt an older tenant into the managed image, set the three variables in its
`lunarwing.env` and run `upgrade-tenant`, or build manually and restart.

Manual build/recovery:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh build-sandbox-image <name>
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant <name>
```

`build-sandbox-image <name>` builds into the admin store of that tenant's
selected sandbox runtime (the argument may be omitted for the new-tenant
Podman-first default); `start-tenant` makes it available in the daemon's
runtime store. If either step fails, mt-admin warns and the generic shell
sandbox fails closed with `ImageUnavailable`; base tenant provisioning remains
available.

The build context is `ic/docker/`, not `ic/`: the Dockerfile has no
build-context COPY/ADD (its CA-bundle COPY comes from a digest-pinned image),
and `ic/` carries the vendored crate tree that must stay in-context for
`Dockerfile.worker`'s offline build.

## Pebble Worker

Rust-based Pebble agentic coding harness. Bridges the NanoGPT community `pebble` binary to the WebSocket protocol. Health on 8443, WebSocket on 9090.

**Dockerfile:** `pebble4lunarwing/Dockerfile`

```bash
# Run from the repository root; the Dockerfile copies the local bridge crate.
docker build -f pebble4lunarwing/Dockerfile -t lunarwing-worker-pebble:latest .

# Or select the worker per tenant and build the shared image via mt-admin
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant <name> --with-pebble
sudo ic/scripts/lunarwing-mt-admin.sh build-pebble-worker
```

Persistent worker mode (`PEBBLE_MODE=websocket`). Requires `AGENT_AUTH_TOKEN` for WebSocket auth and optionally `NANOGPT_API_KEY` for the underlying NanoGPT backend (configure per-tenant via `configure-pebble <name> --nanogpt-api-key <key>`). That command also pins the model backend into `pebble.env` as `PEBBLE_BASE_URL` (declaration) and `NANOGPT_BASE_URL` (the key the binary honours), defaulting to `https://nano-gpt.com/api`, so the tenant's egress allowlist derives the backend the worker really dials (WI#506); an undeclared or unresolvable backend refuses the lane at start instead of failing every job. Upstream Pebble v0.5.0 is fetched by its reviewed full commit, both Cargo builds use committed lockfiles, and base images/package indexes/toolchain downloads are pinned. See `pebble4lunarwing/` for details.

Launch identity (WI#454, supersedes WI#422): the image defaults to
`USER 10004:10004`; mt-admin runs the bridge as `10104:10004` and the task
runner as `10004:10004`, with no capabilities and no new privileges.
Rootless Podman keeps `--userns keep-id:uid=10004,gid=10004`. Rootful
launches use the same pair plus the per-start mounted-workspace identity
gate documented above, replacing the old `PEBBLE_TASK_UID=inherit` exception.
Task caches live under `/home/pebbletask`; the bridge's private home is
`/home/pebble-bridge`. The workspace remains shared and setgid. These runtime
contracts and redirect refusals are pinned by
`test-mt-admin-worker-image-contract.sh`; a runtime restart of an old image
does not acquire them.

In the fat (`--with-toolchains`) image the Rust/Go toolchains are anchored against the `/etc/profile` PATH reset in upstream pebble's `/bin/sh -lc` task shell (WI#334): an `/etc/profile.d` snippet plus real-binary `/usr/local/bin` symlinks and a `rustup` wrapper keep `cargo`/`go` on PATH after the reset, proven at build time in both the real spawn mode and a fully sanitised shell, and re-probed by the smoke script at the end of every fat mt-admin build (which targets a candidate tag and re-tags `:latest` only after the probe passes, using the build's own container runtime and `--pull=never`). Re-check any built image or running worker with `pebble4lunarwing/scripts/smoke-task-shell-toolchain.sh image [ref]` / `container <name>`; see `docs/ops/PEBBLE-WORKER.md` "Task-Shell Toolchain PATH".

Upstream's pinned lockfile ships rustls 0.23.41. The reviewed override now pins **0.23.46** (WI#409), fixing RUSTSEC-2026-0285 and RUSTSEC-2026-0346/0347/0348/0349; `pebble4lunarwing/upstream/Cargo.lock` is copied over the checkout during the image build. It also retires upstream's yanked `chacha20 0.10.1` pin (0.10.2) and, for CVE-2026-111943 (2026-10-20), raises rustls-webpki to **0.103.16** (RUSTSEC-2026-0382) and webpki-roots to **1.0.10** (RUSTSEC-2026-0381) — the distrusted emSign intermediates pair. The Dockerfile supplies the canonical reviewed `PEBBLE_LOCK_SHA256` default for CI, mt-admin, Compose, and plain Docker builds. `scripts/verify_upstream_lock.py` verifies that digest and compares complete parsed package records (including additions/removals, source, checksum, and dependencies; records are keyed by `(name, version, source)` so multi-version crates such as `cpufeatures`/`getrandom` are fully covered), allowing changes only to the reviewed allowlist (rustls/rustls-webpki/webpki-roots/chacha20, plus the 2026-10-24 set: quinn/quinn-proto/quinn-udp, tokio and clipboard-win; run-214 NX-1 keeps errno/rustix/winapi-util out of the allowlist because no dependency edge is re-pointed) and rejecting a no-op override so the input is retired once upstream ships its own fix. The 2026-10-24 refresh (v2.1.7.0 item 3) also raises quinn-proto **0.11.16 → 0.11.19** (RUSTSEC-2026-0338/0339/0340/0341/0342, QUIC denial-of-service; quinn is locked through reqwest's weak `quinn?/ring` edge with `http3` off by default, so it is lockfile-only for every pebble build — fixed so enabling that feature cannot turn the advisories live), quinn **0.11.11 → 0.11.12**, quinn-udp **0.5.15 → 0.5.16**, tokio **1.52.3 → 1.53.3** (RUSTSEC-2026-0386, unsound safe `AsyncFd` constructors), and clipboard-win **5.4.1 → 5.4.3** (yanked), matching the levels already pinned by every other audited root; only those five packages' version/checksum lines change and no package is added or removed. Because this override is manifest-less, the satellite advisory sweep skips it by design — its advisory state is verified by resolving the override against the pinned upstream ref and scanning that graph, in addition to the digest/delta gate. `.gitlab/deliverables.json` explicitly classifies this lock-only override under `build_inputs`, with ci-config regression checks and worker-image candidate coverage; it is not a standalone Cargo root. Rebuild the Pebble image and verify the protected candidate/scan before promotion; changing the lock alone does not patch deployed images. On 2026-10-28 the override also moved pulldown-cmark 0.13.4 to 0.13.5 (RUSTSEC-2026-0414: exponential event growth with the wikilinks option, which pebble enables through `Options::all()` when rendering model output); `PEBBLE_LOCK_SHA256` and the verifier allowlist were re-pinned with it. On 2026-11-02 both Pebble locks moved to pulldown-cmark **0.13.6** for RUSTSEC-2026-0433 (quadratic parsing of nested image wikilinks with the same enabled option), and the reviewed lock digest was re-pinned again.

## Opencode Worker

[opencode](https://opencode.ai) (anomalyco/opencode, formerly sst/opencode) integrated as a persistent LunarWing external worker. A Bun/TypeScript bridge wraps the `@opencode-ai/sdk` and translates between the opencode session/event model and the `lunarwing-agent-v1` WebSocket protocol. The opencode headless server runs internally on `127.0.0.1:4096` (not exposed). Health on 8443, WebSocket on 9090.

**Dockerfile:** `opencode4lunarwing/Dockerfile`

```bash
cd opencode4lunarwing
docker build -t lunarwing-worker-opencode:latest .

# Select the worker per tenant, then build the shared image via mt-admin
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant <name> --with-opencode
sudo ic/scripts/lunarwing-mt-admin.sh build-opencode-worker

# Or build the shared image as part of a per-tenant build
sudo ic/scripts/lunarwing-mt-admin.sh build-tenant <name> --with-opencode

# Include Rust, Go, and C/C++ compilers for build/test jobs
sudo ic/scripts/lunarwing-mt-admin.sh build-opencode-worker --with-toolchains
```

The default image is slim and does not contain Rust, Go, or C/C++ toolchains. Build with `--with-toolchains` when worker jobs must compile or test code; the toolchain image uses Rust 1.96.1. Modes: `--mode websocket` (default, persistent) or `--mode cli` (one-shot; prompt via `TASK_PROMPT`). Requires `AGENT_AUTH_TOKEN` for WebSocket auth. Supports per-tenant model/base-URL overrides via `configure-opencode <name> [--model <m>] [--base-url <url>]`, optional Paseo MCP integration (`PASEO_URL`/`PASEO_TOKEN`), and SSH agent socket bind-mounting. `configure-study-mcp <name> [--home <absolute-checkout>]` gives core and OpenCode the same lw2studymcp data. Without `--home` it uses the shared default checkout (`/var/cache/lunarwing/lw2studymcp`, override with `LUNARWING_MT_STUDY_NOTES_HOME`) and always syncs it to the latest upstream (`LUNARWING_MT_STUDY_NOTES_REPO`, default `https://git.lunarwing.org/cmc/lw2studymcp.git`) first — cloning when missing, fast-forwarding a clean checkout, and refusing to touch a dirty or non-git directory. `--sync` applies the same update to an explicit `--home`. `add-tenant <name> --with-study-mcp [--study-mcp-home <path>]` runs this auto-configuration (read-only) during provisioning so a new tenant starts with current knowledge of the LunarWing codebase. `--allow-write` enables approved core-agent mutations in separate state; OpenCode always receives code and state read-only at `/opt/lw2studymcp` and `/var/lib/lw2studymcp`. OpenCode is pinned to **v1.19.5** (WI#531), fetched by reviewed full commit `810c0eba1a7348036f8bdf61e6632d0c36c6876c` (v1.18.29 was reverted on 2026-09-07: that release crashes on model entries lacking full metadata shape and dies silently in its ai-sdk stream path — see the Dockerfile pin note. Neither regression is verified fixed at v1.19.5; the WI#531 move is authorized while the lane is expected to retire, and one live v1 dispatch on a test tenant after the first master candidate build is the settling step); `OPENCODE_REF` accepts only a full SHA, the checkout is verified, and a frozen Bun lock is mandatory. The release and SHA are baked into `OPENCODE_IMAGE_REF` as `v1.19.5@<full-sha>` and reported together as `opencode_ref`. Base images, Debian indexes, and optional toolchain downloads are also pinned. See `opencode4lunarwing/README.md` for the full environment reference.

Agent-facing dispatch uses `create_job(title: "...", description: "...", mode: "opencode")` in both legacy and Engine V2 chat paths. `opencode` is a named job execution backend, not an extension, MCP server, skill, routine, or SSH host; no registry search or installation is involved.

**Known issue (WI#320 F4, tracked as WI#333; WI#374 reported the same crash on a 2.1.6.0 test tenant):** at the v1.18.25 pin the `mode: "opencode"` lane was broken in the LunarWing worker shape — every task fails with `external worker 'opencode' failed: Unexpected server error` because `opencode serve` crashes in `SystemPrompt.environment` (`undefined is not an object (evaluating 'a.name')`) while assembling the prompt, before any model call, and answers the SDK with that bare HTTP 500 wrapper. No config remediation (base-URL override, MCP/plugin strip, git-init workspace) avoids it, and v1.18.29 is worse (see the pin note above), so the lane cannot be fixed from this repo until upstream ships a fix; the opencode2 lane below is the working one. To surface the root cause, the worker attaches a bounded, ref-correlated, error-level tail of serve's own structured log (`$XDG_DATA_HOME|~/.local/share/opencode/log/opencode.log` — verified at the pinned ref: serve logs 500 causes there as `level=ERROR` lines with a unique `ref=` id, and the 500 body carries the same ref) to these job errors — only the failing request's line attaches, and a 500 without a ref attaches nothing — so the crash's real stack line reaches `list_jobs`/`job_status`. **The v1.19.5 bump (WI#531) does not change this:** `sys.environment(model)` and the surrounding prompt assembly are byte-identical at both tags, so the crash path is carried forward untested and no live dispatch has run at v1.19.5. The v1 lane is expected to be retired (goals item 34: "prob rm opencode v1 in .1.7.0").

**Reproducible build input (WI#467, 2026-10-23):** the pinned v1.19.5 build compiles a models.dev catalog into the binary. At `810c0eba` (and identically at the previous `cb7d8b2f`), `packages/opencode/script/generate.ts` reads it from the file named by `MODELS_DEV_API_JSON` when set, and otherwise fetches `${OPENCODE_MODELS_URL:-https://models.dev}/api.json` live; `script/build.ts` then passes the text to `Bun.build` as the `OPENCODE_MODELS_DEV` define, so the catalog is compiled in as code. Until WI#467 the Dockerfile never set the variable, so the binary changed whenever models.dev published between the candidate build and the `--no-cache` rebuild, and the clean-rebuild gate failed on master. The lane now carries a reviewed snapshot at `opencode4lunarwing/models-dev/api.json` (fetched from models.dev @ `a051b3ca330b`, 2026-10-23), committed pretty-printed so a refresh diffs line by line; the minified bundle is byte-identical to the one built from the one-line form. Its sha256 is pinned as the Dockerfile's `MODELS_DEV_SNAPSHOT_SHA256` ARG (not `*_API_JSON_*`: gitleaks' generic-api-key rule reads an ARG whose name contains `API` and whose value is 64 hex as a leaked key). `validate_config.py` cross-checks the pin, requires the snapshot to be strict JSON (one non-empty object, no duplicate keys, no NaN/Infinity) in the canonical pretty layout, and pins the builder-stage tail from `WORKDIR /build/opencode/packages/opencode` to the next `FROM` instruction by instruction (`OPENCODE_MODELS_DEV_BUILD_STEPS`). The build RUN verifies the hash, runs `scripts/check_models_dev_snapshot.py` (strict JSON, and `generate.ts` must still read both env vars), and builds with `OPENCODE_MODELS_URL=http://127.0.0.1:9`, so an upstream that ignores the snapshot and fetches anyway fails the build instead of compiling a live catalog. Both files hash into the opencode `worker-inputs.sha256`. `scripts/check_models_dev_snapshot.py` exits 0 against
v1.19.5's `generate.ts` and the committed snapshot, so the WI#531 bump left
the pin, the file and `OPENCODE_MODELS_DEV_BUILD_STEPS` unchanged.
**Bump checklist addition:** refreshing the snapshot is a deliberate commit: write the canonical form (`curl -fsS https://models.dev/api.json | python3 -c 'import json, sys; print(json.dumps(json.load(sys.stdin), indent=1, ensure_ascii=False))' > opencode4lunarwing/models-dev/api.json`), update `MODELS_DEV_SNAPSHOT_SHA256` to match (`sha256sum opencode4lunarwing/models-dev/api.json`), and record the models.dev commit/date here. An `OPENCODE_REF` bump re-checks `generate.ts` and updates `OPENCODE_MODELS_DEV_BUILD_STEPS` in the same commit if the build step changes.

## LunarDepth Worker (lunardepth)

[LunarDepth](https://git.lunarwing.org/lunarwing/lunardepth) is an opt-in external-worker lane for the DeepSeek Harness fork. It uses the generic external-worker backend: dispatch with `create_job(mode: "lunardepth")`, not a Rust backend, extension, MCP server, skill, routine, or SSH host. The implementation contract is `docs/proposals/LUNARDEPTH_WORKER_LANE.md`; lane details and the development checks are in `lunardepth4lunarwing/README.md` and `lunardepth4lunarwing/AGENTS.md`.

### Provenance and image contents

The canonical repository is `https://git.lunarwing.org/lunarwing/lunardepth.git`, pinned to full commit `da06c33b2091546dc38c61b95452ce27a4ce32d5` (source package `0.2.1-alpha.1`). The image build checks out and verifies that exact commit and requires these ancestry markers:

- compatibility/item 9: `17aef23e05cc074c8621d38d159d77d25cb348ca`;
- TensorMoon routing: `8c610180c84a2f4c27fb2958e6e7dab9e48d476b`.

A branch, short SHA, moving tag, or substitute repository is not a valid pin. Both worker and CI build arguments carry the same values. Node is `22.23.4-bookworm-slim` at the shared `CI_NODE_IMAGE` digest, pnpm is exactly `11.7.0`, and `DEBIAN_SNAPSHOT` is the shared `20261105T082236Z` snapshot with the WI#559/561 APT controls. `pnpm install --frozen-lockfile` must leave the upstream lock byte-identical.

The build runs LunarDepth's focused worker tests/typechecks and materializes the production closure with `build-exe-for-python-sdk.ts --deploy-to`. Both `@lunardepth/dsh-lunarwing-worker-app` and `@lunardepth/dsh-lunarwing-worker` must resolve from that deployed closure, not a source checkout. The runtime launches built artifacts, never `tsx`. The optional `--with-toolchains` image adds the lane's Python/C++ job toolchain, not the other lanes' Rust/Go sets. Podman 4.5+ or Docker BuildKit/buildx is required for the real entrypoint's `RUN --network=none` split-role smoke.

The builder installs `musl-tools` and runs the full native build, including the static `landlock-run` launcher; the host-addon-only build is insufficient. Container `/proc` restrictions make the bwrap profile unusable in this lane, while Landlock works without added capabilities. The host kernel must support every Landlock ABI the pinned launcher uses: a `partial` probe is refused just like a missing launcher or `SANDBOX_UNAVAILABLE`. For the current pin that is Landlock ABI 5, so the build host, every CI runner that builds the image, and every tenant host need **Linux 6.10 or newer with `landlock` in the active LSM list** (`/sys/kernel/security/lsm`). `build-lunardepth-worker` and the upgrade preflight check this first and stop with that reason, before a long build or a tenant stop. Both the image smoke and live worker smoke require `full`, a workspace write, `mktemp`, and a denied escape.

### Dispatch, identities, and authority domains

The two containers have separate non-root identities, all capabilities dropped, and `no-new-privileges`:

```text
daemon → host-loopback bridge :<lunardepth_wss> → task-runner Unix socket → LunarDepth 127.0.0.1:19091
         task 10005:10005; bridge 10105:10005; harness loopback only
```

Only the bridge publishes anything, and only on host loopback: under mt-admin its WebSocket listens on the tenant's `lunardepth_wss` registry port and its health endpoint (container port `8443`) is published on `lunardepth_health`. `9090` is only the image default, used by the compose file. The bridge holds the tenant hub credential only in its private role. The task receives no `AGENT_AUTH_TOKEN` or `AGENT_AUTH_TOKEN_FILE`; its runner refuses either. A random per-boot local bearer protects the task-only harness API, whose exact `/ws/agent` upgrade uses `lunarwing-agent-v1`. Task health reports protocol `version: "1"`, not the source release; the bridge reports `lunardepth-worker-1.0.0` separately. Process existence is not readiness.

Each pair runs one task at a time and explicitly refuses a second. Jobs sharing a pair also share the task uid, workspace, LunarDepth session history/logs, and local transport credential. Separate tenants or pairs are required for separate confidentiality or grant domains; scrubbing child environments does not create per-job isolation.

### Explicit TensorMoon-compatible route and model key

This lane never inherits `LLM_BASE_URL`, `LLM_MODEL`, TensorZero, or another worker's route. Configure a complete OpenAI-compatible API root and model explicitly:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-lunardepth <tenant> \
  --base-url https://tensormoon.example.com/openai/v1 \
  --model tensormoon::function_name::lunarwing \
  --api-key-file /protected/tenant-lunardepth-key
sudo ic/scripts/lunarwing-mt-admin.sh build-tenant <tenant> --with-lunardepth
sudo ic/scripts/lunarwing-mt-admin.sh start-worker <tenant> lunardepth
```

The URL excludes `/chat/completions`; the example host and function name are placeholders, not a fleet default. HTTPS is required beyond loopback. Non-loopback cleartext HTTP requires the separate `--allow-insecure-http` exception. A deliberately unauthenticated gateway requires `--allow-unauthenticated-gateway` and uses only LunarDepth's nonempty placeholder-key behavior. Omitted values and explicit exceptions survive a re-render; no inherited endpoint marker is synthesized. `export-tenant.sh` does not carry the lane's route or key: pass them again to `import-tenant.sh` (`--with-lunardepth`, `--lunardepth-base-url`, `--lunardepth-model`, `--lunardepth-api-key-file`). Clear a previous exception explicitly with `--disallow-insecure-http` or `--require-api-key`, respectively.

For an existing tenant, `configure-lunardepth` runs the v18 registry migration without renumbering existing ports, allocates the lane's two ports, enables `workers.lunardepth`, and writes the generic daemon endpoint with `supports_credentials = false`. The task env file contains literal unquoted route and key values; placeholder mode and a real key are distinct choices, and `--api-key-file` is refused while the unauthenticated-gateway exception is on, because no key would be stored. Podman env-file parsing therefore cannot turn the URL or credential into a value containing quote characters.

`configure-lunardepth` validates and stores the route. It does not render the effective overlay; that happens at start (below). When the worker pair is running, the command applies the new configuration at once and restarts the pair if it changed, so a unit never keeps an overlay digest that no longer matches its environment. When the pair is not running, the command says that the configuration applies at the next `start-worker <tenant> lunardepth`.

`LUNARDEPTH_API_KEY` is **task-visible, including to the model**. Issue a different independently revocable key per tenant, restricted to that tenant's inference. `configure-lunardepth --api-key-file` stages it only in `lunardepth-task.env`, never in the bridge, overlay, argv, logs, health, examples, or export text. The fleet command enforces the same rule: `add-tenants --lunardepth-api-key-file` is accepted for a single tenant only, and several tenants take `--lunardepth-api-key-dir <dir>` with one `<tenant>.key` file each. Two tenants with the same key are refused before the first tenant is created. `lunarwing-mt-provision-openrc.sh` has `LUNARDEPTH_API_KEY_DIR` for this. The key is not a hidden trust boundary against a model or code running with the task's authority.

### Configuration integrity and confinement

The final immutable overlay names every security row's exact upstream package and an explicit `disabled` value. The complete permission row allows only `workspace-write`/`ask`, with literal `defaultPreset: workspace-write`. `danger-full-access`, `auto`, `dsh-bash-local`, `runnerCommand`, and `runnerFailureSignatures` are forbidden; a headless approval escalation is unavailable and cannot execute.

Three reviewed v1 choices are deliberate:

- `tool-fs-search` is disabled because its unsandboxed ripgrep could expose the model key through `/proc/<pid>/environ`. `ptc-runtime` and `workflow-ptc` are also disabled rather than leaving a second executor surface.
- `TMPDIR`, `TMP`, and `TEMP` are all `/tmp`, allowing confined `mktemp` under both built-in runners. They cannot be redirected through home or workspace `.env` files.
- The profile template is generated at image build by the pinned source's `initProfile`, never by a handwritten manifest. The current pin exports no `PROFILE_SCHEMA_VERSION`, so its recorded schema is `unversioned`. A future pin must export and match its declared schema.

Every start removes lower configuration except the real task-owned `sessions`, `storages`, and `attachments` data directories, then regenerates the profile, home patch, and home `.env` from root-owned read-only templates. The harness gets an empty root-owned launch cwd and home, a system-only absolute `PATH`, and `WS_NO_BUFFER_UTIL=1` plus `WS_NO_UTF_8_VALIDATE=1`. HMR, runtime plugin installation/editing, and settings/config editing are disabled. Task setup is sourced into the task launcher so its `HOME`, `DSH_HOME`, model key, local token, temp paths, and security unsets actually reach the harness and runner.

The effective overlay is rendered by the tenant's worker image itself. At every start mt-admin runs the image's own `render_overlay.py --stdout` on the template baked into that image, in a throw-away container with no network, no capabilities and a read-only root, and installs the bytes root-owned and mode `0444` on the host. The task container renders again from the same template and requires byte equality, so both sides always come from one image: an operator checkout at a newer revision cannot break a tenant whose image is older. The overlay changes on disk only together with the units that use it. The task cannot rely on owner UID `0` appearing correctly under rootless `keep-id`, so the launch contract passes the rendered file's SHA-256; setup requires that exact digest and mode `0444` before validating the route. Host-side root ownership, the read-only bind, and regeneration still apply. Study Notes and SSH-agent mounts are off in v1.

Before readiness, the effective-composition gate requires the exact package/row allowlist and final overlay values without evaluating `!!js`, zero loader warnings, and no missing, skipped, or unresolved bundle. The profile `package.json` must be byte-identical before and after the dump and after actual readiness. The real production entrypoint must also report fresh-session `permission/preset=workspace-write`, `sandbox/mode=workspace-write`, and `approval/policy=ask`, plus full built-in bwrap/Landlock enforcement, successful workspace and `/tmp` writes, and a denied escape. `SANDBOX_UNAVAILABLE`, partial enforcement, an unconfined fallback, or model-key disclosure through an enabled reader prevents acceptance.

LunarDepth's sandbox confines file effects only; it is not a network boundary. The per-tenant `lw-egress-<tenant>` nftables allowlist is the only boundary against a compromised agent. It admits the configured model endpoint and explicitly approved job hosts, not npm, DeepSeek account services, telemetry, public fetch/search, model catalogs, or update services. A missing, malformed, or unresolvable LunarDepth backend fails locally with status 4 rather than silently omitting its target.

The v1 task protocol deliberately has no credential/environment grants, cron, steering, resume, LunarWing tool proxy, or follow-up `task_prompt` capability; unsupported frames are rejected rather than silently discarded. The daemon's ordinary `user_id`, string metadata, conversation history, and empty environment map are accepted. History is bounded at the harness's own caps, 50 messages, 10,000 UTF-16 code units per message and 50,000 in total; the lane overlay sets the harness to those caps and the bridge adapter enforces the same bounds, so every history the daemon sends at its default limits fits, and one above the caps gets an explicit error result, never truncation; a non-empty environment or credential grant returns an explicit error `task_result` without disconnecting. There is no tunnel, relay, pairing, public share, web UI, non-loopback harness, remote plugin, self-update, or third-party telemetry path. The root-owned `dsh` refusal wrapper is only naive-use protection: direct Node invocation can bypass it. The unpublished loopback harness, immutable configuration, host listener audits, and egress filter are the real boundaries.

### Ports, rollout, and rollback

Registry v18 reserves `52000–52999`, allocating `lunardepth_wss` and `lunardepth_health` in two-port blocks. `workers.lunardepth` remains false unless selected; migration does not renumber existing tenants or expose the internal `19091` listener. Both the systemd user-manager path and the OpenRC path are implemented in mt-admin. Only the OpenRC path has run on a live tenant so far; the systemd path is covered by harnesses with stubbed starts.

```bash
sudo ic/scripts/lunarwing-mt-admin.sh build-lunardepth-worker --no-cache
sudo ic/scripts/lunarwing-mt-admin.sh start-worker <tenant> lunardepth
sudo ic/scripts/lunarwing-mt-admin.sh worker-listeners <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh worker-egress <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh worker-smoke <tenant> lunardepth
sudo ic/scripts/lunarwing-mt-admin.sh stop-worker <tenant> lunardepth
```

The build produces a candidate image ID, exports and inspects that immutable image out of band, and only then tags the selected worker image. Nothing that reads the candidate runs as root: the TypeScript compile step and the verifier run as `nobody`, the verifier with an isolated interpreter and a cleared environment on a private snapshot of the lane, as the opencode2 lane does. Image ID, overlay, task environment, and split-launch contract changes recreate the pair. `build-tenant --with-lunardepth`, `build-all --with-lunardepth`, and `upgrade-tenant` use the same lane; pre-stop `build-worker-candidates --enabled --target <ref>` leaves the running tenant untouched if a candidate fails.

The production closure is locked twice: the workspace install uses `pnpm install --frozen-lockfile`, and an exact-block transformation of the pinned build script replaces its legacy live-registry deploy with pnpm's shared-lockfile deploy (`--frozen-lockfile`, injected workspace packages, and `minimumReleaseAgeStrict=true`). `pnpm-workspace.yaml` and its `minimumReleaseAgeExclude` list must not change. After deployment, the lane rejects every installed external `name@version` absent from `pnpm-lock.yaml`, checks workspace versions against the lockfile's importer manifests, and records the sorted inventory hash as `LUNARDEPTH_DEPLOYED_PACKAGES_SHA256` in `worker-inputs.sha256`. Five-digit directory/file modes clear inherited set-ID bits, and the final build assertion rejects any residual set-ID or write bit under `/app`, `/opt/lunardepth`, or `/opt/lunardepth-templates`.

`promote-worker-images <tenant>` records each prior image ID before promotion. `restore-worker-images <tenant>` re-tags those recorded IDs without rebuilding after a failed upgrade; retain the rollback record until the complete upgrade succeeds. Restart the pair after restoring and repeat listener/egress/smoke checks. Rollback requires matching lane scripts, mt-admin modules, units, configuration, and IPC contract. It must not rename or adopt an older writable IPC volume, erase the workspace, or print a rollback token or model key. A pruned old image cannot be restored from its record and is an explicit failure, not a claimed rollback.

### Publication and version-bump checklist

MR source/policy jobs do not receive publication credentials. Protected `worker-lunardepth-candidate` must produce an immutable candidate, run the real split-role smoke, export `lunardepth-rootfs.tar`, and match an uncached clean rebuild's whole-image digest. `worker-lunardepth-exposure-verify` independently validates tar ownership/modes and full image config, while `worker-lunardepth-scan` applies the CRITICAL vulnerability gate to the exact candidate digest. Manual `worker-lunardepth-promote` publishes that verified digest as `$CI_COMMIT_TAG-lunardepth`, never a rebuilt or `:latest` image. It hard-needs candidate, exposure verification, scan, `worker-exposure-policy-guard`, and the BuildKit parser guard. Local tests and an MR pipeline do not substitute for those protected or live acceptance gates.

For every LunarDepth version or security-input bump:

1. Review the complete upstream diff for new exposure/publication surfaces and record the disposition of tunnels, relays, pairing, sharing, web listeners, public fetch/search, telemetry, DeepSeek account routes, remote plugins/MCP, settings/HMR, and self-update. Keep WI#357 egress enforcement unchanged.
2. Move the canonical full `LUNARDEPTH_REF`, both required ancestry checks, source version, and every CI/build inventory together. Re-run toolchain and frozen-lock gates; never accept a moving ref.
3. Re-audit every complete overlay row, package `name`, explicit `disabled` value, permission/preset, and unsupported capability. Re-check that `tool-fs-search`, programmatic execution, and any unsandboxed model-key reader cannot expose the **task-visible model key**.
4. **The profile template and `LUNARDEPTH_REF` move together.** Regenerate it with the pinned `initProfile`; inspect `PROFILE_SCHEMA_VERSION`, every profile writer/migration path, and the before-dump/after-dump/after-readiness hash invariant. Replace `unversioned` only when that exact pin exports the schema.
5. Re-prove full built-in sandbox enforcement, `/tmp` and workspace writes, a denied escape, fresh-session policy events, lower-layer regeneration across two restarts, and no hostile `.env`, module, or profile-manifest persistence. Do not replace a failed real entrypoint smoke with source-only tests.
6. Reconcile Node, pnpm, Debian snapshot, runtime/toolchain, lane input, parser, image-config, and deployment inventories. Keep the two production worker packages resolvable from the materialized closure.
7. Require the protected candidate/rebuild digest match, out-of-band exposure verification, and CRITICAL scan on the exact head before immutable promotion. Exercise mt-admin candidate promotion and image-ID rollback, both init systems, real daemon dispatch, and one tenant-approved TensorMoon inference before claiming live acceptance.

## OpenCode 2 Worker (opencode2)

The OpenCode **2** lane, run as a separate external worker per
`docs/plans/OPENCODE2_EXTERNAL_WORKER_PLAN.md`. v1 stays the default and
keeps its v1.19.5 pin (v1.18.25 was the last dispatch-verified release; see
the known issue above); v2 is opt-in per tenant until upstream's patch
stack settles. Do not configure both lanes on the same
tenant during the transition (agent routing confusion — plan §5).

**Dockerfile:** `opencode2.4lunarwing/Dockerfile` — pinned to
**v2.2.7** at reviewed full commit
`fedc6f208aa2bd54c1019227214568083f6380ad` (WI#609; WI#427 boundary crossing
first crossed at v2.0.32/`843a6c514e606b436a7fe713422ace0a93ad3d61`); the v2 bridge uses
`@opencode/client` (promise API) instead of `@opencode-ai/sdk`, and the
v2 config file is `config/opencode2.json` (`providers` map,
`settings.baseURL`, `package`). Container UID/GID is **10003**
(opencode 10001, kimi 10002) so both opencode lanes keep separate
userns identities on one host.

OpenCode 2 **image builds require Podman >= 4.5.0** (the first release
vendoring Buildah 1.30.0, which added inline `RUN --network`; Podman 4.4.4
still vendors Buildah 1.29.0). This is independent of the >= 4.6 Quadlet
supervision floor: older hosts may keep running existing worker images, but
must upgrade their builder before rebuilding this lane. The mt-admin
preflight rejects an older or unreadable Podman version before any image
prune/build. Docker builds require **BuildKit with buildx installed**; mt-admin
rejects `DOCKER_BUILDKIT=0` and explicitly sets `DOCKER_BUILDKIT=1` for the
build. The legacy Docker builder cannot parse the per-RUN isolation flag and
is unsupported. `upgrade-tenant` also runs this floor check **before it
stops the tenant** (WI#456): `build_tenant` rebuilds pebble/opencode/kimi
first, so on a below-floor host the opencode2 gate used to fail only after
every lane was already down. The probe is `opencode2_builder_supported`
(non-dieing; `build_opencode2_worker` and the upgrade preflight both consume
its reason), so the refusal names the operator action and states the tenant
was not stopped. Both stop paths enforce it: mt-admin's own
`upgrade-tenant` before `stop_tenant`, and `stop-tenant-for-upgrade` — the
boundary `ic/scripts/upgrade-tenant.sh` uses before it runs `build-tenant`
(run-284 review SR-1) — so the external wrapper cannot bypass the check.
This covers only the opencode2 floor; a kimi/pebble build failure after the
stop can still strand a tenant, tracked as WI#527. Sources: Podman
[v4.5.0 go.mod](https://github.com/containers/podman/blob/v4.5.0/go.mod) and
[v4.4.4 go.mod](https://github.com/containers/podman/blob/v4.4.4/go.mod).

**WI#527 — upgrade build ordering and convergence.** The upgrade no longer
stops first and rebuilds second. Both paths check out the target and build
every *selected worker image* to a per-tenant candidate tag
(`lunarwing-worker-<lane>:wi527-<tenant>`) **while the tenant still runs**, then
stop, build only the daemon/bridge/WASM, and promote the candidates to
`:latest` once the target daemon exists. Consequences:

- A worker-lane build failure is a no-op for a running tenant: the clone is
  returned to its recorded position (branch or detached HEAD, and a pre-upgrade
  stash is popped back), the candidates are discarded, and the error says the
  tenant was not stopped. The only post-stop build is the daemon, bridge and
  WASM — no worker lane is rebuilt after the stop, on either path. The external
  wrapper's live path runs `build-tenant --with-wasm --require-wasm` after the
  stop and asks for every *enabled* lane as a candidate
  (`build-worker-candidates <tenant> --enabled`, `--with-pebble` still
  available as an explicit opt-in), so the restarted daemon is always the newly
  built one and no enabled lane silently keeps its previous image.
- Every pre-stop exit on the wrapper path now puts the clone back and pops the
  pre-upgrade stash — including a DarkIRC refusal and a failed `git fetch` /
  `git checkout`, not only a candidate-build failure — and a candidate-build
  failure also discards the partial candidate images and lane record, so a
  later manual promote cannot promote images built from the checkout that
  failure reverted (run-300 TO-4).
- `build_tenant` records each completed stage (`daemon`, `bridge`, `wasm`) to
  a root-owned marker file in the tenant lifecycle lock directory
  (`/run/lock/lunarwing-tenants/<tenant>-build-stages` by default), never in
  tenant-writable state; the directory is created with the same hardened
  no-symlink, root-owned 0700 sequence as the lifecycle locks. Each run starts
  with a `start` sentinel, so "nothing built yet" is distinguishable from an
  unreadable marker. Post-stop convergence classifies the failure by that
  marker, never by matching die prose: before the daemon stage the previous
  build is restarted in place; once the target daemon was built a restart would
  run the *new* binary and apply the target's migrations, so the previous
  commit is rebuilt before starting (non-strict WASM: a broken WASM toolchain
  must not block restoring a binary whose artifacts are already installed), and
  if that fails the tenant is reported **stopped** with explicit rollback
  commands — never as "restored". A missing, unreadable or unknown marker is
  treated as past-the-daemon (fail closed) on both paths (run-300 TO-7 closed
  the wrapper's missing-marker read as "none").
- `promote-worker-images <tenant>` re-runs a promotion that failed after the
  daemon build: already-promoted lanes are skipped, the rollback file keeps the
  **first** pre-promotion `:latest` ID per lane (so a re-run never records an
  already-promoted candidate as the old image), and the lane list is updated
  per lane, so a re-run only promotes what remains. `restore-worker-images
  <tenant>` re-tags those recorded IDs; both upgrade paths call it on a failed
  upgrade, and it is printed in the wrapper's rollback steps. A rollback is
  only reported when the re-tag actually succeeded. The record is cleared by
  `clear-promote-rollback` only when the **whole** upgrade has succeeded
  (both paths), and a new candidate run discards any record from an earlier
  run, so a later failure can never re-tag a lane to an image this run never
  had. `build-worker-candidates` and `promote-worker-images` serialise with a
  running upgrade by **holding** the tenant upgrade lock for the whole verb
  (a bare preflight was a check-then-run race); when they run inside the
  upgrade wrapper's own locked boundary they reuse the inherited descriptor
  instead of re-flocking — a second open-file description would deadlock
  against that parent, which was the run-300 TO-1 blocker.
- A pebble candidate keeps the toolchain profile of the `:latest` image it will
  replace (the image's `org.opencontainers.image.toolchain-profile` label), so
  an upgrade no longer downgrades an operator's fat (`--with-toolchains`) image
  to slim. The fat build's staging tag is derived from this build's output tag
  (`<output-tag>-candidate`), so two concurrent builds cannot retag each
  other's image between the smoke gate and the final retag.
- The post-stop build log is a root-only `mktemp` file under `${TMPDIR:-/tmp}`
  that is kept on failure (`build log: <path>` in the error). It is not pruned
  automatically; on a long-lived host the operator should clear `/tmp` of
  `lw-upgrade-build.*` files. Candidate tags are removed on the success path
  and by `cleanup_candidate_workers` on the pre-stop failure path; a lane whose
  `rmi` fails stays in the lane file (visible to `cleanup-worker-candidates
  <tenant>`, which retries it) and never affects the running lanes, which still
  point at `:latest`. `last-build-stage` and `reset-build-stage` require root,
  because the marker (and the candidate lane list) live in the root-only lock
  directory and are not persistent across a reboot.

```bash
cd opencode2.4lunarwing
# The final --image-filesystem assert anchors the installed wrapper against
# the checkout's scripts/opencode-exposure-guard.sh digest (WI#357 GJ-3a,
# WI#425): a manual build must forward it or the image fails fail-closed.
# Recompute it after any wrapper edit:
#   sha256sum scripts/opencode-exposure-guard.sh
DOCKER_BUILDKIT=1 docker build \
  --build-arg LUNARWING_WRAPPER_SHA256=f2fc2ba6befc2d6d22dbae33029ab9a84ced4e6fa6927839ac636fdd42ca5b7b \
  -t lunarwing-worker-opencode2:latest .

# Select the worker per tenant, then build the shared image via mt-admin
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant <name> --with-opencode2
sudo ic/scripts/lunarwing-mt-admin.sh build-opencode2-worker

# Or build the shared image as part of a per-tenant build
sudo ic/scripts/lunarwing-mt-admin.sh build-tenant <name> --with-opencode2

# Include Rust, Go, and C/C++ compilers for build/test jobs
sudo ic/scripts/lunarwing-mt-admin.sh build-opencode2-worker --with-toolchains
```

CI publication matches the other workers: on protected release tags and
protected default-branch pushes, `worker-opencode2-candidate` builds the
image through `.gitlab/scripts/build-worker-image.sh opencode2` (same
reproducible-candidate + clean-rebuild digest contract), `worker-opencode2-scan`
runs the Trivy CRITICAL gate + SBOM, and `worker-opencode2-promote` publishes
the immutable `$CI_COMMIT_TAG-opencode2` tag. `ci-config` cross-checks the
`OPENCODE_VERSION`/`OPENCODE_REF` ARG defaults against the build script's
opencode2 case block — including its provenance `source_ref` — and the Rust
toolchain pin.

The opencode2 worker applies a checked build-profile patch to the pinned
upstream CLI script: `splitting: false` and `bytecode: false`. This avoids
split-chunk identifiers and ahead-of-time bytecode in the shipped executable;
it keeps upstream version injection, plugins, and artifact verification. Bun
parses the bundled source at startup instead, so startup time may increase.
The patch fails if either expected upstream setting is missing or duplicated,
requiring review when upgrading OpenCode. Executable bytes are not rewritten.

The started-entrypoint build smoke sets `WS_STATE_FILE` under
`/tmp/opencode2-runtime-smoke-entrypoint`, removed by its EXIT trap. The bridge
writes a timestamped WebSocket state file; leaving it at the default
`/tmp/lunarwing_ws_state.json` changed the runtime layer digest between the
candidate and clean rebuild (master pipeline #6658). Keep all smoke-generated
state inside the cleaned tree. The smoke creates a marker before starting the
entrypoint and fails closed if anything outside that tree under `/tmp` or
`/var/log/lunarwing` is newer when the checks finish, so a new residue path
fails the smoke step instead of surfacing only at the image digest comparison.

Image self-tests use `/tmp/opencode2-build-tests`, removed by an EXIT trap in
the same Docker `RUN`, and disable Python bytecode writes. The workspace-path
test also removes its own `oc2-ws-*` directory in `finally` for local runs.
These address the two reported sources of opencode2 digest drift: per-build
chunk IDs in the executable and leftover randomly named test workspaces.
The candidate versus uncached-rebuild **whole-image digest comparison remains
mandatory**; a passing binary check alone does not establish image reproducibility.

WI#510 records the v2.1.2 failure from master pipeline 6658: the smoke
residue was layer 29, but the Bun-compiled executable in layer 14 also differed
between candidate and rebuild. The two binaries had 3,293 differing bytes in
a table of per-build 64-bit IDs with stable `C0000xxxx` suffixes despite
identical source inputs. The checked `bytecode: false` / `splitting: false`
profile fixes the older split/bytecode ID class, but that does not establish
the cause of this intermittent recurrence. Two independently fresh local
v2.1.2 builds were byte-identical (`6c41398f...`, path-dependent, not a portable
checksum). WI#510 notes #12550/#12560 record matching binary and smoke layers
on master 6708; the binary cause remains open, not proven fixed by those
passes. Never mask bytes or weaken the whole-image digest contract.

Master 6708 exposed a third cause: candidate and rebuild crossed UTC midnight,
changing the account-creation layer's `/etc/shadow` last-change day. **All five
worker Dockerfiles now declare `ARG SOURCE_DATE_EPOCH=0` before runtime
`useradd` and assert in the same `RUN` that the account's shadow day equals
`SOURCE_DATE_EPOCH / 86400`.** Keep both the stage-local ARG and the assertion;
a later ARG (the old Kimi layout) cannot affect an earlier `RUN`. CI supplies
the source commit epoch; plain builds default to day zero. Shadow-utils encodes
day zero as an empty last-change field; the assertion treats that as zero but
still refuses a missing shadow entry. BuildKit's `rewrite-timestamp` normalizes
metadata, not this file content. The regression
suite `.gitlab/tests/test_worker_reproducibility.py` checks all five scopes and
both task/bridge accounts, executes the shadow assertions with
epoch-boundary/incorrect-day fixtures, and ties every snapshot to CI.
Protected whole-image candidate/rebuild, scan and exposure verification on the
changed tree remain required.

All five `DEBIAN_SNAPSHOT` defaults and `CI_DEBIAN_SNAPSHOT` now use
`20261105T082236Z`: its security indexes supply patched `libbrotli1`
`1.0.9-2+deb12u1` (Bookworm) and `1.1.0-2+deb13u1` (Trixie), fixing
CVE-2026-111497 without a new Trivy ignore. The three existing dated ignores
were re-verified against these package indexes and the Debian tracker; their
expiry dates are unchanged. This snapshot does **not** update Node's statically
bundled decoder or establish Bun's decoder status: WI#514 still owns those
base-runtime checks/upgrades and protected scan acceptance. Deployed tenants
need rebuilt images and recreated containers before any image fix applies.

The build-time self-test `RUN` steps also run under a throwaway
`HOME`/XDG environment rooted inside the trap-cleaned `/tmp/opencode2-build-tests`
tree, with `/root/.bun`, `/root/.local`, `/root/.cache`, and `/root/.config`
leak tripwires that fail the build. This is the work-item-#177 root cause fix:
OpenCode 2 appends a line stamped with the wall-clock time and a random run ID
(`timestamp=... run=...`) to `$HOME/.local/share/opencode/log/opencode.log` on
EVERY start — including `opencode --version` — so with the default root HOME
each build baked a unique log file into the runtime layer and the candidate
versus rebuild digests could never match (every opencode2 candidate job since
the lane landed failed the gate, e.g. pipelines #3862 and #3880; the v1 worker
is unaffected because opencode 1.x does not write that log). The compiled
executable itself was verified byte-identical across two patched-profile
builds, so the log was the only nondeterministic content in the image.

On a candidate/rebuild digest mismatch, `build-worker-image.sh` now also
prints a best-effort registry-side diagnosis before failing: it fetches both
pushed manifests with the job's registry credentials (busybox `wget` against
the GitLab `jwt/auth` token endpoint), reports both config digests and layer
counts, and diffs the layer digests — or, when all layers match, the two image
configs — pinpointing the first differing entry from the job log alone, which
is the layer-level comparison work item #177 called for.

Verification (2026-09-18, v2.0.5): built upstream commit
`79169fe966d58fb0e0a5e41133716184a0ab4ca6` with Bun 1.4.2 extracted from the
Dockerfile's pinned amd64 image, using `OPENCODE_VERSION=2.0.5 bun run build --
--single` in fresh build processes. Original-profile binaries differed in 3,295
bytes, including the reported random-prefix `C00000184`-style chunk keys.
Two patched-profile builds were byte-identical (`cmp`), SHA-256
`9f71bc51e6d778b0cceec8626a98d4815f3620e4f928358b403e4895f66baf69`.
This is host-build evidence, not a portable expected image/binary checksum.
The patched executable passed `--version` (`opencode v2.0.5`) and `serve --help`.

v2.0.8 bump (2026-09-19): upstream `packages/cli/script/build.ts` is
byte-identical between v2.0.5 and v2.0.8, so the checked build-profile patch
above applies unchanged; client/schema/protocol SDK changes in
`7673ed6bd6547ee0dcb81aab55f1392fb751d652` are additive against the bridge's
`@opencode/client/promise` usage. Whole-image reproducibility remains
CI-verified per revision.
(2026-10-11 correction, WI#329: this entry's "additive against the bridge's
`@opencode/client/promise` usage" claim was checked against protocol
identifier strings, not the generated promise client — the bridge then called
`session.message.list` / `session.permission.reply`, which never existed on
the promise client at v2.0.8 (WI#320 F7 observed the runtime TypeError at
v2.0.21). Both call paths were corrected in the v2.0.23 entry below.)
Bridge executor, result, workspace-path, build-profile, and study-notes tests
passed; workspace cleanup was also checked with an injected assertion failure.
Full image digest verification remains with CI: local Podman could not unpack
the base image because this account lacks sufficient subordinate UID/GID mappings.

v2.0.11 bump (2026-09-20): upstream `packages/cli/script/build.ts` is
byte-identical between v2.0.8 and v2.0.11, so the checked build-profile patch
above applies unchanged; client/schema/protocol SDK changes in
`9eb6902aaf3c35ce985b67c605a775992249066b` are additive against the bridge's
`@opencode/client/promise` usage — `OpenCode.make`, `session.interrupt`,
`session.execution.{succeeded,failed,interrupted}`, `session.idle`,
`session.message.list`, and `session.text.delta` all confirmed present.
Whole-image reproducibility remains CI-verified per revision.
(2026-10-10 correction, WI#329: this entry's `session.message.list`
"confirmed present" claim was checked against protocol/OpenAPI identifier
strings, not the generated promise client the bridge imports — `session.message`
exposes only `get`, so the method never existed at v2.0.11 (WI#320 F7 observed
the runtime TypeError at v2.0.21). The call path was corrected in the v2.0.23
entry below.)

v2.0.14 bump (2026-09-22): tag SHA `08462140ec0de1e4b17d4a353d8d5827f53cf7b0`
verified against both anomalyco/opencode and sst/opencode remotes. Upstream
`packages/cli/script/build.ts` is byte-identical between v2.0.11 and v2.0.14,
so the checked reproducible-build patch applies unchanged. The generated
promise client (`packages/client/src/promise/generated/client.ts`) is
byte-identical between the two tags; the only client/schema/protocol diffs are
an additive `method: "key" | "oauth"` field on `Connection.CredentialInfo` and
the version bumps — no removals against the bridge's `@opencode/client/promise`
usage (`OpenCode.make`, `session.create/prompt/interrupt`, `session.message.list`,
`session.permission.reply`, `session.execution.*`, `session.idle`,
`session.text.delta` all present at v2.0.14). The promise exports map is
unchanged. Whole-image reproducibility remains CI-verified per revision.
(2026-10-10 correction, WI#329: this entry's `session.message.list` /
`session.permission.reply` "present at v2.0.14" claim was checked against
protocol identifier strings, not the generated promise client the bridge
imports; neither method existed — `session.message` exposes only `get` and
there is no `session.permission` group at all (WI#320 F7). Both call paths
were corrected in the v2.0.23 entry below.)

v2.0.15 bump (2026-09-23): tag SHA `6f3639d82ed0760091792189b78f8eeb44f699b1`
verified against anomalyco/opencode. The upstream CLI build script and
`@opencode/client/promise` exports map are unchanged from v2.0.14, so the
checked reproducible-build patch still applies. The generated promise client
now preserves base-URL path prefixes and throws declared HTTP errors as
`Error` objects; neither change alters the bridge's localhost root-base-URL
calls. The new session metadata event and update field are additive to the
bridge's session execution, message, and permission calls. Whole-image
reproducibility must still pass the CI candidate-versus-clean-rebuild gate.
(2026-10-11 correction, WI#329: this entry's "additive to the bridge's
session execution, message, and permission calls" claim vouched for the
bridge surface as checked against protocol identifier strings, not the
generated promise client — the bridge then called `session.message.list` /
`session.permission.reply`, which never existed on the promise client at
v2.0.15 (WI#320 F7). Both call paths were corrected in the v2.0.23 entry
below.)

v2.0.17 bump (2026-09-26): tag SHA `04f4b0610c79e3692f3b91bd7e4489c2c7010e4b`
verified against the anomalyco/opencode remote (this release's checklist item #2
moves the lane off the beta-frequent 2.0.15 pin). The Dockerfile patches the
upstream CLI build script at that SHA; its `prepare_reproducible_build`
self-test guards the patch contract against a 2.0.17 fixture during the image
build. Host-side `prepare_reproducible_build_test.py` passed, including the
untouched-failure contract. Whole-image reproducibility must still pass the CI
candidate-versus-clean-rebuild gate before promotion.

v2.0.23 bump (2026-10-06, WI#303, re-attempt of the rejected MR !1438): tag
SHA `0fd7e2829449b052abf0078666669302923d77af` verified live against the
anomalyco/opencode remote (`git ls-remote`, this date). The `oven/bun` base
digest and `DEBIAN_SNAPSHOT` were reconsidered and stay unchanged. The
build-profile patch contract was **executed** host-side against the pinned
`packages/cli/script/build.ts` fetched at that SHA: exactly one
`bytecode: true` / `splitting: true` each, `prepare()` flips both, and a
double-apply is rejected. The `prepare_reproducible_build` self-test fixture
string moved to 2.0.23 — note the fixture is synthetic: the self-test never
reads upstream `build.ts`, so it guards the patch logic, not the pin (the
executed fetch-and-patch check above is what covers the pin).

Bridge call-path correction (WI#320 finding F7, fixed in this bump): the
generated promise client has **no** `session.message.list` and **no**
`session.permission.reply` — verified against the pinned tree
(`packages/client/src/promise/generated/client.ts`): `session.message`
exposes only `get`; session message listing is the top-level
`client.message.list` (`GET /api/session/:sessionID/message`, a paginated
`{ data, cursor }` envelope with an optional server-side `type` filter), and
permission replies are the top-level `client.permission.reply`. Earlier bump
entries (and MR !1438) claimed `session.message.list` was "present/unchanged"
because they checked protocol/OpenAPI identifier strings, not the client the
bridge imports — the method did not exist at v2.0.21 either, where the runtime
logged `sdk.session.message.list is not a function` and the executor silently
fell back to streamed deltas. `opencode_task_executor.ts` now calls
`client.message.list` (unwraps `.data`, `type: "assistant"`, bounded at 5
pages of 200) and `client.permission.reply`. Paging follows the server
contract (`packages/server/src/handlers/message.ts`): `order` is sent on the
first page only (`desc`, so the bound keeps the newest messages), cursor pages
carry no `order` — the server rejects cursor+order with 400
`InvalidCursorError` — and a short page ends the listing because `cursor.next`
is returned for every non-empty page; the pages are reversed into
chronological order and a later-page failure keeps the pages already fetched.
The self-test mock mirrors the real client shape and that server contract, so
the stale paths and a cursor+order request both fail the suite (the first
re-attempt sent `order` on every page and lost the canonical output on every
real session; !1452 review AL-1), and the image build now constructs the
client and asserts `typeof`
for every method path the bridge calls (`session.create/prompt/interrupt`,
`permission.reply`, `event.subscribe`, `message.list`), so this drift class
fails the build instead of the runtime. (WI#329 follow-up, 2026-10-10: the
asserted paths now come from the single committed list
`opencode2.4lunarwing/scripts/sdk_surface.ts`, which the build gate runs as
`scripts/sdk_surface_assert.ts` and which also drives the executor
self-test's mock-parity check, so the two gates cannot diverge; the stale
v2.0.8/v2.0.11/v2.0.14/v2.0.15/v2.0.20 bump entries carry dated WI#329
corrections. 2026-10-11, !1512 review CY-1/CY-2/CY-3/CY-5: the self-test now
also binds the list to the executor itself — it extracts the real
`sdk.*(...)` call sites and `event.type === "..."` literals from
`opencode_task_executor.ts` and asserts set equality with
`BRIDGE_SDK_METHODS`/`BRIDGE_SDK_EVENTS`, closing the F7-class gap where an
unlisted swallowed call passed both gates; `mockMethodPaths` flattens at any
nesting depth; and the build gate additionally asserts every bridge event
name is still a literal in the pinned `@opencode` client/schema/protocol
source shipped in the image. 2026-10-17, WI#369
(DF-1/DF-2/DF-4 + CY-2/CY-3 residues): the event gate now asserts each
bridge event name is a member literal of the pinned generated `V2Event`
union (`EventSubscribeOutput = V2Event`, what `event.subscribe` actually
yields) instead of a corpus-wide substring — a literal surviving only in
the v1 compat schema no longer counts; the source extractors run on
comment/string-blanked text (prose can no longer cause misleading
mismatches), bind optional-call/non-null/cast receivers and the Yoda event
form, and scan every non-test `scripts/*.ts`; the self-test additionally
wraps the mock in a recording Proxy so every client leaf path the suite
actually touches (alias, cast, bracket, destructure, optional-call no-op,
helper parameter of any name) must be declared in the list; the mock now
also emits `session.idle` and `session.execution.interrupted`; committed
negative tests pin the recursive mock flatten and fail if either build-gate
assertion loop is removed; and the builder stage runs `tsgo --noEmit` over
the SDK-facing scripts against the pinned generated types
(`scripts/tsconfig.sdk-surface.json`), covering call argument and event
payload shapes (the old CY-2 residue).)

Verified host-side with bun: the executor self-test passes against the real
client shape and fails with the pre-fix executor; the build-time assertion
snippet ran against the real pinned v2.0.23 promise source (all six paths
present, both stale paths absent) and fails closed against a drifted stub.
Paging fix verified 2026-10-06 on a local `podman build` of this lane at
v2.0.23 (`WITH_TOOLCHAINS=false`; every build-time self-test green) by a smoke
inside that image against a live `opencode serve` and a loopback fake
OpenAI-compatible model: the executor returns the canonical text from one
`message.list` call, cursor pages without `order` are accepted, newest-first
paging matches the server's chronological order, and the pre-fix executor's
cursor+order page is rejected with `InvalidCursorError` on the real server.
**Not yet executed:** no pipeline can build the v2.0.23 image before merge
(candidate jobs exist only on protected refs), so before promotion the
`worker-opencode2-candidate` job on the first protected pipeline containing
this pin must run green and be cited on WI#303, and a WI#320-style runtime
smoke against the candidate image should re-confirm the corrected executor
paths end-to-end.

### v2.0.32 bump — WI#427 (2026-10-17; verified and tenant-tested 2026-10-18, item 71)

**Authorization:** the maintainer's completed upstream analysis and explicit
approval are recorded in [WI#427](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/427).
The tag was independently resolved with `git ls-remote` (and cross-checked via
the GitHub API) to `843a6c514e606b436a7fe713422ace0a93ad3d61` — a lightweight
tag resolving directly to the commit, same as prior pins. Both Dockerfile
defaults and the CI case block's version/ref pins move together. The pinned
Bun base image digest and Debian snapshot stay unchanged.

**Exposure inventory (v2.0.25–v2.0.32):** the following implementation
cross-check supplements that decision, using the pinned source diff from
`e7a34f09bfd9134dfade5a8ddb843f7030bc9a69` (re-verified against a fresh
v2.0.32 clone on this branch):

| Upstream surface | Worker decision / enforcement |
|---|---|
| OpenTunnel remote access (`0d07f915`, `53533fd0`, `19fb8918`): shared device identity, relay routes on random subdomains, reconnect/retry | Compile out `packages/cli/src/services/remote-tunnel.ts`. The three consumers remain `service-config.ts` → `ensure`, `server-process.ts` → `run`, and `commands/handlers/pair.ts` → `hostname`; the existing refusing stub applies unchanged. Both dynamic and type-only `@opentunnel/client/effect` imports disappear. The declared package stays in the upstream frozen lock; the built-binary marker scan remains mandatory. |
| `pair --remote`, `--yes` / `-y` consent (`38454c6e`), `service set remote true`; saved `remote: { route }` and `remoteAccepted` | Refused by the existing every-token `pair`/`service` wrapper and session/config denies. Tests explicitly cover remote pairing with non-interactive consent and the boot reset discarding both saved fields. No new listener/pairing CLI command outside the guarded surface was found. |
| Foreground serve, pairing addresses, remote URL advertisement; web-UI archive/compression changes | Keep explicit loopback serve and per-boot Basic auth. `serve --service` and non-loopback `--hostname` stay refused. Server-backed CLI commands still default to private servers via the boot-time `disabled: true` reset. |
| Session sharing / publication | `share: "disabled"` remains valid in `packages/schema/src/config.ts`; no additional publication switch was introduced. Keep the baked config and startup protections. |
| Remote MCP: capability negotiation, query-parameter compatibility and numeric timeouts (`fbf5f164`, `7b5f1bd9`, `a448d5b9`) | Existing explicitly configured outbound MCP clients remain supported. No new worker listener or automatic sharing default; these are not an egress boundary. |
| Auto-update: WinGet support and install-method detection changes (`d46588fe`, `e5a557ef`, `707de583`, `47e5e715`), embedded release highlights | `update: "disable"` remains a valid config literal, but it does not reach the updater (`readPolicy()` reads only the global config dir, never `OPENCODE_CONFIG` — run-158 KG-3). `OPENCODE_DISABLE_AUTOUPDATE=1` in the image env silences the background check (the switch the updater actually honours), and the wrapper refuses the manual `upgrade`/`update` command (KG-1) — including `upgrade --method curl`, which would install an unpinned tunnel-capable upstream build at `$HOME/.opencode/bin`. Keep the pin and root-owned binary. WinGet is outside this Linux image. |
| Experimental voice API (`packages/core/src/voice.ts`, `protocol/src/groups/voice.ts`) | Adds authenticated outbound voice-session creation using caller-supplied input and provider credentials, not another listener. The bridge does not call it or provision voice credentials; no startup publication is enabled. WI#424 remains the boundary work for arbitrary outbound use. |
| External provider credentials and top-level integration/tool policies | Azure CLI credentials become external references and policy support is additive. No new exposure config is enabled by this lane; existing tool denies remain in force. |

**SDK and build compatibility:** the generated promise client's six bridge
methods remain present, with both stale nested paths absent — asserted on this
branch by running `scripts/sdk_surface_assert.ts` (bun 1.4.2) directly against
the pinned v2.0.32 client/schema/protocol source: 6 methods present, 2 stale
absent, all 8 bridge event names present. `session.create/prompt/interrupt`,
`permission.reply`, `event.subscribe`, and `message.list` retain the shapes
used by the executor (including cursor paging without `order`). The CLI still
honors `OPENCODE_CONFIG`/`OPENCODE_CONFIG_DIR` (`server-process.ts:118,127`)
and the injected password (`Env.password` reads `OPENCODE_PASSWORD` then
legacy `OPENCODE_SERVER_PASSWORD`, `env.ts:10-11`). `packages/cli/script/build.ts`
still carries exactly one `bytecode: true,` and one `splitting: true,`, so the
checked reproducibility patch applies unchanged.

**WI#369 deferral:** this bump proceeds without WI#369, as explicitly
permitted by WI#427 gate 6. The existing gate still uses syntax-based call
binding and corpus-wide event-literal matching; it is not a type/payload or
V2-union proof. Goals item 64 remains open.
**(Superseded 2026-10-18, item 64:** WI#369 landed on this tree — the builder
stage now runs a `tsgo --noEmit` gate over the SDK-facing scripts against the
pinned generated client types, and `sdk_surface_assert.ts` asserts the eight
bridge event names against the pinned `V2Event` union. The Gentoo
build/rollout recorded below ran through that strengthened gate.)

**Verification (this branch, 2026-10-18):**

- Compile-out guard executed against a fresh `--depth 1` clone at the pinned
  commit: stubbed `remote-tunnel.ts`, importers use exactly
  `run`/`ensure`/`hostname`, zero `@opentunnel/*` specifiers remain.
- Lane suites: all 5 Python suites pass (67 tests); all 4 bun TS self-tests
  pass. Repo-wide `.gitlab/tests` discovery: **322 passed, 4 skipped** (the
  four Pebble `dash` tests need a shell absent on this host).
  `ic/scripts/tests/test-mt-admin-opencode2.sh` passes in full.
- Full local Podman image build passes from this checkout
  (`mt-admin build-opencode2-worker`): compile-out stub applied in-build,
  binary marker scan clean, `opencode --version` asserted `opencode v2.0.32`,
  all embedded self-tests and both exposure-policy asserts green, leak
  tripwires clean. Image `6b92273222ad…` (revision `09c4cb7ae…`), slim
  profile; rebuilt from the tenant clone during `build-tenant`
  (`df42032e6c15…`, revision `4db886f7b…` — digest differs only by the
  source-revision label).
- **Real tenant job (item 71):** temporary test tenant `spitfire` (WI#273
  name, non-animal) provisioned on this host (Arch Linux, systemd 262) with
  `--with-opencode2`, running this branch. Job
  `cbf1c213-2d35-4f5a-a25c-e5031f3f9efa` drove the full path (gateway chat →
  LLM loop → `create_job` → approval gate → resume → WS dispatch → v2.0.32
  executor) to `result {"content": "Completed: OC2_WORKER_OK"}`. Live exposure
  checks before and after: `service.json` = `{"disabled": true}`, no
  `opentunnel` identity dir, wrapper refuses `service set remote true` and
  `pair --remote --yes` (exit 77), `worker-listeners spitfire` clean. Full
  security writeup: `docs/ops/ITEM71_OPENCODE2_V2.0.32_PUBLIC_HTTPS_TUNNEL.md`.
- **Gentoo rollout + real jobs (item 66, 2026-10-18, host: Gentoo/OpenRC):**
  fat image rebuilt from this tree into the admin store
  (`2ee1fe9c9f3e…`, revision `01f3e69aa…`) via
  `mt-admin build-opencode2-worker --with-toolchains` — builder tsgo gate,
  compile-out stub, in-image `sdk_surface_assert` (6 methods present, 2
  stale absent, 8 event names in the pinned `V2Event` union), all TS/Python
  self-tests, binary marker scan and image-filesystem assert green — then
  `restart-tenant` recreated both Gentoo opencode2 containers
  (`_ensure_tenant_image` save|load + contract-hash recreate, the !1663
  path). `soarin` job `c66fc949-96ae-4aeb-ab20-7f4e8d262b2b` →
  `result {"content": "Completed: … exact stdout: OC2_WORKER_OK"}`
  (bridge `success (19886ms)`); `scootaloo` job
  `953f47bb-817f-4e40-9dfb-48de6ee1613d` →
  `result {"content": "Completed: OC2_WORKER_OK"}` (bridge
  `success (28704ms)`, approval-gate `action=always` path exercised).
  Exposure checks on both tenants, before and after the jobs: in-container
  `opencode --version` = `opencode v2.0.32`, `service.json` =
  `{"disabled": true}`, no `opentunnel` identity dir, wrapper refuses
  `service set remote true` / `pair --remote --yes` / non-loopback
  `serve --hostname` (exit 77), `worker-listeners <tenant>` clean.
  scootaloo's lane was found unconfigured (no `OPENCODE2_BASE_URL`;
  pre-dates the WI#330 inheritance) — fixed with
  `configure-opencode2 scootaloo --base-url <tenant LLM endpoint>` before
  its job.

**Remaining acceptance / rollout:** protected `worker-opencode2-candidate`
(including the uncached whole-image digest comparison) and
`worker-opencode2-scan` must still pass on the bump commit before promotion;
an MR pipeline or local build does not satisfy those gates. Roll out via
`mt-admin build-opencode2-worker` then `restart-tenant <tenant>` to recreate
opencode2 worker containers. Verify no remote service config or OpenTunnel
identity, effective sharing disabled, wrapper refusal of
`service set remote true`, and `worker-listeners <tenant>` clean before
driving jobs. (Done 2026-10-18 for the Gentoo host's tenants `soarin` and
`scootaloo` — see the item-66 rollout bullet above; the Arch host's item-71
tenant `spitfire` ran the same checks at provisioning time.)

**Rollback:** preserve each tenant's worker state before its first v2.0.32
boot. Upstream adds the `project.time_archived` column and migrates Azure
CLI credential records from OAuth tokens to external credentials. An image
downgrade alone is not a verified database rollback; restore the matching
pre-upgrade state with the prior image if rollback is needed.

### v2.2.7 bump — WI#609, item 39 (2026-11-12)

**Review reconciliation and pins:** `git ls-remote` resolves lightweight
v2.2.7 directly to `fedc6f208aa2bd54c1019227214568083f6380ad`, the newest
`v2.*` tag, and the fresh clone's HEAD matches. Its 24-hour settling window
(ended 2026-11-11 10:37Z) had passed. npm reports `@moonshot-ai/kimi-code`
`latest` as **2.6.0**, published **2026-11-12 10:11Z** — inside its own
24-hour window at the time of this work — so the kimi lane takes **2.5.3**
(2026-11-10 13:12Z, window passed); 2.6.0 is the next bump, not this one.
Dockerfile defaults, the CI case blocks, the Kimi manifest and its lock moved
together. Regenerating the lock with
`npm install --package-lock-only --ignore-scripts` changed only the three
version fields and the dependency record: the tarball is
`https://registry.npmjs.org/@moonshot-ai/kimi-code/-/kimi-code-2.5.3.tgz`
with integrity
`sha512-NdBttZiktZhKdH1tu/25A+Il1MxO1RTE1gAdZDmogoCT4g8L7k3pcLeJpwy7PNJvHo9CF8fDJiaZ9nsIPn+Gag==`.
Node/Bun base digests and `DEBIAN_SNAPSHOT=20261105T082236Z` are unchanged.
The v1.19.5 pin was re-checked the same day: `v1.19.5` is still the newest
`v1.*` tag and resolves to `810c0eba1a7348036f8bdf61e6632d0c36c6876c`, the
committed pin — no v1 change.

**Kimi 2.5.3 verification:**

- The new steering surface (`POST /api/v1/sessions/{session_id}/steer`) and
  the Web UI / Windows MCP fixes are outside this subprocess lane: it uses
  print mode only (`-p <prompt> --output-format stream-json`, pinned by the
  argv regression) and starts no kimi server, so no new instruction channel
  exists. The four real 2.5.3 contract cases pass against the installed CLI
  in the pinned Node shape: headless invocation accepted with the
  `system.version` / `turn.step.retrying` meta stream unchanged, `--yolo`
  rejected alongside `-p`, baked `[[hooks]]` PreToolUse guard accepted, and
  the MCP NDJSON initialize/tools-list handshake with parent-env pass-through.
  The WI#206 fail-closed hook is unchanged.
- `engines.node` (`>=22.19.0`) and the runtime dependencies (`ws`, `qrcode`,
  `chokidar`) are identical to 2.5.2, so the pinned Node 22 base still
  satisfies the CLI.

**Gate re-verification on the fresh v2.2.7 clone:**

- compile-out: `packages/cli/src/services/remote-tunnel.ts` keeps its path and
  exact `run`/`ensure`/`hostname` importer surface; the real tree stubs
  cleanly (exit 0), and after stubbing no scanned source under
  `packages/{cli,server,schema,core}/src` retains an `@opentunnel` specifier.
  `@opentunnel/client` remains declared but unimported. The diff from v2.2.6
  adds no tunnel, pairing, share or relay file under
  `packages/{cli,server,schema}/src`; the one matching path
  (`commands/handlers/auth/shared.ts`) is credential plumbing.
- reproducible build: `packages/cli/script/build.ts` retains the pinned
  `--single` / `bytecode: true` / `splitting: true` anchors and
  `prepare_reproducible_build.py` exits 0. Bun 1.4.4 and the
  content-equivalent photon repair are unchanged, so the retired fixer stays
  removed.
- SDK surface: the generated client keeps all 6 bridge methods
  (`session.create/prompt/interrupt`, top-level `permission.reply` beside
  `permission.request.*`, `event.subscribe`, top-level `message.list`); both
  known-stale paths stay absent (`session.message` exposes only `get`, and
  `session.form.reply` is not `session.permission.reply`), and all 8 bridge
  event names remain literals of the generated `V2Event` union — the
  `sdk_surface_assert.ts` event gate run against the real v2.2.7
  `generated/types.ts` exits 0. The typecheck compiler pin is unchanged:
  `@typescript/native` is still `npm:typescript@7.1.0-beta` with `bin/tsc`.
- SDK timing change (listed as breaking): the bridge calls none of
  `agent.list`, `integration.list`, `usage.limits`, `vcs.init`, `rpc.call` or
  `artifact.*` (grep over every non-test `scripts/*.ts`), so the removal of
  their up-to-five-second startup wait is a no-op for this lane.
- prompt cache warming: the `opencode.warming` plugin requires an explicit
  top-level `warming` setting or a model/provider `cache.warming`; neither is
  present in `config/opencode2.json`, so `Decision.Off()` applies and the lane
  sends no background keep-alive requests. Nothing was turned off to get
  there; `warming` is unset in the lane config. `interval: "auto"` only
  refines timing when an operator enables warming.
- shell output format: the new `Exit code: N` / timeout tail is produced in
  `packages/core/src/shell/result.ts` as tool-output text. The executor reads
  assistant messages (`message.list`) and event types, never the textual tail
  of a shell result, and no fixture in
  `scripts/opencode_task_executor_test.ts` matches the old tail.
- `@opencode/ai`: the lane imports only `@opencode/client/promise`; the
  removed `ImageResponse.image` cannot reach it.
- no new opt-ins: `OPENCODE_FLAGS` stays unset; no MCP `pool: true`,
  `experimental.all_flags`, `experimental.codemode_vm`, or `websearch.use`
  entry is added. MCP login, Copilot routing, Console policy loading and
  `opencode auth logout` changes are unused by the lane; the every-token
  wrapper and boot-time `service.json` reset remain the exposure controls.

**Local gate results (`taskset -c 0-17`):** opencode2 Python suites
compile-out **10/10**, reproducible build **3/3**, exposure policy **68/68**,
image config **11/11**, entrypoint config **11/11**, runtime smoke **50/50**;
lane Bun suites executor, result, path, SDK surface + negative test, tool
proxy, tool plugin, bridge and task runner **7/7** pass. The kimi real-CLI
contract lane **4/4** passes against the installed 2.5.3 binary.

**Not performed locally:** no full Kimi/OpenCode2 image build
(`mt-admin build-kimi-worker` / `build-opencode2-worker`) and therefore no
in-image `opencode --version`/binary marker scan, candidate-vs-clean-rebuild
digest comparison, protected `worker-*-candidate`/scan jobs, or
`worker-exposure-policy-guard` CI job run on this branch. Those remain the
protected pipeline's gates after push/merge. No tenant rollout or live-model
dispatch was performed.

### v2.2.6 bump — WI#582, goal 24 re-attempt (2026-11-08)

**Review reconciliation and pins:** this re-attempt combines approved !2277
with !2274's unique CLI-mode bound, resolving the XN-2 overlap without dropping
either regression path. XN-1 stays fixed by the reviewed 2000 ms CLI test
budget. The WI#427 OpenTunnel boundary controls were re-run against fresh
upstream artifacts. `npm view @moonshot-ai/kimi-code dist-tags` still names
**2.5.2** as `latest` (the 2.6.0 canary is not stable), and `git ls-remote`
resolves lightweight v2.2.6 directly to
`dc2ffcf8bf26f9e669b6aa22b95f50e12be15e06`. The fresh clone's HEAD matches.
Dockerfile defaults, CI case blocks, the Kimi manifest and its lock moved
together. Regenerating the lock with
`npm install --package-lock-only --save-exact --ignore-scripts` produced no
drift from the reviewed pin: the tarball is
`https://registry.npmjs.org/@moonshot-ai/kimi-code/-/kimi-code-2.5.2.tgz` with
integrity `sha512-a7UuugqQuPBHB2ZMiu8ezAFIwJ0dn6xanuRnTqa2pXXu12ng7kvigAlXy47TxNKpnyj8vgaNStedTv4kbqSCuA==`.
Node/Bun base digests and `DEBIAN_SNAPSHOT=20261105T082236Z` are unchanged.

**Kimi 2.5.2 verification:**

- The removed server APIs are outside this subprocess lane: it uses print mode,
  not WebSocket subscription/replay, snapshots, REST task/status endpoints, or
  transcript `meta`/`todos`. The four real 2.5.2 contract cases pass in the
  pinned Node 22 environment: headless `-p --output-format stream-json`,
  `--yolo` rejection, baked PreToolUse hook acceptance, and MCP NDJSON
  initialize/tools-list with parent-env pass-through. Print-mode meta events
  retain `system.version` and `turn.step.retrying`.
- Creation-time SSH/Docker/custom-command environments stay unselected: the
  WI#582 argv regression pins exactly `-p <prompt> --output-format stream-json`
  with no `--environment`/`--environment-cwd`. The generated config does not
  set `[workspace] require_trust = false`, and the existing trust-env scrub
  remains. The lane neither enables nor relies on the default-off global
  full-text index. `chokidar`, `qrcode`, `ws`, and `engines.node >=22.19.0`
  remain unchanged.
- This re-attempt built local image `b5c9ca168e9a…` with the locked CLI. Its
  full in-image suite finished **183 pass / 3 expected root-only skips / 0
  fail**. The image reports `kimi --version` = **2.5.2**, `/health` carries
  `"kimi_code_version": "2.5.2"` through `KIMI_CODE_IMAGE_VERSION`, and the
  started bridge reaches `/ready` = true. This is a local candidate, not the
  protected CI candidate/rebuild pair.

**Gate re-verification on the fresh v2.2.6 clone and binary:**

- compile-out: `packages/cli/src/services/remote-tunnel.ts` keeps its path and
  exact `run`/`ensure`/`hostname` importer surface. After stubbing, no scanned
  source under `packages/{cli,server,schema,core}/src` retains an
  `@opentunnel` specifier; `@opentunnel/client` remains declared but
  unimported. This re-attempt built the patched binary, verified `opencode
  --version` = **opencode v2.2.6**, and passed the real no-OpenTunnel marker
  scan and lane exposure assertions. The every-token wrapper forwards
  `--version` and refuses `service set remote true`, remote pairing, and
  non-loopback serving with exit 77. The earlier !2277 local image
  (`fc8839ef053d…`) also passed the started-entrypoint runtime smoke **5/5**;
  this re-attempt did not repeat a full OpenCode2 image build.
- reproducible build: `packages/cli/script/build.ts` retains `--single` and
  exactly one `bytecode: true` and `splitting: true` anchor;
  `prepare_reproducible_build.py` exits 0 and the patched build succeeds. Bun
  1.4.4 and the content-equivalent photon whitespace repair remain unchanged,
  so the retired fixer stays removed.
- SDK surface: the real client has all 6 bridge methods
  (`session.create/prompt/interrupt`, `permission.reply`, `event.subscribe`,
  top-level `message.list`), both known-stale paths remain absent, and all 8
  bridge event names are in the generated `V2Event` union. The union has 97
  literal members plus the `V2EventRpc` dynamic family. The builder-shaped
  `@typescript/native` (`npm:typescript@7.1.0-beta`, binary `bin/tsc`)
  typecheck and the real-client assertion both exit 0.
- background waits: upstream's `background_wait` is CLI JSON stdout NDJSON,
  not a serve `V2Event`, so it is deliberately absent from
  `BRIDGE_SDK_EVENTS`. The serve regression injects its shape behind a pending
  prompt and proves `timed_out` after 10 ms with one `session.interrupt`. The
  optional CLI `run` path now uses `OPENCODE_RUN_TIMEOUT_MS` (default 3600000)
  through GNU `timeout --preserve-status -k 5s`, preserving the WI#373
  credential scrub. Its real-entrypoint regression emits the upstream NDJSON
  shape and proves the 2000 ms deadline reaches SIGTERM with exit 143; zero,
  negative, fractional, and non-numeric deadlines are refused.
- new opt-ins stay off: `OPENCODE_FLAGS` is unset; no MCP `pool: true`,
  `experimental.all_flags`, `experimental.codemode_vm`, or `websearch.use`
  entry is present. Download-oriented file/media responses do not intersect
  the bridge's six-method SDK surface. `session_move` now authorizes external
  destinations; `session_rename` remains unguarded, so both explicit deny rules
  stay. No new share, relay, pairing, or public-listener surface was found.
- upstream suites under pinned Bun 1.4.4: noninteractive **22/22** (including
  background-job cancellation), file/media downloads **7/7**, and CLI/core flag
  suites **26/26** pass. Lane executor, SDK, result, path, proxy, plugin,
  bridge, and task-runner suites pass. Python suites pass: compile-out
  **10/10**, reproducible-build **3/3**, exposure policy **68/68**, image
  config **11/11**, entrypoint config **11/11**, runtime smoke **50/50**, and
  study-notes config **8/8**. `check-worker-exposure-policy.sh` passes both
  OpenCode lanes.
- repository gates: `.gitlab/tests/test_goal1_ci.py` **283/283**,
  `test_worker_build_args.py` **16/16**, both Kimi and OpenCode2 mt-admin
  harnesses, CI validation **8 YAML**, Python syntax **148 files**, shell
  syntax **283 scripts + three hooks**, repository ShellCheck, and
  `git diff --check` pass. Executable fixtures used short executable temp
  directories because this host mounts `/tmp` `noexec` and Unix socket paths
  are length-bounded.

**Protected and rollout gates not performed locally:** protected
`worker-{kimi,opencode2}-candidate` and scan jobs, OpenCode2 exposure verify,
the candidate-vs-uncached-clean-rebuild digest comparisons, and a green
post-merge master pipeline remain CI requirements. No tenant rollout or
live-model daemon-dispatched job was performed.

### v2.2.5 bump — WI#531, item 30 (2026-11-02)

**Authorization:** the WI#427 boundary crossing (OpenTunnel compile-out,
wrapper, config and image assertions) remains the standing control set for
every v2 pin; this bump re-verified each gate against the pinned source
rather than trusting the previous pin's review. The tag was independently
resolved with `git ls-remote` to
`9bbb8ce18c80f099e0e448775f59d965ead40a21` (lightweight tag resolving
directly to the commit; the `--depth 1` clone's HEAD matches). Dockerfile
defaults and the CI case block moved together. The Bun base digest and
`DEBIAN_SNAPSHOT` are unchanged (WI#361 owns those).

**Photon-node repair retired.** v2.2.5 carries the `61845b0` whitespace
repair as a content-equivalent backport, so
`fix_photon_patch_context.py` fails its exact-match guard with "already
carries the fixed whitespace" — the documented retirement signal. Removed in
this bump: both `scripts/fix_photon_patch_context{,_test}.py` files, the
Dockerfile fixer `COPY`/`RUN` before `bun install`, its runtime self-test
invocation, and this lane's two `input_files` entries in
`.gitlab/scripts/build-worker-image.sh`. Bun was not downgraded and the
drift guard was not weakened.

**Gate re-verification on the fresh v2.2.5 clone:**

- compile-out guard: `packages/cli/src/services/remote-tunnel.ts` unchanged
  in path and importer surface (`run`/`ensure`/`hostname` exactly); after
  stubbing, no scanned source under `packages/{cli,server,schema,core}/src`
  keeps an `@opentunnel` specifier. `@opentunnel/client` stays
  declared-but-unimported, and the runtime binary marker scan remains
  mandatory.
- reproducible-build anchors: `script/build.ts` still sets `bytecode`/`splitting`
  and takes `--single`; the patch script exits 0 on the pinned tree.
- SDK surface (`sdk_surface_assert` against the pinned client): 6 bridge
  method paths present (`session.create/prompt/interrupt`, `permission.reply`,
  `event.subscribe`, top-level `message.list`), the 2 known-stale paths still
  absent (`session.message` exposes only `get`; `permission` is a top-level
  group), and all 8 bridge event names are member literals of the pinned
  generated V2Event union (98 members; only `V2EventRpc` is a dynamic
  template family and is correctly not gateable).
- builder-stage WI#369 typecheck gate: **version move, same binary** —
  upstream's compiler pin is `@typescript/native` (npm:typescript@7.1.0-beta
  at this pin, was 7.0.2 at v2.1.2). Its bin is still `tsc` at `bin/tsc`, so
  the Dockerfile invocation is unchanged; `tsc --noEmit -p
  tsconfig.sdk-surface.json` exits **0** against the pinned types.
- behavior pins: Code Mode `session_move`/`session_rename` still carry no
  permission assert of their own (KL-1/KL-2 deny rules stay required);
  `service.json` keeps the `{disabled, remote: {route}, hostname, port,
  password, cors, env}` shape with the legacy boolean-`remote` reader, so the
  boot reset stays valid; `pair --remote` consent still exists and the
  wrapper refusals cover it; `updater.ts` still honours
  `OPENCODE_DISABLE_AUTOUPDATE` (the live switch — the lane config's
  `update: "disable"` is not read by `readPolicy()`, which only reads the
  global config dir). The exposure wrapper still refuses
  `service set remote true` with exit 77.
- new binary input: v2.2.5 replaces the separate `@opencode-ai/pty` 0.3.1
  package with the in-tree `packages/process-daemon` (the Rust `opencode-pty`
  0.4.0 service), shipped through npm optional platform packages
  (`@opencode/pty` 0.4.0). It listens on a private per-uid Unix socket under
  `/tmp/opencode-pty-<uid>/` with a token from a 0600 registration file and
  opens no inbound network path, so the runtime listener check is unaffected.
  The candidate/clean-rebuild digest comparison covers the new binary input.
- in-host lane suites: `compile_out_remote_tunnel_test` 10/10,
  `exposure_policy_assert_test` pass, `sdk_surface{,_assert}_test` pass.

**Not verified here (honest gaps):** the bridge/executor/plugin suite needs
the `@opencode/client` workspace link that only a full Bun 1.4.4 image build
creates; this host runs Bun 1.3.1, so `opencode_task_executor_test.ts` fails
to resolve `@opencode/client/promise` and was not counted as evidence. The
protected `worker-opencode2-candidate` digest comparison,
`worker-opencode2-scan`, and `worker-opencode2-exposure-verify` jobs remain
the acceptance gate, followed by `mt-admin build-opencode2-worker` +
`restart-tenant` and the live listener audit. No image has been built from
this pin yet.

### v2.1.2 bump — item 83 (2026-10-21)

**Authorization:** the WI#427 boundary crossing (OpenTunnel compile-out,
wrapper, config and image assertions) remains the standing control set for
every v2 pin; this bump re-verified each gate against the pinned source
rather than trusting the previous pin's review. The tag was independently
resolved with `git ls-remote` to
`a517687fde98a7329ada3ae644a01a9fc6c4e99e` (lightweight tag resolving
directly to the commit, same as every prior pin; the fresh clone's HEAD
matches). Dockerfile defaults, the CI case block, and the CI validator
fixtures moved together. The Bun base digest and `DEBIAN_SNAPSHOT` stay
unchanged (WI#361 owns those).

**Gate re-verification on the fresh v2.1.2 clone:**

- compile-out guard: `packages/cli/src/services/remote-tunnel.ts` unchanged
  in path and importer surface (`run`/`ensure`/`hostname` exactly);
  `@opentunnel/client` stays declared-but-unimported after stubbing — same
  shape as v2.0.32, and the runtime binary marker scan remains mandatory.
- reproducible-build anchors: `script/build.ts` still sets `bytecode`/`splitting`
  and takes `--single`; the patch script exits 0 on the pinned tree.
- SDK surface (`sdk_surface_assert` against the pinned client):
  6 bridge method paths present (`session.create/prompt/interrupt`,
  `permission.reply`, `event.subscribe`, top-level `message.list`),
  the 2 known-stale paths still absent (`session.message` exposes only
  `get`; `permission` is a top-level group), and all 8 bridge event names
  are member literals of the pinned generated V2Event union.
- builder-stage WI#369 typecheck gate: **binary rename** — v2.0.32's
  `@typescript/native-preview` shipped the native compiler as `tsgo`;
  v2.1.2 pins `@typescript/native` (npm:typescript@7.0.2) whose bin is
  `tsc`. The Dockerfile gate invocation moved with the pin and
  `tsc --noEmit -p tsconfig.sdk-surface.json` exits 0 against the pinned
  types (verified on the clone with a full `bun install`).
- behavior pins: the build-agent permission defaults still ship only
  `* allow` plus the `.env` asks (the KG-2 external_directory restore stays
  required); Code Mode `session_move`/`session_rename` still carry no
  permission assert of their own (KL-1/KL-2 deny rules stay required);
  `service.json` keeps the `{remote: {route}, remoteAccepted}` shape and
  the boot reset stays valid; `pair --remote` consent still exists and the
  wrapper refusals cover it; `updater.ts` still honours
  `OPENCODE_DISABLE_AUTOUPDATE` (`inspect()` gate reads it at v2.1.2).
- **new upstream surfaces at v2.1.2, looked at (the !1831 review's MG-3):**
  `SkillScan` (`packages/core/src/config/plugin/skill-scan.ts`, new)
  replaces v2.0.32's glob with an explicit breadth-first walk of the project
  tree to depth 6 collecting `SKILL.md` files plus depth-0 `.md` files and
  skipping dot-directories — in this lane the project tree is the
  task-writable workspace, so everything it discovers is task-authored
  input to the serve process, the trust class the lane already records
  (the EF-10 workspace-shadowing note); no new exposure. The Code Mode
  `todo` plugin tool (`packages/core/src/tool/plugin/todo.ts`, new, name
  `todowrite`) ships disabled behind `experimental.todo === true` — the
  lane config does not set it, so it stays dormant unless task-authored
  workspace config enables it, and unlike `session_move`/`session_rename`
  it carries its own `permission.assert` (`resources: ["*"]`,
  `save: ["*"]`), so it needs no extra deny rule; task-scoped todo state
  is not an exposure surface.
- in-host lane suites against the pinned client: bridge, executor, plugin,
  proxy, sdk_surface (x2), result, path, prompt self-check suites pass;
  `compile_out`, `entrypoint_config`, `exposure_policy_assert`,
  `prepare_reproducible_build`, and `study_notes_config` python suites pass;
  `.gitlab/tests` 234/234 and `validate_config.py` clean.

**Remaining acceptance / rollout:** unchanged from the v2.0.32 entry — the
protected `worker-opencode2-candidate` (whole-image digest comparison) and
`worker-opencode2-scan` jobs must pass on a protected pipeline before
promotion, then `mt-admin build-opencode2-worker` + `restart-tenant` per
tenant with the same live exposure checks (service.json reset, wrapper
refusals, `worker-listeners` clean). **Blocked at the pin's landing (the
!1831 review's MG-1):** the instance's `throttle_unauthenticated_web` rate
limit is blocking the registry push of every worker candidate build since
pipeline 6151 (the !1828 review's MD-1), so no image has been built from
this pin yet — the candidate/scan gate runs only once that limit is lifted
(maintainer action, not in-tree), and until then tenants keep their current
v2.0.32 images. Tenant rollouts are recorded below as they happen.

v2.0.24 bump (2026-10-07, v2.1.5.0 checklist item #2): tag SHA
`e7a34f09bfd9134dfade5a8ddb843f7030bc9a69` verified live against the
anomalyco/opencode remote (`git ls-remote`, this date; a lightweight tag
resolving directly to the commit, same as v2.0.23). The `oven/bun` base
digest and `DEBIAN_SNAPSHOT` were reconsidered and stay unchanged. The
v2.0.23→v2.0.24 upstream diff is large (~1036 files) but the lane's surface
is unchanged, re-verified against the pinned tree itself (not protocol
identifier strings): the generated promise client still has all six bridge
call paths (`session.create/prompt/interrupt`, `permission.reply`,
`event.subscribe`, top-level `message.list`); `session.message` still
exposes only `get` and there is still no `session.permission.reply`;
`message.list`'s query shape and envelope are unchanged (`limit` capped at
200 by the `SessionMessagesQuery` schema, `order`/`cursor`, the server-side
`type` filter including `assistant`, `{ data, cursor: { previous?, next? } }`),
and the server handler still rejects cursor+order with 400
`InvalidCursorError`; every event name the bridge matches
(`session.execution.{succeeded,failed,interrupted}`, `session.idle`,
`permission.asked`, `session.text.delta`, `session.tool.{success,failed}`)
is unchanged; `SessionCreateInput` still takes `{ title, permissions }`,
`SessionPromptInput` `{ sessionID, text }`, and `SessionCreateOutput` still
unwraps `.data`; `OPENCODE_CONFIG` is still honored
(`packages/cli/src/server-process.ts`); and the `@opencode/client` exports
map (`./promise` → `src/promise/index.ts`) plus the promise `make` shape
(raw groups spread, `event` wrapped by `SharedEvents` whose
`subscribe({ signal })` the executor uses) are unchanged. The visible
upstream drift is internal to the lane: a promise service-discovery
refactor (`service-probe.ts` new, `service-contender.ts` and
`promise/service.ts` reworked — the bridge constructs the client with an
explicit `baseUrl` and never calls `ensure`/`discover`) and an ACP-mode
internals rework (the lane runs websocket mode). `packages/cli/script/build.ts`
is byte-identical between the two tags; the build-profile patch contract was
still executed host-side against the pinned file (exactly one
`bytecode: true` / `splitting: true` each, `prepare()` flips both, a
double-apply is rejected), and the moved upstream `bun.lock` is consumed by
the Dockerfile's `bun install --frozen-lockfile` from the tag itself. The
`prepare_reproducible_build` self-test fixture string moved to 2.0.24 — the
fixture is synthetic (it guards the patch logic, not the pin; the executed
fetch-and-patch check above covers the pin). Host-side
`prepare_reproducible_build_test.py` passed (3/3) and the ci-config
`WorkerVersionPinTests` passed (15/15) against the new pins. **Not yet
executed:** no pipeline can build the v2.0.24 image before merge (candidate
jobs exist only on protected refs), so before promotion the
`worker-opencode2-candidate` job on the first protected pipeline containing
this pin must run green (whole-image candidate-versus-clean-rebuild digest
gate), and a runtime smoke against the candidate image should re-confirm
the executor paths end-to-end.

v2.0.21 bump (2026-10-01): tag SHA `8a8bd622a3d7dc29ccf30ec17f84e363ed95ed72`
verified against the anomalyco/opencode remote (v2.1.4.0 checklist item #2
repass). The Dockerfile patches the upstream CLI build script at that SHA
(exactly one `bytecode: true` / `splitting: true` each, so the patch contract
holds); the `prepare_reproducible_build` self-test fixture moved to 2.0.21 to
keep guarding the patch contract during the image build. The v2.0.20→v2.0.21
diff is additive for this lane's surface: `session.create` (with
`permissions`), `session.message.list`, `session.permission.reply`,
`session.interrupt`, `event.subscribe`, `OpenCode.make`, and every event name
the bridge matches (`session.execution.{succeeded,failed,interrupted}`,
`session.idle`, `permission.asked`, `session.text.delta`,
`session.tool.{success,failed}`) are unchanged — the only client/protocol
diffs are an additive optional `message` query param on `session.form.cancel`
(and matching optional `FormState.cancelled.message` /
`SessionFormCancelInput.message` generated-type fields), which the bridge
does not use. Host-side `prepare_reproducible_build_test.py` passed (3/3),
including the untouched-failure contract. Whole-image reproducibility must
still pass the CI candidate-versus-clean-rebuild gate before promotion.
(2026-10-06 correction: this entry's `session.message.list` /
`session.permission.reply` "unchanged" claim was checked against protocol
identifier strings, not the promise client the bridge imports; neither
method existed at v2.0.21 — WI#320 F7 observed the runtime TypeError. Both
call paths were corrected in the v2.0.23 bump above.)

v2.0.20 bump (2026-09-30): tag SHA `84c9be93a56304a108f1a22df0c5d62c26d5b6ca`
verified against the anomalyco/opencode remote (v2.1.4.0 checklist item #2).
The Dockerfile patches the upstream CLI build script at that SHA (exactly one
`bytecode: true` / `splitting: true` each, so the patch contract holds); the
`prepare_reproducible_build` self-test fixture moved to 2.0.20 to keep guarding
the patch contract during the image build. The v2.0.18→v2.0.20 diff is
additive for this lane's surface: `session.create` (with `permissions`),
`session.message.list`, `session.permission.reply`, `session.interrupt`,
`event.subscribe`, `OpenCode.make`, and every event name the bridge matches
(`session.execution.{succeeded,failed,interrupted}`, `session.idle`,
`permission.asked`, `session.text.delta`, `session.tool.{success,failed}`) are
unchanged — the new `credential.list`/`credential.create` client endpoints and
additive generated-type fields (e.g. `SessionStructuredError.response`,
`ConnectionStatus`) are not used by the bridge. Host-side
`prepare_reproducible_build_test.py` passed (3/3), including the
untouched-failure contract. Whole-image reproducibility must still pass the CI
candidate-versus-clean-rebuild gate before promotion.
(2026-10-10 correction, WI#329: this entry's `session.message.list` /
`session.permission.reply` "unchanged" claim was checked against protocol
identifier strings, not the generated promise client the bridge imports;
neither method existed at v2.0.20 (WI#320 F7 observed the runtime TypeError at
v2.0.21). Both call paths were corrected in the v2.0.23 entry above.)

v2.0.18 bump (2026-09-27): tag SHA `cd9a14a6b688d4021bee381dfd39d2cef9c0f862`
verified against the anomalyco/opencode remote (v2.1.3.0 checklist item #2).
The Dockerfile patches the upstream CLI build script at that SHA; the
`prepare_reproducible_build` self-test fixture moved to 2.0.18 to keep guarding
the patch contract during the image build. Host-side
`prepare_reproducible_build_test.py` passed (3/3), including the
untouched-failure contract. Whole-image reproducibility must still pass the CI
candidate-versus-clean-rebuild gate before promotion.

Goal-35 verification (2026-09-18): the #177 root cause was reproduced
host-side with the pinned Bun — two full self-test sequences with a scratch
HOME produced identical trees except `~/.local/share/opencode/log/opencode.log`
(unique `timestamp=`/`run=` values), while the patched-profile CLI binaries
built back-to-back were byte-identical (`80e50218...`), bun's transpiler caches
were byte-identical, and the base image disables bun's runtime caches
(`BUN_RUNTIME_TRANSPILER_CACHE_PATH=0`). The fixed `RUN` shape (throwaway
HOME/XDG + trap) ran the full self-test sequence twice leaving zero files
anywhere, so nothing per-run can reach the image layer. The mismatch-diagnosis
helper was exercised inside a sandbox running the pinned buildkit image's
busybox: token-failure, layer-diff, and config-diff paths all degrade or
report correctly. The definitive candidate-vs-rebuild gate rerun happens on
the next protected master pipeline (container jobs only exist on protected
refs).

Per-tenant overrides: `configure-opencode2 <name> [--model <m>]
[--base-url <url>]` (written as `OPENCODE2_MODEL`/`OPENCODE2_BASE_URL`).
Ports come from the dedicated registry range 51000-51999
(`opencode2_wss`/`opencode2_health`, ports-registry v17); units are
`lunarwing-opencode2-<tenant>` (systemd quadlet + OpenRC `-sup`).
Agent-facing dispatch uses `create_job(mode: "opencode2")`; the daemon
side is fully generic (one more `[[sandbox.external_workers]]` entry —
zero Rust changes).

Workspace confinement mirrors the kimi/pebble workers: the bridge
resolves the daemon-supplied `project_dir` against `WORKSPACE_ROOT`
(canonicalized; `..` and symlink escapes are rejected with a
`task_result` error envelope before anything spawns), and one task at a
time owns a workspace subtree — `workspacePathsOverlap`, so symlink
aliases of the same tree collide. The v2 session permissions ruleset
currently grants `read/write/shell: *`; the confinement is the boundary
that ruleset would enforce if it is ever scoped.

Multi-tenant worker configuration sets the task deadline to `3600000ms` (one hour), configurable for new tenant entries with `LUNARWING_MT_EXTERNAL_WORKER_TIMEOUT_MS`. Re-running mt-admin migrates exact legacy `300000ms` entries; set `LUNARWING_MT_MIGRATE_LEGACY_EXTERNAL_WORKER_TIMEOUTS=false` to preserve an intentional five-minute value. External jobs default to asynchronous dispatch (`wait=false`) so they outlive the current conversation turn; completion and progress remain available through job events/status. Explicit `wait=true` remains available for short tasks, but synchronous execution is still bounded by the parent conversation deadline.

## Kimi Code Worker

MoonshotAI/kimi-code is integrated as a persistent LunarWing external worker. A Node.js/TypeScript bridge spawns a `kimi -p` subprocess for each task and translates its streamed output to the `lunarwing-agent-v1` WebSocket protocol. Health is exposed on 8443 and WebSocket traffic on 9090.

**Dockerfile:** `kimicode4lunarwing/Dockerfile`

```bash
cd kimicode4lunarwing
docker build -t lunarwing-worker-kimi:latest .

# Or via the mt-admin lifecycle
sudo ic/scripts/lunarwing-mt-admin.sh build-kimi-worker

# Include same Rust/Go/C/C++/Python/native toolchains as OpenCode worker
sudo ic/scripts/lunarwing-mt-admin.sh build-kimi-worker --with-toolchains

# Or as part of tenant provisioning and build
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant <name> --with-kimi
sudo ic/scripts/lunarwing-mt-admin.sh build-tenant <name> --with-kimi

# Configure per-tenant provider settings
sudo ic/scripts/lunarwing-mt-admin.sh configure-kimi <name> --model <model> --base-url <url> --api-key-file /secure/path/kimi-api-key
```

Default Kimi image is slim. `--with-toolchains` enables Rust 1.96.1, Go 1.24.3,
C/C++, Python headers, native build systems, and same development libraries as
OpenCode profile. Server mode requires `AGENT_AUTH_TOKEN`, even on loopback;
Compose rejects a missing value before container launch. `KIMI_API_KEY` remains
optional for OAuth-backed setups. mt-admin keeps both credentials out of process
arguments by materializing private `kimi-runtime.env`; API key input uses file
path rather than plaintext argv. The Kimi Code CLI and bridge are installed
with `npm ci` from committed integrity locks. `KIMI_CODE_VERSION` defaults to
`2.5.3` and must match `kimi-cli/package-lock.json`. WI#609 moved the pin from
2.5.2 on 2026-11-12; the engine requirement remains `node >=22.19.0`, satisfied
by the pinned Node 22 base. The worker uses only headless
`kimi -p --output-format stream-json`, not the server APIs removed in 2.5.0.
The real-CLI contract gate (`kimi_cli_contract_test.ts`) runs during the image
build and covers headless invocation, `--yolo` rejection, PreToolUse guard
configuration, and the MCP handshake. The new creation-time environment flags
and workspace trust bypass stay off, and the lane does not use the optional
global-search index; see the WI#582 bump entry for the source audit and
verification trail.
An override without a matching reviewed lock fails closed. The pinned version is baked
into `KIMI_CODE_IMAGE_VERSION` and reported as `kimi_code_version`, and
`ic/scripts/tests/test-mt-admin-kimi.sh` cross-checks Dockerfile default, CI
build script, and committed lock version-agnostically. Base images,
Debian indexes, and optional toolchain downloads are also pinned.

For every persistent worker, `add-tenant --with-<worker>` controls the tenant's
selection in `/etc/lunarwing/ports.json`. `build-tenant --with-<worker>` and the
standalone build commands build shared images but do not select a worker for a
tenant.

### Shared startup isolation (WI#469 / goal 113; WI#552)

`start-tenant` and `restart-tenant` attempt selected workers in the existing
`pebble opencode kimi opencode2` order, after starting the daemon. A stale
image's user-contract refusal returns **3**; a refusal that depends only on one
lane's configuration returns **4** (currently an undeclared, malformed or
unresolvable pebble model backend). The worker pass warns for either status,
continues to later selected lanes, then names all refused lanes and returns
**1** if any were refused. `start-worker` exposes the starter's status 3 or 4
directly. Both refusals remain fail-closed for their own lane: no refused
worker is launched, rendered or recorded as ready.

Other outcomes stay distinct:

- Deselected workers, OpenRC intentionally stopped workers, unallocated ports
  and unavailable images retain their existing soft-skip behavior.
- Shared runtime, credential, workspace or egress-apply failures are **not**
  lane-local refusals: any nonzero status other than 3 or 4 aborts the pass.
  Each starter runs in its own `set -e` subshell so an inner failure cannot fall
  through to hash storage, unit registration or a false `worker ready` line.
  The degraded summary names the failed lanes and every selected,
  desired-running lane never attempted, for example `failed worker lanes:
  pebble; not attempted: opencode kimi opencode2`.
- `upgrade-tenant` preserves startup failure through both upgrade layers and
  the lifecycle/DarkIRC locks. It still refreshes an installed host health
  pipeline, but reports **upgrade incomplete**, the startup status, failed
  lanes and any lanes not attempted, not `tenant upgraded`. This is **not an
  atomic rollback**: the new checkout/binary is already installed, the daemon
  and successful lanes may be running. Repair the failed lanes and retry
  `start-tenant`.

Maintainers: capture lifecycle callbacks and startup results with errexit
temporarily disabled, outside `if`/`||`. Putting the function on the left of
`||` suppresses even explicit `set -e` in its nested worker subshells. Both
lock wrappers restore the caller's errexit state; a nested/inherited lifecycle
lock keeps its outer owner's descriptor and ownership. Userns repair and
failed-checkout recovery also capture startup outside conditional-call contexts,
so a failed restart cannot be reported as recovered. Regression coverage:
`test-mt-admin-worker-startup.sh`, `test-mt-admin-worker-image-contract.sh`,
`test-mt-admin-upgrade-clean-tree.sh` and
`test-mt-admin-upgrade-build-failure.sh` (WI#527).

**WI#456 runbook — kimi dispatch fails with `Connection refused (os error
111)`, no kimi ERROR lines in daemon.log:** one verified cause is refusal before the
container listens whenever a tenant store still holds a pre-WI#400 kimi
image (`USER 10002:10002`, or `USER unset`) while the rootful admin store
has no rebuilt copy — `_ensure_tenant_image`'s admin-store-empty branch keeps
the stale tenant copy in play, and `start_tenant_kimi`'s image-user guard
(`ic/scripts/mt-admin-lib/09-worker-config.sh`) refuses it loudly, leaving
the daemon-side `[[sandbox.external_workers]]` route hot against a dead
socket. WI#469 also allowed an earlier stale lane (especially pebble) to abort
the startup pass before Kimi was attempted at all. The isolation above fixes
that amplifier, **not** the stale image itself. Same class as the pebble
WI#452 — the boot-time liveness probe below covers both lanes.
**Remediation:**
`sudo ic/scripts/lunarwing-mt-admin.sh build-kimi-worker`, then
`start-tenant <name>` (or `start-worker <name> kimi`) — image distribution to
tenant stores plus the image-ID launch contract force-recreates the stale
container on the next start. Capture mt-admin's exit status and failed-lane
summary; rebuild each refused lane, not just the first one. Check
`sudo ic/scripts/lunarwing-mt-admin.sh status <name>` using the tenant's
registry-allocated ports, then verify the worker's authenticated WebSocket
handshake, readiness and a canary dispatch. Never substitute a bearer into
logs or command arguments. The 2026-10-22 image-recovery path was verified
end-to-end on temporary tenant
`kimi93test` (2026-10-22, item 93): fresh image `USER 0:0` at CLI pin 2.3.0,
container root with the setpriv task drop, WS handshake `101 Switching
Protocols` with the scoped bearer (401 with a wrong one), `/ready` true —
see `docs/ops/ITEM93_KIMI_WORKER_456.md`. The item also hardened the three
2026-10-22 hunter findings: WI#461 (the rootful workspace chown walk is
`if !`-guarded with a clean rc-1 refusal instead of a bare-command
set-e/errexit hazard), WI#462 (the task-uid PATH ordering at the setpriv
drop now keeps root-owned entries ahead of task-writable toolchain dirs),
and WI#463 (a bare-name `kimi` in
the drop argv now resolves through the bridge's own root-ownership-checked
PATH, failing closed on lookup failure; absolute `KIMI_BIN` overrides pass
through unchanged).
That historical reproduction is **not** evidence that today's reporting
tenants are repaired. WI#456 remains deployment-gated; WI#512's 2026-10-28
all-three-workers connection-refused report needs current host-side evidence.
This change neither rebuilds fleet images nor adds daemon boot-time liveness
probing (the latter is WI#464 — see "Dead worker socket is logged at boot"
below).

### Dead worker socket is logged at boot (WI#464)

A configured external worker whose socket is dead at daemon start used to
be invisible: `External workers configured: kimi, pebble, opencode` only
means the config parsed, and the first dial happened on the first dispatch,
where a refusal surfaced as a failed job and (on the blocking `wait=true`
path, fixed in the same change) produced no ERROR line at all.

The daemon now probes every configured external worker in a background task:
one real WebSocket handshake per endpoint (bearer + `lunarwing-agent-v1`
subprotocol + the worker's `ready` frame), bounded by a 5-second deadline
that is separate from the worker's dispatch `timeout_ms`. A dead or non-WS
socket therefore reads as unreachable.

Timing matters here. `start-tenant` and `restart-tenant` start the daemon
first and the worker containers afterwards (there is no
`After=lunarwing-<tenant>.service` on the worker units), so an immediate
probe would log a fault on every cold start and host reboot. The first pass
therefore waits a **30-second grace**, reports an unreachable worker as:

```
WARN External worker '<name>' not yet reachable at <host>:<port> (still starting?): <reason>
```

and confirms **30 seconds later**; a worker still dead then is promoted to:

```
ERROR External worker '<name>' unreachable at <host>:<port>: <reason>
```

Every endpoint of a load-balanced worker is dialed, so one line is emitted
per dead endpoint; when the worker is still usable through another endpoint
the line is `WARN ... endpoint <host>:<port> is unreachable (worker reachable
via another endpoint)`. A healthy fleet logs one summary line,
`INFO External worker liveness probe: all N worker(s) reachable`.

The probe never sends job content, never logs the bearer token, never fails
boot, never disables a worker, and never changes dispatch: a dead configured
worker still lets the daemon start, and its first dispatch still reports
`ExternalWorkerConnectionFailed` exactly as before. It works single-node and
multi-tenant from the WS URL the daemon already has; it does not consult the
mt-admin ports registry.

A 5-minute re-probe logs only on a state change (reachable→unreachable and
back), so a worker that dies after boot also produces an ERROR line before
the next dispatch. `lunarwing doctor` lists each configured worker with a
live reachable/unreachable result and endpoint under the External-workers
check (it probes live because doctor is its own process; the daemon's own
in-memory snapshot is not readable across processes).

Cosmetic note for the opencode and opencode2 lanes: the probe connection is
a hub client, so their bridge state file can flip `ready` to false while the
probe was the only connection (`updateState(connectionCount > 0)` on close).
Their units and the image HEALTHCHECK read `/health`, not `/ready`, so this
has no lifecycle effect; only kimi's health check reads `ready`.

When you see a boot ERROR naming a worker, the cause is host-side: a stale
pre-WI#400 image in the tenant store, a crash-looping container, or a port
mismatch (the same class as WI#452 for pebble and WI#456 for kimi — follow
those runbooks above). The probe is the detection signal, not the fix.

### Headless permission contract (WI#195, WI#206)

The Kimi worker runs every task through `kimi -p --output-format stream-json`,
a headless batch invocation with no TTY. WI#206 investigation against the
then-pinned CLI (2.0.2, verified empirically with a mock provider through the real
`-p` path plus bundle reading) established what print mode actually enforces:

- `-p` forces the **auto** permission policy regardless of
  `default_permission_mode` (`--yolo`/`--auto` are rejected alongside `-p`).
- The upstream dangerous-command guard is **skipped entirely in auto mode**
  (upstream code returns early for auto) — so with the old
  `dangerous_command_guard = false` bake, dangerous-class commands executed
  with no approval boundary at all. (Guard on/off changes nothing for
  stalls either: WI#195's stall no longer reproduces on 2.0.2.)
- `[[permission.rules]]` from `config.toml` never load in this version —
  user-configured deny rules are dead config.
- **PreToolUse hooks fire before permission checks in every mode**: exit code
  2 blocks the call and the stderr reason is returned to the model as the
  tool error — a bounded, explicit denial.

The worker therefore ships a fail-closed policy instead of blanket
auto-approval:

- `config/config.toml`: `dangerous_command_guard = true` (inert under `-p`
  today, but a future upstream change then fails closed) plus a `[[hooks]]`
  `PreToolUse` / `Bash` entry invoking the baked guard.
- `hooks/headless_command_guard.mjs`: denies privilege escalation
  (`sudo`/`doas`), power/disk destruction (`shutdown`, `reboot`, `mkfs`,
  `wipefs`, block-device writes), fork bombs, remote-content-piped-to-shell,
  recursive `chmod`/`chown` on system roots, SSH-agent socket use, and
  recursive/forced `rm` outside temp roots. Unparseable payloads deny.
  `/app/config` and `/app/hooks` are read-only to the runtime user, so a
  task cannot edit its own guard.
- `entrypoint.sh`: injects the guard + hook into any `KIMI_CONFIG_SOURCE`
  that lacks them, never exports `KIMI_CODE_DANGEROUS_COMMAND_GUARD` or
  `KIMI_CODE_TRUST_WORKSPACE` (unsets both in CLI and bridge mode), and
  keeps `KIMI_CODE_PERMISSION_MODE_REMINDER=0` (prompt noise for a
  fixed-mode worker). `KIMI_CODE_TRUST_WORKSPACE` is kimi-code 2.2.0's
  workspace-trust switch: a truthy value (`1`, `true`, `yes`, `on`) makes
  the CLI spawn every MCP server listed in the repo's `.mcp.json` or
  `.kimi-code/mcp.json` at startup, before any model turn and outside the
  hook (WI#411).
- `kimi_task_executor.ts`: scrubs `KIMI_CODE_DANGEROUS_COMMAND_GUARD`,
  `KIMI_CODE_PERMISSION_MODE_REMINDER` and `KIMI_CODE_TRUST_WORKSPACE` from
  per-task environments after every merge, so task-controlled
  `context.environment` cannot change the approval policy or grant
  workspace trust.
- mt-admin no longer mounts the tenant SSH-agent socket into the kimi
  container (both the imperative `podman run` and the Quadlet), adds
  `no-new-privileges`, and bumps `KIMI_ROOTLESS_LAUNCH_CONTRACT` so existing
  containers force-recreate. Other worker lanes keep the socket.

Verified end-to-end on 2.0.2: `rm -rf <non-temp>`, `sudo`, `shutdown`, and
`curl … | sh` are denied in ~3s with an explicit reason; ordinary commands
(`echo`, `grep`, `git status`, builds, tests) execute unchanged.

## LLM endpoints for LLM-backed workers (WI#330)

The opencode, opencode2, and kimi worker images bake **placeholder** LLM
endpoints (`set-opencode-base-url.invalid`, `set-opencode2-base-url.invalid`,
`set-kimi-base-url.invalid` — the `.invalid` TLD is guaranteed NXDOMAIN).
Earlier images baked developer-network endpoints (an RFC1918 LAN address and a
NetBird MagicDNS name) that are unreachable from any other host; the MagicDNS
name also did not resolve inside the rootless worker containers on the MT
hosts where this was observed. Each entrypoint now **refuses to start** when
the placeholder survives to the effective runtime config (for opencode2 that
is `$OPENCODE_CONFIG` when the operator sets it), with a message naming the
missing env var, instead of dying at the first LLM request after N retries.

Multi-tenant provisioning has **no built-in LLM endpoint default**: `add-tenant`
fails loudly unless it is given `--tensorzero-url` (or fleet
`LUNARWING_MT_TENSORZERO_URL`), or `--llm-base-url` for a non-proxy tenant.
The tenant's TensorZero proxy uses its own `TENSORZERO_URL` from `proxy.env`,
not a fleet literal. systemd passes the validated URL as an argument; OpenRC
passes `--tensorzero-env` and loads the URL only after dropping to the tenant
(WI#376), so tenant data never becomes root-parsed init-script text. Existing
OpenRC deployments must first neutralize installed proxy units fleet-wide with
`render-units --all-proxies`, then update tenant proxy code before restarting.
The patched `upgrade-tenant` performs this mandatory neutralization before
service operations; see [the proxy URL boundary](MULTITENANCY-PRODUCTION.md#proxy-url-boundary-wi376).

mt-admin renders `OPENCODE_BASE_URL` / `KIMI_BASE_URL` / `OPENCODE2_BASE_URL`
into the tenant's `lunarwing.env`. Resolution order per worker:

1. Explicit per-worker flag (`add-tenant --opencode-base-url` /
   `--kimi-base-url` / `--opencode2-base-url`) or a later
   `configure-* --base-url` call.
2. An operator-set value already in `lunarwing.env` (any value without a
   `<KEY>_SOURCE=inherited` marker) — preserved across re-renders.
3. Inherited from the tenant's LLM endpoint: `--llm-base-url` /
   `LUNARWING_MT_LLM_BASE_URL` when container-reachable, else the tenant's
   TensorZero upstream normalized to `/openai/v1`. Inherited values are marked
   `<KEY>_SOURCE=inherited`, **re-derived on every render** (a TensorZero move
   reaches the workers without a per-worker `configure-*` call), and skipped
   by `export-tenant` so a migrated tenant re-derives them on the target host.
4. If none of those yields a URL and the worker is
   enabled, the render/upgrade fails loudly naming the fix — an unreachable
   default is never rendered.

Automatic inheritance excludes host loopback (`127.0.0.1`, `localhost`,
`[::1]` — inside a rootless container that is the container itself, and the
daemon's own proxy-loopback default is therefore never inherited) and known
overlay MagicDNS suffixes (`*.netbird.selfhosted`, `*.netbird.cloud`,
`*.ts.net`, `*.tailscale.net`). Use a numeric overlay/LAN IP or a public DNS
name. This is a hostname filter, not a network probe; operators must ensure
their chosen endpoint (including explicit per-worker overrides) is reachable.
Supplied URLs are shape-validated (http/https only, no whitespace or control
characters, no userinfo, no `%`) at `add-tenant` and `configure-*` time, and
written quoted.

**Upgrade note:** `upgrade-tenant` (and `patch-env`) backfills a missing
`*_BASE_URL` for every *enabled* worker from the tenant's endpoint —
`LLM_BASE_URL` when container-reachable, else the `proxy.env`
`TENSORZERO_URL`. Upgrade performs this preflight before stopping the tenant
or rebuilding worker images, and fails with services still running when no
endpoint is derivable. Pre-existing tenants therefore keep their workers
through the placeholder rollout; a tenant whose daemon endpoint
is itself loopback-only needs one explicit `configure-*` call.

Override knobs (no image rebuild; restart the worker afterwards):

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-opencode  <name> --base-url <url>
sudo ic/scripts/lunarwing-mt-admin.sh configure-kimi      <name> --base-url <url>
sudo ic/scripts/lunarwing-mt-admin.sh configure-opencode2 <name> --base-url <url>
```

All three `configure-*` commands preserve omitted model/base URL values and
the URL's inherited marker. Model-only or Kimi API-key-only changes therefore
still follow future tenant endpoint changes and remain omitted from export;
only an explicit `--base-url` turns an inherited URL into an operator override.
Kimi trims model/URL overrides consistently while preserving API-key bytes.
For the standalone docker-compose developer path, the three `docker-compose.yml` files forward
the `*_MODEL`/`*_BASE_URL` keys from `.env`/the shell into the container; an
unset key still fails the entrypoint guard loudly.

The MR shell-harness lane exercises all three real entrypoint configuration
sections against the baked configs in isolated temporary directories (unset,
empty, whitespace, model-only, explicit URL, mounted config, and OpenCode 2's
effective `OPENCODE_CONFIG`). These checks stop before health/CLI startup;
whole-image builds remain the protected worker publication gate.

## Shared Protocol

All worker images use the `lunarwing-agent-v1` WebSocket subprotocol; the legacy `ironclaw-agent-v1` alias is no longer accepted. Messages are JSON envelopes with `id`, `type`, `timestamp`, `payload`. The worker sends `ready` on connect, receives `task_request`, streams `task_progress`, and sends a final `task_result`.

### Gateway delegation recovery: wait for the job id (WI#478)

A rejected model draft is **not necessarily a terminal turn failure**. In
particular, `repl_argument_corruption` rejects the entire malformed REPL/tool
response before execution; no tool from that draft runs. The engine can still
recover with a fresh provider response and dispatch the job minutes later.
**Wait for the `create_job` result's `job_id` or a terminal turn outcome rather
than re-dispatching the request.** If the wait is unwanted, use `/interrupt` and
confirm the turn stopped; also check for an already-created job before retrying.
This guidance does not add an idempotency key or change job/approval policy.

Affected Engine V2 turns expose these additive, top-level fields in
`GET /api/chat/history`, including DB-backed/paginated history:

- `failure_kind`: the most recent model failure classification, such as
  `repl_argument_corruption`; never the raw provider diagnostic or rejected draft.
- `recovery_state`: `retrying`, `recovered`, `failed`, or `interrupted`.
- `recovery_attempt`: the engine thread's cumulative model-recovery attempt count.
- `error`: a safe explanation of the rejection/recovery and what to wait for.

While retrying, the history turn has `state: "Retrying"` and no `completed_at`.
A successful replacement draft changes it to `Processing` / `recovered`, which
still does **not** mean a job was created or finished. The final outcome changes
it to `Completed`, `Failed`, or `Interrupted` and supplies `completed_at`.
Recovery markers are tied to the originating persisted user-message id, survive
history reloads, and are not restored into model chat context. Older turns are
not backfilled from journals.

The user-scoped SSE event `turn_recovery` carries `thread_id`, `failure_kind`,
`recovery_state`, `recovery_attempt`, and the same safe explanation as `message`:

```json
{"type":"turn_recovery","thread_id":"…","failure_kind":"repl_argument_corruption","recovery_state":"retrying","recovery_attempt":1,"message":"The model draft was rejected before execution; no tools from that draft ran. Recovery attempt 1 is still running. Wait for the job id or terminal result; do not re-dispatch."}
```

`retrying` keeps the turn open; `recovered` means provider recovery succeeded,
not that the delegated job completed. `failed` means model recovery is unavailable
or exhausted; no automatic retry is running. Reconnect by reading history rather than treating a missing assistant
response as permission to send the same delegation again.

For a single delegation, the system/recovery prompt prefers the available
structured `create_job` tool with a plain-prose `description` and acceptance
criteria, without markdown fences or nested REPL drafts. Multi-step code work
can still use the REPL, and malformed-draft rejection remains enabled.

### Runtime control channel (WI#328)

External worker jobs are no longer fire-and-forget: while a task runs, the daemon can deliver follow-up prompts and text directives into it over the same per-task WebSocket connection. The channel is **capability-gated**: a worker advertises `capabilities: ["task_prompt"]` in its `ready` payload, and the daemon rejects prompts for jobs whose worker never advertised it (`ExternalWorkerControlUnsupported`, HTTP 400) instead of reporting a false "queued". Prompts submitted before the handshake completes are rejected as still-connecting (HTTP 409). Two envelope types extend the protocol:

- `task_prompt` (daemon → worker): `{task_id, seq, prompt, done}`. `prompt` is follow-up instruction text (TEXT directives only — validated daemon-side before enqueue; no shell/exec passthrough, 16 KiB cap, no NUL bytes). `seq` is monotonic per job for ordering and dedupe. `done: true` signals "no more prompts coming, wrap up cleanly" — it grants **one final wrap-up turn**: the daemon rejects every later prompt (HTTP 409) from the moment the wrap-up is accepted, and in the opencode worker the bridge closes the task's queue at accept time so a post-done frame is refused instead of queued-and-dropped. Workers must hold queued prompts until the current turn/LLM call ends — never mid-call.
- `task_prompt_ack` (worker → daemon): `{task_id, seq, accepted}` — a receipt meaning "the worker received and queued it". The daemon records acks only for seqs it actually delivered, and the persisted `control_ack` row carries the `accepted` flag (absent counts as accepted), so a worker-refused prompt is auditable as such rather than masquerading as delivered. A delivered seq whose ack never arrives (e.g. the worker's final-drain refusal races `task_result`) is persisted as an explicit `unacked` `control_prompt` row when the task ends, so the audit trail shows the gap instead of implying delivery.

Delivery is **at-most-once** and honest about it: the daemon's per-job control queue is bounded (32 in flight, 64 per job total — a full queue or cap rejects with HTTP 429), a failed send drops the prompt with a `delivery_failed` job event instead of killing the task, the queue is closed the moment the task's read loop exits (including at the start of the cancel handoff) so late senders get an explicit rejection (HTTP 409), and prompts still queued when the task ends are recorded as `dropped`. There is no redelivery; callers that need certainty should check the job event log for the `delivered`/`control_ack` rows. Every enqueue (with SHA-256 digest and length — never the prompt text), delivery, drop, and ack is persisted in the job event log (`control_prompt` / `control_ack` event types) for an auditable control history. Enqueued prompts do **not** extend the task's timeout/fuel budget.

From the agent side this is surfaced through `job_prompt`, which routes `external:`-mode jobs with a live task handle to the control channel instead of rejecting them — note that an "Always allow" approval grant on `job_prompt` now also reaches credentialed external workers, not just local sandbox jobs. From the browser side, `POST /api/jobs/{id}/prompt` routes the same way and job detail reports `can_prompt: true` only for running external jobs whose worker advertised the capability.

Worker support today: the **opencode** worker (`opencode4lunarwing`) consumes `task_prompt` — it validates frames bridge-side (positive integer seq, non-empty 16 KiB NUL-free prompt, per-task queue bound, seq dedupe), binds every queue to the connection that owns the task (a foreign socket's prompt or cancel is refused), injects queued follow-ups into the same opencode session at the turn boundary (session idle, gated against stale idle replays) framed as `FOLLOW-UP INSTRUCTION (seq=N) from the orchestrator`, closes the queue the moment a `done: true` prompt is accepted (the injected turn is the wrap-up turn), and closes it on every error/cancel/finish path so late frames are acked `accepted: false`. Its final `task_result` output accumulates every turn's canonical text — or the turn's streamed tool/text output when the assistant produced none — and a follow-up turn that fails or times out after the main turn banked completed output resolves as `success` with a `[control] follow-up seq=N failed/timed out` note (without completed output the failure/timeout stands — control notes alone cannot manufacture success), so a late "status?" can never turn finished work into Failed/TimedOut or replace the deliverable. The kimi, opencode2, pebble, and lunardepth bridges do not advertise the capability (pebble runs its CLI one-shot with no session to inject into) and therefore reject prompts explicitly.

`closed` seals **admission**, not consumption: already accepted prompts drain
in FIFO order through the final `done=true` turn. Any terminal failure, timeout
or cancellation records each remaining accepted prompt as
`[control] follow-up seq=N not run`. Permission-reply and SSE errors follow the
same policy as prompt errors. Preserving success requires actual completed
assistant/tool output; control notes alone cannot turn empty or permission-denied
work into success. Explicit cancellation remains `cancelled` and retains completed
output. The 16 MiB daemon WebSocket limit rejects oversized results; it does not
truncate accumulated output or guarantee delivery of an oversized deliverable.
The normal terminal path delivers the banked canonical turns joined. When no
turn was banked, the task resolves `error`; accumulated output is delivered as
diagnostics only (FG-1). Success is preserved through a failed or timed-out
follow-up only when a turn was banked (canonical assistant text, streamed
assistant text, or completed-tool output of that turn), including tool output
from a main attempt retried successfully with no text; a later failed
follow-up preserves that main turn's success (ER-1). For normally completed
turns, success is decided from completed output only: a turn whose sole stream
is a tool error or a transient-retry note banks nothing, so it resolves as an
empty-output (or denied-permission) error while the diagnostics stay in the
progress stream — the same rule in the opencode and opencode2 executors
(WI#386). Opencode2's explicit `session.execution.interrupted` settlement is
an exception: it retains its pre-existing behavior and may still succeed with
partial streamed text. Raw accumulated output feeds the delivered text on the
failure paths but never the success decision itself.

The daemon bounds prompts in UTF-8 bytes; the OpenCode bridge's additional
length check uses 16,384 UTF-16 units. Its queue holds at most 64 pending prompts.
Deduplication applies while admission is open; closed queues refuse redelivery
too. Invalid instructions retain valid task/sequence identifiers in refusal
acks; invalid identifiers themselves cannot be correlated.

Interop: pre-WI#328 workers that never send `task_prompt_ack` still function — the daemon tolerates missing acks and ignores unknown message types, and newer workers must likewise ignore `task_prompt` if they do not implement prompting (they simply omit the capability, and the daemon never sends them the frame).

### Job list lane attribution (WI#479)

`GET /api/jobs` now carries the lane on every entry, so attributing jobs during
multi-job testing no longer needs a detail call per job. Each `JobInfo` gains:

- `job_mode` — the same value the detail response reports (`external:kimi`,
  `external:pebble`, `harness`, …), **omitted** when the job is in the default
  `worker` mode, matching the detail response's omission rule.
- `worker_name` — the lane name for `external:<name>` jobs only, omitted
  otherwise.

Both come from the `job_mode` column already selected by
`list_sandbox_jobs_for_user` / the libSQL equivalent, so the list stays a
single query per source. Agent (non-sandbox) jobs have no sandbox mode and
report neither field. An optional `?mode=` filter accepts `worker`
(the default, also matching mode-less rows), `harness`,
`external` (any external lane) or `external:<name>`; the web Jobs table shows
a Mode column fed from the same field.

### Rebuilt image rollout (image-ID contract)

A worker container is bound to the image ID it was created from. `start-tenant` refreshes the tenant's rootless store with a rebuilt image (`_ensure_tenant_image`), but on OpenRC and the non-Quadlet fallback it then only `podman start`s an existing container, and an already-active Quadlet unit is not restarted, so a rebuilt image did not reach a running worker: the old container kept serving the old image until an operator removed it by hand (`restart-tenant` helped only on Quadlet hosts, where the unit re-runs `podman run`). This is how the kimi 2.2.0 image stayed in service without the WI#411/WI#412 fixes. Every worker lane (`pebble`, `opencode`, `opencode2`, `kimi`, `lunardepth`) now hashes the tenant-store image ID into its container contract (`_worker_launch_contract`, the same mechanism as the LunarVision/LunarVoice sidecar contracts), so `start-tenant` and `restart-tenant` force-recreate the container whenever the image ID differs from the one it was created from, on both init systems. The procedure after any worker change is therefore:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh build-kimi-worker        # or build-<worker>-worker / build-tenant --with-<worker>
sudo ic/scripts/lunarwing-mt-admin.sh restart-tenant <tenant>  # logs "config changed for lunarwing-kimi-<tenant>; force-recreating"
```

Rollout note: the first `start-tenant` after upgrading mt-admin to this version recreates every worker container once, because the stored contract hash gains the image segment. The replaced image stays in the tenant store until the next rebuild's prune. `ic/scripts/tests/test-mt-admin-worker-image-contract.sh` pins the behaviour for all five lanes on both paths.

mt-admin builds also stamp `org.opencontainers.image.revision` with the source checkout's commit (the `SOURCE_REVISION` build-arg every worker Dockerfile declares and CI already passed; mt-admin builds left it `unknown`). Standalone per-lane builds retain the live operator-checkout behavior: `-dirty` is appended when the worker directory has uncommitted or untracked changes and `unknown` is kept outside a git checkout, so a deployed image can be traced to a target commit:

```bash
podman image inspect -f '{{index .Config.Labels "org.opencontainers.image.revision"}}' lunarwing-worker-kimi:latest
```

For all five lanes, `build-tenant`, `build-all`, and pre-stop upgrade candidate
builds resolve the tenant checkout's HEAD as its owner and require the selected
worker directory to be clean. That SHA is the image's `SOURCE_REVISION`, but it
is a provenance identifier, not a trust boundary. The commit must already be an
object in the operator checkout and an ancestor of the release ref being
deployed. `build-tenant` and `build-all` use the operator checkout's current
local branch (`refs/heads/*`), or its exact HEAD commit when detached; the
tenant's branch name and detached state are never consulted. Upgrade candidates
use the explicit `upgrade-tenant --target` ref, which may name a local branch,
tag, remote-tracking ref, or commit. Any ancestor of the selected ref is
accepted, not only its tip. Unknown and off-ref commits are refused before any
image build.

Fetch and fast-forward the operator checkout's current local branch before a
default tenant build. Fetching only `origin/*` does not update that default, so
a tenant at the newer remote tip is refused until the local branch advances.
`--source-repo` / `LUNARWING_MT_SOURCE_REPO` selects the repository used to
clone or update the tenant checkout; it does not replace the operator-checkout
object and ancestry checks above.

The Dockerfile and context are extracted from `git archive` of that commit in
the operator checkout into a root-owned mode-0700 temporary directory. Links and
special files are refused, and the directory is removed on exit. Tenant-authored
working-tree bytes therefore never reach the rootful builder, and uncommitted
operator edits are excluded too. Standalone `build-<lane>-worker` commands keep
using the operator checkout directly. `build-all` retains its clean-source and
mixed-revision checks across tenants.

Podman builds retain `--network=host` because the affected hosts' default build
network cannot reach upstreams; this is an operator networking choice, and the
context is now the validated operator archive rather than a tenant directory.
It does not change the runtime per-tenant egress allowlist. Pebble's fat-build
smoke script is host-executed by root and likewise always comes from the
operator checkout (WI#507/WI#539). `test-mt-admin-pebble-build-inputs.sh` pins
these boundaries and the upgrade harness exercises the candidate call site.

### Upgrade ordering

Since the `ironclaw-agent-v1` alias was removed (2.1.1), a 2.1.1 daemon offers only `lunarwing-agent-v1` and cannot connect to a worker container that still only accepts the legacy name — every dispatch to such a worker fails the WebSocket upgrade (HTTP 400, no echoed subprotocol) and surfaces as an external-worker connection failure whose reason includes the `lunarwing-agent-v1` / `ironclaw-agent-v1` hint. Workers are protocol-compatible in the other direction (2.1.1 workers accept the primary name only, which pre-2.1.1 daemons also offer), so the safe upgrade order is **workers first, then the daemon**, or — simplest — recreate worker containers in the same maintenance window as the daemon. mt-admin managed upgrades (`build-tenant`, `build-pebble-worker`, `build-opencode-worker`, `build-opencode2-worker`, `build-kimi-worker`, each followed by `restart-tenant`) rebuild the image and, through the image-ID contract above, recreate the worker containers; only manual deployments can strand a legacy-only worker.

## Proxy Note

On networks with TLS-intercepting proxies (e.g., corporate networks), Docker builds will fail with "server certificate not trusted" errors. Either build on a network without MITM proxies, or add the proxy CA certificate to each Dockerfile before the `apt-get`/`apk` steps.
