# MCP Part 2 Gap Audit for LunarWing v2.0.7.0

**Audit date:** 2026-08-10

**Audited commit:** `06753dec743edbf97c90f0b023be4edbdfe8ef50`

**Result:** Part 2's principal runtime slices exist, but the implementation is not a
complete MCP compatibility layer. This review found three high-priority correctness or
integrity gaps, five medium-priority protocol, validation, or architecture gaps, and
contradictory status documentation. No evidence supports enabling automatic npm/PyPI
installation or removing the documented sandbox and marketplace deferrals.

## Remediation Status (2026-08-11)

The audited commit and findings below remain the historical review result. Remediation
in this branch addresses all three P1 and five P2 findings:

- **P1 client capabilities:** initialization now advertises empty client capabilities,
  and inbound JSON-RPC envelopes are classified strictly so server requests cannot be
  mistaken for responses.
- **P1 worker recovery:** failed rebuilds no longer reset the stability window. The
  bounded circuit persists terminal state and atomically removes dead wrappers.
- **P1 stdio provenance:** pinned Linux stdio activation executes the same verified,
  sealed native bytes through `memfd` plus `execveat`. Reads have a 128 MiB metadata
  precheck and streaming limit; shebang scripts and non-Linux pinning fail closed.
  Unpinned legacy stdio remains available.
- **P2 discovery/events:** `tools/list` follows bounded pagination with repeated-cursor,
  page, tool-count, byte, and time limits. Negotiated `tools.listChanged` signals flow
  through stream transports and one bounded persistent HTTP GET/SSE listener, including
  gateway transport, into generation-safe host and worker atomic wrapper refresh.
- **P2 version negotiation:** initialization accepts exactly `2024-11-05`; missing or
  different negotiated versions fail before session activation.
- **P2 MCP-over-WASM:** scope is now explicit: one synchronous client-to-server tool
  actor, with no LunarWing client surface for unsolicited server events,
  server-to-client requests, resources, prompts, or subscriptions. A real source WAT
  fixture validates envelopes, persistent state, pagination, calls, memory and fuel
  limits, timeout, terminal traps, and manager wrapper replacement.
- **P2 runtime registry:** decision is explicit: registry is an `INTERNAL` closed
  built-in dispatch boundary, not a production plugin API. Generic external runtime
  descriptors remain deferred.
- **P2 annotations:** canonical later-revision fields and defaults deserialize correctly,
  but remain untrusted advisory input. Negotiated version stays `2024-11-05`; no claim
  is made that annotations belong to that revision, and operator approval policy remains
  authoritative.

Remediation verification passed 4,715 all-feature serialized library tests (4 ignored),
all 3 Engine V2 MCP compatibility tests, rustfmt, all-feature/PostgreSQL/libSQL checks,
and zero-warning all-feature clippy. Full release aggregation remains blocked outside
this MCP diff by unavailable real channel WASM artifacts and existing
skill-selection/TensorZero streaming expectations.

## Scope and Method

This audit treated the following as the Part 2 delivery contract:

- Phase 2 runtime adapter boundary;
- Phase 3 core Docker job-worker stdio placement;
- dedicated/shared gateway client placement;
- Phase 5 stateful MCP-over-WASM;
- the provenance, host sandboxing, and restart-supervision hardening presented as
  v2.0.6.0 MCP Part 2 work.

The primary architecture source was
[`V2_MCP_EXTENSION_ARCHITECTURE_EXPLORATION.md`](../proposals/V2_MCP_EXTENSION_ARCHITECTURE_EXPLORATION.md).
The audit also reconciled [`MCP_ADDITIONS.md`](MCP_ADDITIONS.md),
[`MCP_OVERALL_STATUS.md`](MCP_OVERALL_STATUS.md), and the v2.0.6.0 release notes against
current source. The shared Study Notes topic `mcp-host-local-crash-recovery`
(`applejack-kimi/mcp-host-local-crash-recovery.md`, source commit `a5fc90b`) was used as
an initial map, then reverified because its source commit predates this audit.

Relevant MCP specification references:

