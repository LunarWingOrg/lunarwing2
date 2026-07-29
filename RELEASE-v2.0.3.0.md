# Release Notes for LunarWing v2.0.3.0 — Codename `Sashiko (刺し子)`

**Release Date:** 2026-07-29

> *Sashiko* (刺し子) is a Japanese folk embroidery tradition — reinforcing worn
> fabric with running stitches. LunarWing v2.0.3.0 reinforces the v2 foundation:
> it stitches together Engine V2 security and lifecycle gaps, hardens external
> workers and DarkIRC delivery, replaces proprietary tunnel providers with
> self-hosted NetBird, completes the WeeChat session-restore handoff, and
> refreshes documentation across the tree.

## Overview

LunarWing v2.0.3.0 is a security, reliability, and documentation release built
on v2.0.2.0. Its largest runtime change is the completion of the Engine V2
security and lifecycle hardening track: provenance is now wired into production
dispatch, mission mutation enforces ownership, the ticker is fault-isolated,
event and webhook cadences now dispatch, and expired pending gates are swept in
the background.

The release also replaces the proprietary Cloudflare/ngrok/tailscale tunnel
providers with a self-hosted NetBird provider, hardens DarkIRC outbound delivery
with a paced adapter outbox, completes the WeeChat buffer-restore handoff that
v2.0.2.0 left broken, adds bounded context and retry logic to external workers
and a fourth persistent worker option (Kimi Code), makes the web gateway UI keep
streaming answers across thread switches and reconnects while rendering faster,
introduces Study Notes as a read/write skill with an MCP integration, repairs
the self-improving skills pipeline so learning missions actually fire and
persist extracted skills, and refreshes architecture docs, bug docs, and the
docs README index against current source.

The release, tag, and this document use `2.0.3.0`.

Rust crates and package manifests use `2.0.3`.

Workspace-local builds and container builders pin Rust `1.96.1`.

Cargo manifests declare Rust `1.96.1` as the MSRV (reconciled from `1.92` in
v2.0.2.0).

One new database migration, `V24`, removes legacy tunnel settings rows.

DarkIRC secure contact-key exchange, mt-admin decomposition, package publishing,
and deployment automation remain unimplemented.

See the [v2.0.2.0 release notes](docs/releases/RELEASE-v2.0.2.0.md) for the
previous compatibility baseline and the [release archive](docs/releases/README.md)
for historical notes.

## Highlights

- Wired Engine V2 provenance policy into production dispatch so taint-based
  capability rules now govern active execution.
- Enforced ownership/admin checks on `mission_delete` and hard-deleted missions
  instead of soft-completing them.
- Added background sweep for expired pending gates with configurable expiry.
- Wired `event:` and webhook cadence dispatchers so those mission forms now fire
  through the scheduler.
- Load all active missions at startup across projects, not only the configured
  owner.
- Isolated the Engine mission ticker so one malformed mission no longer aborts
  later work in the same cycle.
- Replaced proprietary Cloudflare/ngrok/tailscale tunnel providers with a
  self-hosted NetBird provider; removed the legacy provider code.
- Hardened DarkIRC outbound delivery with a paced adapter outbox, agent message
  budget, and durable frame-safe delivery.
- Completed the WeeChat session-restore handoff: corrected the FIFO payload to
  `*/upgrade -quit`, added `--upgrade` on startup, bounded stale-FIFO writes,
  and fixed OpenRC stop to run the graceful helper as the tenant user via
  `--chuid`.
- Added bounded context, retry logic, and extended runtime to external workers;
  hardened worker cancellation, terminal state, and timeout handling.
- Added Study Notes as a read/write skill with an approved write path and a
  read-only MCP integration.
- Repaired the self-improving skills pipeline: terminal thread events now
  broadcast (persist-first), missions receive their `trigger_payload`,
  capability leases are revoked and rolled back instead of leaking, and
  missions can persist extracted Skill documents through a real write path.
- Added Cavepony Tony Soprano speech mode.
- Refreshed architecture docs, bug docs, and the docs README index against
  current source; added Arch Linux and Gentoo Python package lists.
