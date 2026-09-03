# Release Notes for LunarWing v2.1.0.0

## Codename: Ma-ai (間合い)

**Status: PRE-RELEASE ** Release date TBD
Claims below were rechecked against release-integration revision
`a67fee21c` (the `aragents/kimil00p/v2.1.0.0` branch reviewed at that commit),
the `v2.0.9.0..a67fee21c` history, current source/manifests/locks, in-tree
regression evidence, the canonical bug reports, and the public work-item
tracker (2026-09-01 accuracy pass, a 2026-09-02 re-verification at `96c951785`,
and a 2026-09-03 re-verification at `a67fee21c`). This file remains at the
repository root until the release process moves it into `docs/releases/`.

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

LunarWing v2.1.0.0 is an agent-interaction control, voice/vision lifecycle,
gateway presentation, supply-chain hardening, and release-engineering release.
Its headline feature gives operators real-time control over a running agent
turn: a mid-turn interrupt aborts an in-flight LLM stream or tool batch — not
only at turn boundaries — and Hermes-style steer injects guidance at the next
safe execution boundary without breaking message-role ordering.

LunarVoice (the ONNX speech-to-text sidecar) gains a complete mt-admin lifecycle,
registry-aware health integration, and a documented production configuration
surface. LunarVision can now use an operator-managed external HTTPS instance,
including automatic XMPP image enrichment and the `vision-analyze` WASM tool.
For Rust builds rooted under `ic/`, the approved crates.io/git dependency closure
is committed under `ic/vendor/` and source-replaces crates.io; root-level
satellite projects outside `ic/` are intentionally outside that replacement.

Correctness and operability work includes exportable Prometheus metrics from
every healthpoint with env-configurable early-warning thresholds; end-to-end
LLM `finish_reason` propagation; passive-voice fabricated-result detection;
deterministic evaluation error ranking; room-bare-JID coordination-knowledge
publisher authorization; durable SSH AcceptFirst pins; a mission-scheduler
startup warmup; a blocking ShellCheck CI gate; and executable browser coverage
for the onboarding web GUI. The web gateway also gains an emoting, draggable
pony/unicorn mascot driven by action phrases in assistant responses.

The release identifier is `2.1.0.0`; LunarWing-owned Rust crates and channel
packages use `2.1.0`. Rust 2024 and MSRV `1.96.1` are unchanged. There are no
new SQL migrations (`V26__study_notes_knowledge.sql` remains latest) and no new
multi-tenant port-registry migration (v16 remains current).

See the [v2.0.9.0 release notes](docs/releases/RELEASE-v2.0.9.0.md) for the
previous compatibility baseline.

## Highlights

- **Mid-turn interrupt.** Each classic chat turn now carries a per-thread
  `CancellationToken` from `agent::interrupt::TurnInterrupts`. Every LLM call
  and tool batch in that agentic loop races against the token via
  `tokio::select!`, so `/interrupt` cancels an in-flight provider stream or
  running tool batch mid-poll instead of waiting for a step boundary. Engine V2
  runs apply their per-run token to in-flight CodeAct, direct, and parallel
  tool host calls as well, so interrupting drops a long-running tool future
  promptly. Existing turn-boundary state and queue handling remain in place.
- **Hermes-style steer.** `/steer <note>` uses priority dispatch and a capped
  four-note buffer on the classic loop, draining notes into one user message at
  the next safe iteration boundary. Steer is accepted while a conversation is
  idle, interrupted, processing, or approval-paused, and queued notes are never
  silently dropped: the engine persists a bounded four-note durable queue that
  clears with the conversation, and pending notes are delivered on the next
  foreground turn and on running-thread, suspended, and approval/auth resumes.
  Engine V2 routes steer through the bridge into the scoped running thread when
  one exists and acknowledges the queued note otherwise. Live verification on a
  throwaway tenant passed 6/6 checks (web gateway, WeeChat, and XMPP ×
  interrupt and steer). Evidence:
  [`docs/ops/v2.1.0.0_GOAL9_STEER_INTERRUPT_LIVE.md`](docs/ops/v2.1.0.0_GOAL9_STEER_INTERRUPT_LIVE.md).
- **LunarVoice production lifecycle.** mt-admin now provides
  install/configure/activate/deactivate/remove for the voice sidecar with
  transactional reconfiguration, model verification, per-tenant ports, systemd
  user Quadlet (and older-Podman fallback), and OpenRC supervision. Health probes
  `/health`, `/ready`, and `/metrics` through registry-discovered tenant ports;
  language normalization and validation are hardened; production runbooks are
  in-tree.
