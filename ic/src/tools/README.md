# Tool System

## Adding a New Tool

### Built-in Tools (Rust)

1. Create `src/tools/builtin/my_tool.rs`
2. Implement the `Tool` trait
3. Add `mod my_tool;` and `pub use` in `src/tools/builtin/mod.rs`
4. Register in `ToolRegistry::register_builtin_tools()` in `registry.rs`
5. Add tests

### WASM Tools (Recommended)

WASM tools are the preferred way to add new capabilities. They run in a sandboxed environment with explicit capabilities.

1. Create a new crate in `tools-src/<name>/`
2. Implement the WIT interface (`wit/tool.wit`)
3. Create `<name>.capabilities.json` declaring required permissions
4. Build with `cargo build --target wasm32-wasip2 --release`
5. Install with `lunarwing tool install path/to/tool.wasm`

See `tools-src/` for examples.

## Tool Architecture Principles

**CRITICAL: Keep tool-specific logic out of the main agent codebase.**

The main agent provides generic infrastructure; tools are self-contained units that declare their requirements through capabilities files.

### What Goes in Tools (capabilities.json)

- API endpoints the tool needs (HTTP allowlist)
- Credentials required (secret names, injection locations)
- Rate limits and timeouts
- Auth setup instructions (see below)
- Workspace paths the tool can read
- Approval policy: `never` (default), `unless_auto_approved`, or `always`

`always` cannot be bypassed by global auto-approval. It also rejects user
`Always` grants unless tool explicitly opts in; built-in `ssh` is narrow
exception. WASM tools with SSH capability always use non-bypassable `always`,
regardless of declared policy.

### What Does NOT Go in Main Agent

- Service-specific auth flows (OAuth for Notion, GitHub, etc.)
- Service-specific CLI commands (`auth notion`, `auth github`)
- Service-specific configuration handling
- Hardcoded API URLs or token formats

### Tool Authentication

Tools declare their auth requirements in `<tool>.capabilities.json` under the `auth` section. Two methods are supported:

#### OAuth (Browser-based login)

For services that support OAuth, users just click through browser login:

```json
{
  "auth": {
    "secret_name": "notion_api_token",
    "display_name": "Notion",
    "oauth": {
      "authorization_url": "https://api.notion.com/v1/oauth/authorize",
      "token_url": "https://api.notion.com/v1/oauth/token",
      "client_id_env": "NOTION_OAUTH_CLIENT_ID",
      "client_secret_env": "NOTION_OAUTH_CLIENT_SECRET",
      "scopes": [],
      "use_pkce": false,
      "extra_params": { "owner": "user" }
    },
    "env_var": "NOTION_TOKEN"
  }
}
```

To enable OAuth for a tool:
1. Register a public OAuth app with the service (e.g., notion.so/my-integrations)
2. Configure redirect URIs: `http://localhost:9876/callback` through `http://localhost:9886/callback`
3. Set environment variables for client_id and client_secret

#### Manual Token Entry (Fallback)

For services without OAuth or when OAuth isn't configured:

```json
{
  "auth": {
    "secret_name": "openai_api_key",
    "display_name": "OpenAI",
    "instructions": "Get your API key from platform.openai.com/api-keys",
    "setup_url": "https://platform.openai.com/api-keys",
    "token_hint": "Starts with 'sk-'",
    "env_var": "OPENAI_API_KEY"
  }
}
```

#### Auth Flow Priority

When running `lunarwing tool auth <tool>`:

1. Check `env_var` - if set in environment, use it directly
2. Check `oauth` - if configured, open browser for OAuth flow
3. Fall back to `instructions` + manual token entry

The agent reads auth config from the tool's capabilities file and provides the appropriate flow. No service-specific code in the main agent.

### WASM Tools vs MCP Servers: When to Use Which

Both are first-class in the extension system (`lunarwing tool install` handles both), but they have different strengths.

**WASM Tools (LunarWing native)**

- Sandboxed: fuel metering, memory limits, no access except what's allowlisted
- Credentials injected by host runtime, tool code never sees the actual token
- Output scanned for secret leakage before returning to the LLM
- Auth (OAuth/manual) declared in `capabilities.json`, agent handles the flow
- Single binary, no process management, works offline
- Cost: must build yourself in Rust, no ecosystem, synchronous only