- Added `lunarwing doctor` MCP diagnostics, in-place MCP config updates, and
  `--strict` mode to the WASM build script.
- Improved the web gateway UI: in-progress streaming answers now survive
  thread switches and SSE reconnects, chat rendering and initial page load are
  faster, and the networking settings panel matches the NetBird tunnel
  replacement.

## Compatibility

| Area | v2.0.3.0 behavior |
|---|---|
| Release version | `2.0.3.0` |
| Crate/package version | `2.0.3` |
| Declared Rust MSRV | `1.96.1` (reconciled from `1.92`) |
| Pinned workspace/container/CI toolchain | Rust `1.96.1` |
| New SQL migration | `V24__remove_legacy_tunnel_settings.sql`; automatic on startup |
| Database parity for V24 | PostgreSQL and libSQL |
| Engine V2 defaults | Unchanged from v2.0.2.0 |
| MCP transports | HTTP, host-local stdio, and Unix socket; registry manifests support HTTP/stdio |
| MCP persisted state | Owner-scoped DB setting is authoritative when DB is enabled; disk remains available for migration or `--no-db` |
| MCP lifecycle state | `enabled` is desired persisted state; `active` is current manager state |
| MCP stdio isolation | Unsandboxed host process under daemon account; structured argv, no shell |
| Mission cadence | `manual`, `hourly`, `daily`, `Nm`, `Nh`, 5-7 field cron with zero seconds, plus `event:` and `webhook:` |
| Mission resolution | One minute; interval forms map to wall-clock cron |
| Pending-gate expiry | Configurable; background sweep replaces traffic-driven cleanup |
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
| WeeChat managed stop | Graceful save/quit with corrected FIFO payload, 20-second helper wait, forced tmux fallback, OpenRC runs helper as tenant UID |
| WeeChat startup | Uses restore helper with `--upgrade` |
| Tenant upgrade | Explicit target, PostgreSQL backup by default, strict fresh WASM build, unit render by default, OpenCode workspace ownership migration |
| Native build helper | Linux x86_64/aarch64; release profile; locked Cargo; `--wasm` is a compatibility no-op |
| GitLab CI | Phase-one source verification only; no package, release artifact, or deployment jobs |
| Canonical forge | GitLab; Forgejo mirror workflow removed |
| Tunnel providers | Self-hosted NetBird; Cloudflare/ngrok/tailscale removed |

## Changes

### Engine V2 Security and Lifecycle Hardening

v2.0.2.0 left several Engine V2 security and lifecycle gaps open. v2.0.3.0
closes them.

- Wired `evaluate_with_provenance()` into the active dispatch path so taint-
  based capability rules now govern which actions are permitted for `LlmGenerated`
  content.
- Enforced ownership/admin checks on `mission_delete`. The tool now hard-deletes
  the mission record instead of soft-completing it, and refuses deletion by
  non-owners.
- Fixed `mission_update` name field mismatch: the parser now reads the field the
  schema advertises.
- Added a background sweep for expired pending gates with configurable expiry,
  replacing the traffic-driven cleanup that could leave a waiting thread blocked
  until later ingress.
- Wired `event:` and webhook cadence dispatchers so those mission forms now fire
  through the scheduler instead of parsing but never executing.
- Load all active missions at startup across projects, not only the configured
  owner project, so persisted non-owner missions resume after restart.
- Isolated the Engine mission ticker so one malformed or failed mission no longer
  aborts later work in the same cycle.
- Hardened mission persistence and gate recovery against crash-between-write
  states.

### Tunnel Provider Replacement

The proprietary Cloudflare, ngrok, and tailscale tunnel providers have been
removed. LunarWing now ships a self-hosted NetBird tunnel provider.

- Added `ic/src/tunnel/netbird.rs` with a self-hosted NetBird provider.
- Removed `cloudflare.rs`, `ngrok.rs`, `tailscale.rs`, and the legacy provider
  dispatch in `tunnel/mod.rs`.
