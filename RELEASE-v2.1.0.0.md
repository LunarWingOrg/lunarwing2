# Release Notes for LunarWing v2.1.0.0

## Codename: Ma-ai (間合い)

**Release date** 2026-09-07

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
gateway presentation, memory-recall, supply-chain hardening, and
release-engineering release. Its headline feature gives operators real-time
control over a running agent turn: a mid-turn interrupt aborts an in-flight LLM
stream or tool batch — not only at turn boundaries — and Hermes-style steer
injects guidance at the next safe execution boundary without breaking
message-role ordering.

LunarVoice (the ONNX speech-to-text sidecar) gains a complete mt-admin lifecycle,
registry-aware health integration, and a documented production configuration
surface. LunarVision can now use an operator-managed external HTTPS instance,
including automatic XMPP image enrichment and the `vision-analyze` WASM tool.
Mission knowledge recall now degrades safely when embeddings or strict full-text
queries fail, and mission sub-agents can resolve the structured memory writer.

Correctness and operability work includes provider-stream integrity detection
and per-provider telemetry; owner-scoped secret references for HTTP MCP
headers; configurable pending-gate expiry; exportable Prometheus metrics from
every healthpoint with env-configurable early-warning thresholds; end-to-end
LLM `finish_reason` propagation; passive-voice fabricated-result detection;
deterministic evaluation error ranking; room-bare-JID coordination-knowledge
publisher authorization; durable SSH AcceptFirst pins; a mission-scheduler
startup warmup; browser-side reconciliation for one lost-final-response path;
and executable browser coverage for the onboarding web GUI. The web gateway
also gains an opt-in emoting, draggable mascot;
`MASCOT_ENABLED=false` keeps it disabled by default.

Protected CI now validates release tags across the backend/test/security matrix
and can publish worker images only through digest-bound, reproducible candidate
and promotion jobs. Production daemon/package publication and tenant deployment
remain outside this release. The release identifier is `2.1.0.0`;
LunarWing-owned Rust crates and channel packages use `2.1.0`. Rust 2024 and
MSRV `1.96.1` are unchanged. There are no new SQL migrations
(`V26__study_notes_knowledge.sql` remains latest) and no new multi-tenant
port-registry migration (v16 remains current).

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
  one exists and acknowledges the queued note otherwise.
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
- **Emoting web mascot (opt-in).** The gateway classifies italic action phrases
  into eight canonical emotes, attaches an optional `emotes` array to final
  response events, and renders a draggable, hideable sprite widget with idle,
  working, greeting, error, and FIFO emote states. The parser covers dialect and 
  a common list of verbs; non-empty emotes are pinned through the
  WebSocket adapter; and enabled public gateways served all eight sprite
  routes. `MASCOT_ENABLED=false` omits the script and sprite routes by default,
  while `true` opts a tenant in. Work item #107 is tracker-closed.
- **Lost final-response mitigation.** For work item #127, the browser records
  terminal response events per thread and can reconcile persisted history after
  `Done` when no final response reaches the page. The merged client guard covers
  one loss shape; server-side lag signaling, sequence-aware recovery, and the
  broader acceptance case remain open.
- **Durable SSH AcceptFirst pins.** Accepted host keys are stored as a versioned,
  tenant-scoped encrypted pin set before a first handshake succeeds. Restart,
  tenant-isolation, corruption, revocation, and concurrent-first-key paths are
  covered; SSH bridge creation fails closed when pin persistence cannot load.
- **Every healthpoint exports metrics.** The models check now emits numeric
  provider counts and latency/status maps, all six legacy checks expose
  degraded/critical thresholds through `HEALTH_<CHECK>_*` knobs, and the new
  `llm_streams` healthpoint exports per-provider stream/retry/abort/latency and
  corruption counters. The exporter is host-global and carries per-tenant labels
  where the source healthpoint is tenant-scoped; `health_metrics` remains the
  v16 registry slot.
- **The `ic/` dependency supply chain is vendored.** `ic/.cargo/config.toml`
  source-replaces crates.io with `ic/vendor/`; locked `ic/` builds pass with
  `CARGO_NET_OFFLINE=true`. `ic/scripts/refresh-vendor.sh` re-vendors the
  workspace plus 18 tracked satellite lockfile roots, checks the resulting set,
  and runs `cargo deny check advisories`.
