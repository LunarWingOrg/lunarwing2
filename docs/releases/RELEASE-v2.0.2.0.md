# Release Notes for LunarWing v2.0.2.0 - Codename `Dōkutsu (洞窟)`

**Release Date:** 2026-07-24

> *Dōkutsu* (洞窟) means cave. LunarWing v2.0.2.0 goes deeper into the v2
> foundation: it strengthens extension lifecycle boundaries, Engine V2
> scheduling and recovery, bounded memory behavior, tenant migration, build
> reproducibility, and operator tooling rather than replacing the architecture.

## Overview

LunarWing v2.0.2.0 is a reliability, security, migration, and operations release
built on v2.0.1.0. Its largest runtime change is MCP lifecycle hardening:
configured servers can now be deactivated and reactivated without reinstalling,
their tools are owner-scoped, HTTP sessions shut down cleanly, and live state is
reported separately from persisted intent. Host-local stdio installation itself
predates this release; v2.0.2.0 completes its practical lifecycle and management
surface.

The release also repairs Engine V2 mission cadence and pending-gate handling,
bounds agent-facing memory and oversized action output, excludes private Engine
runtime files from retrieval, adds a Hermes-to-LunarWing importer, hardens
Kawarimi migration, preserves WeeChat buffers across managed stops, requires
fresh WASM builds during tenant upgrades, adds the Cavepony skill and WASM tool,
and introduces phase-one GitLab CI.

The release, tag, and this document use `2.0.2.0`.

Rust crates and package manifests use `2.0.2`.

Workspace-local builds and container builders pin Rust `1.96.1`.

Cargo manifests continue to declare Rust `1.92` as the MSRV.

One new database migration, `V23`, removes Engine runtime search chunks while preserving the underlying recovery documents.

DarkIRC secure contact-key exchange, mt-admin decomposition, package publishing, and deployment automation remain unimplemented.

See the [v2.0.1.0 release notes](docs/releases/RELEASE-v2.0.1.0.md) for the
previous compatibility baseline and the [release archive](docs/releases/README.md)
for historical notes.

## Highlights

- Added nondestructive MCP deactivate/reactivate lifecycle with exact tool
  ownership, transport cleanup, managed-child shutdown, fresh discovery, and
  separate `enabled` and `active` state.
- Added live gateway-backed MCP toggles, persisted-only offline toggles, in-place
  HTTP/stdio configuration updates, authenticated web routes, conversational
  `tool_deactivate` and `tool_update`, and richer `lunarwing doctor` diagnostics.
- Restricted owner-bound MCP tools so other users cannot discover or execute
  them through the shared registry.
- Added strict Engine V2 cadence parsing and real cron scheduling, including
  `next_fire_at` repair, UTC daily-budget reset, and explicit invalid-input
  errors instead of silent conversion to manual missions.
- Prevented pending-gate collisions, stale request consumption, and failed-store
  divergence between memory and persistence.
- Preserved optional tool-schema fields instead of rewriting every optional
  property into a required nullable field.
- Bounded Engine action output and agent-facing memory results, added revision-
  safe pagination, and stopped retrying known oversized requests.
- Added migration `V23` for PostgreSQL and libSQL so `engine/.runtime/**` remains
  durable for recovery but no longer enters FTS or vector retrieval.
- Added Cavepony concise-response guidance plus a capability-free WASM tool for
  deterministic compression, expansion, and statistics.
- Added the dry-run-first `hermes_kawarimi` importer for Markdown memory, YAML
  configuration, SQLite conversations, and encrypted credentials.
- Hardened Kawarimi with descriptor/stdin passphrase transport, archive and
  manifest validation, expanded-size limits, and fail-closed owner-scope checks.
- Routed onboarding upgrades through init-agnostic `lunarwing-mt-admin.sh`, with
  PostgreSQL backup and unit rendering enabled by default.
- Added graceful WeeChat `/upgrade -quit` shutdown with systemd and OpenRC
  integration, tenant-UID execution, and forced tmux fallback.
- Made tenant upgrades rebuild WASM extensions strictly and abort before stale
  artifacts can be installed.
- Replaced the old native build helper with a Linux x86_64/aarch64, cgroup-aware,
  locked build path using `cargo build --locked`.
- Added unprivileged, secret-free GitLab validation, Rust, Python, shell, and
  security jobs. Packaging, releases, and deployment remain out of scope.
- Moved canonical repository and release operations to GitLab and removed the
  Forgejo mirror workflow.

## Compatibility

| Area | v2.0.2.0 behavior |
|---|---|
| Release version | `2.0.2.0` |
| Crate/package version | `2.0.2` |
| Declared Rust MSRV | `1.92` |
| Pinned workspace/container/CI toolchain | Rust `1.96.1` |
| New SQL migration | `V23__exclude_engine_runtime_from_search.sql`; automatic on startup |
| Database parity for V23 | PostgreSQL and libSQL |
| Engine V2 defaults | Unchanged from v2.0.1.0 |
| MCP transports | HTTP, host-local stdio, and Unix socket; registry manifests support HTTP/stdio |
| MCP persisted state | Owner-scoped DB setting is authoritative when DB is enabled; disk remains available for migration or `--no-db` |
| MCP lifecycle state | `enabled` is desired persisted state; `active` is current manager state |
| MCP stdio isolation | Unsandboxed host process under daemon account; structured argv, no shell |
| MCP stdio environment | Plain configuration, not encrypted secret storage; child also inherits daemon environment |
| Mission cadence | `manual`, `hourly`, `daily`, `Nm`, `Nh`, or 5-7 field cron with zero seconds |
| Mission resolution | One minute; interval forms map to wall-clock cron, not elapsed-duration timers |
| Agent message timeout | 630 seconds by default |
| Engine orchestrator timeout | 600 seconds by default |
| Logical LLM turn budget | 370 seconds by default; `0` disables the cap |
| Backend request timeout | 120 seconds by default |
| Agent memory query | Maximum 1,024 UTF-8 bytes |
| Agent memory output | Maximum 8,000 serialized bytes |
| Engine runtime memory | Persisted but hidden from memory tools and search |
| Kawarimi bundle | Encrypted `.7z` by default; validated legacy `.tar` import retained |
| Machine migration | PostgreSQL only; staged downtime cutover |
| Hermes import | PostgreSQL-only preview; dry-run by default; live apply requires root/container host |
| WeeChat managed stop | Graceful save/quit, 20-second helper wait, forced tmux fallback |
| Tenant upgrade | Explicit target, PostgreSQL backup by default, strict fresh WASM build, unit render by default |
| Native build helper | Linux x86_64/aarch64; release profile; locked Cargo; `--wasm` is a compatibility no-op |
| GitLab CI | Phase-one source verification only; no package, release artifact, or deployment jobs |
| Canonical forge | GitLab; Forgejo mirror workflow removed |

