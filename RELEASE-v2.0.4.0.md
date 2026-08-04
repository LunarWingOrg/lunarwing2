# Release Notes for LunarWing v2.0.4.0 — Codename `Odoshi (威し)`

**Release Date:** TBD

> **First-draft notice:** These notes describe the repository state from the
> `v2.0.3.0` tag through commit `be798a9` (2026-08-04). They are a pre-release
> draft, not a declaration that the final release gate, live deployment checks,
> packaging, or item #11's final release-note review has completed.

> *Odoshi* (威し) evokes warning and deterrence. LunarWing v2.0.4.0 strengthens
> the boundaries around coordinated agents, external processes, extension
> runtimes, inbound files, tenant operations, and staged DarkIRC key exchange,
> while making those protections visible and recoverable for operators.

## Overview

LunarWing v2.0.4.0 is a security, runtime-lifecycle, multi-agent coordination,
and operations release built on v2.0.3.0. It adds an opt-in coordination
pipeline for designated XMPP and IRC rooms, bounded SillyTavern-style lorebooks,
safer and faster reflex routing, managed MCP stdio crash recovery, a sandboxed
GitLab tool, and the first public-only stage of DarkIRC's secure key-exchange
workflow.

The release also hardens Engine V2 stop and gate behavior, retires the Nanocode
worker without deleting tenant data, improves Kimi Code and Pebble workers,
protects XMPP attachment downloads against SSRF and credential leakage, and
moves agent-spawned process trees into bounded lifecycle control. On systemd
hosts, build and agent workloads can run in dedicated resource-controlled
slices; the portable process-group cleanup path also applies on OpenRC.

Operational work includes a live-tested Hermes-to-LunarWing Kawarimi migration,
per-tenant WeeChat health gating, safer tenant import/export boundaries, lower
idle daemon memory, more robust health checks, dependency and CI maintenance,
and refreshed architecture, bug, operations, and root documentation.

The release and tag use `2.0.4.0`. Rust crates and package manifests use
`2.0.4`. The declared and pinned Rust toolchain remains `1.96.1`.

One database migration, `V25`, adds persisted WASM tool approval policy. It is
implemented for PostgreSQL and libSQL and runs through the normal startup
migration path.

DarkIRC automated key exchange is **not complete**: Phase 0/1 and most of Stage
A are present, but activation, peer verification, rotation/revocation, automated
delivery, and required live isolation/leak evidence remain open. Host-local MCP
lifecycle Part 1 is complete, while worker-local placement, package installation,
host-process sandboxing, and later gateway architecture remain deferred.

See the [v2.0.3.0 release notes](docs/releases/RELEASE-v2.0.3.0.md) for the
previous compatibility baseline and the [release archive](docs/releases/README.md)
for older releases.

## Highlights

- Added an opt-in multi-agent coordination pipeline for selected XMPP/IRC rooms:
  canonical room scoping, secret and tag policy, engagement and first-responder
  controls, rate limiting, persistent violations, three-strike disablement,
  authenticated local administration, and outbound coalescing.
- Added bounded lorebook support for a documented subset of SillyTavern World
  Info JSON, including deterministic activation, strict prompt-injection scans,
  caching, and hard source/render budgets.
- Reworked reflex routing so semantic matching is separately opt-in, local
  caches are checked before embedding requests, and hit telemetry uses bounded
  write-behind batches instead of one database update per response.
- Added whole-runtime MCP stdio crash recovery with fresh owner-scoped secret
  resolution, atomic tool replacement, bounded retries, circuit-open state, and
  visible lifecycle status.
- Added a sandboxed self-hosted GitLab REST API v4 WASM tool with host-side PAT
  injection, guarded writes/merges, approval policy, numeric or path project IDs,
  and safe encoded-path handling.
- Implemented public-only DarkIRC Stage A exchange artifacts, full-fingerprint
  authorization, bounded pending state, durable recovery/tombstones, typed
  helper boundaries, and migration/upgrade locking without prematurely enabling
  contact activation.
- Hardened XMPP/LunarVision file handling: private-network and DNS-rebinding
  protections, bounded attachment workers and timeouts, URL stripping/redaction,
  constant-time OCR bearer comparison, and corrected IPv6 loopback credential
  injection.
- Made Engine V2 stop signals abort and drain parallel siblings promptly across
  structured, CodeAct, and orchestrator paths; tightened gate finalization,
  cancellation, action-count, and output-safety behavior.
- Retired Nanocode as a supported worker while preserving legacy workspaces,
  images, containers, port fields, and rollback copies.
- Hardened Kimi Code worker configuration and lifecycle; provider API keys may be
  omitted for OAuth, while bridge bearer authentication remains mandatory.
- Updated the vendored Pebble worker to v0.5.0, including provider, model-catalog,
  cancellation, timeout, diagnostics, PTY, and terminal reliability work.
- Added portable process-tree termination and systemd workload/build slices;
  reduced the measured Dwarfstar idle daemon baseline from roughly 461 MiB to
  92 MiB RSS after bounded Tokio pools and sequential WASM compilation.