- **External LunarVision instances.** A tenant can switch between its managed
  local sidecar and an operator-managed external HTTPS origin. Remote HTTP, URL
  credentials, paths, queries, fragments, redirects, and cross-origin bearer
  forwarding are rejected; WASM egress is scoped to POST under the configured
  origin's `/v1/` path.
- **Emoting web mascot.** The gateway classifies italic action phrases into
  eight canonical emotes, attaches an optional `emotes` array to final response
  events, and renders a draggable, hideable sprite widget with idle, working,
  greeting, error, and FIFO emote states.
- **Durable SSH AcceptFirst pins.** Accepted host keys are stored as a versioned,
  tenant-scoped encrypted pin set before a first handshake succeeds. Restart,
  tenant-isolation, corruption, revocation, and concurrent-first-key paths are
  covered; SSH bridge creation fails closed when pin persistence cannot load.
- **Every healthpoint exports metrics.** The models check now emits numeric
  provider counts and latency/status maps, and all six legacy checks expose
  degraded/critical thresholds through `HEALTH_<CHECK>_*` knobs. The exporter is
  host-global and carries per-tenant labels where the source healthpoint is
  tenant-scoped; `health_metrics` remains the v16 registry slot.
- **The `ic/` dependency supply chain is vendored.** `ic/.cargo/config.toml`
  source-replaces crates.io with `ic/vendor/`; locked `ic/` builds pass with
  `CARGO_NET_OFFLINE=true`. `ic/scripts/refresh-vendor.sh` re-vendors the
  workspace plus 18 tracked satellite lockfile roots, checks the resulting set,
  and runs `cargo deny check advisories`.
- **LLM `finish_reason` is no longer dropped.** Engine V2 `LlmOutput`, blocking
  bridge adapters, stream aggregation, and the worker proxy carry canonical
  `stop`/`length`/`tool_calls`/`content_filter`/`unknown` strings. The parser also
  accepts legacy `tool_use` for rolling-upgrade interop.
- **Smarter fabricated-result detection.** The orchestrator recognizes five
  passive completion claims (for example, "was written" and "was retrieved")
  plus `memory write`/`memory/` tool mentions. When the current turn has no real
  action result, the bounded guard challenges the claim and asks the model to
  call the tool instead of treating the prose as proof.
- **Deterministic evaluation error ranking.** `MetricsCollector::summary()`
  sorts error types by descending frequency with an ascending-name tie-breaker,
  so the top-three cutoff is stable for equal counts.
- **Room-wide coordination-knowledge publishers.** The KNOW-share allowlist now
  matches either the exact XMPP occupant JID or the room bare JID as an explicit
  room-wide opt-in. Exact occupant JIDs already worked; this release adds the
  previously missing room-level fallback. User bare JIDs still do not match MUC
  occupant senders.
- **Mission startup race mitigation.** The mission cron ticker now waits a
  cancellation-aware 30 seconds before its first due-mission check, avoiding the
  former immediately-ready tick while the provider watchdog starts. This is a
  fixed warmup delay, not a direct provider-readiness handshake.
- **Onboarding web GUI covered by browser tests.** A headless-Chromium suite
  exercises picker/settings, all five operations (provision, secrets, upgrade,
  export, import), token handling, and console/network failures. It exposed and
  fixed a `--no-token` UX bug that showed a spurious missing-token warning.
- **ShellCheck is a release gate.** The repository warning baseline is zero and
  the CI `shellcheck` job now runs as a blocking `validate`-stage job on every
  pipeline rather than an allowed-failure observation.

## Compatibility

| Area | v2.1.0.0 behavior |
|---|---|
| Release version | `2.1.0.0` |
| Crate/package version | `2.1.0` for LunarWing-owned release packages |
| Rust edition / MSRV | Rust 2024 / `1.96.1` |
| New SQL migration | None; `V26__study_notes_knowledge.sql` remains latest |
| Multi-tenant port registry | v16; no schema change from v2.0.9.0 |
| Channel registry patches | darkirc 0.1.6 (no change), multica 0.1.9, weechat 0.2.7 (no change), xmpp 0.1.12 |
| Tool registry patches | `vision-analyze` 0.1.2 (was 0.1.1); GitLab 0.1.4 (was 0.1.3); other tools unchanged |
| WASM runtime | Wasmtime / wasmtime-wasi 36.0.14 (was 36.0.13) |
| Dependency resolution | `ic/vendor/` source-replaces crates.io for the `ic/` workspace and satellite manifests rooted under `ic/`; root-level satellites are out of scope |
| Worker image tool pins | opencode `v1.18.25`, kimi-code `0.39.1` |
| Worker image moving tag | CI still defaults `WORKER_IMAGE_TAG` to `v2.0.9.0`; override it for a v2.1.0.0 image build |
| CI scope | Source verification; no package publication or tenant deployment |

