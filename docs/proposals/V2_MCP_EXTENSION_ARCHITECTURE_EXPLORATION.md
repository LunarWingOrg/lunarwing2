# V2 MCP and Extension Runtime Architecture Exploration

> **Current status (2026-08-15): PARTIAL / IMPLEMENTED INCREMENTALLY.** Host-local
> stdio lifecycle, an internal built-in runtime-dispatch registry, stateful
> MCP-over-WASM, dedicated/shared gateway client placement, and an optional bundled
> gateway control/data plane are implemented. Core Docker job workers also support
> explicit worker-local stdio placement with immutable owner snapshots, separate
> credential grants, and bounded lifecycle. The runtime-neutral package model,
> persistent-worker MCP delivery, a marketplace install/upgrade flow, automatic
> package installation, and cross-daemon gateway HA remain open.

**Status:** Exploratory V2 architecture with implemented runtime slices; host-local
stdio landed on 2026-07-09, lifecycle/recovery hardening landed through 2026-08-03,
and the adapter/WASM/gateway runtime work landed on 2026-08-09.
**Date:** 2026-07-09  
**Scope:** Possible V2 extension packaging, MCP compatibility, execution placement,
and tenant policy

## Executive Summary

LunarWing should not choose between "MCP over WASM," "MCP in a worker," and an
MCP gateway as if they were mutually exclusive extension models. They solve
different parts of the problem.

The stronger V2 direction is to define one signed, declarative extension package
and capability model, then support multiple runtime and placement options beneath
it. MCP would remain an important compatibility protocol, but it would not become
the native abstraction for every LunarWing extension.

The proposed conceptual split is:

| Layer | Responsibility |
| --- | --- |
| Package | Identity, publisher, version, artifacts, signatures, and updates |
| Policy | Requested capabilities, tenant grants, credentials, approvals, and limits |
| Runtime | WASM component, local process, dedicated worker, embedded module, or remote service |
| Protocol | Native LunarWing ABI, MCP, channel callbacks, webhooks, or worker protocol |

Under this model:

- Remote HTTP MCP remains appropriate for hosted integrations.
- Local stdio MCP can continue to run as a managed child process where host-local
  execution is acceptable.
- Worker-hosted MCP becomes an explicit placement for project-local, native,
  stateful, or computationally heavy servers.
- MCP-over-WASM becomes a compatibility runtime for portable synchronous MCP tool
  servers, not the default way to write a small LunarWing tool. Broader server events,
  requests, resources, prompts, and subscriptions need a future host event ABI.
- Native WASM tools remain the preferred shape for narrow, capability-scoped tool
  integrations.
- Embedded modules remain exceptional and first-party.
- A gateway remains useful for shared OAuth, policy enforcement, remote sessions,
  auditing, and routing, without becoming the only place an MCP capability can run.

## Why This Is Worth Exploring

LunarWing already supports several extension and delegation mechanisms:

- Built-in Rust tools for tightly coupled runtime behavior.
- WASM tools with capability-scoped host functions.
- WASM channels with lifecycle callbacks and background polling behavior.
- MCP clients using HTTP, stdio, or Unix socket transports.
- Per-job Docker workers.
- Persistent external workers speaking `lunarwing-agent-v1`.

These mechanisms have grown around different needs. V2 is an opportunity to give
them a common package, policy, and lifecycle vocabulary without forcing them into
one execution technology.

The immediate motivation is local MCP deployment. Some MCP servers are distributed
as stdio programs rather than hosted HTTP services. Others need access to the job
workspace, native dependencies, durable state, or long-lived connections. A single
gateway or host-process model does not fit every case.

## Current Implementation Findings

### MCP already supports multiple local and remote transports

The core MCP configuration and CLI support:

- HTTP/HTTPS
- stdio child processes
- Unix domain sockets

`ic/src/tools/mcp/factory.rs` dispatches these configurations to the appropriate
transport. `McpProcessManager` owns stdio process startup, tracking, and shutdown.
The application startup path loads enabled MCP configurations, discovers their
tools, wraps them as LunarWing `Tool` implementations, and registers them in the
main tool registry.

