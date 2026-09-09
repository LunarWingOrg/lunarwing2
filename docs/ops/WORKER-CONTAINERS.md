# Worker Container Images

Several container images provide sandboxed and persistent job execution for LunarWing.

Nanocode is retired and has no active source, build, or runtime image definition.
See [`NANOCODE-REMOVAL.md`](NANOCODE-REMOVAL.md) for upgrade behavior.

## LunarWing Worker

The main sandbox worker. Its runtime image includes Git, build-essential, Node.js/npm, Python 3, Rust, and GitHub CLI; it does not install a Claude Code executable. The orchestrator (`ic/src/orchestrator/`) manages container lifecycle, LLM proxying, and credential injection.

**Dockerfile:** `ic/Dockerfile.worker`

```bash
cd ic
docker build -f Dockerfile.worker -t lunarwing-worker:latest .
```

Runs as non-root user `sandbox` (UID 1000) in `/workspace`. Entrypoint is the `lunarwing` binary — the orchestrator passes the full command via Docker cmd.

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
`0.41.0` and must match `kimi-cli/package-lock.json`; an override without a
matching reviewed lock fails closed. The pinned version is baked into
`KIMI_CODE_IMAGE_VERSION` and reported as `kimi_code_version`. Base images,
Debian indexes, and optional toolchain downloads are also pinned.

For every persistent worker, `add-tenant --with-<worker>` controls the tenant's
selection in `/etc/lunarwing/ports.json`. `build-tenant --with-<worker>` and the
standalone build commands build shared images but do not select a worker for a
tenant.

## Shared Protocol

All worker images use the `lunarwing-agent-v1` WebSocket subprotocol; the legacy `ironclaw-agent-v1` alias is no longer accepted. Messages are JSON envelopes with `id`, `type`, `timestamp`, `payload`. The worker sends `ready` on connect, receives `task_request`, streams `task_progress`, and sends a final `task_result`.

## Proxy Note

On networks with TLS-intercepting proxies (e.g., corporate networks), Docker builds will fail with "server certificate not trusted" errors. Either build on a network without MITM proxies, or add the proxy CA certificate to each Dockerfile before the `apt-get`/`apk` steps.