- Added migration `V24` to remove legacy tunnel settings rows from the database.
- Added `docs/guides/NETBIRD_TUNNEL.md` and `docs/guides/TUNNEL_PROVIDER_MIGRATION.md`.

### DarkIRC Delivery Hardening

- Added a paced adapter outbox for reliable outbound delivery, replacing the
  previous direct-send path that could flood the server.
- Added agent message budget integration so DarkIRC outbound respects the
  configured turn budget.
- Made delivery durable and frame-safe: messages are framed correctly and
  survive adapter restart.
- Added regression coverage for the outbox, pacing, and frame splitting.

### WeeChat Session Restoration

v2.0.2.0 attempted the buffer-restore handoff but left it broken: the FIFO
payload was `*:/upgrade -quit` rather than WeeChat's required `*/upgrade -quit`,
and startup omitted `--upgrade`. v2.0.3.0 corrects both halves.

- Corrected the FIFO payload to `*/upgrade -quit`.
- Added `--upgrade` on startup so WeeChat restores its previous session.
- Bounded stale-FIFO writes to avoid blocking on a dead relay.
- Fixed OpenRC stop to run the graceful stop helper as the tenant user via
  `--chuid`, so it can reach the UID-scoped tmux socket.
- Added `ic/scripts/lunarwing-weechat-start.sh` as the restore helper.
- Added live stop/restore integration coverage.

Tenants with pre-v2.0.2 units must re-render to replace direct kill. Tenants with
v2.0.2 units must re-render again on v2.0.3 so both the corrected stop helper
and the new start helper are installed.

### External Worker Hardening

- Added bounded context to external workers so oversized prompts cannot exhaust
  the worker's context window.
- Added retry logic for transient OpenCode routing failures and provider
  timeouts.
- Extended external worker job runtime beyond the previous default.
- Hardened worker cancellation and lifecycle control so a cancelled job releases
  its terminal and routing resources.
- Hardened external worker terminal state handling to avoid leaked sessions.
- Fixed OpenCode worker results and reconnects.
- Added default OpenCode worker plugin configuration.
- Migrated OpenCode workspace ownership on upgrade so imported tenants keep
  correct file ownership.
- Reset OpenCode worker start limits after upgrade.
- Reduced OpenCode Podman build storage usage.
- Rebuild selected OpenCode workers during upgrades.
- Keep tenant upgrades running with stale worker images when a rebuild is not
  possible.
- Updated the OpenCode worker Rust toolchain.
- Persist external job diagnostics across restarts.

### Study Notes Skill and MCP Integration

- Added `ic/skills/study-notes/SKILL.md` as a read/write skill for managing
  study notes within the workspace.
- Added an approved write path for Study Notes so conversational tools can
  create and update notes through the normal approval flow.
- Added a read-only MCP integration for Study Notes so external MCP clients can
  read notes without write access.
- Fixed Study Notes state handling and tunnel resets.

### Self-Improving Skills Repair

The self-improvement machinery was architecturally sound but inert in
production: four concrete breaks kept learning missions from ever producing a
persisted skill. v2.0.3.0 fixes all four. `SKILL_SELF_IMPROVEMENT` defaults and
behavior are unchanged — this work makes the machinery functional when enabled;
it does not enable it.

- Broadcast the terminal `StateChanged` thread event, but only after the thread
  is persisted as `Done`, so event-driven learning missions now fire on their
  triggers instead of only on manual/API calls. Infrastructure failures after
  execution now transition the thread to `Failed` and emit a terminal event
  rather than disappearing silently.
- Seed `state["trigger_payload"]` into mission thread state across the spawn
  chain. Mission prompts always told the agent to read it; it did not exist.
  Gate-paused missions retain the payload for resume.
- Fixed the capability-lease leak: the finisher revokes a terminal thread's
  leases, spawn/resume persistence failures roll back newly granted leases,
  concurrent resume attempts start one execution, and a startup sweep revokes
  still-valid historical leases owned by terminal, missing, or unreferencing
  threads while retaining the files as audit history.