This means the broad finding in `docs/proposals/SUPERGATEWAY_MCP.md` that LunarWing
has no stdio MCP support is stale relative to the current source. The registry,
conversational `tool_install`, web API/settings, and registry CLI now accept
structured host-local stdio configuration. Installation persists the configuration
without running it; activation starts the managed child and discovers its tools.
A supergateway can still be operationally useful, but it is not required to
connect a stdio server.

The shipped lifecycle now tracks exact tool ownership and supports explicit
activate/deactivate, in-place update, and remove. Deactivation closes transport,
session, client, and managed process resources while preserving config. Active
conversational or authenticated web API updates attempt restart through
`ExtensionManager`; config remains updated if reactivation fails. CLI `mcp update`
remains persisted-config-only. CLI toggle and remove can use the authenticated
LunarWing gateway, but this is not the dedicated shared MCP gateway described later
in this proposal. CLI removal refuses an implicit config-only fallback; `--offline`
is explicit.

Stdio configs can map child environment variable names to owner-scoped LunarWing
secret names. Values are resolved only during activation and are not persisted in
MCP config or retained as restart data. Child processes receive an allowlisted OS
environment rather than inheriting daemon credentials. This is raw credential
exposure to trusted, unsandboxed code, not host-bound credential injection. The
job-worker placement is the one exception to retention: resolved grant values stay
in trusted worker memory for the job lifetime so post-start restart supervision
can re-spawn a crashed child without re-fetching grants.

### Core per-job workers support explicit MCP placement

`McpServerConfig.placement` distinguishes backward-compatible `host` activation
from `job_worker`. Core Docker job creation selects exact enabled server names, with
none selected by default, then takes an immutable owner-scoped snapshot and separate
secret grants. Restart state stores and resolves those same names.
The worker verifies any pinned executable against its own image, initializes MCP,
discovers tools, and atomically adds autonomous wrappers to its fresh `ToolRegistry`.
It unregisters wrappers and stops all MCP runtime state before job completion.

This path requires a mounted `/workspace`, accepts only preinstalled stdio commands,
and allows at most eight servers and 32 grants per job. Across those servers, tool
discovery is capped at 256 tools and 2 MiB of descriptors; stream frames are capped
at 4 MiB. Secret values travel through a dedicated job-token-authenticated route and
never enter normal shell credentials.
Startup, exact grant coverage, provenance, conflicts, approval requirements, and
cleanup fail closed. Initial activation and teardown are bounded. Post-start MCP
child restart supervision is implemented: one supervisor task per server watches
its stdio child and rebuilds it from the immutable job snapshot after an
unexpected exit, with bounded cleanup, rediscovery revalidation, atomic tool
swap, jittered exponential backoff, a stability-window reset, and a five-attempt
circuit that leaves the server down for the rest of the job. Resolved secret
values stay in trusted worker memory for the job lifetime so restarts do not
re-fetch grants. Linux worker activation
also requires Yama `kernel.yama.ptrace_scope >= 1` so same-UID MCP and shell sibling
processes cannot inspect each other's credential environments after `execve`.

Some persistent external workers can independently configure their own MCP clients
or servers. They do not receive this core Docker job-worker placement contract.

### The native WASM tool ABI remains invocation-scoped; MCP uses an actor ABI

The native WASM tool runtime still compiles components once and instantiates a fresh
component for each execution. Its WIT world exports one tool interface with schema,
description, and execute functions. Host capabilities cover HTTP, workspace reads,
tool invocation, secret existence checks, and related controlled operations.

MCP-over-WASM is implemented separately in `ic/wit/mcp-server.wit` and
`ic/src/tools/mcp/wasm_transport.rs`. Each active server retains one component
instance and receives complete JSON-RPC envelopes through a serialized `handle`
export. This supports synchronous client-to-server initialization, initialized
notification, tool discovery, and tool calls while preserving guest state. It has no
event/outbox path for unsolicited server events or server-to-client requests, and
LunarWing exposes no resources, prompts, or subscriptions through this client surface.
The linker supplies no WASI or host imports and bounds artifact size, message size,
memory, fuel, and wall time; a trap stops the actor and manager supervision reconstructs
it from persisted intent. A source WAT fixture validates protocol envelopes, state,
pagination, calls, memory and fuel enforcement, timeout, traps, and wrapper replacement.