**MCP Servers (Model Context Protocol)**

- Growing ecosystem of pre-built servers (GitHub, Notion, Postgres, etc.)
- Any language (TypeScript/Python most common)
- LunarWing transports: HTTP/HTTPS, stdio, Unix domain sockets, persistent WASM
  components, and dedicated/shared gateways
- The agent registry exposes MCP tools. MCP-over-WASM is a synchronous
  client-to-server actor; LunarWing does not expose unsolicited server events,
  server-to-client requests, resources, prompts, or subscriptions through it.
  WebSocket transport is not implemented
- Cost: host-local stdio is an external process with LunarWing service-account
  privileges; an opt-in per-server OS sandbox (below) can limit its filesystem,
  network, process, and resource reach, but raw credentials given to that
  process can still be read or leaked by it

#### Current MCP support

| Area | Current behavior |
|------|------------------|
| Install | `lunarwing mcp add`, conversational `tool_install`, and the authenticated web API accept validated HTTP, stdio, WASM, and gateway config; CLI/web also retain Unix support. Registry manifests retain their existing schema; the browser form currently exposes HTTP and stdio. Install persists config but does not activate it. |
| Activate/deactivate | `ExtensionManager` initializes clients through the internal closed built-in runtime registry, discovers tools, tracks exact ownership, and tears down runtime/session/process state. This registry is not a production plugin API. Desired `enabled` state persists. CLI toggle uses the authenticated LunarWing web gateway when available and accepts `--offline` for persisted-only behavior. |
| Update | CLI `lunarwing mcp update` changes persisted config only and preserves existing WASM/gateway runtime-specific fields. Conversational and authenticated API updates attempt to restart an active server. Browser MCP panel has no update editor. |
| Remove | Web and conversational removal use `ExtensionManager`. CLI removal uses the authenticated LunarWing web gateway by default. Persisted removal happens before active cleanup; `--offline` is explicit persisted-only cleanup. |
| Diagnostics | `lunarwing doctor` validates config and reports transport, enabled state, stdio command availability, WASM component canonicality/availability, and sandbox policy enforceability. |
| Host sandboxing | Stdio supports an opt-in OS policy. MCP-over-WASM exposes no WASI or host imports and bounds artifact/message sizes, memory, replenished fuel, and epoch deadlines. |
| Protocol surface | Client capabilities are empty; inbound envelopes are strictly classified; negotiation accepts exactly `2024-11-05`; `tools/list` pagination is bounded. Negotiated `tools.listChanged` signals trigger generation-safe atomic wrapper refresh through streams and persistent HTTP GET/SSE, including gateways. Later-revision annotations deserialize canonically but remain untrusted hints, not `2024-11-05` claims. LunarWing exposes tools only. WebSocket transport is not implemented. |
| Runtime placement | Host-local process, in-daemon WASM actor, remote HTTP, Unix, external dedicated/shared gateway, and core Docker job-worker placement are implemented. A bundled gateway control-plane service is not implemented. |
| Reliability | Explicit lifecycle cleanup and whole-runtime stdio/WASM crash recovery are implemented. WASM traps stop and discard the actor before manager reconstruction; per-message deadlines and shutdown bounds are enforced. General configurable startup/request timeouts remain open. |

#### MCP-over-WASM servers

An MCP component exports `lunarwing:mcp/server@0.1.0` from
`wit/mcp-server.wit`. LunarWing keeps one component instance for the active
server and sends complete JSON-RPC envelopes to its `handle` export. The
component receives no inherited environment, filesystem, network, standard
streams, or credentials. Its synchronous ABI carries client requests and notifications
only; no server event/outbox exists.

The component path must be absolute and its final entry cannot be a symlink.
Activation uses one bounded file handle and requires a regular file no larger than
64 MiB. Defaults are 64 MiB memory,
500 million fuel per message, a 60-second message deadline, and 4 MiB request
and response envelopes:

```bash
lunarwing mcp add portable --transport wasm \
  --component /opt/lunarwing/mcp/portable.wasm \
  --wasm-memory-mb 96 \
  --wasm-fuel 500000000 \
  --wasm-timeout-seconds 30
```

Committed source WAT coverage validates initialize/initialized envelopes, persistent
actor state, paginated discovery, calls, memory and fuel enforcement, timeout, terminal
traps, and manager replacement of registry-resolved wrappers.