- Added the mission-gated `__save_memory_doc__` host function so missions can
  persist Skill, fix-pattern, and orchestrator documents into the workspace
  memory-doc store — previously `memory_write` silently dropped the extra
  arguments and extracted skills survived only as prose. Arguments resolve by
  keyword then position, and `shared=True` writes to the shared space.
- Scoped the conversation-insights cadence per user (`project_id:user_id`) so
  one user's completed threads no longer consume another user's cadence in a
  shared project.
- Raised the mission meta-prompt truncation budget from 1000 to 4000 bytes.
- Note: the `profile-builder` mission referenced in earlier field notes never
  existed in code; `conversation-insights` is the profile-analog mission.
- Full root-cause analysis and graft provenance:
  `docs/ops/SIS_FIX_PROPOSAL.md`.

### Cavepony

- Added Tony Soprano speech mode to the Cavepony skill.

### MCP Management

- Added in-place MCP server configuration updates (`lunarwing mcp update`) so
  command/args/env/url can be edited without remove+reinstall; active servers
  require explicit restart confirmation.
- Added `tool_update` conversational tool and `POST /api/extensions/{name}/update`
  authenticated web route.
- Added `lunarwing doctor` MCP diagnostics: validates configuration, reports
  transport and enabled state, and resolves stdio commands through PATH.

### Web Gateway UI

- Preserved in-progress streaming responses across thread switches and SSE
  reconnects. Per-thread stream snapshots are stashed when leaving a thread
  and restored on return, so a live answer is no longer lost by switching
  threads or by a dropped event stream.
- Reduced chat rendering overhead: streamed chunks no longer churn DOM
  attributes on every flush, activity-log entries are deferred and capped
  during bursts, and auto-scroll only engages near the bottom so reading
  history during an active stream keeps its position.
- Served static gateway assets with gzip compression and deferred script
  loading, reducing initial page-load time.
- Updated the networking settings panel for the tunnel provider replacement:
  the provider selector now offers `none`, `netbird`, and `custom`, with new
  fields for the NetBird custom domain and name prefix and for the custom
  command, health URL, and URL pattern (English and Chinese labels).
- Added the Tony Soprano option to the Cavepony mode selector in the chat
  composer (English and Chinese labels).

### WASM Build

- Added `--strict` mode to `ic/scripts/build-wasm-extensions.sh` for
  fail-closed builds in CI and upgrade paths.

### Documentation Refresh

- Refreshed 6 of 9 architecture docs in `docs/architecture` against current
  source; audited 3 clean.
- Updated all active bug docs in `docs/bugs` with verified line references and
  current dates.
- Updated `docs/README.md` index: 75 missing entries added, 4 broken rows
  removed, 1 renamed link fixed.
- Updated root `README.md` for v2.0.3.0.
- Added Arch Linux multi-tenant package list (`docs/guides/ARCHLINUX_PACKAGE_LIST.md`).
- Added missing Python packages to the Gentoo multi-tenant package list.
- Added `docs/ops/XMPP_FT_REVIEW_R.md` and `docs/ops/XMPP_FT_REVIEW_A.md`
  detailing XMPP file transfer and LunarVision enrichment findings.
- Added `docs/proposals/REFLEX_COMPILER_V2_UPGRADE_PLAN.md`.
- Added `docs/proposals/MULTIAGENTTEAMORCHESTRATION_INTEGRATION_PLAN_v2.0.4.0.md`.
- Added `docs/ops/CODENAME_IDEAS_v2.0.3.0.md`.
- Added `docs/ops/SIS_FIX_PROPOSAL.md` documenting the self-improving skills
  root causes, fixes, verification coverage, and residual limits.

### Versioning, Toolchain, and Dependencies

- Main daemon, internal crates, XMPP bridge/channel, DarkIRC, and WeeChat crate
  versions moved from `2.0.2` to `2.0.3`.
- `ic/rust-toolchain.toml`, Docker builders, and active GitLab CI config use Rust
  `1.96.1`.
