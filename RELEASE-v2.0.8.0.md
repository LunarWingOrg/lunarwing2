# Release Notes for LunarWing v2.0.8.0 (FIRST DRAFT): Codename `Sekisho (関所)`

** Release Date: ** TBD (draft 2026-08-22)

> *Sekisho* (関所) were the checkpoint stations of Edo-period Japan, where
> travelers, documents, and goods were inspected before being allowed to
> pass. LunarWing v2.0.8.0 posts checkpoints over its own borders: signed
> marketplace trust gates and revocation enforcement for MCP packages, hard
> cgroup boundaries between tenants, health-gated sidecars, and fail-closed
> activation paths that stop suspect cargo at the barrier. This is a first
> draft; expect revision before the release ships.

## Overview

LunarWing v2.0.8.0 is a multi-tenant hardening, MCP marketplace-security, and
agent-resilience release built on v2.0.7.0 (`Mihari`). It brings hard cgroup
v2 resource ceilings and automated subtree delegation to OpenRC/Gentoo tenant
hosts, wires the MCP Part 2 trust stack (publisher signatures, dependency
locks, revocation distribution) into real activation paths, streams SSE
through the bundled MCP gateway, makes every agent termination path tell the
model what would unlock the call, adds opt-in cross-tenant knowledge sharing
over the coordination pipeline, and adds a Prometheus healthpoint metrics
exporter with an early-warning threshold layer for host health.

The release and tag use `2.0.8.0`; LunarWing-owned Rust crates and channel
packages use `2.0.8`. Rust 2024 and MSRV `1.96.1` remain unchanged. There are
no new SQL migrations — `V26` remains the latest — while the multi-tenant
port registry advances to v16 for a host-global health-metrics slot.

See the [v2.0.7.0 release notes](docs/releases/RELEASE-v2.0.7.0.md) for the
previous compatibility baseline.

## Highlights

- Automated OpenRC/Gentoo tenant cgroup isolation: `mt-admin setup-cgroups`
  renders boot-time pre-delegated cgroup v2 subtrees and a PAM login hook,
  `add-tenant`/`remove-tenant` keep them in sync, and tenant builds run
  bounded (memory.high throttle, memory.max OOM kill, capped swap) via a
  root-side `setpriv` migration — no tenant self-migration or chown needed.
