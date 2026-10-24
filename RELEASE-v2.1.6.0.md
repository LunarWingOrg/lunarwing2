# Release Notes for LunarWing v2.1.6.0

## Codename: Tsuno (角, つの): The Horns

**Release date:** 2026-10-24

> Horns emerge at dawn,
> each new growth reaches outward,
> past the old outline. 角

After Bungei Fukkō reopened the workshop, **Tsuno grows beyond it**. The horns make growth visible: new reach emerging from a strong foundation, branching toward places the project could not previously reach. The work of the earlier chapters becomes the ground from which LunarWing can extend into new domains, connect more of its capabilities, and take on work that once lay beyond its reach.

## Overview

LunarWing v2.1.6.0 expands the platform's reach through external workers. The headline addition is the external-worker tool proxy (WI#324): workers can call a curated set of host capabilities — workspace memory, coordination knowledge, and GitLab tools — through the hub over the worker socket, with the hub authoritative at every step. Cron missions for external workers (WI#325) extend that reach into scheduled work, with per-mission tool grants, confined project directories, and explicit busy/skip accounting.

The expansion is deliberate. Cron workers have their own authority domain; mission webhooks require usable secrets; HTTP tool surfaces are credential-redacted before reaching events, prompts, or summaries; bridge credentials are kept out of spawned task environments; and tenant-controlled proxy URLs are no longer shell-evaluated as root by OpenRC proxy units. The OpenRC migration is a required step for multi-tenant hosts. OpenCode gains an orchestrator control channel for follow-up prompts and a single final wrap-up turn.

Operations and worker reliability grow alongside the new capabilities. Multi-tenant provisioning requires a real LLM endpoint instead of an unreachable fleet default, Pebble's image validates its toolchain anchors before promotion, and OpenCode 2 carries its worker-exposure controls. OpenRC migration checks distinguish unsafe units from healthy ones. All three external-worker applications are updated to current upstream versions: Kimi Code 2.3.1, OpenCode 1.19.4, and OpenCode 2.1.2. The Pebble upstream source revision is unchanged, while its reviewed dependency lock receives the security updates.

The release also completes a broad dependency sweep and security re-audit, including rustls 0.23.46, Wasmtime/WASI 36.0.19, the webpki root-store fixes for CVE-2026-111943, the October 23 tokio/ctutils/clipboard-win updates, and crypto-bigint 0.7.6. A breaking change places Turso/libSQL remote-replica sync behind the non-default `libsql-remote-replica` Cargo feature (see Upgrade Notes).

See the [v2.1.5.0 release notes](docs/releases/RELEASE-v2.1.5.0.md) for the previous compatibility baseline.

## Highlights