The authenticated web install API accepts equivalent `component`, `memory_mb`,
`fuel`, and `timeout_seconds` fields. A guest-reported MCP error preserves the
actor; a Wasmtime trap discards it, removes dead wrappers, and triggers bounded
manager recovery from current persisted intent. Use the
native invocation-scoped WASM tool ABI instead when an extension only needs one
narrow tool.

#### Dedicated and shared MCP gateways

Gateway placement expects a separately deployed Streamable HTTP MCP gateway.
Dedicated mode maps one endpoint to one server:

```bash
lunarwing mcp add hosted-tools https://gateway.example.com/mcp \
  --transport gateway --gateway-mode dedicated
```

Shared mode requires a route selected by the gateway:

```bash
lunarwing mcp add project-tools https://gateway.example.com/mcp \
  --transport gateway --gateway-mode shared \
  --gateway-route project-tools
```

LunarWing owns `X-LunarWing-MCP-Gateway-Mode` and
`X-LunarWing-MCP-Route`; configured headers cannot override them. OAuth,
custom authorization headers, Streamable HTTP session IDs, retries, and session
termination otherwise follow direct HTTP behavior. LunarWing supplies this
routing contract but does not bundle the gateway service or its cross-daemon HA,
tenant policy, and credential store.

#### Host-local stdio MCP servers

LunarWing can persist and launch an MCP server as a host-local child process. The
command and each argument are stored separately and executed without a shell.

```bash
lunarwing mcp add local-files \
  --transport stdio \
  --command local-files-mcp \
  --arg /srv/data \
  --env LOG_LEVEL=warn \
  --secret-env API_TOKEN=local_files_token
```

The same structured fields are available in the web MCP settings form and the
conversational `tool_install` tool:

```json
{
  "name": "local-files",
  "kind": "mcp_server",
  "transport": "stdio",
  "command": "local-files-mcp",
  "args": ["/srv/data"],
  "env": {"LOG_LEVEL": "warn"},
  "env_secrets": {"API_TOKEN": "local_files_token"}
}
```

Installation persists configuration only. Activation starts the child process,
negotiates MCP, and registers its tools. Treat the executable as installed code:
stdio servers run unsandboxed unless a per-server sandbox policy is configured
(see "Host sandboxing for stdio MCP servers" below). Values in `env` are plain configuration.
`env_secrets` maps environment variable names to owner-scoped LunarWing secret
names. Only names are persisted; values are decrypted at activation, zeroized from
resolver-owned memory after spawn, and exposed to the child process. No resolved
value is retained as restart configuration. Missing or wrong-owner secrets fail
activation without starting the server. Conversational installs containing secret
references and all conversational MCP updates require explicit approval.

Managed stdio children do not inherit LunarWing's full service environment. They
receive a small compatibility allowlist (`PATH`, home/user, locale, temporary and
XDG paths, TLS certificate paths) plus explicitly configured `env` and
`env_secrets`. Add other required non-secret variables explicitly. Raw secret
values remain visible to the trusted child and potentially to other processes with
the same OS privileges; use WASM tools when host-side credential injection is
required. When `env_secrets` is non-empty, LunarWing suppresses raw MCP child
stderr content to avoid copying an accidentally printed secret into daemon logs.

If an active managed stdio child exits or an MCP-over-WASM actor traps,
`ExtensionManager` removes its dead wrappers, re-reads the owner's enabled config,
starts and initializes a fresh client, rediscovers tools, and atomically registers the
replacement wrappers. Stdio secret references resolve again. Each server recovers in
its own cancellable task. Five unstable
restart attempts use jittered exponential backoff; 30 healthy seconds reset the
attempt window. Further failure opens a visible circuit and requires explicit
activation instead of restart-storming. Disable, update, remove, and application
shutdown cancel and serialize with recovery so they cannot resurrect the old runtime.
Daemon shutdown drains every active MCP client/actor and managed child. Recovery and
runtime cleanup have internal deadlines. This supervision does not install
missing commands or packages; initial activation still fails explicitly when the
configured executable or required secret is unavailable.

MCP servers can be deactivated without uninstalling them. Deactivation unloads
only that server's registered tools, closes its transport and session, stops a
managed stdio child, and persists `enabled = false`; configuration and credentials
remain available for later reactivation. Use `tool_deactivate`, the web MCP panel,
or the CLI against a running gateway:

```bash
lunarwing mcp toggle local-files --disable
lunarwing mcp toggle local-files --enable
```

The CLI uses the configured gateway and bearer token for a live transition. If the
gateway or token is unavailable, it reports the fallback and saves the state for
the next startup. Pass `--offline` to request that behavior explicitly. A custom
`--url` also requires an explicit `--token`, so a stored gateway credential is
never forwarded to an overridden origin.

Removal also uses the authenticated gateway by default so `ExtensionManager` can
stop the transport, unregister owned tools, and delete persisted configuration as
one lifecycle operation:

```bash
lunarwing mcp remove local-files
```

Removal does not fall back to offline deletion if the gateway cannot be reached.
Because a connection can fail after request delivery, inspect status before retrying.
After stopping LunarWing, `lunarwing mcp remove local-files --offline` performs an
explicit persisted-only removal. As with toggle, custom `--url` requires explicit
`--token`.

#### Host sandboxing for stdio MCP servers

A host-local stdio server can carry an opt-in OS sandbox policy, persisted on
the server configuration and enforced on every spawn — startup, manual
activation, and crash recovery share one factory path, so the policy is always
re-applied. A policy that cannot be honored fails activation before any process
starts; a configured policy never silently runs unsandboxed.

```bash
lunarwing mcp add local-files \
  --transport stdio --command local-files-mcp \
  --sandbox --sandbox-max-memory-mb 512 \
  --sandbox-read-only-root --sandbox-writable-path /var/lib/local-files
```

`--sandbox` enables the baseline: the child leads its own process group (so
shutdown kills the whole group, including grandchildren), `no_new_privileges`
blocks setuid and file-capability escalation, and a seccomp filter denies the
`ptrace`, `process_vm_readv`, and `process_vm_writev` syscalls. Granular flags
used without `--sandbox` apply only themselves: resource caps
(`--sandbox-max-memory-mb`, `--sandbox-max-cpu-seconds`, `--sandbox-max-fds`,
`--sandbox-max-processes`, `--sandbox-max-file-size-mb`). Core dumps are
disabled whenever a policy is present unless `--sandbox-allow-core-dumps` is
given.

Two stronger isolations are available on Linux:

- `--sandbox-read-only-root` uses Landlock (kernel >= 5.13) to make the entire
  filesystem read/execute-only except `--sandbox-writable-path` directories.
- `--sandbox-network-isolated` moves the child into a fresh user and network
  namespace whose only interface is a down loopback, removing all network
  access. It needs unprivileged user namespaces (`unshare -Urn true` succeeds).

Limits above the daemon's own hard ulimits are rejected at activation instead of
being silently clamped. `mcp list --verbose` shows the policy and `lunarwing
doctor` warns when a configured policy cannot be honored on this host.
`mcp update` retains the policy for stdio servers and drops it on a transport
switch; web and conversational updates reset it, like provenance.

Scope notes: resource limits, process-group isolation, and core-dump control
work on any unix; the remaining options are Linux-only and fail closed
elsewhere. The seccomp filter blocks the memory-inspection syscalls but cannot
filter same-UID `/proc/<pid>/mem` reads, which the kernel authorizes by
credential checks rather than syscalls — treat raw secrets handed to a child as
visible to the daemon account regardless. The sandbox does not virtualize
devices and does not replace package provenance: provenance answers what code
runs, sandboxing answers what it can damage.

#### Automatic npm/pip installation, provenance, and sandboxing

Automatic npm/pip MCP installation is **not implemented**. LunarWing runs the
configured stdio command directly and assumes an operator already installed it.
The MCP registry records command, arguments, and environment configuration; it
does not download, sign, upgrade, or roll back Node/Python packages.

Package provenance **is implemented** for host-local stdio and WASM servers as an
operator-driven pinning flow:

```bash
lunarwing mcp pin local-files \
  --package @example/local-files-mcp --ecosystem npm \
  --version 1.4.0 --publisher example
lunarwing mcp list --verbose     # shows trust state and digest
lunarwing mcp revoke local-files # block activation + disable
lunarwing mcp unpin local-files  # back to unverified legacy behavior
```