## Changes

### MCP Lifecycle and Live Management

v2.0.1.0 already contained host-local stdio configuration and registry
validation. v2.0.2.0 adds the missing runtime lifecycle around those transports.

- Deactivation unregisters only the exact tool instances owned by that MCP
  server, closes the client and transport, terminates the HTTP session, cancels
  pending authentication, stops a managed stdio child, and persists
  `enabled = false` while retaining configuration and credentials.
- Reactivation constructs a fresh client, negotiates MCP again, rediscovers
  tools, records exact registrations, and persists `enabled = true`.
- Per-server lifecycle locks serialize activate, deactivate, update, and remove
  transitions. Collision-safe registry operations avoid deleting a replacement
  tool registered by another source.
- MCP tools carry the activating owner identity. Discovery and execution use
  owner-aware registry methods so another user sees the same response as if the
  tool did not exist.
- HTTP shutdown stops new requests, waits for in-flight work, and sends an
  authenticated `DELETE` with `Mcp-Session-Id`. The shared client bounds each
  request to 30 seconds.
- Disk configuration writes use a persistent advisory lock and atomic
  replacement. DB state changes use compare-and-set retries instead of silent
  last-writer-wins updates.
- DB load failures are now errors rather than silently falling back to an
  unrelated disk file. A dedicated one-time import path migrates legacy disk
  configuration into the owner-scoped DB key.

Current CLI management includes:

```bash
lunarwing mcp toggle NAME --disable
lunarwing mcp toggle NAME --enable
lunarwing mcp toggle NAME --disable --offline
lunarwing mcp update NAME [URL] [--transport http|stdio] \
  [--command COMMAND] [--arg ARG ...] [--env KEY=VALUE ...]
```

Live toggle uses the configured gateway and bearer token. `--offline` changes
only persisted intent. A custom `--url` requires an explicit `--token`, avoiding
accidental credential forwarding to a different origin. CLI `mcp update` changes
persistence for next startup and instructs the operator to cycle the server;
manager-backed web and conversational updates stop and restart active runtime
state.

Authenticated gateway routes now include:

- `POST /api/extensions/{name}/activate`
- `POST /api/extensions/{name}/deactivate`
- `POST /api/extensions/{name}/toggle`
- `POST /api/extensions/{name}/update`

The MCP panel exposes activation and deactivation and displays desired
`enabled` separately from current `active`. Conversational mutation tools remain
approval-sensitive and are denied by autonomous tool policy where appropriate.

`lunarwing doctor` now validates MCP configuration, reports each server's
transport and enabled state, and checks whether an enabled stdio executable can
be resolved through `PATH` without executing it. This diagnostic currently reads
disk configuration rather than owner-scoped DB state, so DB-only installations
must also use `lunarwing mcp list --verbose`.

### Engine V2 Scheduling, Gates, and Time Budgets

Mission scheduling now uses real cron parsing through `cron` and `chrono-tz`.
User-facing mission cadence accepts:

- `manual`, `hourly`, and `daily`;
- minute intervals from `1m` through `59m`;
- hour intervals from `1h` through `23h`; and
- 5-7 field cron expressions with a zero seconds field.

Interval syntax is translated to wall-clock cron. For example, `30m` means
`*/30 * * * *`; it is not a 30-minute duration measured from creation. Invalid
cadence and timezone values now fail explicitly rather than silently creating a
manual mission.

Cron missions receive `next_fire_at` on create, update, and resume. Startup
repairs older valid missions missing that value. The scheduler advances the next
occurrence even when a daily thread budget blocks execution, and daily budgets
reset on UTC date boundaries. Scheduling remains one-minute resolution and does
not replay every occurrence missed during downtime.

Pending-gate handling now enforces one live gate per user/thread, atomically
replaces expired gates, rejects stale request IDs, rolls memory state back after
persistence failure, and filters expired entries during lookup and restore.
Engine-originated failures use an explicit Engine error variant instead of a
misleading job-context error.

Tool schema adaptation no longer rewrites all optional JSON Schema properties
into required nullable properties. Source schemas pass through unchanged, so
optional fields remain optional for providers and tools.

Deep-dive time budgets increased and gained a consistency assertion:

- Engine orchestrator: 300 to 600 seconds.
- Agent message handling: 400 to 630 seconds.
- Logical LLM turn budget: 270 to 370 seconds.
- Backend request timeout: unchanged at 120 seconds unless configured.

`LLM_TURN_BUDGET_SECS` is clamped against the configured message timeout with a
30-second margin. Custom settings can still create a bad hierarchy because the
Python orchestrator timeout is independently fixed at 600 seconds; operators
must validate overrides as a set.

### Oversized Context and Retry Behavior