## Changes

### Mid-Turn Interrupt and Steer (Hermes-style turn control)

Item 3 of the v2.1.0.0 cycle, with live verification and fixes from item 9:

- `agent::interrupt::TurnInterrupts` gives each classic-loop chat turn a
  `CancellationToken`; registration is per thread and RAII-guarded.
  `AgenticLoopConfig.interrupt_token` races every LLM call and tool batch through
  `tokio::select!`. Cancellation drops the in-flight future and returns
  `LoopOutcome::Stopped` through the existing interrupted-turn path.
- `Thread::interrupt()` and `check_signals` preserve the turn-boundary path when
  no token is live (for example, while awaiting approval), while a boundary
  token check catches cancellation between steps. Job/container consumers of
  the shared loop deliberately retain boundary-only behavior with no token.
- `Thread.steer_notes` has a four-note cap, newline-merged drain, serde default,
  and interrupt clearing. Exact `/steer <note>` submissions use priority
  dispatch and are accepted only while a turn is `Processing`. The classic
  delegate injects the merged note as a user message at the next iteration
  boundary; when a tool batch is running, this occurs after its result messages,
  preserving role alternation and the tool-calling loop. The REPL exposes
  `/steer` in completion and help.
- Live verification on throwaway tenant `dragon` (OpenRC) with throwaway Prosody
  and miniircd servers passed all six required checks: gateway, WeeChat, and
  XMPP × interrupt and steer. Steered turns changed topic mid-turn and
  interrupted turns terminated early. Idle controls retained their established
  responses.
- That live run found an Engine V2 gap: routed `/steer` previously fell through
  to an idle legacy session and was silently lost. `bridge::handle_steer` now
  injects the wrapped note into the scoped running engine thread and returns the
  same acknowledgement as the classic path.

### External LunarVision Instances

Tenants can now use an operator-managed LunarVision instance on another machine
instead of requiring a local GPU sidecar:

- `configure-vision-external` reads a one-line token file, validates and
  health-gates an HTTPS origin, retires managed local systemd/OpenRC assets,
  persists non-secret mode metadata plus the protected daemon environment, and
  rolls back failed transitions. `configure-vision-local` restores managed
  local mode.
- Automatic XMPP enrichment, the `vision-analyze` WASM tool, and host health
  discovery use the configured origin. Plaintext remote HTTP, URL credentials,
  non-root paths, queries, fragments, and redirects are rejected.
- WASM egress is narrowed to POST requests under the configured origin's
  `/v1/` prefix. Bearer credentials are host-owned and constrained to that same
  origin and path; invalid configuration clears the tool's HTTP allowlist.
- Ports-registry discovery and docs cover mixed local/external fleets on both
  systemd and OpenRC hosts. The `vision-analyze` tool registry version is 0.1.2.

### Emoting Gateway Mascot

The gateway now includes a presentation-only mascot pipeline:

- `channels::web::emote` extracts bounded italic action phrases, maps
  case-insensitive keywords to eight `Emote` variants, deduplicates in order,
  and caps each response at four emotes. The requested examples are pinned by
  tests: `*stamps hooves*` maps to `jump`, and `*ears perk*` maps to `alert`.
- Final assistant response events from direct gateway responses, broadcasts,
  and bridged `AppEvent` conversions carry `emotes` only when non-empty. Parsing
  never changes assistant text.
- Eight allowlisted GIFs are embedded under `/mascot/`. The browser widget is
  draggable, position-persistent, hideable/restorable, and uses idle/working
  fallbacks plus greeting, error, and bounded FIFO emote playback.

### LunarVoice Production Completion

The ONNX speech-to-text sidecar (introduced in 2.0.6.0 and health-hardened in
2.0.7.0) now has a production lifecycle:

- mt-admin exposes `install-voice`, `configure-voice`, `activate-voice`,
  `deactivate-voice`, and `remove-voice`, with an atomic reconfiguration lock,
  rollback, model verification, and per-tenant port allocation.
- Service management covers systemd user Quadlet with a managed older-Podman
  fallback and OpenRC with a crash supervisor. Startup is gated on STT
  readiness.
- `/health`, `/ready`, and `/metrics` probes discover each enabled tenant's
  published voice-health port and fail closed on invalid, missing, or duplicate
  mappings.
