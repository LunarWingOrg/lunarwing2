# MCP Additions Status

> **Current status (2026-08-09): PARTIAL.** HTTP/HTTPS, host-local stdio, Unix,
> stateful MCP-over-WASM, and dedicated/shared gateway adapters are implemented
> behind one runtime-neutral activation registry. Owner-scoped secret references,
> diagnostics, explicit lifecycle operations, safe stdio/WASM crash recovery,
> operator-pinned package provenance, and opt-in host OS sandboxing are also
> implemented.
> Worker-local placement, automatic package installation, publisher signatures
> and dependency locking, device-level sandboxing, mandatory default sandbox
> profiles, and a bundled shared gateway service remain unimplemented.

For the concise checkpoint, remaining-validation list, and deferred-work summary,
see [`MCP_OVERALL_STATUS.md`](MCP_OVERALL_STATUS.md).

## Implemented

### Host-local installation and activation

- Typed stdio config stores command, structured arguments, plain non-secret
  environment, and secret-name references. Command execution does not use a shell.
- Registry CLI, `lunarwing mcp add`, conversational `tool_install`, authenticated
  web API, and browser MCP form accept structured MCP configuration.
- Installation persists configuration only. Activation initializes MCP, discovers
  tools, and registers owner-scoped wrappers.
- HTTP/HTTPS, stdio, Unix domain socket, stateful WASM component, and gateway
  transports are supported. The agent registry currently exposes MCP tools; the
  raw WASM actor ABI preserves resources, prompts, notifications, subscriptions,
  and future JSON-RPC methods for clients that invoke those methods.

### Lifecycle and diagnostics

- Activate/deactivate persist desired `enabled` state, preserve configuration, and
  track exact tool ownership. Deactivation unregisters owned tools and closes
  client, session, transport, and managed child resources.
- Web and conversational update/remove operations route through
  `ExtensionManager`. Active conversational or authenticated API updates stop and
  attempt to reactivate the server with new config. Config remains updated when
  reactivation fails, but unchanged persisted intent is disabled until explicit
  activation, so callers must inspect returned message and active status.
- CLI `mcp update` changes persisted configuration only. CLI toggle uses the live
  gateway when available, automatically saves desired state for next startup when
  unavailable, and accepts `--offline` to request persisted-only behavior.
- CLI removal uses the authenticated gateway by default. It does not silently fall
  back to config-only deletion; `--offline` is explicit persisted-only cleanup.
- `lunarwing doctor` reports MCP config validity, transport, enabled state, and
  stdio executable availability.
- A manager-level supervisor detects unexpected host-local stdio exits and terminal
  WASM traps. It first
  unregisters the dead wrappers, then re-reads owner-scoped persisted intent,
  resolves secret references again, starts and initializes a fresh client,
  rediscovers tools, and atomically installs the replacement wrappers.
- Recovery is serialized with activate, update, deactivate, remove, and OAuth
  lifecycle work for the same server. Each server uses its own cancellable recovery
  task, so a hung server does not block recovery of another server. Monotonic runtime
  generations reject stale work, so a disabled, removed, updated, or shutting-down
  server is not resurrected.
- Recovery permits five automatic restart attempts, with jittered exponential backoff
  capped at 60 seconds. A runtime healthy for 30 seconds starts a new attempt window.
  Repeated failure opens the circuit: dead wrappers remain absent, status reports a
  failure, and automatic restart stops until explicit activation retries the server.
- Recovery and child-cleanup stages have internal deadlines. Transport and process
  shutdown each have a 30-second bound. Application shutdown
  cancels and bounds all per-server tasks before concurrently stopping live clients,
  WASM actors, sessions, and managed children. Cleanup therefore still runs when
  the agent loop returns an error.
- Activation failure rolls desired `enabled` state back to false when persisted intent
  is unchanged. Update and removal persist new intent before runtime teardown, closing
  crash windows that could otherwise restore stale configuration.
- Startup, manual activation, and recovery use the same all-or-none tool registration
  rule. Any collision rejects and cleans up the whole MCP runtime rather than exposing
  a partial tool set that cannot recover consistently. Startup conflicts also disable
  unchanged persisted intent so status does not remain enabled without supervision.

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

### Package provenance (operator-pinned)

- `McpServerConfig` carries an optional provenance record: canonical package
  identity, ecosystem (npm/PyPI/binary), immutable version, source, publisher,
  resolved stdio executable or WASM component path, artifact SHA-256, trust state
  (`unverified`/`pinned`/`revoked`), and a recorded timestamp.
- `lunarwing mcp pin` resolves the configured stdio command or WASM component,
  records its digest and any supplied
  package identity, and marks the server pinned. `mcp unpin` removes the record;
  `mcp revoke` records revoked trust and disables the server (persisted first,
  then a live gateway disable unless `--offline`). `mcp list --verbose` shows
  trust state, identity, and digest.
- Activation fails closed: a revoked package is blocked for every transport. Pinned
  stdio executables are verified before spawn; pinned WASM bytes are verified both
  before and during bounded artifact loading, closing the path-swap window. Startup,
  manual activation, and recovery share this factory path. Digest mismatch or
  revocation blocks activation with a provenance error.