- Engine action output stored in orchestrator state is bounded to 8,000
  characters.
- Oversized persisted `ActionResult` values are sanitized before reuse, so old
  state cannot repeatedly poison a new request.
- Typed HTTP `413` responses are non-retryable.
- Recognized provider context-window failures are non-retryable and do not fail
  over to another provider with the same oversized request.
- Provider-message recognition remains best-effort string matching. Unknown
  wording may still retry and add latency or duplicate cost.

### Bounded Memory and Runtime Search Exclusion

Agent-facing memory now has explicit byte budgets before results enter Engine
state:

| Operation | Bound and behavior |
|---|---|
| `memory_search` query | Reject over 1,024 UTF-8 bytes before embedding or DB work |
| Search snippet | At most 2,000 JSON-encoded bytes |
| Complete tool result | At most 8,000 serialized bytes with valid UTF-8 and JSON |
| `memory_read` page | At most 6,000 source bytes plus response bound |
| `memory_tree` | At most 256 entries, depth 10, and 8,000 serialized bytes |

Search returns a ranked prefix with path, document ID, score, and hybrid-match
state. `result_count` reports emitted results, not total database matches; search
does not currently expose a separate truncation flag.

Read and tree continuation use UTF-8-safe offsets plus a content-hash revision.
If a document or directory changes between pages, continuation fails instead of
silently combining inconsistent snapshots.

`engine/.runtime/**` is now an internal boundary:

- Engine recovery documents remain in `memory_documents` so checkpoints,
  threads, events, and leases survive restart.
- New runtime writes receive no FTS or vector chunks.
- `memory_read`, `memory_write`, and `memory_tree` reject or hide normalized
  runtime paths.
- PostgreSQL migration `V23` removes old runtime chunks without deleting source
  documents.
- libSQL migration 23 applies the same cleanup.
- Similar names such as `engine/.runtime-notes.md` remain ordinary searchable
  memory.

Default-scope writes to system-prompt-bearing files also pass through the shared
prompt-injection sanitizer. Layer-specific internal write APIs still rely on
their callers to preserve this boundary.

### Hermes-to-LunarWing Kawarimi Adapter

The new `hermes_kawarimi` package provides a dry-run-first path for importing a
Hermes agent into a freshly provisioned LunarWing tenant:

```bash
python3 -m hermes_kawarimi inspect --source /path/to/hermes
python3 -m hermes_kawarimi import --source /path/to/hermes --tenant NAME
sudo python3 -m hermes_kawarimi import \
  --source /path/to/hermes --tenant NAME --apply
```

Source discovery uses explicit `--source`, then `HERMES_HOME`, then
`~/.hermes`. The adapter extracts data into a neutral snapshot, maps IDs
deterministically with UUIDv5, provisions a normal tenant through mt-admin,
starts it once so normal migrations create the schema, stops the daemon, and
populates the live PostgreSQL database. `--start` is a separate opt-in; the
default apply result remains staged and stopped.

| Hermes source | LunarWing target |
|---|---|
| `SOUL.md`, `MEMORY.md`, `USER.md`, other memory Markdown | `memory_documents` |
| Full `config.yaml` | `imported/hermes-config.json` reference document |
| Top-level YAML scalar settings | Namespaced `hermes.*` settings |
| SQLite sessions and messages | Conversations and conversation messages |
| `.env` and `auth.json` credentials | AES-256-GCM encrypted LunarWing secrets |

All imported rows use the new tenant owner from creation, avoiding the foreign
owner-scope hazard of a database restore. Re-import uses deterministic IDs and
upserts for repeatability. Markdown is treated as untrusted data, not executed.
Import plans can be saved and resumed. `KAWARIMI_SCHEMA_WAIT_SECONDS` controls
the schema readiness wait and defaults to 90 seconds.

This importer is a preview, not a fully rehearsed migration contract. The live
root/container/PostgreSQL apply path remains unverified on a real host. libSQL,
kanban/jobs, Hermes skills, embeddings, state-directory copy, an interactive
wizard, and native OAuth conversion are deferred. Additional failure-path limits
are listed under Known Issues.

See the [Hermes adapter implementation record](docs/proposals/HERMES-KAWARIMI-ADAPTER.md).

### Kawarimi Machine Migration Hardening

Encrypted `.7z` remains the default machine-migration format, but secret
transport and archive handling changed substantially.

Passphrase precedence is now:

1. `KAWARIMI_PASS_FD`
2. `KAWARIMI_PASS`
3. `KAWARIMI_PASS_FILE`
4. Hidden interactive prompt

The preferred path is an anonymous descriptor. `KAWARIMI_PASS_FILE` must be a
regular, non-symlink, current-user-owned mode-`0600` file. Environment values are
converted to internal state and removed before child execution. Both export and
import feed the passphrase to `7z` over stdin, fixing v2.0.1.0's process-argument
exposure.

Export validates the master key and passphrase before stopping writers. A real
export stops the tenant daemon and XMPP bridge, leaves PostgreSQL available for
`pg_dump`, captures selected environment and state, and writes a root-owned
mode-`0600` bundle. `--no-quiesce` is reserved for operators who already stopped
writers; current validation checks the daemon more strongly than the bridge, so
operators must verify both.

Import now:

- privately stages the input before inspection;
- validates paths, duplicate names, types, links, entry counts, and expanded
  sizes before extraction;
- applies strict top-level and environment-manifest allowlists;
- separately validates nested state archive paths and sizes;
- verifies `SECRETS_MASTER_KEY` before daemon startup;
- inspects restored owner scopes, fails on ambiguity, performs explicit rekeying,
  and refuses startup if non-target scopes remain;
- rebuilds and installs WASM instead of restoring host-specific artifacts; and
- leaves gateway, bridge, webhook, relay tokens, ports, and generated worker
  auth host-local, requiring clients to reauthenticate.

