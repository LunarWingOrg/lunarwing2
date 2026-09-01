# Release Notes for LunarWing v2.1.0.0

## Codename: Ma-ai (間合い)

**Status: FIRST DRAFT (pre-release).** Release date TBD. An accuracy pass
(see `docs/ops/AGENT_GOALS_2.1.0.0.md`, item 22) will verify every claim
against the final shipped tree and reconcile the known-issues list against
the work-item tracker before this file is finalized and moved alongside the
other notes in `docs/releases/`.

>Distance is a decision
>
>The blade learns when not to swing
>
>Interrupt the storm mid-word
>
>Steer the turn while it still breathes
>
>Between two beats lives the strike
>
>間合い

## Overview

LunarWing v2.1.0.0 is an agent-interaction control, voice, supply-chain
hardening, and release-engineering release. Its headline feature gives
operators real-time control over a running agent turn: a mid-turn interrupt
aborts an in-flight LLM stream and tool execution — not only at turn
boundaries — and a Hermes-style steer injects guidance into the running turn
that the agent sees immediately after the current tool call completes. LunarVoice
(the Whisper speech-to-text sidecar) is completed to production quality with a
full lifecycle, registry-aware health integration, and documented configuration
surface. The entire crates.io dependency closure is now vendored into the
repository as a committed source-replacement directory, so every build resolves
offline from sources the project controls.

A second wave hardens correctness and operability: every healthpoint emits
exportable Prometheus metrics with env-configurable early-warning thresholds;
LLM `finish_reason` is threaded end-to-end through engine, bridge, and proxy
surfaces in one canonical spelling; the fabricated-tool-success detector learns
passive-voice claims; evaluation error ranking becomes deterministic; the
coordination-knowledge publisher allowlist gains a room-bare-JID fallback; and
the onboarding web GUI gains an executable headless-Chromium test suite.

The release and tag use `2.1.0.0`; LunarWing-owned Rust crates and channel
packages use `2.1.0`. Rust 2024 and MSRV `1.96.1` are unchanged. There are no
new SQL migrations (`V26__study_notes_knowledge.sql` remains latest) and no new
multi-tenant port-registry migration (v16 remains current).

See the [v2.0.9.0 release notes](docs/releases/RELEASE-v2.0.9.0.md) for the
previous compatibility baseline.

## Highlights

- **Mid-turn interrupt.** Each chat turn now carries a per-thread
  `CancellationToken` from a new `agent::interrupt::TurnInterrupts` registry.
  Every LLM call and tool batch in the agentic loop races against that token
  via `tokio::select!`, so `/interrupt` cancels an in-flight provider stream or
  a running tool batch mid-poll instead of waiting for a step boundary.
  Turn-boundary interrupt behavior (state flag, queue clearing, approvals) is
  unchanged.
- **Hermes-style steer.** `/steer <note>` queues a mid-turn note (capped 4-note
  buffer, drained as one merged message) that is injected as a user message
  after the current tool call's results, preserving role alternation and the
  tool-calling loop. Steer notes bypass the deferred-prompt scheduler through
  priority dispatch. Live verification on a throwaway tenant passed 6/6
  surface checks (web gateway, WeeChat, XMPP × interrupt + steer); the Engine
  V2 steer gap found during that run was fixed by routing steer through the
  bridge into the scoped running engine thread. Evidence:
  [`docs/ops/v2.1.0.0_GOAL9_STEER_INTERRUPT_LIVE.md`](docs/ops/v2.1.0.0_GOAL9_STEER_INTERRUPT_LIVE.md).
- **LunarVoice completed to production.** mt-admin now provides
  install/configure/activate/deactivate/remove for the voice sidecar with
  transactional reconfiguration, model verification, per-tenant ports, systemd
  user Quadlet (and older-Podman fallback), and OpenRC supervision. Health is
  registry-aware across `/health`, `/ready`, and `/metrics` on both init
  systems; Whisper `auto` language normalization and validation were hardened;
  full production deployment runbooks are in-tree.