- Completed and live-tested a staged Hermes-to-LunarWing PostgreSQL migration;
  fixed PostgreSQL cleanup and WASM-install failure handling in Kawarimi.
- Added per-tenant WeeChat enable/disable state so health and self-heal do not
  restart intentionally disabled WeeChat services.
- Persisted web work-log summaries and decision records separately from final
  answers, while keeping live work bounded and restoring it across thread views.
- Bumped the LunarWing workspace and channel crates to `2.0.4`, updated compatible
  dependencies, and moved Wasmtime to `36.0.13` to fix `RUSTSEC-2026-0222`.

## Compatibility

| Area | v2.0.4.0 behavior |
|---|---|
| Release version | `2.0.4.0` |
| Crate/package version | `2.0.4` |
| Rust edition / MSRV | Rust 2024 / `1.96.1` |
| Pinned workspace/container/CI toolchain | Rust `1.96.1` |
| New SQL migration | `V25__wasm_approval_policy.sql`; automatic on startup |
| Database parity for V25 | PostgreSQL and libSQL |
| WASM approval values | `never`, `unless_auto_approved`, `always` |
| Supported persistent workers | Pebble, OpenCode, Kimi Code |
| Nanocode | Retired; legacy data and rollback artifacts are preserved |
| Lorebooks | Enabled by default for primary-scope `lorebooks/*.json`; group chats disabled by default |
| Reflex compiler | Disabled by default; semantic matching independently disabled by default |
| Coordination pipeline | Disabled by default; requires explicit identity, room, and admin-token configuration |
| MCP transports | HTTP/HTTPS, host-local stdio, and Unix socket |
| MCP stdio recovery | Managed whole-runtime recovery with five-attempt circuit breaker |
| XMPP inbound download policy | HTTPS, public-address targets by default; private addresses require `ALLOW_PRIVATE_IPS=1` |
| WeeChat lifecycle | Per-tenant `enable_weechat` state gates service rendering, health, and self-heal |
| Systemd tenant workloads | Separate workload/build slices plus daemon resource ceilings |
| OpenRC tenant workloads | Portable process-tree cleanup; no equivalent cgroup resource ceilings |
| CI scope | Phase-one source verification; package, release artifact, and deployment jobs remain absent |

## Changes

### Engine V2 Runtime and Mission Lifecycle

- Stop-aware parallel execution now aborts and drains remaining siblings after
  the first `ExecutionStopped` result in structured batches, CodeAct host calls,
  and Python orchestrator batches. Slow siblings no longer prolong a stop while
  retaining thread or VM state.
- Gate expiry and orphan recovery now finalize through `ThreadManager`, revoke
  leases, and remove conversation tracking rather than writing terminal state
  through incomplete shortcuts.
- The approval-resolution window is interrupt-visible and cancellable, closing
  the gap between pending-gate removal and resumed inline execution.
- `AlwaysAllow` is persisted only after an action truly succeeds; tool-level
  error results no longer earn future automatic approval.
- Tool-action attempts are capped at 50 per model step across structured,
  CodeAct, and orchestrator execution.
- Engine tool output is routed through bounded safety handling, and compact
  metadata prevents large CodeAct results from repeatedly entering model
  context.
- The default orchestrator no longer retries a valid answer solely because it is
  shorter than 80 characters. Retry nudges now require explicit tool intent or
  stronger retry/roleplay signals.
- Leaked or interrupted prose responses recover through the conversational path
  rather than leaving threads without a terminal response.
- System-managed learning missions remain recurring after successful event runs;
  startup repairs older terminal records and refreshes their compiled prompts,
  cadences, success criteria, and budgets while preserving operator pauses and
  history.
- Mission recovery preserves trigger payload and gate-paused initial state,
  limits generated action material, and isolates a failed mission so later work
  can continue.

### Multi-Agent Coordination

The four-phase coordination pipeline is implemented and inactive under the
default empty configuration. Hooks, gates, and administration require
`COORD_ENABLED=true`; the coalescer is separately activated by a nonempty
`COORD_CHANNELS` list unless `COALESCE_ENABLED=false`. It is intended for
designated pager/operations rooms rather than all traffic.

- Hook policy uses canonical destination scope (`xmpp_room`, `xmpp_target`, or
  equivalent metadata), keeping transport name separate from room identity.
- Declarative rules reject likely secrets, strip inbound newlines, and enforce
  outbound single-line and 420-character limits only for configured rooms.
- Stateful hooks add secret entropy checks, engagement policy, process-local
  first-responder claims, response-tag schema, and per-agent rate limits.
- Typed suppression replaces synthetic rejection text, preventing policy output
  from being reprocessed or incorrectly charged as another violation.
- Persistent violation state provides bounded records, seven-day decay,
  three-strike automatic disablement, and 24-hour disable periods.
- `CoordinationGate` runs before Engine V2 tool side effects and silently stops
  disabled agents.