### Current MCP protocol scope is conservative and bounded

The client advertises empty capabilities and strictly separates inbound JSON-RPC
responses, errors, requests, and notifications. Initialization negotiates exactly
`2024-11-05`. Tool discovery follows bounded pagination and rejects repeated cursors.
When negotiated, `tools.listChanged` signals flow through stream transports and one
bounded persistent Streamable HTTP GET/SSE listener, including gateway placement, into
generation-safe host and worker atomic wrapper refresh.

Canonical annotation fields from later MCP revisions deserialize with their specified
defaults but remain untrusted advisory input under operator policy. This does not claim
annotation support in negotiated revision `2024-11-05`.

The separate actor does not change the preferred format for small tools: native WASM
tools keep their narrower capability-scoped ABI and fresh-instance isolation.

### Package integrity exists, but publisher authenticity is incomplete

The registry installer requires SHA-256 checksums for downloaded WASM artifacts
and validates supported artifact locations. This protects against corrupted or
unexpected bytes relative to a trusted manifest.

It does not by itself establish who published the extension, whether that
publisher remains trusted, or whether a compromised registry entry should be
revoked. A V2 marketplace needs package signatures and publisher trust metadata in
addition to checksums.

### Automatic npm/pip MCP installation is not implemented

Current stdio manifests describe a command, arguments, and environment. The MCP path
does not download or manage npm/PyPI packages. Operator pinning verifies and executes
the same sealed native bytes on Linux through `memfd` plus `execveat`, with a 128 MiB
cap; scripts and non-Linux pinning fail closed while unpinned legacy stdio remains.
This still does not verify an npm/PyPI dependency tree. Operators remain responsible
for installation.

Automatic installation could provide one-command setup, exact version selection,
reproducible tenant provisioning, and managed upgrade/rollback. It would also run
package lifecycle hooks and dependency code. Typosquatting, compromised publisher
accounts, mutable registry entries, dependency substitution, and unpinned drift
would become direct code-execution risks.

Package provenance must establish canonical identity, source, immutable version,
artifact digest, publisher signature/key, locked dependencies, trust/revocation
state, capability and placement requests, upgrade diffs, and rollback history.
Provenance answers **what code is this and who supplied it**. It does not prove that
code is safe.

Installation and runtime also need containment: non-root execution, isolated
writable paths, controlled egress, process and resource limits, no implicit daemon
credentials/devices, and reliable teardown. Sandboxing answers **what can this code
damage if it is malicious or compromised**. Sandboxing does not establish package
identity, and signatures do not replace containment.

Automatic installation is not required for MCP. Manually installed pinned commands,
verified bundles, and prebuilt worker images remain valid. Worker-local prototypes
should start only preinstalled or verified packages; arbitrary package-manager
downloads during a job should not be the default.

### The WASM security model is promising but not fully unified

The WASM tool runtime has strong machinery for capability checks, resource limits,
host-bound credential injection, output leak detection, and restricted WASI
contexts. However, tools and channels have separate runtime and host-function
implementations, and known security-parity work remains between their HTTP and
leak-handling paths.

V2 should first extract and unify the common security boundary rather than add a
third independently implemented WASM extension type.

## Design Principle: Separate Package, Policy, Runtime, and Protocol

The central proposal is a runtime-neutral extension descriptor. A conceptual
manifest could express:

```yaml
schema_version: 2
package:
  id: com.example.repository-tools
  version: 1.4.0
  publisher: example

exports:
  - kind: tools
  - kind: resources

runtime:
  type: wasm
  protocol: lunarwing-native
  artifact: repository-tools.wasm

placement:
  allowed:
    - orchestrator
    - job-worker
  preferred: job-worker

capabilities:
  network:
    hosts:
      - api.example.com
  workspace:
    read:
      - project/**
  credentials:
    - name: example_token
      inject:
        host: api.example.com
        header: Authorization

limits:
  memory_mb: 128
  timeout_seconds: 30

provenance:
  digest: sha256:...
  signature: ed25519:...
  publisher_key: example-2026
```

This is illustrative, not a proposed final schema. The important point is that
runtime and protocol are explicit fields instead of being inferred from the
extension kind.

