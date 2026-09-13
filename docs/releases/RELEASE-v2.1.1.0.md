# Release Notes for LunarWing v2.1.1.0

## Codename: Zanshin (残心)

**Release date** 2026-09-13

>After the cut, the mind stays
>
>The strike already finished
>
>Still the hand does not lower
>
>残心

## Overview

LunarWing v2.1.1.0 is a correctness, memory-integrity, coordination, and
release-engineering cycle focused on hardening the engine against
self-modification and memory-index desync, closing the priority-control race,
and completing the knowledge-seeder refresh path. Its headline work is engine
hardening for memory authenticity (untrusted retrieval is neutralized and
labeled; `__save_memory_doc__` forces trusted provenance; System-role prompt
injection is closed at both LLM adapters) and a durable outbox-based index
repair path that self-heals memory-document/index desync on startup and on the
heartbeat hygiene sweep.

A queued-prompt race is closed so a priority `/interrupt` that overtakes a
still-queued prompt cancels it in its conversation lane instead of answering
"Nothing to interrupt." and running the turn anyway. Best-effort engine-level
`thread_start` event plumbing lands for generic subscribers, and the
coordination-knowledge sharing work is repo-complete with live-verified XMPP
wire behavior.

Correctness and operability work includes `mt-admin` restart parity for the
Pebble worker, server-side SSE/WebSocket `stream_gap` signaling, a narrowed
worker-connect failure heuristic, gateway voice `/api/voice/*` routes for the
desktop client, deterministic CI/CD validation with classified deliverables and
blocking clippy/architecture ratchets, and a contributor onboarding guide.

Later-cycle hardening closes the mission-goal memory-index leak (WI#135/#136:
mission goal documents stay stored and readable but never enter the search
index), the SSE silent stream-death residual (suppressed responses now emit a
terminal `Rejected` status so clients reconcile), and the OpenRC
writer-quiescence defect pair (WI#133/#134/#137: tenant stops now verify writer
quiescence and fail loudly instead of racing the respawn window). The
LunarVoice sidecar also gains NVIDIA NeMo Parakeet TDT 0.6B int8 STT support,
live-verified through the gateway `/api/voice/transcribe` route. The release
identifier is `2.1.1.0`; LunarWing-owned Rust crates and channel packages use
`2.1.1`. Rust 2024 and MSRV `1.96.1` are unchanged.

See the [v2.1.0.0 release notes](docs/releases/RELEASE-v2.1.0.0.md) for the
previous compatibility baseline.

## Highlights

- **Memory authenticity and index self-heal (engine hardening).** Untrusted
  memory retrieval is neutralized and self-labeled
  `untrusted_memory_retrieval`; `__save_memory_doc__` forces `trust:trusted`
  and `source:extracted`; and System-role content is neutralized at both LLM
  adapters, closing the `__llm_complete__` explicit-System seam. A new
  `memory_documents.indexed_at` outbox marker (SQL V27 on both backends) tracks
  index staleness, and a startup plus heartbeat-hygiene sweep repairs
  stale/desynced index documents.
- **Priority-control vs queued-prompt race fixed.** A priority `/interrupt`
  that overtakes a prompt still queued in the daemon scheduler now cancels it
  in its conversation lane and answers `Interrupted.` instead of running the
  cancelled turn.
- **Knowledge seeder refresh path.** The knowledge store now has a cadence
  runner and additive refresh scans, closing the static-snapshot gap for
  hosted study MCP.
- **Coordination-knowledge sharing (repo-complete).** Coordination-knowledge
  publication and the `KNOW:` wire path are implemented and live-verified on
  isolated XMPP tenants; production operator actions remain for full
  activation.
- **Pebble worker restart parity.** `mt-admin` stop/start bounce now preserves
  and restarts the Pebble worker instead of leaving a tenant workerless.
- **SSE/WebSocket `stream_gap` signaling.** The server converts broadcast lag
  into an explicit `stream_gap` event, and the browser reloads persisted
  history instead of silently losing partial output.
