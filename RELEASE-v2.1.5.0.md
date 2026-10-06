# Release Notes for LunarWing v2.1.5.0

## Codename: Bungei Fukkō (文芸復興): The Renaissance

**Release date:** 2026-10-XX (not yet released; first draft 2026-10-05)

> Ink long dried wakes;  
> old letters learn new light —  
> the arts rise again. 文芸復興

After Kabuto crowned the armor, **Bungei Fukkō opens the workshop**: a
revival of arts and letters — recovery paths, honest failure messages, and
memory tooling rebuilt with fresh intent.

## Overview

LunarWing v2.1.5.0 is an engine-reliability and operator-truthfulness
release. Engine V2 boot recovery now reaches orphan-project threads,
preserves pending approvals and resumable checkpoints across restarts, and
records restart-lost mission fires instead of silently dropping them.
Stopped and budget-exhausted threads persist honest terminal states, and
malformed orchestrator outcomes fail loud instead of being absorbed.

Model-facing honesty improves across the board: content-filter refusals and
output-budget truncations render distinct terminal guidance, usage billed to
intercepted empty streams is accounted in thread totals and telemetry, and
context-overflow recovery runs its full shrink ladder at default
configuration. Mission tooling gains real discovery (`tool_info` for engine
capabilities), truthful resume leases across restarts, and a completion
contract that no longer accepts a run's self-declared success at face value.

Workspace memory writes return actionable rejection errors, the safety
sanitizer's detect-versus-neutralize contract is documented and pinned, and
tenants gain a read-only, secret-scrubbed `/tenant-logs` view of the daemon
log. Secrets deletion gains non-bypassable per-call approval with a V28
grant-cleanup migration. The cycle closes with the usual dependency sweep
(2.1.4 → 2.1.5 across all owned packages, refreshed registry versions,
regenerated vendor tree) and a full crate-security re-audit.

See the [v2.1.4.0 release notes](docs/releases/RELEASE-v2.1.4.0.md) for the
previous compatibility baseline.

## Highlights

