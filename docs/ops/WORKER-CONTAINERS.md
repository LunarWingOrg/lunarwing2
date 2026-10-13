# Worker Container Images

Several container images provide sandboxed and persistent job execution for LunarWing.

Nanocode is retired and has no active source, build, or runtime image definition.
See [`NANOCODE-REMOVAL.md`](NANOCODE-REMOVAL.md) for upgrade behavior.

## Hub-handshake credential boundary (WI#373)

Every worker container receives `AGENT_AUTH_TOKEN` — the tenant-scoped
`WORKER_AUTH_TOKEN` (WI#205; legacy tenants fall back to the gateway bearer)
that authenticates the daemon→worker WebSocket handshake. Only the **bridge
process** needs it. Task code is untrusted, so no task process may inherit it:
a leaked token lets any job impersonate the daemon toward that worker (and,
where the legacy fallback still applies, toward the whole gateway surface).

Each lane enforces the strip at its task-process boundary:

- **Kimi** — the executor deletes `AGENT_AUTH_TOKEN` from the assembled
  per-task env after all merges (bridge env spread, task-supplied
  `context.environment`, proxy `extraEnv`), so neither inheritance nor
  task-supplied env can deliver it. Pinned by the executor self-test.
- **OpenCode / OpenCode 2** — the entrypoint launches `opencode serve` via
  `env -u AGENT_AUTH_TOKEN`: the server process runs all task code (every
  bash tool child inherits its env), while the separately exec'd bridge keeps
  its own copy. Pinned by the worker entrypoint harness, which asserts both
  directions (serve sees it absent, bridge sees it present).
- **Pebble** — `spawn_pebble` rebuilds the child env via `env_clear()` +
  `strip_bridge_credentials` (everything else preserved: PATH, locale,
  `PEBBLE_*` provider variables) and drops task-supplied re-injections.
  Pinned by a self-exec regression test that carries the token in the parent
  env and probes the real spawned environment through a stub.

Residual, out of scope for WI#373: the OpenCode serve password
(`OPENCODE_SERVER_PASSWORD`) is necessarily present in the serve process env —
the server enforces it itself, and task code already runs inside that trust
boundary. The LLM provider keys (`KIMI_API_KEY`, `TENSORZERO_API_KEY`, …) are
deliberately *not* stripped: task execution needs them.

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
grants phase-1 names. `LUNARWING_TOOL_PROXY_TIMEOUT_MS` bounds one proxied
call (default 300000, capped 900000). The opencode/opencode2/pebble lanes do
not have the bridge yet; see
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
container. (The manager also strips the worker-level list of a non-cron
entry whose endpoint URL exactly matches a cron entry's endpoint, but
exact-string matching cannot catch aliases such as `localhost` vs
`127.0.0.1`, so the rule stands on its own.) A borrowed call is recorded
in the `job_events` — and counted against the 100-frame budget — of the
job whose relay socket was used, not of the job that issued it.

## Generic shell sandbox network policy (WI#154/155)

The generic shell sandbox (`ic/src/sandbox/`) defaults to
`SANDBOX_NETWORK=none`, in code and in mt-admin-rendered multi-tenant envs.
`proxy-enforced` keeps `--network none` and exposes only a per-execution Unix
relay to the public-only allowlist proxy — local Linux runtime and a
Python-3-equipped image are required (the bundled `docker/sandbox.Dockerfile`
image already includes Python 3). `proxy` remains as the legacy advisory mode.

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

## LunarWing Worker

The main sandbox worker. Its runtime image includes Git, build-essential, Node.js/npm, Python 3, Rust, and GitHub CLI; it does not install a Claude Code executable. The orchestrator (`ic/src/orchestrator/`) manages container lifecycle, LLM proxying, and credential injection.

**Dockerfile:** `ic/Dockerfile.worker`

```bash
cd ic
docker build -f Dockerfile.worker -t lunarwing-worker:latest .
```

Runs as non-root user `sandbox` (UID 1000) in `/workspace`. Entrypoint is the `lunarwing` binary — the orchestrator passes the full command via Docker cmd.

### Generic sandbox egress default (WI#154)

The generic podman sandbox (`shell` tool, `ic/src/sandbox/`) defaults to
**`SANDBOX_NETWORK=none`** on multi-tenant hosts: mt-admin renders
`SANDBOX_NETWORK=none` into every tenant's `lunarwing.env` (an operator-set
`proxy` value is preserved on re-render). The advisory `proxy` mode lets a
sandboxed process that ignores proxy env vars reach host-loopback services —
worker WSS/health ports, tenant PostgreSQL, and cross-tenant loopback — so it
is an explicit opt-in (`SANDBOX_NETWORK=proxy`) for tenants that genuinely
need allowlisted web access. The enforced-proxy middle ground is tracked in
WI#155. See `ic/src/NETWORK_SECURITY.md` §5 for the tier table.