Default safety bounds are 100 GiB for the bundle, 100 GiB for restored state,
and 1,000,000 state entries. Legacy plaintext `.tar` import remains accepted
with a warning. Export/import remain PostgreSQL-only and require a downtime
cutover. See [Multi-Tenant Machine Migration](docs/ops/MT-MACHINE-MIGRATION.md).

### Onboarding CLI and Web Console

The active upgrade path no longer reimplements service-manager behavior in
Python. CLI and web flows call `lunarwing-mt-admin.sh upgrade-tenant`, preserving
systemd-user and OpenRC handling in one authority.

Upgrade now requires an explicit branch, tag, or commit target. The default
sequence is:

```text
PostgreSQL backup -> fetch/resolve target -> stop tenant stack -> checkout
-> build daemon, XMPP bridge, and WASM -> render units -> patch env -> start
```

The Python wrapper additionally requires `--apply`. PostgreSQL backup and unit
rendering are enabled unless `--no-backup` or `--skip-render` is explicitly
chosen. There is no current dry-run and no automatic rollback after stop or
checkout; a failed build may leave the tenant stopped on the target ref.

The onboarding web console supports provision, encrypted-secret management,
upgrade, Kawarimi export, and Kawarimi import. Security and reliability changes
include:

- loopback bind by default and refusal of a non-loopback real-mode bind unless
  `--allow-insecure-remote` is explicit;
- a random session token for REST and WebSocket operations;
- one privileged job at a time, with HTTP `409` for concurrent work;
- per-job passphrases excluded from serialized models, logs, and argv and passed
  through an anonymous descriptor;
- mode-`0600` audit files; and
- verification that recognizes both systemd-user `active` and OpenRC `started`
  status shapes through mt-admin output.

`--allow-insecure-remote` adds no TLS. `--no-token` disables web authentication.
Neither is suitable on an untrusted network.

### WeeChat Buffer Preservation

> **Correction recorded for v2.0.3.0:** v2.0.2 attempted this handoff but did
> not complete it reliably. Its FIFO payload was `*:/upgrade -quit` rather than
> WeeChat's required `*/upgrade -quit`, and startup omitted `--upgrade`. The
> v2.0.3.0 implementation corrects both halves, bounds stale-FIFO writes, and
> verifies a real stop/restore cycle. Do not copy the v2.0.2 payload.

Managed WeeChat shutdown now attempts to preserve open buffers, lines,
connections, and session state:

- fresh bootstrap sets `weechat.look.save_layout_on_exit buffers`;
- `lunarwing-weechat-stop.sh` attempted the handoff with the malformed FIFO
  record identified in the correction above;
- the helper waits up to 20 seconds for graceful exit;
- missing FIFO or stalled exit falls back to `tmux kill-session`;
- systemd user units use the helper in `ExecStop` with `TimeoutStopSec=30`; and
- OpenRC invokes the helper under the tenant UID so it can reach the UID-scoped
  tmux socket.

Existing non-empty WeeChat configurations remain preserved and do not receive
the layout setting automatically. Full session restoration requires the
v2.0.3.0 corrected stop/start handoff; layout saving alone is insufficient.
Tenants with pre-v2.0.2 units must re-render to replace direct kill. Tenants with
v2.0.2 units must re-render again on v2.0.3 so startup uses the restore helper.

### Strict WASM Upgrade Builds

`upgrade-tenant` now always performs a required fresh WASM build before install.
The build script uses direct Cargo component output for `wasm32-wasip2`, installs
the target for the active pinned toolchain when missing, deletes stale source
artifacts before compilation, and verifies a new artifact exists.

A failed required WASM build aborts upgrade instead of installing stale tools or
channels. Optional non-upgrade builds retain existing installed artifacts on
failure. Strict failure occurs after tenant stop and checkout, so operators must
repair the toolchain/build and restart manually. A registered source directory
that is completely absent is still silently skipped; this separate packaging
gap is listed under Known Issues.

### Native Build Helper

`scripts/build-lunarwing.sh` was rewritten for Linux x86_64/amd64 and
aarch64/arm64 hosts.

- Release is the default profile; debug remains explicit.
- The command uses `cargo build --locked --bin lunarwing`.
- Automatic jobs use the smaller of 75% of effective CPUs and available memory
  divided by `BUILD_MEMORY_PER_JOB_MB`.
- cgroup v1/v2 CPU and memory ceilings participate in detection.
- Automatic memory allowance defaults to 1,536 MiB per job.
- Explicit unsafe job overrides warn but remain available.
- Canonical target paths are validated and filesystem root is rejected.
- A nonblocking sidecar `flock` prevents concurrent build or clean corruption.
- `--dry-run`, retained logs, custom repo/target/log paths, and optional sccache
  are supported.
- `--wasm` remains accepted only as a compatibility no-op. Use
  `ic/scripts/build-wasm-extensions.sh` for extensions.
- The old `--no-kill` option is gone because the new helper never kills unrelated
  Cargo or rustc processes.

### GitLab CI and Repository Operations

Phase-one GitLab CI adds merge-request-safe source verification without
production credentials, privileged runners, tenant access, or a Docker socket.

Required jobs cover:

- composed CI configuration, shell/Python syntax, registry JSON, version policy,
  changed-line panic policy, rustfmt, and locked Cargo metadata;
- default, PostgreSQL-only, libSQL-only, and all-feature `cargo check` shapes;
- workspace library and hermetic libSQL tests;
- onboarding, web, Hermes Kawarimi, and DarkIRC Python suites;
- shell harnesses for mt-admin, migration, build, and health behavior;
- Cavepony WASM fuel compatibility;
- `cargo-deny`; and
- current-tree Gitleaks with exact-fingerprint exceptions only.