- **LLM `finish_reason` is no longer dropped.** Engine V2 `LlmOutput`, blocking
  bridge adapters, stream aggregation, and the worker proxy carry canonical
  `stop`/`length`/`tool_calls`/`content_filter`/`unknown` strings. The parser also
  accepts legacy `tool_use` for rolling-upgrade interop.
- **Provider stream integrity is observable and fail-safe.** Repeated substantial
  n-grams, ChatML/Llama role delimiters, and bare role headers are detected in
  bounded prose (code fences are excluded). Corrupted output is marked
  non-executable and tool calls are stripped; typed observations remain outside
  model text and flow through retries, caches, failover, routing, and the
  orchestrator. Per-provider telemetry persists and is exported without making
  the healthpoint an automatic circuit breaker.
- **Mission memory recall degrades instead of failing closed.** Embedding errors
  fall back to keyword search, strict full-text misses get one sanitized OR pass,
  retrieval filters on actual keyword overlap with a type/recency fallback, and
  mission RLM children can resolve `__save_memory_doc__` with an explicit prompt
  contract.
- **HTTP MCP bearer headers can use owner-scoped secrets.** `header_secrets`
  references are resolved only at activation, never persisted in plaintext, fail
  closed on missing or wrong-owner secrets, and suppress the OAuth path for
  static-header servers. The CLI and web install paths expose the same mapping.
- **Pending-gate expiry is configurable and bounded.**
  `AGENT_PENDING_GATE_EXPIRY_SECS` overrides the DB setting within the inclusive
  30-second-to-30-day range; out-of-range values use the 1,800-second default.
- **Protected CI/CD is release-aware.** Protected RC/final tags receive the
  complete validation context, PostgreSQL-only and libSQL-only test-target
  checks, a non-skipping PostgreSQL integration job, and digest-pinned security
  checks. Worker candidates use rootless BuildKit, immutable full-SHA tags,
  clean rebuild equality, Trivy/SBOM evidence, and worker-specific digest-bound
  promotion; CI never publishes `latest` or short-SHA worker tags. Transient
  apt/npm/V8 build state is scrubbed, and two cold builds per worker produced
  identical OCI manifest digests. The critical-vulnerability gate removes
  fixable findings and carries six reviewed, rationale-bearing, time-limited
  no-fix exceptions that expire on 2027-03-06.
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
| Tool registry patches | cavepony 0.4.2 (was 0.4.0); GitLab 0.1.4 (was 0.1.3); llm-context 0.1.1 (was 0.1.0); multica-bridge 0.1.2 (was 0.1.1); ssh 0.1.1 (was 0.1.0); `vision-analyze` 0.1.3 (was 0.1.1); voice-transcribe 0.1.1 (was 0.1.0); web-search 0.2.2 (was 0.2.1) |
| WASM runtime | Wasmtime / wasmtime-wasi 36.0.14 (was 36.0.13) |
| Dependency resolution | `ic/vendor/` source-replaces crates.io for the `ic/` workspace and satellite manifests rooted under `ic/`; root-level satellites are out of scope |
| Worker image tool pins | opencode `v1.18.25` at reviewed upstream commit `cb7d8b2f5e44876ef98b661dc10590c915af3a9f` (rolled back from v1.18.29 after live upstream regressions); kimi-code `0.41.0` |
| Worker image publication | Protected rootless BuildKit candidates use unique full-SHA/pipeline/job tags; only the tested digest is promoted to `vX.Y.Z.W[-rcN]-<worker>`; CI publishes neither `latest` nor short-SHA tags |
| Gateway mascot default | Disabled; set `MASCOT_ENABLED=true` per tenant to load the widget and sprite routes |
| Pending-gate expiry | `AGENT_PENDING_GATE_EXPIRY_SECS` over DB `agent.pending_gate_expiry_secs`, default 1,800 seconds; values outside 30 seconds through 30 days fall back to the default |
| HTTP MCP credentials | `header_secrets` are owner-scoped activation-time references; secret values are not persisted in `mcp-servers.json` |
| CI scope | Protected worker-image candidate publication/promotion is supported; daemon/package publication and tenant deployment remain deferred |

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

The follow-up engine work — work items #103 and #104, plus the interrupt half
of work item #105 — hardened both controls against the idle and interrupt
races that the live pass could not cover:

- Steer is no longer dropped while a conversation is idle, interrupted, or
  approval-paused. Engine conversations persist a serde-default queue of at
  most four pending steer notes that is cleared with the conversation. Routed
  steer acknowledges as soon as it is queued, injects into a running scoped
  thread when one exists, and otherwise persists across the
  idle/thread-completion race; pending notes are consumed by the next
  foreground turn and also delivered on running-thread injection, suspended
  resume, and approval/auth resume. The legacy loop accepts idle, interrupted,
  processing, and approval-paused steer with the same four-note bound, and
  serde defaults keep existing conversation records compatible.
- Failed runtime steer injection no longer acknowledges a dropped signal: when
  the destination run is closed or already cancelled, the note falls back to
  the durable conversation queue and the caller is told the queue was accepted.
- The Engine V2 per-run cancellation token now wraps in-flight CodeAct, direct,
  and parallel tool host calls as well as LLM streams, so `/interrupt` drops a
  long-running tool future promptly and returns a normal stopped outcome.
- Engine documentation now pins the contract: steer injects at the next
  model-call boundary, after the current step or tool result, and deliberately
  never cancels the running tool. These fixes close the source side of work
  items #103 and #104 and the interrupt half of #105; regression coverage
  passed, and all three tracker items were closed by 2026-09-07. Checklist item
  43 then verified gateway in-flight cancellation,
  mid-turn steer, idle queued steer, and the gate-expiry path on fresh tenant
  `merlin`. WeeChat was skipped after a repeatable WASM poll trap, and XMPP
  outbound peer delivery remained environment/bootstrap-shaped after inbound
  processing succeeded. Evidence:
  [`docs/ops/v2.1.0.0_GOAL43_STEER_INTERRUPT_LIVE_RERUN_KI.md`](docs/ops/v2.1.0.0_GOAL43_STEER_INTERRUPT_LIVE_RERUN_KI.md).

### Expired-Gate Turn Resume (work item #108)

Previously, a pending approval gate that expired through the background sweep
was treated as a failure: the paused thread was finalized as `Failed` with no
resume, no channel notification, and no reply, so the conversation was left
permanently unanswered. Expiry now resumes the paused turn:

- The expired gate is injected into the resumed thread as an expired-gate
  action result with the same shape as a denial, the model wraps the turn up,
  and the wrap-up reply is delivered to the gate's source channel through the
  normal broadcast, SSE response, and v1 history paths, including outbound
  hooks.
- The gateway also receives a `GateResolved` event with an `expired` resolution
  so any open approval prompt clears.
- Resume is defensive: a conversation missing from the in-memory map (for
  example, gates restored after a daemon restart) no longer blocks the resume;
  a thread that cannot be resumed still fails closed but the channel is told;
  and a wrap-up that requests another gate is not re-armed, ending the turn
  with an explanatory notice instead of looping. `/interrupt` and `/steer`
  still observe the wrap-up because the thread stays in the running map until
  terminal.
- Checklist item 43 verified the full expiry path live with a 120-second
  setting: the expired result reached the thread, the no-re-arm guard ended a
  second approval request, and the completed reply arrived through SSE.

### Configurable Pending-Gate Expiry (work items #111 and #115)

`AGENT_PENDING_GATE_EXPIRY_SECS` is now the environment override for approval
and authentication gate lifetime. Resolution remains env > DB setting
`agent.pending_gate_expiry_secs` > the 1,800-second default. Values below 30
seconds fall back to the default because the background sweep runs every 60
seconds. Values above 2,592,000 seconds (30 days) also fall back, preventing
unsigned-to-signed wraparound and Chrono duration overflow. The accessor is
resolved once in `AgentConfig` instead of rereading settings ad hoc in the
bridge router. The `.env.example` and Engine V2 configuration guide document
the bounded range.

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
  systemd and OpenRC hosts. The `vision-analyze` tool registry version is 0.1.3.

### HTTP MCP Header Secret References

Hosted HTTP MCP servers that use a static bearer header no longer have to be
misclassified as OAuth-only servers:

- `McpServerConfig.header_secrets` maps an HTTP header name to an owner-scoped
  secret name. The CLI's `--secret-header KEY=SECRET_NAME`, `mcp update`,
  `tool_install`, and the web install path validate and preserve the mapping;
  secret values never enter persisted MCP configuration.
- Activation resolves each reference through the activating owner's secrets
  store, fails closed on a missing store/secret or wrong owner, and injects the
  resolved value only into the live HTTP client. Managed-header and duplicate
  conflicts are rejected, and secret-bearing installs retain the Always
  approval requirement.