- A bearer-authenticated administration service binds only to loopback and
  provides status, clear, disable, and enable operations. Multi-tenant
  provisioning generates and preserves a tenant token.
- A bounded outbound actor coalesces rapid same-destination messages after hooks
  and before transport rate limiting. It has queue, delivery, merge, size, and
  shutdown bounds; `ALERT:` bypasses coalescing.
- Follow-up review fixed all fourteen issues in
  [`UNVERIFIED_ISSUES.MD`](docs/ops/UNVERIFIED_ISSUES.MD), including production
  gate registration, WASM-channel coverage, room isolation, authentication,
  violation attribution, stale timers, registry races, and tag-only panic paths.

The first-responder claim map remains process-local. Separate tenant daemons in
the same room can both claim a message; a shared compare-and-insert backend is
future work.

### Lorebooks

- Added primary-workspace lorebooks at `lorebooks/*.json` with SillyTavern World
  Info object exports and a native list form.
- Supports constant and selective entries, all four `selectiveLogic` modes,
  case-sensitive and whole-word matching, per-entry scan depth, deterministic
  probability, stable priority, and bounded rendering.
- Group-chat activation defaults off because room participants can influence
  keyword triggers.
- Raw and decoded content is scanned on write and again on load. Unsafe legacy or
  direct-database entries are skipped; cross-entry and cross-file combinations
  receive combined scans before prompt rendering.
- Hard bounds cover file count and size, entries, keys, scan text and operations,
  token budget, and rendered output.
- A scope/agent LRU and bounded database batch loading avoid reparsing the full
  lorebook corpus on every turn. Writes and deletes invalidate cache entries.

This is a documented v1 import subset, not full SillyTavern parity. Insertion
positions, recursion, vector matching, groups, sticky/cooldown behavior, macros,
persona filters, regex keys, a management UI, and tokenizer-aware remaining
context are deferred.

### Reflex Compiler and Fast Paths

- Exact and fuzzy routing remain local when the compiler is enabled.
- Semantic routing now requires `REFLEX_SEMANTIC_ENABLED=true`, preventing an
  unexpected embedding-provider network request on every exact/fuzzy miss.
- The local pattern-embedding cache is checked before provider access, and only
  missing embeddings are generated while semantic matching is enabled.
- Per-request hit updates were replaced by a bounded in-memory accumulator with
  a three-second worker. PostgreSQL uses one additive `UNNEST` update; libSQL
  uses repeated prepared updates in one transaction.
- Failed or cancelled flushes merge deltas back with exponential backoff, and
  shutdown attempts a bounded final flush.
- Fixed semantic-path lock ordering, canonical owner-scoped hit attribution,
  match-count persistence, per-user compiler scope, eviction startup,
  compilation-attempt limits, disabled-pattern resurrection, fuzzy config
  wiring, and PostgreSQL prune columns.
- Infrastructure build errors deliberately do not consume pattern compilation
  attempts; regression coverage verifies retry after provider recovery.

The compiler and semantic mode both remain off by default. Later Software
Builder validation, test-harness, auto-registration, and wider v2 bridge work
remain separate tasks.

### MCP Host-Local Lifecycle

MCP Overhaul Part 1 now covers managed host-local installation and lifecycle.

- Supports HTTP/HTTPS, stdio subprocesses, and Unix sockets.
- Adds explicit install, activate, deactivate, update, remove, authentication,
  diagnostics, and health/status paths.
- Stdio secret references resolve owner-scoped at activation. Child processes
  inherit a restricted compatibility environment plus explicit configuration.
- A crashed stdio runtime is rebuilt as a whole: fresh config and secrets, new
  process/transport/client, and all-or-none tool registration.
- Recovery is generation-safe and cancellable, uses bounded jittered retries,
  requires a stability window, and opens a persistent circuit after five
  attempts.
- Atomic tool replacement avoids stale clients and partially registered tool
  sets. Recovery stops before child shutdown, preventing resurrection during
  deactivate or daemon exit.
- Browser and API status distinguish recovering and terminal circuit-open state.
- CLI removal is lifecycle-aware through the authenticated manager path;
  persisted-only cleanup requires an explicit offline mode.

MCP stdio remains trusted, unsandboxed host code under the daemon account.
Automatic npm/pip installation, package provenance, worker-local MCP,
host-process sandboxing, generic runtime adapters, MCP-over-WASM, and dedicated
or shared MCP gateways remain deferred.

### DarkIRC Secure Staged Key Exchange

Phase 0/1 and most of Stage A are implemented without enabling unfinished
activation paths.

- Added a Unix key-manager/helper boundary with locked dependencies,
  capability-safe file operations, tenant-scoped state, bounded records, and
  public-only exchange artifacts.
- `prepare` and `respond` require full-fingerprint authorization. Nicknames are
  display metadata, never authorization.
- Semantic retries occur under the exchange lock before one generator
  invocation; publication is no-replace, and byte-identical retry can recover
  durability after a final-rename failure.