- Whisper `auto` language handling is normalized and validated across requests,
  direct engine configuration, and the mt-admin CLI against the canonical
  Whisper code set. No-ONNX builds remain warning-free; synthesis bodies and
  compressed audio are bounded.
- The complete environment template, lifecycle design, standalone deployment
  runbook, setup guide, Fedora/Debian/Arch/Gentoo runbooks, and health readiness
  contract are documented.

### Per-Healthpoint Metrics Exporters

Completing the v2.0.8.0 exporter design
([`docs/proposals/HEALTHPOINT_METRICS_EXPORTER.md`](docs/proposals/HEALTHPOINT_METRICS_EXPORTER.md)):

- `health-models.sh` emits numeric provider counts, `latency_max_ms`, and
  per-provider `provider_latency_ms` / `provider_status` maps, so the Prometheus
  textfile exporter no longer drops the models healthpoint.
- Degraded/critical thresholds for gateway, xmpp, ratelimit, clickhouse,
  tensorzero, and models are configurable through validated
  `HEALTH_<CHECK>_*` environment knobs.
- The exporter remains one host-global output/listener with tenant labels for
  tenant-scoped healthpoints; it is not one listener per tenant. Tests cover the
  new model series and per-metric threshold crossings, and the examples include
  one early-warning rule per healthpoint.

### Fabricated Tool-Success Detection (F4 hardening)

The passive-voice false-claim detector was re-ported against current release
code without applying the stale development stash:

- `signals_claimed_tool_success()` recognizes five passive completion phrases
  ("was written", "was saved", "was stored", "was persisted", "was
  retrieved") and both requested tool-mention forms (`memory write`,
  `memory/`).
- The guard acts only when actions are available and the current user turn has
  neither a tool call nor a structured action result. It issues a bounded
  corrective prompt asking the model to execute the tool; after the configured
  nudge cap, ordinary text completion remains possible.
- Positive tests cover every new phrase; negative tests cover passive prose
  without a tool mention and tool mentions without a completion claim.

### LLM `finish_reason` End-to-End (work item #95)

Provider finish reasons now survive every Engine V2 adapter surface:

- Engine: `LlmOutput.finish_reason` carries a canonical string; the
  blocking-to-stream fallback emits the declared reason, and
  `collect_llm_stream` captures the terminal Done reason.
- Bridge: `map_completion_response` and `map_tool_response` map
  `FinishReason::as_str()` into `LlmOutput.finish_reason`; tests pin `length`
  and `tool_calls` pass-through.
- Proxy: serialization delegates to `FinishReason::as_str()` and worker parsing
  delegates to `FinishReason::from_stream_reason()`, which accepts legacy
  `tool_use` as a rolling-upgrade alias.

### Deterministic Evaluation Error Ranking (work item #96)

`MetricsCollector::summary()` sorts error types by descending frequency before
taking three, with an ascending error-name tie-breaker. A regression test seeds
five categories and verifies both frequency ordering and deterministic cutoff.

### Mission-Scheduler Startup Warmup (work item #94)

The cron ticker used to use Tokio's immediately-ready first interval tick while
the provider watchdog started concurrently. It now waits a cancellation-aware
30 seconds before creating the 60-second interval; a paused-time regression test
proves a due mission cannot fire at 29 seconds and does fire at 30 seconds. This
mitigates the startup race with a fixed delay; it does not observe watchdog
health directly.

### Coordination-Knowledge Publisher Allowlist (work item #97)

XMPP groupchat `sender_id` is the full occupant JID (`room@server/nick`). Exact
full-occupant entries already matched; room bare JIDs and user bare JIDs did not.

- `is_allowed_publisher()` now matches the exact occupant JID or strips the nick
  and matches the room bare JID as an explicit room-wide opt-in. User bare JIDs
  still never match MUC occupants; exact entries remain nick-pinned.
- Unit coverage pins full-occupant, room-bare, user-bare, and no-nick behavior.
  An integration test accepts a KNOW share through a room-bare entry while
  ignoring an occupant from an unlisted room.
- Docs spell out both supported forms and warn that an empty
  `XMPP_ALLOW_ROOMS_JSON` joins no rooms and drops all groupchat.

### Durable SSH AcceptFirst Pins (work item #98)

The SSH bridge no longer forgets trust-on-first-use host keys on restart:

- `HostKeyVerifier::new_persistent` loads a versioned, tenant-scoped pin set from
  the AES-256-GCM secrets store. A first key is persisted before it is published
  in memory or accepted; malformed records and load/write failures prevent SSH
  bridge initialization instead of silently constructing an empty verifier.