- **Mission goal text is no longer search-indexable (engine hardening).**
  WI#135's cross-thread mission-context leakage is closed on the input side:
  every document under `engine/projects/*/missions/*` (any depth or suffix,
  including `mission.json.bak` siblings) and the project-level `missions.json`
  manifest is stored and readable but excluded from the search index, a
  pre-repair legacy-chunk purge sweep runs at boot and on the heartbeat
  hygiene pass, and `engine/**` search results carry an `engine_internal`
  provenance label.
- **SSE silent stream-death residual closed.** Hook-suppressed responses now
  emit a terminal `Rejected` status instead of broadcasting nothing, so web
  and desktop clients re-enable input and reconcile history instead of
  hanging in a live-work state.
- **OpenRC writer quiescence verified at stop time.** WeeChat OpenRC status
  wording is classified correctly (stopped sessions no longer read as
  running), `stop_tenant` verifies post-shutdown writer quiescence with a
  bounded poller and fails loudly, and the kawarimi import loader quiesces
  writers before its PostgreSQL-only populate window.
- **Parakeet STT in the LunarVoice sidecar.** NVIDIA NeMo Parakeet TDT 0.6B
  int8 becomes an STT engine option via a `nemo_transducer` model-type fix
  and a pinned sherpa-rs master carrying sherpa-onnx v1.12.15, with a host
  model-metadata patching runbook; live-verified on three tenants.
- **Voice gateway for the desktop client.** Authenticated, bounded
  `/api/voice/{status,transcribe,synthesize}` routes proxy the LunarVoice
  sidecar behind the existing auth middleware.

## Compatibility

| Area | v2.1.1.0 behavior |
|---|---|
| Release version | `2.1.1.0` |
| Crate/package version | `2.1.1` for LunarWing-owned release packages |
| Rust edition / MSRV | Rust 2024 / `1.96.1` |
| New SQL migration | `V27__memory_index_outbox.sql` (PostgreSQL and libSQL) |
| Multi-tenant port registry | v16; no schema change from v2.1.0.0 |
| Channel registry patches | darkirc 0.1.7, multica 0.1.10, weechat 0.2.8, xmpp 0.1.13 |
| Tool registry | No changes from v2.1.0.0 |
| WASM runtime | Wasmtime / wasmtime-wasi 36.0.14 (unchanged) |
| Dependency resolution | `ic/vendor/` source-replaces crates.io for the `ic/` workspace and satellite manifests rooted under `ic/` |
| Worker image tool pins | opencode `v1.18.25` at reviewed upstream commit; kimi-code `0.41.0` (unchanged) |
| Gateway mascot default | Disabled; unchanged |
| Knowledge sync | Optional additive refresh via `KNOWLEDGE_SYNC_*` envs and `mt-admin` knowledge-sync module |
| CI scope | Daemon/package publication and tenant deployment remain deferred |

## Changes

### Engine Hardening: Memory Authenticity and Index Desync (work item #132)

The headline design-to-build item. Memory retrieval and persistence now
fail closed against prompt-injection and self-modification, and the
document/index contract self-heals:

- `__retrieve_docs__` neutralizes retrieved content and self-labels it
  `untrusted_memory_retrieval`; System-role content is neutralized at both the
  rig `convert_messages` and LunarWing Cloud adapters, closing the
  `__llm_complete__` explicit-System seam (idempotent on already-neutralized
  prompts).
- `__save_memory_doc__` forces Skill `trust:trusted` / `source:extracted` and
  strips `pending_patch` / `pending_prune`; compaction summaries are prefixed
  and neutralized; `daily/*.md` joins the write-time injection scan set.
- Append durability is corrected so a durable content write no longer returns
  `Err` (removing LLM-retry duplication), split-pattern post-commit rejects now
  compensate by restoring pre-append content atomically, and delete-by-path is
  single-transaction on both backends (resolving the document id inside the
  transaction so a concurrent-delete race cannot return false success).
- New outbox marker `memory_documents.indexed_at` (SQL V27, backfilled so
  upgrades never mass re-embed) stamps staleness in the committing statement;
  `WorkspaceStore::list_stale_index_documents` and
  `Workspace::repair_stale_index` sweep at boot and on the heartbeat hygiene
  pass.

### Priority-Control vs Queued-Prompt Race (goal #9 follow-up)