- Configured secret headers suppress OAuth discovery and stale OAuth bearer
  injection. A live hosted study-notes server authenticated successfully after
  activation with its resolved bearer header; focused owner-scope, failure, and
  wire tests passed. Full details:
  [`docs/ops/v2.1.0.0_MCP_HTTP_HEADER_SECRETS.md`](docs/ops/v2.1.0.0_MCP_HTTP_HEADER_SECRETS.md).

### Emoting Gateway Mascot

The gateway now includes a presentation-only mascot pipeline:

- `channels::web::emote` extracts bounded italic action phrases, maps
  case-insensitive keywords to eight `Emote` variants, deduplicates in order,
  and caps each response at four emotes. The requested examples are pinned by
  tests: `*stamps hooves*` maps to `jump`, and `*ears perk*` maps to `alert`.
- After the live mascot defect was double-detected and triaged (work item
  #107), the keyword table was extended by roughly fifty entries covering the
  herd's equine dialect (`nicker`, `whinny`, `snort`, tail flicks, pawing,
  pinned ears) and common live roleplay verbs (`smile`, `grin`, `giggle`,
  `laugh`, `nod`, `sigh`), with stem forms matching inflections
  (`whinn` catches both `whinny` and `whinnies`). The triage confirmed the
  parser gap was the live defect and that the wiring, event shapes, and widget
  paths were intact; the full record is in
  [`docs/ops/v2.1.0.0_GOAL33_WI107_MASCOT_TRIAGE.md`](docs/ops/v2.1.0.0_GOAL33_WI107_MASCOT_TRIAGE.md).
- Final assistant response events from direct gateway responses, broadcasts,
  and bridged `AppEvent` conversions carry `emotes` only when non-empty. Parsing
  never changes assistant text. The item-45 recheck added an explicit non-empty
  WebSocket-adapter regression and verified that parser, widget, route, and disk
  sprite sets agree 8/8.
- Eight allowlisted GIFs are embedded under `/mascot/`. The browser widget is
  draggable, position-persistent, hideable/restorable, and uses idle/working
  fallbacks plus greeting, error, and bounded FIFO emote playback. Its default
  anchor clears the chat input bar.
- The widget is now an explicit tenant opt-in: `MASCOT_ENABLED=false` is written
  for new and legacy tenants, preserved on re-render and export/import, and
  prevents mascot script, JavaScript, and sprite routes from being registered.
  Setting it to `true` and restarting restores the existing widget. Two enabled
  public gateways served the current script and all eight sprites during the
  item-45 recheck; no authenticated source-matched human animation turn was run.
  Work item #107 is now tracker-closed. Full evidence:
  [`docs/ops/v2.1.0.0_GOAL45_WI107_VERIFICATION.md`](docs/ops/v2.1.0.0_GOAL45_WI107_VERIFICATION.md).

### Web Gateway Final-Response Reconciliation (work item #127)

The gateway broadcasts terminal `Done` status before the final `response` event.
If the broadcast receiver lags, the response can be dropped while SSE keepalive
traffic leaves the connection looking healthy; the old `Done` handler then
cleared live-work state without rendering or reloading the persisted answer.
A client-side candidate mitigation is now present:

- Final `response` events record a per-thread render timestamp, including for
  background threads.
- On `Done`, when the current page has neither live stream text nor a recent
  rendered response, the browser waits 1.5 seconds for the normal response race
  and then calls `loadHistory()` if the response still did not arrive. The
  assistant row is persisted before `Done`, so this path can recover it.
- Two Playwright scenarios were added to target the intended missing-response
  reconcile and healthy no-reload paths through a controllable `EventSource`.

This is not the complete transport fix. `BroadcastStream::Lagged` is still
silently discarded, there is no gap event or per-thread sequence protocol, and
the current `sawResponseThisTurn` condition does not schedule this reload when
live stream text existed before `Done`. Work item #127 therefore remains open
for server-side loss signaling, full-path correction, and acceptance.

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

### LLM Provider Stream Integrity (work item #110)

The provider stream is now observed as a typed side channel rather than being
left to the model to interpret as its own cognition:

- The provider chain records stream starts, first-token latency, retries, aborts,
  decode failures, and provider attribution through failover, response cache,
  circuit breaker, and smart routing. Observations stay outside transcript text
  and are validated before the orchestrator consumes them.
- Bounded prose detection catches repeated substantial n-grams, ChatML/Llama
  role-delimiter fragments, and bare `SYSTEM:`/`ASSISTANT:` headers while
  ignoring fenced and inline code examples. A matched response is marked
  non-executable, tool calls are removed, and a visible integrity marker tells
  the agent to treat instruction-like content as generated data.