- Concurrent first connections serialize and recheck under the verifier lock,
  so different keys cannot both win. Removing a host also revokes its pin in
  persistent storage.
- Built-in and WASM SSH retain the server key algorithm in the shared pin.
  `ssh_git` learns an unpinned AcceptFirst key through a russh key-exchange-only
  preflight, then invokes OpenSSH with strict checking against that materialized
  key. Explicit `known_host_key` remains authoritative; `Strict` remains the
  default and recommended mode.

### Onboarding Web GUI Browser Testing

The multi-tenant onboarding web console gains executable browser coverage:

- `lunarwing_mt_onboard_web/browser_tests.py` (optional Playwright dependency)
  covers picker/assets/settings; client validation; REST and WebSocket success
  for provision, secrets, upgrade, export, and import; failure/result rendering;
  generated credentials; token rejection/acceptance; `sessionStorage` reuse;
  and unexpected console/network errors.
- In `--no-token` mode the UI previously showed "Session token missing or
  invalid" merely because the URL lacked a token. It now shows that warning only
  after the backend returns `401`.

### Dependency Supply Chain Vendoring

The approved dependency closure for Rust manifests rooted under `ic/` is now
committed:

- `cargo vendor` covers the `ic/` workspace plus 18 tracked satellite lockfile
  roots under `ic/`, including every registered WASM tool. The tree has 1,050
  directories total over 843 unique package names, including the two
  pre-vendored `libsignal-protocol{,-sys}` path dependencies. Root-level
  satellites outside `ic/` are intentionally not source-replaced.
- `ic/.cargo/config.toml` source-replaces crates.io with `ic/vendor/`. Locked
  workspace checks and metadata pass with `CARGO_NET_OFFLINE=true`.
- `ic/scripts/refresh-vendor.sh` uses `--locked --no-delete`, regenerates missing
  libsignal checksums, checks the directory set against all 19 tracked
  lockfiles without re-resolving, and then runs `cargo deny check advisories`.
- Repo-wide secret, shell-syntax, ShellCheck, and Python-syntax scanners exclude
  the pinned third-party tree. Advisory matching still evaluates Cargo.lock
  against the separate RustSec advisory database.

### Dependency, Version, and Worker Updates

- LunarWing-owned release crates and channel packages moved from 2.0.9 to 2.1.0.
  Channel registry patches are multica 0.1.9 and xmpp 0.1.12; darkirc 0.1.6 and
  weechat 0.2.7 are unchanged. Tool registry bumps are `vision-analyze`
  0.1.1 → 0.1.2 and GitLab 0.1.3 → 0.1.4.
- fs4 0.6.6 was removed in favor of standard-library `File::lock` /
  `try_lock` (available before the 1.96.1 MSRV), preserving
  `TryLockError::WouldBlock` behavior across bootstrap, gate persistence,
  pairing, DarkIRC key management, and MCP config.
- The `html-to-markdown-rs` manifest constraint moved from 2.3 to 3, changing
  the locked package from 2.30.0 to 3.11.6. The OCR sidecar moved direct
  `reqwest` 0.12 → 0.13 and `base64` 0.22 → 0.23.
- Wasmtime and wasmtime-wasi moved 36.0.13 → 36.0.14 for
  RUSTSEC-2026-0222/RUSTSEC-2026-0269; the corresponding Cranelift and
  capability crates moved with that patch release. The release-cycle audit
  recorded no remaining semver-compatible updates across 19 tracked lockfile
  roots before the vendor snapshot.
- Worker tool pins moved opencode `v1.18.23` → `v1.18.25` and kimi-code `0.39.0`
  → `0.39.1`. Their respective Dockerfile defaults, examples/docs, the Kimi CI
  variable, and the Kimi mt-admin harness assertion are synchronized. The
  container job's moving image tag is a separate setting and remains
  `v2.0.9.0` by default.

### Runtime and Release-Engineering Hardening

- Engine V2 now marks a provider failure as recoverable only if the failed
  stream emitted no text. Once a partial stream reached clients, the error is
  non-recoverable, preventing an unsafe automatic retry of already-emitted
  output.
- The mission cron warmup described above closes the immediately-ready startup
  tick without delaying channel readiness.
- The repository ShellCheck warning baseline was cleared, and the CI job moved
  from an allowed-failure observation to a blocking `validate` gate on every
  pipeline. A vision-harness emulation that ShellCheck 0.9 (CI Bookworm)
  reported as SC2218 carries a targeted in-source suppression, so the blocking
  gate passes under both ShellCheck 0.9 (CI) and 0.11.