- **Boot recovery that misses nothing (WI#301).** Global restart recovery
  now includes threads whose project record is gone, preserves `Waiting`
  approvals and resumable checkpoints, and records — with best-effort owner
  notification — mission fires lost to a restart, without automatically
  replaying side effects. History persists before acknowledgement, and
  thread-ID deduplication covers the crash window between writes.
- **Honest terminal states (WI#302).** Malformed orchestrator outcomes fail
  loud and count toward rollback; token/time/cost exhaustion returns
  `BudgetExhausted` without fresh-budget retries; stopped engine threads
  persist `Failed` and legacy workers `Cancelled`, including stops during
  planning. Partial-progress completion phrases no longer pass as success.
- **Distinct model-failure guidance (WI#297).** New non-recoverable failure
  kinds for content-filter refusals and output-budget truncation tell the
  user what actually happened and what to do next, and usage from
  intercepted empty streams is folded into thread accounting and telemetry
  instead of vanishing.
- **Full context-overflow recovery at default config (WI#298).** The
  context-shrink ladder (0.60x, then 0.35x) is no longer clamped to a single
  rung by the generic recovery-attempt limit; it stays bounded by per-kind
  and lifetime caps.
- **Missions you can discover, resume, and trust (WI#295, WI#309, WI#311).**
  `tool_info` answers truthfully for mission capabilities with lease-aware
  `callable`; restart resume re-grants mission action leases from the
  canonical definitions; and non-manual missions no longer auto-complete on
  a run's goal-achieved declaration — parked recurring missions recover via
  owner-authorized `mission_resume`.
- **Actionable memory-write rejections (WI#290).** Injection rejections
  carry matched-pattern descriptions, rephrase guidance, and a narrowly
  scoped inline-quote route; the API answers 422/409 instead of 500.
- **Real lightweight-routine budgets (WI#291).** `max_tool_rounds: N` buys N
  tool rounds plus one text-only reply; the decorative nominal limit of 20
  is replaced by a single hard limit of 10, and new multi-tenant tenants get
  `ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS=6`.
- **Scrubbed tenant logs (WI#287).** Read-only `/tenant-logs` serves the
  daemon's own secret-scrubbed log copy, gated by `SANDBOX_TENANT_LOGS`.
- **Non-bypassable secrets deletion approval (WI#307).** Per-call approval
  for secret deletion cannot be pre-granted; migration V28 revokes stale
  `always_allow` delete grants on both database backends.

## Compatibility

| Area | v2.1.5.0 |
|---|---|
| Release / owned packages | Release `2.1.5.0`; all eleven LunarWing-owned release packages use `2.1.5` |
| Rust edition / MSRV | Rust 2024 / `1.96.1`, unchanged |
| SQL migrations | `V28__revoke_secret_delete_always_grants.sql` added (secrets grant cleanup, both backends); V28 is latest |
| Multi-tenant port registry | v17, unchanged |
| WASM runtime | Wasmtime/WASI 36.0.17; matching Cranelift family 0.123.17, unchanged |
| Channel registry | darkirc 0.1.11, multica 0.1.14, weechat 0.2.12, xmpp 0.1.17 |
| Tool registry | cavepony-tool 0.4.5, gitlab 0.1.8, gotify 0.1.2, llm-context 0.1.3, multica-bridge 0.1.4, ssh 0.1.3, vision-analyze 0.1.5, voice-transcribe 0.1.3, web-search 0.2.4 |
| External workers | opencode2 `v2.0.23` (bumped from v2.0.21, WI#303); kimi-code `2.1.1`, opencode `v1.18.25`, and Pebble `2d0f3786…`, unchanged from v2.1.4.0 |
| Bundled skills | Seven product skills, unchanged; no skill migration |

Registry version bumps require rebuilding/reinstalling the affected WASM
extensions. The channel version bumps themselves do not introduce protocol
changes. The V28 migration runs automatically at startup on PostgreSQL and
libSQL; see Upgrade Notes for its downgrade implication.

## Changes

### Engine V2 Boot Recovery and Stop Semantics (WI#301, WI#302)

Global boot recovery now enumerates orphan-project threads alongside
project-owned ones, so a mission or chat thread whose `project.json` is
missing is no longer abandoned at restart. `Waiting` approvals and
resumable checkpoints are preserved across the restart boundary, and
mission fires whose scheduled moment passed while the daemon was down are
recorded and surfaced to the owner as restart-lost — **they are not
automatically replayed**, since their side effects may no longer be
appropriate.

History is persisted before acknowledgement, and thread-ID deduplication
handles a crash between the two writes, so an acked message cannot be
processed twice and an unacked one cannot be lost. Retry recovery retains
its markers during backoff, deduplicates timer admission, and refuses older
markers after a newer fire.

Malformed orchestrator outcomes now fail loud and count toward rollback
accounting instead of being absorbed as soft failures. Token, time, and
cost exhaustion return `BudgetExhausted` without burning fresh-budget
retries. Stopped engine threads persist `Failed` and legacy workers
`Cancelled` — including stops issued during planning — and partial-progress
completion phrases are rejected without rejecting ordinary completion
detail. Three consecutive unconfirmed text-only responses now terminate the
turn as failure rather than fabricating success or burning the whole
iteration budget.

### Pending-Gate Persistence Hardening (WI#299, WI#300)

The pending-gate store (`pending-gates.json`) now survives its own failure
modes. An unparseable live file is quarantined to
`pending-gates.json.corrupt-<unix-millis>` with a warn log naming both
paths, and the store falls back to empty state so new saves proceed; boot
reconciliation then fails the orphaned `Waiting` threads — the designed
degraded path rather than a wedged daemon.

Temp and lock files are created with mode `0600` on unix (the state file
carries unredacted tool parameters), so atomic rename no longer resets an
operator's chmod to the umask default. Temp files use unique
`tmp-<pid>-<nanos>-<seq>` names with `create_new(true)`, so a stranded temp
or pre-planted symlink is never opened or truncated. Saves replace the live
inode atomically; a reader holding the old fd keeps the last complete state.

Separately, the watcher's `GatePaused` arm now discards any pending gate
armed for the thread before finalizing an expired turn, closing a
re-arm-and-expire race that could leave a stale gate behind.

### Honest Model-Failure Terminals and Empty-Stream Usage (WI#297)

Engine V2 gains two non-recoverable failure kinds with their own terminal
guidance: `ContentFiltered` (the provider refused; rephrase or switch
models) and `OutputBudgetExhausted` (the model spent its entire output
budget; raise the output token limit). The bridge maps
`EmptyStream{ContentFilter|Length}` to these instead of a generic rejected
request.

`LlmError::EmptyStream` now carries the token usage intercepted from the
`Done`-first chunk. Retry and failover sum discarded empty-attempt usage and
forward it on the `Retried` stream observation; the engine stream
accumulator folds it into the call's recorded usage, so thread token
accounting sees every billed call — including the retry-storm case where
eleven hidden empty calls previously went uncounted. On exhaustion the sum
is folded into the returned error, and new per-provider
`empty_stream_discarded_{input,output}_tokens_total` telemetry counters
record each forwarding point.

The transient-empty smart-routing escalation now runs on every cheap-routed
arm (Simple, and Moderate with or without cascade), so repeated cheap
empties can no longer poison failover leg 0's cooldown and bypass the
primary for all traffic. The raw upstream finish string embedded in
operator detail is capped at 64 characters and stripped of control
characters.

### Context-Overflow Recovery Ladder (WI#298)

`context_overflow` recovery is now exempt from the per-streak
`model_recovery_max_attempts` clamp, so the policy's full shrink ladder —
0.60x, then 0.35x — fires at the compiled default of 1 (previously only the
first rung was reachable). The exemption is deliberate: context-overflow
recovery changes its inputs rather than retrying blindly, so the
restart-storm rationale behind the clamp does not apply. It remains bounded
by the per-kind policy cap (2) and the lifetime
`model_recovery_total_attempts` cap (default 4,
`ENGINE_V2_MODEL_RECOVERY_TOTAL_ATTEMPTS`), and
`ENGINE_V2_MODEL_RECOVERY_ATTEMPTS=0` still disables engine recovery
entirely.

### Mission Capability Discovery and Resume Leases (WI#295, WI#309)

The eight mission `ActionDef`s now live beside `handle_mission_call` as
`mission_action_defs()`, a single source the router registers the
`missions` capability from — lease planning and discovery can no longer
drift apart. The effect bridge answers `tool_info` for a mission function
at the execution point with name, description, parameters, and
`kind: engine_capability` plus a lease-aware `callable` flag: a thread
holding the lease gets `callable: true`; an unleased thread gets `false`
with a refusal note; threads the host holds no leases for (such as
`rlm_query` children) are answered truthfully rather than with a false
"not callable". Positional `tool_info("x")` and hyphen/underscore aliases
are accepted.

On restart resume, the bridge now includes the canonical mission action
definitions in the unscoped resume lease catalog when its mission manager
is wired. Threads resuming from `Waiting` or `Suspended` with stale
persisted IDs and an empty in-memory lease manager re-acquire their mission
grants through the existing thread-type planner, persistence, and rollback
path; execution prompts remain registry-only.

### Mission Completion Requires Evidence (WI#311)

Non-manual missions no longer auto-complete from a run's
goal-achieved/mission-complete declaration; the completion prompt now asks
about run success, and status and owner checks remain authoritative.
Existing parked recurring missions can be recovered with owner-authorized
`mission_resume`; automatic revival was deliberately rejected because it
could unexpectedly resume tools and notifications after the owner accepted
a completion report. Global bootstrap warns and sends best-effort owner
guidance, and the recovery verb explicitly names completed and failed
missions. **No bulk migration is performed** — see Upgrade Notes.

### Routine/Mission Registry Split (WI#278)

In Engine V2 threads, the eight v1 `routine_*` tools now error honestly
toward their `mission_*` equivalents with concrete call shapes, instead of
silently querying the wrong registry. The redirect is split by capability:
read-only `routine_history` and `routine_list` redirect to
`mission_history(name)`/`mission_list()` in every thread type, while the
six mutating verbs redirect in Foreground threads only. Hyphenated aliases
(`routine-delete`) take the same gate. `routine_update`'s redirect also
cites the host command
`lunarwing routines edit --name <routine> --max-tool-rounds <n>` for the
WI#291 budget surface.

v1 `routine_list` appends a read-only `engine_missions` section that unions
the engine default project's own+shared missions with the caller's own
project's own+shared missions (deduplicated by mission id), so the shared
learning missions are listed for non-owner users too, and the listing
agrees with `routine_history`'s global visible scan. Private missions of
other users remain hidden.

### Lightweight Routine Tool Budgets (WI#291)

`max_tool_rounds: N` now buys N tool-calling rounds plus one final
text-only reply. It used to be N−1, so `1` now allows one tool round and a
default routine can make up to four model calls per run instead of three.
`ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS` and every routine's value share one
hard limit of 10 — the nominal 20 was silently capped at 5 by two hidden
clamps; rows stored above 10 run with 10. New multi-tenant tenants get
`ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS=6`, a ceiling that does not lift a
routine's own value (the per-routine default stays 3).

One routine is raised with `routine_update` from legacy-engine (v1) chats,
or on the host with
`lunarwing routines edit --name <routine> --max-tool-rounds <n>`. A
provider-recovery retry no longer costs a round, and the limit message
names the knob(s) that bounded the run — including the tie case where both
the routine's value and the env cap must rise together.

### rlm_query Failure Redaction (WI#296)

Both model-visible exits of the `rlm_query` child loop are now built from
typed failure kinds only. Provider error text and free-form orchestrator
diagnostics no longer reach the model-visible child response; the raw text
is logged host-side at debug. Every `ThreadFailureKind` renders as its kind
plus a fixed recovery instruction.

### Actionable Memory-Write Rejections (WI#290)

`memory_write` no longer folds injection rejections into a retry-inviting
"not authorized" error. The new `ContentRejected` error carries the matched
pattern descriptions with line numbers (capped, with a count, so the remedy
survives the tool-output cap), states plainly that it is not a permission
problem and will reject again on every retry, and offers rephrase guidance
plus the daily-log-only inline-quote route (one backtick pair, single line,
at most 200 bytes) with the caveat that quoting neutralizes nothing — the
span is stored verbatim and stays model-visible.

A clean append refused only because already-stored content trips the gate
is classified separately, with escalate-first repair guidance. The
quarantine-on-reject storage-failure fallback now routes through the same
classifier, so a clean entry is never blamed for stored poison. The web API
answers **422** for injection rejections and **409** for quarantine and
existing-content cases instead of 500. Gate strictness is unchanged — the
detector itself is untouched.

### Safety Sanitizer Detection Contract (WI#289)

The detect-versus-neutralize decision is now recorded and pinned: detection
stays position-free and deliberately stricter than the line-start-only
neutralization — strict at ingress, best-effort at render. The rationale
lives in the `lunarwing_safety` sanitizer module docs, the workspace
README, and the safety rules, with parity tests across the SOUL.md,
MEMORY.md, daily-log, and lorebook prompt-file classes.

Detection now uses overlapping-match iteration with (pattern, start)
de-duplication, closing an evasion where an exact `assistant:` match at
Medium severity swallowed a High-severity one. NUL bytes are removed before
token escapes, `[INST]`/`[/INST]` escaping is ASCII case-insensitive, and
NUL-split labels are documented as surfacing only as `null_byte`. The
daily-log quoted-span demotion remains the **only** relaxation; widening
that is a separate gate-loosening decision, not part of this release.

### Scrubbed Tenant Logs (WI#287)

A read-only `/tenant-logs` mount serves the daemon's own secret-scrubbed
copy of its log (`<base>/logs/sandbox/daemon.log`, rotated at 8 MiB),
written by the daemon itself on systemd, OpenRC, and launchd, single-node
and multi-tenant. The mount exists only when `SANDBOX_TENANT_LOGS` is not
`off` and the container runtime maps the sandbox user onto the daemon uid
(docker; podman ≥ 5; podman 4.3+ with uid 1000).

stderr, the SSE stream, and the copy are scrubbed at write time by one
shared LogScrubber fed by a mutable known-value set (gateway token,
secrets-store overlay, runtime env overrides), with URL passwords matched
raw and percent-decoded and SGR escape sequences stripped before scrubbing
so a stray ESC cannot hide a token. The boot banner masks the token
off-TTY. At defaults the copy holds INFO and above;
`SANDBOX_TENANT_LOGS=debug` plus a raised log level exposes per-tool-call
lines. Mount-guard defaults are unchanged and only tightened.

### Secrets Deletion Approval and V28 Grant Cleanup (WI#307)

Secret deletion now requires non-bypassable per-call approval: persisted
`always_allow` rows are ignored at execution time for the deletion path,
and both secrets tools carry autonomous-mode opt-outs. Migration
**V28** revokes stale `always_allow` delete grants for all owners on both
PostgreSQL and libSQL; the libSQL extraction is guarded with `json_valid`
so malformed or unrelated rows survive. Runtime protection does not depend
on the cleanup — see Upgrade Notes for the downgrade implication. Daemon
discovery and engine lease planning now share one denylist, including
SSH/SSH Git.

### Dotenv Runtime Overlay (WI#310)

Runtime reload now imports newly added working-directory/instance dotenv
keys into the thread-safe overlay instead of skipping them.
First-value-wins and process-env precedence are preserved, and existing
runtime overrides survive reload. Wizard database/secret reads, onboarding
detection, status, and doctor share the overlay, so in-process quick
onboarding can continue directly into config resolution without a restart.

### Vendor Refresh Self-Healing (WI#294)

`ic/scripts/refresh-vendor.sh` now checks the WI#292 rig-core patch marker
in all five marker-bearing files before vendoring and again after the
post-vendor restore, naming each file that lacks it, with a whole-crate
scan that warns on marker-bearing files missing from the known list. An
EXIT trap armed around `cargo vendor` restores the committed patched tree
when vendor fails or is interrupted, and stale staging directories are
removed before vendoring. Dirty-guard and version-bump refusals print the
full discard commands and point at the patch documentation. A hermetic
shell harness pins the behavior; the happy path is unchanged.

### Dependency Sweep, Security Audit, and CI

All eleven LunarWing-owned release packages advance from `2.1.4` to
`2.1.5`: the daemon, five workspace crates, the XMPP bridge, and four WASM
channels, with every path-dependency requirement between them. All channel
registry packages (darkirc 0.1.11, multica 0.1.14, weechat 0.2.12, xmpp
0.1.17) and all nine tool registry packages (see Compatibility) advanced
together, and a tracked-manifest/lock audit confirms no project-owned
package or path dependency remains at 2.1.4.

External crates took MSRV-respecting semver-compatible updates across all
24 swept lockfile roots (cc 1.6.0, html-to-markdown-rs 3.16.0, insta
1.49.0, lazy_static 1.5.1, libc 0.2.190, uuid 1.27.0,
wasm-encoder/wasmparser 0.261.0, wast/wat 261.0.0, among others), and
`ic/vendor/` was regenerated (1148 crate directories) with the rig-core
patch restored and re-verified.

The crate-security re-audit (see
[the audit document](docs/ops/v2.1.5.0_CARGO_DENY_AUDIT.md)) audited the
full `--workspace --all-features` graph — a superset of every CI compile
graph — plus all 26 satellite manifest roots, with the advisory database at
`ef6173c` (2026-10-03). The honest posture: **8 vulnerability advisories
and 1 informational notice are accepted and shipped**, each with recorded
exposure (rustls-webpki/h2 via the legacy libsql chain, rsa via russh,
failure via the vendored libsignal OMEMO wrapper, and the informational
unmaintained rental). No unaccounted advisory exists in any committed
lockfile. New CI gates enforce this: the `satellite-advisory-sweep`
full-validation job and a `lockfile-only-advisory-guard` that fails on
stale entries and queries the same widened graph. The yanked-crate gate is
verifiably enforced with no index-failure masking.

The opencode2 worker bumps from v2.0.21 to **v2.0.23** (WI#303; upstream tag
SHA `0fd7e2829449b052abf0078666669302923d77af` verified live). The bump also
corrects the executor's canonical-output and permission-reply call paths for
the v2 promise client (WI#320 finding F7: `session.message.list` and
`session.permission.reply` never existed — both are top-level client groups),
and the image build now asserts every method path the bridge calls. The
correction is source-verified (executor self-test plus construction of the
pinned client); runtime re-verification against a live v2.0.23 server awaits
the candidate image build on the first protected pipeline (see
`docs/ops/WORKER-CONTAINERS.md`). Other worker pins are unchanged from
v2.1.4.0.

### Documentation and Repository Maintenance

All 32 active guides were re-validated against the v2.1.5.0 tree and carry
a 2026-10-04 revalidation stamp; contributor onboarding's freeze notice now
names the work items still open at release time. Operator references, the
testing guide, release indexes, and the CI README were synchronized with
the cargo-deny gate changes, the WI#299 quarantine behavior, and the WI#291
budget semantics. An audit of all 25 open tracker items with per-item
repository disposition lives at
[docs/ops/v2.1.5.0_OPEN_WORK_ITEMS_STATUS.md](docs/ops/v2.1.5.0_OPEN_WORK_ITEMS_STATUS.md).
Stale versioned operations documents moved to `docs/ops/history/` with
inbound links repaired and a per-file archive index.

## Upgrade Notes

1. **Back up state.** Preserve PostgreSQL, tenant environment files,
   `/etc/lunarwing/ports.json`, workspace state, rendered units, worker
   data, voice models, and channel state. The port registry stays at v17;
   SQL advances to V28 (see below).
2. **Heed the V28 downgrade implication.** `V28__revoke_secret_delete_always_grants.sql`
   revokes stale `always_allow` secret-deletion grants on both backends and
   **its cleanup is irreversible on downgrade** — downgrading to v2.1.4.0
   does not restore the revoked rows. Runtime protection in v2.1.5.0 does
   not depend on the cleanup (the rows are ignored at execution), and
   generic settings writes can still create inert grant rows on older
   versions.
3. **Deploy rebuilt components together.** Rebuild the daemon, XMPP bridge,
   and installed WASM channels and tools; rebuild/reinstall all four
   channel and nine tool registry updates through the normal deployment
   path.
4. **Worker images.** opencode2 bumps to v2.0.23 (WI#303) — promote the
   rebuilt image through the normal `worker-opencode2-candidate` → scan →
   promotion path before tenants use it; kimi-code 2.1.1, opencode v1.18.25,
   and Pebble `2d0f3786…` are unchanged and need no rebuild unless your pins
   drifted.
5. **Choose tenant settings.** New multi-tenant tenants render
   `ROUTINES_LIGHTWEIGHT_MAX_ITERATIONS=6` (a ceiling, not a default budget)
   and `SANDBOX_TENANT_LOGS=info`; both are preserved on re-render and not
   added to existing tenants by upgrade paths. Set `SANDBOX_TENANT_LOGS=off`
   to disable the `/tenant-logs` mount, or `debug` with a raised log level
   for per-tool-call lines.
6. **Apply the updated tool contracts.** In Engine V2 threads use
   `mission_*` tools (the v1 `routine_*` tools redirect with call shapes);
   use `mission_list`/`mission_history` for listings. Recover parked
   recurring missions with owner-authorized `mission_resume` — none are
   revived automatically. Expect `memory_write` injection rejections to
   name the matched patterns; rephrase, or use the single-line ≤200-byte
   inline-quote route for daily logs. Raise a routine's budget with
   `routine_update` (v1 chats) or
   `lunarwing routines edit --name <routine> --max-tool-rounds <n>` on the
   host. Note that `max_tool_rounds: N` now buys N tool rounds plus one
   text-only reply, so existing routines gain one effective model call per
   run.
7. **Expect honest failures after interrupted restarts.** Boot recovery now
   fails `Waiting` threads orphaned by a quarantined pending-gates file
   (inspect `pending-gates.json.corrupt-<ts>`), records restart-lost mission
   fires for owner review instead of replaying them, and persists stopped
   threads as `Failed`/`Cancelled` rather than leaving them limbo.

### Security, Deployment, and Release Limitations

**Carried known issues.** The daily-log gate follow-up work (WI#288) and
the mission graceful-shutdown retry seam (WI#276) remain open with merge
requests in flight and are not in this release. The opencode2 executor
correction shipped with the v2.0.23 bump (WI#303) is source-verified but not
yet runtime-verified against a live v2.0.23 server — a green
`worker-opencode2-candidate` build and a smoke test on the first protected
pipeline are the remaining evidence before promotion (see
`docs/ops/WORKER-CONTAINERS.md`). See
[the open-items audit](docs/ops/v2.1.5.0_OPEN_WORK_ITEMS_STATUS.md) — a
point-in-time snapshot dated 2026-10-04, with the #303 entry updated
2026-10-06 — for the disposition table, including items deferred to v2.1.6.0
and v2.1.8.0.

**Accepted advisories.** Eight vulnerability advisories and one
informational notice ship with recorded exposure; none is unaccounted for
in any committed lockfile. The audit does not cover the Python requirements
files, the kimicode4lunarwing npm lockfiles, or the digest-pinned container
images (Trivy-scanned separately) — see the audit document's scope section.

**Engine behavior.** Engine V2 consumes system-prompt edits as
initialization snapshots; activation still requires a tenant restart and a
new conversation. `tool_info`'s describe path does not surface the
routine→mission redirect (the execution-time error does). Existing parked
recurring missions require explicit owner-authorized `mission_resume`; no
bulk migration is performed by design.

**Tenant logs.** `/tenant-logs` is unavailable when the container runtime
cannot map the sandbox user onto the daemon uid; the log copy holds INFO
and above by default. `GATEWAY_USER_TOKENS` (a JSON map) is registered for
scrubbing as one value, so its individual tokens are caught by shape only.
`println!`/panic output is documented as unscrubbed.

**Deployment boundaries.** The onboarding web app remains a trusted local
console without user accounts or TLS: retain loopback binding and use an
SSH tunnel for remote access. `ALLOW_PRIVATE_IPS=1` disables public-address
SSRF restrictions and is only for intentionally trusted private-network
deployments. MCP does not automatically install npm/pip packages, and
external workers do not automatically inherit core MCP configuration.
OpenRC hosts without usable cgroup v2 delegation use nice-only build
scheduling. Kawarimi remains PostgreSQL-only. DarkIRC Secure Key Exchange
Stages B–D and automated QR/web/trusted-channel delivery remain disabled.

---

> The workshop door opens,  
> brushes wait where rust once slept —  
> color finds the page.

> What revival keeps  
> is not the past, but the hand  
> that dares to begin.

文芸復興