- Servers without a record keep legacy unverified behavior. `mcp update`
  retains the matching stdio or WASM record; switching runtime types drops the digest
  and demotes pinned to unverified. Web/conversational updates do not carry
  provenance and reset the record; re-pin after using them.
- Not yet covered: publisher signatures, trusted signing keys, locked
  dependency graphs, upgrade diffs, marketplace trust roots, and revocation
  distribution for compromised publishers.

### Host OS sandboxing for stdio children

- `McpServerConfig` carries an optional `sandbox` policy (stdio-only, like the
  provenance digest rule). `lunarwing mcp add --sandbox` enables the baseline;
  granular `--sandbox-*` flags set individual restrictions.
- Portable (any unix): setrlimit caps for memory, CPU seconds, file
  descriptors, process count, and file size; core dumps disabled unless
  explicitly allowed; a request above the daemon's hard limit fails activation
  instead of clamping; process-group isolation makes the child a group leader
  so shutdown SIGKILLs the whole group and cannot orphan grandchildren.
- Linux-only, failing closed when requested elsewhere: `no_new_privileges`
  (PR_SET_NO_NEW_PRIVS), a seccomp filter denying `ptrace`/
  `process_vm_readv`/`process_vm_writev`, Landlock read-only root with explicit
  writable paths (kernel >= 5.13), and network isolation via a fresh
  user+network namespace with a down loopback.
- Enforcement runs in the fork/exec window from state prepared before the
  fork, and every spawn path (startup, manual activation, crash recovery)
  shares the factory, so the policy is always re-applied. A policy that cannot
  be honored fails activation before any child starts.
- `mcp list --verbose` shows the policy; `lunarwing doctor` warns when the
  host cannot honor it. `mcp update` retains it for stdio servers and drops it
  on transport switch; web/conversational updates reset it, like provenance.
- Limits: the seccomp filter blocks the memory-inspection syscalls but not
  same-UID `/proc/<pid>/mem` reads (a kernel credential check, not a syscall);
  `PR_SET_DUMPABLE` does not survive `execve`, so inbound memory-read
  protection cannot be applied from the spawn path. Device isolation and
  mandatory default sandbox profiles remain future work.

### Runtime adapters, MCP-over-WASM, and gateways

- `ic/src/tools/mcp/runtime.rs` defines the public `McpRuntimeAdapter` boundary,
  activation context/request types, stable runtime kinds, typed activation errors,
  and an extensible registry. The factory validates persisted intent and provenance
  once, then dispatches HTTP, stdio, Unix, WASM, or gateway activation through the
  registry. Every adapter returns the same `McpClient`, preserving discovery,
  wrapper ownership, approval, update, deactivation, and shutdown behavior.
- `ic/wit/mcp-server.wit` defines a stateful actor ABI carrying complete JSON-RPC
  envelopes. One component instance is retained for an active server and requests
  are serialized through it, so initialization state and subscriptions can persist.
  The linker exposes no WASI or other host imports, so guest code receives no
  environment, filesystem, network, standard streams, or credentials. Activation
  requires a bounded, single-open, non-symlink regular component artifact and
  enforces a 64 MiB artifact cap. Per-message
  limits cover memory, replenished fuel, epoch interruption, a 1-300 second timeout,
  and 4 MiB request/response envelopes. A trap permanently stops that actor rather
  than reusing possibly-corrupt state; manager supervision removes dead wrappers and
  reconstructs a fresh actor from current persisted intent.
- Gateway config explicitly distinguishes `dedicated` and `shared` modes. Both use
  Streamable HTTP and retain normal session/OAuth behavior. The transport owns
  `X-LunarWing-MCP-Gateway-Mode`; shared mode additionally requires and injects
  `X-LunarWing-MCP-Route`. User headers cannot override either routing header.
  A gateway service may therefore dedicate an endpoint to one server or route many
  servers while LunarWing uses the same client lifecycle. The service itself remains
  independently deployed; LunarWing does not concentrate gateway credentials or
  cross-tenant policy inside the daemon.
- `lunarwing mcp add/list/test`, `lunarwing doctor`, conversational `tool_install`,
  startup activation, `ExtensionManager`, and the authenticated web install API
  understand the new runtimes. Example installs:

  ```bash
  lunarwing mcp add portable --transport wasm \
    --component /opt/lunarwing/mcp/portable.wasm
  lunarwing mcp add shared-tools https://gateway.example.com/mcp \
    --transport gateway --gateway-mode shared --gateway-route project-tools
  ```

## Not Implemented

