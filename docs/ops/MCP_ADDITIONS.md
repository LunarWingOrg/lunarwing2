# MCP Additions Status

> **Current status (2026-08-01): PARTIAL.** HTTP/HTTPS plus host-local stdio and
> Unix MCP; owner-scoped secret references; diagnostics; and explicit lifecycle
> operations are implemented. Worker-local placement, automatic package
> installation, package provenance, OS sandboxing for host-local stdio, safe crash
> recovery, runtime adapters, MCP-over-WASM, and a dedicated MCP gateway are not
> implemented.

## Implemented

### Host-local installation and activation

- Typed stdio config stores command, structured arguments, plain non-secret
  environment, and secret-name references. Command execution does not use a shell.
- Registry CLI, `lunarwing mcp add`, conversational `tool_install`, authenticated
  web API, and browser MCP form accept structured MCP configuration.
- Installation persists configuration only. Activation initializes MCP, discovers
  tools, and registers owner-scoped wrappers.
- HTTP/HTTPS, stdio, and Unix domain socket transports are supported. Current MCP
  integration exposes tools, not resources, prompts, subscriptions, or WebSocket
  transport.

### Lifecycle and diagnostics

- Activate/deactivate persist desired `enabled` state, preserve configuration, and
  track exact tool ownership. Deactivation unregisters owned tools and closes
  client, session, transport, and managed child resources.
- Web and conversational update/remove operations route through
  `ExtensionManager`. Active conversational or authenticated API updates stop and
  attempt to reactivate the server with new config. Config remains updated when
  reactivation fails, so callers must inspect returned message and active status.
- CLI `mcp update` changes persisted configuration only. CLI toggle uses the live
  gateway when available, automatically saves desired state for next startup when
  unavailable, and accepts `--offline` to request persisted-only behavior.
- CLI removal uses the authenticated gateway by default. It does not silently fall
  back to config-only deletion; `--offline` is explicit persisted-only cleanup.
- `lunarwing doctor` reports MCP config validity, transport, enabled state, and
  stdio executable availability.

### Owner-scoped secret environment references

- Stdio `env_secrets` maps child environment variable names to owner-scoped
  LunarWing secret names. Only names are persisted.
- CLI uses `--secret-env KEY=SECRET_NAME`; web and conversational requests use an
  `env_secrets` object.
- Activation resolves values for the activating owner and fails before spawn when
  the secret store or requested secret is unavailable.
- Resolver-owned temporary values use `SecretString`. No resolved values are kept
  as restart configuration.
- Plain and secret mappings for the same environment variable are rejected.
- Stdio children receive an allowlisted compatibility environment plus explicit
  config instead of inheriting daemon credentials. Raw child stderr content is
  suppressed when secret references are present.
- Secret-bearing conversational installs and all conversational MCP updates require
  approval. Update paths preserve forced-approval policy.
- `mcp test --user` resolves secrets for the selected owner and explicitly cleans
  its temporary runtime.

## Not Implemented

| Feature | Current status and reason |
|---------|---------------------------|
| Automatic npm/pip installation | No production package installer exists. LunarWing runs an operator-installed command. Safe automation needs immutable package identity, digest/signature verification, dependency locking, capability review, sandboxing, audit, revocation, health checks, and rollback. |
| MCP package provenance | WASM artifact checksums do not authenticate stdio commands or their npm/pip dependency trees. Publisher trust, signatures, lock data, upgrade diffs, and revocation remain design work. |
| Host-local stdio OS sandbox | Environment inheritance is restricted, but the child still runs with LunarWing service-account privileges. Environment filtering is not filesystem, network, process, device, or resource isolation. |
| Generic worker-local MCP | Core per-job workers do not load LunarWing MCP configs or register MCP wrappers. External workers may independently use MCP, but that is a separate integration, not placement of installed LunarWing MCP packages. |
| Automatic crash recovery | No child supervisor safely rebuilds the MCP client and tool registry. Restarting only the process leaves wrappers bound to a dead transport. |
| Runtime adapter boundary | Process, WASM, worker, remote, and gateway lifecycle remain separate concrete paths rather than one runtime-neutral interface. |
| MCP-over-WASM | Existing WASM tool ABI is invocation-scoped and does not implement MCP server sessions, resources, prompts, notifications, or subscriptions. |
| Dedicated MCP gateway | Browser/CLI lifecycle routes use the LunarWing gateway, but no separate shared MCP control plane provides cross-daemon sessions, package placement, or centralized policy. |
| Per-server lifecycle timeouts | Configurable startup, request, and shutdown timeout controls remain deferred. |

## Why Package Provenance and Sandboxing Matter

Automatic installation would provide one-command setup, exact version selection,
reproducible tenants, and managed upgrades/rollback. It would also execute package
manager lifecycle code and dependency trees. Typosquatting, compromised publishers,
mutable registry state, and dependency drift therefore become LunarWing execution
risks.

Provenance answers **what code is this and who supplied it** through package source,
immutable version, digest, publisher signature, trust state, dependency lock, and
revocation. Sandboxing answers **what can this code damage** through non-root
execution, isolated writable paths, controlled network, bounded resources, scoped
credentials, and reliable teardown. Signing does not prove code safe; sandboxing
does not prove identity. Both are required before arbitrary npm/pip installation is
a safe default.

Automatic installation is optional. Manually installed pinned commands, verified
bundles, or prebuilt worker images remain valid and safer deployment choices. See
[`ic/src/tools/README.md`](../../ic/src/tools/README.md#automatic-npmpip-installation-provenance-and-sandboxing)
for operator-facing details.

## Why Worker-Local MCP Matters

Worker-local placement would run an approved server beside a job's `/workspace` and
toolchain. With enforced mounts, network policy, grants, limits, and teardown, it
can isolate arbitrary code from the daemon host, scope files and raw credentials to
one job, permit conflicting package versions, and bind runtime lifetime to the job.
It best fits repository inspection, code generation, testing, native dependencies,
and other workspace-coupled tools.

It is not needed for trusted operator-installed host processes, hosted HTTP MCP, or
durable heavyweight services better placed in dedicated workers. Costs include
per-job startup, package/image availability, descriptor and grant delivery,
separate worker/MCP health, credential exposure inside the worker, and teardown
reliability. Worker isolation reduces blast radius but does not remove provenance
requirements. First prototype should use preinstalled or verified packages, not
arbitrary package-manager downloads during jobs.

## Rejected Reference Implementation

`rarity/kimi-19-20260730-1801` was reviewed as untrusted reference and must not be
cherry-picked wholesale:

- Its crash supervisor replaces a process-manager transport while `McpClient` and
  registered wrappers remain bound to the dead transport. Disable/shutdown races
  can also resurrect the process.
- Its npm/pip path executes unverified package lifecycle code under the LunarWing
  service account without provenance, pinning, sandboxing, or rollback.
- Its restart config retains resolved secret values.

The safe secret-reference design was reimplemented independently. The unused
process-only `try_restart` cache was removed. Correct recovery must recreate and
initialize the client, rediscover tools, atomically replace wrappers, publish
accurate status, and obey lifecycle cancellation generations.

## Historical Milestones

- 2026-07-09: first-class host-local stdio installation surfaces completed.
- 2026-07-21 through 2026-07-24: registry validation, diagnostics,
  deactivate/reactivate, and in-place update lifecycle completed.
- 2026-07-31: owner-scoped `env_secrets` and child-environment hardening completed.
- 2026-08-01: CLI removal routed through live `ExtensionManager` cleanup with
  explicit `--offline` behavior.
