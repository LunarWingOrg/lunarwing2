# Release Notes for LunarWing v2.1.6.0

**DRAFT — pre-release; content reflects the v2.1.6.0 tree as of 2026-10-14 and may grow before the tag is cut.**

## Codename: Tsuno (角, つの): The Horns

**Release date:** TBD (drafted 2026-10-14)

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
are credential-redacted before they reach events, prompts, or summaries; and
the `AGENT_AUTH_TOKEN` bridge credential no longer rides into spawned worker
task environments. The opencode worker gains a control channel for
orchestrator follow-ups with a closed-queue contract and a single final
wrap-up turn.

Operations get their share: multi-tenant worker LLM endpoints no longer
fall back to an unreachable fleet default (tenants fail loud at add time
and re-derive on upgrade), the pebble fat image proves its toolchain
anchors in CI before a build can pass, and OpenRC proxy migrations refuse
silently-wrong states instead of refusing healthy ones. The cycle closes
with the usual dependency sweep (2.1.5 → 2.1.6 across all owned packages,
refreshed registries, regenerated vendor tree) and a full crate-security
re-audit — including one **breaking change**: Turso/libSQL remote-replica
sync now lives behind the non-default `libsql-remote-replica` cargo feature
(see Upgrade Notes).

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
  `mcp__lunarwing__*` surface (opencode/opencode2/pebble lanes pending).
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
  interactive job's grant through a same-uid relay socket — and any non-cron
  entry sharing a cron entry's endpoint (compared by normalized host:port)
  is stripped too. Operator guidance: give each cron mission its own
  container.
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

## Compatibility

| Area | v2.1.6.0 |
|---|---|
| Release / owned packages | Release `2.1.6.0`; all eleven LunarWing-owned release packages use `2.1.6` |
| Rust edition / MSRV | Rust 2024 / `1.96.1`, unchanged |
| SQL migrations | V28 remains latest; no new migrations this cycle |
| Multi-tenant port registry | v17, unchanged |
| WASM runtime | Wasmtime/WASI 36.0.17; matching Cranelift family 0.123.17, unchanged |
| Channel registry | darkirc 0.1.12, multica 0.1.15, weechat 0.2.13, xmpp 0.1.18 |
| Tool registry | cavepony-tool 0.4.6, gitlab 0.1.9, gotify 0.1.3, llm-context 0.1.4, multica-bridge 0.1.5, ssh 0.1.4, vision-analyze 0.1.6, voice-transcribe 0.1.4, web-search 0.2.5 |
| External workers | kimi-code `2.1.1`, opencode `v1.18.25`, opencode2 `v2.0.24` (held per WI#357), Pebble `2d0f3786…` — all unchanged from v2.1.5.0 |
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
closed).

The **kimi** lane ships the worker-side bridge: `lunarwing_tool_proxy.ts`
(a per-task relay; replies routed by call_id scoped to the owning hub
socket; task close fails in-flight calls) plus `lunarwing_mcp_server.ts` (a
thin stdio MCP server registered in `$KIMI_CODE_HOME/mcp.json`), giving the
agent a native `mcp__lunarwing__*` tool surface. The opencode, opencode2,
and pebble worker-side bridges remain open, and real-worker acceptance
evidence waits on the rebuilt kimi image (protected-pipeline/deploy gate).
Design doc: `docs/proposals/EXTERNAL-WORKER-TOOL-PROXY.md`.

### Cron Missions on External Workers (WI#325)

A mission supervisor per `(worker, mission_id)` owns the tick chain: the
schedule drives ticks, each tick runs as a real job with inline
`job_events`, and a lapse triggers cancel → terminal-wait → backoff
re-entry. Heartbeat watchdog, persisted pause state and tick counter
(surviving daemon restarts), and a circuit breaker round out the loop; the
breaker count itself is not durable (WI#371).

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

Residuals are tracked as WI#370 (retention/list visibility, event-outcome
FK coverage), WI#371 (read-failure pause safety, durable breaker count),
and WI#375 (remaining harness items). Audio capture is deferred pending the
WI owner's scope decision. Design doc:
`docs/proposals/EXTERNAL-WORKER-CRON-MODE.md`.

### Harness Event Redaction Hardening (WI#326)