`mcp pin` resolves the configured command (explicit path or `PATH` search) or WASM
component, records its SHA-256 digest plus the optional package
identity (canonical id, ecosystem, immutable version, source, publisher), and
marks the server `pinned`. Every later activation — startup, manual activation,
and crash recovery alike — verifies the selected artifact and **fails closed** on
mismatch, so silently replaced or upgraded code cannot start
until the operator re-pins. `mcp revoke` records `revoked` trust (blocking
activation for any transport) and disables the server; a revoked server stays
blocked until it is re-pinned or unpinned. Servers without a provenance record
keep the legacy unverified behavior. `mcp update` retains the matching record for
stdio and WASM servers; switching runtime types drops the digest. Web and conversational update
requests do not carry provenance and reset the record — re-pin after using
them.

Atomic stdio pinning is Linux-only and accepts native executables up to 128 MiB, enforced
by metadata precheck and streaming limit. Activation executes the same verified sealed
bytes through `memfd` plus `execveat`; stored resolved path is informational only.
Shebang scripts are rejected because authenticating script bytes alone does not
authenticate their interpreter or dependency closure. Use a pinned native
launcher that owns those dependencies, or leave the script command unpinned. Pinning
on non-Linux fails before persistence; unpinned legacy stdio remains available.

What this does not yet cover: publisher signatures and trusted signing keys,
locked dependency graphs, upgrade diffs, marketplace trust roots, and
compromised-publisher revocation distribution.

Safe automatic installation would improve usability: one-command installs, exact
version selection, reproducible tenants, and managed upgrades/rollback. It also
creates a package-supply-chain execution path. npm/pip packages and dependencies can
run install/build hooks; package names can be typosquatted; publishers or registry
entries can be compromised; and unpinned dependency resolution can drift between
tenants.

Before automatic installation, package provenance must cover:

- Canonical package identity, source registry, and immutable version.
- Artifact digest, publisher signature, and trusted signing key.
- Locked dependency graph or equivalent reproducible package bundle.
- Publisher/package trust state, revocation, and compromised-version handling.
- Requested capabilities and placement, with upgrade diffs before grants expand.
- Transactional install, health check, rollback, and auditable install history.

Provenance answers **what code is this and who supplied it**. It does not prove code
is safe. Installation and runtime still need containment: non-root execution,
isolated writable paths, controlled network egress, bounded CPU/memory/processes,
no implicit host devices or daemon credentials, and reliable teardown. The opt-in
host sandbox above covers the resource, filesystem, network, and process subsets
for stdio servers; device isolation and mandatory default sandbox profiles remain
open. Sandboxing
answers **what can this code damage if it is malicious or compromised**. Neither
control replaces the other.

Automatic install is optional, not required for MCP support. Operators can keep
using manually installed pinned commands, verified extension bundles, or prebuilt
worker images. Preinstalled or verified packages are the safer prerequisite for a
worker-local prototype; arbitrary network package installation during a job should
not be the default.

#### Worker-local MCP

Core Docker job workers support explicit `job_worker` placement for stdio MCP
servers. Existing configs remain `host` by default. Configure a preinstalled command:

```bash
lunarwing mcp add project-tools \
  --transport stdio --placement job-worker \
  --command project-tools-mcp \
  --arg /workspace \
  --secret-env API_TOKEN=project_tools_token
```

Activation marks this server eligible for future core Docker jobs; it does not
start a host process. `create_job` grants none by default. Pass exact enabled
worker server names through `mcp_servers`, for example
`{"mcp_servers":["project-tools"]}`. Each job snapshots only that ordered
selection for its owner. Duplicate, missing, disabled, host-placed, or
circuit-open selections fail closed. The snapshot is immutable for that job,
requires a mounted `/workspace`, and is limited to eight servers and 32 secret
grants. Persistent external workers do not receive this placement.

Restart metadata persists selected server names, not secret values or mutable
server descriptors. Restart reloads owner configuration and re-resolves those
same names; removed, disabled, or moved servers reject restart with a conflict
instead of silently broadening or dropping capability. Legacy jobs stored before
explicit selection restart with no worker MCP access.

Only stdio is accepted. Command and arguments execute without a shell, and the
command must already exist in the worker image. Pinned provenance is checked against
the executable visible inside that image before spawn. Automatic npm/pip installation
does not occur. Host-only OAuth, request headers, Unix/WASM/gateway transports, and
host sandbox policy are rejected for worker placement.