- `docs/architecture/ENGINE-V2.md` now records that one 300-second default
  `tool_async_timeout_secs` deadline intentionally bounds both deferred tool and
  deferred LLM joins in a `ResolveFutures` round. Separate per-surface knobs and
  retry/re-arm behavior were considered and rejected; this is documentation of
  the shipped policy, not a new runtime knob.

### Multi-Tenant Operations Fixes

- `clone_tenant_repo` detects a shallow `SOURCE_REPO` and uses depth-bounded
  (`--depth=10000`) transfers, avoiding the prior half-clone after Git rejected
  an incomplete object negotiation.
- `install-wasm` skips and removes the DarkIRC WASM channel when
  `enable_darkirc=false`, preventing polls against a nonexistent adapter and the
  resulting health-log noise. Its help text documents the behavior.

## Upgrade Notes

1. Back up PostgreSQL, tenant environment files, `/etc/lunarwing/ports.json`,
   workspace state, rendered units, worker data, voice models, and channel state.
2. No SQL or port-registry migration is introduced. `V26` and registry v16
   remain current.
3. Rebuild the daemon and installed WASM extensions through the normal deployment
   path. Channel registry versions multica 0.1.9 and xmpp 0.1.12 changed;
   darkirc 0.1.6 and weechat 0.2.7 did not. Rebuild/reinstall
   `vision-analyze` for its tool-registry bump to 0.1.2.
4. Cargo manifests rooted under `ic/` now resolve crates.io through
   `ic/vendor/`. For a dependency change, run `ic/scripts/refresh-vendor.sh`,
   review the vendor diff, and commit it with the manifests/locks. Root-level
   satellite projects remain outside this source replacement.
5. `/interrupt` and `/steer` are available through the web gateway, WeeChat,
   XMPP, and REPL. Engine V2-routed controls target the scoped running engine
   thread. A control that races ahead of a queued-but-not-running turn can still
   return "Nothing to interrupt." or the corresponding idle steer response.
6. Manage LunarVoice through `install-voice`, `configure-voice`,
   `activate-voice`, `deactivate-voice`, and `remove-voice`. Reconcile any
   manually deployed sidecar to mt-admin's rendered units and registry ports.
7. To use external LunarVision, run `configure-vision-external <name> --url
   <https-origin> --token-file <path>`; the health endpoint must be reachable
   before the switch. `configure-vision-local <name>` restores the managed local
   sidecar. Existing local tenants remain local unless explicitly changed.
8. Health thresholds are tunable through the documented `HEALTH_<CHECK>_*`
   variables in `ic-infrastructure-health-check/config.sh.example`. Defaults
   retain the previous thresholds.
9. Coordination-knowledge tenants must both join the intended MUC via
   `XMPP_ALLOW_ROOMS_JSON` and authorize either exact occupant JIDs or the room
   bare JID in `COORD_KNOWLEDGE_PUBLISHERS`, then restart and verify the wire
   path. An empty room allowlist drops all groupchat.
10. Worker tool pins are opencode `v1.18.25` and kimi-code `0.39.1`. The CI
    container template still defaults the moving image tag to `v2.0.9.0`, so
    set `WORKER_IMAGE_TAG=v2.1.0.0` when producing release-tagged images.
11. Earlier releases have no durable AcceptFirst pin to migrate. The first
    successful post-upgrade connection stores the encrypted pin used for later
    calls and restarts. If that initial path is not trustworthy, configure
    `known_host_key` from a trusted source and use `host_key_mode = "Strict"`
    before connecting.

## Known Issues

