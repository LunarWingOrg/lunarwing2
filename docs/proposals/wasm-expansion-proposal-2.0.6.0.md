# WASM Functionality Expansion Proposal

> **Status (reverified 2026-08-25): RESEARCH COMPLETE / PROPOSAL PARTIALLY
> OVERTAKEN, PHASE PLAN OPEN.** Originally written for a retired v2.0.6.0
> release-goals item #10. The baseline below now includes later `ssh-exec`,
> stateful MCP-over-WASM, and marketplace-trust-library work. Those additions do
> not complete the proposed phases: current source still has no shared WASM
> admission semaphore, per-extension execution metrics, tool workspace-write,
> extension KV API, channel timers, WASM hooks, or WASM routines. Generic WASM
> `TrustLevel` still does not attenuate requested capabilities, and channel HTTP
> egress still lacks the tool runtime's redirect/DNS-pinning parity. Claims were
> rechecked against revision `57e8a36f`; items marked *(verify)* still require
> version-specific library research before implementation.

## Goal

Map where LunarWing's WASM subsystem stands today, identify concrete
expansion directions (new host capabilities, new WASM extension kinds,
runtime/observability upgrades, guest developer experience), and propose a
phased plan that sequences security debt before feature growth.

This is a research + direction document. Most of its Phase 1–4 roadmap remains
open; later releases independently added some capabilities and trust primitives
that are now reflected in the current-state sections. Each remaining phase names
the files it would touch so follow-up work items can be scoped directly.

## Current State (verified)

### Three WIT contracts

| Contract | Package / version | World | Shape |
|----------|-------------------|-------|-------|
| `ic/wit/tool.wit` | `lunarwing:agent@0.3.0` | `sandboxed-tool` | Guest exports `tool` (`execute`, `schema`, `description`); imports `host` |
| `ic/wit/channel.wit` | `lunarwing:agent@0.3.0` | `sandboxed-channel` | Guest exports `channel` lifecycle callbacks; imports `channel-host` |
| `ic/wit/mcp-server.wit` | `lunarwing:mcp@0.1.0` | `mcp-server` | Guest exports one `handle(json-rpc) -> string` actor; **no host imports at all** |

Host WIT version constants live in `ic/src/tools/wasm/mod.rs`
(`WIT_TOOL_VERSION` / `WIT_CHANNEL_VERSION`, both `0.3.0`); extension
sidecars declare `wit_version` and are checked same-major/not-greater-than-host
at load time (`loader.rs::check_wit_version_compat`).

### Host capability surface

Tool guests (`ic/src/tools/wasm/`, `host.rs` + `wrapper.rs`):

- `log` (rate-limited: 1000 entries/execution, 4KB/message), `now-millis`
- `workspace-read` (read-only; path-validated, prefix-allowlisted)
- `http-request` (endpoint allowlist, host-side credential injection,
  response leak scan, per-tool rate limit, `redirect::Policy::none()` +
  DNS-resolve-then-pin at `wrapper.rs:618` / `:1707`)
- `ssh-exec` (host alias allowlist, keys/agent host-side, output leak scan)
- `tool-invoke` (alias indirection, rate-limited, output leak scan)
- `secret-exists` (existence check only, never values)

Channel guests (`ic/src/channels/wasm/`) get the same base minus
`ssh-exec`/`tool-invoke`, plus `emit-message` (100/exec, 64KB content),
`store-attachment-data` (20MB/attachment, 50MB/callback), namespaced
`workspace-write` under `channels/<name>/`, and DM-pairing host functions
(`pairing-upsert-request`, `pairing-is-allowed`, `pairing-read-allow-from`).
The host runs a managed event loop: HTTP router (`router.rs`) + polling
scheduler (minimum 30s interval); callbacks are `on-start`, `on-http-request`,
`on-poll`, `on-respond`, `on-status`, `on-broadcast`, `on-shutdown`.

