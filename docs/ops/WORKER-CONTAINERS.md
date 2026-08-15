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
cd pebble4lunarwing
docker build -t lunarwing-worker-pebble:latest .

# Or select the worker per tenant and build the shared image via mt-admin
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant <name> --with-pebble
sudo ic/scripts/lunarwing-mt-admin.sh build-pebble-worker
```

Persistent worker mode (`PEBBLE_MODE=websocket`). Requires `AGENT_AUTH_TOKEN` for WebSocket auth and optionally `NANOGPT_API_KEY` for the underlying NanoGPT backend (configure per-tenant via `configure-pebble <name> --nanogpt-api-key <key>`). See `pebble4lunarwing/` for details.

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

The default image is slim and does not contain Rust, Go, or C/C++ toolchains. Build with `--with-toolchains` when worker jobs must compile or test code; the toolchain image uses Rust 1.96.1. Modes: `--mode websocket` (default, persistent) or `--mode cli` (one-shot; prompt via `TASK_PROMPT`). Requires `AGENT_AUTH_TOKEN` for WebSocket auth. Supports per-tenant model/base-URL overrides via `configure-opencode <name> [--model <m>] [--base-url <url>]`, optional Paseo MCP integration (`PASEO_URL`/`PASEO_TOKEN`), and SSH agent socket bind-mounting. `configure-study-mcp <name> --home <absolute-checkout>` gives core and OpenCode the same lw2studymcp data. `--allow-write` enables approved core-agent mutations in separate state; OpenCode always receives code and state read-only at `/opt/lw2studymcp` and `/var/lib/lw2studymcp`. The opencode upstream is cloned at build time and pinned to a release tag via the `OPENCODE_REF` build arg (default `v1.18.18`); override with `podman build --build-arg OPENCODE_REF=<tag>`. The pinned ref is baked into the image as `OPENCODE_IMAGE_REF` and reported on the worker `/health` endpoint as `opencode_ref`. See `opencode4lunarwing/README.md` for the full env var reference.

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
path rather than plaintext argv.

For every persistent worker, `add-tenant --with-<worker>` controls the tenant's
selection in `/etc/lunarwing/ports.json`. `build-tenant --with-<worker>` and the
standalone build commands build shared images but do not select a worker for a
tenant.

## Shared Protocol

All worker images use the `lunarwing-agent-v1` WebSocket subprotocol; the legacy `ironclaw-agent-v1` name is still accepted as an alias for one release. Messages are JSON envelopes with `id`, `type`, `timestamp`, `payload`. The worker sends `ready` on connect, receives `task_request`, streams `task_progress`, and sends a final `task_result`.

## Proxy Note

On networks with TLS-intercepting proxies (e.g., corporate networks), Docker builds will fail with "server certificate not trusted" errors. Either build on a network without MITM proxies, or add the proxy CA certificate to each Dockerfile before the `apt-get`/`apk` steps.
