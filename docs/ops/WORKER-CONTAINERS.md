# Worker Container Images

Several container images provide sandboxed and persistent job execution for LunarWing.

Nanocode is retired and has no active source, build, or runtime image definition.
See [`NANOCODE-REMOVAL.md`](NANOCODE-REMOVAL.md) for upgrade behavior.

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
[`v2.1.2.0_ITEM26_WI179_SANDBOX_HARDENING.md`](v2.1.2.0_ITEM26_WI179_SANDBOX_HARDENING.md).

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
**v2.0.14** at reviewed full commit
`08462140ec0de1e4b17d4a353d8d5827f53cf7b0`; the v2 bridge uses
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
`OPENCODE_VERSION`/`OPENCODE_REF` ARG defaults against the build script and
the Rust toolchain pin.

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
`2.0.2` and must match `kimi-cli/package-lock.json` (the pin moved `0.42.0` →
`0.43.0` → `0.43.1` early in the v2.1.2.0 cycle, then `0.43.1` → `2.0.2` on
2026-09-21 with `53ccb8377`, adopting the npm-latest 2.x line together with a
deterministic CLI contract gate so clean image builds resolve a model without
host config; the 2.x behavior changes the worker cares about are the headless
permission-contract findings below, all verified on 2.0.2); an override
without a matching reviewed lock fails closed. The pinned version is baked
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
pinned CLI (2.0.2, verified empirically with a mock provider through the real
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

## Shared Protocol

All worker images use the `lunarwing-agent-v1` WebSocket subprotocol; the legacy `ironclaw-agent-v1` alias is no longer accepted. Messages are JSON envelopes with `id`, `type`, `timestamp`, `payload`. The worker sends `ready` on connect, receives `task_request`, streams `task_progress`, and sends a final `task_result`.

### Upgrade ordering

Since the `ironclaw-agent-v1` alias was removed (2.1.1), a 2.1.1 daemon offers only `lunarwing-agent-v1` and cannot connect to a worker container that still only accepts the legacy name — every dispatch to such a worker fails the WebSocket upgrade (HTTP 400, no echoed subprotocol) and surfaces as an external-worker connection failure whose reason includes the `lunarwing-agent-v1` / `ironclaw-agent-v1` hint. Workers are protocol-compatible in the other direction (2.1.1 workers accept the primary name only, which pre-2.1.1 daemons also offer), so the safe upgrade order is **workers first, then the daemon**, or — simplest — recreate worker containers in the same maintenance window as the daemon. mt-admin managed upgrades (`build-tenant`, `build-pebble-worker`, `build-opencode-worker`, `build-kimi-worker`) already rebuild and recreate worker containers as part of a tenant upgrade; only manual deployments can strand a legacy-only worker.

## Proxy Note

On networks with TLS-intercepting proxies (e.g., corporate networks), Docker builds will fail with "server certificate not trusted" errors. Either build on a network without MITM proxies, or add the proxy CA certificate to each Dockerfile before the `apt-get`/`apk` steps.