Secret names are removed from the descriptor and retained as separate job-scoped
grants. The worker fetches values through a dedicated job-token-authenticated route;
descriptor and value coverage must match exactly. MCP credentials do not enter the
normal worker shell environment, but the MCP child receives its explicit values and
can read or leak them. Resolved values are retained as `SecretString` in trusted
worker memory for the job lifetime so post-start restart supervision can re-spawn a
crashed child without re-fetching grants; they are zeroized when the runtime drops
at job end.

Worker startup initializes every client and discovers every tool before atomically
registering wrappers in the per-job registry. Paginated discovery is bounded across
all pages. Any startup error, tool-name conflict,
approval requirement, autonomous-execution denial, or partial failure rejects the
whole MCP snapshot and cleans up started runtimes. Stdio JSON-RPC frames are capped
at 4 MiB; the full job snapshot may expose at most 256 tools and 2 MiB of serialized
tool descriptors. Older worker images receive
`--require-worker-mcp` and fail rather than silently ignoring requested placement.

MCP children receive a scrubbed environment without the worker bearer token. On
Linux, the trusted worker parent marks itself non-dumpable before any untrusted child
starts. Worker MCP activation additionally requires Yama
`kernel.yama.ptrace_scope >= 1`, blocking same-UID sibling MCP and shell processes
from reading each other's environments or memory after `execve`. Failure to install
parent protection or verify sibling isolation prevents worker startup.

After activation, one supervisor task per server watches its managed stdio
child. An unexpected exit, closed stdio, or missing runtime restarts the server
from the job's immutable snapshot: the dead runtime is cleaned up with bounded
waits, a fresh client is initialized, tools are rediscovered and revalidated
against the same autonomous-execution and aggregate limits, and replacement
wrappers are atomically swapped into the registry. Old wrappers remain registered
until replacement is ready, so calls during recovery fail against the dead client
rather than exposing a partial registry. Discovery captures exact notification generation,
so a concurrent `tools.listChanged` remains pending instead of being lost. Five unstable
restart attempts use jittered exponential backoff (1s base, 30s cap); 30 healthy seconds reset the attempt
window. Further failure persists terminal circuit state, removes dead wrappers, and
stops supervision for that server. Supervision never reloads owner configuration or re-fetches grants; the
job snapshot stays immutable.

Before job completion, the worker cancels supervision first so a restart cannot
resurrect a runtime mid-teardown, then unregisters exact wrappers, closes
clients and sessions, and stops child processes with bounded waits. Cleanup
failure makes the job fail. Job/container termination provides the final
cleanup boundary.

Benefits:

- Direct access to that job's `/workspace` and installed language toolchains.
- Isolation of arbitrary server code from the LunarWing daemon and host filesystem.
- Job-scoped files, credentials, package versions, limits, and teardown.
- Safe coexistence of jobs requiring conflicting dependencies or server versions.
- MCP child failures remain isolated from the host daemon and can be reported
  separately from worker failure.
- Strong fit for repository inspection, code generation, testing, and project-local
  native tools.

Worker-local MCP is useful when a server must operate on the current project, needs
native dependencies, or should not be trusted with LunarWing service-account host
privileges.
It is unnecessary for a trusted operator-installed host process, an existing hosted
HTTP MCP endpoint, or a durable heavyweight service better placed in a dedicated
worker or gateway.

Costs include per-job startup, image/package availability, descriptor and grant
delivery, credential exposure inside the MCP child, and resolved secret values
retained in worker memory for the job lifetime to support restart supervision.
Worker isolation limits blast radius but does not remove package
provenance needs. Add automatic package installation only after provenance,
capability review, sandbox, audit, revocation, upgrade, and rollback contracts exist.

#### LunarWing Study Notes

> **v2.0.6.0+:** the native `knowledge_*` built-in tools
> (`knowledge_search`, `knowledge_get_topic`, `knowledge_submit_note`,
> `knowledge_list_stale`, `knowledge_stats` — see
> `src/workspace/README.md` § Study-Notes Knowledge Store) are the primary
> study-notes path and need no external server. The bundled `study-notes`
> skill (v3) uses them directly. The external lw2studymcp setup below remains
> for backward compatibility with existing deployments.