Cargo jobs run with CPU affinity `0-5`, at most two build workers, and serialized
test cases where process-environment mutation creates races. Version-check bypass
requires both a maintainer-controlled `ALLOW_VERSION_CHECK_SKIP=1` and the
`skip-version-check` merge-request label.

Clippy, architecture boundaries, ShellCheck, and scheduled secret-history scans
remain nonblocking observational jobs. PostgreSQL service integration, full WASM
and browser matrices, standalone crates, packages, release assets, deployment,
and canary rollout remain deferred.

The CI files are committed but project-runner activation evidence is not. CI
Lint, a clean required-job runner pipeline, a default-branch pipeline, and a
scheduled run remain release gates. GitLab is now the canonical repository;
the Forgejo mirror workflow and Codeberg-oriented release command files were
removed.

### Versioning, Toolchain, and Dependencies

- Main daemon, internal crates, XMPP bridge/channel, DarkIRC, and WeeChat crate
  versions moved from `2.0.1` to `2.0.2`.
- `ic/rust-toolchain.toml`, Docker builders, and active GitLab CI config use Rust
  `1.96.1`.
- Cargo manifests still declare MSRV `1.92`; the root and GitLab READMEs contain
  stale `1.92` image/toolchain text and must not be treated as active build pins.
- Engine added `cron` and `chrono-tz`; `spin` moved from `0.9.8` to `0.9.9`.
- No broad dependency refresh was performed. Major `rand`, `base64`, and
  `tower-http` migrations remain deferred.
- The GitLab phase-one pipeline does not replace existing Cargo packaging or
  produce release binaries.

## Security Fixes

- Bound MCP tool visibility and execution to the activating owner.
- Replaced prefix-based MCP cleanup with exact-instance unregister operations,
  preventing one server from removing another source's replacement tool.
- Added lifecycle serialization, atomic disk replacement, DB compare-and-set,
  transport draining, session DELETE, pending-auth cancellation, and managed
  stdio child termination.
- Required explicit credentials for a custom MCP gateway URL rather than
  forwarding a stored token to another origin.
- Tightened MCP header validation, including duplicate case variants and
  client-supplied protocol-managed headers.
- Bounded agent memory queries, snippets, pages, trees, and complete serialized
  output before persistence into Engine context.
- Excluded `engine/.runtime/**` from retrieval while retaining restart state.
- Applied prompt-injection scanning to default-scope writes of files injected
  into the system prompt.
- Stopped retry/failover for typed HTTP `413` and recognized context-window
  failures.
- Moved Kawarimi archive passphrases from process arguments to descriptor/stdin
  transport and cleared inherited passphrase variables before child execution.
- Added private staging, archive path/type/link/count/size checks, strict
  manifest allowlists, master-key validation, and fail-closed owner-scope
  reconciliation to migration import.
- Kept onboarding web passphrases out of serialization, logs, and argv; forced
  audit files to mode `0600`; and rejected accidental non-loopback real-mode bind.
- Added build target-path validation, filesystem-root rejection, nonblocking
  locking, and shell-injection regression coverage to the native build helper.
- Kept GitLab merge-request jobs unprivileged and secret-free, with no production
  network or Docker-socket requirement.

## Bug Fixes and Polish

- Fixed invalid mission cadence silently becoming manual.
- Fixed cron missions missing or failing to advance `next_fire_at`.
- Fixed daily mission budgets failing to reset at UTC day boundaries.
- Fixed pending gates colliding for the same user/thread or consuming stale
  request IDs.
- Fixed persistence failure leaving a gate live only in memory.
- Fixed optional JSON Schema properties becoming required nullable properties.
- Fixed oversized persisted action results poisoning later Engine requests.
- Fixed known oversized requests retrying or failing over unchanged.
- Fixed Engine runtime recovery state appearing in memory search and agent tree
  output.
- Fixed multibyte memory truncation producing invalid UTF-8 or invalid JSON.
- Fixed pagination combining different document or tree revisions.
- Fixed the skill-proposal UI checking `ok` when the API returns `success`.
- Fixed graceful WeeChat stop running as root on OpenRC and missing the tenant's
  tmux socket.
- Fixed Cavepony runtime regex compilation exhausting the default WASM fuel
  budget and corrected the capability-sidecar version assertion.
- Fixed DarkIRC integration-test reset handling for early `400`/`413` responses.
- Fixed CI validation, shell harness, webhook environment-race, and libSQL test
  reliability issues present in the initial pipeline implementation.
- Fixed tenant upgrades allowing stale WASM artifacts after a failed extension
  build.
- Fixed self-heal empty/whitespace state being treated as valid state rather than
  quarantined corruption.

## Upgrade Notes

1. **Back up more than PostgreSQL.** The upgrade wrapper backs up PostgreSQL by
   default, but not tenant environment files, the port registry, workspace/state,
   or rendered service units. Preserve those separately before changing refs.
2. **Upgrade all nodes together before allowing writes.** V23 removes existing
   `engine/.runtime/**` search chunks once. An old v2.0.1 node can recreate them
   because old write paths still index runtime files. Stop old writers, upgrade
   the fleet, then restart on v2.0.2.0.
3. **Allow automatic V23 migration.** No manual SQL is required. Verify runtime
   source documents remain and their search chunks are absent after startup.
4. **Review active missions before restart.** Older valid cron missions missing
   `next_fire_at` are repaired and may begin firing. Invalid cadence now fails
   instead of silently becoming manual.
5. **Audit timeout overrides.** A persisted or environment override of
   `AGENT_HANDLE_MESSAGE_TIMEOUT_SECS=400` can terminate new 600-second Engine
   work. Use at least the new default `630` unless the full hierarchy is changed
   deliberately.