- Pending exchanges enforce count, byte, expiry, and private-key reuse bounds.
  Tombstones and recovery journals survive crashes.
- Contact-preserving config regeneration and typed mt-admin helpers replace
  direct tenant-env truncation and ad hoc shell mutation.
- Structured tenant migration excludes runtime exchange state, validates source
  ownership and links, and retains lifecycle and DarkIRC writer locks through
  readiness, writer stop, manifest pinning, and archive seal.
- Migration passphrases cross the process boundary by anonymous file descriptor,
  not argv or environment, and partial archives are removed on failure.
- DarkFi generation has a fixed deadline and combined output cap; timeout or
  overflow kills and reaps its isolated process group before locks are released.
- Six Stage A completion crash windows have deterministic recovery regressions.

Stage B activation/rollback, Stage C peer verification, Stage D
rotation/revocation/same-host pairing, QR/web/trusted-channel delivery, and live
two-user process/log leak evidence are not complete. Installed Stage A pending
state intentionally blocks managed migration and upgrade until a health-gated
activation path exists.

### XMPP and LunarVision File Handling

- Recognized inbound OOB and AES-GCM attachment URLs are removed from model-visible
  message text on both successful and failed downloads.
- Download errors log redacted origin information rather than full credential- or
  key-bearing URLs; transport errors have embedded URLs removed.
- Downloads require HTTPS, reject embedded credentials and redirects, bypass
  environment proxies, resolve DNS under a deadline, reject non-public targets
  by default, and pin validated addresses to reduce DNS rebinding risk.
- Private targets remain available only through explicit `ALLOW_PRIVATE_IPS=1`,
  matching the existing WASM host policy.
- Attachment extraction moved behind a bounded FIFO worker with whole-operation,
  DNS, concurrency, URL-count, and attachment-count limits. Saturation delivers
  the message without downloads while still stripping recognized sensitive URLs.
- LunarVision bearer comparison is constant-time.
- IPv6 loopback host normalization now permits scoped vision credential injection
  where the old bracket mismatch prevented it.

Outbound XEP-0454 byte encryption, byte-bounded bridge queues, transfer-manager
decoupling, and a durable transfer outbox remain deferred.

### External Workers

#### Nanocode Retirement

- Nanocode is no longer supported. New configurations support Pebble, OpenCode,
  and Kimi Code workers.
- Tenant lifecycle operations retire legacy Nanocode supervision, active
  containers, config entries, environment keys, and enabled registry state under
  the tenant lifecycle lock.
- Retirement is deliberately non-destructive: workspaces, stopped containers,
  images, reserved names, port fields, archived environment, and first rollback
  copies remain available for manual downgrade or cleanup.
- Legacy migration bundles remain readable but discard retired Nanocode settings.
  Saved onboarding and Hermes plans that request Nanocode fail with an explicit
  retirement error.

#### Kimi Code

- `KIMI_API_KEY` is optional for persisted OAuth-backed Kimi Code sessions, but
  non-OAuth providers still require it.
- `AGENT_AUTH_TOKEN` remains mandatory for the bridge, including loopback server
  mode; Compose rejects a missing bridge token before launch.
- Added an optional toolchain image mode matching OpenCode's Rust, Go, C/C++,
  Python, and native development packages.
- Hardened workspace path overlap locking, cancellation, subprocess termination,
  progress heartbeats, context bounds, protocol compatibility, and config
  generation tests.
- External jobs default to asynchronous `wait=false`; persisted status and events
  remain readable after daemon restart.

#### Pebble 0.5.0

- Updated the vendored Pebble worker to v0.5.0.
- Added Neuralwatt and Lilac OpenAI-compatible providers plus Grok subscription
  access through the official Grok CLI ACP transport.
- Improved provider authentication checks, catalog caching and health, model
  selection, provider diagnostics, streamed cancellation, child cleanup, default
  timeouts, PTY/terminal behavior, and Unicode safety.
- Added a 32-pass default turn bound and stopped injecting legacy conversation
  dumps as global project memory.

### GitLab WASM Tool and WASM Approval Policy

- Added a sandboxed GitLab REST API v4 tool for projects, issues, merge requests,
  branches, repository files, search, pipelines/jobs, and current-user lookup.
- PAT material is injected only at the host HTTP boundary and is not exposed to
  the WASM guest.
- Merge and file mutations require current SHA/commit guards to reduce stale
  review races.
- Numeric project IDs, `group/project`, and the `project_id` alias are accepted.
- Encoded path separators are normalized for namespaced branches and nested
  files while encoded traversal and backslashes remain rejected.
- The bundled tool prompts by default but permits session/global auto-approval or
  the UI's always-allow choice.
- V25 persists `approval_policy` on tool capabilities for PostgreSQL and libSQL.
- GitLab config preload now reads `config/gitlab.json` directly instead of
  listing every workspace path for each call.
- WASM HTTP response caps are enforced while streaming, including missing or
  inaccurate `Content-Length`, rather than after full buffering.

### Multi-Tenant Operations, Health, and Resource Isolation