The host should derive an internal capability graph from installed package
descriptors and tenant grants. Users and operators should not need to author a
literal graph for ordinary installation.

## Alternatives Evaluated

### 1. Embedded MCP modules

In this model, an integration is compiled directly into the LunarWing binary and
exposes an MCP-compatible server interface internally.

**Advantages**

- Lowest invocation overhead.
- Straightforward access to internal Rust APIs.
- No process, network, or serialization boundary is required internally.
- Suitable for foundational first-party capabilities that must ship with the
  daemon.

**Costs and risks**

- Expands the trusted computing base.
- Couples extension release cadence to LunarWing releases.
- Increases binary size and feature-flag combinations.
- A crash, memory error, or dependency conflict affects the daemon.
- Third-party code would require the highest trust level.

**Assessment**

Keep this option for first-party, tightly coupled integrations. Do not make it a
general marketplace runtime.

### 2. Native WASM tools

In this model, extensions implement LunarWing's native tool ABI and use declared
host capabilities.

**Advantages**

- Small and explicit attack surface.
- Good resource isolation and deterministic invocation lifecycle.
- Existing installation, capability, auth, activation, and removal machinery.
- No MCP protocol overhead for integrations that only expose tools.
- Strong fit for portable, stateless, request/response integrations.

**Costs and risks**

- Current ABI exports one tool per component shape rather than a complete MCP
  server surface.
- Native dependencies and unsupported language/runtime assumptions can block
  portability.
- Invocation-scoped instances require explicit host storage for durable state.
- Tool and channel security primitives need consolidation.

**Assessment**

This should remain the preferred native format for small, capability-scoped tools.
It should not be renamed or wrapped as MCP merely for conceptual uniformity.

### 3. MCP-over-WASM

In this model, a WASM component implements an MCP server interface and the host
adapts messages to the component without requiring an operating-system process or
TCP listener.

There are two possible versions of this idea:

1. A compatibility runtime that hosts an existing portable MCP implementation.
2. A new LunarWing-specific MCP server WIT ABI that extension authors target.

The first preserves ecosystem compatibility. The second risks duplicating the
native WASM tool ABI while also inheriting MCP lifecycle complexity.

**Advantages**

- Sandboxes MCP server logic using the existing Wasmtime foundation.
- Avoids stdio process management and port allocation.
- Could expose tools, resources, prompts, and notifications through one protocol.
- Allows the same packaged server to be used by LunarWing and, potentially, other
  MCP clients through a host-provided transport adapter.

**Costs and risks**

- Requires MCP initialization, capability negotiation, cancellation, notification,
  and session semantics.
- Requires a persistent actor/session design or explicit state externalization.
- Existing Node/Python stdio servers cannot be moved into WASM automatically.
- A LunarWing-specific WIT mapping may become another compatibility surface to
  maintain alongside the MCP specification.
- Long-lived streaming and subscription behavior need bounded resource ownership.

**Assessment**

Treat shipped MCP-over-WASM as an optional synchronous tool compatibility runtime.
Portable MCP reuse beyond that surface, including resources, prompts, notifications,
server requests, and cross-client serving, needs more ABI and client work. Do not
require simple WASM tools to implement MCP.

### 4. MCP inside a per-job worker container

In this model, the job worker starts one or more approved MCP servers inside the
same container and registers their tools in the worker's local tool registry.

**Advantages**

- MCP server shares the job's `/workspace` view and toolchain.
- Arbitrary server code remains inside the worker sandbox rather than running on
  the host.
- Strong fit for repository inspection, code generation, and project-local tools.
- Existing worker images already include runtimes used by many MCP packages.
- Server lifetime naturally follows the job when isolation is more important than
  reuse.

**Costs and risks**

- Repeated startup and package initialization for every job.
- Runtime package installation introduces network dependence and version drift.
- Credentials granted to the job may become visible to the MCP child process.
- MCP process failures need to be distinguished from worker failures.
- The worker protocol must receive approved package descriptors and grants.

**Assessment**

Implemented as explicit `job_worker` placement for workspace-coupled stdio MCP
servers in core Docker jobs. Commands must be preinstalled; optional pinned
provenance is checked in the worker. Runtime package installation remains blocked.

### 5. MCP inside a persistent external worker