A priority `/interrupt` that overtakes a prompt still queued in the daemon's
`MessageScheduler` (no engine thread yet) now cancels that prompt in its
conversation lane (channel + user + scope) and answers `Interrupted.` instead
of `Nothing to interrupt.` followed by the cancelled turn running anyway.
Lane-scoped cancellation lives in `ic/src/agent/dispatch.rs`
(`LaneKey`, `MessageScheduler::cancel_lane`), and the acknowledgement text is
shared via `ic/src/bridge` so dispatch and the engine router cannot drift.

### Knowledge Seeder Refresh Path (work item #121)

The knowledge store now has a cadence-runner topology with an additive refresh
scan, replacing the static-snapshot-only behavior. The 2.1 fallback is an
atomically-published checkout generation plus additive scans; save-state temp
filenames carry a nanosecond nonce alongside the pid so concurrent CLI
invocations cannot collide, and healthy import skips no longer increment
consecutive-failure/backoff state.

### Coordination-Knowledge Sharing (work item #97)

Coordination-knowledge publication and the `KNOW:` wire path are
repo-complete and live-verified on isolated XMPP tenants (4/4 including the
user-bare negative). Production activation remains an operator action:
`XMPP_ALLOW_ROOMS_JSON`, `COORD_KNOWLEDGE_PUBLISHERS` full-occupant JIDs, and a
restart of both engines. Recommended hardening (init-agnostic read-only
coordination doctor, bounded reason counters, automated live matrix) is
tracked as follow-ups.

### Pebble Worker Restart Parity (work item #128)

Pebble Quadlet shutdown now stops the owning systemd user unit first (with a
container-stop fallback), and startup clears any `StartLimitBurst` lockout
before restarting, so a tenant bounce no longer leaves the tenant without its
Pebble worker.

### SSE/WebSocket `stream_gap` Signaling and Silent Stream-Death Closure (work item #127)

Server-side SSE and WebSocket subscriptions now convert broadcast lag into an
explicit `stream_gap` event, log the missed-event count, and continue with
retained events instead of silently swallowing the gap. The browser clears
partial live-stream state and reloads persisted history on `stream_gap`, and
`Done` reconciles after its response grace period whenever no terminal response
rendered.

The owned silent stream-death residual is also closed: a hook-suppressed
response (`transform_response` Rejected) used to return
`SubmissionResult::Suppressed` with no terminal broadcast, leaving clients
stuck in a live-work state. Both suppression sites in
`ic/src/agent/thread_ops.rs` now emit a terminal `StatusUpdate::Status("Rejected")`
after persisting tool calls, and the web client's `status` handler treats
`Rejected` like `Done` (clears the streaming snapshot, finalizes the activity
group, re-enables input, reconciles history after a grace period).

### Worker Connect-Failure Heuristic (work item follow-up, MR !893)

`describe_connect_failure` previously appended a rolling-upgrade/subprotocol
hint to any HTTP 4xx rejection lacking an echoed `Sec-WebSocket-Protocol`, so a
worker auth rejection (401) misleadingly surfaced "recreate/upgrade your worker
container" guidance. It now applies the hint only to statuses that actually
indicate a protocol mismatch (400 Bad Request / 426 Upgrade Required).

### Gateway Voice API (work item #131)

New `ic/src/channels/web/voice.rs` adds authenticated, bounded
`/api/voice/{status,transcribe,synthesize}` routes behind the existing auth
middleware. The gateway proxies the loopback LunarVoice sidecar and holds the
`VOICE_AUTH_TOKEN` server-side. Bounds: base64 audio 24 MiB, synthesis text
10 000 chars, per-route body limits, per-user 10 req/60 s rate limit.
Sidecar 401 maps to 500 (never passthrough), other upstream failures to 502,
timeouts to 504.

### Best-Effort `thread_start` Event Plumbing (work item #109, scope A)

Generic best-effort `engine/thread_start` event plumbing lands for top-level
Foreground threads (Mission recursion and Research-child fan-out excluded),
with payload carrying `source_thread_id`, `project_id`, `thread_type`, and
`goal`. Synchronous pre-execution sanitization remains out of scope (scope B).

### CI/CD Validator Hardening (goal #32 follow-up)

`validate_config.py` validates worker version pins, pinned CI tool images, and
every manifest `ci` job name against the assembled CI configuration;
`rust-clippy-all` and `architecture-boundaries` are promoted to blocking
ratchets; JUnit reports cover Python unit and shell harnesses; and the
classified `.gitlab/deliverables.json` manifest gates unclassified lockfiles.