6. **Clean MCP configuration before load.** Remove case-insensitive duplicate
   headers and custom `Mcp-Session-Id`, `Content-Type`, or `Accept` headers, which
   are now rejected.
7. **Confirm MCP owner scope and DB migration.** DB-backed deployments migrate
   disk configuration once into the configured owner. Old records under
   `default` may need explicit operator reconciliation for non-default owners.
8. **Treat MCP stdio configuration as shell-equivalent authority.** The process
   is unsandboxed, inherits daemon environment, and runs under the service
   account. Do not put secrets in MCP `env` or plaintext headers.
9. **Use explicit MCP toggle direction.** Prefer `--enable` or `--disable` over a
   bare inversion, especially when an HTTP server may take up to its request
   timeout to drain.
10. **Restart after CLI MCP update or remove through a manager-backed surface.**
    CLI `mcp update` and `mcp remove` change persistence only. Web/conversational
    update and remove perform runtime lifecycle cleanup.
11. **Upgrade tenants through mt-admin or the onboarding wrapper.** Supply an
    explicit target. Keep PostgreSQL backup and unit rendering enabled unless a
    rehearsed recovery plan requires otherwise.
12. **Prepare Rust and WASM before stopping tenants.** Rustup-backed builds select
    Rust `1.96.1`; upgrades need `wasm32-wasip2`. A strict WASM failure after
    checkout can leave the tenant stopped.
13. **Re-render existing WeeChat units.** On v2.0.3 this installs both corrected
    session helpers. `weechat.look.save_layout_on_exit buffers` remains a useful
    ordering fallback, but it does not replace the upgrade-state handoff.
14. **Do not rely on `build-lunarwing.sh --wasm`.** It is a no-op. Run
    `ic/scripts/build-wasm-extensions.sh` or use mt-admin's strict upgrade path.
15. **Rename saved onboarding endpoint state.** `tensorzero_url` was replaced by
    `llm_base_url`; old resume JSON can silently lose that override.
16. **Rehearse Kawarimi before production cutover.** Pause target fleet self-heal,
    verify both old daemon and bridge are stopped, preserve the master key, use
    `KAWARIMI_PASS_FD`, validate owner scope, and reauthenticate clients after
    host-local tokens regenerate.
17. **Treat Hermes apply as preview.** Inspect warnings, require a real source,
    verify imported row counts and one decrypted secret, and leave the tenant
    stopped until memory/search behavior and credentials are checked.
18. **Do not treat GitLab phase one as release delivery.** It builds no package or
    release artifact and performs no deployment. Complete CI Lint and clean-runner
    activation evidence before tagging.
19. **Review mission authorization before multi-user production use.** Current
    conversational mission pause/resume/delete paths do not consistently enforce
    owner/admin checks. Restrict these tools or fix authorization before exposing
    them across tenants.

### Configuration Quick Reference

| Setting | Default | Effect |
|---|---|---|
| `AGENT_HANDLE_MESSAGE_TIMEOUT_SECS` | `630` | Outer agent turn timeout |
| `LLM_TURN_BUDGET_SECS` | `370` | Aggregate logical LLM-call budget; `0` disables |
| `LLM_REQUEST_TIMEOUT_SECS` | `120` | Per-backend request timeout |
| `KAWARIMI_PASS_FD` | Unset | Preferred anonymous descriptor carrying migration passphrase |
| `KAWARIMI_PASS` | Unset | Legacy environment passphrase, consumed and cleared |
| `KAWARIMI_PASS_FILE` | Unset | Owner-only mode-`0600` passphrase file |
| `KAWARIMI_MAX_BUNDLE_BYTES` | `107374182400` | Maximum staged/expanded bundle bytes |
| `KAWARIMI_MAX_STATE_BYTES` | `107374182400` | Maximum expanded state bytes |
| `KAWARIMI_MAX_STATE_ENTRIES` | `1000000` | Maximum nested state entries |
| `KAWARIMI_SCHEMA_WAIT_SECONDS` | `90` | Hermes schema readiness wait |
| `BUILD_JOBS` | Auto | Explicit native build parallelism override |
| `BUILD_MEMORY_PER_JOB_MB` | `1536` | Memory allowance used by automatic job selection |
| `CARGO_TARGET_DIR` | `$HOME/.cargo-target` in helper | Native build target directory |
| `BUILD_LOG_DIR` | `$TMPDIR` or `/tmp` | Native build log directory |
| `LUNARWING_REPO` | Script-detected repo | Native build repository override |

Inherited v2.0.1.0 defaults remain unchanged for `ENGINE_V2`,
`ENGINE_V2_CHANNELS`, `SKILL_SELF_IMPROVEMENT`, and channel owner routing.

## Testing and Verification

The following outcomes are recorded in implementation documents and current
source history. They were not rerun solely for this release-note draft.

### Recorded Results

- Hermes Kawarimi: 30 unit tests recorded passing across model, extraction,
  mapping, config, and loader behavior.
- Hermes `inspect` and dry-run `import`: recorded smoke-tested against a
  synthetic source without writes.
- `cargo test --no-fail-fast`: recorded green after six known architectural
  failures were marked `#[ignore]`.

The six ignored tests remain unresolved:

- two bootstrap greeting/readiness integration tests; and
- four per-user system-prompt identity/isolation tests whose agent-loop path
  still uses one shared workspace instead of the web gateway's per-user
  `WorkspacePool`.

Run `cargo test -- --ignored` to reproduce them. A green default Cargo result
therefore means no failures among non-ignored tests, not that these six contracts
are fixed.

### Added or Expanded Regression Coverage

Committed tests cover:

- stdio and authenticated HTTP MCP deactivate/reactivate, exact tool cleanup,
  owner isolation, concurrent activation, transport shutdown, and browser
  controls;