- Cargo manifests now declare MSRV `1.96.1` (reconciled from `1.92`).
- Bumped `base64` from `0.21` to `0.22` (Tier 2 crate audit).
- Bumped `tokio-xmpp` from `5` to `6` and `xmpp-parsers` from `0.22` to `0.23`.
- Applied pending cargo update (168 package bumps).
- Installed `cargo-deny` and removed unblocked advisory ignores from `deny.toml`.
- Added observational `rust-msrv-check` CI job on Rust `1.92` (nonblocking).

## Security Fixes

- Wired Engine V2 provenance policy into production dispatch, enabling taint-
  capability rules that were previously implemented but inactive.
- Enforced ownership/admin checks on `mission_delete`, closing the gap where an
  arbitrary known mission UUID could be deleted by any caller.
- Hard-deleted missions instead of soft-completing them, so a deleted mission no
  longer retains its record in persistence and list output.
- Added background sweep for expired pending gates, reducing the window where a
  stale gate can block a thread.
- Wired `event:` and webhook cadence dispatchers, closing the gap where those
  mission forms parsed but never fired.
- Hardened DarkIRC outbound delivery with paced, durable, frame-safe sending.
- Fixed OpenRC WeeChat stop to run the graceful helper as the tenant user,
  closing the root-execution gap that silently failed to find the tmux socket.
- Hardened external worker cancellation and terminal state to avoid leaked
  sessions and routing resources.
- Added bounded context to external workers to prevent prompt-exhaustion attacks.
- Added `--strict` WASM build mode for fail-closed CI and upgrade paths.

## Bug Fixes and Polish

- Fixed `mission_update` name field mismatch between schema and parser.
- Fixed one malformed mission aborting later work in the same ticker cycle.
- Fixed persisted non-owner missions remaining inactive after restart.
- Fixed OpenRC WeeChat stop running as root instead of tenant user.
- Fixed WeeChat FIFO payload (`*:/upgrade -quit` → `*/upgrade -quit`).
- Fixed WeeChat startup omitting `--upgrade` for session restore.
- Fixed OpenCode worker results and reconnects.
- Fixed external worker timeout handling.
- Fixed duplicate worker environment overrides.
- Fixed Study Notes state handling and tunnel resets.
- Fixed job cancellation CI regressions.
- Fixed CI parity checks and standalone locks.
- Fixed stale CI test assertions.
- Fixed DarkIRC adapter to survive malformed and control-char IRC lines.
- Fixed web gateway streams disappearing when switching threads or when the
  SSE connection dropped mid-response.
- Made worker error tests root-safe.
- Fixed release branch CI validation.

## Upgrade Notes

1. **Back up more than PostgreSQL.** The upgrade wrapper backs up PostgreSQL by
   default, but not tenant environment files, the port registry, workspace/state,
   or rendered service units. Preserve those separately before changing refs.
2. **Upgrade all nodes together before allowing writes.** V24 removes legacy
   tunnel settings rows once. An old v2.0.2 node can recreate them because old
   code still writes those settings. Stop old writers, upgrade the fleet, then
   restart on v2.0.3.0.
3. **Allow automatic V24 migration.** No manual SQL is required. Verify legacy
   tunnel settings rows are absent after startup.
4. **Review active missions before restart.** Missions with `event:` or
   `webhook:` cadences now dispatch for the first time. Verify those missions
   are intended to fire.
5. **Audit timeout overrides.** A persisted or environment override of
   `AGENT_HANDLE_MESSAGE_TIMEOUT_SECS=400` can terminate new 600-second Engine
   work. Use at least the new default `630` unless the full hierarchy is changed
   deliberately.
6. **Clean MCP configuration before load.** Remove case-insensitive duplicate
   headers and custom `Mcp-Session-Id`, `Content-Type`, or `Accept` headers,
   which are now rejected.
7. **Confirm MCP owner scope and DB migration.** DB-backed deployments migrate
   disk configuration once into the configured owner. Old records under
   `default` may need explicit operator reconciliation for non-default owners.
8. **Treat MCP stdio configuration as shell-equivalent authority.** The process
   is unsandboxed, inherits daemon environment, and runs under the service
   account. Do not put secrets in MCP `env` or plaintext headers.