In this model, a dedicated worker owns one or more MCP servers and exposes selected
capabilities to LunarWing through `lunarwing-agent-v1`, MCP, or a future extension
runtime protocol.

**Advantages**

- Supports native libraries, GPUs, language runtimes, and heavyweight services.
- Amortizes startup cost and preserves connection pools or caches.
- Suitable for durable state, long-lived subscriptions, and streaming APIs.
- Keeps complex code outside the main daemon.
- Can be scaled and load-balanced independently.

**Costs and risks**

- Larger operational footprint than WASM or a local subprocess.
- Requires health checks, supervision, version negotiation, and secure networking.
- Tenant and credential isolation must be explicit in a shared worker.
- The boundary between worker-native tools and MCP-provided tools can become
  confusing without consistent package metadata.

**Assessment**

This is a valid placement, not merely a fallback for failed WASM ports. It is the
preferred option for stateful, heavy, native, or independently scalable servers.

### 6. Dedicated MCP gateway

In this model, a gateway manages connections to multiple local or remote MCP
servers and presents one controlled endpoint to LunarWing.

**Advantages**

- Central OAuth, token refresh, auditing, routing, and rate limiting.
- Shared sessions and process reuse across daemon instances or tenants.
- Protocol adaptation can be isolated from the core daemon.
- Clear network and policy enforcement point for hosted MCP services.
- Can hide transport differences from LunarWing.

**Costs and risks**

- Additional service to deploy and monitor.
- Can become a bottleneck or single failure domain.
- Project-local filesystem access becomes difficult or unsafe.
- A gateway with broad credentials and tenant access is a high-value target.
- It does not remove the need for package provenance or capability policy.

**Assessment**

Client placement for this option is implemented. `McpGatewayMode` distinguishes a
dedicated endpoint from a shared endpoint; shared mode requires a validated route,
and the gateway transport injects host-owned mode/route headers while retaining
Streamable HTTP session and OAuth behavior. LunarWing also bundles an optional
loopback-default control/data plane with authenticated route administration,
HTTPS-only upstreams, concurrency-bounded streaming responses (SSE relays
chunks without a cumulative cap; other bodies buffer under the body cap), MCP
session-header forwarding, and end-to-end GET/SSE notification delivery. Cross-daemon sessions, HA,
centralized policy, and credential ownership remain responsibilities of the
deployment. Gateway placement is not mandatory for workspace-local or
dedicated-worker capabilities.

### 7. Signed extension marketplace

This idea applies across all runtimes rather than competing with them.

**Required properties**

- Stable package identity and semantic version.
- Publisher key identity and signature.
- Artifact digests for every supported runtime target.
- Declared capabilities and credential injection rules.
- Supported LunarWing ABI/protocol versions.
- Runtime and placement requirements.
- Dependency declarations and conflict rules.
- Revocation and compromised-key handling.
- Reproducible or attestable builds where practical.
- Upgrade policy with capability-diff review.

**Assessment**

This is the strongest long-term idea in the set. It should be designed independently
of which execution runtimes ship first.

### 8. Declarative capability graph

A capability graph could represent:

- Packages and their exported tools/resources/prompts/channels.
- Runtime dependencies.
- Delegation edges such as tool-to-tool invocation.
- Credential and network grants.
- Tenant subscriptions and denials.
- Placement constraints.
- Approval requirements.

The graph is valuable for policy evaluation, conflict detection, planning, audit
visualization, and determining what an agent may see in a given turn.

It would be burdensome as the normal user-authored installation format. The better
approach is to derive it from manifests, tenant grants, and active runtime state.

## Recommended V2 Shape

### One package model

All installable extensions should use one top-level package vocabulary, even if the
payload is a WASM component, process declaration, worker image, or remote endpoint.
The current distinction between MCP URL manifests and downloadable WASM artifacts
should become a runtime choice inside a broader package descriptor.

### Two protocol categories

1. **LunarWing-native:** Narrow ABIs optimized for direct tool, channel, hook, or
   future resource behavior.
2. **Compatibility protocols:** MCP and other external protocols implemented by
   adapters.

Native protocols should be used when LunarWing owns both sides and a smaller
contract improves safety or performance. MCP should be used when ecosystem
compatibility, broader primitives, or external client interoperability matters.

