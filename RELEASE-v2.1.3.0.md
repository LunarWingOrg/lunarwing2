# Release Notes for LunarWing v2.1.3.0 (DRAFT)

## Codename: Ō-Yoroi (大鎧, the Great Armor)

**Release date** 2026-09-28

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
the closing of a year-old credential-rotation gap.

The headline engine work seals CodeAct interpreter step records out of the
user-visible transcript (work item #227 follow-up). Engine-authored step
blobs (`[stdout]`, `[name ERROR]`, `[return]`) were previously recorded as
role-User messages and synced into every surface that documents human
statements; the class of leak behind the Kageho "stale date system error"
incident is now closed structurally — a new `SealedStepRecord` role is
host-minted, rerouted into `internal_messages`, excluded from delegated-worker
context, and classified by a host-owned decision that only two VM-unforgeable
shapes can claim. The trace analyzer still surfaces actionable code errors
over sealed records, and channel UIs keep their "Code executed" status hints
via a follow-up that also fixed an error-precedence bug the new coverage
exposed.

The multi-tenant operator surface received the release's largest hardening
program. WeeChat relay credentials gained a managed rotation verb
(`rotate-relay-password`) with lifecycle-locking against concurrent starts,
hard-link and symlink refusal, a setuid-free exec helper that never passes
the secret through argv or the environment, and consumer-aware restarts
(closing WI#202's no-rotation gap). Tenant env and capability secret writes
were hardened against temp-path swaps and partial writes: a root-tenant
writer separation replaces mktemp-and-rename everywhere, and — the WI#257
review finding — every fallible producer now stages its complete result in
root-private space and has its exit status checked before the tenant-side
atomic commit, so a failed read or jq parse can no longer replace a valid
env/capabilities file with a zero-byte result (the reproducing failure mode:
a mode-000 `lunarwing.env` became a 0-byte 0600 file). New tenants get
explicit, documented env defaults (WI#239) — circuit-breaker 5/15,
`LLM_TURN_BUDGET_SECS=599`, `LLM_REQUEST_TIMEOUT_SECS=2599`, sandbox policy
`workspace_write`, and more — each wrapped to preserve operator overrides on
re-render, alongside corrected XMPP defaults and the canonical
`AGENT_AUTO_APPROVE_TOOLS` key with legacy-key migration. The web onboarding
path was completed against a real tenant lifecycle: Podman namespace
recovery ordering, reaper isolation from the hardened daemon, non-destructive
client version probes, and required-worker verification with a recorded
export/restore/restart rehearsal.

The sandbox itself was stabilized under the hardened daemon: `podman
--version` joins the rootless namespace constructor and under a hardened
user unit could mistake a live-but-unreadable pause process for a recycled
PID and unlink `pause.pid`; the version probe now uses the client-only
`podman version --format` shape, and the reaper's sweep commands run through
the same manager-forked transient-service isolation as sandbox execution so
even `podman ps` from the hardened daemon cannot invalidate live pause
state. Writer quiescence and sandbox-image builds were hardened the same
way.

The web gateway gained a read-only tenant-facing Missions surface (WI#217):
`GET /api/missions`, `/api/missions/summary`, and `/api/missions/{id}` over
the existing bridge ownership checks, plus a Missions tab with summary cards
and mission detail — observability without a chat turn. The web onboarding
demo now builds v2.1.3 and the mascot is hidden by default (WI#234).
External workers were refreshed: kimi-code moves to `2.1.1` with a restored
NetBird-overlay default endpoint and a dedicated `FrontierCODEK` TensorZero
function (plus retuned runtime defaults — longer heartbeat, longer kill
grace, 8 concurrent tasks, 4-hour timeout), and opencode2 advances to
`v2.0.18` at reviewed commit `cd9a14a6b…`. The cargo-deny advisory ignore
list was re-verified against upstream (all 8 still required, each with a
self-contained reason), and the release identifier moves to `2.1.3` across
all eleven LunarWing-owned release packages with the four channel registry
packages bumped in step.

The release identifier is `2.1.3.0`; LunarWing-owned Rust crates and channel
packages use `2.1.3`. Rust 2024 and MSRV `1.96.1` are unchanged. There are
no new SQL migrations (`V27__memory_index_outbox.sql` remains latest). The
multi-tenant port registry stays at v17. Yoroi laced the plates; Ō-Yoroi is
what the craftsman built once every joint was known.

See the [v2.1.2.0 release notes](docs/releases/RELEASE-v2.1.2.0.md) for the
previous compatibility baseline.

## Highlights

- **Sealed step records (WI#227 follow-up).** Machine-generated CodeAct step
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
  roles.
- **Managed WeeChat relay password rotation (WI#202, goal 8).** New mt-admin
  verb `rotate-relay-password <tenant>` closes the long-standing gap where
  `RELAY_PASSWORD` had no rotation protocol (only `rotate-pg-password`
  existed). The rotation runs under the tenant lifecycle lock with a
  start barrier against concurrent units, refuses missing/symlinked
  credential files and unsafe hard links, stages the new secret through a
  root-private temp path committed by the tenant-side atomic writer via a
  dedicated `lunarwing-relay-env-exec` helper (no secret in argv or the
  child environment), rewrites `relay.conf` as the tenant, and restarts
  only the running consumers with a brief, documented interruption.
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
  assert the previous file stays byte-for-byte intact.
- **Explicit env defaults for new tenants (WI#239).** add-tenant now renders
  explicit lines for `ALLOW_LOCAL_TOOLS`, `AGENT_AUTO_APPROVE_TOOLS`,
  `AGENT_MAX_TOOL_ITERATIONS=99`, `SANDBOX_POLICY=workspace_write`,
  `SANDBOX_ALLOW_FULL_ACCESS=false`, `SKILL_SELF_IMPROVEMENT=true`,
  `SKILL_PRUNE_QUARANTINE_DAYS`, `REFLEX_COMPILER_ENABLED=false`,
  `ENGINE_V2_MODEL_RECOVERY_ATTEMPTS`,
  `ENGINE_V2_MODEL_RECOVERY_TOTAL_ATTEMPTS`,
  `ENGINE_V2_TOOL_ASYNC_TIMEOUT_SECS`, plus circuit-breaker defaults 5/15,
  `LLM_TURN_BUDGET_SECS=599`, and `LLM_REQUEST_TIMEOUT_SECS=2599` (and the
  stale IRONCLAW header is dropped). Every default is wrapped in
  `_env_existing` so an operator's custom value survives add-tenant and
  upgrade re-renders.
- **XMPP defaults and approval-override preservation (goal 37).** mt-admin
  writes only the canonical `AGENT_AUTO_APPROVE_TOOLS=false` for new
  tenants; re-renders prefer the existing canonical value, migrate a
  legacy-only `AUTO_APPROVE_TOOLS` value, and drop the legacy key from the
  generated file. Fresh tenants default to
  `XMPP_JID=<tenant>@xmpp.localhost` and
  `XMPP_ALLOW_FROM=user@xmpp.localhost`, with the bridge's JSON allowlist
  using the same default and no longer adding the agent JID to a stored
  operator allowlist. Explicit flags and existing values keep their
  precedence.
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
- **Tenant-facing Missions observability (WI#217).** The web gateway exposes
  read-only mission routes (`GET /api/missions`, `/api/missions/summary`,
  `GET /api/missions/{id}`) backed by the existing bridge
  `list_engine_missions`/`get_engine_mission` ownership checks, and a new
  Missions tab shows a mission list (name/cadence/threads/today's
  budget/last fired/status), summary cards (total/active/paused/
  budget-spent/never-fired), and a detail view with goal, current focus,
  success criteria, cadence JSON, approach history, and spawned threads —
  EN and zh-CN. Missions remain managed via the model-facing tools.
- **External worker refresh.** kimi-code advances to `2.1.1` (build arg,
  CI source ref, and entrypoint contract), restoring the
  NetBird-overlay default endpoint
  (`http://cotterhomelab3.netbird.selfhosted:3000/openai/v1`) after the
  plain-IP interim, moving to the dedicated `FrontierCODEK` TensorZero
  function, and retuning runtime defaults (`KIMI_PROGRESS_HEARTBEAT_MS`
  60000→120000, `KIMI_KILL_GRACE_MS` 5000→7000,
  `KIMI_MAX_CONCURRENT_TASKS` 4→8, `KIMI_MAX_TIMEOUT_MS` 3600000→
  14400000). opencode2 advances `v2.0.17` → `v2.0.18` at reviewed commit
  `cd9a14a6b688…`, verified live against the upstream remote. opencode
  stays `v1.18.25`; pebble stays `2d0f3786`.
- **cargo-deny advisory ignores re-verified (WI#198/#199).** All 8 advisory
  ignores were re-verified against upstream as of 2026-09-26 — russh
  0.63.3 still requests rsa 0.10.0-rc.18, no constant-time rsa release
  exists, the libsql rustls-webpki 0.102 chain survives even a libsql
  bump, and `failure` 0.1.8 remains final — and each ignore now carries a
  self-contained reason with the re-verification date and the exact
  upstream pin keeping it blocked. No ids were removed. Evidence:
  `docs/ops/history/v2.1.3.0_CARGO_DENY_AUDIT.md`.
- **Web onboarding demo and mascot default (WI#234).** The onboard-web
  demo's fake mt-admin compiles v2.1.3 (not v1.1.9), and the bat mascot is
  hidden by default with opt-in via settings.

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
| External crate updates | aws-lc-rs 1.18.0→1.18.1, aws-lc-sys 0.44.0→0.45.0, rustls-webpki 0.103.14→0.103.15, zerovec-derive 0.11.4→0.11.5; `ic/vendor/` regenerated with no source drift |
| Worker image tool pins | kimi-code `2.1.1` (2.x line; default endpoint back on the NetBird overlay with the `FrontierCODEK` function); opencode2 `v2.0.18` at `cd9a14a6b688d4021bee381dfd39d2cef9c0f862`; opencode `v1.18.25` at `cb7d8b2f…` (no change); pebble `2d0f3786…` (no change) |
| Engine transcript roles | New `SealedStepRecord`/`StepRecord` wire role for interpreter step records; legacy pre-seal transcripts keep the `User` spelling and remain readable |
| Podman version probe | `podman version --format …` (client-only) replaces `podman --version` in the sandbox detect/probe/reaper paths; same `(major, minor)` output contract |
| New tenant env defaults | WI#239 explicit defaults render at add-tenant (see Highlights); re-renders preserve operator values |
| XMPP defaults (new tenants) | `XMPP_JID=<tenant>@xmpp.localhost`, `XMPP_ALLOW_FROM=user@xmpp.localhost`; bridge JSON allowlist matches; existing JIDs/allowlists and explicit flags keep precedence |
| Auto-approve env key | `AGENT_AUTO_APPROVE_TOOLS` is the only key written for new tenants; legacy `AUTO_APPROVE_TOOLS` is migrated on re-render and dropped from generated files |
| mt-admin verbs | New: `rotate-relay-password <tenant>`; all existing verbs unchanged |
| Mascot default | Hidden (opt-in via `MASCOT_ENABLED=true` / settings); web onboard demo targets v2.1.3 |
| Deny policy | `ic/deny.toml` advisory ignores unchanged in id set (8); each entry now carries a self-contained reason, re-verification date, and blocking upstream pin |

## Changes

### Sealed Interpreter Step Records (WI#227, goal 34)

The orchestrator recorded every CodeAct step blob (`[stdout]`, `[name
ERROR]`, `[return]`) as a role-User message in its working transcript, which
synced into `thread.messages` through the `json_to_thread_messages` user
fallback — so engine-authored interpreter output entered records and API
surfaces that document human statements. That was the remaining root cause
behind the Kageho-reported leak class (the old "stale date system error"
OSError line read as a prompt-injection attempt; denial-path text could land
the same way).

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

Review remediation (the `!1183` findings) kept the seal defense-in-depth
without losing the diagnostics: the trace `code_error` analyzer matches
sealed records as well as legacy user blobs so interpreter errors keep
producing the actionable warnings the mission self-improvement trigger
consumes; the orchestrator regression drives `sync_runtime_state` over
flagged working messages and asserts zero leakage into `thread.messages`;
and the router test pins that the operator thread DTO (`get_engine_thread`'s
served set) can never carry a sealed role or sealed diagnostic text.

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
[`docs/ops/GOAL34_SEALED_STEP_OUTPUT.md`](docs/ops/GOAL34_SEALED_STEP_OUTPUT.md).

### Managed WeeChat Relay Password Rotation (WI#202, goal 8)

`RELAY_PASSWORD` previously had no rotation protocol — only
`rotate-pg-password` existed, leaving the WeeChat relay credential
effectively permanent for a tenant's lifetime. The new mt-admin verb
`rotate-relay-password <tenant>`:

- runs under the tenant lifecycle lock with a relay start barrier
  (`ic/scripts/lunarwing-relay-env-exec.py` installs as
  `/usr/local/libexec/lunarwing-relay-env-exec`) so a concurrent
  `start-tenant` cannot race the credential swap;
- refuses missing, symlinked, or hard-linked credential files and verifies
  the env directory contract before touching anything;
- stages the new secret through a root-private temp path and commits it via
  the tenant-side atomic writer — the secret never appears in argv or any
  child environment, and the dedicated `weechat.env` (mode 0600,
  `RELAY_PASSWORD` only) is never reopenable by root;
- reads the old credential as the tenant so a swapped source path cannot
  make root read another file, and rewrites `relay.conf` as the tenant;
- restarts only the running WeeChat consumers (brief service interruption)
  so the daemon and adapters converge on the new credential.

Harness coverage: `test-rotate-relay-password.sh` (271 lines, including the
concurrency symlink/ownership pre-tests), `test-relay-start-barrier.py`,
and `test-relay-password-live.sh`.

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

### Explicit Env Defaults for New Tenants (WI#239)

`06-env-generation.sh` now renders explicit defaults at add-tenant time
instead of relying on in-code fallbacks: `ALLOW_LOCAL_TOOLS`,
`AGENT_AUTO_APPROVE_TOOLS`, `AGENT_MAX_TOOL_ITERATIONS=99`,
`SANDBOX_POLICY=workspace_write`, `SANDBOX_ALLOW_FULL_ACCESS=false`,
`SKILL_SELF_IMPROVEMENT=true`, `SKILL_PRUNE_QUARANTINE_DAYS`,
`REFLEX_COMPILER_ENABLED=false`,
`ENGINE_V2_MODEL_RECOVERY_ATTEMPTS`,
`ENGINE_V2_MODEL_RECOVERY_TOTAL_ATTEMPTS`,
`ENGINE_V2_TOOL_ASYNC_TIMEOUT_SECS`, circuit-breaker threshold/recovery
5/15, `LLM_TURN_BUDGET_SECS=599`, and `LLM_REQUEST_TIMEOUT_SECS=2599`. The
stale IRONCLAW legacy header is dropped. Existing tenant-customized vars
(`XMPP_JID`, `XMPP_ALLOW_FROM`, `LLM_BASE_URL`) remain
substitution-driven so onboarding/web flows keep working. Every new default
is wrapped in `_env_existing`, so an operator's custom value survives
add-tenant/upgrade re-renders — matching the existing `SANDBOX_RUNTIME` /
`LLM_CIRCUIT_BREAKER_*` preservation pattern. Verified by the extended
`test-add-tenant-env-flags.sh` harness.

### XMPP Defaults and Approval-Override Preservation (goal 37)

mt-admin's env generation previously wrote only the legacy
`AUTO_APPROVE_TOOLS` key and could clobber an operator's canonical
`AGENT_AUTO_APPROVE_TOOLS=true` on re-render. New tenants now render
`AGENT_AUTO_APPROVE_TOOLS=false` only; re-renders prefer the existing
canonical value, migrate a legacy-only `AUTO_APPROVE_TOOLS` value, and drop
the legacy key from the generated file. Fresh tenants default to
`XMPP_JID=<tenant>@xmpp.localhost` and
`XMPP_ALLOW_FROM=user@xmpp.localhost`, and the bridge's JSON allowlist uses
the same default while no longer adding the agent JID to a stored operator
allowlist. Explicit flags and existing JIDs/allowlists retain their
precedence. Verified by `test-add-tenant-env-flags.sh` (canonical/legacy
precedence, idempotent re-renders, local-user allowlist and bridge parity)
and the onboard unit tests.

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
  descendant reaping on timeout.
- **Writer quiescence and sandbox-image builds.** Stop/export writer
  quiescence verification and the per-tenant sandbox-image build path were
  hardened in the same pass (goal 12), including the userns-repair
  intermediate-step exception, with harness coverage in
  `test-mt-admin-podman-repair-quiescence.sh` and the extended
  `test-mt-admin-worker-lifecycle.sh` / `test-mt-admin-sandbox-image.sh`.

### Web Onboarding Lifecycle Acceptance (goal 12)

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

### Web Onboarding Demo and Mascot Default (WI#234)

The onboard-web demo's fake mt-admin now compiles v2.1.3 instead of
v1.1.9, and the bat mascot defaults to hidden (opt-in via settings) —
aligning the demo with the current release line and the production mascot
default. In-browser tests drive all five onboarding flows.

### External Workers: kimi 2.1.1 and opencode2 v2.0.18 (goal 2)

- **kimi-code `2.1.1`.** The worker image build arg, CI source ref, and
  entrypoint contract move to `@moonshot-ai/kimi-code@2.1.1`. The default
  model endpoint returns to the NetBird overlay
  (`http://cotterhomelab3.netbird.selfhosted:3000/openai/v1`) — restoring
  the NetBird default after the plain-IP interim — and selects the
  dedicated `FrontierCODEK` TensorZero function (was `FrontierCODE`). The
  WI#206 fail-closed `PreToolUse` Bash guard is live-verified against
  2.0.2 behavior with the 2.1.1 contract gate running during the worker
  image build. Runtime defaults retuned: `KIMI_PROGRESS_HEARTBEAT_MS`
  120000, `KIMI_KILL_GRACE_MS` 7000, `KIMI_MAX_CONCURRENT_TASKS` 8,
  `KIMI_MAX_TIMEOUT_MS` 14400000.
- **opencode2 `v2.0.18`.** The lane advances from v2.0.17 to v2.0.18 at
  reviewed full commit `cd9a14a6b688d4021bee381dfd39d2cef9c0f862` (tag SHA
  verified live against the upstream remote). Dockerfile pins, CI
  source_ref/build args, and the reproducible-build test fixture move in
  step.
- opencode stays at `v1.18.25` (`cb7d8b2f…`, deliberately un-adopted
  beyond) and pebble stays at `2d0f3786…`.

### Cargo-Deny Advisory Ignore Audit (WI#198/#199, goal 3)

All 8 advisory ignores in `ic/deny.toml` were re-verified against upstream
as of 2026-09-26; none can be removed for v2.1.3.0: russh 0.63.3 (latest)
still requests rsa 0.10.0-rc.18 (RUSTSEC-2023-0071 survives a russh bump),
rsa 0.10.0-rc.18 is still the newest published release with no
constant-time fix for the Marvin advisory, libsql 0.9.30 stable and
0.10.0-pre.4 prerelease both still request hyper-rustls ^0.25 / hyper ^0.14
so the rustls-webpki 0.102 chain survives even a libsql bump, and
`failure` 0.1.8 remains the final release. Each ignore now carries its own
self-contained reason with a re-verification date and the exact upstream
pin keeping it blocked — no more bare strings or "same as X" boilerplate.
Verified with cargo-deny 0.20.2 (advisories/bans/licenses/sources all ok).
Evidence: `docs/ops/history/v2.1.3.0_CARGO_DENY_AUDIT.md`.

### Open Work-Item Audit and Half-Broken-Tenant Runbook (goals 19, 4)

The full open-item tracker was re-audited at HEAD `eaa9c7ea5`
(`docs/ops/v2.1.3.0_OPEN_WORK_ITEMS_STATUS.md`): of 13 open items, 6 were
addressed in this goal cycle (WI#240, #239, #234, #217, #185, #198 —
tracker closure outstanding), 1 explicitly deferred by its own body (#191,
post-2.1.3.0 EMA wiring), 2 deferred to 2.1.4.0 (#184, #220), 1 stopgap in
place (#200 WeeChat pairing patch), and no 2.1.3.0 code blocker remains
among the open items. A new troubleshooting runbook for half-broken
tenants after upgrade (WI#240/#238 sandbox-image and WI#229 voice-store
symptoms) landed at
`docs/ops/TROUBLESHOOTING-HALF-BROKEN-TENANT-UPGRADE.md` and is indexed
from the MT production troubleshooting docs.

### Version Bumps and Supply Chain (goal 1)

All eleven LunarWing-owned release packages move from `2.1.2` to `2.1.3`:
the daemon, the five workspace crates (`lunarwing_common`,
`lunarwing_engine`, `lunarwing_skills`, `lunarwing_safety`,
`lunarwing_health_metrics`), the XMPP bridge, and the four WASM channels
(darkirc, multica, weechat, xmpp — registry versions 0.1.9 / 0.1.12 /
0.2.10 / 0.1.15 respectively). Six lockfiles were refreshed, and the
tracked manifest/lock audit confirmed zero project-owned packages remain at
2.1.2. External crate updates in the same sweep: aws-lc-rs 1.18.1,
aws-lc-sys 0.45.0, rustls-webpki 0.103.15, zerovec-derive 0.11.5; the
vendored set was regenerated (1,056 dirs) with no source drift, and
`cargo deny check` passes all categories. Channel behavior is unchanged —
the bumps are rebuild-only.

### Documentation Parity (goals 14, 17, 21)

Every guide under `docs/guides/` (31 files outside `history/`) was
re-verified against the current tree and re-stamped for v2.1.3.0; three
broken intra-repo links were repaired. The architecture docs were repassed
for the sealed-step-record change (`ENGINE-V2.md` documents the
`StepRecord` role and the sealing boundary). The Hermes Kawarimi adapter
was audited for v2.1.3.0 breakage — none found
(`docs/ops/v2.1.3.0_HERMES_KAWARIMI_ADAPTER_AUDIT.md`). The ops index,
releases index, contributor-onboarding freeze status, and worker-container
pins were refreshed for the cycle.

## Upgrade Notes

1. Back up PostgreSQL, tenant environment files, `/etc/lunarwing/ports.json`,
   workspace state, rendered units, worker data, voice models, and channel
   state.
2. No SQL migration is introduced; `V27` remains latest. The port registry
   stays at v17 — no registry migration on upgrade.
3. Rebuild the daemon and installed WASM extensions through the normal
   deployment path. All four channel registry versions changed (darkirc
   0.1.9, multica 0.1.12, weechat 0.2.10, xmpp 0.1.15) — behavior is
   unchanged, but the channels must be rebuilt/reinstalled. Tool registry
   versions are unchanged from v2.1.2.0.
4. **New-tenant env defaults (WI#239).** Newly provisioned tenants render
   the explicit defaults listed under Compatibility; existing tenants are
   untouched except that a re-render preserves any operator-set value and
   gains nothing the operator did not already have. If you relied on an
   in-code fallback differing from these defaults for new tenants, review
   the WI#239 list before provisioning.
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
   tooling that scraped step blobs from `thread.messages`, read the
   operator DTO (`get_engine_thread`) instead — it never carries sealed
   content. Channel status hints ("Code executed", etc.) continue to work
   for both record shapes.
10. **WeeChat relay rotation (WI#202).** `rotate-relay-password <tenant>`
    briefly interrupts the tenant's running WeeChat consumers; schedule it
    accordingly. The verb requires the tenant lifecycle lock path and
    installs `lunarwing-relay-env-exec` under `/usr/local/libexec` on
    first use — re-run the mt-admin install/refresh step if you deploy the
    script tree manually.
11. **Vendor snapshot.** `ic/vendor/` was regenerated for the four external
    crate updates; if you maintain a local vendor override, refresh it
    with `ic/scripts/refresh-vendor.sh` before building.
12. The bundled-skills catalog is unchanged from v2.1.2.0 (exactly seven
    product skills); no skill migration is needed.

### Security, Deployment, and Release Limitations (carried)

- DarkIRC Secure Key Exchange Stages B–D and automated QR/web/trusted-channel
  delivery remain disabled behind separate acceptance gates (carried
  unchanged).
- `proxy-enforced` sandbox mode remains implemented and gated; live
  docker-runtime e2e was not run on the dev VM (carried).
- MCP does not auto-install npm/pip packages and does not pass configured
  MCP servers into `lunarwing-agent-v1` external workers (carried).
- The onboarding web app remains a trusted local console without user
  accounts or TLS; keep its loopback binding and use an SSH tunnel for
  remote access (carried).
- OpenRC hosts without usable cgroup v2 delegation fall back to nice-only
  build scheduling (carried).
- `ALLOW_PRIVATE_IPS=1` opts out of public-address SSRF restrictions;
  enable only for intentionally trusted private-network deployments
  (carried).
- Kawarimi remains PostgreSQL-only (carried).
- OpenCode v1.18.29+ remains deliberately un-adopted for the opencode lane
  pending upstream proof (carried).
- WI#159 (open): terminal triggers
  (`thread_completed_with_issues`/`thread_completed_with_learnings`) still
  fire for Research child threads — only Mission threads are filtered — so
  event-triggered missions can self-sustain via their own research
  children; blast radius remains 30 spawns/day per mission (carried).
- WI#191 (deferred by its own item): the EMA estimation module wiring
  starts after this release.
- WI#184/#220 (deferred to 2.1.4.0): secrets-store pairing redesign and
  per-tenant system-prompt editing; the WI#200 WeeChat pairing 30-day
  stopgap remains in place until #184 lands.
- WI#41 (open at draft time): on a fresh `install-wasm`,
  `05-tenant-build.sh` copies `weechat.capabilities.json` as root before
  the channels-dir chown, so the tenant-UID writer rejects the fresh
  root-owned file and the standalone install leaves generic 9001/6681
  ports; a subsequent `start-tenant` repatches it. Ownership must be
  transferred before patching.

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
  (WI#202 closed in source; tracker closure outstanding).
- Tenant env/capability writes are safe against temp-path swaps (no
  root-created temps in tenant dirs) and partial writes (staged producers
  with checked exit status before the atomic commit) — the WI#257
  zero-byte-replacement mode is fixed and regression-pinned.
- New tenants get explicit, override-preserving env defaults including
  circuit-breaker 5/15 and turn/request timeouts (WI#239).
- The canonical `AGENT_AUTO_APPROVE_TOOLS` key is preserved on re-render,
  legacy values migrate, and XMPP new-tenant defaults are correct with
  bridge allowlist parity (goal 37).
- The Podman version probe no longer joins the rootless namespace
  constructor, and the sandbox reaper runs isolated from the hardened
  daemon — the pause-pid unlink class is closed (goal 12).
- The real web onboarding lifecycle (export, restore, restart) is
  verified end-to-end on a live tenant with regression coverage.
- The gateway exposes read-only mission list/summary/detail routes and a
  Missions tab with auth enforcement and no-engine degradation (WI#217).
- The onboard-web demo builds v2.1.3 and the mascot defaults to hidden
  (WI#234).
- Worker pins: kimi-code 2.1.1 (NetBird-overlay default endpoint restored,
  `FrontierCODEK` function, retuned runtime defaults), opencode2 v2.0.18
  at `cd9a14a6b…`; opencode and pebble unchanged.
- The cargo-deny advisory ignore list is fully re-verified with
  self-contained reasons and upstream pins (WI#198); the open-item audit
  found no remaining 2.1.3.0 code blocker.
- All eleven LunarWing-owned release packages and four channel registry
  packages are at 2.1.3 / current bumps; the vendored snapshot is
  regenerated and cargo-deny clean.