The accuracy pass reconciled two separate tracking surfaces against revision
`96c951785`: the nine active canonical reports in
[`docs/bugs/README.md`](docs/bugs/README.md), and the public GitLab work-item
state re-queried on 2026-09-02 (unchanged from the 2026-09-01 pass). GitLab has
six open items (#86, #87, #94, #95, #97, #98); #96 is closed. An open tracker
state is not treated as proof that its source fix is absent, so the source
disposition is stated explicitly below.

### Public Work-Item Tracker Reconciliation

- **#86 — legacy external-worker subprotocol is still present (open).** The
  daemon and Pebble/OpenCode/Kimi workers still offer or accept
  `ironclaw-agent-v1` alongside `lunarwing-agent-v1`. This is deliberate
  compatibility in current source, but it does not satisfy the work item's
  request to remove the old protocol name.
- **#87 — LunarVision testing item is not actionable (open).** Its public title
  and `lv` description provide no failure, scope, or acceptance criterion. No
  specific release defect can be inferred; the item needs clarification before
  it can be verified or closed.
- **#94 — mission warmup race is mitigated, not health-synchronized (open).**
  Source now delays the first cron check by 30 seconds and tests that boundary,
  but it does not wait for an explicit watchdog-ready signal. Providers taking
  longer than the fixed delay can still overlap the first mission check.
- **#95 — `finish_reason` source fix ships, tracker remains open.** Blocking,
  streaming, bridge, and proxy paths now carry the canonical value and have
  regression coverage. The remaining action is tracker closure, not a known
  source gap found in this pass.
- **#97 — source fallback ships; live operator work remains (open).** Room-bare
  publisher matching is implemented, but the sunburst/ruffles deployment still
  needs both room-join allowlists, publisher configuration, restarts, and a live
  KNOW-path retest.
- **#98 — durable AcceptFirst source fix ships, tracker remains open.** The
  encrypted persistent-pin implementation and its restart/concurrency/failure
  tests are present. The remaining action is tracker closure.
- **#96 — deterministic top-error ranking is implemented and closed.** Its
  source and tracker dispositions agree.

### Active Canonical Runtime and Integration Follow-ups

- **Synchronous worker calls still occupy their originating scheduling lane.**
  Built-in Docker `create_job` defaults to `wait=true` and holds that lane until
  result, timeout, or interrupt; external workers default to `wait=false`.
  Independent scoped Engine V2 gateway threads can proceed, and control messages
  are still ingested. See
  [`BUG-agent-worker-lifecycle.md`](docs/bugs/BUG-agent-worker-lifecycle.md).
- **Some E2E coverage remains incomplete.** Two bootstrap greeting scenarios
  remain ignored because of a harness-readiness race, and the synthetic
  clipboard test still lacks a current supported-CI browser verification. See
  [`BUG-e2e-test-bugs.md`](docs/bugs/BUG-e2e-test-bugs.md).
- **External-worker token persistence lacks its final live re-check.** Source
  and round-trip tests preserve worker tokens, but a daemon restart followed by
  a real authenticated worker request has not closed the report. See
  [`BUG-external-worker-config-persistence.md`](docs/bugs/BUG-external-worker-config-persistence.md).
- **Kawarimi import flag parity is incomplete.** `import-tenant.sh` always adds
  `--with-wasm` to its build internally but rejects an operator-supplied
  `--with-wasm`. See
  [`BUG-kawarimi-import-flag-parity.md`](docs/bugs/BUG-kawarimi-import-flag-parity.md).
- **SSH Git can report success for an empty checkout.** A bare remote whose HEAD
  points to a missing branch makes Git exit zero with a warning; `ssh_git`
  returns that stderr but does not detect the unborn checkout. See
  [`BUG-ssh-git-ref-and-remote-head.md`](docs/bugs/BUG-ssh-git-ref-and-remote-head.md).
- **WeeChat `rand_check` is a dead latent stub.** It ignores its probability and
  always returns false; no current call site exists. See
  [`BUG-weechat-relay-rand-check.md`](docs/bugs/BUG-weechat-relay-rand-check.md).
- **Worker workspace path expansion is partial.** Structured OpenCode
  `project_dir` values normalize `~`; prompt-generated paths and Pebble's
  `project_dir` do not. See
  [`BUG-worker-workspace-path-expansion.md`](docs/bugs/BUG-worker-workspace-path-expansion.md).
- **XMPP polling retains a bounded-queue backpressure risk.** Poll supervision
  and stall health exist, but `reserve_many` can await indefinitely when the
  downstream queue is full. The stale poll epoch makes the block observable;
  the dispatch await itself still has no bound. See
  [`BUG-xmpp-polling-and-backpressure.md`](docs/bugs/BUG-xmpp-polling-and-backpressure.md).
- **OMEMO MUC fallback spam remains unverified.** The historical post-restart
  cross-JID symptom has no current live reproduction. Generic stuck-processing
  recovery is fixed, but that does not prove the OMEMO symptom is gone. See
  [`BUG-xmpp-omemo-warmup-and-processing.md`](docs/bugs/BUG-xmpp-omemo-warmup-and-processing.md).

### Additional 2.1 Limitations

- **Direct edits to generated WeeChat capabilities are not durable.** A test-time
  `dm_policy` edit in `weechat.capabilities.json` is overwritten by rebuild or
  `install-wasm`. Persist policy through `extensions.weechat.setup_fields` (or
  the canonical local adapter config), not by editing the generated artifact.
- **Worker release-image tags do not advance automatically.** The OpenCode and
  Kimi binaries are pinned to the versions above, but `.gitlab/ci/containers.yml`
  still defaults `WORKER_IMAGE_TAG` to `v2.0.9.0`; release jobs must override it.

### Security, Deployment, and Release Limitations (carried)

- DarkIRC Secure Key Exchange Stages B–D and automated QR/web/trusted-channel
  delivery remain disabled behind separate acceptance gates.
- MCP does not auto-install npm/pip packages, has no signed-package marketplace
  CLI, and does not pass configured MCP servers into `lunarwing-agent-v1`
  external workers. Custom HTTP MCP headers remain direct string values in MCP
  configuration rather than owner-scoped secret references.
- The browser gateway puts bearer tokens in native `EventSource` query strings.
  Protect browser history and access/proxy logs until the transport uses
  short-lived SSE tickets or cookie-scoped sessions.
- The onboarding web app remains a trusted local console without user accounts
  or TLS. Keep its default loopback binding and token authentication; use an SSH
  tunnel for remote access.
- OpenRC hosts without usable cgroup v2 delegation fall back to nice-only build
  scheduling instead of hard CPU, memory, and task limits; mt-admin reports the
  fallback.
- `ALLOW_PRIVATE_IPS=1` opts out of public-address SSRF restrictions. Enable it
  only for intentionally trusted private-network deployments; the harness and
  rendered service environment intentionally seed it for private test services.
- `taskset -c 0-11 cargo check -j12 --tests --no-default-features --features
  postgres` still fails in cfg(test) paths that reference libsql-gated test
  helpers. CI does not run that exact feature/test shape.
- Kawarimi remains PostgreSQL-only.
- GitLab CI verifies source but does not publish release packages or deploy
  tenants automatically.
- Large worker-image pushes can still fail with registry `499 Client Closed
  Request` until proxy-chain read timeouts are raised on the GitLab host. See
  [`docs/ops/REGISTRY_PUSH_499.md`](docs/ops/REGISTRY_PUSH_499.md).

### Resolved or Source-Fixed Since v2.0.9.0

- Mid-turn interrupt aborts classic-loop LLM streams and tool batches, while
  bridge-owned Engine V2 interrupt/steer targets scoped running threads. Live
  gateway, WeeChat, and XMPP checks passed 6/6.
- Interrupted classic and Engine V2 turns persist a history marker and render
  as `Interrupted`, including paginated history boundaries, instead of being
  mislabeled `Failed` when no assistant row lands.
- Proxy-disabled tenant generation derives TensorZero's `/openai/v1` surface
  from a bare upstream URL while preserving explicit `--llm-base-url` values.
- LunarVoice has the complete mt-admin lifecycle and registry-aware readiness
  checks on systemd and OpenRC; external LunarVision can replace a local GPU
  sidecar with a constrained HTTPS origin.
- The gateway mascot parses and renders eight emote states without changing chat
  content.
- SSH AcceptFirst pins persist across restart, fail closed on storage errors,
  and are shared with strict `ssh_git` host-key materialization (#98 source fix;
  tracker still open).
- Models now contribute numeric exporter series, and six legacy health checks
  expose configurable degraded/critical thresholds.
- The scoped `ic/` dependency closure is committed under `ic/vendor/`; locked
  offline workspace checks pass, every registered WASM tool has a committed
  lockfile, and the refresh script includes advisory verification. GitLab's
  formerly missing `serde_ignored` dependency is included.
- LLM `finish_reason` survives blocking completions, stream aggregation, bridge,
  and proxy paths (#95 source fix; tracker still open). A partially emitted
  Engine V2 stream is no longer advertised as safely retryable.
- Mission cron startup waits 30 seconds before its first due check (#94
  mitigation; tracker still open), and evaluation top-error ranking is
  deterministic (#96, closed).
- Room-bare JIDs can authorize all occupants in a coordination-knowledge room;
  live tenant configuration/retest remains outstanding (#97).
- Passive fabricated-result phrases are detected and challenged with a bounded
  tool-use correction rather than accepted as execution evidence.
- The onboarding console no longer shows a false token warning in `--no-token`
  mode, and its five operation flows have browser coverage.
- Shallow tenant-source clones and disabled-DarkIRC WASM installation are fixed.
- ShellCheck is a zero-warning blocking CI gate, and Wasmtime 36.0.14 carries the
  advisory fixes selected for this release.