- **Every healthpoint emits metrics.** The host health pipeline's models check
  now emits an exportable numeric metrics block (provider counts, max and
  per-provider latency), and all six legacy healthpoints expose
  degraded/critical thresholds as `HEALTH_<CHECK>_*` environment knobs, feeding
  the per-tenant `health_metrics` port-registry exporter with configurable
  early-warning signals.
- **The dependency supply chain is vendored.** All crates.io dependencies are
  committed under `ic/vendor/` with `[source.crates-io] replace-with` in
  `ic/.cargo/config.toml`. Every cargo command under `ic/` resolves exclusively
  from the vendor tree — `cargo check` passes with no network access — and
  `ic/scripts/refresh-vendor.sh` re-vendors, asserts the set still matches the
  committed lockfiles, and re-runs `cargo deny check advisories` after any
  dependency change.
- **LLM `finish_reason` is no longer dropped.** Engine V2 `LlmOutput` carries a
  canonical `finish_reason` string; the bridge's blocking completion adapters
  pass it through instead of discarding it; and the orchestrator proxy, stream
  terminal metadata, and engine surfaces now share one serializer
  (`FinishReason::as_str()`) and one parser (with a `"tool_use"` legacy alias
  for rolling-upgrade interop).
- **Smarter fabricated-claim detection.** The orchestrator's false-claim
  detector now catches passive-voice completion claims ("was written", "was
  saved", "was stored", "was persisted", "was retrieved") and space-separated
  tool mentions ("memory write"), with positive and negative regression
  coverage.
- **Deterministic evaluation error ranking.** `MetricsCollector::summary()`
  sorts error types by descending frequency with an ascending name
  tie-breaker, so the top-three cutoff is stable for equal counts.
- **Room-wide coordination-knowledge publishers.** The KNOW-share publisher
  allowlist now matches a full XMPP occupant JID or a room bare JID as an
  explicit room-wide opt-in. Previously neither form could match a MUC
  occupant sender, so groupchat shares were silently ignored.
- **Onboarding web GUI covered by real browser tests.** A headless-Chromium
  suite exercises picker, settings, all five operations (provision, secrets,
  upgrade, export, import), token handling, and console errors. It exposed and
  fixed a `--no-token` UX bug that showed a spurious "session token missing"
  warning.

## Compatibility

| Area | v2.1.0.0 behavior |
|---|---|
| Release version | `2.1.0.0` |
| Crate/package version | `2.1.0` for LunarWing-owned release packages |
| Rust edition / MSRV | Rust 2024 / `1.96.1` |
| New SQL migration | None; `V26__study_notes_knowledge.sql` remains latest |
| Multi-tenant port registry | v16; no schema change from v2.0.9.0 |
| Channel registry patches | darkirc 0.1.6 (no change), multica 0.1.9, weechat 0.2.7 (no change), xmpp 0.1.12 |
| Tool registry patches | No change from v2.0.9.0 (GitLab remains 0.1.3) |
| Dependency resolution | Committed vendor tree (`ic/vendor/`) source-replaces crates.io for all `ic/` builds |
| Worker image tool pins | opencode `v1.18.25`, kimi-code `0.39.1` |
| CI scope | Source verification; no package publication or tenant deployment |

## Changes

### Mid-Turn Interrupt and Steer (Hermes-style turn control)

Item 3 of the v2.1.0.0 cycle, with live verification and fixes from item 9:

- A new `agent::interrupt::TurnInterrupts` registry hands each chat turn a
  `CancellationToken`; `Agent::run_agentic_loop` registers per thread with an
  RAII guard. `AgenticLoopConfig.interrupt_token` races every LLM call and
  tool batch through `tokio::select!` — cancelling aborts the in-flight
  stream/batch mid-poll and returns `LoopOutcome::Stopped`, threading through
  the existing "Interrupted" mapping.
- `Thread.interrupt()` state flag and `check_signals` remain the path when no
  token is live (e.g. approval-pending), and a boundary token check catches
  cancels between steps. Job/container loop consumers keep boundary-only
  behavior by design.
- `Thread.steer_notes` (cap 4, merged-newline drain, cleared on `interrupt()`)
  plus `Submission::Steer` via `/steer <note>` with priority dispatch accept
  notes only while a turn is `Processing`. The delegate drains notes into
  `LoopSignal::InjectMessage` at the next iteration boundary — after the
  current tool call's results, as a user message — so role alternation and the
  tool-calling loop survive intact. The REPL exposes `/steer` in completion
  and help.
- Live verification on a throwaway test tenant (`dragon`, OpenRC host) with
  throwaway prosody and miniircd servers passed all six required surfaces:
  gateway/WeeChat/XMPP × interrupt/steer. Steered turns demonstrably changed
  topic mid-turn, and interrupted turns terminated early. Idle-state controls
  answer "Nothing to interrupt." as before.
- Fix found by that live run: under tenant-default `ENGINE_V2=true` a mid-turn
  `/steer` previously fell through to the legacy loop and answered a
  misleading "Nothing is running..." — the bridge now owns steer on
  engine-v2-routed channels via `bridge::handle_steer`, injecting the note
  into the scoped running engine thread with the same wrap framing as the
  classic loop.

### LunarVoice Production Completion

The Whisper speech-to-text sidecar (started in the 2.0.7 era) is finished to
production quality:

- mt-admin exposes voice install/configure/activate/deactivate/remove with
  transactional reconfiguration (atomic lock, rollback on failure), Whisper
  model verification, and per-tenant port allocation.
- Service management covers both init families: systemd user Quadlet with a
  managed fallback for older Podman, and an OpenRC service paired with a
  crash supervisor. Both readiness-gate startup on STT health.
- Health integration is ports-registry aware: `/health`, `/ready`, and
  `/metrics` probes discover each enabled tenant's published voice ports on
  both init systems and fail closed on ambiguous mappings.
- Whisper `auto` language handling was normalized and validated end-to-end
  (request languages, direct engine languages, CLI validation, canonical
  code set including Cantonese); no-ONNX builds stay warning-free; synthesis
  request bodies and compressed audio are bounded.
- Documentation: complete environment template, production lifecycle in the
  design doc, a standalone deployment runbook, setup-guide lifecycle section,
  per-distro lifecycle runbooks (Fedora/Debian/Arch/Gentoo), and the health
  readiness contract.

### Per-Healthpoint Metrics Exporters

Completing the v2.0.8.0 exporter design
([`docs/proposals/HEALTHPOINT_METRICS_EXPORTER.md`](docs/proposals/HEALTHPOINT_METRICS_EXPORTER.md)):

- `health-models.sh` emits an exportable numeric `metrics` block — provider
  counts, `latency_max_ms`, and per-provider `provider_latency_ms` /
  `provider_status` maps — so the Prometheus textfile exporter no longer drops
  the models healthpoint.
- Degraded/critical thresholds for the six legacy checks (gateway, xmpp,
  ratelimit, clickhouse, tensorzero, models) are env-configurable via
  `HEALTH_<CHECK>_*` knobs with numeric-validation fallback, enabling
  early-warning signals before a hard failure alerts.
- The exporter suite covers the new series and per-metric threshold crossings;
  README/env tables, the proposal status, and `config.sh.example` document the
  knobs with a sample early-warning rule per healthpoint.

### Fabricated Tool-Success Detection (F4 hardening)

The passive-voice false-claim detector was re-ported against current release
code (the stale dev-branch stash was not applied):

- Five passive-voice completion claims ("was written", "was saved", "was
  stored", "was persisted", "was retrieved") and both tool-mention forms
  ("memory write", "memory/") are now recognized by
  `signals_claimed_tool_success()`.
- Regression coverage pins every passive form positively, plus passive prose
  without a tool mention and tool mentions without a completion claim
  negatively; the full engine suite stays green (no false-positive
  regressions).

### LLM `finish_reason` End-to-End (work item #95)

The engine was previously blind to provider finish reasons on several
surfaces. Per the work item, one canonical spelling now threads everywhere:

- Engine: `LlmOutput` gains `pub finish_reason: String`; the blocking→stream
  fallback emits the declared reason; `collect_llm_stream` captures the Done
  chunk's reason instead of discarding it.
- Bridge: `map_completion_response` / `map_tool_response` map
  `FinishReason::as_str()` into `LlmOutput.finish_reason` with pass-through
  regression tests for `"length"` and `"tool_calls"`.
- Proxy: `format_finish_reason` delegates to `as_str()` and the worker parser
  delegates to `from_stream_reason()` (extended with the legacy `"tool_use"`
  alias for rolling-upgrade interop) — one serializer and one parser across
  proxy, stream, and engine surfaces.

### Deterministic Evaluation Error Ranking (work item #96)

`MetricsCollector::summary()` sorts error types by descending frequency before
taking the top three, with an ascending error-name tie-breaker so equal counts
rank deterministically. A regression test seeds five categories and verifies
the cutoff behavior for ties.

### Coordination-Knowledge Publisher Allowlist (work item #97)

Ruffles' analysis was confirmed at source: XMPP groupchat `sender_id` is the
full occupant JID (`room@server/nick`), so literal equality against the
allowlist could never match either a room bare JID or a user bare JID.

- `is_allowed_publisher()` now matches the full occupant JID **or** the room
  bare JID (occupant nick stripped) as an explicit room-wide opt-in. User bare
  JIDs still never match MUC occupant senders; a full-occupant entry pins to
  the exact nick (nick change = config change), now documented.
- Regression coverage pins the matching rules and includes an integration test
  ingesting a `KNOW:` share via a room-bare entry while an unlisted room's
  occupant stays ignored.
- Docs spell out both entry forms and warn that an empty
  `XMPP_ALLOW_ROOMS_JSON` means join-no-rooms + drop-all-groupchat.

### Onboarding Web GUI Browser Testing

The multi-tenant onboarding web console gains executable end-to-end coverage:

- A headless-Chromium suite (`lunarwing_mt_onboard_web/browser_tests.py`,
  optional Playwright dependency) exercises the picker, assets, settings,
  client-side validation, REST + WebSocket completion for all five operations,
  failure/result rendering, generated-credential display, and token handling
  (missing-token rejection, valid-token acceptance, `sessionStorage` reuse) —
  with zero unexpected console errors or failed requests.
- Real bug found and fixed: with the launcher in `--no-token` mode the UI
  unconditionally displayed "Session token missing or invalid". The warning
  now appears only after a backend `401`, preserving required-token rejection
  without false warnings.

### Dependency Supply Chain Vendoring

All crates.io dependencies are now vendored into the repository:

- `cargo vendor` output for the `ic/` workspace plus ten satellite lockfile
  roots under `ic/` (xmpp-bridge, the WASM channel crates, gotify tools,
  unix-socket client, test fixtures, openclaw-ports bridge) is committed under
  `ic/vendor/` — 1048 crate directories covering 842 unique names, including
  the pre-vendored libsignal-protocol path dependencies. `tools-src/gitlab`
  stays excluded (pre-existing stale lockfile; see Known Issues).
- `ic/.cargo/config.toml` source-replaces crates.io with the vendor directory.
  `cargo check` (and metadata) passes with `CARGO_NET_OFFLINE=true` — no
  crates.io access needed or permitted.
- `ic/scripts/refresh-vendor.sh` re-vendors with `--locked --no-delete`,
  regenerates libsignal checksums, asserts the vendored set still matches the
  committed lockfile pins set-wise (report-only; never re-resolves), then runs
  `cargo deny check advisories`. Refresh output is reviewed and committed with
  the manifest/lockfile change.
- Repo-wide scanners (`scan-secrets.sh`, `shell-syntax.sh`, `shellcheck.sh`,
  `python_syntax.py`) exclude the pinned third-party tree; new scanners must
  stay vendor-aware (documented in both `AGENTS.md` files).
- Advisory scanning is unchanged: `cargo deny check advisories` cross-references
  Cargo.lock against the RustSec advisory-db.

### Dependency, Version, and Worker Updates

- LunarWing-owned crates and channel packages moved from 2.0.9 to 2.1.0;
  registry patches moved to multica 0.1.9 and xmpp 0.1.12 (darkirc 0.1.6 and
  weechat 0.2.7 unchanged; tool registries unchanged).
- External crate updates: fs4 removed in favor of std `File::lock` /
  `try_lock` (Rust 1.89+, below the 1.96.1 MSRV), preserving
  `TryLockError::WouldBlock` semantics across bootstrap, gate persistence,
  pairing store, darkirc key manager, and MCP config; `html-to-markdown-rs`
  2.3 → 3.11.6; ocr-sidecar `reqwest` 0.12 → 0.13 and `base64` 0.22 → 0.23.
  All 19 tracked lockfile roots were re-verified against the latest
  semver-compatible updates.
- Worker image tool pins: opencode `v1.18.23` → `v1.18.25`, kimi-code `0.39.0`
  → `0.39.1`, synchronized across the Dockerfiles, `.env.example`, CI
  variables, docs, and the mt-admin harness assertion.

### Multi-Tenant Operations Fixes

- mt-admin `clone_tenant_repo` now tolerates a shallow `SOURCE_REPO`: it probes
  `git rev-parse --is-shallow-repository` and uses a depth-bounded transfer
  instead of failing with a swallowed checkout error.
- `install-wasm` no longer installs the DarkIRC WASM channel on tenants with
  `enable_darkirc=false` (previously it polled a nonexistent adapter and
  flooded logs with `[health] Adapter poll failed`), and removes stale
  darkirc artifacts; `--help` advertises the behavior.

## Upgrade Notes

1. Back up PostgreSQL, tenant env files, `/etc/lunarwing/ports.json`,
   workspace state, rendered units, worker data, voice models, and channel
   state before an upgrade.
2. No SQL migration or port-registry migration is introduced. `V26` and port
   registry v16 remain the current formats.
3. Rebuild the daemon, WASM channels, and selected worker images through the
   normal deployment path. The multica (0.1.9) and xmpp (0.1.12) channel
   registry versions changed; darkirc and weechat are rebuild-only updates.
4. All cargo operations under `ic/` now resolve from `ic/vendor/`. If you add
   or update a dependency, run `ic/scripts/refresh-vendor.sh`, review the
   `vendor/` diff, and commit it with the manifest/lockfile change. Fresh
   clones need no extra step.
5. `/interrupt` and `/steer` work on the web gateway, WeeChat, XMPP, and the
   REPL. On gateway channels routed to Engine V2 (the tenant default), steer
   is delivered through the bridge's scoped engine thread. Control commands
   stop or steer **running** turns; a queued-but-not-yet-running prompt still
   answers "Nothing to interrupt.".
6. LunarVoice lifecycle is driven through mt-admin
   (`voice install/configure/activate/deactivate/remove`). Existing manually
   deployed voice sidecars should be reconciled to the rendered units so
   health probes and supervision agree with the registry.
7. Health-metric thresholds are now tunable via `HEALTH_<CHECK>_*` environment
   variables (see `ic-infrastructure-health-check/config.sh.example`). Existing
   installs keep their previous hardcoded behavior by default; set the knobs
   to adopt early-warning signals.
8. Coordination-knowledge publishers on live tenants: set
   `XMPP_ALLOW_ROOMS_JSON` to the room(s) to join and set
   `COORD_KNOWLEDGE_PUBLISHERS` to full occupant JIDs (or the room bare JID as
   a room-wide opt-in), then restart the engine. An empty room allowlist drops
   all groupchat.
9. Worker pins moved to opencode `v1.18.25` and kimi-code `0.39.1`; rebuild
   worker images through the normal CI path to pick them up.

## Known Issues

This draft combines the active canonical bug tracker (re-verified 2026-08-25,
[`docs/bugs/README.md`](docs/bugs/README.md)), limitations carried from
v2.0.9.0, and gaps documented during the 2.1 cycle. Item 22 of the release
checklist will reconcile this list against the work-item tracker before final
release.

### Active Runtime and Integration Follow-ups

- **Synchronous worker calls still occupy their originating turn.** A
  built-in Docker `create_job` call with the default `wait=true` holds its
  turn until the worker result, timeout, or an interrupt; mid-turn
  interrupt/steer (this release) gives operators a control path while a job
  is in flight. External workers default to `wait=false`. See
  [`BUG-agent-worker-lifecycle.md`](docs/bugs/BUG-agent-worker-lifecycle.md).
- **Some E2E coverage remains incomplete.** Bootstrap E2E scenarios are
  `#[ignore]`'d (harness race) and clipboard runtime behavior is unverified.
  See [`BUG-e2e-test-bugs.md`](docs/bugs/BUG-e2e-test-bugs.md).
- **External-worker token persistence lacks a final live re-check.** See
  [`BUG-external-worker-config-persistence.md`](docs/bugs/BUG-external-worker-config-persistence.md).
- **Kawarimi flag parity is incomplete.** Import still rejects an explicit
  `--with-wasm`. See
  [`BUG-kawarimi-import-flag-parity.md`](docs/bugs/BUG-kawarimi-import-flag-parity.md).
- **SSH Git can accept an empty checkout after a bare-remote HEAD mismatch.**
  See [`BUG-ssh-git-ref-and-remote-head.md`](docs/bugs/BUG-ssh-git-ref-and-remote-head.md).
- **WeeChat's `rand_check` is still a stub.** See
  [`BUG-weechat-relay-rand-check.md`](docs/bugs/BUG-weechat-relay-rand-check.md).
- **Worker workspace path expansion is partial.** Structured OpenCode project
  paths expand `~`; prompt-generated paths and Pebble `project_dir` remain.
  See
  [`BUG-worker-workspace-path-expansion.md`](docs/bugs/BUG-worker-workspace-path-expansion.md).
- **XMPP polling backpressure residual.** Poll-loop supervision/health is
  implemented; an unbounded send await on a full downstream queue remains,
  visible to the stall detector. See
  [`BUG-xmpp-polling-and-backpressure.md`](docs/bugs/BUG-xmpp-polling-and-backpressure.md).
- **OMEMO warmup/processing unverified.** Historical MUC fallback-spam lacks a
  current live reproduction. See
  [`BUG-xmpp-omemo-warmup-and-processing.md`](docs/bugs/BUG-xmpp-omemo-warmup-and-processing.md).

### Newly Documented in 2.1

- **Interrupted engine turns render as `Failed` in gateway v1 turns-history.**
  `build_turns_from_db_messages` lacks an Interrupted state for turns with no
  assistant response (pre-existing mapping gap surfaced by mid-turn
  interrupt). See
  [`docs/ops/v2.1.0.0_GOAL9_STEER_INTERRUPT_LIVE.md`](docs/ops/v2.1.0.0_GOAL9_STEER_INTERRUPT_LIVE.md).
- **WI#97 operator actions are out-of-repo.** The code fallback ships, but
  live sunburst/ruffles tenants still need the room allowlist and publisher
  env changes plus restarts (Upgrade Note 8).
- **`tools-src/gitlab` lockfile remains stale** and is excluded from the WASM
  tool build loop and from vendoring; the tool registry stays at 0.1.3.
- **`weechat.capabilities.json` `dm_policy` is not a supported override**:
  `install-wasm` overwrites test-time policy edits; set policy through the
  tenant env surface instead.

### Security, Deployment, and Release Limitations (carried)

- DarkIRC Secure Key Exchange Stages B–D and automated QR/web/trusted-channel
  key delivery remain disabled and separately gated behind Stage A acceptance.
- MCP does not automatically install npm/pip packages, has no signed-package
  marketplace CLI, and does not deliver configured MCP servers to
  `lunarwing-agent-v1` external workers. Custom HTTP MCP header values are
  still stored directly rather than through owner-scoped secret references.
- The web UI still puts its bearer token in native `EventSource` query
  strings. Protect browser history and access/proxy logs until short-lived
  SSE tickets or cookie-scoped sessions replace that transport.
- Onboarding remains a trusted local console without user accounts or TLS.
  Keep it loopback-bound with token authentication and use an SSH tunnel for
  remote access.
- OpenRC hosts without unified cgroup v2 delegation fall back to nice-only
  build bounding rather than hard memory, CPU, and task ceilings. mt-admin
  reports the fallback reason.
- `ALLOW_PRIVATE_IPS=1` weakens public-address SSRF protections. Enable it
  only for intentionally trusted private-network deployments (the harness and
  service path seed it for private Postgres/TensorZero test setups).
- `cargo check --tests --no-default-features --features postgres` retains
  pre-existing cfg(test) failures; CI does not run that exact shape.
- Kawarimi remains PostgreSQL-only.
- GitLab CI verifies source but does not publish release packages or deploy
  tenants automatically.
- Large worker-image pushes can still fail with registry
  `499 Client Closed Request` until proxy-chain read timeouts are raised on
  the GitLab host. See [`docs/ops/REGISTRY_PUSH_499.md`](docs/ops/REGISTRY_PUSH_499.md).
- Vendoring gates which crates enter the tree: new dependencies require an
  explicit `refresh-vendor.sh` run plus diff review (this is intended
  behavior, but it is a workflow change for contributors).
- LunarVision external (non-local GPU) instance support was attempted during
  the cycle but is **not** part of this release; it remains an open checklist
  item.

### Resolved Since v2.0.9.0

- Mid-turn interrupt now aborts in-flight LLM streams and tool batches instead
  of waiting for turn boundaries, and `/steer` injects operator guidance into
  a running turn without breaking the tool-calling loop — verified live on
  gateway, WeeChat, and XMPP.
- Engine V2-routed gateway channels no longer silently drop steer notes
  (bridge-owned scoped delivery).
- LunarVoice is feature-complete with a full mt-admin lifecycle,
  readiness-gated health on both init families, and complete docs.
- The models healthpoint emits exportable metrics, and all legacy
  healthpoints support env-configurable degraded/critical thresholds.
- The crates.io closure is committed under `ic/vendor/`; `cargo check` passes
  with no network access, and advisory scanning still works offline-first.
- LLM `finish_reason` survives blocking completions, stream aggregation, and
  the orchestrator proxy in one canonical spelling (#95).
- Evaluation top-error ranking is deterministic for ties (#96).
- Coordination-knowledge shares can be authorized per-room via the room bare
  JID; groupchat allowlist matching actually works against MUC occupant
  senders (#97).
- Passive-voice fabricated tool-success claims are detected and rejected.
- The onboarding console no longer shows a spurious token warning in
  `--no-token` mode, and its flows are covered by a real browser suite.
- mt-admin no longer half-clones from a shallow source repository, and
  `install-wasm` no longer installs the DarkIRC channel where it is disabled.