MCP-over-WASM (`ic/src/tools/mcp/wasm_transport.rs`, landed under goals item
#7): one persistent actor per server, opaque JSON-RPC envelope, bounded
single-open artifact load (64MB max component, 4MB max message), optional
SHA-256 digest pinning, shared engine, memory/fuel/epoch limits,
generation-safe recovery after terminal traps.

### Sandbox posture

- wasmtime `36.0.13` pinned (`ic/Cargo.toml`), component model enabled,
  `wasm_threads` **disabled**, debug info disabled, Cranelift `OptLevel::Speed`.
- Fuel metering (default 500M instructions), 10MB default memory via
  `WasmResourceLimiter` (`limits.rs`), 60s default timeout, epoch
  interruption with a 500ms ticker thread (`runtime.rs`).
- Compile once / instantiate fresh per execution (NEAR pattern); persistent
  compilation cache with per-engine isolation on Windows (#448).
- Binary integrity: BLAKE3 hash verification on load (`storage.rs`).
- Guests build for `wasm32-wasip2`; tools/channels get an **empty**
  `WasiCtxBuilder::new().build()` context (no preopens, no env, no argv);
  MCP actors get no WASI imports whatsoever.
- Capability grant is opt-in-default-deny via `Capabilities`
  (`capabilities.rs`): approval policy, workspace, http, tool_invoke,
  secrets, webhook, ssh.

### Bundled extensions and lifecycle

- Bundled WASM tools (`ic/tools-src/`): gitlab, multica-bridge, llm-context,
  vision-analyze, cavepony, web-search, ssh, voice-transcribe, gotify. The
  authoritative source dirs are the `source.dir` entries in
  `ic/registry/tools/*.json`; `ic/lunarwing-gotify-tool/` is a stale
  unreferenced copy of the gotify tool kept for provenance.
- Bundled WASM channels (`ic/channels-src/`): xmpp, weechat, darkirc, multica.
- Build orchestration: `ic/scripts/build-wasm-extensions.sh`.
- Install/auth/activate/remove lifecycle: `ic/src/extensions/manager.rs`;
  catalog/discovery: `ic/src/extensions/{discovery,registry}.rs` and
  `ic/src/registry/`; distribution direction researched in
  `docs/ops/LUNARHUB_IRONHUB_MARKETPLACE_RESEARCH.md` (goals item #9).

## Findings: Gaps and Expansion Candidates

### A. Security debt that gates further expansion

These are already analyzed elsewhere; they are restated here because adding
capability surface before closing them compounds the risk. Cross-reference
rather than re-spec:

1. **WASM channel HTTP egress is weaker than tool egress** — the channel
   host builds plain `reqwest::Client::builder()` clients
   (`channels/wasm/wrapper.rs:402` and `:5247`) with no redirect policy and
   no DNS-resolve-then-pin, unlike the tool host. Spec:
   `docs/proposals/IRONCLAW_ADDITION_CANDIDATES.md` item 1 (verified still
   open at `fc01dc30`).
2. **Channel leak-scan ordering** — host-injected credentials can trip the
   leak detector in channel callbacks. Same doc, item 2.
3. **`TrustLevel` is vestigial** — defined in `tools/wasm/storage.rs:30`,
   only ever logged (`tools/registry.rs:1009`), never threaded into
   capability derivation. A `User`-trust sidecar can request wildcard host
   patterns and pre-provisioned host secrets. Spec: Reborn port analysis
   P1-C (`docs/proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-reborn-port-analysis.md`)
   and `IRONCLAW_ADDITION_CANDIDATES.md` ("WASM credential authority
   ceiling"). Verified vestigial at `fc01dc30`.
4. **No aggregate admission bound** — per-execution limits exist, but no
   process-wide semaphore bounds concurrent prepare/execute/callback storms
   (grep confirms no `Semaphore` in either WASM tree). Upstream IronClaw had
   a starvation incident here (`799eb1540`); spec:
   `IRONCLAW_ADDITION_CANDIDATES.md` item 6.
5. **Wasmtime audit/update workstream** — pinned `36.0.13`; keep upgrades a
   dedicated workstream with advisory review (`IRONCLAW_ADDITION_CANDIDATES.md`).
6. **Selective channel activation** — headless deployments load every
   discovered channel; an activation allowlist reduces noise and credential
   exposure (`IRONCLAW_ADDITION_CANDIDATES.md`).

### B. Host capability surface gaps (WIT evolution)

1. **No workspace write for tools.** Channels have namespaced
   `workspace-write`; tools are read-only. A namespaced
   `tools/<name>/` write capability would enable caching, state notebooks,
   and artifact-producing tools (report generators, exporters) without
   weakening the boundary.
2. **No state/KV API for tools or MCP actors.** This is open question #8 in
   `docs/proposals/V2_MCP_EXTENSION_ARCHITECTURE_EXPLORATION.md`. Today a
   tool is stateless across invocations (fresh instance, no persistence
   except workspace files it can read but not write), and an MCP actor's
   state dies with its generation. A host-mediated KV (`state-get` /
   `state-set` / `state-delete`, per-extension namespace, size-capped,
   DB-backed) covers both without giving guests filesystem access.
3. **No timer/cron callback for channels.** `channel.wit` offers only
   `on-poll` (minimum 30s). The module diagram references a "Timer
   Scheduler" that was never exported. Distinct one-shot/absolute timers
   (`on-timer`) would let channels do scheduled digests and reminders
   without polling.
4. **No streaming output.** Tool `execute` is request/response; long-running
   tools (transcoding, crawling, builds) cannot report incremental progress.
   Options: a `progress` host import, or component-model async
   (`wasmtime` 36 supports `component-model-async` behind a config flag —
   *(verify)* maturity before committing).
5. **No websocket host function for channels.** Deferred in
   `IRONCLAW_ADDITION_CANDIDATES.md` ("only if a real channel needs it" —
   WeeChat-WSS is the candidate). A WIT bump purely for this creates binding
   churn across all guests; batch it with other 0.4.0 changes.
6. **MCP-over-WASM has zero host imports by design.** If WASM MCP servers
   need outbound HTTP or state, prefer granting the *existing* tool-capability
   set (allowlist + injection + leak scan) to the actor world rather than
   inventing a third capability stack — one security model to audit.

### C. New WASM extension kinds

The AGENTS.md rule today: built-in Rust for core, WASM for sandboxed
tools/channels, MCP for external servers. Candidates that fit the WASM lane:

1. **WASM hooks.** `ic/src/hooks/` is native-only (grep shows no guest
   hook mechanism). Sandboxed lifecycle hooks (pre/post tool call, message
   transforms, guardrails) would let operators customize agent behavior
   without rebuilding the daemon — the strongest community-extension ask
   that currently has no answer. Needs: a `hook` WIT world (event in,
   decision/rewrite out), a hook registry bridge, and the same capability
   stack as tools.
2. **WASM routines / scheduled jobs.** The routine engine
   (`src/agent/routine_engine.rs`) is native. A WASM routine kind (cron
   schedule + `on-tick` export + tool-like capabilities) would make
   scheduled automation distributable through the extension registry.
3. **Sandboxed code execution tool.** A first-party WASM "code interpreter"
   (guest runs user-supplied scripts inside the existing sandbox) is the
   natural fit for agent-authored code: the sandbox already exists, and the
   fuel/memory/timeout envelope is exactly what untrusted generated code
   needs. Distribution via a bundled tool keeps it operator-controlled.
4. **WASM output processors / middleware** (redaction, formatting,
   translation on channel egress). Lower priority; much of this is covered
   by native redaction (`src/tools/redaction.rs`) and hooks if (1) lands.

### D. Runtime, performance, and observability

1. **No per-extension metrics.** Neither WASM tree emits usage metrics —
   only rate-limit counters in `tools/wasm/host.rs`. Add
   `tracing`-backed counters/histograms (executions, traps, fuel consumed,
   callback latency, emit counts) wired into `src/observability/` so
   operators can see a misbehaving extension before it eats the fuel budget.
2. **Trap diagnostics.** `debug_info(false)` keeps modules small but traps
   are opaque. Options: opt-in debug info per dev channel, and wasmtime
   coredump-on-trap (`Config::coredump_on_trap`) captured to a bounded
   diagnostics dir for post-mortems.
3. **Instantiation cost.** Fresh-instance-per-callback is a deliberate
   isolation choice and should stay; the cheap wins are keeping the
   compilation cache warm across restarts (already landed) and measuring
   whether `prepare` churn shows up on channel-heavy tenants before
   considering pooling.
4. **Aggregate admission bound** — see A4; implement as one shared
   semaphore in `WasmToolRuntime`/`WasmChannelRuntime` with queue metrics.

### E. Guest developer experience

1. **Rust-only today.** All bundled guests are Rust `cargo-component`
   builds for `wasm32-wasip2`. Documented templates for other guest
   toolchains (TinyGo, componentize-py, javy for JS) would widen the
   contributor pool; each needs a conformance pass before being advertised.
2. **No conformance harness.** A `wasm-conformance` test fixture (guest
   implementing the full WIT surface + host harness asserting behavior)
   would let third-party toolchains self-certify. `ic/tests/fixtures/
   test-echo-tool/` is the seed but covers only the tool world.
3. **Scaffolding.** `lunarwing extension new --kind tool|channel --lang
   rust` generating WIT bindings, capabilities sidecar, and build script
   would remove the copy-an-existing-tool onboarding tax.
4. **WIT versioning policy.** Two contracts share `lunarwing:agent@0.3.0`
   while MCP is `lunarwing:mcp@0.1.0`. Before a 0.4.0 bump, write down the
   additive-change vs major-bump rules (the loader already enforces
   same-major/not-greater; keep guests on the oldest compatible minor).

## Proposed Phasing

- **Phase 0 — Security debt (prerequisite).** Items A1–A4 minimum, A5 as a
  parallel workstream. All are already specced in
  `IRONCLAW_ADDITION_CANDIDATES.md` / the Reborn analysis; this proposal adds
  nothing new here except sequencing: **no new capability surface lands
  before the channel-egress parity fix and the TrustLevel authority
  ceiling.**
- **Phase 1 — Operability.** Per-extension metrics (D1), aggregate
  admission semaphore (A4/D4), selective channel activation (A6), trap
  coredumps (D2). Low WIT churn, immediate fleet value.
- **Phase 2 — WIT 0.4.0 capability additions.** KV/state API (B2),
  namespaced workspace-write for tools (B1), timer callback (B3); batch the
  websocket host function (B5) only if WeeChat-WSS commits to it. One WIT
  bump for all, with the versioning policy (E4) written first.
- **Phase 3 — New extension kinds.** WASM hooks (C1) first — highest demand,
  reuses the tool capability stack — then WASM routines (C2). Code
  execution (C3) as a first-party bundled tool.
- **Phase 4 — Ecosystem.** Conformance harness (E2), scaffolding (E3),
  non-Rust guest templates (E1), streaming/async evaluation (B4). Coordinate
  distribution-side work (signing, digest pinning for tools/channels the way
  MCP-over-WASM already pins) with the marketplace research in
  `docs/ops/LUNARHUB_IRONHUB_MARKETPLACE_RESEARCH.md`.

## Non-Goals

- Re-enabling `wasm_threads`, shared memory, or WASI filesystem/network
  preopens for guests — the empty-`WasiCtx` posture is intentional; expand
  via host capabilities, not ambient authority.
- Replacing MCP transports with WASM, or making WASM the default extension
  runtime — MCP-over-WASM stays opt-in per the V2 architecture doc.
- Component-model composition (wac linking of guest-to-guest calls) —
  `tool-invoke` aliasing already covers the use case with an auditable
  boundary.
- A hosted marketplace / deep links — tracked separately as v2.0.8.0+ work
  per the item #9 research.

## Open Questions

1. Should the KV/state API (B2) be DB-backed per owner-scope (durable,
   migrates with tenant) or workspace-file-backed (visible to the agent)?
   DB-backed matches secrets/settings layering; file-backed matches the
   channel workspace convention.
2. Do WASM hooks (C1) run synchronously in the turn path (latency risk,
   needs hard fuel caps) or as async observers (weaker guarantees)?
3. Is component-model async mature enough in wasmtime 36 to build streaming
   (B4) on, or should progress reporting stay a polled host import?
   *(verify against wasmtime 36 release notes before Phase 4)*
4. Which security primitives must be unified between tools, channels, and
   MCP actors before a fourth WASM runtime type (hooks) is acceptable —
   restated from V2 MCP exploration question #10; Phase 0 answers most of it.

## References

- WIT: `ic/wit/{tool,channel,mcp-server}.wit`
- Runtimes: `ic/src/tools/wasm/`, `ic/src/channels/wasm/`,
  `ic/src/tools/mcp/wasm_transport.rs`
- Capability/security: `ic/src/tools/wasm/{capabilities,allowlist,credential_injector,limits,storage}.rs`
- Lifecycle: `ic/src/extensions/manager.rs`, `ic/src/registry/`
- Prior analyses: `docs/proposals/IRONCLAW_ADDITION_CANDIDATES.md`,
  `docs/proposals/OLDPROJECT_PORT_ANALYSES/ironclaw-reborn-port-analysis.md`,
  `docs/proposals/V2_MCP_EXTENSION_ARCHITECTURE_EXPLORATION.md`,
  `docs/proposals/GITWASM/README.md`,
  `docs/ops/LUNARHUB_IRONHUB_MARKETPLACE_RESEARCH.md`