- Added per-tenant `enable_weechat` registry state plus `enable-weechat` and
  `disable-weechat` verbs. systemd and OpenRC health checks classify intentionally
  disabled WeeChat units as skipped, preventing self-heal restart storms after
  unit rendering.
- Fixed health fail-open behavior when systemd tenant discovery fails and locked
  registry updates against concurrent lifecycle/health reads.
- Agent-spawned shell, direct sandbox, and artifact-build processes now lead
  their own process group. Timeout, cancellation, or future drop sends bounded
  termination to the complete tree.
- On reachable systemd user managers, workloads run through transient scopes in
  `lunarwing-workloads.slice`; tenant builds use `lunarwing-build.slice`.
- Rendered systemd defaults add workload/build memory, CPU, task, and daemon
  ceilings. All are operator-overridable. OpenRC receives the portable tree
  reaper but not cgroup-equivalent hard limits.
- Bounded Tokio worker/blocking pools, sequential WASM compilation, and artifact
  shadowing reduced the live Dwarfstar daemon's recorded settled RSS from about
  461 MiB to 91.7 MiB and thread count from about 70 to 12.
- Reduced normal dev/test debug data to line tables, with a separate
  `full-debug` Cargo profile for interactive debugging.
- Added Debian, Ubuntu, and Fedora dependency lists, including Python operations
  packages and native build prerequisites; refreshed Gentoo and Arch references.

### Kawarimi and Tenant Migration

- A real running Hermes data set was inspected and migrated into a new staged
  LunarWing PostgreSQL tenant, including conversations, messages, memory,
  settings, and encrypted secrets. Row counts and one decrypted secret were
  verified before the test tenant was removed.
- Added mt-admin `start-pg` and `stop-pg` operations so Kawarimi can populate a
  stopped tenant without leaving the daemon and bridge active.
- Tracks ownership of PostgreSQL startup and stops the service on populate or
  final-start failures, avoiding leaked database services after failed imports.
- Treats WASM installation as an explicit checked phase instead of returning an
  unexplained failed result.
- Preserves secret import counts and expands CLI plan-wiring tests for workers,
  toolchains, safe defaults, apply, and start behavior.
- Hardened encrypted import/export staging under protected home-owned paths and
  kept passphrases out of insecure temporary paths.

Kawarimi remains PostgreSQL-only. Operators should still rehearse with a real
bundle and verify all restored secrets, memory/search behavior, XMPP/OMEMO state,
and regenerated host-local tokens before cutover.

### Web Gateway

- Added bounded live-work panels and persisted reasoning summaries/decision
  records so operators can inspect work history separately from the final answer.
- Final responses no longer replace the live-work record; thread history rebuilds
  the two surfaces separately.
- Live work remains bounded to avoid unbounded browser memory and is restored
  when revisiting an active thread.
- MCP lifecycle status now exposes recovery and circuit-open states through the
  browser and API.
- Coordination administration remains a separate loopback bearer-authenticated
  service, not an unauthenticated route on the gateway.

### Dependencies, CI, and Documentation

- Bumped the main daemon, internal crates, XMPP bridge/channel, DarkIRC channel,
  WeeChat relay, and Multica channel to `2.0.4`.
- Applied compatible lockfile updates across the main workspace and satellite
  crates, including Rustls `0.23.43` and HTTP `1.5.0` in the audit snapshot.
- Updated Wasmtime and its lockstep family to `36.0.13`, resolving
  `RUSTSEC-2026-0222` without adding an advisory exception.
- Added overridable, digest-pinned CI image variables so operators can mirror
  Docker Hub images into the GitLab registry and avoid anonymous pull-rate
  failures.
- Stabilized Gitleaks fixtures, shell harnesses, Kawarimi test staging, registry
  version checks, panic-policy checks, and release-branch validation.
- Refreshed root README feature/setup coverage, architecture documents, the
  active bug tracker, operational guides, package lists, and the documentation
  index against current source.

GitLab CI still implements source verification only. It does not publish a
package, create a release artifact, or deploy a tenant.

## Security Fixes

- Fixed `RUSTSEC-2026-0222` by upgrading Wasmtime to `36.0.13`.
- Added pre-side-effect coordination gating, authenticated local administration,
  canonical room policy scope, typed suppression, and correct actor attribution.
- Prevented coordination coalescer and violation-store memory growth with hard
  queues, record caps, deadlines, and debounced persistence.
- Added complete process-tree ownership for agent-spawned workloads and systemd
  resource isolation for daemon, workload, and build scopes.
- Prevented XMPP attachment SSRF, proxy inheritance, redirect following, DNS
  rebinding, sensitive URL retention, and unbounded extraction stalls.
- Made LunarVision bearer comparison constant-time and fixed IPv6 loopback
  credential injection.
- Enforced streamed WASM response limits before full buffering.
- Added owner-scoped MCP stdio secret references, restricted child environments,
  lifecycle-safe crash recovery, and atomic tool registration.