- [2024-11-05 lifecycle and capability negotiation](https://modelcontextprotocol.io/specification/2024-11-05/basic/lifecycle)
- [2024-11-05 tool discovery, pagination, and list-change notification](https://modelcontextprotocol.io/specification/2024-11-05/server/tools)
- [2024-11-05 canonical TypeScript schema](https://raw.githubusercontent.com/modelcontextprotocol/specification/main/schema/2024-11-05/schema.ts)

## Requirement Matrix at Audited Commit

| Part 2 slice | Current evidence | Audit status |
|---|---|---|
| Common runtime dispatch | `McpRuntimeAdapter` and `McpRuntimeRegistry` dispatch HTTP, stdio, Unix, WASM, and gateway transports | Implemented internally; production registry injection is missing |
| Stateful MCP-over-WASM | Persistent actor, raw JSON request/response ABI, no imports, digest and resource bounds, trap health | Implemented for synchronous client-to-server exchanges; bidirectional MCP and real guest interoperability remain unverified |
| Dedicated/shared gateway placement | Validated mode/route and host-owned routing headers over HTTP | Client contract implemented; no bundled gateway is expected, but end-to-end shared gateway behavior is unverified |
| Core job-worker placement | Immutable selected snapshots, exact grants, preinstalled stdio commands, bounded discovery/cleanup | Implemented, with restart circuit and stale-wrapper defects described below |
| Worker restart supervision | Per-server watch, rebuild, backoff, stability window, five-attempt target | Present but does not reliably enforce its retry bound after failed rebuilds |
| Operator-pinned provenance | Package metadata and SHA-256 checks for stdio executables/WASM components | Present; stdio hash-to-exec is not atomic |
| Host stdio sandbox | Opt-in rlimits/process group plus Linux seccomp, Landlock, and network isolation | Implemented within its documented opt-in scope |
| Marketplace/package manager | Signatures, dependency locks, automatic installation, upgrade diffs, distributed revocation | Intentionally deferred; must remain disabled |

## Findings

### P1 — LunarWing advertises bidirectional client capabilities it cannot service

`McpRequest::initialize` advertises both `roots` and `sampling` in
`ic/src/tools/mcp/protocol.rs:118-133`. Under MCP, the presence of these capability
objects tells a server that it may issue `roots/list` and `sampling/createMessage`
requests to the client.

The transport contract is only `send(request) -> response`
(`ic/src/tools/mcp/transport.rs:20-56`). Stream transports parse all inbound lines as
`McpResponse` and have no server-request dispatcher
(`ic/src/tools/mcp/transport.rs:86-150`). HTTP/SSE similarly skips everything except a
response matching the current client request ID
(`ic/src/tools/mcp/http_transport.rs:271-340`). The WASM actor accepts one host request
and returns one response; it also has no host-side inbound-request channel
(`ic/src/tools/mcp/wasm_transport.rs:121-163`).

This is more than a missing optional feature. A conforming server is invited to issue a
request LunarWing cannot answer. On a stream transport, a server-originated request with
an ID equal to a pending client request can be deserialized and routed through the
client's pending-response map, even though JSON-RPC request IDs are independent in each
direction. Valid roots or sampling traffic can therefore fail or corrupt an otherwise
valid in-flight exchange.

**Recommendation:** immediately advertise an empty client capability object until
bidirectional request handling exists. Before enabling roots or sampling, introduce a
message enum that distinguishes request, response, error, and notification; maintain
separate inbound/outbound ID routing; add bounded handlers; and apply explicit policy and
approval controls to sampling.

### P1 — Worker restart attempts can reset while recovery is still failing

The worker intends to stop after five unstable attempts
(`ic/src/worker/mcp.rs:32-39`). `claim_worker_mcp_restart` resets the counter whenever
`healthy_since` is at least 30 seconds old (`ic/src/worker/mcp.rs:645-656`). A successful
replacement refreshes `healthy_since` in `finish_worker_mcp_rebuild`, but failed spawn,
initialization, discovery, limit validation, or tool replacement leaves the timestamp
from the last successful runtime unchanged (`ic/src/worker/mcp.rs:446-489` and
`501-573`).

Once that old timestamp crosses 30 seconds, every subsequent failed recovery claim can
reset the attempt counter to zero. The five-attempt circuit and increasing backoff may
therefore never be reached during a persistent rebuild failure. The current unit tests
check the counter helper in isolation and one successful restart, but do not exercise a
stable runtime followed by repeated failed rebuilds (`ic/src/worker/mcp.rs:879-905` and
`953-1014`).

A second problem appears when the circuit does open: old wrappers deliberately remain
registered while a replacement is prepared (`ic/src/worker/mcp.rs:403-406`), but the
circuit-open branch only logs and stops the supervisor (`ic/src/worker/mcp.rs:369-381`).
It does not unregister those wrappers or mark a terminal state. The worker agent can
continue seeing tools whose client and child are permanently dead.

**Recommendation:** represent health explicitly, clear or suspend `healthy_since` on the
first detected failure, and reset attempts only after a newly rebuilt child has remained
continuously healthy for the full window. On terminal circuit open, atomically unregister
the dead server's exact wrappers. Add a regression that forces every rebuild stage to
fail after a previously stable runtime and proves bounded attempts, backoff, wrapper
removal, and shutdown cancellation.

### P1 — stdio provenance has a hash-to-exec race

Pinned stdio activation resolves and hashes an executable in
`McpPackageProvenance::verify_stdio_command`
(`ic/src/tools/mcp/provenance.rs:327-351`). After that asynchronous check returns,
`StdioRuntimeAdapter` passes the original command string to `spawn_stdio`
(`ic/src/tools/mcp/factory.rs:218-245`), which constructs a new `Command` and lets the OS
resolve/open the executable again (`ic/src/tools/mcp/process.rs:40-64` and
`ic/src/tools/mcp/stdio_transport.rs:122-156`).

An executable or PATH entry can be replaced between verification and `exec`. The
persisted `resolved_command` is informational and is not the file object executed. This
means SHA-256 pinning detects ordinary drift but does not fully enforce the identity of
the bytes actually started. The WASM path avoids this class of race by reading once,
verifying those bytes, and compiling those same bytes.

**Recommendation:** execute the verified file descriptor (`fexecve`/`execveat`) where
available, or stage and execute an immutable private copy derived from the verified
bytes. Cross-platform fallback should fail closed for a policy that promises atomic
provenance. Also validate regular-file/executable metadata and add an adversarial
replacement test before describing stdio pinning as race-free.

### P2 — Tool discovery ignores pagination and list-change events

The pinned 2024-11-05 protocol supports a cursor in `tools/list` and `nextCursor` in the
result. LunarWing sends one request with no cursor (`ic/src/tools/mcp/protocol.rs:147-150`),
models `ListToolsResult` with only `tools` (`ic/src/tools/mcp/protocol.rs:270-274`), then
caches that first page indefinitely (`ic/src/tools/mcp/client.rs:512-539`).

Servers that paginate expose only their first page. This affects host, gateway, WASM,
and job-worker placement. Worker startup can silently register an incomplete capability
set even though its count and descriptor-size limits are intended to apply to the whole
server.

The parsed `tools.listChanged` capability is not acted on. Server notifications are
discarded by stream and HTTP readers, and no code clears the cache or atomically updates
registered wrappers after `notifications/tools/list_changed`. Dynamic servers therefore
stay stale until a full runtime restart.

**Recommendation:** follow `nextCursor` to completion with page/count/byte/time bounds;
reject repeated cursors; apply worker aggregate limits across pages; and route
list-change notifications into a generation-safe rediscovery plus atomic wrapper swap.

### P2 — Protocol version negotiation is parsed but not enforced

LunarWing sends only `2024-11-05`, but `InitializeResult.protocol_version` is optional
(`ic/src/tools/mcp/protocol.rs:193-209`). `reinitialize_session` accepts a missing or
arbitrary version, marks the session initialized, and sends `notifications/initialized`
without a compatibility check (`ic/src/tools/mcp/client.rs:353-406`). The canonical
2024-11-05 schema requires `protocolVersion` and requires the client to disconnect when
it cannot support the server's selected version.

**Recommendation:** make required initialize fields required in the Rust type, maintain
an explicit supported-version set, and reject/close on a missing or unsupported result
before marking the session initialized. If newer Streamable HTTP revisions are added,
carry the negotiated `MCP-Protocol-Version` header as required by those revisions.

### P2 — MCP-over-WASM preserves envelopes, not the claimed full MCP behavior

The WIT boundary is useful and deliberately version-light, but it is synchronous:
`handle(request-json) -> result<string, string>` (`ic/wit/mcp-server.wit:11-18`). It can
carry client requests and client notifications such as `notifications/initialized`, but
it has no path for unsolicited server notifications or server-to-client requests. The
public `McpClient` exposes initialization, tool listing, and tool calls only
(`ic/src/tools/mcp/client.rs:491-608`). Resources, prompts, subscriptions, sampling,
roots, cancellation, progress, and dynamic tool events are not usable through the
LunarWing client lifecycle.

No committed MCP component fixture was found, and the focused tests do not instantiate a
real component through `WasmMcpTransport::load`. WASM manager recovery tests use fake
healthy/unhealthy transports (`ic/src/extensions/manager.rs:6763-6843`). The proposal
itself correctly calls representative guest interoperability outstanding release
validation, while several status and release statements overstate this as preserving
subscriptions and notifications.

**Recommendation:** describe the shipped scope as a stateful synchronous JSON-RPC actor
for the current tool client. Add a real guest component fixture covering initialize,
initialized notification, paginated tools, call, state retention, timeout, trap, and
recovery. A future full MCP actor needs a bounded host event/outbox ABI and inbound
request dispatch.

### P2 — The public runtime registry is not extensible in production composition

`McpRuntimeRegistry` is public and can replace adapters for its five closed enum kinds in
an explicit factory call (`ic/src/tools/mcp/runtime.rs:19-39` and `151-181`). Production
activation does not receive a registry from `AppBuilder` or `ExtensionManager`.
`create_client_from_config` constructs the private hardcoded default registry on every
call (`ic/src/tools/mcp/factory.rs:21-40` and `133-142`), and all production activation
sites use that path. `McpRuntimeKind` and `McpTransportConfig` are closed enums, so an
external adapter cannot introduce a new runtime kind or participate in normal startup,
lifecycle recovery, or worker placement.

**Recommendation:** either reword this as an internal common dispatch boundary, or wire
an `Arc<McpRuntimeRegistry>` through composition and lifecycle owners with duplicate-kind
validation. A genuinely extensible model also needs a versioned runtime descriptor that
does not require adding a core enum variant for each adapter.

### P2 — Later-revision tool annotations do not map to the approval fields

The current negotiated 2024-11-05 schema has no tool annotations, but LunarWing models
annotation-derived approval hints. The Rust fields use snake_case with no serde rename
(`ic/src/tools/mcp/protocol.rs:48-67`). In MCP revisions that define annotations, the
wire names are camelCase (`destructiveHint`, `readOnlyHint`, and others). A standard
`destructiveHint: true` is therefore ignored and defaults to false, while
`McpToolWrapper::requires_approval` relies on that parsed value
(`ic/src/tools/mcp/client.rs:718-730`). Existing tests construct Rust structs directly
and do not deserialize canonical annotation JSON.

Explicit `always_require_approval` remains effective, and server annotations are
advisory rather than a trusted security boundary. Nevertheless, the current code does
not provide the compatibility or safety behavior it appears to promise.

**Recommendation:** when a protocol revision with annotations is supported, map canonical
camelCase fields with explicit aliases and add wire-format tests. Preserve explicit
operator policy as authoritative and never allow an annotation to weaken it.

### Documentation — Status documents contradict current source

Current source contains worker restart supervision, and the v2.0.6.0 release notes and
top of the V2 proposal say it shipped. However:

- `MCP_OVERALL_STATUS.md:120-122` still says post-start worker restart is deferred;
- `MCP_ADDITIONS.md:221-223` still says there is no in-job restart supervisor;
- `V2_MCP_EXTENSION_ARCHITECTURE_EXPLORATION.md:626-629` says it remains open, while
  the same document later says it is implemented.

These contradictions can cause release reviewers either to miss real defects or to plan
already completed work. The new audit should be used as the v2.0.7.0 checkpoint; the
older status documents should be reconciled when fixes are implemented.

## Intentional Deferrals Confirmed

The following are real gaps in the larger MCP architecture, but they are explicitly
deferred rather than regressions in the delivered Part 2 runtime slices:

- automatic npm/PyPI installation and arbitrary package-manager execution;
- publisher signatures, signing trust roots, dependency locks, capability/upgrade
  diffs, and marketplace revocation distribution;
- ~~mandatory default sandbox profiles and device isolation~~;
- ~~bundled gateway control plane~~, cross-daemon HA, and centralized cross-tenant
  credential ownership;
- ~~persistent external-worker MCP placement~~ and a general dedicated-worker contract;
- configurable per-server startup/request timeouts for every transport.

**Update (2026-08-14, item #8 / `aj/kimi-8-20260814-1200`):** four deferrals are now
implemented in `ic/src/tools/mcp/`:

- **Publisher signatures, dependency locks, revocation distribution, upgrade diffs** —
  `marketplace.rs`: ed25519 publisher signatures over canonical manifests, a trust
  store, signed lockfiles that pin exact version+SHA-256 per dependency, signed
  revocation lists with monotonic sequences and a `RevocationStore` that persists
  atomically, refuses rollback/replay, fetches bounded updates over HTTPS, and
  checks persisted provenance records (`check_provenance`).
- **Mandatory sandbox profiles** — `McpSandboxConfig::default_profile()` plus
  `McpServersFile::require_sandbox_profiles`, which rejects enabled host stdio
  servers missing mandatory controls (legacy opt-in remains the default).
- **Persistent external-worker placement** — `McpPlacement::ExternalWorker` with a
  validated `external_worker` name, durable in the servers file; host and worker
  factories fail closed since activation belongs to the named persistent worker.
- **Bundled gateway control plane** — `gateway_control.rs`: a bearer-authenticated
  admin API over a persisted route table plus a data plane that speaks the existing
  routing-header contract, strips client routing headers, injects per-route upstream
  credentials, and proxies with bounded bodies/timeouts.
- **Generic external runtime descriptors** — `descriptor.rs` plus
  `McpTransportConfig::External`: versioned descriptors register out-of-tree runtimes
  without new core enum variants; activation spawns the descriptor's adapter over
  MCP stdio with bounded options and at least the mandatory sandbox baseline when
  the descriptor requires it.

Still deferred: automatic npm/PyPI installation and arbitrary package-manager
execution (blocked until provenance, locking, containment, audit, revocation,
health, and rollback are jointly enforced end to end), cross-daemon gateway HA and
centralized cross-tenant credential ownership, and per-server configurable
startup/request timeouts for every transport.

Automatic package installation must remain blocked until provenance, immutable locking,
containment, audit, revocation, health, and rollback are jointly enforced.

## Verification Performed

From `ic/`:

```text
taskset -c 0-11 cargo test -j12 --lib mcp -- --test-threads=12
```

Result: **368 passed, 0 failed, 4,291 filtered out**.

The passing suite verifies substantial existing behavior, including host recovery,
sandbox controls, gateway header ownership, worker placement, and one successful worker
restart. It does not invalidate the findings above: there is no failed-rebuild worker
circuit regression, paginated discovery test, canonical annotation JSON test,
unsupported-version test, inbound server-request test, or real MCP-over-WASM guest
fixture.

This was a source and focused-test audit. It did not deploy a live shared gateway or run
a tenant/container smoke test, so cross-route session/OAuth isolation remains an explicit
validation uncertainty rather than a claimed defect.

## Recommended Order

1. Stop advertising unsupported roots/sampling capabilities and fix worker circuit-open
   behavior, including dead-wrapper removal.
2. Close the stdio hash-to-exec provenance race before treating pins as an execution
   identity guarantee.
3. Enforce version negotiation; add bounded paginated discovery and notification-aware
   rediscovery.
4. Correct annotation wire mapping when adopting a revision that defines it.
5. Add real MCP-over-WASM and gateway interoperability fixtures, then reconcile the
   conflicting status documents.
6. Decide whether the adapter registry is intentionally internal or must become a real
   composition/plugin boundary.

Part 2 should be described as **implemented runtime slices with known protocol and
recovery gaps**, not as a complete implementation of the full MCP surface.