| Feature | Current status and reason |
|---------|---------------------------|
| Automatic npm/pip installation | No production package installer exists. LunarWing runs an operator-installed command. Safe automation needs immutable package identity, digest/signature verification, dependency locking, capability review, sandboxing, audit, revocation, health checks, and rollback. Provenance pinning (below) covers the identity/digest/trust subset; the rest remains. |
| MCP package provenance | Partially implemented: operator-pinned stdio executable and WASM component digests, package identity, trust state, and revocation. Publisher signatures, signing-key trust roots, dependency lock data, upgrade diffs, and marketplace revocation remain design work. |
| Host-local stdio OS sandbox | Partially implemented: opt-in per-server policy covering resource limits, core-dump control, process-group isolation and group teardown (all unix), plus Linux-only no_new_privileges, seccomp ptrace/process_vm denial, Landlock read-only root with writable paths, and user+network namespace network isolation. Remaining: device isolation, mandatory default profiles, and same-UID `/proc/<pid>/mem` inbound reads (kernel credential check, not syscall-filterable). |
| Generic worker-local MCP | Core per-job workers do not load LunarWing MCP configs or register MCP wrappers. External workers may independently use MCP, but that is a separate integration, not placement of installed LunarWing MCP packages. |
| Bundled gateway service | LunarWing implements dedicated/shared gateway client placement and a protected routing contract, but does not bundle or operate the external gateway control-plane service. Cross-daemon HA, centralized package policy, and cross-tenant credential ownership belong to that deployment. |
| Per-server lifecycle timeouts | WASM message deadlines are configurable and transport/process shutdown is bounded. Configurable startup and request timeout controls for every transport remain deferred. |

## Historical v2.0.4 Release Boundary and Assumptions

At the v2.0.4 boundary, item 14 closed only the **host-local lifecycle** scope.
The following bullets record that historical boundary; the runtime adapter,
MCP-over-WASM, and gateway-adapter deferrals were completed on 2026-08-09:

- **Host-local recovery only:** supervision applies to an already activated stdio
  runtime owned by this `ExtensionManager`. HTTP and Unix transports keep their
  existing lifecycle. Initial activation failures remain explicit errors rather than
  silently starting an unbounded installer or fallback runtime.
- **Persisted intent remains authoritative:** every recovery re-reads the server for
  its owner. Missing, disabled, removed, or non-stdio configuration cancels recovery.
  Secret-name references are resolved afresh; decrypted values are not cached as
  restart descriptors.
- **Crash loops fail closed:** five unstable restart attempts are allowed. The next
  failed health check opens the circuit and requires explicit activation. A manual
  lifecycle operation cancels in-flight recovery before taking the server lifecycle
  lock.
- **No automatic npm/pip execution:** the rejected implementation's `npm install`
  and `pip install` behavior is intentionally absent. A package manager must not run
  publisher lifecycle code as the LunarWing service user until immutable identity,
  verification, locking, containment, audit, revocation, and rollback contracts
  exist.
- **No generic worker placement:** the job worker still receives no installed MCP
  descriptor or evaluated placement grants. A future prototype must use
  preinstalled or verified packages and define workspace, network, credential,
  resource, health, and teardown behavior before registration in the worker-local
  tool registry.
- **No claimed general runtime adapter:** client preparation and atomic tool
  replacement are shared inside the MCP lifecycle to make recovery correct. This is
  deliberately smaller than a public adapter interface spanning WASM, process,
  worker, remote, and gateway runtimes.
- **Existing gateway is not a shared MCP gateway:** authenticated browser and CLI
  lifecycle routes still target one LunarWing daemon. Cross-daemon sessions,
  multi-tenant placement, centralized package policy, shared credentials, routing,
  and gateway high-availability semantics remain future architecture work.

The public adapter registry, stateful WASM actor, and gateway routing contract now
supersede the last two historical bullets. Worker-local placement and safe automatic
package installation remain separate security and ownership boundaries.

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

The safe secret-reference design and manager-level recovery were implemented
independently. The unused process-only `try_restart` cache remains removed. Recovery
now recreates and initializes the client, rediscovers tools, atomically replaces
wrappers, publishes accurate status, and obeys lifecycle cancellation generations;
resolved secret values are never retained as restart configuration.

## Historical Milestones

- 2026-07-09: first-class host-local stdio installation surfaces completed.
- 2026-07-21 through 2026-07-24: registry validation, diagnostics,
  deactivate/reactivate, and in-place update lifecycle completed.
- 2026-07-31: owner-scoped `env_secrets` and child-environment hardening completed.
- 2026-08-01: CLI removal routed through live `ExtensionManager` cleanup with
  explicit `--offline` behavior.
- 2026-08-03: manager-level stdio crash recovery added with fresh secret resolution,
  full client initialization, atomic tool replacement, per-server cancellable tasks,
  bounded jittered retry, crash-loop circuit breaking, generation checks, accurate
  status, and deadline-bounded shutdown ordering that prevents resurrection.
- 2026-08-08: opt-in host OS sandboxing for stdio children added — rlimits and
  process-group isolation on unix; no_new_privileges, seccomp ptrace/process_vm
  denial, Landlock read-only root with writable paths, and user+network namespace
  network isolation on Linux; fail-closed activation, CLI and doctor surfaces.
- 2026-08-09: public runtime adapter registry, stateful MCP-over-WASM actor ABI,
  and dedicated/shared gateway routing contract added with CLI, conversational and
  web API, diagnostics, startup, lifecycle, and focused test coverage. Review
  hardening removed WASI imports, pinned exact component bytes, moved guest work off
  Tokio workers, added terminal-trap recovery and whole-runtime daemon shutdown, and
  verified gateway routing headers on wire.