- `health-llm-streams.sh` persists schema-versioned per-provider telemetry and
  exports tenant/provider metrics through the host-global health pipeline. It is
  monitor-only: a recent issue degrades the healthpoint but does not silently
  remove a provider from service. Regression coverage includes fragmented
  streams, retry attribution, malformed snapshots, and the corrupted-output
  execution guard. A plain-prose role header remains an intentional fail-safe
  false-positive trade-off.

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

### Mission Memory Persistence and Recall (work item #30)

The investigation separated a misleading symptom from the actual defect:
mission output was being persisted, but several recall paths failed closed and
made an existing document look absent.

- Embedding-provider failures now log and fall back to keyword-only search
  instead of aborting the query. PostgreSQL full-text search gets one sanitized,
  capped OR-term retry when its strict AND query returns no rows.
- Engine retrieval filters on real keyword overlap; when there is no overlap it
  returns a bounded type-priority/recency fallback rather than an empty context.
  This avoids turning a weak match into a false claim that memory does not exist.
- Mission RLM children carry mission semantics, so they can resolve the
  `__save_memory_doc__` host function. Mission prompts now document the function,
  supported document types, and raise-on-failure behavior.
- A fresh webhook-fired probe wrote and indexed a structured mission note and
  recovered it through exact, polluted, and natural-language searches. Existing
  mission and workspace persistence formats remain compatible; unbounded mission
  history and duplicate-seeding observations remain open follow-ups.

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

### Protected CI/CD and Worker Image Reproducibility

The release pipeline no longer treats worker-image publication as an ordinary
manual job:

- Candidate jobs exist only for protected default-branch pushes and protected
  version-shaped RC/final tags. They are non-interruptible, do not retry, and
  publish a unique full-commit/pipeline/job tag. A separate serialized
  promotion job copies the already-tested digest to a worker-specific release
  tag; it never rebuilds or writes `latest`.
- Rootless BuildKit replaced archived Kaniko. Worker Dockerfiles and build
  helpers pin base images, source revisions, package indexes, dependency locks,
  and downloaded toolchains where applicable. Transient apt/dpkg/ldconfig logs,
  npm debug logs, and the Node V8 compile cache are removed in the creating
  layer. After that remediation, two cold builds of each worker produced
  identical OCI manifest digests. A clean second build must match the first
  digest; candidate metadata, input hashes, Trivy results, SBOM, and
  source/health metadata are retained as promotion evidence.
- The first full candidate-to-scan run passed reproducibility for all three
  workers and exposed the remaining critical-image findings. Unused `gh` was
  removed from OpenCode/Kimi, Kimi pins npm 12.0.2 for the fixed `tar` transitively
  bundled by npm, and all workers use the 20260906 Debian snapshot. Six findings
  with no fix in that snapshot remain in `.gitlab/trivy-ignore`, each with an
  in-file dependency rationale and an `expiring:2027-03-06` deadline; expiration
  restores a failing critical scan rather than silently extending the exception.
- Protected RC/final tags run the complete validation context, including
  backend-specific test-target compilation, the non-skipping PostgreSQL
  integration target, cargo-deny, secret/policy scans, WASM compatibility, and
  the required Python and shell suites. Validation caches are pull-only;
  `sccache` is capped at 10 GiB on the documented 40 GiB runner, with two-hour
  validation and eight-hour worker-candidate timeout contracts.
- The desktop live harness is explicitly ignored in ordinary libSQL test runs
  because it requires an operator-controlled `DESKTOP_ITEM7_CONTROL` session;
  it remains runnable with `--ignored` instead of failing a headless CI job.
  The date-boundary assertion in the knowledge freshness regression was also
  made deterministic for CI.
- Production package publication and tenant deployment remain deliberately
  deferred: mt-admin still builds/activates tenant software locally, while the
  protected pipeline currently supplies verified worker-image artifacts and
  evidence only. The activation checklist in [`.gitlab/README.md`](.gitlab/README.md)
  remains required before enabling production distribution.

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
- Every built-from-source tool gains a committed `Cargo.lock` plus a matching
  registry patch-version bump, with a strict build-harness test, so installed
  tenants rebuild vendor-consistent WASM artifacts instead of resolving from
  crates.io ad hoc.

### Dependency, Version, and Worker Updates