The harness-worker secret redactor scrubs secrets from job events in their
raw, base64/url-encoded, and line-wrapped (folded) forms, including
JSON-escaped line breaks and any CLI wrap width. This cycle fixes a folded
64-char coverage gap for 58–62-byte encodings, an availability regression
where a mounted secret longer than the remaining event text paid for a
searcher setup per event leaf (~62 s CPU measured on a 2 MB event; now
skipped), a cache/clear race that could leave an unremovable cached
redactor for an ended job, and a folded-range sweep that no longer drops
overlapping matches. Known residuals — the direct-pass/fast-path overlap
gap, the 4096-char arming cap's partial-print exposure, and the
aho-corasick per-job automaton direction — are tracked in WI#383.

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
`worker-opencode-ts` CI lane runs the five bun self-test suites plus
`tsc --noEmit` on every MR/push, and the `CI_BUN_IMAGE` digest is
cross-checked against the Dockerfile's pinned `oven/bun` FROM so retagging
cannot silently dissolve the tie. Known residuals: WI#386 (clean-turn
tool-error banking; retry-diagnostics-only main turn), WI#387
(postgres-only dead-code warnings), and the pre-existing WouldBlock
`delivery_failed` false-negative class.

### opencode2 SDK Surface Pinning (WI#329)