9. **Use explicit MCP toggle direction.** Prefer `--enable` or `--disable` over
   a bare inversion, especially when an HTTP server may take up to its request
   timeout to drain.
10. **Restart after CLI MCP update or remove through a manager-backed surface.**
    CLI `mcp update` and `mcp remove` change persistence only. Web/conversational
    update and remove perform runtime lifecycle cleanup.
11. **Upgrade tenants through mt-admin or the onboarding wrapper.** Supply an
    explicit target. Keep PostgreSQL backup and unit rendering enabled unless a
    rehearsed recovery plan requires otherwise.
12. **Prepare Rust and WASM before stopping tenants.** Rustup-backed builds
    select Rust `1.96.1`; upgrades need `wasm32-wasip2`. A strict WASM failure
    after checkout can leave the tenant stopped.
13. **Re-render existing WeeChat units.** v2.0.3 installs both the corrected
    stop helper and the new start helper with `--upgrade`. Tenants with v2.0.2
    units must re-render to get both halves of the session-restore handoff.
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
18. **Do not treat GitLab phase one as release delivery.** It builds no package
    or release artifact and performs no deployment. Complete CI Lint and
    clean-runner activation evidence before tagging.
19. **Review mission authorization before multi-user production use.** Mission
    pause and resume now enforce ownership; delete now hard-deletes with an
    ownership check. Shared-mission controls still rely on caller-side admin
    checks that are not fully wired in the conversational path.
20. **Migrate tunnel providers.** Cloudflare, ngrok, and tailscale providers are
    removed. Configure the self-hosted NetBird provider or a static/custom
    tunnel before the next restart.

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

Inherited v2.0.2.0 defaults remain unchanged for `ENGINE_V2`,
`ENGINE_V2_CHANNELS`, `SKILL_SELF_IMPROVEMENT`, and channel owner routing.

## Testing and Verification

The following outcomes are recorded in implementation documents and current
source history. They were not rerun solely for this release-note draft.

### Recorded Results

- Hermes Kawarimi: 30 unit tests recorded passing across model, extraction,
  mapping, config, and loader behavior.
- Hermes `inspect` and dry-run `import`: recorded smoke-tested against a
  synthetic source without writes.
- `cargo test --no-fail-fast`: recorded green after known architectural
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

- Engine V2 provenance dispatch, ownership-checked delete, name field fix,
  ticker fault isolation, event/webhook dispatch, cross-project mission loading,
  gate sweep, and gate expiry;
- DarkIRC adapter outbox, pacing, frame splitting, and malformed-line survival;
- WeeChat corrected stop helper, start helper, live stop/restore, and OpenRC
  `--chuid` rendering;
- external worker bounded context, retry, cancellation, terminal state, and
  timeout handling;
- Study Notes read/write skill and MCP integration;
- self-improving skills: trigger-payload seeding, persist-before-broadcast
  terminal events, gate-pause payload retention, lease revoke/rollback/startup
  sweep, per-user insights cadence, and the mission-gated
  `__save_memory_doc__` write path;
- web gateway stream preservation across thread switches and SSE reconnects,
  DOM-attribute-free streaming flushes, deferred log rendering, single history
  reconciliation on reconnect, and gzip static-asset compression;
- stdio and authenticated HTTP MCP deactivate/reactivate, exact tool cleanup,
  owner isolation, concurrent activation, transport shutdown, and browser
  controls;
- in-place MCP config update persistence and lifecycle restart;
- mission cadence parsing, cron scheduling, startup repair, UTC budget reset,
  pending-gate collision, expiry, stale request IDs, and persistence rollback;
- oversized action output and non-retryable context-limit handling;
- memory byte budgets, Unicode/JSON truncation, revision pagination, runtime
  path denial, and PostgreSQL/libSQL migration behavior;
- Cavepony dictionaries, modes, literal preservation, validation, capability
  sidecar, registry manifest, default-fuel WASM execution, and Tony Soprano mode;