### Contributor Onboarding

`docs/guides/CONTRIBUTOR-ONBOARDING.md` materializes the Baud contributor
onboarding document and is indexed in the `docs/README.md` guides table.

### Mission-Goal Memory-Index Isolation (work items #135/#136)

Continuing the WI#132 hardening family: mission `mission.json` goal documents
were tenant-wide searchable, so a sibling mission could retrieve another
mission's full goal text (including in-band nonce tokens) with a single
`memory_search` query. `is_mission_path` now excludes the whole
`engine/projects/*/missions/*` subtree (any depth and suffix, including
`mission.json.bak` and nested segments) and the project-level
`missions.json` manifest (plus its suffix variants) from the search index —
documents stay stored and readable, but they never become chunks. A new
`purge_mission_index_chunks` sweep removes pre-fix legacy chunks at boot
(before the embedding backfill) and on the heartbeat hygiene pass on both
backends, and `build_memory_search_output` labels every `engine/**` result
with `"provenance": "engine_internal"`, mirroring the
`untrusted_memory_retrieval` seam from WI#132. A mission doc written to
`.../mission.json.bak` or an extra nested segment can no longer become
search-indexable, closing the exact-string-basename bypass flagged in
WI#136.

### OpenRC Writer Quiescence (work items #133/#134/#137)

Three cooperating defects in the OpenRC tenant-stop path are fixed: the
WeeChat OpenRC unit's `status()` reports `session is not running` when
stopped, and the bare `*running*` substring pattern classified every stopped
WeeChat tenant as an active writer forever (so stop/restart/export could
never cleanly complete); writer containers with a restart policy could be
respawned by the runtime right after `stop_tenant`'s stop calls; and the
supervise-daemon SIGTERM teardown can leave a container `Running=true` for
seconds. Stop-time classification now matches both WeeChat status wordings
explicitly, `stop_tenant` verifies writer quiescence after shutdown with a
bounded poller (1 s interval, 30 s deadline, env-overridable) and fails
loudly on active or unclassifiable state before Nanocode retirement, and the
kawarimi import loader runs `mt-admin stop-writers` between `start-tenant`
and `stop-tenant` so the PostgreSQL-only populate window never races
writers. A tenant with no detectable container runtime (uninstalled,
broken, or daemon-only) classifies as quiesced rather than unclassifiable —
zero writer containers can exist or respawn — while an existing-but-failing
runtime still fails closed. Live-verified on a real OpenRC host with a fresh
test tenant (restart, stop, start, and a real export all clean).

### LunarVoice Parakeet STT (voice sidecar)

The LunarVoice sidecar gains NVIDIA NeMo Parakeet TDT 0.6B int8 as an STT
engine, added alongside the existing `whisper-tiny` option (which remains the
default; select Parakeet via `VOICE_STT_MODEL`). `TransducerConfig` carries
the `nemo_transducer` model type for the Parakeet branch (NeMo-2.x decoder
input names abort the generic transducer path), and sherpa-rs is pinned to
the upstream master revision vendoring sherpa-onnx v1.12.15, which accepts
Parakeet exports at decoder init. `docs/ops/v2.1.1.0_PARAKEET_STT_MODEL_METADATA_PATCHING.md`
documents the one-time host-side model-archive metadata patch and SELinux
labeling caveat. Live-verified on three tenants: exact transcription through
the daemon `/api/voice/transcribe` route, Kokoro TTS unaffected.

## Upgrade Notes

1. Back up PostgreSQL, tenant environment files, `/etc/lunarwing/ports.json`,
   workspace state, rendered units, worker data, voice models, and channel
   state.
2. One new SQL migration (`V27__memory_index_outbox.sql`) is introduced on both
   PostgreSQL and libSQL; it is backward-filled so upgrades do not mass
   re-embed existing documents. The port registry remains v16.
3. Rebuild the daemon and installed WASM extensions through the normal
   deployment path. All four channel registry packages changed (darkirc 0.1.7,
   multica 0.1.10, weechat 0.2.8, xmpp 0.1.13); tool registry packages are
   unchanged from v2.1.0.0.