- LunarWing-owned release crates and channel packages moved from 2.0.9 to 2.1.0.
  Channel registry patches are multica 0.1.9 and xmpp 0.1.12; darkirc 0.1.6 and
  weechat 0.2.7 are unchanged. Tool registry bumps are cavepony 0.4.0 → 0.4.2,
  GitLab 0.1.3 → 0.1.4, llm-context 0.1.0 → 0.1.1, multica-bridge
  0.1.1 → 0.1.2, ssh 0.1.0 → 0.1.1, `vision-analyze` 0.1.1 → 0.1.3,
  voice-transcribe 0.1.0 → 0.1.1, and web-search 0.2.1 → 0.2.2 (registry-level
  patch versions; of the crate versions, only cavepony moved, 0.4.0 → 0.4.2).
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
- Worker tool pins are opencode `v1.18.25` and kimi-code `0.41.0`, advancing
  the v2.0.9.0 defaults (`v1.18.23` and `0.39.0`). The attempted OpenCode
  `v1.18.29` update was rolled back after live worker diagnosis found two
  upstream regressions: incomplete model metadata can crash provider/model
  resolution, and the ai-sdk stream path can terminate silently before its
  first chunk. `v1.18.25` at reviewed commit
  `cb7d8b2f5e44876ef98b661dc10590c915af3a9f` is the last release proven
  end-to-end in this worker; the corrected model ID/API-key/context
  configuration remains. Kimi's package and integrity lockfiles are installed
  with `npm ci`. Dockerfile defaults, protected CI inputs, examples/docs, and
  the Kimi mt-admin assertion are synchronized.

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
- New tenant provisioning can seed `BRAVE_SEARCH_API_KEY` (with the legacy
  `BRAVE_API_KEY` fallback) into the tenant's encrypted web-search secret store
  after the gateway is ready. The upload is best-effort and never prints the
  key; tenants without a host key are unchanged.

## Upgrade Notes

1. Back up PostgreSQL, tenant environment files, `/etc/lunarwing/ports.json`,
   workspace state, rendered units, worker data, voice models, and channel state.
2. No SQL or port-registry migration is introduced. `V26` and registry v16
   remain current.
3. Rebuild the daemon and installed WASM extensions through the normal deployment
   path. Channel registry versions multica 0.1.9 and xmpp 0.1.12 changed;
   darkirc 0.1.6 and weechat 0.2.7 did not. Rebuild/reinstall `vision-analyze`
   for its tool-registry bump to 0.1.3; rebuilt tools carry vendor-consistent
   committed lockfiles with matching registry patch versions (ssh 0.1.1,
   llm-context 0.1.1, multica-bridge 0.1.2, voice-transcribe 0.1.1, web-search
   0.2.2, GitLab 0.1.4, cavepony 0.4.2), so installed tenants rebuild
   vendor-consistent WASM artifacts.
4. Cargo manifests rooted under `ic/` now resolve crates.io through
   `ic/vendor/`. For a dependency change, run `ic/scripts/refresh-vendor.sh`,
   review the vendor diff, and commit it with the manifests/locks. Root-level
   satellite projects remain outside this source replacement.
5. `/interrupt` and `/steer` are available through the web gateway, WeeChat,
   XMPP, and REPL. Engine V2-routed controls target the scoped running engine
   thread when one exists; steer that arrives while a conversation is idle,
   interrupted, or approval-paused is acknowledged as queued and delivered on
   the next turn or resume instead of returning an idle response. Interrupting
   a queued-but-not-running turn still returns "Nothing to interrupt." Checklist
   item 43 passed the gateway interrupt/steer and idle-queue cases live; its
   WeeChat case was skipped because of a fresh-channel WASM trap and its XMPP
   outbound peer case remained bootstrap-shaped, so review the linked live record
   before treating those channel paths as fully accepted.
6. Manage LunarVoice through `install-voice`, `configure-voice`,
   `activate-voice`, `deactivate-voice`, and `remove-voice`. Reconcile any
   manually deployed sidecar to mt-admin's rendered units and registry ports.
7. To use external LunarVision, run `configure-vision-external <name> --url
   <https-origin> --token-file <path>`; the health endpoint must be reachable
   before the switch. `configure-vision-local <name>` restores the managed local
   sidecar. Existing local tenants remain local unless explicitly changed.
8. Health thresholds are tunable through the documented `HEALTH_<CHECK>_*`
   variables in `ic-infrastructure-health-check/config.sh.example`. The new
   `llm_streams` check reads persisted per-provider telemetry and uses its
   documented issue window; defaults retain the previous thresholds.