- Added guarded GitLab mutations, host-only credential injection, path
  normalization, and persisted approval policy.
- Hardened DarkIRC public artifact publication, retry/replay, tenant state,
  migration locking, passphrase transport, generator bounds, and crash recovery.
- Added prompt-injection scanning and strict source/render bounds for lorebooks.
- Fixed process and migration cleanup paths that could leave children or
  PostgreSQL services running after timeout, cancellation, or import failure.

## Upgrade Notes

1. **Back up tenant state before checkout.** Preserve PostgreSQL, tenant env,
   `/etc/lunarwing/ports.json`, workspace/state, rendered units, bridge/OMEMO
   state, and worker data. The normal upgrade wrapper does not make every one of
   these backups.
2. **Upgrade all nodes before allowing mixed-version writes.** V25 adds a new
   persisted approval field. Let startup migrations complete before traffic.
3. **Review WASM approval policy.** Existing rows receive the conservative
   `never` default. Reinstall or update tools whose manifests require
   `unless_auto_approved` or `always`, and confirm their effective policy.
4. **Expect Nanocode retirement.** Normal lifecycle operations stop legacy
   supervision and remove active Nanocode configuration, while preserving data,
   stopped containers, images, port fields, and rollback files. Do not delete
   preserved artifacts until downgrade needs are ruled out.
5. **Re-render tenant units.** This installs per-tenant WeeChat state, coordination
   token/port configuration, workload/build slices, daemon limits, and current
   worker/service definitions.
6. **Set WeeChat intent explicitly.** Use mt-admin's `enable-weechat` or
   `disable-weechat`; do not remove units as a substitute. Health and self-heal
   follow the registry flag.
7. **Review systemd resource ceilings.** Defaults fit the tested development host
   but may need `LUNARWING_MT_*` overrides for larger tenants. OpenRC receives
   process-tree cleanup but not equivalent hard resource controls.
8. **Audit coordination before enabling it.** The default empty configuration is
   inactive. Configure a unique agent ID, exact room scopes, aliases, and admin
   token. A nonempty `COORD_CHANNELS` also enables coalescing unless
   `COALESCE_ENABLED=false`, even when `COORD_ENABLED=false`. First-responder
   coordination is process-local, so do not rely on it for cross-daemon mutual
   exclusion.
9. **Review lorebook content.** Lorebooks are enabled for primary-scope files by
   default, while group-chat activation is off. The lexical scanner can reject
   benign instruction-like lore; rephrase rather than bypassing validation.
10. **Review reflex flags.** The compiler remains off by default. Semantic routing
    now needs a second explicit opt-in and can contact the embedding provider.
11. **Revalidate private attachment endpoints.** Private XMPP upload/download
    hosts now require `ALLOW_PRIVATE_IPS=1`. Use the exception only on trusted
    private networks; public-address HTTPS remains the safe default.
12. **Keep Kimi bridge authentication configured.** OAuth can replace
    `KIMI_API_KEY`, but not `AGENT_AUTH_TOKEN`. Re-render/recreate the worker if
    upgrading from a configuration that omitted bridge authentication.
13. **Treat MCP stdio as daemon-account code execution.** Review commands,
    owner-scoped secret references, and inherited environment. Kill/recovery
    smoke tests are still recommended before relying on automatic restart.
14. **Do not enable unfinished DarkIRC exchange stages.** Public Stage A artifacts
    do not activate a contact. Installed pending Stage A state blocks managed
    migration/upgrade until later activation health gates exist; cancel or
    complete it through supported lifecycle paths rather than deleting files.
15. **Rehearse Kawarimi.** It remains PostgreSQL-only and imports regenerate
    gateway, bridge, webhook, and relay tokens. Verify every required secret and
    search behavior before starting the restored tenant.
16. **Do not mistake CI for release delivery.** Current CI has no package,
    artifact publication, or deployment job. Produce and validate release
    artifacts through the documented release process.

### New and Changed Configuration Quick Reference