### Multiple explicit placements

At minimum, V2 should distinguish:

- `orchestrator`: Executes in or adjacent to the daemon.
- `job-worker`: Executes inside an isolated job container with project-local access.
- `dedicated-worker`: Executes in a persistent independently supervised service.
- `remote`: Executes at a hosted network endpoint.

Placement must affect credential grants, workspace visibility, network policy,
resource limits, and lifecycle ownership. It cannot be a cosmetic deployment hint.

### Derived tenant capability views

Each tenant should receive a derived view of active exports after evaluating:

- Installed package versions.
- Tenant subscriptions.
- Explicit operator grants and denials.
- Runtime health.
- Authentication state.
- Placement availability.
- Per-session or per-job approvals.

The LLM should only see tool definitions from this evaluated view. Installing a
package must not automatically grant all tenants or all execution placements access.

## Conceptual Runtime Selection

| Requirement | Preferred runtime/placement |
| --- | --- |
| Small stateless API integration | Native WASM tool |
| Existing hosted MCP endpoint | Remote HTTP MCP, optionally through gateway |
| Existing local stdio MCP trusted on host | Managed local process |
| MCP requiring job workspace | MCP in job worker |
| Native dependencies or language runtime | Job worker or dedicated worker |
| Durable state or long-lived subscriptions | Dedicated worker or gateway |
| GPU or heavy compute | Dedicated worker |
| Stateful synchronous MCP tools in portable sandbox | MCP-over-WASM |
| Core first-party runtime integration | Embedded Rust module |

This table should guide defaults, not impose hard prohibitions. Manifests and policy
must still constrain which placements are permitted.

## Security Considerations

### Treat every local MCP server as installed code

MCP servers can access data and execute actions. A stdio command is not safer merely
because it communicates over JSON-RPC. Host-local and worker-local servers must be
subject to package trust, capability review, and credential scoping.

### Keep raw credentials at the narrowest possible boundary

The preferred pattern is host-side credential injection into an allowed outbound
request. When a process or worker must receive a raw credential, the package should
declare that requirement explicitly and the UI should distinguish it from boundary
injection.

### Capability claims are requests, not facts

A package manifest is authored by a potentially untrusted publisher. The host must
validate and enforce capabilities. MCP tool annotations may inform approval policy,
but they must not override operator policy or package trust.

### Signing does not replace sandboxing

A valid signature identifies the publisher and protects package integrity. It does
not prove that code is safe. Publisher trust, capability minimization, sandboxing,
runtime limits, audit events, and revocation all remain necessary.

### Placement changes the threat model

- Orchestrator-local execution risks the daemon and host.
- Job-worker execution risks the mounted project and granted job credentials.
- Dedicated workers risk shared state and cross-tenant isolation.
- Remote services receive whatever data is sent over the network.
- Gateways concentrate credentials and policy authority.

The package manager should show a placement-specific risk summary before activation.

## Lifecycle Considerations

The package lifecycle should remain consistent even when runtime behavior differs:

1. Discover package metadata.
2. Verify publisher and artifact provenance.
3. Review requested capabilities and placements.
4. Install immutable package version.
5. Configure non-secret settings.
6. Authenticate or provision credentials.
7. Activate for selected tenant scopes and placements.
8. Perform runtime health and protocol negotiation.
9. Expose the evaluated capability view.
10. Upgrade with artifact and capability diff review.
11. Deactivate, revoke grants, stop runtime resources, and remove.

The implemented MCP runtime registry is an `INTERNAL` closed built-in dispatch boundary,
not a production plugin API. Generic external runtime descriptors remain deferred. It
delegates these resources:

- Process adapter: child lifecycle, stdio framing, sandboxing, provenance, and
  shutdown. Whole-runtime recovery still recreates the client and wrappers rather
  than restarting only the child.
- WASM adapter: component compilation, one persistent actor, no host imports,
  artifact digest pinning, fuel, memory, epoch deadlines, message bounds, and
  terminal-trap recovery.
- Unix adapter: connection ownership and stream shutdown.
- Remote adapter: Streamable HTTP sessions, OAuth, HTTP policy, and retries.
- Gateway adapter: dedicated/shared placement, protected route selection, sessions,
  OAuth, and independent gateway-service ownership.

