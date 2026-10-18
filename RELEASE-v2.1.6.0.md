# Release Notes for LunarWing v2.1.6.0

**DRAFT — pre-release; content reflects the v2.1.6.0 tree as of 2026-10-18 and may grow before the tag is cut.**

## Codename: Tsuno (角, つの): The Horns

**Release date:** TBD (drafted 2026-10-16; refreshed 2026-10-18)

> Not raised in anger,
> but set and sure all the same —
> the horns hold the line. 角

After Bungei Fukkō reopened the workshop, **Tsuno lowers its head**: a
guarding release — worker authority fenced to its own domain, credentials
kept out of untrusted environments, and every silent failure given a name
and an audit line.

## Overview

LunarWing v2.1.6.0 is an external-worker security and operator-truthfulness
release. The headline work is the external-worker tool proxy (WI#324):
workers can now call a curated capability set — workspace memory,
coordination knowledge, and GitLab tools — through the hub over the worker
socket, with the hub authoritative at every step. On top of it sit cron
missions for external workers (WI#325): scheduled, supervised mission loops
with their own per-mission tool grants, confined project directories, and
honest busy/skip accounting.

The rest of the cycle is boundary work. A cron-mode worker's worker-level
tool grant is stripped at load so one container never holds two authority
domains; a mission webhook stored with an empty secret can never be fired
(the constant-time compare treated two empties as equal); HTTP tool surfaces
are credential-redacted before they reach events, prompts, or summaries; the
`AGENT_AUTH_TOKEN` bridge credential no longer rides into spawned worker
task environments; and tenant-controlled proxy URLs are no longer
shell-evaluated as root by OpenRC proxy units — the cycle's highest-severity
fix (WI#376), with a **required** migration step for OpenRC multi-tenant
hosts (see Upgrade Notes). The opencode worker gains a control channel for
orchestrator follow-ups with a closed-queue contract and a single final
wrap-up turn.

Operations get their share: multi-tenant worker LLM endpoints no longer
fall back to an unreachable fleet default (tenants fail loud at add time
and re-derive on upgrade), the pebble fat image proves its toolchain
anchors in CI before a build can pass, the opencode2 lane ships its WI#357
exposure-policy controls, and OpenRC proxy migrations refuse silently-wrong
states instead of refusing healthy ones. The cycle closes with the usual
dependency sweep (2.1.5 → 2.1.6 across all owned packages, refreshed
registries, regenerated vendor tree) and a full crate-security re-audit with
two late-cycle security upgrades (rustls 0.23.46, Wasmtime/WASI 36.0.18) —
including one **breaking change**: Turso/libSQL remote-replica sync now
lives behind the non-default `libsql-remote-replica` cargo feature (see
Upgrade Notes).

See the [v2.1.5.0 release notes](docs/releases/RELEASE-v2.1.5.0.md) for the
previous compatibility baseline.

## Highlights

- **External workers can call host tools — through the hub, on a leash
  (WI#324).** A per-task unix-socket tool proxy in the worker bridge relays
  `tool_list`/`tool_call` frames to the daemon, which enforces the curated
  capability set (`memory_read`/`memory_write`,
  `knowledge_search`/`knowledge_submit_note`/`knowledge_get_topic`,
  `gitlab_tool`) at load, dispatch, and catalog construction. The prompt-write
  boundary denies proxied writes to identity, config, memory-bootstrap,
  daily-log, and `engine/**` files. The kimi worker lane ships the first
  worker-side bridge: an in-image MCP shim gives the agent a native
  `mcp__lunarwing__*` surface; the opencode lane ships a serve-process
  plugin over the same relay (opencode2/pebble lanes pending).
- **Cron missions on external workers (WI#325).** A mission supervisor owns
  the tick chain: heartbeat watchdog, lapse → cancel → terminal-wait →
  backoff re-entry, persisted pause/tick counters, inline `job_events`, and
  a circuit breaker. Owner-authenticated `GET/POST
  /api/workers/{worker}/cron` and the `cron_mission` tool manage missions;
  ticks run in confined, bridge-created per-mission project directories
  (`missions/<mission_id>`) with permanent-vs-transient errno
  classification on both host and bridge sides.
- **One container, one authority domain.** A cron worker's worker-level
  `tool_allowlist` is stripped at load — a tick can no longer borrow an
  interactive job's grant through a same-uid relay socket — any non-cron
  entry sharing a cron entry's endpoint (compared by normalized
  host:port) is stripped too, and two cron entries sharing one container
  with *differing* `cron.tool_allowlist` grants are now detected at load
  and fail closed: every cron grant on that shared listener is stripped
  with an `error!` (identical grants stay, with a warn). Operator
  guidance: give each cron mission its own container.
- **Mission webhook hardening (WI#352, WI#359).** A legacy mission stored
  with an empty webhook secret can never authenticate a fire; create/update
  reject unpresentable secrets; the single-owner webhook-path rule is
  enforced under a serialized check-and-save across create, update, and
  resume; and run-spawning mission verbs share an aggregate per-user rate
  budget (60/min, 600/h) charged before hooks.
- **Credential redaction that follows the data (WI#359).** HTTP tool
  surfaces have credentials scrubbed before they reach persisted events,
  approval prompts, or channel summaries: header values (by name *or*
  auth-scheme prefix), URL userinfo, query values, fragments, and JSON/form
  bodies, using one shared credential-key list.
- **Tenant proxy URLs are no longer evaluated as root (WI#376).** A
  tenant-controlled `TENSORZERO_URL` from `proxy.env` was copied into the
  OpenRC proxy init script and shell-evaluated as root during any OpenRC
  operation. Rendered units now pass the fixed `--tensorzero-env` option and
  the proxy reads the literal value after `command_user` drops privileges;
  legacy fleet units are neutralized up front and mt-admin fails closed
  until the migration runs — a **required** step for OpenRC multi-tenant
  hosts (see Upgrade Notes).
- **Honest pending gates (WI#332).** Gates whose thread vanished or left
  `Waiting` are dropped from the HTTP views and swept from memory and
  persistence with an audit line and a `thread_terminated` broadcast;
  approvals that race a thread termination fail closed; authentication-fallback
  gates keep their old live-until-expiry contract.
- **Shutdown honesty for missions (WI#337, WI#339, WI#341).** Shutdown
  interruption stamps are tracked per thread (overlapping fires no longer
  shadow each other), the boot sweep isolates failures per mission and per
  stamp, and owed-but-undelivered outcome notices are stamped at shutdown
  and delivered at the next boot. Parity checks (thread type, project,
  owner, thread-history membership) clear forged or corrupt stamps with a
  warning instead of retrying them (WI#338).
- **Worker endpoint truth (WI#330).** Multi-tenant provisioning requires a
  real LLM endpoint up front — no more unreachable built-in default — and
  worker base URLs inherit from the tenant endpoint with provenance
  markers, re-derive on every render, backfill on upgrade, and are skipped
  by tenant export so a migrated tenant re-derives on the target host. All
  three worker images bake `.invalid` placeholders and refuse to start if
  one survives to the effective config.
- **Pebble toolchain anchors, proven (WI#334).** The fat image's PATH-anchor
  block is exercised verbatim under dash in every ci-config run, and the
  mt-admin build gate smoke-probes the `:candidate` tag against the same
  container runtime that built it — a failing image can never be re-tagged
  `:latest`.
- **OpenCode control channel (WI#328).** The orchestrator can now send
  bounded, acked follow-up prompts to a running opencode task, closing with
  one final wrap-up turn; success is preserved only when the turn banked
  completed output, and every delivered-but-unacked prompt is persisted in
  the audit trail.
- **Workers must never self-expose (WI#357).** The opencode2 lane ships
  naive-use guards and build-time controls — an argv-scanning `opencode`
  wrapper, root-owned read-only scripts/config, the OpenTunnel compile-out,
  a byte-identity wrapper anchor, a BuildKit-faithful Dockerfile guard, and
  the host-side `mt-admin worker-listeners` audit — and opencode2 advances
  to **v2.0.32** under the maintainer-approved WI#427 (protected
  candidate/scan green; the tenant rollout of the merged image is still
  open).
- **Credential echoes scrubbed in the sandbox proxy (WI#395).** Injected
  credentials can no longer ride back out through forwarded response
  headers (raw, form-encoded, re-encoded, or JSON-escaped spellings),
  redirect targets, or response bodies; credentialed exchanges force
  identity encoding and refuse opaque compressed bodies instead of
  forwarding them blind.
- **Mission threads steer, they don't own (WI#356).** Mission threads are
  refused `mission_create`/`mission_delete`/`mission_resume` outright, and
  their `mission_update` is allowlist-bound to steering fields — a mission
  thread can no longer rename missions, wire one to a webhook with a
  model-chosen secret, or zero its daily budget.
- **kimi-code 2.2.0.** The kimi worker advances to
  `@moonshot-ai/kimi-code` 2.2.0 with the contract lane verified against
  the real 2.2.0 binary (host run; the in-image gate runs on the next
  protected candidate build — on a rebuilt image the wire now carries
  `max_tokens: 131072`, verified end to end on a live tenant), plus new
  environment scrubs (`KIMI_CODE_TRUST_WORKSPACE`, WI#411) and a pinned
  `max_output_size` for the FrontierCODEK model (WI#412).

## Compatibility

| Area | v2.1.6.0 |
|---|---|
| Release / owned packages | Release `2.1.6.0`; all eleven LunarWing-owned release packages use `2.1.6` |
| Rust edition / MSRV | Rust 2024 / `1.96.1`, unchanged |
| SQL migrations | V28 remains latest; no new migrations this cycle |
| Multi-tenant port registry | v17, unchanged |
| WASM runtime | Wasmtime/WASI 36.0.18; matching Cranelift family 0.123.18 — security upgrade from 36.0.17 (RUSTSEC-2026-0352/0363; see Crate Security Re-Audit) |
| Bundled skills | Seven product skills, unchanged; no skill migration |
| Channel registry | darkirc 0.1.12, multica 0.1.15, weechat 0.2.13, xmpp 0.1.18 |
| Tool registry | cavepony-tool 0.4.6, gitlab 0.1.9, gotify 0.1.3, llm-context 0.1.4, multica-bridge 0.1.5, ssh 0.1.4, vision-analyze 0.1.6, voice-transcribe 0.1.4, web-search 0.2.5 |
| External workers | kimi-code `2.2.0` (upgraded from 2.1.1), opencode `v1.18.25`, opencode2 `v2.0.32` (maintainer-approved WI#427; OpenTunnel compiled out), Pebble `2d0f3786…` (upstream ref unchanged; its reviewed lock was upgraded for the rustls advisories) |
| Cargo features | New non-default `libsql-remote-replica` feature gates Turso sync (breaking — see Upgrade Notes) |

Registry version bumps require rebuilding/reinstalling the affected WASM
extensions. The channel version bumps themselves do not introduce protocol
changes.

## Changes

### External-Worker Tool Proxy (WI#324)

External workers can call a curated set of host tools through a per-task
unix-socket relay in the worker bridge, answered by the hub over the worker
websocket. The exact positive capability set is
`memory_read`/`memory_write`,
`knowledge_search`/`knowledge_submit_note`/`knowledge_get_topic`, and
`gitlab_tool` (legacy `gitlab-tool` spellings resolve to the canonical
grant), enforced at manager load, dispatch, and catalog construction;
host-file, execution, attachment, HTTP, and job-plane tools stay excluded.
The catalog advertises a proxied `memory_write` schema whose contract
matches what the boundary actually permits — explicit workspace `target`
required, no `layer` — instead of the tool's own always-denied defaults.

The prompt-write boundary denies proxied writes to identity/config files,
memory/heartbeat/bootstrap, profile/directives, lorebooks, daily logs
(including the omitted-target default), and all of `engine/**`, enforced
before hooks and on hook-final arguments. Socket frames are bounded before
JSON parsing (16 MiB message/frame caps) with a separate 256 KiB
`tool_call` cap, and a per-job 100-call cap validates worker-controlled
identifiers before persisting or echoing them.

Cron authority is split from interactive authority: tick jobs draw from a
separate `cron.tool_allowlist` (default empty — every proxied tick call
denied), and a cron-mode worker's worker-level list is stripped at load
with an `error!`, because a tick could otherwise borrow a concurrent or
earlier interactive job's grant through that task's same-uid relay socket.
The same strip applies to any non-cron entry sharing a cron entry's
endpoint, compared by normalized container key (lowercased host, loopback
spellings folded, scheme-default ports applied, path/query ignored — one
host:port that path-routes several containers strips all of them, failing
closed). Two cron entries sharing one container are likewise detected
(WI#396, closed): grouped by the same normalized key, differing
`cron.tool_allowlist` sets strip every cron grant on the shared listener
with an `error!` (identical sets stay, with a `warn!`) — one container is
one authority domain, so one mission's tick could otherwise borrow the
other's grants through its relay socket, concurrent or later.

The **kimi** lane ships the worker-side bridge: `lunarwing_tool_proxy.ts`
(a per-task relay; replies routed by call_id scoped to the owning hub
socket; task close fails in-flight calls) plus `lunarwing_mcp_server.ts` (a
thin stdio MCP server registered in `$KIMI_CODE_HOME/mcp.json`), giving the
agent a native `mcp__lunarwing__*` tool surface. The **opencode** lane now
ships its worker-side bridge too (the same relay ported to the v1 SDK, plus
a serve-process plugin registered in `config/opencode.json` that statically
exposes the phase-1 tools and resolves the calling session's task id from
its `lunarwing-opencode-<task_id>` title), running in the new
`worker-opencode-ts` CI lane and the image-build gate. The opencode2 and
pebble worker-side bridges remain open. WI#324's real-worker acceptance (a
granted proxied call from a real worker, and `knowledge_search` hits
through the proxy) has no evidence yet: the kimi test-tenant jobs on
rebuilt images (soarin, fleetfoot) returned a fixed `KIMI_WORKER_OK` string
without a proxied call, and mt-admin renders no worker `tool_allowlist`
(the default is empty, so a managed tenant has no proxied tools until an
operator grants them). The opencode plugin reaches tenants only with a
rebuilt image, on the known-broken v1.18.25 lane (WI#333, bump-or-retire
pending), and no real opencode job has exercised it — **WI#324 remains
open**. Design doc: `docs/proposals/EXTERNAL-WORKER-TOOL-PROXY.md`.

### Cron Missions on External Workers (WI#325)

A mission supervisor per `(worker, mission_id)` owns the tick chain: the
schedule drives ticks, each tick runs as a real job with inline
`job_events`, and a lapse triggers cancel → terminal-wait → backoff
re-entry. Heartbeat watchdog, persisted pause state and tick counter
(surviving daemon restarts), and a circuit breaker round out the loop; the
breaker count is now durable too (WI#371 — see the durability section
below).

The `cron_mission` LLM tool and owner-authenticated `GET/POST
/api/workers/{worker}/cron` routes manage missions; the tool is on the
autonomous denylist and pause/resume rows record their origin. Ticks run in
`cron.project_dir` (default `missions/<mission_id>`, host-validated as a
relative path), which updated bridges create with descriptor-relative
symlink-safe semantics; all four bridges classify bootstrap errnos as
permanent (validation, ENOTDIR/ELOOP, EACCES/EPERM, containment) versus
transient (EMFILE/ENFILE/ENOMEM, ENOSPC/EDQUOT/EIO, swap races), and the
host checks the transient marker first. Busy workers and dispatch-time
capacity exhaustion retry the same link up to five times, then skip to the
next scheduled fire without touching the breaker. IANA `cron.timezone` is
honored, duplicate mission ids across workers are rejected at load, and
non-owners get one uniform `CronMissionNotOwned`. Tick links have a
host-imposed four-hour maximum matching Kimi's default cap.

**WI#325 is closed**; audio capture was scoped out at close-out. Of its
residuals, WI#370 (retention/list visibility, event-outcome FK coverage) is
open; WI#375 (remaining harness items) and WI#396 (the cross-cron
shared-container pairing, now detected and failed closed — see the
External-Worker Tool Proxy section) were closed on 2026-10-17, and WI#371
(read-failure pause safety, durable breaker count) on 2026-10-18 — see Cron
Mission Control Durability below. Design
doc: `docs/proposals/EXTERNAL-WORKER-CRON-MODE.md`.

### Cron Mission Control Durability (WI#371)

Mission control state is now honest across faults. A control-state read
or parse failure (or a failed legacy pause-key read) starts the mission
**paused** with an actionable `durability_error` naming the fault and the
remedy — never a silently-resumed mission over unreadable state; the
legacy `cron-mission:<id>:paused` key remains a read-only fallback so
pre-WI#371 pauses stay paused. The pause flag and the
`consecutive_abandoned` breaker counter share one atomic settings row
(`cron-mission:<id>:control`, single write as the atomic unit), so the
counter persists on every abandon and the trip lands pause+count in one
write; the legacy key is never rewritten. Control writes retry with
bounded exponential backoff, and exhaustion never claims success:
`durability_error` surfaces through the HTTP route (`"durable": false`)
and the `cron_mission` tool, and `cron_paused`/`cron_resumed` audit rows
carry a `durable` flag. A backend-agnostic fault-injection test decorator
pins absent/malformed/failed-read distinction, retry-then-success,
exhaustion, and the restart-surviving breaker chain. WI#371 was closed
on 2026-10-18.

### Harness Event Redaction Hardening (WI#326)

The harness-worker secret redactor scrubs secrets from job events in their
raw, base64/url-encoded, and line-wrapped (folded) forms, including
JSON-escaped line breaks and any CLI wrap width. This cycle fixes a folded
64-char coverage gap for 58–62-byte encodings, an availability regression
where a mounted secret longer than the remaining event text paid for a
searcher setup per event leaf (~62 s CPU measured on a 2 MB event; now
skipped), a cache/clear race that could leave an unremovable cached
redactor for an ended job, and a folded-range sweep that no longer drops
overlapping matches. WI#326 was closed on 2026-10-14; known residuals — the
direct-pass/fast-path overlap gap, the 4096-char arming cap's
partial-print exposure, and the aho-corasick per-job automaton direction —
are tracked in WI#383.

### OpenCode Worker Control Channel (WI#328)

The opencode worker bridge accepts `task_prompt` follow-up frames from the
orchestrator: per-task queue (64 prompts), sequence validation and dedupe,
and a close-on-accept `done` contract — once the final prompt is accepted,
the daemon rejects every later prompt with HTTP 409 and the worker runs one
final wrap-up turn. Frames are bound to the socket that owns the task, and
every delivered seq the worker never acked is persisted as
`control_prompt unacked seq=N` when the read loop exits, so nothing
vanishes from the audit trail.

Terminal accounting is honest on both paths: success is preserved only when
the failing turn banked completed output (never its own tool errors,
partial text, or retry diagnostics), empty or denied work is an error,
accepted-but-unrun prompts are accounted as control notes in the result
output, and control notes can never manufacture success. A new
`worker-opencode-ts` CI lane runs the bun self-test suites (seven since the
WI#324 relay and plugin suites joined) plus `tsc --noEmit` on every
MR/push, and the `CI_BUN_IMAGE` digest is cross-checked against the
Dockerfile's pinned `oven/bun` FROM so retagging cannot silently dissolve
the tie. Known residuals: WI#386 (a clean turn whose only part is a tool
error still reports success; its retry-diagnostics-only case is fixed on
this lane), WI#387 (postgres-only dead-code warnings), and the
pre-existing WouldBlock `delivery_failed` false-negative class.

### opencode2 SDK Surface Pinning (WI#329)

The opencode2 lane's bridge↔SDK contract is now pinned by a single
committed list (`sdk_surface.ts`) driving two gates: the host self-test
extracts the executor's real `sdk.*` call sites from source and asserts set
equality with the pinned method list (a swallowed call now fails the
suite), and the image build asserts every pinned method and event literal
still exists in the shipped `@opencode` client source — an upstream rename
fails the image build instead of hanging tasks at runtime. Late in the
cycle the gate was widened (WI#369): the executor's event stream is typed
with the client's exported `OpenCodeEvent` union, so the builder-stage tsgo
gate now covers event payload shapes as well as names, and the two
dependency-free SDK-surface suites run in the `worker-opencode2-ts` MR lane
instead of only at the protected image build.

### Worker LLM Endpoint Configuration (WI#330)

Multi-tenant provisioning no longer ships an unreachable fleet default:
`add-tenant`/`add-tenants` require an explicit or ambient LLM endpoint
before any tenant state exists, and systemd and OpenRC proxy units render
each tenant's own `proxy.env` upstream (repairing a pre-existing
proxy/daemon upstream split). Worker base URLs resolve with provenance:
operator-set values win; missing values inherit from the tenant endpoint
(marked `<KEY>_SOURCE=inherited`), re-derive on every render, backfill
during `upgrade-tenant` before any image rebuild, and are skipped by
`export-tenant` so migrated tenants re-derive on the target host. One
validation funnel (`validate_llm_url`) applies at add and configure time,
and loopback/overlay-MagicDNS endpoints are kept out of worker envs while
the daemon may keep them. All three worker images bake `.invalid`
placeholder endpoints and their entrypoints refuse to start — naming the
env var and the fix — if a placeholder survives to the effective config.

### libSQL Remote-Replica Feature Gate (WI#331) — Breaking Change

Turso remote-replica sync moves behind the new non-default
`libsql-remote-replica` cargo feature. Default and postgres-only builds no
longer compile the legacy rustls-webpki 0.102.8 / h2 0.3.27 chain at all,
and a build without the feature **fails closed at startup** (an error
naming the feature; never a silent local fallback) when `LIBSQL_URL` is
configured. `lunarwing status`/`doctor` are feature-aware instead of
misreporting health, and the wizard warns that unsetting `LIBSQL_URL` turns
a replica into a diverging standalone database. The XMPP bridge lockfile
re-resolved from 931 to 895 packages with the legacy chain gone, and a new
`default-graph-advisory-guard` CI job pins the default-feature production
graph free of the legacy chain at both build roots.

### Crate Security Re-Audit

The v2.1.6.0 audit (`docs/ops/v2.1.6.0_CARGO_DENY_AUDIT.md`) is a
risk-acceptance record, not a clean bill: **eight vulnerability advisories,
two unsound notices (`failure`, `atty`), and two unmaintained notices**
(twelve IDs) ship accepted via `ic/deny.toml`, each with recorded exposure
and a named clearing owner (rsa: WI#368; `failure`/`rental`/`atty`: WI#367;
the seven libsql 0.6.0 legacy-chain acceptances — four rustls-webpki 0.102,
the h2 0.3.27 pin, and the rustls 0.22.4 pair — WI#418, whose standing
decision record `docs/ops/v2.1.6.0_WI418_LIBSQL_LEGACY_CHAIN.md`
(2026-10-16) adopts the WI#331 feature gate as the long-term mitigation
after re-confirming upstream — libsql 0.10.0 went stable 2026-10-13 and
still pins hyper-rustls ^0.25 — so a bump still clears nothing). All three
owners were closed on 2026-10-18 with every chain still in `ic/Cargo.lock`,
so none of the twelve has an open clearing owner at release (see Accepted
advisories under Security, Deployment, and Release Limitations). After the
WI#331 gate, exactly one vulnerability (rsa
via russh) compiles into default builds; the webpki/h2 chain and the
accepted rustls 0.22.4 advisories are opt-in only. No unaccounted advisory
exists in any committed lockfile.

Late in the cycle the `unsound` advisory scope was widened to `"all"`
(WI#360): cargo-deny's default workspace scope never surfaced transitive
unsound advisories, so `atty` 0.2.14 (RUSTSEC-2021-0145, Windows-only
unaligned read, no patched release) was invisible to every gate. It is now
accepted with a recorded exposure (dev-graph only, via the vendored
libsignal-protocol's `env_logger` 0.6.2 dev-dep; never a production edge)
and `[bans]` restrictions limiting the acceptance to that exact graph
position (`atty` only under `env_logger`, `env_logger <0.10.0` only under
the vendored `libsignal-protocol`); a lockfile-only guard fails if `atty`
ever resolves into a non-dev build graph. Its named clearing owner,
WI#367, was closed on 2026-10-18 with the `env_logger` 0.6.2 dev-dep edge
still in place.

Two security upgrades landed late in the cycle on top of the semver sweep:

- **rustls 0.23.45 → 0.23.46** (WI#409), fixing RUSTSEC-2026-0346/0347/
  0348/0349. The daemon manifest now requires `>=0.23.46` on the 0.23 line
  and `ic/deny.toml` bans the vulnerable range so a downgrade fails the
  gate; the reviewed Pebble upstream lock was upgraded too, with its
  Dockerfile digest refreshed (rebuild/promotion required to deploy). Only
  0346/0349 remain accepted, on the opt-in rustls 0.22.4 libSQL chain.
- **Wasmtime/WASI 36.0.17 → 36.0.18** (Cranelift 0.123.17 → 0.123.18),
  fixing RUSTSEC-2026-0352/0363 — a filesystem sandbox escape and a
  component-handle host-memory exhaustion. The patch introduces a default
  per-store cap of 100,000 live component handles; exhausting it traps.
  Rebuild **and restart** the daemon and XMPP bridge to deploy it.

Verification for both is recorded in `docs/ops/v2.1.6.0_CARGO_DENY_AUDIT.md`
(October 14 update, pipeline 5710 addendum, the October 16 zip-yank
follow-up described under Dependency Sweep below, and the October 17
`unsound = "all"` widening follow-up described above).

### Pending-Gate Lifecycle (WI#332)

`GET /api/chat/pending-gates` and the single-gate lookup drop gates whose
thread vanished, left `Waiting`, or changed owner; a 60-second sweep
discards them from memory and persistence with a warn naming
user/thread/request/gate/action ids and a `thread_terminated` broadcast so
open prompts clear. Authentication-fallback gates are exempt and live until
expiry exactly as before. The sweep compares request ids under lock so a
replacement gate parked mid-read always wins, and `resolve_gate` re-checks
liveness after taking the gate — an approval racing a thread termination
fails closed instead of executing the gated action. Approvals on
engine-routed channels that match no engine gate fall through to v1 with a
warn naming ids and scope.

### Mission Shutdown Stamps and Outcome Notices (WI#337, WI#339, WI#341)

`pending_shutdown_interruption` is now a map keyed by thread id, so a cron
tick and an armed retry no longer shadow each other; the legacy single-slot
shape is still read and folded into the map on the next write. The boot
sweep isolates failures per mission (a corrupt row blocks only itself) and
per stamp, admits armed retries only after the fire guard drops, restores a
stamp in memory if its resolution fails, and resolves overlapping fires
newest-wins with the superseded fire resolved record-only. Interruption
notices are sent only after the retry marker persists, so a crash can never
leave a delivered "retry scheduled" notice without the persisted retry.

Outcome notices owed at shutdown (completion, max-iterations, budget
exhaustion, exhausted retry chains) are stamped as
`pending_outcome_notification` and delivered by a new boot sweep with the
stamp removal as acknowledgement — best-effort, at-least-once, matching the
existing WI#301/WI#276 semantics. Stamps older than 24 hours are cleared as
stale, and `resolve_stamped_interruption` now mirrors the restart recorder's
parity checks (thread type, project, owner, `thread_history` membership):
partial forgeries and corrupt stamps are cleared with a warning instead of
arming retries (WI#338; full write-path reservation is WI#322, planned for
v2.1.7.0). A known stuck-key abort window is tracked as WI#394.

### Mission Webhook and Rate-Limit Hardening (WI#352, WI#359)

A mission stored with an empty or unpresentable webhook secret is dormant:
the fire path's constant-time compare is guarded by a usable-stored-secret
check (`ct_eq("", "")` no longer authenticates), and create/update reject
unusable secrets — interior HTAB is allowed since the wire carries it, but
leading/trailing whitespace and non-visible-ASCII can never be presented.
The single-owner webhook-path rule is enforced on create, update, and
resume under a serialized check-and-save, and a startup warning surfaces
legacy multi-owner path conflicts.

Run-spawning mission verbs share an aggregate per-user rate bucket (60/min,
600/h) alongside the per-verb buckets, charged before `ToolCall` hooks so a
refused call spends no tokens. `max_threads_per_day` saturates at
`u32::MAX` instead of wrapping, and same-secret create dedup reports
`webhook_secret_applied: false` without becoming a secret-equality oracle.
WI#359 was closed on 2026-10-14. WI#392 (closed 2026-10-17) then fixed
its approval `display_parameters` and http `ToolCall` hook payload
residuals and widened the credential-key predicate; the longer-term
non-agent stop path was left carried on WI#359 and has no open owner.

### Credential Redaction on HTTP Surfaces (WI#359)

`redact_event_params` now covers the `http`/`web_fetch` surfaces end to
end: string-form and structured `headers` are redacted by normalized name
against one shared credential-key list *and* by auth-scheme value prefix
(`Bearer `, `Basic `, `AWS4-HMAC-SHA256 `, …); URLs lose userinfo and the
fragment, and credential-like query values become `[REDACTED]` with
untouched pairs preserved byte-for-byte; string-form JSON and form bodies
are recursively credential-redacted before re-serialization. reqwest error
text is stripped of the full request URL, closing a query-injection leak
through unknown key names. `summarize_params` skips credential-keyed values
generically, so `ToolStarted`/`ToolCompleted` summaries and persisted
`params_summary` never carry them. A late follow-up (2026-10-17) extends
the shared credential-key matcher to numbered fields: a trailing ASCII
digit counter on each folded word (`pass1`, `smtp_pass2`, `sid2` —
multi-digit, separator, and fullwidth forms included) is trimmed before
the short-name and exception checks, so numbered password/session keys
are redacted like their unnumbered forms while ordinary keys
(`side2`, `bypass2`, `Compass3`, `passenger1`) stay untouched.

### Pebble Toolchain Anchors and Build Smoke Gate (WI#334)

The pebble fat image's PATH-anchor RUN block restores the rustup proxies,
`GOROOT/bin`, and `GOPATH/bin` in exact image-ENV order (pre-created so the
guard holds from the first task command), and the block is executed
verbatim under dash in the ci-config lane on every run. The mt-admin smoke
gate now probes the `:candidate` tag with `--pull=never` against the same
container runtime that built it (explicit `CONTAINER_RT` passthrough,
podman-first autodetect otherwise), re-tagging `:latest` only after the
probe passes — a failing fat build can no longer leave the failing image as
`:latest`.

### Tenant Proxy URL Boundary and OpenRC Proxy Transition (WI#376, WI#393, WI#410)

The cycle's highest-severity fix: a tenant-controlled `TENSORZERO_URL` from
`proxy.env` was copied into the OpenRC proxy init script and
shell-evaluated as **root** during any OpenRC operation. Rendered units now
pass the fixed `--tensorzero-env` option and the proxy reads the literal
value after `command_user` drops privileges; both renderers read
`proxy.env` as the tenant with ownership, permission, and symlink checks;
legacy fleet proxy units are neutralized before any other OpenRC operation;
and mt-admin fails closed while proxy units await migration (!1547). The
migration is a **required** upgrade step for every OpenRC multi-tenant
host — see Upgrade Notes.

The transition itself was then hardened against silently-wrong states. The
OpenRC dispatch preflight no longer turns a detection failure into a false
"migration required" refusal — only a positively detected systemd host
skips the legacy-unit guard, fail-closed by construction. The
tenant-restore probe classifies the rc+printed-state pair so a
crash-looping `Restart=always` daemon read as `activating` can never reach
`pg_restore --clean`, and a symlinked or non-regular init entry refuses
with a named reason instead of counting as "already stopped" during
remove/upgrade.

The safe-unit contract then became real template equality (WI#393):
renderer and checker share one body renderer and one PATH-independent
python3 resolver (`type -P` under a fixed PATH — a caller's shell function
or shadow PATH can no longer flip a fleet-wide verdict), the per-unit check
compares byte-for-byte with `cmp`, and a per-process verdict cache keyed on
the unit's dev:inode:mtime(ns):size signature plus the resolved interpreter
collapses the per-operation cost (a 20-unit fleet `status` renders 20
bodies once, then stats). A unit whose interpreter spelling differs from
the canonical one is refused until re-rendered; `upgrade-tenant` re-renders
automatically.

Finally (WI#410), the two root cron execution paths — `health-openrc.sh`
and `lunarwing-self-heal.sh` — re-check the same contract at the top of
every tick, before any `rc-service` invocation, via the shared
`openrc-proxy-unit-contract.sh` library: an unsafe unit refuses the tick
with a critical health report (empty service metrics, so self-heal gains no
restart target) and a self-heal page through `escalate_service`. The
migration-window guarantee no longer depends on arming-time state.

The canonical interpreter resolver now covers the weechat and darkirc
renderers too (WI#415, landed 2026-10-17): the five remaining OpenRC render
sites that baked the caller's `$(command -v python3)` (the weechat tmux
`command_args`, the weechat `start()`, the weechat relay `relay_exec_prefix`,
the weechat `adapter_command` default, and the darkirc `darkirc_adapter_command`
default) route through the shared `_openrc_proxy_python3` — the same
PATH-independent resolver the proxy contract uses (`type -P` under the fixed
`/usr/local/bin:/usr/bin:/bin`, with the `LUNARWING_MT_PROXY_PYTHON3`
override). `_render_weechat_openrc_unit` resolves once up front and fails
the render closed when nothing resolves; tenant renders resolve lazily, so
python3-free tenants with neither service render unchanged. WI#415 was
closed on 2026-10-18; the systemd renderer's equivalent caller-PATH sites
(`07-units-systemd.sh`) remain out of its scope.

### opencode v1 Crash-Line Enrichment (WI#374)

WI#374 was filed against opencode2 and diagnosed as the v1 lane
(`mode = "opencode"`). When an opencode v1 serve task fails with the bare
`Unexpected server error` wrapper, the bridge now attaches the matching
crash line from serve's structured log: bounded to a ≤8 KiB window starting
at the task-start offset, `level=ERROR` lines only, correlated by the exact
`err_<8 hex>` request ref (no ref, no attach), code-point-safe line caps,
and known secret values scrubbed before the tail enters the job record. The
log is opened `O_RDONLY|O_NOFOLLOW|O_NONBLOCK` so a planted FIFO, symlink,
or renamed-over directory cannot wedge or redirect the read. WI#374 itself
was closed as a duplicate of WI#333 — the v1.18.25 lane crash stays
upstream-blocked; the enrichment exists so its failures are diagnosable.

### Worker Credential Environment Hygiene (WI#373)

The `AGENT_AUTH_TOKEN` bridge credential no longer rides into spawned task
processes in any worker lane: kimi deletes it after all env merges, the
opencode/opencode2 entrypoints run `opencode serve` under
`env -u AGENT_AUTH_TOKEN` (the separately exec'd bridge keeps its copy),
pebble rebuilds the child env from scratch (`env_clear` + an allowlist),
and all six one-shot exec sites (`cli`/`acp` modes) strip it too.
Task-supplied env entries reusing the key are dropped with a warning.

This is **hygiene, not a confidentiality boundary**: the token remains in
the environment of same-uid long-lived processes (PID 1, the entrypoint
shell, the health server, the bridge), so `cat /proc/1/environ` hands it
back — only pebble `cli`, which exec-replaces PID 1, is total. WI#373 was
closed on 2026-10-14; real isolation (fd or root-only file handoff,
separate task uid) remains WI#400. Worker images must be rebuilt for the
strips to reach live tenants.

### Daily-Log Backup Cap Fail-Closed (WI#335)

The fail-closed daily-log backup lane now aborts on **every** rotation or
`.bak`-write error, not just the generation cap — a swallowed
infrastructure failure would otherwise overwrite the rolling slot (exactly
where the poison sits right after it lands) without a proven backup.
Operator lanes keep the warn-and-proceed behavior. The cap refusal is now
classified `ToolError::NotAuthorized` (was `ExecutionFailed`, which read as
transient and invited retries) and emits a `lunarwing::safety` warn naming
file, scope, and generations — an operator action unblocks it; retries will
not. The evidence window is unchanged (eight backup generations, so nine
agent replaces per day, then operator-only); the unprobed-slot concurrency
window remains the accepted WI#288 residual.

The rotation audit trail was then closed out (WI#336, closed 2026-10-16).
The discard `lunarwing::safety` line is now emitted before the destructive
generation shift, recording the generation marked for discard; a successful
roll-aside into a numbered generation emits its own line (previously only
the `.bak` "preserved" line fired, never naming the slot); and a failed
rotation on the warn-and-proceed lane fingerprints the un-rotated `.bak`
content whose slot is about to be overwritten. Because the daemon log
scrubber redacts any standalone 64-hex value as a possible gateway token,
the fingerprint fields log the first 128 bits of the blake3 hex (the
`b3sum --length 16` shape) — a prefix that survives every production log
sink while distinguishing a day's handful of generations.

### Pagination Envelope Tail in Earlier-Turn Tool Context (WI#351)

Earlier-turn tool results keep their pagination envelope tail. Both the v1
session rebuild and the persisted `role=tool_calls` record head-cut tool
results at 1000 bytes, amputating the `next_cursor`/`truncated`/`complete`
envelope list tools serialize at the tail — a later "next page" turn then
improvised cursors. Successful tool results at both layers now use
`truncate_preview_middle` (byte-budgeted head+tail middle elision,
char-boundary safe), so the real cursor survives; error previews stay
head-only, and the 500-byte display preview is unchanged.

### Mission-Thread Steering Bounds (WI#356)

Mission threads no longer hold lifecycle verbs: `mission_create`,
`mission_delete`, and `mission_resume` are refused to Mission threads
before the supervision pause (a resume would un-delete the
conversation-insights mission's opt-out), mirrored as `callable: false`
with the real reason in `tool_info`. Their `mission_update` is now bound to
an allowlist of steering fields (`goal`, `current_focus`,
`success_criteria`, and the retry-policy scalars) — every other field,
including renames, defaults to refused — so a mission thread can no longer
turn any mission into a webhook-triggered one with a model-chosen secret,
zero its daily budget, or rename it. Owner-scoped steering (fire/pause,
goal/focus rewrites) stays deliberate and is recorded in
`docs/architecture/ENGINE-V2.md`. WI#356 was closed on 2026-10-16.

### Worker Exposure Policy (WI#357)

Workers must never self-expose: LunarWing owns every public exposure path.
For the opencode2 lane this cycle lands the naive-use guards and build-time
controls: the in-image `opencode` wrapper scans **every** argv token and
refuses self-exposure shapes (`service`/`pair`, `--service`, every
`--hostname` occurrence); scripts and config are root-owned and read-only
in the image, with the runtime config re-derived from the read-only copy at
every boot; the OpenTunnel client is compiled out with a marker scan; the
installed wrapper's byte identity is anchored to the checkout via
`LUNARWING_WRAPPER_SHA256`; and the CI Dockerfile guard now parses
continuation glue, parser directives, heredocs, and BOM/NUL bytes the way
BuildKit does, so a hidden instruction cannot slip past the assertions. On
the host, `mt-admin worker-listeners <tenant>` audits every worker
container's listeners host-side from `/proc` (TCP **and** UDP, with
all-interfaces and inspect-failure findings).

These layers are naive-use guards, **not** a boundary against a compromised
agent — only the deferred egress allowlist (control 1) is; the automated
runtime-smoke suite is likewise deferred to WI#424 for 2.1.7.0. WI#427
records the maintainer's explicit approval to advance opencode2 to
**v2.0.32** through these guards; !1490 remains closed. The bump proceeds
with WI#369's SDK-gate blind spots still open. A real job ran end to end
on the temporary systemd tenant spitfire (gateway chat → LLM loop →
`create_job` → approval gate → resume → WS dispatch → the v2.0.32 executor)
to a normal completion, with pre/post exposure checks clean (`service.json`
disabled, no OpenTunnel identity dir, the wrapper refusing
remote-service/pair shapes, and `worker-listeners` reporting nothing) — but
on an image built from `4db886f7b`, before the final fix commit `3a24626fa`
(the wrapper's `upgrade`/`update` refusal, `OPENCODE_DISABLE_AUTOUPDATE=1`,
and the restored `external_directory` ask). The merged image passed the
protected `worker-opencode2-candidate` and `worker-opencode2-scan` jobs on
master pipeline 5955 but has not run a tenant job, and WI#427 stays open
(its Gentoo/OpenRC rollout and a real job on the merged image are pending).
See `docs/ops/ITEM71_OPENCODE2_V2.0.32_PUBLIC_HTTPS_TUNNEL.md` for the
security write-up of upstream's Public HTTPS Tunnel feature (architecture,
threat model, control set, residual risks) and the v2.0.32 record in
`docs/ops/WORKER-CONTAINERS.md`.

### Sandbox Proxy Credential-Echo Scrubbing (WI#395)

The sandbox HTTP proxy injects credentials into allowlisted outbound
requests; this cycle closes the echo channels on the way back. Forwarded
response headers are byte-scrubbed for the raw and form-encoded spellings
of every injected credential — redirect `Location`/`Link` echoes included —
with two-round percent-decode detection on proxy headers; response bodies
are scrubbed at the byte level; and `headers_json` is parsed and redacted
so JSON-escaped spellings cannot slip through. Credential-injected
exchanges strip every guest `Accept-Encoding` spelling, send exactly
`identity`, and refuse any response body (empty bodies included — the
WI#429 over-refusal below) whose `Content-Encoding` carries a non-identity
token or whose transfer coding the scrub cannot inspect (tightened by
WI#419 below) — an uninspectable body is never forwarded blind. The WASM guest HTTP wrapper
applies one guest-facing redaction over the whole outcome, covering
validator errors that quote the injected URL. Residuals (re-encoded body
echoes, splice re-checks, Range/206 handling) are recorded in
`ic/src/NETWORK_SECURITY.md`. WI#395 was closed on 2026-10-16.

The coding-opacity rule was then tightened to hyper's exact de-chunking
behavior (WI#419, landed 2026-10-17): hyper removes exactly one chunked
layer, and only when the last comma token of the last `Transfer-Encoding`
field is `chunked` — so `chunked,` (trailing comma), `chunked` followed by
an empty field, `chunked, chunked`, and double-chunked bodies all reach
the echo scrub with their framing intact. A transfer-coding-only body is
inspectable now only when it has exactly one `chunked` token in last
position; every other shape — including unreadable or empty
`Transfer-Encoding` fields — is opaque and refused, and the refusal covers
retained-framing spellings regardless of whether hyper de-chunks them.
Known residuals from the same review are filed as WI#429 (the proxy's
empty-body over-refusal: HEAD/304/204 plus retained framings still give
502) and WI#430 (the analogous gap in the WASM tool wrapper's
`guest_body_encoding_is_opaque`); both remain open. WI#419 itself was
closed on 2026-10-17.

### Channel WASM Host Credential Redaction (WI#408)

The channel-host side of the same class is closed at the trait boundary
(WI#408, landed 2026-10-17). Channel WASM host functions that inject
credentials (`http_request` URL placeholders, query credentials, header
credentials) now scrub every echo spelling on the way back to the guest:
raw, `urlencoding::encode`, and form (`query_pairs_mut`) spellings —
including the upstream-parsed query forms (`+` read as a space, `%XX`
escapes decoded) — plus JSON-escaped spellings in `headers_json` and
byte-level body scrubbing. One guest-facing redaction at the trait
boundary covers success and error outcomes alike, so allowlist deny
reasons, leak-scan previews, and send errors no longer quote the injected
URL or its parts; body-read errors strip the URL too. Deny reasons are
computed from the guest's own URL, and a placeholder value that cannot
survive parsing (containing `#` or an ASCII control character) is
refused before injection. Credentialed channel exchanges force
`Accept-Encoding: identity`, are never followed across redirects, and
refuse opaque or retained-framed bodies (the same hyper-exact
transfer-coding rule as WI#419). The pins run the real host functions
against live loopback upstreams. This is the channel host only: WASM tools
go through the separate tool wrapper (`ic/src/tools/wasm/wrapper.rs`),
whose retained-framing gap is WI#430. WI#408 was closed on 2026-10-18; the
four channel-host residuals listed in `ic/src/NETWORK_SECURITY.md` are now
tracked on WI#432.

### Self-Heal Backoff Cap (WI#377)

`lunarwing-self-heal.sh`'s `compute_backoff()` honoured a custom
`BACKOFF_MAX`/`--backoff-max` only up to attempt 20 — the deep-retry
short-cut expanded `${max:-3600}` before the variable existed, silently
ignoring the cap exactly in the deep-retry regime. The declarations and
numeric validation now sit above the short-cut, which returns the real cap;
an invalid cap warns and yields 0 like the ≤20 path.

### Single-Node Endpoint Defaults Removed (WI#402)

The deprecated `setup-instance.sh` and the XMPP test-env harness no longer
carry built-in LLM/TensorZero endpoint defaults: `setup-instance.sh`
requires `--llm-base-url` (dying with guidance before any file is
written), and `lunarwing-xmpp-test-env.sh` resolves the TensorZero
gateway from `LUNARWING_TEST_TENSORZERO_URL` or `TENSORZERO_URL` —
refusing at every bake/use point (init, fresh `proxy.env`, the
`ENABLE_PROXY=false` daemon env, proxy start, unit renders) when neither
knob is set, while previously initialized roots keep working through the
value baked in their `proxy.env`. A fresh-root refusal leaves no `env/`
files behind (no fresh tokens or keys from a failed bring-up), and
`doctor` diagnoses the missing knob instead of probing an empty URL.
`lunarwing_toolcall_diag.py` takes its gateway host from `--host` —
required unless `--default` explicitly opts into the historical LAN
address. WI#402 was closed on 2026-10-18. The standalone TensorZero
proxies under `tensorzero-proxy-configurations/` still default to the
developer-LAN endpoint; that residual was tracked as WI#438, closed the
same day with the defaults unchanged.

### Engine Auto-Import Pre-Injection (WI#364)

Monty code blocks generated by the LLM no longer fail on missing stdlib
imports: the orchestrator pre-injects `import <module>` for any dotted
reference (`module.`) to the six Monty-available stdlib modules
(`asyncio`, `json`, `re`, `math`, `datetime`, `sys`) that is referenced,
not already imported, and not called as a bare function (the builtin
`json` tool keeps working). Wired into the single choke point every
LLM-generated block passes through before execution — direct CodeAct
steps, orchestrator `__execute_code_step__` calls, and `rlm_query`
children alike — with explicit imports taking precedence, comment tails
and `from`/alias bindings respected, and injected imports never shifting
reported error line numbers. Import-clean code executes byte-identically;
the scan is a boundary matcher, not a full parser (documented limitations
in the helper docstring and MONTY.md).

### mt-admin Tenant-Ownership Hardening (WI#390, WI#416)

Two hardening passes landed in the mt-admin modules. WI#390 (closed
2026-10-17): mt-admin no longer writes tenant env and config files as root
with `cat >` then `chown`; the converted writers stage content root-private
and commit it through the tenant-side atomic writer (root never opens the
tenant-writable path), their value reads come from root-private snapshots
taken as the tenant (the tenant env renders, the worker base-URL backfill,
and the four `configure_*` functions: `configure_opencode`,
`configure_opencode2`, `configure_kimi`, `configure_study_mcp`), and a
refused tenant-side commit now fails `add-tenant`/`upgrade-tenant` instead
of being swallowed. Root reads and writes WI#390 did not convert
(`tenant_pg_password`, `resolve_worker_auth_token`, the study-notes mount
sources) remain open as WI#423, and the rootful pebble bind-source
re-resolution as WI#422. WI#416 (partial —
three of its six named sites): the trailing root `chown -R` in
`configure_weechat_relay` is gone (the promoted tree is already
tenant-owned; after a symlink swap that chown would have recursively
chowned a victim tenant's WeeChat home), the relay start lock is created
via a tenant-side `noclobber` drop, and the SSH key staging drops its
redundant root chown. The remaining WI#416 sites (the `chmod 2770`
workspace prep, the add-tenant re-run env chmod, the study-seed root
mktemp) stand unconverted. WI#390 was closed on 2026-10-17 and WI#416 on
2026-10-18.

### Kimi Worker 2.2.0 and Environment/Config Pins (WI#411, WI#412)

The kimi external worker advances to `@moonshot-ai/kimi-code` **2.2.0**
(npm `latest` at bump time; the contract lane — stream-json
print mode, option conflicts, the WI#206 PreToolUse guard table, the WI#324
MCP handshake — was verified 4/4 against the real 2.2.0 binary in a host
run; the in-image gate runs on the next protected candidate build). On top of
the bump, the executor and entrypoint now also scrub
`KIMI_CODE_TRUST_WORKSPACE` from task environments alongside the existing
guard variables (WI#411), and the lane config pins `max_output_size =
131072` on the FrontierCODEK model with a TOML-parsing CI pin, restoring
the request cap the 2.1.1 line sent (WI#412; verified on the wire against
a rebuilt image on a live tenant — every captured `chat/completions`
request carries `max_tokens: 131072`; the 2,095-token answer completed
normally but is short enough that the run confirms the wire value, not the
cap's effect). Both
fixes reach tenants only with a rebuilt/promoted kimi image — see Upgrade
Notes.

### Worker Image Rollout and Traceable mt-admin Builds

Rebuilt worker images now reach running tenants on both init systems: the
worker-container launch contract hash gained the image ID, so the first
`start-tenant`/`restart-tenant` after upgrading mt-admin recreates every
worker container once (a rebuilt image previously kept serving the old
container until an operator removed it by hand — that is how a promoted
kimi 2.2.0 image stayed in service without the WI#411/WI#412 fixes).
mt-admin builds are also traceable: every `build-*-worker` stamps
`org.opencontainers.image.revision` (the checkout HEAD, `-dirty`, or
`unknown` outside git), as CI builds already did.

### Documentation and Open-Item Audit Passes

All documentation under `docs/architecture/` and `docs/guides/` was
revalidated against the release tree (mt-admin quickstart, testing guide,
gateway/voice, DarkIRC adapter, contributor onboarding, ENGINE-V2,
self-heal and harness docs — with stale anchors repaired and the WI#376
operator instructions corrected to the real refusal behavior). A new
point-in-time audit, `docs/ops/v2.1.6.0_OPEN_WORK_ITEMS_STATUS.md` (a
2026-10-14 01:05Z snapshot — work items filed after it, WI#402 through
WI#415, postdate it), reconciles every open work item by release label and
feeds the known-issues list below.

### Dependency Sweep and Vendor Refresh

All eleven LunarWing-owned release packages advance from `2.1.5` to
`2.1.6`: the daemon, five workspace crates, the XMPP bridge, and four WASM
channels, with every inter-crate path-dependency requirement. All four
channel registry packages and all nine tool registry packages advanced
together (see Compatibility), and a tracked-manifest/lock audit confirms no
project-owned package or path dependency remains at 2.1.4 or 2.1.5.

External crates took MSRV-respecting semver-compatible updates across all
26 swept lockfile roots (tokio 1.53.2, hyper 1.12.0, h2 0.4.20, jiff
0.2.38, toml 1.1.8, uuid 1.28.0, zeroize 1.9.1, html-to-markdown-rs 3.17.2,
syn 3.0.7, among others), and `ic/vendor/` was regenerated — 1150
directories: 1148 vendored crates plus the two pre-vendored libsignal path
dependencies — with the rig-core patch restored and marker-verified. Two
security upgrades landed late-cycle on top of the sweep (rustls 0.23.46,
Wasmtime/WASI 36.0.18 — see Crate Security Re-Audit). The
`pebble4lunarwing/upstream/Cargo.lock` build-input pin was deliberately
untouched by the sweep, then upgraded separately for the rustls advisories
with its Dockerfile digest refreshed. External worker pins: kimi-code
advanced to 2.2.0; opencode v1.18.25 and the Pebble upstream ref are
unchanged; opencode2 advances v2.0.24 → v2.0.32 per WI#427 (OpenTunnel
compiled out; see Worker Exposure Policy).

A crates.io mass-yank forced one late major bump (WI#417, closed
2026-10-16): every `zip` release below 9.0.3 was yanked on 2026-10-16
behind three upstream advisories against `extract()`, failing the
`yanked = "deny"` cargo-deny gates on every pipeline. LunarWing never
extracts archives to disk — both call sites read entries in memory behind
size caps — so no code path was exposed, and `zip` advanced 2.4.2 → 9.0.4
as dependency hygiene (a fully yanked major line receives no further
fixes). The deflate backend moves from miniz_oxide to the already-vendored
pure-Rust zlib-rs; entry-name decoding is now fallible (undecodable names
are skipped by the slide/sheet scans and reported as `<undecodable entry
name>`), and zip 9's `enclosed_name()` strips a leading root instead of
refusing absolute entries — `..` traversal is still refused, and neither
caller extracts. The vendor set is unchanged in size (typed-path added,
the orphaned derive_arbitrary dropped). Exposure analysis and verification
are recorded in the audit doc's 2026-10-16 follow-up.

## Upgrade Notes

1. **Back up state.** Preserve PostgreSQL, tenant environment files,
   `/etc/lunarwing/ports.json`, workspace state, rendered units, worker
   data, voice models, and channel state. The port registry stays at v17
   and SQL stays at V28 — no migration runs for this release.
2. **OpenRC multi-tenant hosts (WI#376, required).** In a maintenance
   window with the health cron and OpenRC watchdog paused, run `sudo
   ic/scripts/lunarwing-mt-admin.sh render-units --all-proxies` from the
   patched checkout (legacy units are archived under
   `/var/lib/lunarwing/openrc-proxy-backups`). Then update
   `tensorzero-proxy-configurations/lunarwing-proxy.py` in **every proxy
   tenant's checkout** before anything restarts that tenant — a reboot,
   self-heal run, watchdog run, or `start-tenant` otherwise takes the whole
   tenant down until its checkout is updated — and restart through
   mt-admin. Until the migration succeeds, every OpenRC mt-admin command
   except `help`, the migration itself, and `upgrade-tenant` refuses (exit
   1; `writers-active` reports 2 = unknown). On systemd hosts, proxy URL
   changes still need `render-units <tenant>` plus a restart. Before
   resuming the health cron, refresh the host health pipeline: run
   `upgrade-tenant` on any tenant (it re-syncs the installed health lib)
   or `ic/scripts/enable-health-fleet.sh`. The WI#410 per-tick guard and
   the WI#377 backoff fix ship only in that copy. A synced lib refuses
   every OpenRC tick and pages while any legacy proxy unit is still
   installed. See the "Proxy URL boundary (WI#376)" section of
   `docs/ops/MULTITENANCY-PRODUCTION.md`.
3. **Turso sync users: rebuild with the new feature (breaking).** Installs
   configured for Turso sync (`LIBSQL_URL` + `LIBSQL_AUTH_TOKEN` set) **will
   not start** on default v2.1.6.0 builds: the daemon fails closed at
   startup with an error naming the `libsql-remote-replica` cargo feature,
   and `lunarwing doctor` reports the same. To keep Turso sync, rebuild
   with `--features libsql-remote-replica` (and accept the pinned
   rustls-webpki 0.102 / h2 0.3 / rustls 0.22.4 exposure recorded in
   `docs/ops/v2.1.6.0_CARGO_DENY_AUDIT.md`). Do **not** simply unset
   `LIBSQL_URL`: the existing replica file then becomes a diverging
   standalone database whose writes never reach the Turso primary — the
   setup wizard now warns about this explicitly. Every shipped build path
   (mt-admin `ic/scripts/mt-admin-lib/05-tenant-build.sh`,
   `scripts/build-lunarwing.sh`, `ic/Dockerfile`, `ic/Dockerfile.worker`)
   uses default features, so shipped binaries fail loudly rather than
   silently diverging; mt-admin currently exposes no opt-in feature knob.
4. **Rebuild and promote the worker images.** The WI#324 kimi and opencode
   tool-proxy bridges, the WI#325 cron bootstrap, the WI#328 control channel,
   the WI#330 placeholder guards, the WI#373 credential strips, and the
   WI#411/WI#412 kimi env/config pins all ship in the worker images.
   Rebuild all four worker images (`build-<worker>-worker` or
   `build-tenant --with-<worker>`), promote each through the normal
   candidate → scan → promotion path, then `restart-tenant` each tenant.
   The version pins alone are not the content: the opencode2 image carries
   the WI#357 exposure controls and the WI#329 SDK gate, the opencode image
   carries the WI#374 crash-line enrichment, the WI#373 credential strip,
   and the WI#324 serve-process plugin, and the pebble image carries
   WI#334 and the reviewed-lock rustls
   upgrade — opencode stays v1.18.25, while opencode2 advances to v2.0.32
   under WI#427. Skipping those rebuilds leaves WI#357 undeployed and the
   opencode tool proxy unsurfaced. Preserve
   opencode2 state before the first new-version boot: upstream migrates its
   database, so rolling back the image alone is not a verified rollback.
   kimi-code is upgraded to **2.2.0** (an earlier 2.2.0 image promoted
   during the cycle predates the WI#411/WI#412 fixes — rebuild from the
   release tree and recreate the tenant containers; mt-admin now recreates
   worker containers automatically when the image ID changes, on OpenRC as
   well). Note the one-time rollout effect: the first
   `start-tenant`/`restart-tenant` after upgrading mt-admin recreates every
   worker container once, because the stored contract hash gains the image
   ID. mt-admin builds now stamp `org.opencontainers.image.revision`
   (checkout HEAD, `-dirty`, or `unknown`) on the images they build.
   `upgrade-tenant` (and `patch-env <tenant>`) backfill enabled workers'
   base URLs; the `build-<worker>-worker` / `build-tenant --with-<worker>`
   plus `restart-tenant` path above does not. On a tenant you rebuild that
   way, run `patch-env <tenant>` first, or set the URL with
   `configure-kimi`/`configure-opencode`/`configure-opencode2 <tenant>
   --base-url <url>`: the rebuilt images refuse to start while a worker's
   base URL is still the `.invalid` placeholder.
5. **Deploy rebuilt components together.** Rebuild the daemon, XMPP bridge,
   and installed WASM channels and tools; rebuild/reinstall all four
   channel and nine tool registry updates through the normal deployment
   path. The rustls 0.23.46 and Wasmtime/WASI 36.0.18 security fixes ship
   in the daemon and XMPP bridge binaries — rebuild **and restart** both to
   deploy them.
6. **Review cron worker authority.** Cron tick jobs draw from the new
   separate `cron.tool_allowlist` (default empty — every proxied tick call
   denied), and a cron-mode worker's worker-level `tool_allowlist` is now
   stripped at load with an `error!`. Ordinary dispatches to a cron-mode
   worker get no proxied tools. Never point a `tool_allowlist`-bearing
   entry at the same endpoint as a `mode = "cron"` entry — the shared
   endpoint is stripped (normalized host:port comparison, fails closed) —
   and give each cron mission its own container: two cron entries sharing
   one container with *differing* `cron.tool_allowlist` grants are now
   detected at load and every cron grant on that listener is stripped
   (identical grants stay, with a warning). A control-state read failure
   now starts the affected mission paused with a `durability_error`
   instead of resuming it blind (WI#371); resolve the fault and resume.
7. **Daily-log backup cap refusals are classified (WI#335).** A replace
   that would exceed the daily-log backup generations now fails with
   `NotAuthorized` (previously a transient-looking execution error): an
   operator action (rotating or archiving the day's `.bak` generations)
   unblocks it; retries will not.
8. **Expect honest gate cleanup.** Gates parked on dead or departed threads
   are now swept (warn + `thread_terminated` broadcast) instead of
   lingering; approvals landing after a thread ends fail closed with a
   "thread ended" reply. Authentication-fallback gates are unaffected.
9. **Expect mission notices after the first boot.** Threads interrupted by
   a pre-upgrade shutdown may earn one boot-time retry notice as the new
   per-thread stamps and owed-outcome notices resolve; a `2.1.5.0 →
   2.1.6.0` upgrade emits no spurious "re-armed" notices for healthy retry
   markers.
10. **Single-node harness defaults are gone (WI#402).** `setup-instance.sh`
   (already deprecated behind `LUNARWING_ONBOARD_LEGACY_OK=1`) now
   requires `--llm-base-url`, and the XMPP test-env harness requires
   `LUNARWING_TEST_TENSORZERO_URL` (or `TENSORZERO_URL`) — previously
   initialized roots keep their baked `proxy.env` value, but fresh
   bring-ups die with guidance instead of silently dialing the historical
   LAN default. Pass an explicit endpoint.

### Security, Deployment, and Release Limitations

**Worker proxy scope.** The kimi and opencode lanes ship the worker-side
tool-proxy bridge in this release (an MCP shim for kimi, a serve-process
plugin for opencode); opencode2/pebble workers have no
`mcp__lunarwing__*`-equivalent surface yet. Proxied tools are off by
default: a worker's `tool_allowlist` is empty unless an operator sets it,
and mt-admin renders none. No real worker has made a granted proxied call
yet, so WI#324's real-worker acceptance is still open, and the opencode
plugin ships only with its rebuilt image on a lane that stays frozen at
v1.18.25 pending the WI#333 decision. On both lanes the tool grant is
ambient for every process in the task (untrusted repo scripts can call the
relay). Workspace content can shadow or disable the bridge — `.mcp.json`
files for kimi, the `.opencode/` config, plugins, and tools for opencode —
and on the opencode lane any process in the container can create or rename
a session to carry another task's title, which changes only which job a
call is attributed to. The hub stays authoritative on the capability set.
`memory_read` grants disclose workspace content to egress-enabled workers;
see `docs/proposals/EXTERNAL-WORKER-TOOL-PROXY.md` and
`docs/ops/WORKER-CONTAINERS.md`.

**Worker credential hygiene.** The WI#373 strips are env hygiene, not
isolation (see the section above); WI#400 tracks the real boundary.

**Known-broken lane.** The opencode **v1** worker lane (v1.18.25) still
crashes inside `opencode serve` before any model call (WI#333); the new
log enrichment surfaces its crash lines in job records, but the lane
decision (bump or retire, planned for v2.1.7.0) is unchanged.

**CI registry rate limits.** HTTP 429s from the crate registry and the
`get_sources` fetch force manual retries on master pipelines; no work item
owns this yet.

**Accepted advisories.** Eight vulnerability advisories, two unsound
notices, and two unmaintained notices (twelve IDs) ship accepted — see
`docs/ops/v2.1.6.0_CARGO_DENY_AUDIT.md`. Of the eight vulnerabilities, only
rsa (via russh) compiles into default builds; the rustls-webpki 0.102 /
h2 0.3 chain and the accepted rustls 0.22.4 pair (RUSTSEC-2026-0346/0349)
exist only in opt-in `libsql-remote-replica` builds, under the WI#418
standing decision record (`docs/ops/v2.1.6.0_WI418_LIBSQL_LEGACY_CHAIN.md`,
2026-10-16), which re-homes the seven legacy-chain acceptances, adopts the
feature gate as the long-term mitigation, and sets their per-release
re-verification. The twelfth acceptance — `atty` (RUSTSEC-2021-0145),
surfaced by the WI#360 `unsound = "all"` widening — is dev-graph only (see
Crate Security Re-Audit). The clearing owners named in `ic/deny.toml`
(WI#368 for rsa, WI#367 for `failure`/`rental`/`atty`, WI#418 for the
libsql chain) were all closed on 2026-10-18 with every chain still in
`ic/Cargo.lock`, so no accepted advisory has an open clearing owner at
release; WI#363, the CI guard that would require one, is still open.

**Known issues / open at release.** Reconciled per item in
`docs/ops/v2.1.6.0_OPEN_WORK_ITEMS_STATUS.md`, a 2026-10-14 01:05Z snapshot
(work items filed after it, WI#402 and later, postdate the audit), and
re-checked against the tracker on 2026-10-18. Open at release: WI#397
(`high` — sb&kumo findings); WI#324 (the opencode2/pebble worker-side
bridges and real-worker acceptance evidence); WI#427 (the opencode2 v2.0.32
bump: the tenant rollout of the merged image, including Gentoo/OpenRC, is
pending); WI#422 (security: on rootful hosts the pebble bind source is
re-resolved at every container start, so a tenant symlink swap can redirect
host-root writes); WI#423 (security: mt-admin's remaining root reads/writes
of tenant-writable env paths); WI#389 (security: both onboarding paths add
the tenant user to the container runtime's group by default, which is
root-equivalent on Docker hosts); WI#429 (the sandbox proxy's empty-body
over-refusal) and WI#430 (the WASM tool wrapper's retained-framing gap);
WI#363 (the `ic/deny.toml` open-owner guard — see Accepted advisories);
WI#424 (the deferred worker egress allowlist and opencode2 runtime-smoke
suite), WI#425 (out-of-band worker-image verification), and WI#432
(WASM-host redaction follow-ups, now holding the four WI#408 channel-host
residuals), all three planned for v2.1.7.0; and WI#436 (v2.1.7.0 triage of
the test-tenant failures — its first item, one failing note failing every
hourly knowledge refresh and leaving `knowledge: critical`, is live in this
release). The residual owners named in the sections above (WI#322, WI#333,
WI#369, WI#370, WI#383, WI#386, WI#387, WI#394) are open too, and WI#400
tracks real worker credential isolation beyond the WI#373 hygiene strips.
The five test-build field reports (WI#366, WI#372, WI#385, WI#398, WI#399)
and the WI#359/WI#373 triage items were closed on 2026-10-14; WI#336,
WI#356, WI#376, WI#377, WI#380, WI#393, WI#395, WI#410, WI#411, and WI#417
closed during final review (2026-10-13 through 2026-10-16). Closed since:
WI#357, WI#364, WI#375, WI#390, WI#392, WI#396, and WI#419 (2026-10-17);
WI#360, WI#371, WI#415, the advisory-clearing owners WI#367, WI#368, and
WI#418 (chains still locked — see Accepted advisories), and WI#402, WI#408,
and WI#412 (2026-10-18). WI#416 and WI#438 also closed on 2026-10-18 —
WI#416 with three of its six root-ownership sites addressed (the
`configure_weechat_relay` trailing `chown -R`, the relay start lock, and the
SSH key staging; the rest stand as accepted), and WI#438 with the standalone
TensorZero proxies' developer-LAN endpoint defaults remaining in-tree per
WI#402's scope note.

**Deployment boundaries.** The onboarding web app remains a trusted local
console without user accounts or TLS: retain loopback binding and use an
SSH tunnel for remote access. `ALLOW_PRIVATE_IPS=1` disables public-address
SSRF restrictions and is only for intentionally trusted private-network
deployments. External workers do not automatically inherit core MCP
configuration. OpenRC hosts without usable cgroup v2 delegation use
nice-only build scheduling. Kawarimi remains PostgreSQL-only. DarkIRC
Secure Key Exchange Stages B–D and automated QR/web/trusted-channel
delivery remain disabled.

---

> The line is not drawn
> to keep the workshop closed, but
> to keep what it holds.

> Horns remember what
> the head has chosen to guard —
> nothing passes twice.

角