| Setting | Default | Effect |
|---|---:|---|
| `COORD_ENABLED` | `false` | Enables coordination hooks, gate, and administration for designated rooms |
| `COORD_AGENT_ID` | required when enabled | Local coordination actor identity |
| `COORD_CHANNELS` | empty | Exact room/destination scopes; a nonempty list also activates coalescing independently of `COORD_ENABLED` |
| `COORD_ADMIN_TOKEN` | required for admin API | Enables the loopback coordination administration service |
| `COORD_ADMIN_PORT` | `18777` single-node | Loopback administration port; multi-tenant provisioning assigns a tenant port |
| `COALESCE_ENABLED` | `true` | Enables coalescing for a nonempty `COORD_CHANNELS` list; set false to suppress it |
| `LOREBOOK_ENABLED` | `true` | Loads primary-scope `lorebooks/*.json` |
| `LOREBOOK_ALLOW_GROUP_CHATS` | `false` | Allows participant-controlled lorebook triggers in group rooms |
| `LOREBOOK_SCAN_DEPTH` | `10` | Messages scanned per entry; hard maximum 100 |
| `LOREBOOK_MAX_ENTRIES` | `20` | Entries injected per turn; hard maximum 64 |
| `LOREBOOK_TOKEN_BUDGET` | `1200` | Lorebook prompt budget; hard maximum 16000 |
| `REFLEX_COMPILER_ENABLED` | `false` | Enables exact/fuzzy reflex compilation and routing |
| `REFLEX_SEMANTIC_ENABLED` | `false` | Enables embedding-backed semantic reflex matching |
| `LUNARWING_WORKLOAD_ISOLATION` | `auto` | Uses transient systemd scopes when a user manager is reachable |
| `LUNARWING_WORKLOAD_MEMORY_MAX` | deployment-specific | Overrides agent workload memory ceiling |
| `ALLOW_PRIVATE_IPS` | unset | Set to `1` only for trusted private XMPP/LunarVision endpoints |
| `KIMI_API_KEY` | unset | Optional for OAuth-backed Kimi Code; required for non-OAuth providers |
| `AGENT_AUTH_TOKEN` | required for Kimi server mode | Authenticates LunarWing to the Kimi bridge |

## Testing and Verification

This section distinguishes implementation evidence from a final release gate.
The listed results were recorded by the corresponding implementation/audit work;
they were not all rerun together solely for this first draft.

### Recorded Results

- Coordination review: all 14 reported integration issues verified and fixed;
  focused hook, gate, coalescer, XMPP, agent, health, Engine V2, format, check,
  and all-feature clippy suites recorded green.
- P1 hardening: regressions cover bounded coalescing, violation persistence,
  streamed WASM response caps, lorebook cache/bounds, concise Engine answers,
  bounded XMPP attachment work, and concurrent gate expiry.
- MCP Part 1: full Engine suite plus focused MCP recovery, config, process,
  shutdown, update, and ownership regressions recorded green; format, check, and
  warnings-denied clippy recorded clean.
- DarkIRC Stage A: 82 key-manager tests, 20 key-helper tests, eight DarkIRC shell
  suites, Kawarimi safeguards, Cargo check, rustfmt, clippy, shell syntax, and
  ShellCheck recorded passing.
- Resource isolation: process-group timeout/drop tests, a 19-check rendering
  harness, warnings-denied clippy, and live Dwarfstar systemd scope/build/daemon
  measurements recorded successful.
- Dependency audit snapshot: default and all-feature Cargo checks plus the XMPP
  bridge check recorded clean; the later Wasmtime security update recorded
  `cargo-deny` advisories/bans/licenses/sources as passing under configured
  exceptions.
- Kawarimi: focused Python suites and a real staged Hermes PostgreSQL migration
  recorded successful, including row counts and one decrypted-secret comparison.

### Still Required Before Final Release

- Run the complete required GitLab pipeline on the final release commit without
  runner pull-rate failures.
- Run the release version/codename checks against the final tag and release-note
  location.
- Run final backend-specific and all-feature Cargo checks and the selected live
  tenant upgrade matrix.
- Complete DarkIRC's real two-user isolation and `/proc`/stdout/stderr/journal
  leak evidence; do not equate fixture coverage with that live proof.
- Smoke-test managed MCP stdio crash, recovery, circuit open, deactivate, and
  daemon shutdown against a real child process.
- Revalidate active deployment-only WeeChat, Podman health, OMEMO, and XMPP queue
  observations on upgraded tenants.
- Perform item #11's final release-note accuracy and consistency pass.

## Known Issues

### Active Canonical Bug Reports

The canonical tracker was reconciled against source on 2026-08-03. The table
below carries its current unresolved or live-validation state rather than the
older v2.0.3.0 wording.

| Area | Status | Current issue |
|---|---|---|
| [Agent and worker lifecycle](docs/bugs/BUG-agent-worker-lifecycle.md) | Partial | `wait=true` still occupies the originating turn. Priority interrupts work and ordinary messages enter a bounded deferred FIFO. External workers default to `wait=false`; sandbox jobs still default to waiting. |
| [E2E test bugs](docs/bugs/BUG-e2e-test-bugs.md) | Partial / validation gap | Bootstrap/tool fixes landed and the Gmail fixture was retired, but bootstrap browser tests remain ignored because of a harness race; clipboard behavior is unverified in the current environment. |
| [External worker config persistence](docs/bugs/BUG-external-worker-config-persistence.md) | Fixed in source; live check pending | Provisioning and TOML token re-injection have round-trip tests, but a live daemon-to-worker token-auth re-check is still needed before archiving the report. |
| [Kawarimi import flag parity](docs/bugs/BUG-kawarimi-import-flag-parity.md) | Open | Explicit `--with-wasm` is rejected even though import adds WASM internally. |
| [SSH Git ref and remote HEAD](docs/bugs/BUG-ssh-git-ref-and-remote-head.md) | Partial | Null-like refs are fixed; a bare remote whose HEAD names a missing branch can still yield an empty checkout reported as successful. |
| [WeeChat `rand_check`](docs/bugs/BUG-weechat-relay-rand-check.md) | Partial / latent | The helper still always returns false, although no current production call site uses it. |
| [Worker workspace paths](docs/bugs/BUG-worker-workspace-path-expansion.md) | Partial | Structured OpenCode paths normalize `~`; prompt-generated paths and Pebble `project_dir` remain unresolved. |
| [XMPP polling and backpressure](docs/bugs/BUG-xmpp-polling-and-backpressure.md) | Partial | Poll supervision and stall visibility are implemented, but a full downstream queue can still hold an unbounded send await. |
| [XMPP OMEMO warmup/processing](docs/bugs/BUG-xmpp-omemo-warmup-and-processing.md) | Unverified | Generic stuck-`Processing` recovery is fixed; historical MUC fallback spam has not been reproduced or closed on a current live tenant. |