- **External workers gain a bridge to host capabilities (WI#324).** A per-task unix-socket tool proxy in the worker bridge relays
  `tool_list`/`tool_call` frames to the daemon, which enforces the curated
  capability set (`memory_read`/`memory_write`,
  `knowledge_search`/`knowledge_submit_note`/`knowledge_get_topic`,
  `gitlab_tool`) at load, dispatch, and catalog construction. The prompt-write
  boundary denies proxied writes to identity, config, memory-bootstrap,
  daily-log, and `engine/**` files. The kimi worker lane ships the first
  worker-side bridge: an in-image MCP shim gives the agent a native
  `mcp__lunarwing__*` surface. The opencode and opencode2 lanes ship
  serve-process plugins exposing the same phase-1 set as direct
  `lunarwing_*` tools, and the pebble lane ships the same MCP shim shape
  inside its Rust bridge binary.
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
- **OpenCode 2 keeps exposure under LunarWing's control (WI#357).** The worker image includes an argv-scanning wrapper, root-owned read-only scripts and configuration, the OpenTunnel compile-out, a byte-identity wrapper anchor, and a BuildKit-faithful Dockerfile guard. The host-side `mt-admin worker-listeners` audit inspects TCP and UDP listeners across worker containers. OpenCode 2 is pinned to upstream `v2.1.2`.
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
- **Kimi Code advances with its upstream (WI#411, WI#412, WI#454).** The worker is updated to `@moonshot-ai/kimi-code` 2.3.1. Task environments scrub `KIMI_CODE_TRUST_WORKSPACE`, the FrontierCODEK lane pins `max_output_size = 131072`, and task execution is delegated through the worker's task-runner process. The image contract checks run against the actual CLI during image builds.


## Compatibility

| Area | v2.1.6.0 |
|---|---|
| Release / owned packages | Release `2.1.6.0`; all eleven LunarWing-owned release packages use `2.1.6` |
| Rust edition / MSRV | Rust 2024 / `1.96.1`, unchanged |
| SQL migrations | V28 remains latest; no new migrations this cycle |
| Multi-tenant port registry | v17, unchanged |
| WASM runtime | Wasmtime/WASI 36.0.19; matching Cranelift family 0.123.19 — security upgrades from 36.0.17 (RUSTSEC-2026-0352/0363, then RUSTSEC-2026-0379/0380; see Crate Security Re-Audit) |
| Bundled skills | Seven product skills, unchanged; no skill migration |
| Channel registry | darkirc 0.1.12, multica 0.1.15, weechat 0.2.13, xmpp 0.1.18 |
| Tool registry | cavepony-tool 0.4.6, gitlab 0.1.9, gotify 0.1.3, llm-context 0.1.4, multica-bridge 0.1.5, ssh 0.1.4, vision-analyze 0.1.6, voice-transcribe 0.1.4, web-search 0.2.5 |
| External workers | kimi-code `2.3.1`, opencode `v1.19.4` (models.dev catalog pinned in-image), opencode2 `v2.1.2` (OpenTunnel compiled out; worker-exposure controls included), Pebble `2d0f3786…` (upstream ref unchanged; reviewed lock includes the current rustls and webpki security updates) |
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
agent a native `mcp__lunarwing__*` tool surface. The **opencode** and
**opencode2** lanes ship the same bridge-side relay plus serve-process
plugins (`lunarwing_tool_plugin`) exposing the phase-1 set as direct
`lunarwing_*` tools; each plugin resolves the calling session's task id
from its `lunarwing-opencode-<task_id>` or `lunarwing-opencode2-<task_id>`
title, and both lanes run the relay and plugin suites in their MR CI lanes
(`worker-opencode-ts`, `worker-opencode2-ts`) and image-build gates. The
opencode2 lane additionally denies upstream's unchecked built-in
`session_move`/`session_rename` tools so they cannot rewrite the
title-based task binding or the external-directory confinement. The pebble
lane ships the same relay plus an in-image stdio MCP shim inside its Rust
bridge binary (`pebble4lunarwing mcp-shim`). The default
`tool_allowlist` is empty, so a managed tenant has no proxied tools until an
operator grants them (`lunarwing-mt-admin.sh configure-worker-tools`, then
`restart-tenant`). The proxy, worker bridges, and plugins are distributed in
the worker images. Design doc:
`docs/proposals/EXTERNAL-WORKER-TOOL-PROXY.md`.

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

**WI#325 is closed**; audio capture was scoped out at close-out. Cron tick
retention and job visibility (WI#370), the shared-container authority checks
(WI#396), and durable pause/breaker state (WI#371) are documented below.
Design doc: `docs/proposals/EXTERNAL-WORKER-CRON-MODE.md`.

### Cron Tick Retention and Job Visibility (WI#370)

Cron missions are bounded tenants of the jobs store now. At the 60-second
schedule floor a mission added up to 1,440 `agent_jobs` rows per day before
reentries, and `list_jobs` sorts newest-first with a 100-row window — ticks
hid ordinary jobs within hours. Two cooperating bounds landed:

- **Per-mission retention** (`cron.retain_last_ticks`, default 50, floor 1,
  cap 10,000): after every tick — and once at supervisor start, so an idle or
  paused mission still gets swept on boot — the supervisor deletes that
  mission's tick rows beyond the newest `retain_last_ticks`, always keeping
  the persisted last-job anchor (which owner pause/resume audit events key
  to), deleting `job_events` before their row (`job_events.job_id` has no
  `ON DELETE CASCADE`), and re-checking the anchor afterwards so a deleted
  or foreign anchor can never receive orphan or cross-owner events. The
  window counts JOB ROWS, not distinct ticks — a tick's re-dispatched links
  each spend one row. Tick rows are identified exactly — the
  supervisor-authored prompt-title prefix for that mission, scoped to the
  worker's `job_mode` and the mission owner — so ordinary jobs, other
  missions' ticks, and other owners' rows are never matched, and
  `create_job` refuses an ordinary task whose text starts with that
  reserved opening, so a caller cannot forge the marker to hide work from
  the default listing.
- **`list_jobs` cron filter**: tick rows and live tick contexts are tagged
  `"cron_tick": true` and excluded from the tool's window unless
  `include_cron: true` is passed.

**Upgrade note — irreversible first-boot deletion.** With no config change,
the FIRST supervisor start after upgrading to this version runs the startup
sweep on every configured mission and deletes that mission's existing tick
rows beyond the newest 50 (the default window), together with their
`job_events`. That deletion cannot be undone: snapshot the database first
if the pre-upgrade tick history matters, or set a higher
`retain_last_ticks` in each mission's cron block before the first boot.
Swept rows also drop their `llm_calls`, so usage aggregates built from them
shrink accordingly.

Persisted-event outcomes (`cron_reentry`, `cron_tick_end`,
`abandoned`/`skipped_busy`/`paused`, and breaker-origin `cron_paused`) are
asserted against the store. The `postgres_cron_retention` integration target
pins the FK-safe delete order and verifies that `job_events` refuses an
event for a missing job row.
Retention trades audit depth for boundedness: ticks beyond the window lose
their `job_events` audit trail with the row; raise the knob when a longer
audit window matters more than the space.

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
overlapping matches. WI#326 was closed on 2026-10-14.

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
the tie.

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
risk-acceptance record, not a clean bill: **ten vulnerability advisories,
two unsound notices (`failure`, `atty`), and two unmaintained notices**
(fourteen IDs) ship accepted via `ic/deny.toml`, each with recorded exposure
and a named clearing owner (rsa: WI#368; `failure`/`rental`/`atty`: WI#367;
the nine libsql 0.6.0 legacy-chain acceptances — five rustls-webpki 0.102,
the webpki-roots 0.26.11 shim, the h2 0.3.27 pin, and the rustls 0.22.4
pair — WI#418, whose standing
decision record `docs/ops/v2.1.6.0_WI418_LIBSQL_LEGACY_CHAIN.md`
(2026-10-16) adopts the WI#331 feature gate as the long-term mitigation
after re-confirming upstream — libsql 0.10.0 went stable 2026-10-13 and
still pins hyper-rustls ^0.25 — so a bump still clears nothing). All three
owners were closed on 2026-10-18 with every chain still in `ic/Cargo.lock`,
and the two 2026-10-20 emSign acceptances below re-home under the same WI#418
decision record, so none of the fourteen has an open clearing owner at
release (see Accepted
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

Four security upgrades landed late in the cycle on top of the semver sweep:

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
- **Wasmtime/WASI 36.0.18 → 36.0.19** (Cranelift 0.123.18 → 0.123.19),
  fixing RUSTSEC-2026-0379/0380 (published 2026-10-20; pipeline 6121)
  — two critical-class Cranelift miscompilation bugs (an aarch64 `br_table`
  jumping to an incorrect target, and bit-packing silent overflow), the
  class that can undermine the Wasm-level safety guarantees when executing
  untrusted WASM tools and channels, so both are fixed by upgrade, not
  accepted. Both manifest minimums (daemon and XMPP bridge) rise to
  36.0.19; 37 package records move in each lockfile (24 Wasmtime-family
  36.0.19 + 13 Cranelift 0.123.19); `ic/vendor/` regenerated with the
  rig-core patch preserved. No new acceptances (the twelve-entry policy
  total at that point was unchanged). Rebuild **and restart** the daemon
  and XMPP bridge; rolling back below 36.0.19 reintroduces both
  miscompilations.
- **webpki root-store trio for CVE-2026-111943** (published 2026-10-20;
  pipeline 6137): RUSTSEC-2026-0381 (`webpki-roots`), 0382
  (`rustls-webpki`), and 0383 (`webpki-root-certs`) — certificate paths
  through the distrusted emSign intermediates (the emSign ECC Root CA - G3
  private keys are now well known) were accepted by the bundled/verifying
  webpki stacks. Fixed by semver-compatible upgrade everywhere a patch
  exists: `webpki-roots` and `webpki-root-certs` 1.0.9 → 1.0.10 and
  `rustls-webpki` 0.103.15 → 0.103.16 across `ic`, the XMPP bridge,
  pebble, and the OCR sidecar, with `ic/vendor/` refreshed and `[bans]`
  floors raised (`rustls-webpki <0.103.16`, `webpki-roots`/
  `webpki-root-certs <1.0.10`, only the accepted 0.26.11 pin permitted
  below the patched webpki-roots line). The pebble image's reviewed lock
  override moves with them (re-pinned, `webpki-roots` added to the delta
  gate's allowlist — deployed pebble images still need rebuild and
  promotion). Two accepted advisories apply to the WI#418 libsql 0.6.0
  legacy chain, which has no patch for its pinned line and stays behind
  the opt-in `libsql-remote-replica` gate: 0381 on the `webpki-roots`
  0.26.11 shim (a documented semver shim whose served roots come from its
  1.x dependency, now the patched 1.0.10 — the shim itself is still
  flagged by version) and 0382 on `rustls-webpki` 0.102.8 (no 0.102-line
  patch exists), moving the policy total twelve → fourteen. Pipeline 6137
  is green with the upgrades in.
- **tokio 1.53.3, ctutils 0.4.5, clipboard-win 5.4.3** (2026-10-23, advisory-db
  snapshots `f7b2f6bc`/`84d943a6`). The advisory-db refresh turned cargo-deny
  and the satellite-advisory-sweep red on every v2.1.6.0 pipeline with three
  entries: RUSTSEC-2026-0385 on `ctutils` 0.4.3 (fixed upstream in 0.4.4),
  the yanked `clipboard-win` 5.4.1, and RUSTSEC-2026-0386 on `tokio
  < 1.53.3` — unsound safe `AsyncFd` constructors, which the widened
  `unsound = "all"` scope (`ic/deny.toml`, WI#360) surfaces as a hard
  error. Each is fixed by semver-compatible upgrade: tokio 1.53.3 across
  `ic`, the XMPP bridge, `unix-socket-client`, `pebble`,
  `pebble4lunarwing`, both sidecars, and both
  `git-lunarwing-unix-socket-client` repos (the repo-root one and the
  `replv2git/` copy); ctutils 0.4.5 in `ic` and the
  XMPP bridge; clipboard-win 5.4.3 in `ic`, the XMPP bridge, and `pebble`.
  The locks were bumped minimally — a plain `cargo update` also re-picked
  unrelated `windows-sys`/`socket2` edges (quinn onto socket2 0.5.10), so
  each lockfile changes only these packages' version and checksum lines,
  and every root was re-verified with `cargo metadata --locked`. No
  first-party or vendored code calls the flagged tokio constructors. The
  manifest-less upstream pin `pebble4lunarwing/upstream/Cargo.lock`
  (`verify_upstream_lock.py`) is unchanged. No new acceptances; the
  fourteen-ID policy total is unchanged, and `ic/vendor/` was refreshed.
- **crypto-bigint 0.7.6** (2026-10-24, advisory-db `21d909e4`;
  RUSTSEC-2026-0387). Timing variability in operations meant to be
  constant-time: subtraction, zero counting, division, Montgomery
  multiplication, `UintRef` equality, and `BoxedMontyForm::invert`. Fixed
  in 0.7.6. Only two lockfiles carry the crate, through the SSH key and
  signature path (russh, ssh-encoding, rsa, elliptic-curve, primefield,
  rfc6979, crypto-primes): `ic/Cargo.lock` and the XMPP bridge's. The lock
  edit is minimal — 0.7.5 `1a52aa3f` → 0.7.6 `6bb08299`, both checksums
  checked against the crates.io index — because 0.7.6 has the same normal
  dependency list and its only tightened requirement, `ctutils ^0.4.5`, is
  already met by the 2026-10-23 upgrade; no other entry moves and no crate
  is added. `ic/vendor/` was refreshed (only `ic/vendor/crypto-bigint`
  changes, rig-core patch restored, vendor set matches the locks) and the
  gate reports `advisories ok, bans ok, licenses ok, sources ok` on
  `21d909e4`. No new acceptances; the fourteen-ID total is unchanged.
  Deploy note: this is a binary-content fix, so the daemon and XMPP bridge
  must be rebuilt **and restarted** with the rustls/Wasmtime/webpki set.

Verification for all six is recorded in `docs/ops/v2.1.6.0_CARGO_DENY_AUDIT.md`
(October 14 update, pipeline 5710 addendum, the October 16 zip-yank
follow-up described under Dependency Sweep below, the October 17
`unsound = "all"` widening follow-up described above, the two
October 20 addenda — pipeline 6121 for Wasmtime 36.0.19, pipeline 6137
for the emSign trio — the 2026-10-23 tokio/ctutils/clipboard-win
refresh, and the 2026-10-24 crypto-bigint refresh).

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
arming retries (WI#338; the write-path reservation that backs it landed as
WI#322, below).

### `engine/projects` Reserved From Memory Writes (WI#322)

`memory_write` and the web memory write route now refuse the whole
`engine/projects/` tree (`is_engine_projects_path` in
`ic/src/tools/builtin/memory.rs`), so a mission record can no longer be
forged or edited through a memory tool: engine project and mission records
(`engine/projects/*/missions/**/mission.json`) are Engine-owned state, and
the refusal is reported as such rather than as a generic path error. This
closes the write-path half of WI#338's parity story; WI#322 closed on
2026-10-21.

### Leak Detector: GitLab PAT and Complete Bearer Redaction (WI#353)

The safety leak detector (`ic/crates/lunarwing_safety/src/leak_detector.rs`)
gains a `gitlab_pat` block pattern for the `glpat-` prefix, which had no
detector: an uncut token body was fully visible because the block truncation
boundaries never matched it. Redaction of `Bearer` tokens now covers the
full bearer-token grammar — JWT dot-separated segments (`.`) and
tilde-separated tokens (`~`) included — so the head of a JWT and the tail
of a tilde token are redacted with the rest, not truncated mid-token; the
match is case-insensitive against `bearer` to catch worker logs that spell
`token_type: bearer` / a lowercase `bearer <token>`. Placeholder bodies
(GitLab's own docs spell the shape as `glpat-xxxxxx…`) are skipped via
`is_plausible_token_body`, so fetched documentation is not blocked
wholesale. WI#353 was closed on 2026-10-22; the run-185/run-187 review
refinements (case-insensitive bearer, tilde tokens, placeholder skip,
straddle-sweep fixtures) all landed with it.

### Config-Hash Sidecars Moved to Root-Owned Host State (WI#446, WI#450, WI#451)

The per-container config-hash sidecar that lets `start-tenant` recreate a
container only when its config actually changed moved out of the
tenant-controlled `~/.config` tree into root-owned host state
(`/var/lib/lunarwing/container-hashes/<tenant>/`) — root's historical
mkdir/printf under `~/.config` followed pre-placed symlinks, and the
root-owned sidecars the old writer left on upgraded tenants could never be
updated by a tenant-side writer (every start then force-recreated the
container). That move is WI#446 (closed 2026-10-21).

**WI#450** (closed 2026-10-21, run-181 MC-1) closes the confidentiality gap
the move opened: each hash covers a secret-bearing env file or Quadlet, and
the tenant home that used to hold these sidecars was private — root's
ambient umask would leave the base and per-tenant directories `0755` and
the file `0644`, readable by every local user. The directories are now
forced `0700` and the file is written under `umask 077` (`0600`); the base
`mkdir` stays outside `umask 077` so a fresh `/var/lib/lunarwing` does not
block tenants from reaching the 0755 `voice-models` tree.

**WI#451** (closed 2026-10-22, run-181 MC-2) pins the symlink-redirect
refusals on the voice renderer's Quadlet and fallback paths — half of the
run-181 rootful-Quadlet finding — so the voice renderer's `~/.config`
writes can no longer follow a tenant-planted symlink onto another tenant's
state.

### `prepare_worker_workspace` Creates as the Tenant, Chowns Without Following (WI#453, WI#461)

The workspace sits inside the tenant-controlled `~/lunarwing` tree, so the
historical root `mkdir -p` + unbounded `chown -R` followed a tenant-planted
symlink at any owned component into arbitrary host territory — the same
root-follows-tenant-path class WI#446 eliminated everywhere else.
**WI#453** (closed 2026-10-22): `prepare_worker_workspace` validates the
workspace path first (`darkirc_path_components_safe` refuses non-absolute
paths and any `.`, `..` or symlinked component), then creates the
workspace **as the tenant** (`sudo -u <tenant> mkdir -p`, or a plain
`mkdir -p` when mt-admin already runs as the tenant) and refuses a leaf
that is not a plain directory owned like the tenant root — so root never
opens a path the tenant chose; the chown step uses
`chown --no-dereference` on the workspace root and every non-hard-link
descendant, so a symlink in the tree can no longer redirect root's chown
onto a victim path. The `chmod 2770` site that WI#416 left unconverted is
now the tenant-side `tenant_chmod` in this path. **WI#461** (closed
2026-10-24, hunter 2026-10-22 LOW): both chown steps in the walk refuse
cleanly under `set -euo pipefail` — a `chown` failure inside a suppressed
errexit context (`cmd || status=$?`) used to be silently swallowed and the
bind mount reached the container with the wrong owner; both now return rc-1
with a named message and restore `PWD` on every exit path.

### `validate_llm_url` Escapes and Length-Caps the Tenant URL (WI#384)

`validate_llm_url`'s `die` messages (`must be an http(s) URL`, `has no
host`, `must not embed userinfo`, etc.) used to print the tenant-supplied
URL verbatim, leaking it into the job trace. The scheme and no-host
error lines now route through `sanitize_display_value`
(`ic/scripts/mt-admin-lib/06-env-generation.sh`), which percent-encodes
every non-printable character and `%` and caps the value at 160 bytes
(appending `...` only when a byte was actually dropped) — the URL is still
printed there, but in a short, terminal-safe, injection-free form; every
other die site prints no URL at all. Because the scheme check runs first,
a credential URL with a mistyped scheme still shows its userinfo through
those two escaped lines. WI#384 was closed on 2026-10-23.

### Boot-Time Mission Writers Reload Under the Lock (WI#317)

Boot-time and reconcile-time mission writers no longer save stale list
snapshots: `record_restart_interrupted_missions` (the boot sweep that
records interrupted missions), `ensure_mission_by_metadata`'s reconcile
branch, and `deactivate_skill_learning_missions` each treat their listing
as a candidate set only — per mission they take the mission lock, reload
the record from the store, re-check their conditions on the fresh record,
and save under the guard, the same serialization `fire_mission_inner`, the
outcome watcher, and `record_unsettled_shutdown_fire` already use. A
mission deleted between the candidate list and the lock is skipped instead
of resurrected by a stale save; a concurrent fire's writes (thread history,
`threads_today`, `last_fired_at`, trigger dedup key) can no longer be
overwritten by a boot-time recorder working from an older copy. "Fire lost;
not auto-refired" semantics are unchanged. The race is pinned by a
regression test that parks the listing between snapshot and save, lands a
fire-shaped reservation save (thread history, `threads_today`,
`last_fired_at`, trigger dedup key) in the window, then releases the
recorder and asserts all four fire writes survive with the INTERRUPTED
entry recorded exactly once. Engine-internal change only (open for
maintainer acceptance, WI#317).

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
WI#359 and its WI#392 follow-up closed after the approval-parameter and
HTTP `ToolCall` hook payloads were brought under the same redaction rules.

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

The canonical interpreter resolver now covers the WeeChat and DarkIRC
OpenRC render sites that previously expanded the caller's `$(command -v
python3)`. They now use the shared `_openrc_proxy_python3` resolver
(`type -P` under the fixed `/usr/local/bin:/usr/bin:/bin`, with the
`LUNARWING_MT_PROXY_PYTHON3` override). `_render_weechat_openrc_unit`
resolves once up front and fails the render closed when nothing resolves;
tenant renders resolve lazily, so Python-free tenants with neither service
render unchanged. WI#415 closed on 2026-10-18.

### OpenCode v1 Crash-Line Enrichment (WI#374)

When an OpenCode v1 serve task returns the generic `Unexpected server error`
wrapper, the bridge attaches the matching error line from serve's structured
log. The lookup is bounded to an 8 KiB window from task start, accepts only
`level=ERROR` entries correlated by the exact `err_<8 hex>` request reference,
and scrubs known secret values before the log tail enters the job record. The
log is opened with `O_RDONLY|O_NOFOLLOW|O_NONBLOCK`, so a FIFO, symlink, or
renamed-over directory cannot wedge or redirect the read. The OpenCode v1
worker is updated to upstream `v1.19.4`.

### Worker Credential Environment Hygiene (WI#373, WI#400)

The `AGENT_AUTH_TOKEN` bridge credential is removed from spawned task
processes across the worker lanes: Kimi strips it after environment merges,
OpenCode and OpenCode 2 launch `opencode serve` with the token removed, and
Pebble rebuilds child environments from a clean slate. One-shot CLI/ACP
execution paths strip it as well, and task-supplied entries that reuse the
reserved key are dropped with a warning.

Rootless Kimi, OpenCode v1, and Pebble images use a root-only token-file
handoff and a distinct task UID so task code cannot read the bridge credential
through process environments or the token file. Rebuild and promote worker
images, then recreate tenant containers, for image-level changes to take effect.

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
agent replaces per day, then operator-only).

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

LunarWing owns the worker's public exposure paths. The OpenCode 2 image
includes an argv-scanning wrapper that refuses self-exposure forms such as
`service`, `pair`, `--service`, and `--hostname`; scripts and configuration
are root-owned and read-only, with runtime configuration re-derived from
the read-only copy at boot. The OpenTunnel client is compiled out and checked
by a marker scan. The installed wrapper is anchored to the checkout by
`LUNARWING_WRAPPER_SHA256`, and the Dockerfile guard parses BuildKit
continuations, parser directives, heredocs, and BOM/NUL bytes. On the host,
`mt-admin worker-listeners <tenant>` audits worker-container TCP and UDP
listeners from `/proc`, including all-interface binds and inspection failures.

The OpenCode 2 worker tracks upstream `v2.1.2`; the exposure controls remain
part of the LunarWing image and deployment contract. See
`docs/ops/ITEM71_OPENCODE2_V2.0.32_PUBLIC_HTTPS_TUNNEL.md` for the exposure
architecture and threat model, and `docs/ops/WORKER-CONTAINERS.md` for the
worker deployment contract.

### Sandbox Proxy Credential-Echo Scrubbing (WI#395)

The sandbox HTTP proxy injects credentials into allowlisted outbound
requests; this cycle closes the echo channels on the way back. Forwarded
response headers are byte-scrubbed for the raw and form-encoded spellings
of every injected credential — redirect `Location`/`Link` echoes included —
with two-round percent-decode detection on proxy headers; response bodies
are scrubbed at the byte level; and `headers_json` is parsed and redacted
so JSON-escaped spellings cannot slip through. Credential-injected
exchanges strip every guest `Accept-Encoding` spelling, send exactly
`identity`, and refuse any response whose content coding or transfer framing the scrub
cannot safely inspect (tightened by
WI#419 below) — an uninspectable body is never forwarded blind. The WASM guest HTTP wrapper
applies one guest-facing redaction over the whole outcome, covering
validator errors that quote the injected URL. WI#395 was closed on
2026-10-16.

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
WI#419 closed on 2026-10-17.

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
against live loopback upstreams. WI#408 was closed on 2026-10-18.

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
address. WI#402 was closed on 2026-10-18. This change applies to the setup
script and XMPP test harness; standalone proxies under
`tensorzero-proxy-configurations/` retain their separately configured
defaults.

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

mt-admin uses root-private staging and tenant-side atomic writers for
tenant environment and configuration changes. Values are read from
root-private snapshots taken as the tenant, and a refused tenant-side commit
fails `add-tenant` or `upgrade-tenant` rather than being swallowed. The same
ownership model covers password and worker-token resolution, Study Notes
mount sources, workspace preparation, relay locking, SSH-key staging, unit
and Quadlet rendering, WeeChat home creation, and add-tenant environment
permissions.

Study Notes sync accepts the operator default or a path whose components and
existing ownership chain are safe, symlink-free, and not group/other-writable.
Untrusted tenants are skipped without aborting the hourly scan, non-operator
checkouts are never pulled, and untrusted homes receive no Study Notes mount.
Container config-hash sidecars live in root-owned host state at
`/var/lib/lunarwing/container-hashes/<tenant>/`, rather than tenant-controlled
home directories. Workspace creation and ownership changes avoid following
tenant-planted symlinks, including during rootful Pebble launch (WIs #390,
#416, #422, #423, #445, #446, #450, #451, #453, and #461).

### Kimi Code 2.3.1 and Environment/Config Pins (WI#411, WI#412)

The Kimi external worker is updated to `@moonshot-ai/kimi-code` **2.3.1**.
The image contract covers stream-JSON print mode, option conflicts, the
WI#206 `PreToolUse` guard table, and the WI#324 MCP handshake against the
real CLI. The executor and entrypoint scrub `KIMI_CODE_TRUST_WORKSPACE` from
task environments alongside the existing guard variables (WI#411), and the
lane configuration pins `max_output_size = 131072` on the FrontierCODEK
model (WI#412). Rebuild and promote the worker image to deploy these changes.

### Kimi Task-Runner Delegation (WI#454)

The Kimi lane delegates task execution to a dedicated task-runner process
running as the task UID. A bridge-side client routes tasks over a Unix socket,
so the runner does not inherit the hub credential. The launcher provisions
the socket in a root-only `0700` directory, starts the runner without the
bridge credential, and routes task execution through the runner rather than
spawning the CLI in the bridge process. `KIMI_TASK_RUNNER=skip` is an explicit operator-controlled option for local
development and direct CLI execution.

The runner acknowledges task acceptance, reports the CLI process group,
streams progress, and returns one result. Runner-socket lines are capped at
the hub's `WS_MAX_PAYLOAD_BYTES` plus 64 KiB, preserving the same truncation
contract as local execution. The bridge holds the workspace lock until the
CLI exits, maps a dead runner to an error result, and exposes runner liveness
through `/ready`. The entrypoint supervises the runner and stops the bridge,
runner, and health server after repeated runner failures so the container
can exit cleanly. Tests run as the runner UID under rootless BuildKit without
requiring `CAP_SYS_PTRACE`; task-side PATH resolution is restricted to
root-owned executables.

### OpenCode Reproducible Build: models.dev Snapshot Pin (WI#467)

The OpenCode worker pins its models.dev catalog to the reviewed snapshot at
`opencode4lunarwing/models-dev/api.json` rather than fetching a mutable catalog
during the image build. The snapshot is strict-JSON validated, its digest is
pinned as `MODELS_DEV_SNAPSHOT_SHA256`, and both the snapshot and build inputs
are included in `worker-inputs.sha256`. The image build verifies the hash and
checks that upstream still consumes the configured snapshot instead of
silently fetching live data. The OpenCode worker is updated to upstream
`v1.19.4`.

### Worker Image Rollout and Traceable mt-admin Builds

Rebuilt worker images now reach running tenants on both systemd and OpenRC.
The worker-container launch contract hash includes the image ID, so the first
`start-tenant` or `restart-tenant` after upgrading mt-admin recreates each
worker container once. Current upstream pins are Kimi Code `2.3.1`, OpenCode
v1 `v1.19.4`, and OpenCode 2 `v2.1.2`; Pebble's upstream source revision is
unchanged, with its reviewed dependency lock refreshed for the security
updates in this release. mt-admin builds stamp
`org.opencontainers.image.revision` with the checkout HEAD, `-dirty`, or
`unknown` outside Git, matching CI-built image traceability.

### Documentation and Release Review

Documentation under `docs/architecture/` and `docs/guides/` was revalidated
against the release tree, including mt-admin quickstart, testing, gateway and
voice, DarkIRC adapter, contributor onboarding, Engine V2, self-heal, and
harness guidance. Stale anchors and the OpenRC proxy migration instructions
were corrected as part of the pass.

### Dependency Sweep and Vendor Refresh

All eleven LunarWing-owned release packages advance from `2.1.5` to
`2.1.6`: the daemon, five workspace crates, the XMPP bridge, and four WASM
channels, with every inter-crate path-dependency requirement. All four
channel registry packages and all nine tool registry packages advanced
together (see Compatibility), and a tracked-manifest/lock audit confirms no
project-owned package or path dependency remains at 2.1.4 or 2.1.5.

External crates took MSRV-respecting semver-compatible updates across all
26 swept lockfile roots (tokio 1.53.3, hyper 1.12.0, h2 0.4.20, jiff
0.2.38, toml 1.1.8, uuid 1.28.0, zeroize 1.9.1, html-to-markdown-rs 3.17.2,
syn 3.0.7, among others — tokio moved to 1.53.3 with the 2026-10-23
advisory-db refresh), and `ic/vendor/` was regenerated — 1150
directories: 1148 vendored crates plus the two pre-vendored libsignal path
dependencies — with the rig-core patch restored and marker-verified. Six
security upgrades landed late-cycle on top of the sweep (rustls 0.23.46;
Wasmtime/WASI 36.0.18 then 36.0.19; the CVE-2026-111943 webpki root-store
trio; the 2026-10-23 tokio/ctutils/clipboard-win set; crypto-bigint 0.7.6
on 2026-10-24 — see Crate Security Re-Audit). The
`pebble4lunarwing/upstream/Cargo.lock` build-input pin was deliberately
untouched by the sweep, then upgraded separately twice — for the rustls
advisories and again for the webpki trio — with its Dockerfile digest
refreshed each time. External worker pins are current upstream versions: Kimi Code 2.3.1,
OpenCode v1.19.4, and OpenCode 2.1.2. Pebble's upstream source ref is
unchanged, with its reviewed dependency lock updated for the current security
advisories. OpenTunnel remains compiled out in the OpenCode 2 worker; see
Worker Exposure Policy. All four worker Dockerfiles and
the CI `CI_DEBIAN_SNAPSHOT` moved from the 20260906T000000Z to the
20261017T082437Z Debian snapshot, which carries the Debian-fixed perl
(WI#361).

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
4. **Rebuild and promote the worker images.** Rebuild all four worker images
   (`build-<worker>-worker` or `build-tenant --with-<worker>`), promote each
   through the normal candidate → scan → promotion path, then run
   `restart-tenant` for each tenant. The release images include the external-
   worker tool-proxy bridges, cron bootstrap, OpenCode control channel,
   worker endpoint placeholder guards, credential handling, and current
   worker-specific configuration. OpenCode v1 includes the pinned models.dev
   snapshot; OpenCode 2 includes its SDK-surface gate and exposure controls;
   Pebble includes the tool-proxy MCP shim and reviewed dependency-lock
   security updates; Kimi includes the 2.3.1 CLI contract, configuration pins,
   and task-runner delegation.

   The current upstream pins are Kimi Code **2.3.1**, OpenCode v1 **1.19.4**,
   and OpenCode 2 **2.1.2**. All four worker images use the
   `20261017T082437Z` Debian snapshot, including the fixed Perl packages
   covered by WI#361. Preserve OpenCode 2 state before its first boot after
   changing image versions because upstream may migrate its database.

   `upgrade-tenant` (and `patch-env <tenant>`) backfills enabled workers'
   base URLs. If a worker image is built directly using `build-<worker>-worker`
   or `build-tenant --with-<worker>`, run `patch-env <tenant>` first or set the
   URL with `configure-kimi`, `configure-opencode`, or `configure-opencode2`
   using `--base-url <url>`. The images refuse to start if a `.invalid`
   placeholder survives into the effective worker configuration.
5. **Deploy rebuilt components together.** Rebuild the daemon, XMPP bridge,
   and installed WASM channels and tools; rebuild/reinstall all four
   channel and nine tool registry updates through the normal deployment
   path. The rustls 0.23.46, Wasmtime/WASI 36.0.19 (36.0.18's successor —
   both Cranelift miscompilation fixes), CVE-2026-111943 webpki,
   2026-10-23 tokio/ctutils/clipboard-win, and 2026-10-24 crypto-bigint
   0.7.6 security fixes ship
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

**Worker proxy scope.** The worker tool proxy exposes only the curated
capability set, and each worker's `tool_allowlist` is empty by default.
Operators must explicitly grant tools with `configure-worker-tools` and
restart the tenant. A grant is available to processes inside that task, so
treat the worker's workspace and executable content as part of the trust
boundary. Workspace-level MCP/plugin configuration can affect whether a
worker-side bridge is invoked. `memory_read` grants expose workspace content
to workers whose network configuration permits egress. The hub remains
authoritative for the capability set. See
`docs/proposals/EXTERNAL-WORKER-TOOL-PROXY.md` and
`docs/ops/WORKER-CONTAINERS.md`.

**Accepted advisories.** The release records ten vulnerability advisories,
two unsound notices, and two unmaintained notices (fourteen IDs) accepted
under `ic/deny.toml`; see `docs/ops/v2.1.6.0_CARGO_DENY_AUDIT.md`. Of the
ten vulnerabilities, only `rsa` via `russh` compiles into default builds.
The rustls-webpki 0.102 / h2 0.3 / webpki-roots 0.26.11 chain and accepted
rustls 0.22.4 advisories are confined to opt-in `libsql-remote-replica`
builds. Their accepted status and exposure are recorded in the WI#418
standing decision record (`docs/ops/v2.1.6.0_WI418_LIBSQL_LEGACY_CHAIN.md`).
The `atty` acceptance (RUSTSEC-2021-0145) is dev-graph only, through the
vendored `libsignal-protocol` `env_logger` 0.6.2 development dependency.
The accepted-advisory guard requires every accepted advisory to name a
clearing-owner work item and checks that referenced paths exist.

**Deployment boundaries carried forward.** The onboarding web app remains a
trusted local console without user accounts or TLS: retain loopback binding
and use an SSH tunnel for remote access. `ALLOW_PRIVATE_IPS=1` disables
public-address SSRF restrictions and is intended only for explicitly trusted
private-network deployments. External workers do not automatically inherit
core MCP configuration. OpenRC hosts without usable cgroup v2 delegation use
nice-only build scheduling. Kawarimi remains PostgreSQL-only. DarkIRC Secure
Key Exchange Stages B–D and automated QR/web/trusted-channel delivery remain
disabled.

---

> Not another wall,  
> new horns reach beyond the old,  
> the world grows wider.

角