LunarWing can seed the shared [lw2studymcp](https://git.lunarwing.org/cmc/lw2studymcp)
checkout as a managed stdio server. Read-only mode is default. Clone it at an
absolute, stable path, verify its database, then set `LW2STUDYMCP_HOME`:

```bash
git clone https://git.lunarwing.org/cmc/lw2studymcp.git /srv/lunarwing/lw2studymcp
python3 /srv/lunarwing/lw2studymcp/memorydb.py stats
export LW2STUDYMCP_HOME=/srv/lunarwing/lw2studymcp
```

Startup validates the canonical layout and creates or refreshes its managed
`study_notes` config. Refresh preserves a disabled managed config. A user-owned
config with the same name wins. (Before v2.0.6.0 the bundled skill was gated on
`LW2STUDYMCP_HOME`; the v3 skill uses the native tools and is always available.)

Write mode requires mutable state separate from executable code and skill content:

```bash
install -d /srv/lunarwing/study-notes-state
cp -p /srv/lunarwing/lw2studymcp/agent-notes.sqlite3 \
  /srv/lunarwing/study-notes-state/
cp -a /srv/lunarwing/lw2studymcp/agent-notes-by-agent \
  /srv/lunarwing/study-notes-state/
export LW2STUDYMCP_STATE_HOME=/srv/lunarwing/study-notes-state
export LW2STUDYMCP_ALLOW_WRITE=1
```

This adds `knowledge_submit_note` and `knowledge_update_status`. LunarWing marks
both as `Always` approval tools. Code remains outside mutable state, preventing a
note-writing agent from replacing MCP code or trusted skill instructions.

Multi-tenant hosts should configure the same readable checkout through mt-admin:

```bash
sudo scripts/lunarwing-mt-admin.sh configure-study-mcp TENANT \
  --home /srv/lunarwing/lw2studymcp
sudo scripts/lunarwing-mt-admin.sh restart-tenant TENANT
```

Add `--allow-write` to create and seed isolated state at
`<tenant-state-dir>/study-notes`. Use `--state-home <absolute-path>`
for operator-managed state. Existing shared state must already grant tenant read
and write access. Sharing one state path across tenants makes every participant a
trusted writer whose notes become prompt input for all other participants.

```bash
sudo scripts/lunarwing-mt-admin.sh configure-study-mcp TENANT \
  --home /srv/lunarwing/lw2studymcp --allow-write
sudo scripts/lunarwing-mt-admin.sh restart-tenant TENANT
```

Write tools are exposed only to core LunarWing agents, where every mutation gets
per-call approval. Podman workers always receive code and current state read-only
at `/opt/lw2studymcp` and `/var/lib/lw2studymcp`. This prevents OpenCode's broad
shell/edit permissions from bypassing MCP validation. Workers see core writes on
later reads. On SELinux hosts, mt-admin applies `container_file_t` to both mounts.

Rerun `configure-study-mcp` without `--allow-write` to disable mutation tools while
retaining latest state for reads. Back up both state database and notes directory
while tenant is stopped, or use one filesystem snapshot. State is not automatically
committed back to the code checkout. Default per-tenant state is included in tenant
state exports; custom external state must be backed up separately. Database updates
become visible per request; restart LunarWing/OpenCode only after server or skill
code changes.

**Decision guide:**

| Scenario | Use |
|----------|-----|
| Good MCP server already exists | **MCP** |
| Handles sensitive credentials (email send, banking) | **WASM** |
| Quick prototype or one-off integration | **MCP** |
| Core capability you'll maintain long-term | **WASM** |
| Needs background connections (websockets, polling) | **MCP** |
| Multiple tools share one OAuth token (e.g., Google suite) | **WASM** |

The LLM-facing interface is identical for both (tool name, schema, execute), so swapping between them is transparent to the agent.

## Autonomous Job Iterations

Local and Docker `create_job` workers default to 200 agent-loop iterations.
Callers may set `max_iterations` from 1 through 500; large multi-fix write jobs
should usually request 300 or more. Named external workers such as OpenCode,
OpenCode, Kimi Code, and Pebble control their own iteration policy and reject this option.

This budget is separate from interactive chat's `AGENT_MAX_TOOL_ITERATIONS`.