- mission cadence parsing, cron scheduling, startup repair, UTC budget reset,
  pending-gate collision, expiry, stale request IDs, and persistence rollback;
- oversized action output and non-retryable context-limit handling;
- memory byte budgets, Unicode/JSON truncation, revision pagination, runtime
  path denial, and PostgreSQL/libSQL migration behavior;
- Cavepony dictionaries, modes, literal preservation, validation, capability
  sidecar, registry manifest, and default-fuel WASM execution;
- onboarding config, upgrade routing, secret handling, web request validation,
  one-job-at-a-time behavior, and verification propagation;
- Kawarimi encrypted archives, wrong passwords, unsafe members, manifest
  allowlists, expanded-size limits, owner-scope flags, and passphrase transport;
- WeeChat stop-helper fallback plus systemd and OpenRC rendering;
- strict WASM upgrade failure and stale-artifact prevention;
- native build resource selection, cgroups, locking, path safety, dry-run, and
  command-injection resistance; and
- DarkIRC early-reset and registry-lock integration behavior.

Presence of test code is not a recorded passing release run. No broad claim that
all of these suites passed is made here.

## Known Issues

This list reconciles the v2.0.1.0 tracker with current source. All canonical bug
reports are inherited; none became a newly opened canonical report or was fully
closed during this cycle. Release-specific findings follow separately.

### Active Canonical Bug Reports

| Area | Status | Current issue |
|---|---|---|
| [Agent and worker lifecycle](docs/bugs/BUG-agent-worker-lifecycle.md) | Partial, inherited | `create_job(wait=true)` still occupies the originating turn; ordinary messages queue while interrupts remain responsive. New default outer timeout is 630 seconds. |
| [External worker config persistence](docs/bugs/BUG-external-worker-config-persistence.md) | Partial, inherited | Full TOML rewrites can omit worker bearer tokens, so reload/restart can attempt unauthenticated worker access. |
| [Nanocode image distribution](docs/bugs/BUG-mt-nanocode-image-size.md) | Open, inherited | Rootless multi-tenant setup copies the worker image through `save | load` into each tenant store, increasing setup time and disk pressure. |
| [Kawarimi import flag parity](docs/bugs/BUG-kawarimi-import-flag-parity.md) | Open, inherited | Explicit `--with-wasm` remains rejected even though import enables WASM internally. |
| [SSH Git ref and remote HEAD](docs/bugs/BUG-ssh-git-ref-and-remote-head.md) | Partial, inherited | A bare remote whose HEAD names a missing branch can produce an empty checkout still reported as successful. |
| [Worker workspace paths](docs/bugs/BUG-worker-workspace-path-expansion.md) | Partial, inherited | Structured OpenCode/Nanocode paths normalize `~`; prompt-generated paths and Pebble remain unresolved. |
| [XMPP polling/backpressure](docs/bugs/BUG-xmpp-polling-and-backpressure.md) | Partial, inherited | A full 256-message downstream queue can block `send().await` while the poll task remains alive, preventing supervisor restart until health becomes stale. |
| [XMPP OMEMO fallback/processing](docs/bugs/BUG-xmpp-omemo-warmup-and-processing.md) | Unverified, inherited | Historical encrypted-MUC fallback spam after restart has not been reproduced or disproved; generic stuck-processing recovery is fixed. |
| [E2E test bugs](docs/bugs/BUG-e2e-test-bugs.md) | Validation gap, inherited | Synthetic clipboard behavior remains unverified; this is not a confirmed production defect. Bootstrap cases are now ignored because of a readiness race. |
| [WeeChat `rand_check`](docs/bugs/BUG-weechat-relay-rand-check.md) | Latent debt, inherited | Helper still always returns false, but no production call site currently uses it. |

### Security and Authorization Limitations

- **Mission mutation lacks consistent ownership enforcement.** Conversational
  mission pause/resume/delete paths do not reliably verify caller ownership or
  admin status. An arbitrary known mission UUID can be completed by `delete`,
  and shared-mission controls rely on caller-side checks that are absent in this
  path. Restrict mission tools in multi-user deployments until fixed.
- **MCP stdio is trusted host code.** An authenticated gateway caller able to
  configure and activate arbitrary stdio MCP effectively gains daemon-account
  code execution. Stdio uses structured argv without a shell, but runs
  unsandboxed and inherits daemon environment.
- **MCP configuration is not secret storage.** Stdio `env` and custom HTTP
  headers persist in plaintext. Use encrypted LunarWing secret mechanisms and
  tightly protect the gateway bearer token.
- **Onboarding remote override has no TLS.** `--allow-insecure-remote` only
  bypasses loopback protection. `--no-token` disables REST and WebSocket auth.
  Use neither on an untrusted network.
- **Onboarding web hardening remains incomplete.** No dedicated request body
  cap, mutation rate limit, CSRF/origin policy, or WebSocket Origin validation is
  established in the current implementation.
- **System-prompt guard is not universal.** Default-scope workspace writes scan
  prompt-bearing files, but internal layer-specific write APIs can bypass that
  sanitizer if called incorrectly.
- **Engine provenance policy is not wired into production dispatch.** Provenance
  types and `evaluate_with_provenance()` exist, but active execution paths still
  use the non-provenance policy method.

### Engine and Mission Limitations

- `mission_delete` marks a record `Completed` and removes it from active IDs but
  retains it in persistence/list output while reporting it as deleted.
- `mission_update` advertises a `name` field while one parser path reads
  `new_name`; a model call can report success without renaming.
- `event:` and `webhook:` cadence forms parse but have no runtime dispatcher and
  never fire through the scheduler.
- Startup currently loads scheduled missions only for the configured owner
  project; persisted non-owner missions can remain inactive after restart.
- Cron is wall-clock, one-minute resolution, and no-catch-up. `23h` maps to an
  hour-field cron step rather than a true elapsed 23-hour interval.