### Generic sandbox image + tenant defaults (WI#223)

For new multi-tenant tenants, the generic sandbox's image is the lean
**`lunarwing-sandbox:latest`**, built from `ic/docker/sandbox.Dockerfile`
(common build tooling only — no lunarwing binary, much smaller than
`lunarwing-worker:latest`). mt-admin renders these defaults into every
tenant's `lunarwing.env` during `add-tenant` (existing values are preserved
if that command is explicitly re-run):

```bash
SANDBOX_RUNTIME=podman
SANDBOX_IMAGE=lunarwing-sandbox:latest
SANDBOX_AUTO_PULL=false
```

`add-tenant` builds the image into the selected admin store when absent. For
rootless Podman it distributes the image into the tenant's own store
(`_ensure_tenant_image` semantics — a rebuilt admin image is re-distributed);
for Docker it checks the shared store. `start-tenant` also reconciles the
image before daemon startup, so a failed add-tenant build or distribution can
be retried. On hosts with both CLIs, the generic shell sandbox prefers Podman
even when mt-admin runs its worker containers with Docker; an explicit
`SANDBOX_RUNTIME=docker` still chooses Docker. Provisioning uses the tenant's
selected runtime and prepares its rootless Podman store when needed.
Only tenants configured with this managed image are reconciled;
an operator-selected custom image is left alone. `SANDBOX_AUTO_PULL=false` is
deliberate: the tag exists only in local stores, so an auto-pull would fail
against a registry that has never hosted it. Single-node hosts keep the
in-code defaults (`auto` runtime preference, `lunarwing-worker:latest`,
auto-pull on).