### Additional Source-Verified or Deployment Follow-ups

- **Mission goal pollution:** conversation-insight payload construction does not
  filter out mission threads before selecting recent goals.
- **Multica base-path mismatch:** capability grants beginning at `/api/` do not
  match a configured base URL that includes a path prefix such as
  `/cotter-openclaw`.
- **Removed channel artifacts are not pruned automatically:** old Feishu,
  Telegram, or other WASM files left in a tenant channel directory can still load
  after registry removal. Reconcile stale tenant artifacts during upgrade.
- **WeeChat 401 and Podman `starting` reports need live revalidation:** current
  source renders matching credentials and health endpoints, but source review
  cannot repair or prove the state of an already-drifted tenant.
- **Secret-leak warning noise:** standalone 64-character hex values can trigger a
  masked warning even when they are hashes rather than secrets. No plaintext
  disclosure is established by that warning alone.
- **MCP HTTP secret references:** custom HTTP headers are supported but their
  values remain persisted directly in MCP config; owner-scoped secret-name
  references currently apply to stdio environment variables, not HTTP headers.
- **Provider/runtime streaming stalls:** the historical TensorZero-specific
  observation remains a monitoring item without enough repository evidence to
  close or reproduce it.

### Security and Runtime Limitations

- **MCP stdio is unsandboxed trusted host code.** It runs under the daemon account
  and can access that account's permissions. Crash recovery improves lifecycle,
  not containment.
- **Onboarding is a trusted local console.** It has no user-account system or TLS.
  Keep loopback binding and token authentication enabled; use an SSH tunnel for
  remote access.
- **DarkIRC Stage A is not an activated secure contact.** Live cross-user and
  process/log leak validation is incomplete; helper/daemon/attestation
  publication is not one reader-atomic host-crash transaction.
- **Coordination first-responder state is process-local.** Multiple daemons can
  still respond to the same room event.
- **`COORD_ENABLED` does not gate the coalescer.** A nonempty `COORD_CHANNELS`
  list constructs the coalescer whenever `COALESCE_ENABLED` retains its `true`
  default. Set `COALESCE_ENABLED=false` when room scopes must be configured
  without coalescing.
- **Lorebook scanning is lexical and conservative.** Benign instruction-like
  prose can be rejected, and the implemented import subset does not provide full
  SillyTavern semantics.
- **Engine output-safety ordering has a residual limitation.** The incident audit
  records that oversized prefixes are truncated before later redaction/policy
  stages, while later transformed normal-sized output is not re-capped.
- **OpenRC lacks systemd-equivalent resource ceilings.** Process-tree cleanup is
  portable, but hard memory/CPU/task accounting needs init-specific deployment
  controls.
- **Private attachment endpoints require an explicit exception.** Enabling
  `ALLOW_PRIVATE_IPS=1` weakens the public-address SSRF boundary and should be
  limited to trusted networks.

### Feature and Release Limitations

- Reflex compiler and semantic routing remain opt-in. Software Builder
  validation, test harness, auto-registration, and later bridge phases are not
  complete.
- DarkIRC Stages B-D and automated QR/web/trusted-channel delivery are not
  implemented.
- MCP package installation, provenance/revocation, host sandboxing, worker-local
  placement, MCP-over-WASM, and dedicated/shared gateway architecture are
  deferred.
- Lorebook management UI, recursion, vector matching, groups, regex keys,
  insertion roles, and tokenizer-aware remaining-context budgets are deferred.
- Kawarimi and machine migration remain PostgreSQL-only and require staged
  downtime. There is no automatic rollback after a failed checkout/import.
- Current CI does not build a distributable package or release artifact and does
  not deploy tenants.
- `ic/deny.toml` still carries eight explicit advisory exceptions: four
  `rustls-webpki` issues pinned through libSQL, one `lopdf` issue pinned through
  `pdf-extract`, the RSA Marvin timing advisory without an upstream fixed
  release, and two `failure`-crate advisories in vendored/transitive code.
- Broad major dependency modernization remains deferred, including libSQL,
  reqwest, tokio-tungstenite, rand, and the next Wasmtime major lines.