The opencode2 lane's bridge↔SDK contract is now pinned by a single
committed list (`sdk_surface.ts`) driving two gates: the host self-test
extracts the executor's real `sdk.*` call sites from source and asserts set
equality with the pinned method list (a swallowed call now fails the
suite), and the image build asserts every pinned method and event literal
still exists in the shipped `@opencode` client source — an upstream rename
fails the image build instead of hanging tasks at runtime.

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
risk-acceptance record, not a clean bill: six vulnerability advisories, one
unsound notice, and two unmaintained notices ship accepted via
`ic/deny.toml`, each with recorded exposure and a clearing owner (WI#331
closed its own decisions; rsa rides WI#368; `failure`/`rental` ride
WI#367). After the WI#331 gate, exactly one vulnerability (rsa via russh)
compiles into default builds; the webpki/h2 chain is opt-in only. No
unaccounted advisory exists in any committed lockfile.

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
`params_summary` never carry them.

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

### OpenRC Proxy Transition Fixes (WI#376)

The OpenRC dispatch preflight no longer turns a detection failure into a
false "migration required" refusal — only a positively detected systemd
host skips the legacy-unit guard, fail-closed by construction (this was the
red `shell-harnesses` CI class). The tenant-restore probe classifies the
rc+printed-state pair so a crash-looping `Restart=always` daemon read as
`activating` can never reach `pg_restore --clean`, and a symlinked or
non-regular init entry refuses with a named reason instead of counting as
"already stopped" during remove/upgrade.

### opencode v1 Crash-Line Enrichment (WI#374)

When an opencode v1 serve task fails with the bare `Unexpected server
error` wrapper, the bridge now attaches the matching crash line from
serve's structured log: bounded to a ≤8 KiB window starting at the
task-start offset, `level=ERROR` lines only, correlated by the exact
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
back — only pebble `cli`, which exec-replaces PID 1, is total. Real
isolation (fd or root-only file handoff, separate task uid) is tracked as
WI#400. Worker images must be rebuilt for the strips to reach live tenants.

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
syn 3.0.7, among others), and `ic/vendor/` was regenerated (1148 crate
directories) with the rig-core patch restored and marker-verified. The
`pebble4lunarwing/upstream/Cargo.lock` build-input pin is deliberately
untouched. External worker pins are unchanged from v2.1.5.0 (opencode2's
v2.0.26 bump remains gated per WI#357).

## Upgrade Notes

1. **Back up state.** Preserve PostgreSQL, tenant environment files,
   `/etc/lunarwing/ports.json`, workspace state, rendered units, worker
   data, voice models, and channel state. The port registry stays at v17
   and SQL stays at V28 — no migration runs for this release.
2. **Turso sync users: rebuild with the new feature (breaking).** Installs
   configured for Turso sync (`LIBSQL_URL` + `LIBSQL_AUTH_TOKEN` set) **will
   not start** on default v2.1.6.0 builds: the daemon fails closed at
   startup with an error naming the `libsql-remote-replica` cargo feature,
   and `lunarwing doctor` reports the same. To keep Turso sync, rebuild
   with `--features libsql-remote-replica` (and accept the pinned
   rustls-webpki 0.102 / h2 0.3 exposure recorded in
   `docs/ops/v2.1.6.0_CARGO_DENY_AUDIT.md`). Do **not** simply unset
   `LIBSQL_URL`: the existing replica file then becomes a diverging
   standalone database whose writes never reach the Turso primary — the
   setup wizard now warns about this explicitly. Every shipped build path
   (mt-admin `05-tenant-build.sh`, `scripts/build-lunarwing.sh`,
   `ic/Dockerfile`, `Dockerfile.worker`) uses default features, so shipped
   binaries fail loudly rather than silently diverging; mt-admin currently
   exposes no opt-in feature knob.
3. **Rebuild and promote the worker images.** The WI#324 kimi tool-proxy
   bridge, the WI#325 cron bootstrap, the WI#328 control channel, the
   WI#330 placeholder guards, and the WI#373 credential strips all ship in
   the worker images — promote each rebuilt image through the normal
   candidate → scan → promotion path before tenants use it.
   `upgrade-tenant` backfills enabled workers' base URLs before rebuilding,
   so managed tenants do not need per-worker `configure-*` calls. opencode2
   stays at v2.0.24 (WI#357 hold); kimi-code 2.1.1, opencode v1.18.25, and
   Pebble `2d0f3786…` need no version change.
4. **Deploy rebuilt components together.** Rebuild the daemon, XMPP bridge,
   and installed WASM channels and tools; rebuild/reinstall all four
   channel and nine tool registry updates through the normal deployment
   path.
5. **Review cron worker authority.** Cron tick jobs draw from the new
   separate `cron.tool_allowlist` (default empty — every proxied tick call
   denied), and a cron-mode worker's worker-level `tool_allowlist` is now
   stripped at load with an `error!`. Ordinary dispatches to a cron-mode
   worker get no proxied tools. Never point a `tool_allowlist`-bearing
   entry at the same endpoint as a `mode = "cron"` entry — the shared
   endpoint is stripped (normalized host:port comparison, fails closed) —
   and give each cron mission its own container.
6. **Expect honest gate cleanup.** Gates parked on dead or departed threads
   are now swept (warn + `thread_terminated` broadcast) instead of
   lingering; approvals landing after a thread ends fail closed with a
   "thread ended" reply. Authentication-fallback gates are unaffected.
7. **Expect mission notices after the first boot.** Threads interrupted by
   a pre-upgrade shutdown may earn one boot-time retry notice as the new
   per-thread stamps and owed-outcome notices resolve; a `2.1.5.0 →
   2.1.6.0` upgrade emits no spurious "re-armed" notices for healthy retry
   markers.

### Security, Deployment, and Release Limitations

**Worker proxy scope.** Only the kimi lane ships the worker-side tool-proxy
bridge in this release; opencode/opencode2/pebble workers have no
`mcp__lunarwing__*` surface yet. Real-worker acceptance evidence for the
kimi lane rides the protected image pipeline. The tool grant is ambient for
every process in the task (untrusted repo scripts can call the relay), and
workspace `.mcp.json` files can shadow the `lunarwing` entry — the hub
stays authoritative on the capability set. `memory_read` grants disclose
workspace content to egress-enabled workers; see
`docs/proposals/EXTERNAL-WORKER-TOOL-PROXY.md` and
`docs/ops/WORKER-CONTAINERS.md`.

**Worker credential hygiene.** The WI#373 strips are env hygiene, not
isolation (see the section above); WI#400 tracks the real boundary.

**Known-broken lane.** The opencode **v1** worker lane (v1.18.25) still
crashes inside `opencode serve` before any model call (WI#333); the new
log enrichment surfaces its crash lines in job records, but the lane
decision (bump or retire, planned for v2.1.7.0) is unchanged.

**Accepted advisories.** Six vulnerability advisories, one unsound notice,
and two unmaintained notices ship accepted (see
`docs/ops/v2.1.6.0_CARGO_DENY_AUDIT.md`); only rsa (via russh) compiles
into default builds, and its clearing is tracked as WI#368.

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