- OpenRC tenants now get hard cgroup v2 memory/CPU/task ceilings (work item
  #54), closing the gap where only systemd had hard enforcement; hosts where
  delegation is denied fall back to nice-only build bounding with a loud
  reason.
- Missions gain a creation-time daily thread budget (`max_threads_per_day`,
  default 10 kept as a cost guardrail, 0 = unlimited), remaining-budget
  visibility on CLI and web list/detail surfaces, warn-level exhaustion logs,
  and precise not-fired reasons (work item #56).
- Mission triggers are now deduplicated at the engine: a persisted,
  FIFO-bounded `processed_trigger_keys` set skips already-processed payloads
  before any threads or budget are spent, so daemon restarts and listener
  re-deliveries no longer re-fire stale missions (work item #69).
- `SKILL_SELF_IMPROVEMENT=0` now really means disabled: skill-extraction and
  skill-maintenance missions are gate-paused at bootstrap/reconcile, their
  triggers are suppressed, and re-enabling resumes exactly the gate-paused
  missions while operator pauses stay paused (work item #69).
- MCP Part 2 trust stack is integrated, not just library code: ed25519
  publisher signatures, signed dependency lockfiles, and signed revocation
  lists are enforced fail-closed on config load and startup activation; the
  bundled gateway control plane can be enabled at startup; generic external
  runtime descriptors are registerable via `mcp add --transport external`.
- MCP Part 3 hardens the bundled gateway data plane: true SSE streaming
  passthrough, separate transient/stream permit pools, request-body read
  deadlines, upstream userinfo rejection and log sanitization, and stricter
  bearer-token validation.
- Agent failure recovery with next steps: recoverable provider failures get
  one bounded recovery turn, the iteration limit gets one text-only final
  turn, denied approvals resume the loop with per-call denial results, and
  disabled/not-found/auth-required/rate-limited errors all state what unlocks
  the call.
- Engine V2 now persists an explicit "always" approval at tool level even
  when the approved invocation fails (ssh excepted), so flaky tools like the
  GitLab WASM tool stop re-prompting on every call (work item #70).
- WeeChat outbound integrity fix: multi-line tool results and agent replies
  no longer lose everything after the first newline, and lines starting with
  `/` are escaped instead of being executed as WeeChat commands in the
  operator's client (work item #66).
- XMPP image attachment fix: the bridge's `/v1/messages/send` endpoint was
  silently bounded by axum's 2 MiB default body limit, so any base64-encoded
  attachment larger than ~1.4 MiB raw was rejected with HTTP 413 before the
  send handler ran — agents could not attach and send ordinary photos. The
  send body limit is now an explicit 70 MiB, sized to the host's 50 MiB
  outbound attachment cap plus base64 inflation, scoped to the send route
  only (every other route keeps the 2 MiB default), with bearer
  authorization running as middleware before any request body is buffered
  (work item #42).
- Healthpoint metrics exporter: the host health pipeline now emits a
  Prometheus textfile exposition of the aggregated report and self-heal
  state after every check, with per-check durations, a graduated
  warn/crit early-warning threshold layer, a deduplicated Gotify
  notification path, and an optional loopback-only listener behind the new
  port-registry v16 `health_metrics` slot.
- Cross-tenant knowledge sharing: opt-in `KNOW: <topic> | <finding>` room
  records propagate one durable finding into every watching tenant's `shared`
  study-notes scope as `needs-verification`, behind publisher allowlists and
  ingest budgets, safe on mixed-version fleets.
- The built-in study-notes knowledge layer reaches full parity with the
  canonical seven-tool store, and mt-admin can now provision every new tenant
  with a read-only, always-current study-notes MCP server via
  `add-tenant --with-study-mcp`.
- Opencode external worker bumped to opencode `v1.18.18`, with the pinned ref
  baked into the image and exposed on the worker `/health` endpoint.
- Kimi external worker bumped to Kimi Code CLI `0.37.2` and retargeted at the
  LunarWing TensorZero gateway (`FrontierCODE` over the NetBird overlay) as
  its baked default provider — the managed `api.kimi.com` service has failed
  worker prompts since 0.37.2 — with the pinned version baked into the image
  and exposed on the worker `/health` endpoint.

## Compatibility

| Area | v2.0.8.0 behavior |
|---|---|
| Release version | `2.0.8.0` |
| Crate/package version | `2.0.8` for LunarWing-owned release packages |
| Rust edition / MSRV | Rust 2024 / `1.96.1` |
| New SQL migration | None; `V26__study_notes_knowledge.sql` remains latest |
| Multi-tenant port registry | v16 — new host-global `health_metrics` slot (default port 50000, above all tenant ranges); auto-migrates from v15 |
| MCP negotiated version | Client requests `2024-11-05` and also accepts `2025-03-26` from servers that answer it; anything else fails before activation |
| Channel registry patches | darkirc 0.1.5, multica 0.1.7, weechat 0.2.6, xmpp 0.1.10 |
| Tool registry patches | gitlab 0.1.3 |
| LunarVision health | Dedicated operational listener (`8089` single-node); MT hosts discover per-tenant `vision_health` from the registry |
| LunarVoice health | Dedicated operational listener (`8091` single-node); starts fail loudly if the sidecar never goes healthy |
| CI scope | Source verification; no package publication or tenant deployment |

## Changes

### Multi-Tenant OpenRC/Gentoo Cgroup Isolation

- New `mt-admin setup-cgroups` command automates the pre-delegated subtree
  design from `docs/ops/PREDELEGATED_SUBTREE_CGROUPS.md`: a boot-time
  `/etc/local.d/lunarwing-cgroups.start` rendered from the ports registry and
  a `pam_exec` login hook wired into `system-login`/`sshd`/`su`/`login`
  (deliberately never `sudo`). `add-tenant` and `remove-tenant` keep subtrees
  and the boot script in sync automatically.
- Tenant builds are placed in the tenant's `build` cgroup via a root-side
  `setpriv` migration behind a bounded go-file rendezvous; bounding works on
  every OpenRC host regardless of PAM configuration. Verified live: su
  sessions land in `lunarwing-<t>/session`, `sudo -u` sessions stay out,
  memory.high throttling and memory.max OOM kills (rc=137) engage, and cgroup
  cleanup propagates real exit statuses.
- Hard cgroup v2 ceilings for OpenRC tenants (work item #54): direct memory
  values are converted to bytes, startup verifies every configured effective
  limit, build setup fails safely when memory enforcement is absent, real
  build exit status 2 is never rerun unbounded, and operator conf.d overrides
  survive atomic rerenders. Hybrid cgroup mode is detected and documented;
  when self-migration is denied the build falls back to nice-only with the
  reason reported.
- mt-admin itself was refactored into a thin composition root over fourteen
  numbered `mt-admin-lib/NN-*.sh` modules (common, detect, containers,
  ports-registry, tenant-lifecycle, tenant-build, env-generation,
  units-systemd, units-openrc, worker-config, owner-scope, postgres,
  health-pipeline, cgroups-openrc). The CLI surface and entrypoint are
  unchanged; all tenant lifecycle work in this release was re-validated
  against the post-split layout.
- New tenants no longer have the legacy `IRONCLAW_BASE_DIR` and
  `IRONCLAW_SOCKET` aliases written into their generated env — only the
  `LUNARWING_*` primaries. Both legacy variables remain accepted by the
  daemon, CLI, and REPL, so existing tenant envs keep working unchanged.

### Missions

- `mission_create` accepts `max_threads_per_day` (default 10, 0 = unlimited).
- `mission_list` and the web mission list/detail surfaces expose
  `threads_today`, `max_threads_per_day`, and `budget_exhausted`.
- Budget exhaustion logs at warn with the mission name and counters (was
  debug-only and silent), and `mission_fire` reports the precise not-fired
  reason (terminal / paused / budget exhausted, noting the UTC-day reset).
- Trigger-level dedup (work item #69): each mission persists a FIFO-bounded
  (512) `processed_trigger_keys` set derived from trigger payloads
  (`src:<source_thread_id>[:<content_hash>]` / `hash:<content_hash>`), and
  `fire_mission` skips already-processed payloads before budget accounting
  and context retrieval — zero threads, zero budget, zero tokens on
  re-delivery. The key is recorded in the same durable reservation as the
  thread and rolled back on spawn failure so retries still work. The
  conversation-insights listener stamps each batch with a stable content hash
  so daemon restarts no longer re-fire stale batches.
- "Disabled means disabled" for skill learning (work item #69):
  `SKILL_SELF_IMPROVEMENT=0` now gates the skill-extraction and
  skill-maintenance (pruning) missions in addition to self-improvement —
  they are paused at bootstrap/reconcile with a `skill_learning_gate_paused`
  marker, their fires and operator resumes are rejected with the unlock
  path, and the extraction trigger is gated at the listener. Re-enabling
  resumes exactly the gate-paused missions; explicit operator pauses stay
  paused. Conversation-insights and expected-behavior missions are
  deliberately not gated.

### MCP Marketplace Trust and Gateway (Parts 2 and 3)

- Publisher signatures (ed25519 `verify_strict` over canonical manifests), a
  PublisherTrustStore, signed lockfiles pinning exact version+sha256,
  ManifestDiff upgrade diffs, and signed revocation lists with monotonic
  sequence numbers and atomic fsync persistence.
- Activation-path enforcement: the new `marketplace` section in
  `mcp-servers.json` makes every config load fail closed on revoked enabled
  servers and on corrupt trust stores/caches; startup pulls the newest signed
  revocation list over bounded HTTPS (fetch failures keep the cache) and
  skips newly-revoked packages; extension-manager activation re-checks the
  cached list per server.
- The bundled gateway control plane is enable-able at startup via
  `LUNARWING_MCP_GATEWAY_CONTROL_BIND` + `LUNARWING_MCP_GATEWAY_ADMIN_TOKEN`
  (loopback-only unless explicitly overridden); invalid config disables with
  a warning. Constant-time bearer auth, 3-header allowlist, routing-header
  stripping, redacted listings, HTTPS-only upstreams, bounded bodies/timeouts.
- External runtime descriptors: the `external_runtimes` section is validated
  at load and threaded through startup, extension-manager, and `mcp test`;
  `lunarwing mcp add --transport external` registers external servers with
  sandbox flags honored; the adapter verifies pinned provenance digests.
  `lunarwing doctor` flags unconfigured worker names and undeclared runtimes.
- Mandatory sandbox profiles: `require_sandbox_profiles` +
  `missing_mandatory_controls`, with the legacy opt-in default preserved.
- Gateway data plane (Part 3): Content-Type-keyed SSE streaming passthrough
  with no cumulative lifetime cap, clean-502 buffered responses,
  connect/time-to-headers timeouts, separate transient (64) and stream (64)
  permit pools so notification SSE cannot starve tool-call POSTs, a
  request-body read deadline (`408` on timeout), upstream userinfo rejection,
  sanitized upstream URLs in logs/client errors, a production SSE per-read
  idle timeout (default 300 s), and 1..=4096 visible-ASCII bearer tokens.
- The MCP client now accepts protocol revision `2025-03-26` in addition to
  the requested `2024-11-05`, so servers that answer with the newer revision
  (including the managed study-notes server) connect instead of failing
  activation with `Unsupported MCP protocol version`.

### Approval Persistence on Engine V2

- The engine v2 gate resume path now persists an explicit "always" grant at
  tool level regardless of whether the approved invocation succeeded (work
  item #70), matching the v1 chat path. Previously a grant was silently
  discarded whenever the call returned `is_error: true` — exactly what flaky
  tools do (GitLab REST 404s, timeouts), so "always" was indistinguishable
  from "yes" and every later call prompted again.
- ssh remains the exception: its persistent grant is still earned only after
  a genuinely successful command, and tools that disallow persistent grants
  are still filtered by `allows_always_approval`.

### GitLab WASM Tool

- The bundled GitLab tool now speaks GitLab's unified Work Items API (work
  item #60): new `get_work_item` and `list_work_items` operations cover Issue
  and Task types, and `get_issue` falls back from classic-API 404s to an
  exact Work Items IID lookup, so issues migrated to Work Items stay
  reachable. Single-item reads go through the collection `iids` filter
  because GitLab 19.2 may expose the collection while feature-gating the
  single-item REST endpoint for API tokens.
- Listing follows bounded same-origin keyset pagination with explicit
  completion data, deduplicates stable IDs, and reconciles classic issue
  totals — closing the pagination stalls and miscounts seen against large
  projects.
- The tool package, capabilities sidecar, and registry entry were bumped to
  0.1.3.

### Agent Failure Recovery With Next Steps

- New safe `recovery_kind` / `recovery_instruction` / `terminal_guidance`
  vocabulary in LLM errors; provider diagnostics are never copied into
  model-visible guidance (asserted by tests).
- The agentic loop grants one model-visible recovery turn for recoverable
  provider failures and one text-only final turn past the iteration limit;
  typed `LoopOutcome::Failed` carries terminal next steps.
- Denied approvals resume the loop with per-call denial tool-results
  (deferred batch calls resolved too) instead of ending the turn.
- Disabled, not-found, auth-required, rate-limited, and
  autonomous-unavailable errors all state what unlocks the call; hook
  rejections name the unlock path.
- External workers continue after unavailable-capability results, get a
  no-progress recovery turn (3 all-failed rounds → warning → typed failure),
  and lightweight routines mirror the one-shot provider recovery contract.

### Cross-Tenant Knowledge Sharing

- Opt-in `KNOW: <topic-slug> | <finding>` coordination records propagate one
  durable finding to every watching tenant daemon, each writing into its own
  database's `shared` study-notes scope as `needs-verification` with bounded
  sender provenance (`coord-<readable>-<96bit-hash>` agent namespace).
- Ingestion is a priority-13 fail-open hook with a publisher allowlist
  (`COORD_KNOWLEDGE_PUBLISHERS`, empty = deny all), per-sender and
  daemon-wide ingest budgets, and cancellation-safe detached ingest with
  stranded-write repair and metadata-failure rollback.
- Mixed-fleet safe: KNOW is schema-valid everywhere but emission is gated by
  `COORD_KNOWLEDGE_PUBLISH` at the prompt, tag-schema hook, message tool, and
  channel delivery boundary; knowledge tools mark coordination notes
  untrusted and wrap content before model exposure. mt-admin preserves and
  backfills all six `COORD_KNOWLEDGE_*` tenant env vars.
- Hardening follow-ups: the global ingest lock was replaced with 64
  FNV-striped per-path locks so unrelated KNOW writes ingest concurrently;
  the periodic knowledge sweep now reaps stranded coordination wire notes
  (wire namespace + sentinel + no metadata, 24h grace); a hard
  `<!-- lunarwing:coordination-wire-note -->` sentinel is written from the
  first content commit; `knowledge_list_stale` gained an
  `include_coordination=false` opt-out and `knowledge_stats` a coordination
  count.

### Study-Notes Knowledge Store

- The built-in (native) knowledge layer now ships all seven canonical tools:
  `knowledge_list_topics` (topic coverage survey) and
  `knowledge_update_status` (metadata-only lifecycle updates with canonical
  merge semantics) join the existing five, and both are registered and
  protected.
- Metadata parity with the canonical store: `stale_after_days` (default 90)
  and `status_reason` fields on every note, per-note stale thresholds honored
  by stale detection, a `min_confidence` search filter on both backends, and
  tags filters aligned to canonical "require every listed tag" semantics.
- mt-admin study-MCP provisioning: `add-tenant --with-study-mcp` wires a new
  tenant to a read-only managed `study_notes` MCP server during provisioning.
  The shared checkout (default `/var/cache/lunarwing/lw2studymcp`,
  overridable via `LUNARWING_MT_STUDY_NOTES_REPO` /
  `LUNARWING_MT_STUDY_NOTES_HOME`) is flock-serialized, cloned when missing,
  fast-forwarded when clean, and refused when dirty — so tenants always start
  with the latest upstream knowledge of the LunarWing codebase.
  `configure-study-mcp --home` is now optional, and `--sync` forces a latest
  upstream pull for an explicit home.

### Health Observability (Healthpoint Metrics)

- New `health-metrics-exporter.sh` runs from the health pipeline's
  `cron-wrapper.sh` after every check (default on, non-fatal) and flattens
  the newest aggregated report plus self-heal `state.json` into an atomic
  Prometheus textfile exposition: status 0/1/2/3, `component_enabled`
  (off ≠ failing), per-check `duration_seconds`, per-tenant
  lunarvision/lunarvoice labels discovered from the ports registry (never a
  hardcoded single-node port), init-agnostic unit series, and self-heal
  restart/escalation/backoff/flapping gauges. Only numeric and boolean
  leaves are exported, so no secret strings can leak.
- Graduated early-warning thresholds: `THRESHOLD_<metric>="warn:>N crit:>M"`
  config emits `lunarwing_health_early_warning` series and drives a
  deduplicated Gotify notification path (per metric+severity cooldown);
  ready-made Prometheus rules and a Grafana dashboard ship under
  `ic-infrastructure-health-check/contrib/`.
- Port registry v16 allocates one host-global `health_metrics` slot (default
  50000, above every tenant range; hand-edited collisions are refused). The
  migration runs automatically on the next mt-admin registry operation or
  via the standalone `ic/scripts/migrate-ports-v16.sh`.
- Optional loopback-only listener (`ic/crates/lunarwing_health_metrics`)
  serves the exposition over HTTP with loopback bind + peer enforcement and
  constant-time bearer comparison; its installer supports systemd, OpenRC,
  and launchd.
- systemd health-path hardening follow-ups: the watchdog can now target the
  system or per-user manager via `LUNARWING_WATCHDOG_SYSTEMD_SCOPE`
  (system|user); self-heal's per-tenant systemd user-bus probes are bounded
  by `SELF_HEAL_SYSTEMD_PROBE_TIMEOUT` (default 15 s) so a hung user manager
  can no longer stall the whole sweep; `health-systemd.sh` flags active
  timers with no scheduled next wake (a monitoring blind spot where a timer
  looks healthy but will never fire again); the mt-admin-rendered health
  oneshot carries an explicit `TimeoutStartSec` instead of relying on
  systemd's default; and the watchdog installer no longer aborts when the
  initial smoke run fails. Covered by two new test suites
  (`test-watchdog-systemd.sh`, `test-health-metrics-unit.sh`).

### WASM Channel Tool Activity

- New global `tool_activity` flag (env `TOOL_ACTIVITY` or the
  `channels.tool_activity` setting, default off) routes
  `StatusUpdate::{ToolStarted, ToolCompleted, ToolResult}` events to WASM
  channels, closing work item #47's observability gap: operators can now watch
  an agent's tool calls land over XMPP, WeeChat, and DarkIRC instead of only
  hearing the final answer.
- The WASM host wrapper drops tool-activity events before invoking the guest
  unless the flag is on, so default behavior and guest CPU usage are
  unchanged; the flag is honored both at startup and for hot-activated
  channels. Native channels keep their per-channel `/debug` toggles.
- The XMPP guest renders the preformatted tool messages in DMs (groupchat
  statuses stay suppressed as before); the DarkIRC and WeeChat guests render
  them as `[status]` lines.

### WeeChat Outbound Integrity

- Fixed tool output corruption in the WeeChat channel (work item #66): the
  relay's `POST /api/input` routes through the typed-input path, which
  truncates non-multiline buffers at the first newline — every multi-line
  tool result or agent reply lost everything after its first line. Outbound
  messages are now split per line (stripping `\r`, NUL, and control chars)
  before chunking.
- Lines starting with `/` were previously executed as WeeChat commands in
  the operator's client (e.g. a tool-output line like `/msg bob hi`);
  buffer-message sends now escape the command prefix by doubling the leading
  slash. The intentional `/msg` DM fallback stays raw.
- Channel registry patches bumped per CI source-version policy, final
  versions for this release: darkirc 0.1.5, weechat 0.2.6 (0.2.5 carried the
  tool-activity rendering, 0.2.6 the outbound integrity fix), xmpp 0.1.10.

### CI, Dependencies, and Housekeeping

- Fixed the `rust-test-lib` / `rust-test-libsql` CI failures: the stateful
  MCP WAT fixture's byte-exact `initialize` envelope is now templated with
  placeholders rendered from the same `McpRequest::initialize` code path the
  transport uses, so it tracks `CARGO_PKG_VERSION` forever (4738 lib tests
  green, libsql-feature 4725 green).
- Fresh RustSec audit of all 19 tracked lockfiles: one new advisory since the
  last pass, `RUSTSEC-2026-0258` (h2 unbounded empty DATA frames), was fixed
  everywhere it was fixable — h2 0.4.15 → 0.4.16 in the main and xmpp-bridge
  lockfiles, and the OCR sidecar's warp 0.3 → 0.4.3 upgrade eliminated its
  entire hyper 0.14 / h2 0.3 tree. The one remaining h2 0.3.27 copy
  (reachable only through the legacy libsql → tonic → hyper 0.14 chain, with
  no 0.3-line patch) was accepted with reason, bringing the accepted-findings
  count to eight. Details in `docs/ops/CRATE_SECURITY_AUDIT_v2.0.8.0.md`.
- Version bump: all LunarWing crates and WASM channels 2.0.7 → 2.0.8,
  channel registry patch bumps (darkirc 0.1.5, multica 0.1.7, weechat 0.2.6,
  xmpp 0.1.10), full `cargo update` on the main lockfile (MSRV
  1.96.1-compatible), cargo-deny clean. The new `lunarwing_health_metrics`
  crate is enrolled in the CI release-version policy.
- Fixed an external-worker wire bug found by live smoke testing: the
  orchestrator serialized `TaskContext.project_dir` as an explicit JSON
  `null` when unset, which the Kimi worker's TypeScript bridge rejects —
  every Kimi job dispatched without an explicit project directory failed at
  the bridge before the worker could start. The field is now omitted when
  unset, a shape all three workers (Pebble, OpenCode, Kimi Code) accept.
- Opencode external worker: `OPENCODE_REF` pinned to `v1.18.18` as a single
  global-scope ARG baked into the runtime image as `OPENCODE_IMAGE_REF` and
  surfaced on `/health` as `opencode_ref`; build contract re-verified against
  the canonical `anomalyco/opencode` tag.
- Kimi external worker: `KIMI_CODE_VERSION` pinned to `0.37.2` as a single
  global-scope ARG baked into the runtime image as `KIMI_CODE_IMAGE_VERSION`
  and surfaced on `/health` as `kimi_code_version`; CLI contract
  (`-p`/`--output-format stream-json`, stream-json line shapes, config keys)
  re-verified against the `0.37.2` npm release. The baked default provider is
  now the LunarWing TensorZero gateway (`tensorzero/FrontierCODE`,
  OpenAI-compatible, reached over the NetBird overlay) — the same function
  the OpenCode worker uses — because worker prompts against the managed
  `api.kimi.com` service fail on 0.37.2. `KIMI_MODEL` overrides that do not
  match an existing config alias retarget the baked model entry so
  `default_model` stays resolvable (0.37.2 refuses startup otherwise),
  `KIMI_API_KEY` falls back to `TENSORZERO_API_KEY`, and the previous
  managed-service OAuth flow remains available by mounting a custom config
  via `KIMI_CONFIG_SOURCE`.
- New operator documentation: a Debian/Ubuntu tenant spin-up guide joins the
  existing Gentoo/Arch/Fedora guides, and the architecture, bugs, guides,
  internal, and proposals doc sets were each re-verified against the code for
  this release.
- Repo hygiene: root branding assets moved to owned, descriptive paths (README
  and onboarding references updated), stale root `PEBBLE.md` archived, empty
  `.codex` markers and a stale resume artifact removed. Full ledger in
  `docs/ops/v2.0.8.0_REPO_HYGIENE.md`.
- CI now runs the full master job set on `aragents/kimil00p/*` branches.
- Worker container images are now published under the moving `v2.0.8.0` and
  `latest` tags instead of accumulating a unique short-SHA tag per build,
  and Kaniko registry pushes force HTTP/1.1 with `--push-retry=5` so blob
  PATCHes through the Traefik front end stop failing with 502 Bad Gateway.

## Upgrade Notes

1. Back up PostgreSQL, tenant env files, `/etc/lunarwing/ports.json`,
   workspace state, rendered units, worker data, voice models, and channel
   state.
2. No new SQL migrations ship in this release (V26 remains latest). The
   multi-tenant port registry migrates v15 → v16 automatically on the next
   mt-admin registry operation (or run `ic/scripts/migrate-ports-v16.sh`
   manually); the migration only adds the host-global `health_metrics` slot
   and refuses to proceed if a hand-edited collision exists.
3. OpenRC/Gentoo multi-tenant hosts: run
   `ic/scripts/lunarwing-mt-admin.sh setup-cgroups` once to render the
   boot-time subtree script and PAM hook, then re-add or re-render tenants so
   bounded builds and hard ceilings take effect. Review
   `docs/ops/PREDELEGATED_SUBTREE_CGROUPS.md` first on hybrid cgroup hosts.
4. To enforce MCP marketplace trust, populate the new `marketplace` section
   in `mcp-servers.json` (trust store, revocation cache, revocation URL and
   publisher key). Once configured, revoked or corrupt entries fail closed at
   load and activation. Leave it unconfigured to keep the previous behavior.
5. To run the bundled MCP gateway control plane, set
   `LUNARWING_MCP_GATEWAY_CONTROL_BIND` and a
   `LUNARWING_MCP_GATEWAY_ADMIN_TOKEN` (≥16 chars); it binds loopback unless
   `LUNARWING_MCP_GATEWAY_CONTROL_ALLOW_NON_LOOPBACK=1` is set.
6. Cross-tenant knowledge sharing is off by default; set
   `COORD_KNOWLEDGE_PUBLISH=1` and `COORD_KNOWLEDGE_PUBLISHERS` per tenant to
   opt in. Older daemons ignore KNOW records safely.
7. Rebuild the opencode external-worker image so the pinned `v1.18.18` build
   takes effect; check `/health` for `opencode_ref` afterward. Rebuild the
   kimi external-worker image for the pinned `0.37.2` CLI; check `/health`
   for `kimi_code_version`.
8. Tenants created before this release may carry the legacy
   `IRONCLAW_BASE_DIR`/`IRONCLAW_SOCKET` env aliases. No action is needed —
   both remain accepted — but newly generated tenant envs contain only the
   `LUNARWING_*` primaries.
9. The healthpoint metrics exporter runs with the host health pipeline by
   default and only writes a local `.prom` file. Serving it over HTTP is
   opt-in: install the loopback-only listener with
   `ic/scripts/install-health-metrics-listener.sh` after the v16 port
   migration has allocated the `health_metrics` slot.
10. If you previously set `SKILL_SELF_IMPROVEMENT=0`, note it now also pauses
    the skill-extraction and skill-maintenance missions (previously only
    self-improvement was gated). Re-enabling the flag resumes exactly those
    gate-paused missions; missions you paused yourself stay paused.

## Known Issues

### Security, Runtime, and Release Limitations

- Eight accepted RustSec advisories remain in the shipped graph, blocked by
  libSQL's legacy TLS stack, the vendored Signal wrapper, and the unpatchable
  h2 0.3 line in libSQL's legacy chain; the v2.0.8.0 re-audit fixed
  `RUSTSEC-2026-0258` everywhere an upgrade exists and found no other newly
  fixable advisories. See `docs/ops/CRATE_SECURITY_AUDIT_v2.0.8.0.md`.
- DarkIRC Secure Key Exchange Stages B-D and automated QR/web/trusted-channel
  delivery remain disabled; Stage A live-host release acceptance remains a
  release/deployment gate rather than proof of activated contact security.
- MCP still does not automatically install npm/pip packages. Worker-protocol
  MCP delivery is deferred (external-worker servers are config-validated but
  not pushed to `lunarwing-agent-v1` workers), and there is no signed-package
  marketplace install/upgrade CLI yet — trust primitives are enforced, but
  catalog install is manual. Custom MCP HTTP header values are still stored
  directly rather than through owner-scoped secret references. A stdio MCP
  bridge tool can be installed to use HTTP MCP servers with proper secret
  management.
- The web UI still places its bearer token in native `EventSource` query
  strings. Keep access/proxy logs protected until short-lived SSE tickets or
  cookie-scoped sessions replace this transport.
- Onboarding remains a trusted local console without user accounts or TLS.
  Keep it loopback-bound with token authentication and use an SSH tunnel
  remotely.
- OpenRC hosts without a unified cgroup v2 hierarchy (or where delegation is
  denied) get nice-only build bounding instead of hard memory/CPU/task
  ceilings; mt-admin reports the reason loudly.
- `ALLOW_PRIVATE_IPS=1` weakens the public-address attachment boundary; use it
  only for trusted private networks.
- Engine output safety still truncates oversized prefixes before later
  redaction/policy transforms and does not re-cap every later transformation.
- `cargo check --tests --no-default-features --features postgres` currently
  fails on three pre-existing cfg(test) breakages on master (libsql-gated
  `test_db` helpers and a multi-tenant test harness builder); CI never runs
  that shape (postgres runs check-only, tests run under libsql), but a fix is
  still owed.
- Kawarimi remains PostgreSQL-only.
- Full release test aggregation remains blocked by unavailable real channel
  WASM artifacts and existing skill-selection/TensorZero streaming
  expectations.
- Current GitLab CI verifies source but does not publish release artifacts,
  packages, or tenant deployments.

### Resolved Since v2.0.7.0

- OpenRC's missing hard memory/CPU/task ceilings: cgroup v2 ceilings and
  automated pre-delegated subtrees now ship for multi-tenant OpenRC hosts
  (work item #54).
- The opaque 10-threads-per-day mission limit: budgets are now configurable
  at creation, visible on list surfaces, and loudly logged on exhaustion
  (work item #56).
- Missions re-firing stale triggers after daemon restarts or listener
  re-deliveries (skill-extraction and conversation-insights token burn):
  trigger-level dedup keys now make re-delivery a zero-cost skip, and
  `SKILL_SELF_IMPROVEMENT=0` fully gates the skill-learning missions (work
  item #69).
- Engine V2 discarding "always" approvals whenever the approved tool call
  failed, making the GitLab WASM tool re-prompt on every call: grants now
  persist at tool level independent of call success, ssh excepted (work item
  #70).
- The GitLab tool's classic-issues-only API: unified Work Items support,
  bounded keyset pagination, and a `get_issue` 404 fallback now cover
  GitLab's Work Items migration (work item #60).
- Kimi external-worker jobs failing at dispatch whenever no explicit project
  directory was set — the worker bridge rejected the serialized JSON `null`:
  `TaskContext.project_dir` is now omitted on the wire when unset.
- WeeChat truncating multi-line tool output at the first newline and
  executing outbound lines starting with `/` as client commands (work item
  #66).
- XMPP bridge rejecting photo-sized image attachments with HTTP 413: the
  `/v1/messages/send` JSON body limit is now an explicit 70 MiB instead of
  axum's 2 MiB default, scoped to the send route so every other route keeps
  the default bound, and authorization now runs before the body is buffered
  — closing the pre-auth memory-amplification window from
  `docs/ops/MEDIUM.MD`. A regression test proves large attachment sends
  reach the handler (work item #42).
- WASM channels silently dropping tool-activity status events: the global
  `tool_activity` flag now routes ToolStarted / ToolCompleted / ToolResult to
  the XMPP, WeeChat, and DarkIRC guests, which render them (work item #47).
- MCP Part 2's library-only trust stack: signature, lockfile, and revocation
  checks now run on real config-load and activation paths, and the bundled
  gateway control plane can actually be enabled.
- The bundled MCP gateway's buffered data plane: SSE responses now stream,
  and `listChanged` passthrough is no longer structurally blocked.
- The built-in study-notes layer lagging the canonical store: all seven
  tools, the current metadata model, and canonical filter semantics now ship
  natively, and `add-tenant --with-study-mcp` provisions new tenants with
  the always-current read-only store automatically.
- MCP servers answering initialize with protocol `2025-03-26` (such as the
  managed study-notes server) failing to connect: the client now accepts that
  revision alongside the requested `2024-11-05`.
- The `rust-test-lib` / `rust-test-libsql` CI failures caused by the
  version-locked WAT fixture envelope.