The core Docker worker adapter now owns immutable descriptor/grant delivery,
worker-image provenance checks, bounded startup, atomic tool registration, and
pre-completion teardown. Post-start MCP process restart ships with generation-safe
atomic refresh, bounded retry, persistent circuit state, and dead-wrapper removal.
Package delivery and persistent external-worker placement remain open.

## Suggested Exploration Sequence

The host-local installation work below was selected as an immediate improvement.
If broader V2 work proceeds, the least-regret order is:

### Phase 0: Host-local stdio installation and lifecycle (completed)

- Add typed stdio MCP registry manifests.
- Route registry, chat, API, and web installation through validated
  `McpServerConfig` persistence.
- Keep activation explicit and use the existing `McpProcessManager` lifecycle.
- Expose transport and command metadata in installed-extension responses.
- Do not add runtime package downloads or worker placement in this phase.
- Subsequent hardening added diagnostics, exact tool ownership,
  activate/deactivate/update/remove, owner-scoped `env_secrets`, restricted child
  environment inheritance, explicit runtime cleanup, and manager-level stdio crash
  recovery with per-server cancellation, bounded retry, crash-loop circuit breaking,
  generation-safe atomic tool replacement, and deadline-bounded shutdown.

### Phase 1: Package and policy vocabulary

- Define runtime-neutral package identity, exports, capabilities, placement, and
  provenance.
- Define capability-diff behavior for upgrades.
- Define tenant grant and activation semantics.
- Decide signature trust roots and revocation behavior.

This phase can be designed without implementing new runtimes.

### Phase 2: MCP runtime adapter boundary (completed 2026-08-09)

- `McpRuntimeAdapter` and `McpRuntimeRegistry` provide the common MCP activation
  interface and preserve existing client/tool lifecycle behavior across runtimes.
- HTTP, stdio, Unix, stateful WASM, and gateway adapters ship in the default registry.
- Registry remains internal closed built-in dispatch. Production plugin composition and
  generic external runtime descriptors remain deferred package/runtime design work.
- Broader package-level adapter vocabulary and common WASM tool/channel network,
  credential, leak-detection, rate-limit, and audit primitives remain Phase 1/package
  policy work rather than MCP activation blockers.

### Phase 3: Worker-local MCP prototype (completed 2026-08-09)

- Pass an allowlisted, resolved MCP runtime descriptor to a job worker.
- Start only preinstalled or verified server packages.
- Register discovered MCP tools in the worker-local registry.
- Scope workspace and credentials to the job.
- Measure startup time, failure isolation, and teardown reliability.

Implemented for core Docker jobs with stdio-only placement, immutable owner-scoped
snapshots, exact separate credential grants, server/grant limits, old-worker
compatibility guard, autonomous-tool checks, and fail-closed bounded cleanup.

### Phase 4: Marketplace provenance

- Add publisher signatures and package trust state.
- Add capability review and upgrade diffs.
- Add revocation and compromised publisher handling.
- Extend artifacts beyond the current WASM-only package shape.

### Phase 5: MCP-over-WASM runtime (implemented 2026-08-09)

The stateful actor and host transport are implemented. Raw JSON-RPC preserves MCP
client requests and client notifications without version-specific WIT expansion. Current
LunarWing use is synchronous initialization, paginated tool discovery, and tool calls;
there is no unsolicited server-event or server-request path and no LunarWing resources,
prompts, or subscriptions surface. Host owns persistent actor state and enforces memory,
fuel, epoch-deadline, timeout, and envelope limits.

Source WAT interoperability now covers protocol framing, state, pagination, calls,
resource limits, timeout, traps, and manager wrapper replacement. Serving same component
to a non-LunarWing client remains release validation.
Native WASM tools remain the better implementation for ordinary narrow tool calls.

## Non-Goals

- Replacing existing HTTP MCP support.
- Requiring all extensions to speak MCP.
- Moving every built-in Rust tool into an extension runtime.
- Allowing arbitrary runtime package installation during a job by default.
- Treating package signatures as proof of safety.
- Designing the final V2 manifest schema in this exploratory note.
- Making gateway, worker, or MCP-over-WASM placement mandatory for every extension.

## Open Questions