Manual build/recovery:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh build-sandbox-image <name>
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant <name>
```

`build-sandbox-image <name>` builds into the admin store of that tenant's
selected sandbox runtime (the argument may be omitted for the new-tenant
Podman-first default); `start-tenant` makes it available in the daemon's
runtime store. If either step fails, mt-admin warns
and the generic shell sandbox fails closed with `ImageUnavailable`; base tenant
provisioning remains available. `upgrade-tenant` **does not** add these keys to
existing tenants or switch their existing sandbox image. To opt in an older
tenant, set the three variables in its `lunarwing.env`, build the image, then
`restart-tenant <name>` so both its daemon configuration and image store update.

The build context is `ic/docker/`, not `ic/`: the Dockerfile has no
COPY/ADD, and `ic/` carries the vendored crate tree that must stay
in-context for `Dockerfile.worker`'s offline build.

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

Persistent worker mode (`PEBBLE_MODE=websocket`). Requires `AGENT_AUTH_TOKEN` for WebSocket auth and optionally `NANOGPT_API_KEY` for the underlying NanoGPT backend (configure per-tenant via `configure-pebble <name> --nanogpt-api-key <key>`). Upstream Pebble v0.5.0 is fetched by its reviewed full commit, both Cargo builds use committed lockfiles, and base images/package indexes/toolchain downloads are pinned. See `pebble4lunarwing/` for details.

In the fat (`--with-toolchains`) image the Rust/Go toolchains are anchored against the `/etc/profile` PATH reset in upstream pebble's `/bin/sh -lc` task shell (WI#334): an `/etc/profile.d` snippet plus real-binary `/usr/local/bin` symlinks and a `rustup` wrapper keep `cargo`/`go` on PATH after the reset, proven at build time in both the real spawn mode and a fully sanitised shell, and re-probed by the smoke script at the end of every fat mt-admin build (which targets a candidate tag and re-tags `:latest` only after the probe passes, using the build's own container runtime and `--pull=never`). Re-check any built image or running worker with `pebble4lunarwing/scripts/smoke-task-shell-toolchain.sh image [ref]` / `container <name>`; see `docs/ops/PEBBLE-WORKER.md` "Task-Shell Toolchain PATH".

Upstream's pinned lockfile ships rustls 0.23.41 (RUSTSEC-2026-0285, fixed >= 0.23.45) and has no newer release, so the reviewed patched lock is committed at `pebble4lunarwing/upstream/Cargo.lock` and copied over the checkout during the image build; it also retires upstream's yanked `chacha20 0.10.1` pin (0.10.2). The Dockerfile supplies the canonical reviewed `PEBBLE_LOCK_SHA256` default for CI, mt-admin, Compose, and plain Docker builds. `scripts/verify_upstream_lock.py` verifies that digest and compares complete parsed package records (including additions/removals, source, checksum, and dependencies; records are keyed by `(name, version, source)` so multi-version crates such as `cpufeatures`/`getrandom` are fully covered), allowing changes only to rustls/rustls-webpki/chacha20 and rejecting a no-op override so the input is retired once upstream ships its own fix. `.gitlab/deliverables.json` explicitly classifies this lock-only override under `build_inputs`, with ci-config regression checks and worker-image candidate coverage; it is not a standalone Cargo root.

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

The default image is slim and does not contain Rust, Go, or C/C++ toolchains. Build with `--with-toolchains` when worker jobs must compile or test code; the toolchain image uses Rust 1.96.1. Modes: `--mode websocket` (default, persistent) or `--mode cli` (one-shot; prompt via `TASK_PROMPT`). Requires `AGENT_AUTH_TOKEN` for WebSocket auth. Supports per-tenant model/base-URL overrides via `configure-opencode <name> [--model <m>] [--base-url <url>]`, optional Paseo MCP integration (`PASEO_URL`/`PASEO_TOKEN`), and SSH agent socket bind-mounting. `configure-study-mcp <name> [--home <absolute-checkout>]` gives core and OpenCode the same lw2studymcp data. Without `--home` it uses the shared default checkout (`/var/cache/lunarwing/lw2studymcp`, override with `LUNARWING_MT_STUDY_NOTES_HOME`) and always syncs it to the latest upstream (`LUNARWING_MT_STUDY_NOTES_REPO`, default `https://git.lunarwing.org/cmc/lw2studymcp.git`) first — cloning when missing, fast-forwarding a clean checkout, and refusing to touch a dirty or non-git directory. `--sync` applies the same update to an explicit `--home`. `add-tenant <name> --with-study-mcp [--study-mcp-home <path>]` runs this auto-configuration (read-only) during provisioning so a new tenant starts with current knowledge of the LunarWing codebase. `--allow-write` enables approved core-agent mutations in separate state; OpenCode always receives code and state read-only at `/opt/lw2studymcp` and `/var/lib/lw2studymcp`. OpenCode is pinned to **v1.18.25**, fetched by reviewed full commit `cb7d8b2f5e44876ef98b661dc10590c915af3a9f` (reverted from v1.18.29 on 2026-09-07: that release crashes on model entries lacking full metadata shape and dies silently in its ai-sdk stream path — see the Dockerfile pin note; re-bump only after both regressions are fixed upstream); `OPENCODE_REF` accepts only a full SHA, the checkout is verified, and a frozen Bun lock is mandatory. The release and SHA are baked into `OPENCODE_IMAGE_REF` as `v1.18.25@<full-sha>` and reported together as `opencode_ref`. Base images, Debian indexes, and optional toolchain downloads are also pinned. See `opencode4lunarwing/README.md` for the full environment reference.

Agent-facing dispatch uses `create_job(title: "...", description: "...", mode: "opencode")` in both legacy and Engine V2 chat paths. `opencode` is a named job execution backend, not an extension, MCP server, skill, routine, or SSH host; no registry search or installation is involved.

## OpenCode 2 Worker (opencode2)

The OpenCode **2** lane, run as a separate external worker per
`docs/plans/OPENCODE2_EXTERNAL_WORKER_PLAN.md`. v1 stays the default and
keeps its proven v1.18.25 pin; v2 is opt-in per tenant until upstream's
2.0.x patch stack settles. Do not configure both lanes on the same
tenant during the transition (agent routing confusion — plan §5).

**Dockerfile:** `opencode2.4lunarwing/Dockerfile` — pinned to
**v2.0.24** at reviewed full commit
`e7a34f09bfd9134dfade5a8ddb843f7030bc9a69`; the v2 bridge uses
`@opencode/client` (promise API) instead of `@opencode-ai/sdk`, and the
v2 config file is `config/opencode2.json` (`providers` map,
`settings.baseURL`, `package`). Container UID/GID is **10003**
(opencode 10001, kimi 10002) so both opencode lanes keep separate
userns identities on one host.