4. To enable additive knowledge sync, set the documented `KNOWLEDGE_SYNC_*`
   environment variables (or the `mt-admin` knowledge-sync module); otherwise
   behavior is unchanged.
5. Coordination-knowledge tenants must both join the intended MUC and authorize
   occupants via `COORD_KNOWLEDGE_PUBLISHERS`, then restart both engines.
6. Worker tool pins are unchanged (opencode `v1.18.25`, kimi-code `0.41.0`);
   do not restore OpenCode v1.18.29 until the documented upstream regressions
   are fixed.

### Active Canonical Runtime and Integration Follow-ups

- Synchronous pre-execution sanitization (work item #109 scope B) remains
  unaddressed; the thread-start event is best-effort notification only.
- Production coordination-knowledge activation (sunburst/ruffles operator
  actions) and the automated live matrix remain operator/tracking follow-ups.
- The desktop voice round-trip (real transcription + synthesis) still needs a
  live tenant before desktop goal sign-off.
- A live OpenRC kawarimi rehearsal on a production-class host would confirm
  the WI#133/#134 respawn window stays closed at import time; the fix was
  live-verified on this box with a fresh test tenant.
- WI#136's FTS nonce-tokenizer follow-up (Part 4) remains out of scope,
  tracked as a separate work item; mission-goal isolation itself is already
  closed on the write/purge side.

### Security, Deployment, and Release Limitations (carried)

- DarkIRC Secure Key Exchange Stages B–D and automated QR/web/trusted-channel
  delivery remain disabled behind separate acceptance gates.
- The onboarding web app remains a trusted local console without user accounts
  or TLS.
- OpenRC hosts without usable cgroup v2 delegation fall back to nice-only build
  scheduling.
- `ALLOW_PRIVATE_IPS=1` opts out of public-address SSRF restrictions.
- GitLab CI can build and validate but does not publish daemon packages or
  deploy tenants automatically; mt-admin still performs tenant-local
  activation.
- Kawarimi remains PostgreSQL-only.

### Resolved or Source-Fixed Since v2.1.0.0

- Engine hardening for memory authenticity and index desync (work item #132):
  retrieval neutralization and labeling, forced trusted provenance on
  `__save_memory_doc__`, System-role seam closure, and the V27 outbox-based
  index self-heal sweep.
- Priority `/interrupt` overtaking a queued prompt now cancels the prompt in
  its lane and acknowledges (goal #9 race).
- Knowledge seeder now supports additive refresh via the cadence runner
  (work item #121).
- Coordination-knowledge `KNOW:` wire path live-verified on isolated XMPP
  tenants (work item #97).
- Pebble worker restart parity on `mt-admin` stop/start bounce (work item
  #128); a tenant bounce no longer leaves the worker dead.
- SSE/WebSocket `stream_gap` explicit signaling with browser history reload,
  and terminal `Rejected` status for hook-suppressed responses so clients
  never hang in a live-work state (work item #127).
- Mission goal documents (whole `engine/projects/*/missions/*` subtree and
  the `missions.json` manifest) no longer enter the search index, legacy
  chunks are purged at boot and on the hygiene pass, and `engine/**` search
  results carry an `engine_internal` provenance label (work items #135/#136).
- OpenRC writer quiescence verified at stop time: WeeChat status wording
  fixed, bounded post-shutdown quiesce poller with fail-loud contract, and
  the kawarimi loader quiesces writers before the PostgreSQL-only populate
  window; live-verified on a real OpenRC host (work items #133/#134/#137).
- LunarVoice sidecar supports NVIDIA NeMo Parakeet TDT 0.6B int8 STT via
  `nemo_transducer` model type plus sherpa-onnx v1.12.15 runtime, with a
  host-side model patching runbook; live-verified on three tenants.
- Worker connect-failure heuristic narrowed to protocol-mismatch statuses only
  (MR !893 follow-up).
- Gateway voice `/api/voice/*` routes with server-side token holding (work
  item #131).
- Best-effort `engine/thread_start` event plumbing (work item #109 scope A).
- CI/CD validator gaps: worker pins, classified deliverables, and blocking
  clippy/architecture ratchets (goal #32 follow-up).
- Contributor onboarding guide added to `docs/guides/` (goal #34).

---
