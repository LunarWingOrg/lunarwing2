# Release Notes for LunarWing v2.1.3.0

## Codename: Ō-Yoroi (大鎧, the Great Armor)

**Release date:** 2026-09-29

> Yoroi laced plate over plate
>
> The great armor closes its gaps
>
> What the craftsman polished
>
> 大鎧

## Overview

LunarWing v2.1.3.0 is a hardening, secrets-handling, and operator-ergonomics
release — the fitting of the great armor over the v2.1.2.0 (Yoroi) isolation
work. Where Yoroi built the plates, Ō-Yoroi lacquers the joints: the release
is dominated by defense-in-depth on the multi-tenant operator surface, the
sealing of machine-generated engine output out of the human transcript, and
the closing of the managed credential-rotation gap.

WeeChat relay credentials gained a managed rotation verb
(`rotate-relay-password`) with lifecycle-locking against concurrent starts,
hard-link and symlink refusal, a setuid-free exec helper that reloads the
credential at the launch boundary, and consumer-aware restarts
(closing WI#202's no-rotation gap). New tenant defaults were adjusted

The sandbox itself was stabilized.

The web gateway gained a read-only tenant-facing Missions surface.

The release identifier is `2.1.3.0`; LunarWing-owned Rust crates and channel
packages use `2.1.3`. Rust 2024 and MSRV `1.96.1` are unchanged. There are
no new SQL migrations (`V27__memory_index_outbox.sql` remains latest). The
multi-tenant port registry stays at v17. Yoroi laced the plates; Ō-Yoroi is
what the craftsman built once every joint was known.

See the [v2.1.2.0 release notes](docs/releases/RELEASE-v2.1.2.0.md) for the
previous compatibility baseline.

## Highlights

- **Sealed step records** Machine-generated CodeAct step
  output is inference data, not conversation: a new
  `MessageRole::SealedStepRecord` is host-minted via exhaustive matching,
  `thread.add_message` reroutes it into `internal_messages` so the visible
  transcript can no longer contain it, `classify_wire_role` in the
  orchestrator is a host-owned decision where only the persisted
  `StepRecord` spelling and host-flagged writes classify sealed, and the
  flag is consumed host-side and never persisted or forwarded as model
  content. The llm adapter still maps sealed records to `Role::User` for
  inference, so the model reads its own step output without it ever
  claiming human authorship; the WI#161/goal-8 neutralization stack
  applies downstream. Regressions pin classification, visible-transcript
  exclusion, internal placement, flag stripping, trace `code_error`
  coverage over sealed records, the operator DTO never carrying a sealed
  role, and channel code-execution status hints for both legacy and sealed
  roles
- **Managed WeeChat relay password rotation** New mt-admin
  verb `rotate-relay-password <tenant>` closes the long-standing gap where
  `RELAY_PASSWORD` had no rotation protocol (only `rotate-pg-password`
  existed). The rotation runs under the tenant lifecycle lock with a
  start barrier against concurrent units, refuses missing/symlinked
  credential files and unsafe hard links, stages the new secret through a
   root-private staging path committed by the tenant-side atomic writer
   (no secret in writer argv/environment). The dedicated
   `lunarwing-relay-env-exec` helper guards launches and reloads the runtime
   credential; rotation preserves the literal `${env:RELAY_PASSWORD}` in
   `relay.conf`, and restarts
  only the running consumers with a brief, documented interruption
- **Tenant env/capability writers hardened against temp-path swaps and
  partial writes (WI#257 and the goal-8/12 hardening).** Root no longer
  creates temp files inside tenant env directories: every tenant env,
  capability, and credential write goes through a tenant-side atomic
  writer that rejects non-tenant-owned parents and existing destinations.
  The WI#257 review finding closed the remaining partial-write hole —
  `_tenant_env_replace` and the `jq` capability rewrites previously piped
  fallible producers straight into the atomic commit, so a failed tenant
  read or parse error could replace a valid file with a partial/zero-byte
  result. All fallible producers (`_tenant_env_replace`,
  `patch_ssh_tool_allowlist`, `patch_weechat_caps`,
  `configure_gotify_capabilities`, `_voice_remove_daemon_credentials`,
  `write_kimi_runtime_env`) now generate their complete result in
  root-private staging, the producer exit status is checked, and only then
  is the staged file handed to the tenant-side writer. Failure-path tests
  assert the previous file stays byte-for-byte intact
- **Explicit env defaults for new tenants (WI#239).** add-tenant now renders
  explicit lines for multiple values
- **XMPP defaults and approval-override preservation**
- **Sandbox stability under the hardened daemon.** The Podman version probe
  switched from `podman --version` to the client-only `podman version
  --format …` shape: `--version` joins the rootless namespace constructor,
  and under a hardened user unit Podman 6.1.2 could mistake a live but
  unreadable pause process for a recycled PID and unlink `pause.pid`. The
  sandbox reaper's sweep commands now run through the same manager-forked
  transient-service isolation as sandbox execution (`runtime_cli_output`),
  preserving the sweep deadline and stopping the whole service/tree on
  timeout — so even `podman ps` from the hardened daemon cannot invalidate
  live pause state. Writer quiescence during stop/export and sandbox-image
  builds were hardened in the same pass (goal 12), and the real web
  onboarding lifecycle acceptance — Podman namespace recovery ordering,
  reaper isolation, non-destructive version probes, required-worker
  verification, export/restore/restart rehearsal — completed against a
  live tenant.
- **Tenant-facing Missions observability **
- **External worker refresh.**
- **Actionable voice errors for long replies** The gateway
  preserves sidecar 422/503/504 semantics instead of collapsing them into 502,
  performs a speed-aware synthesis-duration pre-flight (`text_too_long`), and
  gives the sidecar timeout 30 seconds of proxy headroom. mt-admin propagates
  the sidecar's duration and timeout settings to the daemon
- **Recoverable worker timeouts** Stuck detection
  measures idle time rather than elapsed runtime; late genuine completion can
  move a stuck job to Completed. Genuine timed-out output is retained as a
  bounded 16 KiB tail, recoverable through `job_status` after restart. Synthetic
  keep-alives do not masquerade as evidence; real diagnostics and whitespace
  separator deltas remain intact. Kimi gains a CLI-silence watchdog.
- **Complete tool inventories (WI#245, goal 46).** `tool_list` now has entry
  and per-server tool-name pagination with explicit continuation metadata and
  serialized byte budgets. Identifiers remain lossless, MCP status/config
  fields remain available, and already-installed long-named extensions remain
  manageable after upgrade.
- **GitLab issue-write alias (WI#266, goal 49).** GitLab tool 0.1.6 accepts
  `body` as an alias for `description` on `create_issue`/`update_issue` only;
  conflicting values are rejected rather than silently chosen.
- **Idle mission daily-budget rollover** Startup and every
  scheduler tick persist UTC rollover for active missions of every cadence,
  without firing idle missions or overwriting concurrent spawn reservations.
- **WASM runtime security update.** Wasmtime/wasmtime-wasi advance to 36.0.16
  for RUSTSEC-2026-0314/0316, with the vendor snapshot refreshed. A separate
  lockfile-only advisory CI guard catches activation of the currently unused
  PyO3 dependency (WI#255)

## Compatibility

| Area | v2.1.3.0 behavior |
|---|---|
| Release version | `2.1.3.0` |
| Crate/package version | `2.1.3` for all eleven LunarWing-owned release packages (daemon, five workspace crates, XMPP bridge, four WASM channels) |
| Rust edition / MSRV | Rust 2024 / `1.96.1` (no change) |
| New SQL migration | None; `V27__memory_index_outbox.sql` remains latest |
| Multi-tenant port registry | v17 (no change); v15 LunarVoice, v16 health_metrics, v17 opencode2 blocks remain current |
| Channel registry patches | darkirc 0.1.9, multica 0.1.12, weechat 0.2.10, xmpp 0.1.15 (behavior unchanged; rebuild required) |
| Tool registry patches | gitlab 0.1.6 (issue writes accept `body` as a `description` alias, WI#266); other tools unchanged from v2.1.2.0 |
| WASM runtime | Wasmtime / wasmtime-wasi 36.0.16 (36.0.14→36.0.16 for RUSTSEC-2026-0314/0316; vendored set regenerated) |
| External crate updates | aws-lc-rs 1.18.0→1.18.1, aws-lc-sys 0.44.0→0.45.0, rustls-webpki 0.103.14→0.103.15, zerovec-derive 0.11.4→0.11.5, plus Wasmtime 36.0.16; `ic/vendor/` refreshed |
| Worker image tool pins | kimi-code `2.1.1` (2.x line; default endpoint back on the NetBird overlay with the `FrontierCODEK` function); opencode2 `v2.0.18` at `cd9a14a6b688d4021bee381dfd39d2cef9c0f862`; opencode `v1.18.25` at `cb7d8b2f…` (no change); pebble `2d0f3786…` (no change) |
| Engine transcript roles | New `SealedStepRecord`/`StepRecord` wire role for interpreter step records; legacy pre-seal transcripts keep the `User` spelling and remain readable |
| Podman version probe | `podman version --format …` (client-only) replaces `podman --version` in the sandbox detect/probe/reaper paths; same `(major, minor)` output contract |
| New tenant env defaults | WI#239 explicit defaults render at add-tenant (see Highlights); re-renders preserve operator values |
| XMPP defaults (new tenants) | `XMPP_JID=<tenant>@xmpp.localhost`, `XMPP_ALLOW_FROM=user@xmpp.localhost`; bridge JSON allowlist matches; existing JIDs/allowlists and explicit flags keep precedence |
| Auto-approve env key | `AGENT_AUTO_APPROVE_TOOLS` is the only key written for new tenants; legacy `AUTO_APPROVE_TOOLS` is migrated on re-render and dropped from generated files |
| mt-admin verbs | New: `rotate-relay-password <tenant>`; all existing verbs unchanged |
| Onboarding mascot default | Hidden (opt-in via settings); web onboard demo targets v2.1.3 |
| Deny policy | `ic/deny.toml` advisory ignores unchanged in id set (8); dated reasons and blocking dependency chains documented in the policy/audit |
| Voice synthesis | Speed-aware 422 `text_too_long` pre-flight; sidecar 422/503/504 preserved; proxy timeout is `VOICE_REQUEST_TIMEOUT_SECS` + 30s |
| External worker evidence | `partial_output` is a bounded genuine-output tail recoverable after restart; heartbeat-only timeouts provide no fabricated evidence |
| Kimi silence watchdog | `KIMI_SILENCE_TIMEOUT_MS=900000` by default, capped at the task deadline; 0 disables it |
| Agent-facing `tool_list` | Paginated entries and tool-name windows; callers must follow `next_offset` and `tools_next_offset` to enumerate everything |
| New identifier bounds | Extension names ≤200 bytes on install; existing longer names remain addressable. MCP tool names ≤256 characters at discovery |
| Mission daily budget | Active missions roll over at startup/tick as well as fire time; undated legacy counts are preserved on the upgrade day |
| OpenCode 2 onboarding | CLI/web provision selection and model/base-URL overrides supported; Kawarimi import selection remains unsupported |

## Changes

### Sealed Interpreter Step Records

The orchestrator recorded every CodeAct step blob (`[stdout]`, `[name
ERROR]`, `[return]`) as a role-User message in its working transcript, which
synced into `thread.messages` through the `json_to_thread_messages` user
fallback — so engine-authored interpreter output entered records and API
surfaces that document human statements. The seal prevents this authorship
misclassification as defense-in-depth. The original Kageho "stale date system
error" incident was fixed separately by host-clock OS calls (`605971dfd`,
v2.1.2.0), which stopped producing the CodeAct OS-denial blob; the seal is not
a claim that model hallucinations or reinterpretations are eliminated.

Machine-generated step output is inference data, not conversation; it is now
sealed at the boundary instead of trusting the Python VM to behave:

- `MessageRole::SealedStepRecord` + `ThreadMessage::sealed_step_record`,
  host-minted via exhaustive matching, with `wire_name()` giving the stable
  `StepRecord` wire spelling. `thread.add_message` emits `MessageAdded`
  under that spelling and reroutes the record into `internal_messages`, so
  the visible transcript can no longer contain it.
- `classify_wire_role` in `executor/orchestrator.rs` is a host-owned
  decision, exhaustive over `MessageRole`, decoding orchestrator wire
  state. Only two VM-unforgeable shapes classify sealed — the persisted
  `StepRecord` spelling and items the default orchestrator flagged
  `_step_record`; unknown spellings and unflagged writes stay `User`
  (engine warnings, bounded probes, and nudges keep their turn shape).
- `default.py seal_step_record` flags freshly produced `format_output`
  blobs before the next host sync; the flag is consumed host-side and never
  persisted or forwarded as model content.
- Bootstrap, persistence, and bridge role spellings move from `Debug`
  formatting to `wire_name()`; `prompt_context_from_thread_messages`
  excludes sealed records from delegated-worker context.
- The llm adapter maps `SealedStepRecord` to `Role::User`: inference reads
  its own step output, it never claims human authorship, and the existing
  WI#161/goal-8 neutralization stack still applies downstream.

Review remediation (including the `!1191` findings) kept the seal defense-in-depth
without losing the diagnostics: the trace `code_error` analyzer matches
sealed records as well as legacy user blobs so interpreter errors keep
producing the actionable warnings the mission self-improvement trigger
consumes; the orchestrator regression drives `sync_runtime_state` over
flagged working messages and asserts zero leakage into `thread.messages`;
and the router test pins that the operator thread DTO (`get_engine_thread`'s
served set) can never carry a sealed role or sealed diagnostic text.
A response-boundary regression also verifies that a model quoting or
reinterpreting sealed output produces an Assistant-authored visible response,
never a new human-authored message.

A follow-up pass fixed the UX regression the seal introduced and a latent
precedence bug: sealed records previously bypassed `Thread::add_message`'s
`MessageAdded` event and the bridge's `interpret_message_event` only matched
role `User`, so channel UIs would have silently lost the "Code executed",
"Code executed (no output)", and "Code error — retrying…" hints for every
CodeAct step. The event now fires under the `StepRecord` spelling,
`interpret_message_event` accepts both the legacy `User` role (pre-seal
transcripts) and `StepRecord`, and error-shaped previews win over the
`[stdout]` prefix check — previously `[stdout]\nOSError: …` reported "Code
executed". Evidence:
[`docs/ops/history/GOAL34_SEALED_STEP_OUTPUT.md`](../ops/history/GOAL34_SEALED_STEP_OUTPUT.md).

### Managed WeeChat Relay Password Rotation (WI#202, goal 8)

`RELAY_PASSWORD` previously had no rotation protocol — only
`rotate-pg-password` existed, leaving the WeeChat relay credential
effectively permanent for a tenant's lifetime. The new mt-admin verb
`rotate-relay-password <tenant>`:

- runs under the tenant lifecycle lock with a shared exec-boundary start barrier
  (`ic/scripts/lunarwing-relay-env-exec.py` installs as
  `/usr/local/libexec/lunarwing-relay-env-exec`). All three consumers
  (daemon, WeeChat adapter, WeeChat) are guarded on systemd user units and
  OpenRC, including self-heal/supervisor launches. Rotation takes exclusive
  admission before stopping services and cancels pending starts;
- refuses missing, symlinked, or hard-linked credential files and verifies
  the env directory contract before touching anything;
- stages complete credential writes and commits them via the tenant-side
  atomic writer, without passing the password in writer-process argv or
  environment. The dedicated `weechat.env` is mode 0600 and contains only
  `RELAY_PASSWORD`. The runtime launcher deliberately reloads that password
  into the consumer environment under lock, including inside the tmux pane,
  rather than trusting an old init/tmux environment;
- validates and preserves `relay.conf`'s literal `${env:RELAY_PASSWORD}`
  expression; it does not replace it with plaintext;
- rolls both env files back on write failure. A durable recovery marker blocks
  starts after interruption or failed rollback; repair matching credentials
  and rerun rotation to clear it through a successful reconciled retry;
- restarts only the running WeeChat consumers (brief service interruption)
  so the daemon and adapters converge on the new credential.

Existing tenants must re-render their units before rotation; the command
rejects unguarded deployed units. Coverage includes systemd/OpenRC rendering,
real self-heal dispatch, concurrent launcher processes, SIGKILL/rollback
recovery, and root/post-drop boundary tests. An isolated real WeeChat run
verified the rendered tmux launcher, password replacement, preserved config
expression, and restart persistence. Full deployed-tenant init acceptance
was not repeated after the final barrier revision.

### Tenant Env and Capability Secret Writes Hardened (WI#257, goals 8/12)

Two related hardening programs landed on the mt-admin writer surface:

**Root/tenant writer separation.** Root no longer creates temp files inside
tenant env directories (`mktemp` + `mv` is gone from
`_write_weechat_env` and friends). All tenant env, capability, and
credential writes go through `write_tenant_file_atomic`/`_tenant_env_replace`,
a tenant-side atomic writer that requires the parent directory and any
existing destination to be tenant-owned and mode-appropriate — closing the
temp-path swap window where a tenant-controlled name could redirect a root
write. The Postgres, worker-config, vision, and voice credential paths were
migrated in the same pass.

**No fallible pipes into the atomic commit (WI#257).** The review's high
finding: `_tenant_env_replace` and the `tenant_jq` capability rewrites piped
fallible producers straight into `write_tenant_file_atomic`, which commits
whatever bytes reach stdin and cannot observe an upstream failure — a failed
tenant-side read or jq parse error could replace a valid env/capabilities
file with a partial or zero-byte result (reproduced on the previous head: a
mode-000 `lunarwing.env` became a 0-byte 0600 file). All fallible producers
now generate their complete result in root-private staging, the producer's
exit status is checked, and only then is the staged file handed to the
tenant-side atomic writer. `write_tenant_file_atomic` documents the
no-fallible-pipes contract. Failure-path tests assert the previous file
stays byte-for-byte intact (`test-mt-admin-tenant-capability-writers.sh`,
`test-rotate-relay-password.sh`, plus CI-fixture fixes so the whole
shell-harness suite runs green as root in CI images without `sudo`).

### Explicit Env Defaults for New Tenants

`06-env-generation.sh` now renders explicit defaults at add-tenant time
instead of relying on in-code fallbacks

### XMPP Defaults and Approval-Override Preservation (goal 37)

mt-admin's env generation previously wrote only the legacy
`AUTO_APPROVE_TOOLS` key and could clobber an operator's canonical
`AGENT_AUTO_APPROVE_TOOLS=true` on re-render. New tenants now render
`AGENT_AUTO_APPROVE_TOOLS=false` only; re-renders prefer the existing
canonical value, migrate a legacy-only `AUTO_APPROVE_TOOLS` value, and drop
the legacy key from the generated file

### Sandbox and Reaper Stability Under the Hardened Daemon (goal 12)

- **Client-only version probe.** `podman --version` joins Podman's rootless
  namespace constructor; under a hardened user unit (no-new-privileges
  context), Podman 6.1.2 could mistake a live but unreadable pause process
  for a recycled PID and unlink `pause.pid`. The detect, cli_runner, and
  process-tool probes now use `podman version --format "podman version
  {{.Client.Version}}"` — the `version` subcommand bypasses the namespace
  constructor — keeping the same `(major, minor)` parser contract for the
  `--userns` ladder.
- **Reaper isolation.** The sandbox reaper's CLI sweep (`podman ps` /
  `podman rm`) ran as plain daemon children; from the hardened daemon even
  those reads could invalidate live pause state. The new
  `runtime_cli_output` helper in `ic/src/orchestrator/reaper.rs` runs the
  sweep through the shared `WorkloadSpawner` (manager-forked transient
  services on systemd; process-group fallback on OpenRC), preserving the
  5s sweep deadline and stopping the whole service/tree on timeout or
  cancellation. Regressions pin isolation (cgroup containment) and
  descendant reaping on timeout
- **Writer quiescence and sandbox-image builds.** Stop/export writer
  quiescence verification and the per-tenant sandbox-image build path were
  hardened in the same pass, including the userns-repair
  intermediate-step exception, with harness coverage in
  `test-mt-admin-podman-repair-quiescence.sh` and the extended
  `test-mt-admin-worker-lifecycle.sh` / `test-mt-admin-sandbox-image.sh`.

### Web Onboarding Lifecycle Acceptance

The web onboarding path completed real-tenant acceptance: Podman namespace
recovery ordering repairs, sandbox reaper calls isolated from the hardened
daemon, non-destructive client version probes, required PostgreSQL and
selected workers in onboarding verification, CLI verification failures
rejected (not ignored), and web-quoted source credentials accepted during
encrypted export. Lifecycle, reaper, verifier, and migration regressions
were added, and a successful real-tenant export, restore verification, and
restart rehearsal was recorded.

### Tenant-Facing Missions API and UI (WI#217)

Work item #217 asked for a tenant-facing mission-management surface. The
gateway now exposes the observability half: `GET /api/missions`,
`GET /api/missions/summary`, and `GET /api/missions/{id}` in
`ic/src/channels/web/handlers/missions.rs`, backed by the existing bridge
`list_engine_missions`/`get_engine_mission` (ownership checks already
enforced there). The web UI gains a Missions tab: a read-only mission list
(name/cadence/threads/today's budget/last fired/status), summary cards
(total/active/paused/budget-spent/never-fired), and a detail view with
goal, current focus, success criteria, cadence JSON, approach history, and
a spawned-thread table — EN and zh-CN strings. Missions remain managed via
the model-facing tools. Gateway tests cover bearer-auth enforcement and
graceful no-engine degradation.

### Mission Daily-Budget Maintenance

Fire-time rollover already reset dated counters before checking
`max_threads_per_day`; the reported old `threads_today_date` was primarily
persisted-state/observability drift for missions that did not reach that path.
Startup and every scheduler tick now persist rollover for active manual,
event, system-event, webhook, and cron missions, including idle/not-yet-due
missions. Maintenance does not spawn threads or advance schedules, and reloads
under the existing fire/update lock so it cannot overwrite a new reservation.

Same-day counters are unchanged; a missing legacy date is stamped with today's
UTC date without granting extra quota. Paused and terminal missions are not
swept. Failed maintenance saves are isolated to that mission and retried next
tick. Five new regressions cover cadence parity, startup/reload, save-failure
recovery, same-day/legacy handling, and concurrent reservation boundaries;
the engine suite passed 636/636. Live dwarfstar persistence acceptance remains
an operator step.

### Web Onboarding Demo improvement

The onboard-web demo's fake mt-admin now compiles v2.1.3 instead of
v1.1.9

### External Workers: kimi 2.1.1 and opencode2 v2.0.18

The kimi 2.1.1 pin was already present when goal 2 was verified; that goal's
new version change was opencode2 v2.0.17 → v2.0.18

- **kimi-code `2.1.1`.** The worker image build arg, CI source ref, and
  entrypoint contract move to `@moonshot-ai/kimi-code@2.1.1`
- **opencode2 `v2.0.18`.** The lane advances from v2.0.17 to v2.0.18 at
  reviewed full commit `cd9a14a6b688d4021bee381dfd39d2cef9c0f862` (tag SHA
  verified live against the upstream remote). Dockerfile pins, CI
  source_ref/build args, and the reproducible-build test fixture move in
  step
- opencode v1 stays at `v1.18.25` (`cb7d8b2f…`, deliberately un-adopted
  beyond) and pebble stays at `2d0f3786…`

### Worker Liveness and Timeout Evidence (WI#263/#267, goals 43/48)

- `JobContext.last_activity_at` tracks genuine CLI progress. Stuck detection
  uses idleness (falling back to `started_at`), not total elapsed runtime;
  synthetic `[worker]` notices do not refresh that idle baseline. A genuine
  late worker result can transition Stuck → Completed; rejected transitions
  are logged
- Kimi's `KIMI_SILENCE_TIMEOUT_MS` watchdog defaults to 15 minutes (0 disables
  it), capped by the task deadline. Real CLI stdout refreshes it; executor
  heartbeats do not. A silent CLI is terminated with a timed-out result carrying
  any genuine streamed partial work
- Both daemon-deadline and worker-reported timeouts retain a bounded 16 KiB
  tail in `metadata.partial_output` and a persisted `partial_output` job event,
  for `wait=true` and `wait=false`. `job_status` recovers the snapshot from
  events after daemon restart, and the timed-out result message includes output.
  Short genuine output is retained too; there is no 512-byte minimum.
- Evidence filtering is narrower than idle detection: only the synthetic
  `[worker] Still waiting for …; task remains active.` keep-alive sentence is
  stripped. Genuine `[worker]` provider-retry diagnostics and standalone
  whitespace deltas remain in order, preserving `alpha beta\ngamma` rather than
  collapsing it into `alphabetagamma`. Empty heartbeat-stripped timeout results
  fall back to the daemon's evidence buffer; non-timeout results are unchanged.

### Voice Long-Reply Failures

The gateway now preserves the sidecar's 422 processing failure, 503 unavailable,
and 504 timeout semantics rather than returning one opaque 502. A duration
pre-flight rejects over-cap synthesis requests with 422 `text_too_long` and
accounts for the requested speed across 0.5–2.0; a reply exceeding the cap at
1x may fit at 2x. `/api/voice/status` exposes `max_synth_audio_secs`.

The proxy timeout derives from `VOICE_REQUEST_TIMEOUT_SECS` plus 30 seconds
of headroom, and mt-admin mirrors that knob and `VOICE_MAX_TTS_AUDIO_SECS`
from `voice.env` into `lunarwing.env` on voice mutation paths. Response details
stay generic for clients; upstream diagnostics remain server-side. This does
not fix the separate sidecar inference-permit/TTS-mutex retention defect
(WI#264), which remains deferred. See the
[gateway voice API guide](docs/guides/GATEWAY_VOICE_API.md).

### Tool Inventory Completeness and Identifier Compatibility (WI#245, goal 46)

`tool_list` returns deterministic entry pages (`offset`, `limit`, `next_offset`,
`truncated`) and per-entry tool-name windows (`tools_offset`, `tools_limit`,
`tools_total`, `tools_next_offset`, `tools_truncated`). Callers must follow both
continuation layers; a single page is not the full inventory. Serialized
entries are capped at 32 KiB and pretty-printed pages at 60 KiB before the
safety layer, with continuation metadata recomputed when entries are dropped.

Identifiers are not elided. MCP `enabled`, `placement`, `transport`, `url`, and
`command` remain available; bounded free text carries an explicit elision
marker. Genuinely empty results return an empty page, while a nonempty page
that cannot fit the budget errors instead of pretending to be complete.
New extension names are limited to 200 bytes and discovered MCP tool names
to 256 characters. Lifecycle operations grandfather longer already-installed
extension names while still rejecting traversal-shaped names. The web gateway's
internal extension listing remains full-sized. Contract:
[tools README](ic/src/tools/README.md#tool_list-pagination-and-completeness-wi245).

### GitLab Issue Description Alias

GitLab tool 0.1.6 accepts either `description` or its `body` alias on
`create_issue`/`update_issue`. Supplying both non-null values fails with a clear
same-field error; a null twin is dropped before deserialization. Notes keep
their real `body` field, and merge-request writes and issue reads do not acquire
the alias. The crate, capability file, registry entry, and tool lockfile advance
together. Live issue creation using the alias remains an operator step

### OpenCode 2 Provisioning Selection

CLI and web onboarding now offer `opencode2` with optional model/base-URL
overrides, forwarding selection to mt-admin's add/build phases. Configuration
round-trips the fields; deselecting the worker drops its overrides. The
`opencode2-` tenant-name prefix is reserved consistently with mt-admin.
Kawarimi import does not yet forward `--with-opencode2`, so the import form
deliberately omits this option

### Standalone WASM Capability Ownership

`install-wasm` now transfers channel/tool directory and copied-file ownership
to the tenant **before** invoking tenant-UID capability writers. Fresh WeeChat
capabilities therefore receive allocated tenant ports instead of retaining
generic 9001/6681 endpoints. Root/unprivileged fixtures cover first install,
writer refusal on root-owned inputs, mode 0600, allocated ports, and repeat
install; the root fixture also handles CI images without a `nobody` group.

### Cargo-Deny Advisory Ignore Audit

All 8 advisory ignores in `ic/deny.toml` were re-verified against upstream
as of 2026-09-26; none can be removed for v2.1.3.0: russh 0.63.3 (latest)
still requests rsa 0.10.0-rc.18 (RUSTSEC-2023-0071 survives a russh bump),
rsa 0.10.0-rc.18 is still the newest published release with no
constant-time fix for the Marvin advisory, libsql 0.9.30 stable and
0.10.0-pre.4 prerelease both still request hyper-rustls ^0.25 / hyper ^0.14
so the rustls-webpki 0.102 chain survives even a libsql bump, and
`failure` 0.1.8 remains the final release. Each ignore now carries its own
dated reason, with blocking upstream pins documented in the policy comments
and audit. No advisory IDs were removed
Verified with cargo-deny 0.20.2 (advisories/bans/licenses/sources all ok)
Evidence: `docs/ops/history/v2.1.3.0_CARGO_DENY_AUDIT.md`

The late-cycle Wasmtime/wasmtime-wasi 36.0.14 → 36.0.16 update addresses
RUSTSEC-2026-0314/0316 and refreshes the vendored set. WI#255's alert was
misattributed: RUSTSEC-2026-0101 concerns `safe-agent-rs`, which is absent here,
not PyO3. PyO3 0.28.3 is lockfile-only through jiter's unused Python feature;
the `lockfile-only-advisory-guard` CI job detects activation of that dependency
and its separate RUSTSEC-2026-0176 risk. It is not a PyO3 version bump or an
additional advisory ignore. The job-83933 clippy failure was separately fixed
with `std::slice::from_ref`, without suppressing the lint (goal 39).

### Version Bumps and Supply Chain

All eleven LunarWing-owned release packages move from `2.1.2` to `2.1.3`:
the daemon, the five workspace crates (`lunarwing_common`,
`lunarwing_engine`, `lunarwing_skills`, `lunarwing_safety`,
`lunarwing_health_metrics`), the XMPP bridge, and the four WASM channels
(darkirc, multica, weechat, xmpp — registry versions 0.1.9 / 0.1.12 /
0.2.10 / 0.1.15 respectively). Six lockfiles were refreshed, and the
tracked manifest/lock audit confirmed zero project-owned packages remain at
2.1.2. External crate updates in the same sweep: aws-lc-rs 1.18.1,
aws-lc-sys 0.45.0, rustls-webpki 0.103.15, zerovec-derive 0.11.5; the
initial vendoring audit found no drift for those four already-vendored pins;
the later Wasmtime 36.0.16 security update regenerated the vendor snapshot.
The recorded `cargo deny check` passed all categories. Channel behavior is
unchanged — the bumps are rebuild-only

### Documentation Parity

Every guide under `docs/guides/` (31 files outside `history/`) was
re-verified against the current tree and re-stamped for v2.1.3.0; three
broken intra-repo links were repaired. The architecture docs were repassed
for the sealed-step-record change (`ENGINE-V2.md` documents the
`StepRecord` role and the sealing boundary). The Hermes Kawarimi adapter
was audited for v2.1.3.0 breakage — none found
(`docs/ops/history/v2.1.3.0_HERMES_KAWARIMI_ADAPTER_AUDIT.md`). The ops index,
releases index, contributor-onboarding freeze status, and worker-container
pins were refreshed for the cycle

## Upgrade Notes

1. Back up PostgreSQL, tenant environment files, `/etc/lunarwing/ports.json`,
   workspace state, rendered units, worker data, voice models, and channel
   state.
2. No SQL migration is introduced; `V27` remains latest. The port registry
   stays at v17 — no registry migration on upgrade.
3. Rebuild the daemon and installed WASM extensions through the normal
   deployment path. All four channel registry versions changed (darkirc
   0.1.9, multica 0.1.12, weechat 0.2.10, xmpp 0.1.15) — behavior is
   unchanged, but the channels must be rebuilt/reinstalled. Rebuild/reinstall
   GitLab tool 0.1.6 for the issue-write alias; other tool registry versions
   are unchanged from v2.1.2.0.
4. **New-tenant env defaults (WI#239).** Newly provisioned tenants render
   the explicit defaults listed under Compatibility; existing tenants are
   not rewritten merely by replacing the binary. On an mt-admin re-render,
   existing values are preserved, but missing keys receive the new explicit
   defaults. Review the WI#239 list before provisioning or re-rendering if
   you previously relied on different in-code fallbacks.
5. **Legacy `AUTO_APPROVE_TOOLS` migration (goal 37).** On the next mt-admin
   re-render, a legacy-only `AUTO_APPROVE_TOOLS` value is migrated to the
   canonical `AGENT_AUTO_APPROVE_TOOLS` key and the legacy key is dropped
   from the generated file. Operators with automation reading the legacy
   key must switch to the canonical key.
6. **Kimi worker rebuild.** Rebuild the kimi worker image for `2.1.1` and
   note the default endpoint change (NetBird overlay +
   `FrontierCODEK`); tenants that pin `KIMI_BASE_URL`/`KIMI_MODEL`
   explicitly are unaffected. The retuned runtime defaults (heartbeat
   120000ms, kill grace 7000ms, 8 concurrent tasks, 4h timeout) apply to
   new renders; explicit tenant values keep precedence.
7. **opencode2 rebuild.** Rebuild the opencode2 worker image at `v2.0.18`
   (`cd9a14a6b…`); pull worker images by the digest recorded in
   `worker-image-evidence.json`.
8. **Podman probe shape.** Hosts with hardened user units benefit
   immediately from the client-only `podman version` probe; no operator
   action. If you maintain external tooling that parses `podman --version`
   from LunarWing logs, note the probe output shape is now the formatted
   client version.
9. **Sealed step records.** Existing (pre-2.1.3.0) transcripts are
   unaffected and remain readable under the legacy `User` spelling. New
   CodeAct step output is sealed into `internal_messages`; if you have
   tooling that scraped step blobs from `thread.messages`, use trace
   diagnostics or engine-internal records instead; the operator DTO
   (`get_engine_thread`) intentionally excludes sealed content too.
   Channel status hints ("Code executed", etc.) continue to work
   for both record shapes.
10. **WeeChat relay rotation (WI#202).** `rotate-relay-password <tenant>`
    briefly interrupts the tenant's running WeeChat consumers; schedule it
    accordingly. Before rotating an existing tenant, run
    `lunarwing-mt-admin.sh render-units <tenant>` through the deployed
    mt-admin path to install the launcher/barrier and refresh systemd-user
    or OpenRC units. Rotation refuses unguarded effective units. Interrupted
    rotation or failed rollback leaves starts blocked: repair matching env
    credentials and rerun rotation, rather than deleting the recovery marker.
11. **Vendor snapshot.** `ic/vendor/` includes the Wasmtime 36.0.16 security
    update and the external crate pins above; if you maintain a local vendor
    override, refresh it
    with `ic/scripts/refresh-vendor.sh` before building.
12. The bundled-skills catalog is unchanged from v2.1.2.0 (exactly seven
    product skills); no skill migration is needed.
13. **Voice limits/timeouts (WI#259).** Re-render managed voice configuration
    so `VOICE_MAX_TTS_AUDIO_SECS` and `VOICE_REQUEST_TIMEOUT_SECS` agree between
    sidecar and daemon. Desktop clients should handle 422 `text_too_long` by
    chunking/trimming replies and distinguish 503/504 from a generic 502.
14. **Worker timeout behavior (WI#263/#267).** Deploy the updated daemon and
    Kimi image for idle tracking, durable genuine-output evidence, and the CLI
    silence watchdog. Configure `KIMI_SILENCE_TIMEOUT_MS` if a workload needs
    a different stdout-silence budget; 0 disables that watchdog, not the task
    deadline. Heartbeat-only timeouts intentionally have no partial evidence.
15. **Tool inventory pagination (WI#245).** Update consumers that assumed one
    `tool_list` call was exhaustive: follow entry and tool-name continuation
    offsets. New over-bound MCP tool names fail discovery; grandfathered
    installed extension names remain manageable without renaming.
16. **Mission day counters (WI#243).** Active persisted counters roll over at
    startup/next tick even without a trigger. An undated legacy mission keeps
    its existing count for the upgrade day. No manual JSON migration is needed;
    paused/terminal rows remain historical until the mission is active again.

### Security, Deployment, and Release Limitations (carried)

- DarkIRC Secure Key Exchange Stages B–D and automated QR/web/trusted-channel
  delivery remain disabled behind separate acceptance gates (carried
  unchanged)
- MCP does not auto-install npm/pip packages. Core MCP configuration is not
  automatically inherited by external workers; explicitly configured worker
  integrations (such as the OpenCode study-notes mount) remain separate.
- The onboarding web app remains a trusted local console without user
  accounts or TLS; keep its loopback binding and use an SSH tunnel for
  remote access (carried).
- OpenRC hosts without usable cgroup v2 delegation fall back to nice-only
  build scheduling (carried)
- `ALLOW_PRIVATE_IPS=1` opts out of public-address SSRF restrictions;
  enable only for intentionally trusted private-network deployments
  (carried)
- Kawarimi remains PostgreSQL-only (carried)
- OpenCode v1.18.29+ remains deliberately un-adopted for the opencode lane
  pending upstream proof (carried)
- Historical WI#159 behavior remains: terminal triggers
  (`thread_completed_with_issues`/`thread_completed_with_learnings`) still
  fire for Research child threads — only Mission threads are filtered — so
  event-triggered missions can self-sustain via their own research
  children; the default budget is 30 spawns/day per mission (custom/unlimited
  budgets can allow more).
- WI#191 (deferred by its own item): the EMA estimation module wiring
  starts after this release.
- WI#184/#220 (deferred to 2.1.4.0): secrets-store pairing redesign and
  per-tenant system-prompt editing; the WI#200 WeeChat pairing 30-day
  stopgap remains in place until #184 lands.
- WI#264 (2.1.4.0): a timed-out Kokoro synthesis can retain the sidecar's
  inference permit and TTS mutex. The WI#259 gateway error/timeout changes
  do not cancel that underlying work or solve resource retention.
- WI#244 (reported, deferred to 2.1.4.0): intermittent FrontierTEEC/TensorZero
  response-body decode failures can abort orchestrator mission threads. The
  daily-counter fix does not establish a fix for that provider failure.
- WI#265 (2.1.4.0): legacy `routine_history` does not resolve Engine V2 mission
  names. Use the mission tools and gateway Missions detail view for mission
  state/history; the routine and mission registries remain distinct.
- OpenCode 2 is selectable for provisioning, not Kawarimi import. Goal 12's
  live rehearsal covered Pebble/OpenCode on systemd user units, import
  dry-run and exported-DB restore verification — not live OpenRC, model-backed
  jobs, Kimi/OpenCode 2, import apply, or cross-host cutover

### Resolved or Source-Fixed Since v2.1.2.0

- Engine-authored CodeAct step output is sealed out of the user-visible
  transcript (WI#227 follow-up: sealed role, host-owned classification,
  internal placement, delegated-worker exclusion, trace coverage, and the
  operator DTO boundary regression).
- Channel code-execution status hints survive the seal, and the
  error-precedence bug (`[stdout]\nOSError: …` reporting "Code executed")
  is fixed.
- WeeChat relay credentials have a managed rotation verb with
  concurrency-safe, symlink/hard-link-refusing, no-secret-in-argv handling
  (WI#202 source-fixed).
- Tenant env/capability writes are safe against temp-path swaps (no
  root-created temps in tenant dirs) and partial writes (staged producers
  with checked exit status before the atomic commit) — the WI#257
  zero-byte-replacement mode is fixed and regression-pinned.
- New tenants get explicit, override-preserving env defaults including
  circuit-breaker 5/15 and turn/request timeouts (WI#239).
- The canonical `AGENT_AUTO_APPROVE_TOOLS` key is preserved on re-render,
  legacy values migrate, and XMPP new-tenant defaults are correct with
  bridge allowlist parity
- The Podman version probe no longer joins the rootless namespace
  constructor, and the sandbox reaper runs isolated from the hardened
  daemon — the pause-pid unlink class is closed
- The real web onboarding lifecycle (export, restore, restart) is
  verified within the scoped Pebble/OpenCode rehearsal: encrypted export,
  temporary-DB restore verification, import dry-run, and source restart.
- The gateway exposes read-only mission list/summary/detail routes and a
  Missions tab with auth enforcement and no-engine degradation (WI#217).
- The onboard-web demo builds v2.1.3 and the mascot defaults to hidden
- Fresh standalone `install-wasm` transfers ownership before patching
  tenant capabilities, so WeeChat receives its allocated ports (goal 41).
- Long voice replies have speed-aware pre-flight and preserved sidecar error
  semantics with staggered timeouts
- Worker stuck detection is idle-based, late genuine completion is accepted,
  Kimi has a silence watchdog, and genuine timeout evidence survives restart
  without heartbeat pollution or lost whitespace separators
- `tool_list` has bounded, lossless-identifier pagination; MCP status/config
  fields and lifecycle access to pre-cap long names are preserved
- GitLab tool 0.1.6 accepts the issue-write `body` alias with explicit
  duplicate-field handling
- OpenCode 2 provisioning selection/overrides are wired through CLI and
  web onboarding
- Active missions persist daily-budget rollover at startup/tick, with
  same-day/legacy handling and concurrent-reservation safety
- Worker pins: kimi-code 2.1.1 (NetBird-overlay default endpoint restored,
  `FrontierCODEK` function, retuned runtime defaults), opencode2 v2.0.18
  at `cd9a14a6b…`; opencode and pebble unchanged
- The cargo-deny advisory ignore list is fully re-verified with
  dated reasons and dependency-chain evidence (WI#198). Wasmtime/wasmtime-wasi
  36.0.16 addresses RUSTSEC-2026-0314/0316; CI additionally guards the
  lockfile-only PyO3 activation risk (WI#255) and the clippy regression is fixed