```bash
cd opencode2.4lunarwing
docker build -t lunarwing-worker-opencode2:latest .

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

Image self-tests use `/tmp/opencode2-build-tests`, removed by an EXIT trap in
the same Docker `RUN`, and disable Python bytecode writes. The workspace-path
test also removes its own `oc2-ws-*` directory in `finally` for local runs.
These address the two reported sources of opencode2 digest drift: per-build
chunk IDs in the executable and leftover randomly named test workspaces.
The candidate versus uncached-rebuild **whole-image digest comparison remains
mandatory**; a passing binary check alone does not establish image reproducibility.

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
source shipped in the image.)

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
`2.1.1` and must match `kimi-cli/package-lock.json` (the pin moved `0.42.0` →
`0.43.0` → `0.43.1` → `2.0.2` earlier in the v2.1.2.0/v2.1.3.0 cycle, then
`2.0.2` → `2.1.1` on 2026-09-26 for checklist item #2, adopting npm `latest`
again; engine requirement unchanged at `node >=22.19.0`, satisfied by the
pinned node 22 base; the 2.x behavior changes the worker cares about are the
headless permission-contract findings below, verified on 2.0.x — the 2.1.1
live CLI contract gate (`kimi_cli_contract_test.ts`) runs during the worker
Dockerfile build and still needs a protected candidate build to verify it).
An override without a matching reviewed lock fails closed. The pinned version is baked
into `KIMI_CODE_IMAGE_VERSION` and reported as `kimi_code_version`, and
`ic/scripts/tests/test-mt-admin-kimi.sh` cross-checks Dockerfile default, CI
build script, and committed lock version-agnostically. Base images,
Debian indexes, and optional toolchain downloads are also pinned.

For every persistent worker, `add-tenant --with-<worker>` controls the tenant's
selection in `/etc/lunarwing/ports.json`. `build-tenant --with-<worker>` and the
standalone build commands build shared images but do not select a worker for a
tenant.

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
  that lacks them, never exports `KIMI_CODE_DANGEROUS_COMMAND_GUARD`
  (unsets it), and keeps `KIMI_CODE_PERMISSION_MODE_REMINDER=0` (prompt
  noise for a fixed-mode worker).
- `kimi_task_executor.ts`: scrubs `KIMI_CODE_DANGEROUS_COMMAND_GUARD` and
  `KIMI_CODE_PERMISSION_MODE_REMINDER` from per-task environments, so
  task-controlled `context.environment` cannot change the approval policy.
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

### Runtime control channel (WI#328)

External worker jobs are no longer fire-and-forget: while a task runs, the daemon can deliver follow-up prompts and text directives into it over the same per-task WebSocket connection. The channel is **capability-gated**: a worker advertises `capabilities: ["task_prompt"]` in its `ready` payload, and the daemon rejects prompts for jobs whose worker never advertised it (`ExternalWorkerControlUnsupported`, HTTP 400) instead of reporting a false "queued". Prompts submitted before the handshake completes are rejected as still-connecting (HTTP 409). Two envelope types extend the protocol:

- `task_prompt` (daemon → worker): `{task_id, seq, prompt, done}`. `prompt` is follow-up instruction text (TEXT directives only — validated daemon-side before enqueue; no shell/exec passthrough, 16 KiB cap, no NUL bytes). `seq` is monotonic per job for ordering and dedupe. `done: true` signals "no more prompts coming, wrap up cleanly" — it grants **one final wrap-up turn**: the daemon rejects every later prompt (HTTP 409) from the moment the wrap-up is accepted, and in the opencode worker the bridge closes the task's queue at accept time so a post-done frame is refused instead of queued-and-dropped. Workers must hold queued prompts until the current turn/LLM call ends — never mid-call.
- `task_prompt_ack` (worker → daemon): `{task_id, seq, accepted}` — a receipt meaning "the worker received and queued it". The daemon records acks only for seqs it actually delivered, and the persisted `control_ack` row carries the `accepted` flag (absent counts as accepted), so a worker-refused prompt is auditable as such rather than masquerading as delivered. A delivered seq whose ack never arrives (e.g. the worker's final-drain refusal races `task_result`) is persisted as an explicit `unacked` `control_prompt` row when the task ends, so the audit trail shows the gap instead of implying delivery.

Delivery is **at-most-once** and honest about it: the daemon's per-job control queue is bounded (32 in flight, 64 per job total — a full queue or cap rejects with HTTP 429), a failed send drops the prompt with a `delivery_failed` job event instead of killing the task, the queue is closed the moment the task's read loop exits (including at the start of the cancel handoff) so late senders get an explicit rejection (HTTP 409), and prompts still queued when the task ends are recorded as `dropped`. There is no redelivery; callers that need certainty should check the job event log for the `delivered`/`control_ack` rows. Every enqueue (with SHA-256 digest and length — never the prompt text), delivery, drop, and ack is persisted in the job event log (`control_prompt` / `control_ack` event types) for an auditable control history. Enqueued prompts do **not** extend the task's timeout/fuel budget.

From the agent side this is surfaced through `job_prompt`, which routes `external:`-mode jobs with a live task handle to the control channel instead of rejecting them — note that an "Always allow" approval grant on `job_prompt` now also reaches credentialed external workers, not just local sandbox jobs. From the browser side, `POST /api/jobs/{id}/prompt` routes the same way and job detail reports `can_prompt: true` only for running external jobs whose worker advertised the capability.

Worker support today: the **opencode** worker (`opencode4lunarwing`) consumes `task_prompt` — it validates frames bridge-side (positive integer seq, non-empty 16 KiB NUL-free prompt, per-task queue bound, seq dedupe), binds every queue to the connection that owns the task (a foreign socket's prompt or cancel is refused), injects queued follow-ups into the same opencode session at the turn boundary (session idle, gated against stale idle replays) framed as `FOLLOW-UP INSTRUCTION (seq=N) from the orchestrator`, closes the queue the moment a `done: true` prompt is accepted (the injected turn is the wrap-up turn), and closes it on every error/cancel/finish path so late frames are acked `accepted: false`. Its final `task_result` output accumulates every turn's canonical text — or the turn's streamed tool/text output when the assistant produced none — and a follow-up turn that fails or times out after the main turn banked completed output resolves as `success` with a `[control] follow-up seq=N failed/timed out` note (without completed output the failure/timeout stands — control notes alone cannot manufacture success), so a late "status?" can never turn finished work into Failed/TimedOut or replace the deliverable. The kimi, opencode2, and pebble bridges do not advertise the capability (pebble runs its CLI one-shot with no session to inject into) and therefore reject prompts explicitly.

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
diagnostics only (FG-1). Success is
preserved through a failed or timed-out follow-up only when a turn was banked
(canonical text, streamed text, or completed-tool output of that turn),
including tool output from a main attempt retried successfully with no text;
a later failed follow-up preserves that main turn's success (ER-1). Raw
accumulated output feeds the delivered text on the failure paths but never
the success decision itself.

The daemon bounds prompts in UTF-8 bytes; the OpenCode bridge's additional
length check uses 16,384 UTF-16 units. Its queue holds at most 64 pending prompts.
Deduplication applies while admission is open; closed queues refuse redelivery
too. Invalid instructions retain valid task/sequence identifiers in refusal
acks; invalid identifiers themselves cannot be correlated.

Interop: pre-WI#328 workers that never send `task_prompt_ack` still function — the daemon tolerates missing acks and ignores unknown message types, and newer workers must likewise ignore `task_prompt` if they do not implement prompting (they simply omit the capability, and the daemon never sends them the frame).

### Upgrade ordering

Since the `ironclaw-agent-v1` alias was removed (2.1.1), a 2.1.1 daemon offers only `lunarwing-agent-v1` and cannot connect to a worker container that still only accepts the legacy name — every dispatch to such a worker fails the WebSocket upgrade (HTTP 400, no echoed subprotocol) and surfaces as an external-worker connection failure whose reason includes the `lunarwing-agent-v1` / `ironclaw-agent-v1` hint. Workers are protocol-compatible in the other direction (2.1.1 workers accept the primary name only, which pre-2.1.1 daemons also offer), so the safe upgrade order is **workers first, then the daemon**, or — simplest — recreate worker containers in the same maintenance window as the daemon. mt-admin managed upgrades (`build-tenant`, `build-pebble-worker`, `build-opencode-worker`, `build-kimi-worker`) already rebuild and recreate worker containers as part of a tenant upgrade; only manual deployments can strand a legacy-only worker.

## Proxy Note

On networks with TLS-intercepting proxies (e.g., corporate networks), Docker builds will fail with "server certificate not trusted" errors. Either build on a network without MITM proxies, or add the proxy CA certificate to each Dockerfile before the `apt-get`/`apk` steps.