- One malformed or failed mission can abort later work in the current ticker
  cycle. Mission notifications and history are best-effort rather than durable
  outbox delivery.
- Pending-gate expiry remains hardcoded at 30 minutes and cleanup is
  traffic-driven, not a background sweep. A waiting thread can remain waiting
  until later ingress triggers cleanup.
- A gate removed in memory can reappear after restart if persisted removal fails
  and the process crashes before repair.
- Custom timeout values can violate the default-safe hierarchy. Values at or
  below the 30-second margin warn/clamp the logical LLM budget but do not resize
  every Engine layer.
- Soft agent timeout suppresses late output but allows the timed-out task to run
  briefly; a later turn can overlap old side effects during that window.
- Strict provider endpoints that relied on LunarWing's accidental schema
  rewriting may behave differently now that source tool schemas pass through.

### MCP Limitations

- No automatic stdio crash monitor invokes the existing restart helper. Recovery
  requires deactivate/reactivate or daemon restart, and `active` means manager
  membership rather than verified child liveness.
- HTTP lifecycle shutdown can wait for an in-flight 30-second request and a
  30-second session DELETE. Per-server startup/request/shutdown timeout controls
  are not implemented.
- Lifecycle locks remain allocated for each unique server name until process
  restart. Stable deployments are unaffected; high name churn grows the map.
- Same-name MCP servers cannot be active for two owners at once because runtime
  server state is globally keyed by name.
- Tool name collisions are skipped rather than replaced, so a server can become
  active with only part of its advertised tool set.
- CLI `mcp update` and `mcp remove` are persistence-only. Updating stdio through
  CLI reconstruction can drop metadata not represented by update flags.
- Web update requires redundant body `name`, defaults omitted/unknown transport
  to HTTP, omits stdio args/env from list output, and can return `success: true`
  even when reactivation failed. Re-query `active` after mutation.
- `lunarwing doctor` and status inspect disk MCP config, not owner-scoped DB
  state, and can disagree with runtime.
- Permanent removal leaves owner-scoped OAuth secrets unless an operator revokes
  and deletes them separately.
- Global daemon shutdown stops managed stdio children but does not send HTTP
  session DELETE for every active MCP server.
- Worker-local MCP, automatic npm/pip installation, secret injection, cwd
  support, per-server timeouts, and automatic crash restart remain deferred.

### Memory and Database Limitations

- `memory_search` emits a budget-limited ranked prefix but no total-match count or
  explicit truncation flag; callers cannot distinguish a complete search from
  an output-budget truncation.
- The 8,000-byte memory budget applies to the inner result; the enclosing
  `ToolOutput` serialization can be slightly larger.
- V23 has committed libSQL migration coverage, but no recorded real PostgreSQL
  integration run for the final release tree.
- Mixed-version nodes can recreate runtime search chunks after V23. Stop old
  writers and upgrade together.
- Downgrading after V23 does not restore chunks, but old code can recreate them
  on later writes.
- PostgreSQL vector search remains brute-force after migration V9 and scales
  approximately O(N x D).

### Migration and Onboarding Limitations

- Current encrypted Kawarimi cutover has script/harness coverage but no recorded
  live database, secret, workspace, and OMEMO round trip.
- Machine migration and Hermes import remain PostgreSQL-only.
- Target host self-heal can restart a newly rendered tenant during restore/rekey.
  Pause the host-global health pipeline for the migration window.
- `--no-quiesce` validates daemon state more strongly than bridge state. Verify
  both are stopped before snapshotting OMEMO state.
- Gateway, bridge, webhook, and relay tokens regenerate after machine import;
  clients must reauthenticate.
- Upgrade has no automatic rollback. Failure after stop/checkout can leave a
  tenant stopped on the new ref. Selected worker images are not rebuilt by the
  upgrade path.
- Onboarding resume data using `tensorzero_url` no longer carries that endpoint;
  migrate it to `llm_base_url`.
- CLI provision verification failures do not consistently change process exit
  status, and fresh CLI gateway port `0` can skip the TCP verification that the
  web flow resolves from the port registry.

### Hermes Import Limitations

- Live `--apply` remains unrehearsed on a real LunarWing host.
- A missing source directory produces a warning rather than a hard failure;
  apply can provision an empty tenant unless the operator stops.
- Schema-wait timeout can return before the cleanup stop, leaving the newly
  provisioned daemon running.
- Database rows commit before secret imports. Individual secret failures become
  warnings and the CLI can still report completion; verify every required
  credential.
- Secret provider metadata is mapped for reporting but currently stored with a
  null provider field.
- Imported memory documents are inserted directly without search chunks; they
  remain readable but may not enter search until rewritten or explicitly
  reindexed.
- No kanban/jobs, skills, embeddings, general state-directory copy, libSQL,
  wizard, or secret decrypt-verification phase exists.

### CI, Build, and Release Limitations

- GitLab phase-one CI is committed but not activation-proven on project runners.
- Clippy, architecture boundaries, ShellCheck, and historical secret scanning
  remain nonblocking.
- PostgreSQL service integration, browser E2E, broad WASM/channel coverage,
  standalone crates, packages, release assets, deployment, and rollout are
  absent.
- Declared MSRV is 1.92, while workspace, Docker, and active CI configuration pin
  1.96.1. The release does not establish a clean 1.92 compatibility run.
- Docker native builds and standalone WASM builds do not consistently use
  `--locked`; some build dependencies remain tag/latest based rather than digest
  or version pinned.
- The dependency lock remains behind available compatible updates; broad
  dependency modernization is deferred.
- DarkIRC secure contact-key exchange remains manual and unimplemented despite a
  completed checklist box. Current config regeneration can replace manually
  managed contact sections.