- onboarding config, upgrade routing, secret handling, web request validation,
  one-job-at-a-time behavior, and verification propagation;
- Kawarimi encrypted archives, wrong passwords, unsafe members, manifest
  allowlists, expanded-size limits, owner-scope flags, and passphrase transport;
- strict WASM upgrade failure and stale-artifact prevention;
- native build resource selection, cgroups, locking, path safety, dry-run, and
  command-injection resistance;
- DarkIRC early-reset and registry-lock integration behavior.

## Known Issues

### Active Canonical Bug Reports

| Area | Status | Current issue |
|---|---|---|
| [Agent and worker lifecycle](docs/bugs/BUG-agent-worker-lifecycle.md) | Partial, inherited | `create_job(wait=true)` still occupies the originating turn; ordinary messages queue while interrupts remain responsive. New default outer timeout is 630 seconds. |
| [External worker config persistence](docs/bugs/BUG-external-worker-config-persistence.md) | Partial, inherited | Full TOML rewrites can omit worker bearer tokens, so reload/restart can attempt unauthenticated worker access. |
| [Nanocode image distribution](docs/bugs/BUG-mt-nanocode-image-size.md) | Open, inherited | Rootless multi-tenant setup copies the worker image through `save | load` into each tenant store, increasing setup time and disk pressure. |
| [Kawarimi import flag parity](docs/bugs/BUG-kawarimi-import-flag-parity.md) | Open, inherited | Explicit `--with-wasm` remains rejected even though import enables WASM internally. |
| [SSH Git ref and remote HEAD](docs/bugs/BUG-ssh-git-ref-and-remote-head.md) | Partial, inherited | A bare remote whose HEAD names a missing branch can produce an empty checkout still reported as successful. |
| [Worker workspace paths](docs/bugs/BUG-worker-workspace-path-expansion.md) | Partial, inherited | Structured OpenCode/Nanocode paths normalize `~`; prompt-generated paths and Pebble remain unresolved. |
| [E2E test bugs](docs/bugs/BUG-e2e-test-bugs.md) | Validation gap, inherited | Synthetic clipboard behavior remains unverified; this is not a confirmed production defect. Bootstrap cases are now ignored because of a readiness race. |
| [WeeChat `rand_check`](docs/bugs/BUG-weechat-relay-rand-check.md) | Latent debt, inherited | Helper still always returns false, but no production call site currently uses it. |

### Newly Opened or Updated in v2.0.3.0

| Area | Status | Issue |
|---|---|---|
| [Agent loop blocks on worker jobs](docs/bugs/BUG-agent-loop-blocks-on-worker-jobs.md) | Open, new | `create_job(wait=true)` blocks the originating agent turn; long-running worker jobs can stall the user's conversation. |

### Security and Authorization Limitations

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
- **Shared-mission admin checks are not fully wired.** Mission pause and resume
  enforce ownership; delete now hard-deletes with an ownership check. But shared-
  mission controls in the conversational path still rely on caller-side admin
  checks that are not enforced by the engine.

### Engine and Mission Limitations

- `event:` and webhook cadences now dispatch, but their runtime behavior is
  newly enabled and has not been rehearsed on a production workload.
- Self-improvement triggers depend on the Engine V2 `ThreadManager` broadcast;
  work performed by the v1 harness, main assistant sessions, or external
  workers emits no such events and produces no learning triggers. End-to-end
  live validation of the extraction loop (thread completion → `skill-extraction`
  firing → a `source: "extracted"` Skill doc) is still owed on a real tenant.
- Cron is wall-clock, one-minute resolution, and no-catch-up. `23h` maps to an
  hour-field cron step rather than a true elapsed 23-hour interval.
- Mission notifications and history are best-effort rather than durable outbox
  delivery.
- Pending-gate expiry is now configurable but defaults to the previous 30-minute
  value; operators should tune it for their workload.
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

### Release Limitations

1. The dependency lock remains behind available compatible updates; broad dependency modernization is deferred.
2. DarkIRC secure contact-key exchange remains manual. Automatic key exchange feature still in development.