9. Coordination-knowledge tenants must both join the intended MUC via
   `XMPP_ALLOW_ROOMS_JSON` and authorize either exact occupant JIDs or the room
   bare JID in `COORD_KNOWLEDGE_PUBLISHERS`, then restart and verify the wire
   path. An empty room allowlist drops all groupchat.
10. Worker tool pins are opencode `v1.18.25` (reviewed upstream commit
    `cb7d8b2f5e44876ef98b661dc10590c915af3a9f`) and kimi-code `0.41.0`.
    Do not restore OpenCode v1.18.29 until both documented upstream regressions
    are fixed. Pull worker images by the digest recorded in
    `worker-image-evidence.json`; do not depend on a moving `WORKER_IMAGE_TAG`,
    `latest`, or a short-SHA tag. Review the dated `.gitlab/trivy-ignore`
    exceptions before promotion; all expire on 2027-03-06.
11. Set `MASCOT_ENABLED=true` in a tenant environment and restart to opt into
    the gateway mascot. New and upgraded tenants default to `false`; export and
    import preserve an explicit choice.
12. Set `AGENT_PENDING_GATE_EXPIRY_SECS` in the tenant environment when a gate
    lifetime other than the DB setting is required. Values must be between 30
    seconds and 30 days inclusive; otherwise the safe 1,800-second default is
    used. Restart the tenant after changing it.
13. For an HTTP MCP server using a static bearer header, store the complete
    header value in the owner's secrets store and configure
    `header_secrets` (or `lunarwing mcp add --secret-header`). Do not put the
    secret value in `mcp-servers.json`; activation resolves it per owner and
    skips OAuth discovery.
14. Earlier releases have no durable AcceptFirst pin to migrate. The first
    successful post-upgrade connection stores the encrypted pin used for later
    calls and restarts. If that initial path is not trustworthy, configure
    `known_host_key` from a trusted source and use `host_key_mode = "Strict"`
    before connecting.

### Active Canonical Runtime and Integration Follow-ups

- **Kawarimi import flag parity is incomplete.** `import-tenant.sh` always adds
  `--with-wasm` to its build internally but rejects an operator-supplied
  `--with-wasm`. See
  [`BUG-kawarimi-import-flag-parity.md`](docs/bugs/BUG-kawarimi-import-flag-parity.md).

### Security, Deployment, and Release Limitations (carried)

- DarkIRC Secure Key Exchange Stages B–D and automated QR/web/trusted-channel
  delivery remain disabled behind separate acceptance gates.
- MCP does not auto-install npm/pip packages, has no signed-package marketplace
  CLI, and does not pass configured MCP servers into `lunarwing-agent-v1`
  external workers. HTTP `header_secrets` now provides owner-scoped references
  for sensitive static headers, but non-secret literal headers remain literal
  configuration values and the broader MCP limitations remain.
- The onboarding web app remains a trusted local console without user accounts
  or TLS. Keep its default loopback binding and token authentication; use an SSH
  tunnel for remote access.
- OpenRC hosts without usable cgroup v2 delegation fall back to nice-only build
  scheduling instead of hard CPU, memory, and task limits; mt-admin reports the
  fallback.
- `ALLOW_PRIVATE_IPS=1` opts out of public-address SSRF restrictions. Enable it
  only for intentionally trusted private-network deployments; the harness and
  rendered service environment intentionally seed it for private test services.
- Protected CI now compiles PostgreSQL-only and libSQL-only test targets and
  runs a non-skipping PostgreSQL integration job. That integration job requires
  its disposable pgvector/PostgreSQL service; it is not a substitute for a
  tenant deployment or a local live-provider test.
- Kawarimi remains PostgreSQL-only.
- GitLab CI can publish protected, digest-bound worker images after candidate
  verification, but it does not publish daemon packages or deploy tenants
  automatically; mt-admin still performs tenant-local activation.
- Large worker-image pushes can still fail with registry `499 Client Closed
  Request` until proxy-chain read timeouts are raised on the GitLab host. See
  [`docs/ops/REGISTRY_PUSH_499.md`](docs/ops/REGISTRY_PUSH_499.md).

### Resolved or Source-Fixed Since v2.0.9.0

- Mid-turn interrupt aborts classic-loop LLM streams and tool batches, while
  bridge-owned Engine V2 interrupt/steer targets scoped running threads. The
  earlier OpenRC `dragon` matrix passed 6/6 across gateway, WeeChat, and XMPP;
  the later systemd `merlin` rerun independently passed the gateway controls and
  documented the fresh-channel WeeChat/XMPP caveats above.
