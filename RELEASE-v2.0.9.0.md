# Release Notes for LunarWing v2.0.9.0

## Codename: Inazuma (稲妻)

**Release date:** 2026-08-29

>The sky cracks white and we move as one

>Kiyome washed the blade clean

>Now the storm arrives to strike where polishing never could

>One flash of consolidation across the codebase

>The storm after the cleansing

>Lightning

>⚡

[](https://files.catbox.moe/rmbmci.jpg)
## Overview

LunarWing v2.0.9.0 is a tool-output security, self-repair reliability,
least-privilege, and tenant-operations release. It closes tool-result safety
bypasses exposed by work item #75, including the Engine V2 structured-JSON path;
makes the repair watchdog stop retrying terminal failures while recognizing
healthy built-in tools; seeds newly provisioned Study Notes tenants once without
touching upgraded tenants; warns about host and registry port collisions; and
prevents mission threads from being offered `create_job` when their capability
leases do not permit it.

A second wave of changes hardens Engine V2 and the channel relays themselves:
Engine V2 starts eagerly with the daemon instead of waiting for chat traffic,
independent gateway threads gain bounded per-thread concurrency instead of
head-of-line blocking, foreground threads honor the configured tool-iteration
limit, and wedged async tool or LLM futures time out instead of hanging a
thread forever. The WeeChat relay adapter no longer loses inbound messages
around reconnects, buffers outbound input during relay outages, and keeps its
recovery state across process restarts. The SSH management API now requires
bearer authentication, and the webhook server binds to loopback by default.

The release and tag use `2.0.9.0`; LunarWing-owned Rust crates and channel
packages use `2.0.9`. Rust 2024 and MSRV `1.96.1` are unchanged. There are no
new SQL migrations (`V26__study_notes_knowledge.sql` remains latest) and no new
multi-tenant port-registry migration (v16 remains current).

See the [v2.0.8.0 release notes](docs/releases/RELEASE-v2.0.8.0.md) for the
previous compatibility baseline.

## Highlights

- **Tool-result integrity across both agent engines.** Oversized output is now
  truncated before leak, policy, and prompt-injection scanning; tool failures go
  through the same sanitize-and-wrap path as successes; and ChatML-family role
  token openers such as `<|im_start|>` are neutralized unconditionally.
- **Engine V2 defense in depth.** Structured JSON successes are neutralized
  without losing JSON shape, failures are sanitize-wrapped, and every action
  result is neutralized again at the history-formatting sink. The sink also
  repairs unsafe role-token openers in previously persisted action history when
  that history is formatted again.
- **Self-repair converges instead of looping.** A tool in terminal
  `ManualRequired` state is surfaced once per boot and then removed from the
  retry set; repair-attempt counters start fresh once per daemon boot; healthy
  built-in tools self-verify without being sent to the WASM builder; and a
  successful tool call clears stale failure and repair counters.
- **Discriminated-union WASM tools advertise usable schemas.** If a compact
  advertised schema keeps the action discriminator but omits required fields
  from action variants, LunarWing now includes the `tool_info` hint. This fixes
  action-only calls such as `gitlab_tool` requests that omitted `project`.
- **One-shot Study Notes bootstrap for new tenants.** A tenant created with
  `add-tenant --with-study-mcp` can import the synced lw2studymcp note corpus
  into its private knowledge store on first startup, preserving canonical
  provenance and lifecycle metadata. Existing content is never overwritten,
  completed seeds do not repopulate, and upgrading an existing tenant does not
  arm the seed.
- **Port-collision visibility.** Fresh allocations warn when any assigned port
  already has a loopback listener, and `mt-admin doctor` reports duplicate port
  assignments inside the registry. Both checks are diagnostic and non-fatal;
  the allocator's stdout contract remains the bare base port.
- **Lease-scoped mission actions.** Engine V2 execution prompts expose only
  actions covered by the thread's valid capability leases. Mission and other
  restricted threads therefore no longer see or attempt `create_job`, while
  foreground threads retain it and late tool activation still works.
- **Bounded concurrency for independent gateway threads.** Engine V2 gateway
  input is scheduled per `(channel, authenticated user, conversation scope)`:
  up to four distinct threads for one user run concurrently, same-thread and
  non-gateway paths stay serialized, and the bounded queue answers overflow
  with an explicit busy response instead of stalling daemon-wide ingestion.
- **Engine V2 threads cannot hang forever.** Every deferred tool or LLM future
  in a ResolveFutures round is bounded by `tool_async_timeout_secs` (default
  300s, overridable with `ENGINE_V2_TOOL_ASYNC_TIMEOUT_SECS`); on expiry the
  wedged task is aborted and the call resolves as a normal failure. Foreground
  threads also honor `AGENT_MAX_TOOL_ITERATIONS` instead of a hardcoded 50, and
  Engine V2 now initializes eagerly at agent startup so scheduled missions,
  routines, thread recovery, and the mission webhook endpoint work immediately
  after a daemon restart.
- **Orchestrator rejects invented tool success.** Text and provider-mangled
  `FINAL(...)` responses that claim completed memory/tool work without a real
  call are rejected and retried, and empty `FINAL` pseudo-actions can no longer
  synthesize a successful "Task completed" response.
- **WeeChat relay stops losing messages around reconnects.** WebSocket ERROR
  frames no longer masquerade as clean disconnects, outbound operator input is
  buffered in a bounded outbox during relay outages, lines the relay accepted
  while the socket was down are backfilled after reconnect, and recovery state
  (line watermarks plus an inbound journal) survives adapter restarts.
- **SSH API fenced and webhook server loopback by default.** Every SSH host and
  key-management route now requires `Authorization: Bearer <SSH_API_TOKEN>` and
  is not mounted at all when the token is unset, and the webhook server's
  fallback bind changed from `0.0.0.0` to `127.0.0.1`.
- **SSH key rotation now fails closed.** `configure-ssh` backfills the dedicated
  SSH API token for older tenants, reloads already-running daemons before upload,
  and retains staged keys while stopping startup on any upload failure instead
  of silently continuing with the old signing key.
- **Safer default workspace guidance.** Profile updates now explicitly require
  `memory_write` with `append: false` and a complete merged JSON document, daily
  logs point to `daily/YYYY-MM-DD.md`, and the memory tool warns that appending
  to JSON can corrupt the document.

## Compatibility

| Area | v2.0.9.0 behavior |
|---|---|
| Release version | `2.0.9.0` |
| Crate/package version | `2.0.9` for LunarWing-owned release packages |
| Rust edition / MSRV | Rust 2024 / `1.96.1` |
| New SQL migration | None; `V26__study_notes_knowledge.sql` remains latest |
| Multi-tenant port registry | v16; no schema change from v2.0.8.0 |
| Channel registry patches | darkirc 0.1.6, multica 0.1.8, weechat 0.2.7, xmpp 0.1.11 |
| Tool registry patches | No change from v2.0.8.0 (GitLab remains 0.1.3) |
| Worker image moving tag | `v2.0.9.0` (plus `latest` and immutable commit-SHA tags) |
| CI scope | Source verification; no package publication or tenant deployment |

## Changes

### Tool-Result Safety and Role-Token Neutralization

Work item #75 began with a report of mangled, repeated error output containing a
SYSTEM-like ChatML sequence. The exact model-side degeneration was not reliably
reproducible, but the audit found deterministic safety gaps in LunarWing's
handling of error and oversized tool output.

- `SafetyLayer::sanitize_tool_output` now applies its configured length bound
  first and then runs secret-leak detection, policy enforcement, and injection
  scanning over the bounded result. Output larger than 100 KiB can no longer
  bypass all safety checks merely because it is large.
- `wrap_for_llm` always changes the special-token opener `<|` to the visually
  equivalent `<\u200B|` form. This is independent of the optional injection
  scanner and breaks the contiguous token recognized by ChatML-family
  tokenizers. The transform is idempotent and reversible by the existing unwrap
  path.
- The v1 chat dispatcher and shared `process_tool_result` helper now sanitize and
  wrap tool errors just like successful results. That covers worker jobs,
  container execution, and approval-resume callers of the shared helper.
- Engine V2's bridge neutralizes sanitized content before parsing successful
  JSON, preserving `serde_json::Value` structure while removing intact role
  delimiters. Its error path now sanitize-wraps the error text.
- Engine V2's `format_action_output` applies the same neutralization before
  truncation for every action-result shape. This second boundary protects
  internal and bridge results and cleans unsafe openers from old persisted
  action history when reused.

A second audit wave (work items #79 and #80) closed every remaining sink
through which retained or external content could carry intact role tokens into
an LLM-bound prompt:

- Retrieved memory documents are neutralized in both engines — the Rust
  context builder neutralizes its complete rendered memory section, and the
  compiled Python orchestrator applies an equivalent shared helper across
  memory-document type, title, and content fields.
- `wrap_external_content` neutralizes the complete wrapper, including untrusted
  source and body fields, in addition to its delimiter defense.
- Active skill content and metadata, dynamic CodeAct prompts (including learned
  overlays), assembled system prompts, and mission meta prompts pass through
  the same neutralizer.
- Legacy `ChatMessage::tool_result` values are neutralized and the provider
  boundary sanitizes tool messages, so every v1 chat tool result is covered.
- The Python orchestrator's `format_output` neutralizes the complete rendered
  CodeAct step blob (stdout, action previews, action errors, return values)
  before front-truncation, and persisted User-role history blobs are repaired
  when they are re-rendered. Truncation can never cut an intact opener back out
  of a neutralized marker because the neutralizer itself is idempotent.

The software-builder loop remains a narrower exception: it handles
host-internal read/write/shell tools and host-generated errors without threading
through a `SafetyLayer`. It is not an equivalent untrusted WASM/MCP result path,
but unifying it is still a possible follow-up.

Evidence and detailed threat analysis:

- [`docs/ops/v2.0.9.0_GOAL1_TOOL_RESULT_INTEGRITY.md`](docs/ops/v2.0.9.0_GOAL1_TOOL_RESULT_INTEGRITY.md)
- [`docs/ops/ROLE_TOKEN_SMUGGLE.md`](docs/ops/ROLE_TOKEN_SMUGGLE.md)

### Self-Repair and Tool Schema Reliability

Work item #73's four reported failure modes are addressed, and work item #76's
report of the watchdog retrying `ManualRequired` tools forever is fully
closed:

- Exhausted `ManualRequired` repairs emit one warning and owner alert per boot,
  then leave the active repair set instead of logging and retrying every
  minute. The terminal filter now covers **every** `ManualRequired` outcome,
  including outcomes that consume no repair attempt because the WASM builder or
  failure store is unavailable (the builder is disabled by default); those
  previously re-warned and re-alerted every repair cycle on default-config
  hosts.
- `ToolFailureStore::reset_tool_repair_attempts` resets repair-attempt counters
  at daemon startup, providing a fresh bounded cycle after a restart without
  erasing failure history.
- Repair recognizes registered non-WASM built-ins. If a tool such as
  `memory_read` is registered and functional, self-verification clears the
  broken flag instead of invoking a WASM builder that cannot rebuild it.
- Successful tool execution records last-known-good state and resets both the
  error count and repair-attempt count while retaining history.
- WASM schema advertisement detects when a compact discriminated-union schema
  omits variant-required fields and appends a `tool_info` hint. This preserves
  full invocation guidance for tools such as `gitlab_tool` without expanding
  every schema unconditionally.

### New-Tenant Knowledge Store Seeding

`add-tenant --with-study-mcp` now does more than configure a read-only MCP
server: it arms a one-shot import into that newly created tenant's private,
native knowledge store.

- `KNOWLEDGE_SEED_FROM_STUDY_MCP=1` is written only by the new-tenant
  `--with-study-mcp` provisioning path. `configure-study-mcp`, routine env
  regeneration, and `upgrade-tenant` do not newly enable it.
- Startup imports valid notes from
  `$LW2STUDYMCP_HOME/agent-notes-by-agent/<agent>/<topic>.md` in a background
  task and preserves status, confidence, study time, source commit, tags, stale
  threshold, status reason, and supersession metadata.
- A `knowledge_seed_state.json` marker records completion. Later boots skip the
  pass, including when the shared checkout gains more notes.
- Existing note paths are not overwritten. Recovery is limited to rows stranded
  by the seed's own interrupted write sequence, and the reserved `coord-*`
  namespace is never imported.
- Live verification covered both primary init families: a fresh OpenRC tenant
  and a fresh systemd tenant were populated, while tenants created on an older
  branch and then upgraded remained unseeded with their prior workspace state
  unchanged.

Evidence:

- [`docs/ops/v2.0.9.0_GOAL3_KNOWLEDGE_SEED.md`](docs/ops/v2.0.9.0_GOAL3_KNOWLEDGE_SEED.md)
- [`docs/ops/v2.0.9.0_GOAL4_KNOWLEDGE_SEED_SYSTEMD.md`](docs/ops/v2.0.9.0_GOAL4_KNOWLEDGE_SEED_SYSTEMD.md)

### Multi-Tenant Port-Collision Diagnostics

Work item #65 closes an observability gap without changing allocation semantics:

- After allocating a fresh tenant block, mt-admin probes every numeric assigned
  port in the primary, extended, Kimi, and voice blocks on loopback. A listener
  produces a precise stderr warning naming the tenant, slot, and port.
- The probe validates numeric values before using them and never changes the
  allocator's stdout, so existing command substitutions continue receiving only
  the base port.
- A read-only registry scan finds ports assigned to more than one tenant slot,
  including the host-global `health_metrics` slot.
- `mt-admin doctor` includes a `no duplicate ports in registry` check.

These are warnings, not reservations. A process can still bind after the probe,
and an existing collision must still be resolved by the operator.

### Engine V2 Capability-Lease Enforcement

Engine V2 now separates two action catalogs that previously shared one view:

- Execution prompts and action lookup use the active thread's valid leases and
  expose only actions those leases cover.
- `ThreadManager` retains an unscoped registered-action catalog only for
  thread-type-aware lease planning and late-tool provisioning.

This prevents mission threads from seeing and attempting the administrative
`create_job` action when their lease intentionally excludes full job containers.
Foreground access, registered late tools, and thread-type-aware lease planning
remain intact.

### Engine V2 Startup, Scheduling, and Bounds

Work items #69, #85, and #88, plus stress finding F3, harden Engine V2's
runtime behavior:

- Engine V2 initializes eagerly at agent startup instead of lazily on the
  first engine-routed chat message. After a daemon restart the cron ticker,
  terminal-event listener, thread recovery, and mission bootstrap are live
  immediately, and `POST /api/missions/webhook/{path}` no longer returns
  "engine v2 is not running" until chat traffic arrives. Handlers keep their
  lazy fallback for direct engine use outside the agent loop.
- A `MessageScheduler` replaces the single global active-turn slot. Scoped
  gateway user input is keyed by `(channel, authenticated user, conversation
  scope)`, so independent threads proceed in parallel — up to four distinct
  threads per user — while same-thread, non-gateway, and control traffic
  stays serialized in FIFO order. Exclusive messages and cross-user arrivals
  act as fairness barriers, the waiting queue is bounded at 256 with an
  explicit busy response, supervisor drop guards abort detached inner turns,
  and abandoned Engine V2 threads are cancelled during shutdown. This removes
  the daemon-wide ingestion stall and cross-thread head-of-line blocking
  observed in the v2.0.9.0 systemd stress run.
- Foreground Engine V2 threads (gateway, XMPP, DarkIRC, WeeChat) now receive
  the daemon's resolved `AGENT_MAX_TOOL_ITERATIONS` instead of the engine's
  hardcoded default of 50, which was surfacing as a spurious max-iteration
  error on high-iteration deployments. Mission and research thread defaults
  are unchanged.
- Every pending tool or LLM future awaited in a ResolveFutures round is
  bounded by `ThreadConfig::tool_async_timeout_secs` (default 300 seconds;
  env override `ENGINE_V2_TOOL_ASYNC_TIMEOUT_SECS`). A wedged task is aborted
  with a structured warning and the deferred call resolves as a normal
  failure instead of hanging the engine thread forever.

### Orchestrator Tool-Result Integrity

OpenRC stress findings F4/F5 exposed two related model-output failures: models
occasionally narrated memory operations that were never dispatched, and some
OpenAI-compatible providers surfaced CodeAct `FINAL(...)` syntax as structured
tool calls that then failed lease checks.

- The compiled orchestrator detects high-confidence fabricated memory/tool
  success reports when no action ran in the current user turn. It appends a
  bounded recovery instruction requiring real structured or CodeAct calls rather
  than accepting the self-report.
- Structured `FINAL`, `FINAL_VAR`, and observed provider-mangled forms are
  consumed before action history. Real calls in mixed batches remain intact;
  pseudo-actions no longer create `No lease for action 'FINAL'` failures.
- Normalized `FINAL(...)` answers pass through the same fabricated-success guard
  before terminal completion, closing the bypass where provider normalization
  could otherwise turn an invented result into an accepted final answer.
- Bare or empty `FINAL` calls no longer invent `Task completed.`. They become an
  empty text response and follow normal action-obligation/recovery handling.

This is guardrail hardening, not proof that an LLM will always dispatch requested
work. Workflows that require durable writes should still read the target back.

### WeeChat Relay Resilience

Work item #78 and stress-finding follow-ups take the Python relay adapter
(`lunarwing_weechat_wss/weechat_relay/ws_adapter.py`) from 0.2.0 to 0.4.2:

- WebSocket ERROR frames are handled separately from clean closures: they are
  logged at WARNING with the relay's exception payload, and the reconnect
  path now actually waits the advertised 5 seconds instead of retrying
  immediately.
- Outbound `/api/input` posts made during a relay outage are queued in a
  bounded outbox (100 entries, oldest drop counted) and flushed in order
  after the next successful connect. Enqueueing returns 202 with the queue
  depth, and non-retryable 4xx responses are dropped with a warning rather
  than poisoning the queue.
- A post-reconnect recovery pass backfills lines the relay accepted while the
  socket was down — WeeChat does not replay sync events after a reconnect,
  so those lines were previously lost forever. Per-buffer line watermarks
  persist across adapter process restarts, and inbound events are journaled
  so unpicked lines also survive a restart.
- `/api/health` now exposes `last_disconnect` (type, reason, UTC timestamp),
  `reconnect_count`, and the outbox depth/drop counters so hosts can monitor
  reconnect behavior.
- The aiohttp protocol heartbeat is opt-in via `WS_ADAPTER_HEARTBEAT_SECONDS`
  because WeeChat 4.7.x/4.10.x never answers WebSocket PINGs; the previous
  forced-PING behavior caused constant reconnect churn.

A live OpenRC stress run against a throwaway tenant exercised ERROR frames,
adapter restarts, and relay restarts; lines that previously vanished now
arrive exactly once through the backfill and journal paths.

### SSH API and Webhook Server Hardening

A security review (finding "wrench #2", MEDIUM, plus a log-exposure pass)
closed the last unauthenticated route family on the shared webhook server:

- Every SSH host- and key-management route requires
  `Authorization: Bearer <SSH_API_TOKEN>` with constant-time comparison. When
  the token is unset the routes are not mounted at all (a fail-closed 404)
  and a warning is logged. Previously anyone able to reach the webhook port
  could enumerate SSH hosts, register arbitrary ones, overwrite stored keys
  (which hot-load into the agent), or delete keys.
- The webhook server's fallback bind changed from `0.0.0.0:8080` to
  `127.0.0.1:8080`; a non-loopback bind now only happens when an operator
  sets `HTTP_HOST` explicitly (the startup warning is retained).
- mt-admin generates and persists `SSH_API_TOKEN` in each tenant's
  `lunarwing.env` and sends it on the staged SSH-key upload and its readiness
  probe.
- `configure-ssh` and staged-key startup preserve or atomically backfill a
  missing token for older tenants. If the daemon is already active, mt-admin
  reloads it before upload so the fail-closed router sees the new credential.
- Missing/empty credentials, readiness failures, transport failures, and rejected
  upload responses now return nonzero, retain the staged key, and stop tenant
  startup before workers continue with an old signing key.
- The bearer token is supplied through an anonymous curl config FD, private-key
  JSON is built with `jq --rawfile`, and endpoint failure bodies are suppressed,
  keeping token/key material out of process argv and operator logs.
- Empty configured tokens now leave the SSH API router unmounted, matching the
  absent-token fail-closed behavior.
- SSH key-upload secrets no longer appear in tracing spans or logs. See
  [`docs/ops/SSH_KEY_LOG_EXPOSURE_KAGEHO.md`](docs/ops/SSH_KEY_LOG_EXPOSURE_KAGEHO.md).

### Gateway, Workspace, and Memory-Search Accuracy

Smaller correctness fixes verified against the live stress findings:

- `POST /api/chat/send` returns 400 for a missing or blank `thread_id`
  instead of accepting work whose reply could never be routed.
- `/api/memory/search` now returns each result's `document_path` instead of
  an opaque document UUID.
- `Workspace::normalize_path` lexically resolves `.` and `..` segments and
  clamps parent traversal at the virtual workspace root, preventing
  traversal-shaped entries in the document database.
- Concurrent chunk replacements for one workspace document are serialized per
  document, fixing intermittent 500 responses when two memory writes landed
  on the same file inside the chunker window.
- Startup sandbox warnings are routed through the configured owner scope, so
  WASM channels resolve persisted qualified targets before guest broadcast
  callbacks instead of logging invalid-target errors at boot.

### Multi-Tenant Service-Status Accuracy

mt-admin no longer reports a live custom OpenRC service (notably WeeChat) as
`unknown`: a successful custom `status` command is classified by its exit
code as `started`, an explicit not-running response maps to `stopped`, and
only unrecognized failures remain `unknown` — so post-provision verification
and health checks see real state on OpenRC hosts.

Channel health now also reflects the daemon's real delivery state instead of
the mere presence of a registered wrapper:

- failed channel startup remains unhealthy until the channel is replaced or
  starts successfully;
- WASM guests explicitly mark operational callback failures with a `[health]`
  warning/error prefix (ordinary diagnostic warnings do not flap health);
- three consecutive poll or dispatch health failures mark a channel unhealthy,
  and a clean poll restores it;
- DarkIRC adapter connectivity/protocol failures use that explicit signal; and
- native XMPP health tracks its bound-session state, with an exit guard that
  clears stale connected state whenever the background task ends.

### Workspace Prompt and Memory Guidance

The default system-prompt stack was checked against the current workspace
implementation. The runtime preamble remained accurate; the following seeded
and deployed workspace guidance changed:

- `context/profile.json` updates must use `memory_write` with `append: false` and
  contain the complete merged JSON document. The old instruction could append
  bytes to valid JSON, after which profile parsing and personalization silently
  stopped working.
- Bootstrap's profile-write step carries the same replacement requirement.
- The `memory_write.append` schema text warns against appending to any JSON
  document.
- `SOUL.md` now points daily notes to `daily/YYYY-MM-DD.md`, matching the real
  workspace and memory-tool path rather than the obsolete `memory/` prefix.
- Setup documentation correctly identifies `BOOTSTRAP.md` as a runtime seed,
  not a file shipped in `deploy/workspace-template/`.

System prompt and workspace documents were also preserved byte-for-byte during a
live `upgrade-tenant` test on systemd; the test tenant was removed afterward.
See
[`docs/ops/v2.0.9.0_GOAL16_SYSTEM_PROMPT_UPGRADE_SYSTEMD.md`](docs/ops/v2.0.9.0_GOAL16_SYSTEM_PROMPT_UPGRADE_SYSTEMD.md).

### Skill Attenuation Visibility

The reported inability to write `memory.md` without an unspecified environment
variable was not reproducible and no environment variable gates
`memory_write`. Registration, default permissions, approval behavior, routine
availability, Engine V2 exposure, gateway writes, and workspace writes are all
environment-independent.

The plausible hidden cause was confirmed as the intentional trust ceiling for an
active registry-installed skill: it can retain memory read/search tools while
attenuating `memory_write`. LunarWing now logs this per-turn tool attenuation at
`INFO` once, making the security decision visible without weakening it.

### Documentation and Release Housekeeping

- `docs/ops/` was reorganized around a maintained index and versioned history;
  completed checklists, audits, transcripts, and superseded plans moved under
  `docs/ops/history/`, and local links were repaired.
- Architecture, active bug reports, and proposals were re-verified against
  source. Corrections include Engine V2 leases, knowledge seeding, self-repair,
  XMPP attachment bounds, worker/SSH security boundaries, and the in-repo Rust
  OCR sidecar deployment path.
- LunarWing-owned crates and channel packages moved from 2.0.8 to 2.0.9;
  registry patches moved to darkirc 0.1.6, multica 0.1.8, weechat 0.2.7, and
  xmpp 0.1.11.
- Container CI's moving worker tag is now `v2.0.9.0`; `latest` and immutable
  commit-SHA tags remain in use. Worker image tool pins moved to opencode
  v1.18.23 and kimi-code 0.39.0.
- The root README was rewritten for clarity and now documents the WeeChat
  relay's reconnect recovery (outbox, backfill, health fields) and the
  `--with-study-mcp` one-shot knowledge seed. A new operator guide,
  [`docs/guides/TENANT_KNOWLEDGE_SEED.md`](docs/guides/TENANT_KNOWLEDGE_SEED.md),
  walks through seeding a new tenant from the synced lw2studymcp base and the
  correct re-arm path.
- New `docs/ops/v2.0.9.0_OPEN_WORK_ITEMS_STATUS.md` tracks each open GitLab
  work item's addressed/unaddressed state, and the OpenRC and systemd stress
  plans plus dated findings live under `docs/ops/v2.0.9.0_GOAL25_*` and
  `docs/ops/v2.0.9.0_GOAL26_*`.
- The CI architecture-boundary check passes again: the LLM module now imports
  the role-token neutralizer from the `lunarwing_safety` workspace crate
  directly rather than through the `crate::safety` re-export shim.

## Upgrade Notes

1. Back up PostgreSQL, tenant env files, `/etc/lunarwing/ports.json`, workspace
   state, rendered units, worker data, voice models, and channel state before an
   upgrade.
2. No SQL migration or port-registry migration is introduced. `V26` and port
   registry v16 remain the current formats.
3. Rebuild the daemon, WASM channels, and selected worker images through the
   normal deployment path. Registry channel versions changed, and CI worker
   images now use the moving `v2.0.9.0` tag alongside `latest` and a commit SHA.
4. Existing tenants are deliberately **not** bulk-populated from lw2studymcp.
   One-shot native knowledge seeding is armed only for a newly created tenant
   using `add-tenant --with-study-mcp`. Do not manually add
   `KNOWLEDGE_SEED_FROM_STUDY_MCP=1` to an established tenant unless that import
   is explicitly desired and its existing note paths have been reviewed.
5. Port-listener findings during a fresh allocation are warnings. Run
   `ic/scripts/lunarwing-mt-admin.sh doctor`, inspect the named listener or stale
   registry entry, and resolve the collision before relying on the tenant.
6. Tool output may now be truncated, wrapped, redacted, or role-token-neutralized
   where v2.0.8.0 exposed raw error or oversized content. Integrations must not
   depend on model-visible tool errors remaining byte-for-byte unchanged.
7. Custom workspace instructions that update `context/profile.json` should match
   the new default: send one complete JSON document with `append: false`.
8. No operator action is needed for the mission lease correction or self-repair
   counter reset. After the first v2.0.9.0 restart, repair attempts begin a new
   bounded boot cycle and restricted threads receive the corrected action list.
9. The SSH host/key API now requires `Authorization: Bearer <SSH_API_TOKEN>`.
   mt-admin provisions and persists the token in each tenant's `lunarwing.env`;
   any custom integration calling those routes must send it. When the token is
   unset the SSH routes are not mounted at all.
10. The webhook server now binds `127.0.0.1` by default. If external machines
    legitimately post webhooks to it, set `HTTP_HOST` explicitly — the previous
    implicit `0.0.0.0` exposure is gone.
11. Redeploy the WeeChat relay adapter (0.4.2) alongside the daemon so
    reconnect fixes, the outbound queue, and reconnect backfill take effect.
    Leave `WS_ADAPTER_HEARTBEAT_SECONDS` unset unless the relay is known to
    answer WebSocket PINGs.
12. Engine V2 deferred tool/LLM calls now time out after
    `tool_async_timeout_secs` (default 300). Tune
    `ENGINE_V2_TOOL_ASYNC_TIMEOUT_SECS` only if legitimate tool calls can
    exceed five minutes.

## Known Issues

This list combines the active canonical bug tracker re-verified on 2026-08-25
with release limitations carried from v2.0.8.0 and checked against the current
v2.0.9.0 draft. The canonical status and reproduction details live in
[`docs/bugs/README.md`](docs/bugs/README.md).

### Active Runtime and Integration Follow-ups

- **Synchronous worker calls still occupy their originating thread.** A
  built-in Docker `create_job` call with the default `wait=true` holds its
  originating turn until the worker result, timeout, or an interrupt;
  distinct scoped Engine V2 gateway threads for the same user now run in
  parallel (bounded to four, with an explicit busy response on overflow), but
  a later message on the same thread remains deferred. External workers
  default to `wait=false`. See
  [`BUG-agent-worker-lifecycle.md`](docs/bugs/BUG-agent-worker-lifecycle.md).
- **Some E2E coverage remains incomplete.** Bootstrap E2E scenarios are ignored
  because of a harness race, and clipboard runtime behavior remains unverified.
  See [`BUG-e2e-test-bugs.md`](docs/bugs/BUG-e2e-test-bugs.md).
- **External-worker token persistence lacks a final live re-check.** Source and
  round-trip tests preserve worker auth tokens across provisioning, merges, and
  TOML rewrites, but a live daemon restart plus authenticated worker request is
  still required before archiving the report. See
  [`BUG-external-worker-config-persistence.md`](docs/bugs/BUG-external-worker-config-persistence.md).
- **Kawarimi flag parity is incomplete.** Import still rejects an explicit
  `--with-wasm` even though the import path adds that behavior internally. See
  [`BUG-kawarimi-import-flag-parity.md`](docs/bugs/BUG-kawarimi-import-flag-parity.md).
- **SSH Git can accept an empty checkout after a bare-remote HEAD mismatch.**
  Null-like refs are fixed, but the remaining mismatch is not detected as a
  structured failure. See
  [`BUG-ssh-git-ref-and-remote-head.md`](docs/bugs/BUG-ssh-git-ref-and-remote-head.md).
- **WeeChat's `rand_check` is still a stub.** The unused import was fixed, but
  the function always returns false and should be implemented or removed. See
  [`BUG-weechat-relay-rand-check.md`](docs/bugs/BUG-weechat-relay-rand-check.md)

### Security, Deployment, and Release Limitations

- DarkIRC Secure Key Exchange Stages B-D and automated QR/web/trusted-channel
  key delivery remain disabled. Stage A acceptance does not imply that contact
  security is fully activated.
- MCP does not automatically install npm/pip packages, has no signed-package
  marketplace install/upgrade CLI, and does not deliver configured MCP servers
  to `lunarwing-agent-v1` external workers. Custom HTTP MCP header values are
  still stored directly rather than through owner-scoped secret references.
- The web UI still puts its bearer token in native `EventSource` query strings.
  Protect browser history and access/proxy logs until short-lived SSE tickets or
  cookie-scoped sessions replace that transport.
- Onboarding remains a trusted local console without user accounts or TLS. Keep
  it loopback-bound with token authentication and use an SSH tunnel for remote
  access.
- OpenRC hosts without unified cgroup v2 delegation fall back to nice-only build
  bounding rather than hard memory, CPU, and task ceilings. mt-admin reports
  the fallback reason.
- `ALLOW_PRIVATE_IPS=1` weakens public-address SSRF protections. Enable it only
  for an intentionally trusted private-network deployment
- `cargo check --tests --no-default-features --features postgres` retains the
  pre-existing cfg(test) failures described in the v2.0.8.0 notes; CI does not
  run that exact feature/test shape. PostgreSQL production check coverage and
  default/libSQL test coverage are separate jobs.
- Kawarimi remains PostgreSQL-only.
- GitLab CI verifies source but does not publish release packages or deploy
  tenants automatically.
- Large worker-image pushes can still fail with registry `499 Client Closed
  Request` until proxy-chain read timeouts are raised on the GitLab host. See
  [`docs/ops/REGISTRY_PUSH_499.md`](docs/ops/REGISTRY_PUSH_499.md).

### Resolved Since v2.0.8.0

- Oversized tool output no longer skips safety scanning, and tool failure text no
  longer bypasses sanitize-and-wrap handling in the chat/shared execution paths.
- Engine V2 structured JSON and action history no longer preserve intact
  ChatML-family role-token openers from tool output.
- Retrieved memory documents, external-content wrappers, active skills,
  CodeAct and assembled system prompts, and mission meta prompts no longer
  carry intact role tokens into LLM-bound prompts (work items #79/#80).
- Terminal `ManualRequired` repairs no longer retry forever, repair counters no
  longer remain exhausted across daemon restarts, and healthy built-in tools are
  no longer sent to the WASM repair builder.
- Fresh tenant allocation now reports occupied host ports, and `mt-admin doctor`
  reports duplicate assignments in the registry.
- Mission threads no longer see `create_job` when their capability lease denies
  it.
- The reported environment-variable dependency for `memory_write` was ruled
  out; intentional installed-skill attenuation is now visible at info level.
- Engine V2 no longer starts dormant: missions, routines, thread recovery, and
  the mission webhook endpoint are live immediately after a daemon restart,
  and independent gateway threads no longer head-of-line block each other.
- Foreground Engine V2 threads honor `AGENT_MAX_TOOL_ITERATIONS`, and wedged
  deferred tool/LLM futures are aborted by `tool_async_timeout_secs` instead
  of hanging a thread forever.
- Fabricated memory/tool success reports and normalized `FINAL(...)` pseudo-calls
  no longer bypass action execution, enter leased action history, or synthesize
  successful completion from an empty answer.
- The WeeChat relay no longer drops lines accepted during socket outages or
  adapter restarts, no longer retries instantly after ERROR frames, and no
  longer churns reconnects against WeeChat versions that ignore WebSocket
  PINGs.
- The SSH host/key API is bearer-authenticated and fail-closed, key-upload
  secrets no longer reach argv/logs, key rotation failures retain staged keys
  and stop startup instead of silently keeping the old key, older tenants
  backfill the dedicated token safely, and the webhook server defaults to a
  loopback bind.
- `memory_write` no longer intermittently returns 500 when two updates reach
  the same workspace document concurrently, workspace paths containing `..`
  no longer create traversal-shaped database entries, `/api/memory/search`
  returns usable document paths, and blank-`thread_id` chat sends are
  rejected with 400 instead of being silently unroutable.
- mt-admin reports live custom OpenRC services as `started` instead of
  `unknown`.