1. Should extension packages be globally installed and tenant-activated, or should
   package installation itself be tenant-scoped?
2. Which authority signs first-party and community packages, and how are publisher
   keys rotated or revoked?
3. Should a package contain multiple runtime artifacts with placement fallback, or
   should each runtime be a separate package variant?
4. Can tenant policy override a package's preferred placement while remaining within
   its declared allowed placements?
5. Which MCP primitives beyond tools are real LunarWing V2 requirements: resources,
   prompts, subscriptions, sampling, elicitation, or all of them?
6. Should worker-local MCP remain available only to the worker agent, as in the
   prototype, or can the orchestrator safely proxy selected tools into the primary
   agent session?
7. How should package-defined tools retain stable identity across runtime changes and
   upgrades?
8. What state API should invocation-scoped WASM tools and persistent WASM actors use?
9. What audit events are required for package install, grant, activation, credential
   access, placement selection, tool invocation, upgrade, and revocation?
10. Which security primitives must be unified between WASM tools and channels before
    another WASM runtime type is acceptable?

## Evidence Needed Before a Decision

A V2 decision should be based on representative workloads rather than protocol
elegance alone. Useful prototypes and measurements include:

- Startup and memory cost for representative Node, Python, Rust, and WASM servers.
- Failure and cleanup behavior for crashed stdio servers in host and worker modes.
- Credential exposure comparison across host injection, process env, worker grants,
  and gateway forwarding.
- Session and notification requirements from real MCP servers under consideration.
- Cross-tenant isolation tests for persistent workers and gateways.
- Capability review usability for operators during install and upgrade.
- Compatibility tests across current and future MCP protocol versions.

## Preliminary Recommendation

Proceed with the runtime-neutral package and policy model as the V2 architectural
center. Preserve existing MCP transports and treat MCP as a compatibility protocol.
Use native WASM tools for small integrations, worker placement for workspace-local
or native servers, dedicated workers for stateful/heavy services, and gateways for
shared remote control-plane concerns.

The host-local stdio installation, explicit lifecycle, safe whole-runtime crash
recovery, and core Docker worker-local prototype are complete. Worker placement is
limited to preinstalled or worker-verified stdio commands and job-scoped credentials;
do not combine it with arbitrary npm/pip installation. Post-start worker MCP restart
supervision is now implemented with bounded per-server rebuilds from the immutable
job snapshot, backoff, and a per-job circuit. Next evidence should measure
representative startup, post-start failure, and cancellation behavior before adding
broader worker types.

The optional MCP-over-WASM actor/session ABI and dedicated/shared gateway client
placement are now available without changing native WASM tool defaults. Continue to
defer general embedded extensions; reserve them for first-party capabilities whose
coupling to the daemon is intentional.

## Relevant Source and Proposal References

- `ic/src/tools/mcp/config.rs`
- `ic/src/tools/mcp/factory.rs`
- `ic/src/tools/mcp/runtime.rs`
- `ic/src/tools/mcp/wasm_transport.rs`
- `ic/src/tools/mcp/gateway_transport.rs`
- `ic/src/tools/mcp/process.rs`
- `ic/src/tools/mcp/stdio_transport.rs`
- `ic/wit/mcp-server.wit`
- `ic/src/extensions/manager.rs`
- `ic/src/cli/mcp.rs`
- `ic/src/tools/builtin/extension_tools.rs`
- `ic/src/channels/web/server.rs`
- `ic/src/app.rs`
- `ic/src/worker/container.rs`
- `ic/src/worker/mcp.rs`
- `ic/src/orchestrator/job_manager.rs`
- `ic/src/orchestrator/api.rs`
- `ic/src/tools/registry.rs`
- `ic/Dockerfile.worker`
- `ic/src/tools/wasm/runtime.rs`
- `ic/src/tools/wasm/wrapper.rs`
- `ic/wit/tool.wit`
- `ic/src/channels/wasm/`
- `ic/src/registry/manifest.rs`
- `ic/src/registry/installer.rs`
- `ic/src/orchestrator/external_worker.rs`
- `docs/proposals/SUPERGATEWAY_MCP.md`
- `docs/proposals/EXTERNAL-WORKER-PLAN-UPGRADES.md`