- Interrupted classic and Engine V2 turns persist a history marker and render
  as `Interrupted`, including paginated history boundaries, instead of being
  mislabeled `Failed` when no assistant row lands.
- The gateway client can reload persisted history for the `Done`/missing-final-
  response case when no live stream text remains (#127 candidate mitigation).
  Server-side gap signaling and the broader intermittent-output issue remain
  open and are not marked resolved.
- Proxy-disabled tenant generation derives TensorZero's `/openai/v1` surface
  from a bare upstream URL while preserving explicit `--llm-base-url` values.
- LunarVoice has the complete mt-admin lifecycle and registry-aware readiness
  checks on systemd and OpenRC; external LunarVision can replace a local GPU
  sidecar with a constrained HTTPS origin.
- SSH AcceptFirst pins persist across restart, fail closed on storage errors,
  and are shared with strict `ssh_git` host-key materialization (#98; closed
  2026-09-02).
- Models now contribute numeric exporter series, six legacy health checks expose
  configurable degraded/critical thresholds, and the `llm_streams` healthpoint
  exports provider-labeled retry, abort, latency, decode, and corruption series.
- The scoped `ic/` dependency closure is committed under `ic/vendor/`; locked
  offline workspace checks pass, every registered WASM tool has a committed
  lockfile, and the refresh script includes advisory verification. GitLab's
  formerly missing `serde_ignored` dependency is included.
- LLM `finish_reason` survives blocking completions, stream aggregation, bridge,
  and proxy paths (#95; repository-complete, re-verified 2026-09-03, and tracker
  closed 2026-09-04). A partially emitted Engine V2 stream is no longer
  advertised as safely retryable.
- Mission cron startup waits 30 seconds before its first due check (#94
  mitigation, closed), and evaluation top-error ranking is deterministic
  (#96, closed).
- Room-bare JIDs can authorize all occupants in a coordination-knowledge room;
  the fix is live-verified on isolated XMPP tenants (4/4, including the
  user-bare negative), and the sunburst/ruffles production retest remains an
  operator action (#97, open).
- Idle, interrupted, and approval-paused steer now queues and acknowledges
  instead of dropping (#103; tracker-closed after the item-46 recheck), and
  Engine V2 `/interrupt` cancels in-flight tool host calls promptly (#104/#105;
  both tracker-closed). The remaining WeeChat/XMPP caveats are environmental/
  live-harness follow-ups, not silently marked as passes.
- An expired approval gate now resumes the paused turn and delivers the
  wrap-up reply to the source channel instead of silently finalizing the
  thread as `Failed` (#108; regression-tested, live-verified by item 43, and
  tracker-closed 2026-09-06).
- Provider stream integrity is observed outside model text, corrupted output is
  downgraded to non-executable content, and per-provider telemetry is exported
  through `llm_streams` (#110; tracker-closed 2026-09-06).
- Mission memory recall now degrades through keyword/OR/fallback paths and
  mission children can resolve `__save_memory_doc__`; live persistence and
  polluted/natural-language retrieval passed the item-30 probe.
- HTTP MCP `header_secrets` resolves owner-scoped static bearer headers and
  suppresses inappropriate OAuth discovery; focused and live hosted-server
  checks passed, and #117 is tracker-closed.
- Pending-gate expiry is configurable through `AGENT_PENDING_GATE_EXPIRY_SECS`
  with env-over-DB precedence and safe lower/upper bounds; #111 and #115 are
  tracker-closed.
- Protected CI now gates release contexts and produces reproducible,
  digest-bound worker candidates/promotions. All three workers passed paired
  cold-build digest checks; the critical scan's six reviewed no-fix exceptions
  are explicit and expire on 2027-03-06. Daemon package publication and tenant
  deployment remain deferred.
- The desktop and Android companion application repositories are linked from
  the root README and the documentation index (#102, closed).
- Passive fabricated-result phrases are detected and challenged with a bounded
  tool-use correction rather than accepted as execution evidence.
- The onboarding console no longer shows a false token warning in `--no-token`
  mode, and its five operation flows have browser coverage.
- Shallow tenant-source clones and disabled-DarkIRC WASM installation are fixed.
- ShellCheck is a zero-warning blocking CI gate, and Wasmtime 36.0.14 carries the
  advisory fixes selected for this release.
