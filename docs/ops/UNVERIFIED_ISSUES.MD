# v2.0.4.0 Branch Review Issues

**Review date:** 2026-07-30
**Reviewed implementation range:** `961521a..a80c4f4` (39 branch-only commits at review time)
**Initial report commit:** `271d2b8`
**Overall status:** Not release-ready (at review time)

**Verification pass (2026-07-31, branch aj/kimi-2-20260731-0001):** all 14 issues independently verified against the code; every one was confirmed real. ISSUE-001 through ISSUE-012 and ISSUE-014 are fixed in the commits noted per issue below; ISSUE-013 is addressed via the doc-accuracy path the recommended fix allows (shared backend deferred). Each fix carries regression tests; suites: hooks 83/83, coordination+gate+coalesce green, xmpp 76/76, agent 459/459, health-systemd 34/34, health-openrc 43/43.

**Residual-fix verification (2026-07-31, branch `lotofissueswithorch`, head `d0d0a04`):** six follow-up fixes verified with passing suites. (1) Coalesce delivery-barrier read guard is now moved INTO the detached flush task — previously dropped at `spawn_flush` return, letting `flush_all` complete mid-delivery (`test_flush_all_waits_for_early_flush_callback` was failing). (2) `bootstrap_hooks` clippy signature (`too_many_arguments`). (3) AlwaysAllow now persists only when the approved action genuinely succeeded — tool-level failures arrive as `Ok(is_error: true)` and no longer earn auto-approval. (4) Gate expiry/orphan finalization is routed through `ThreadManager::finalize_waiting_thread_as_failed` + `ConversationManager::untrack_thread_everywhere` — previously direct store writes leaked no-TTL leases and stale conversation tracking. (5) The approval resolving window (inline action execution between gate removal and resume) is interrupt-visible and cancellable via a per-thread `CancellationToken`; regression test `interrupt_cancels_resolving_window_and_finalizes_waiting_thread`. (6) `engine_v2_channel_delivery_matrix` transform-reject case fixed — test-side race: it waited for thread `Done`, which the finisher sets BEFORE the handler runs the transform hook; it now waits for the hook itself. Suites this pass: `lunarwing_engine` lib 435/435; root focused suites 42/42 (coordination_ 7, mission_notification_ 3, coalesce 19, interrupt_ 6, expired_gate 5, resolving_window 1, coordination_gate 1); `engine_v2_channel_delivery_matrix` 1/1 (`--features="libsql integration"`); health-systemd 49/49; health-openrc 43/43; mt-admin shell harnesses pass; `cargo check --all-features`, `cargo clippy --all --benches --tests --examples --all-features -- -D warnings`, `cargo fmt --all -- --check` all clean.

This report covers coordination core, web/XMPP integration, init and health
changes, dependency/version updates, merge integrity, and CI coverage. The
Kawarimi migration was excluded at operator request because its live migration
test had already completed successfully. No conclusion in this report applies
to Kawarimi.

Compilation and linting are clean. Findings below concern runtime integration,
state transitions, concurrency, service lifecycle, security boundaries, and
test coverage that compile-time checks do not exercise.

## Issue Index

| ID | Severity | Summary |
|----|----------|---------|
| ISSUE-001 | High | Coordination hooks receive transport name instead of room identity |
| ISSUE-002 | High | XMPP coalescing is absent from the deployed WASM channel path |
| ISSUE-003 | High | Three-strike disable gate is not registered in production |
| ISSUE-004 | High | Violation accounting can strike the wrong agent or count twice |
| ISSUE-005 | High | Coalesce buffer has lost-update and stale-timer races |
| ISSUE-006 | High | `disable-weechat` does not control service lifecycle |
| ISSUE-007 | High | Multi-tenant systemd health can fail open |
| ISSUE-008 | Medium | Expired automatic disable cannot rearm until strikes decay |
| ISSUE-009 | Medium | Coordination admin mutations have no authentication |
| ISSUE-010 | Medium | Declarative coordination rules apply to every channel |
| ISSUE-011 | Medium | A response containing only an allowed tag can panic |
| ISSUE-012 | Medium | Registry updates can race and health can use a stale registry |
| ISSUE-013 | Medium | First-responder claims are process-local |
| ISSUE-014 | Low | Violation-store tests share temporary paths |

## Release Blockers

### ISSUE-001: Coordination hooks receive transport name instead of room identity

**Severity:** High
**Status:** Confirmed real — FIXED in `52ce1bd`
**Affected code:** `ic/src/agent/thread_ops.rs:525-532`,
`ic/src/agent/agent_loop.rs:1179-1185`,
`ic/src/channels/wasm/wrapper.rs:2653-2665`,
`ic/src/hooks/coordination.rs:192-199`

#### Observed behavior

`ResponseTransform`, `BeforeInbound`, and `BeforeOutbound` hook events use
`IncomingMessage.channel`. For an installable XMPP channel, the WASM wrapper
sets this field from `dispatch.channel_name`, which is normally the adapter name
`"xmpp"`. The actual room or direct-message target remains in metadata such as
`xmpp_room` and `xmpp_target`.

`CoordinationState::is_coordination_channel` compares the hook event's channel
string against `COORD_CHANNELS`. Deployment documentation instructs operators
to populate `COORD_CHANNELS` with room JIDs or channel names, for example
`coord@conference.example`.

#### Failure modes

1. With `COORD_CHANNELS=coord@conference.example`, hook events carry `"xmpp"`, so secret detection, engagement policy, first-responder claims, tag enforcement, rate limiting, and violation checks do not run.
2. With `COORD_CHANNELS=xmpp`, hooks run for every XMPP room and direct message, not only the intended coordination room.
3. Native XMPP tests use room-shaped synthetic hook values and therefore do not reproduce the deployed WASM path.

#### Impact

Documented configuration silently disables most coordination policy. The only
configuration that activates it broadens enforcement to unrelated XMPP
conversations. This can either leave coordination rooms unprotected or reject
normal multiline and untagged messages in unrelated rooms and DMs.

#### Recommended fix

Preserve transport name and destination identity as separate values. Hook
events should carry a canonical policy scope derived from routing metadata,
preferably through the existing channel routing helper, while retaining
`message.channel` for adapter dispatch. Coordination hooks should match only
the canonical destination.

#### Required validation

- Feed a real WASM-emitted XMPP message with `xmpp_room` metadata through inbound, transform, and outbound hooks.
- Verify configured room receives all coordination policies.
- Verify a second room and an XMPP DM remain unaffected.
- Verify native and WASM XMPP paths produce the same policy scope.

### ISSUE-002: XMPP coalescing is not present in the deployed channel path

**Severity:** High
**Status:** Confirmed real — initial transport-boundary fix in `c995575`;
completed by the 2026-07-31 follow-up with transport-key isolation and a
shutdown callback barrier
**Affected code:** `ic/src/main.rs:379-384`, `ic/src/main.rs:523-554`,
`ic/src/channels/xmpp/mod.rs:1145-1192`,
`docs/proposals/MULTIAGENTTEAMORCHESTRATION/DEPLOYMENT.md:74-87`,
`docs/proposals/MULTIAGENTTEAMORCHESTRATION/DEPLOYMENT.md:123-129`

#### Observed behavior

Startup migrates native XMPP configuration to the installable channel and then
loads WASM channels. The coalesce buffer is instantiated and called only by the
native Rust `XmppChannel`. Current production source contains no non-test
construction of that native channel.

The deployment document says all four phases are implemented and describes
coalescing as wired into XMPP `respond()`, but its deferred-work section also
states that WASM-channel coalescing is skipped. The latter statement matches
the runtime path.

#### Impact

Rapid terminal responses in deployed XMPP rooms are not merged, despite the
feature being marked complete. Expected message-volume reduction, duplicate
suppression, overflow behavior, and shutdown flushing are absent in production.

#### Recommended fix

Put coalescing at a transport-independent outbound boundary used by both native
and installable channels, or add equivalent buffering to the WASM channel host
wrapper. Run coordination hooks before buffering and the transport rate limiter
after a flush. Preserve destination, conversation scope, attachments, and
shutdown ownership in the buffer key and callback.

Until that lands, mark Phase 4 partial rather than implemented.

#### Required validation

- Exercise actual installable XMPP response delivery, not `XmppChannel` directly.
- Verify two rapid same-room responses merge and different rooms never merge.
- Verify `ALERT:` bypass, max-merge flush, overflow split, and shutdown flush.
- Verify each flushed message still passes the active transport rate limiter.

### ISSUE-003: Three-strike disable gate is not registered in production

**Severity:** High
**Status:** Confirmed real — initial gate wiring in `32cb470`; completed by the
2026-07-31 follow-up with typed terminal denial across Engine V2 executors
**Affected code:** `ic/src/gate/coordination.rs:17-83`,
`ic/src/main.rs:656-689`

#### Observed behavior

`CoordinationGate` implements `ExecutionGate` and can deny tool execution for a
disabled agent. Production startup creates `ViolationStore`, registers hooks,
and starts the admin server, but never constructs or installs
`CoordinationGate`. `is_silent_deny` also has no production caller outside its
module.

The deployment document acknowledges that gate pipeline assembly is deferred,
while the release checklist describes three-strike auto-disable as an
implemented phase.

#### Impact

Strike three updates status and may block a final outbound response when hooks
match, but it does not stop the disabled agent from processing future input or
executing tools. Manual disable through the admin endpoint has the same gap.
This violates the gate's documented purpose: "blocks all tool execution."

#### Recommended fix

Construct `CoordinationGate` with the same `Arc<ViolationStore>` used by hooks
and register it in the active Engine V2 execution-gate pipeline at priority 5.
Gate callers must recognize `SILENT_DENY_PREFIX`, log the denial without
returning policy text to the channel, and stop execution before any tool side
effect.

#### Required validation

- Disable the configured local agent, submit a tool-capable request, and prove no tool starts.
- Repeat with manual disable and automatic strike-three disable.
- Verify ordinary clean agents still execute.
- Verify silent denial emits no channel response and no synthetic policy response.

### ISSUE-004: Violation accounting can strike the wrong agent or count one event twice

**Severity:** High
**Status:** Confirmed real — initial accounting fix in `52ce1bd`; completed by
the 2026-07-31 follow-up with Engine V2 transform parity and typed transform
suppression
**Affected code:** `ic/src/agent/agent_loop.rs:1179-1185`,
`ic/src/agent/agent_loop.rs:1456-1478`,
`ic/src/agent/thread_ops.rs:525-545`,
`ic/src/hooks/coordination.rs:325-340`,
`ic/src/hooks/coordination.rs:623-657`

#### Observed behavior

Three independent flows corrupt strike accounting:

1. An inbound hook rejection returns text such as `[Message rejected: ...]` as a normal agent result. That synthetic text enters outbound hooks and lacks a required coordination tag, so the local agent can receive a strike for rejecting someone else's inbound message.
2. `TagSchemaHook` records a strike during `ResponseTransform`. `thread_ops` replaces the rejected response with untagged text such as `[Response filtered: ...]`. The replacement then reaches `BeforeOutbound`, where the same hook records another missing-tag strike.
3. `deliver_outbound_with_hooks` copies `message.user_id` from the inbound message into the outbound event. `SecretEntropyHook` records that event user, so an outbound secret can be attributed to the remote sender or owner scope rather than the local emitting agent.

#### Impact

The three-strike counter is not a reliable measure of agent behavior. Unrelated
room traffic can penalize an innocent agent, one invalid response can consume
two strikes, and a genuine outbound leak can penalize the wrong identity. This
can disable an agent after fewer than three agent violations while leaving the
actual emitter unblocked.

#### Recommended fix

Use a typed suppression outcome for rejected inbound and transformed responses
instead of manufacturing channel text. Carry explicit actor identity in hook
events: inbound sender, local emitting agent, and owner/persistence scope must
not share one ambiguous `user_id`. Run schema enforcement at one outbound stage
or mark already-evaluated responses so the same policy cannot record twice.

#### Required validation

- Reject an untargeted inbound room message and assert no local-agent strike.
- Submit one malformed local response and assert exactly one strike.
- Emit an outbound high-entropy token and assert the configured local agent is struck.
- Assert rejected content produces no synthetic response that re-enters hooks.

### ISSUE-005: Coalesce buffer has lost-update and stale-timer races

**Severity:** High
**Status:** Confirmed real — FIXED in `0d4228c`
**Affected code:** `ic/src/channels/coalesce.rs:163-237`

#### Observed behavior

`submit` releases the pending-map lock after observing no entry, then calls
`submit_new`, which reacquires the lock and unconditionally inserts. Two
concurrent first submissions can both observe absence. The later insertion
replaces the earlier pending message rather than merging it.

Generation values are derived only from the currently pending entry. After an
early flush removes an entry, a replacement starts again at generation zero.
An older generation-zero timer can then wake and remove the newer entry.

#### Impact

Valid outbound updates can disappear without an error, flush under the wrong
window, or be delivered in an unexpected order. The race is most likely during
rapid multi-task responses, which is exactly the load the coalescer is intended
to handle.

#### Recommended fix

Perform lookup, merge, overflow handling, and new insertion atomically under one
map lock using an entry-style operation. Keep timer identity in a monotonic
per-key sequence that survives entry removal, or use a cancellation token owned
by each pending entry. A stale timer must never match a replacement entry.

#### Required validation

- Use a barrier to submit two first messages concurrently and assert neither is lost.
- Early-flush a key, immediately reinsert, advance paused time, and assert the old timer cannot remove the new entry.
- Stress multiple keys to confirm atomicity does not serialize flush callbacks under the map lock.

### ISSUE-006: `disable-weechat` changes health classification but not service lifecycle

**Severity:** High
**Status:** Confirmed real — initial lifecycle gating in `346c9f1`; completed by
the ISSUE-006 follow-up on `aj/kimi-2-20260731-0001` after independent review
**Affected code:** `ic/scripts/lunarwing-mt-admin.sh:1554-1583`,
`ic/scripts/lunarwing-mt-admin.sh:5941-5965`,
`ic/scripts/lunarwing-mt-admin.sh:6012-6026`,
`ic/scripts/lunarwing-mt-admin.sh:6182-6200`,
`ic/scripts/lunarwing-mt-admin.sh:6890-6937`

#### Observed behavior

The command records `enable_weechat=false`, but its own comment confirms that it
only changes health and self-heal classification. It does not stop, disable, or
remove WeeChat units.

On systemd, units are always rendered with `Restart=on-failure`, and
`start_tenant_systemd` always includes backend and adapter units in its enable
list. A currently failing unit keeps its systemd restart loop; a disabled tenant
is enabled again for future boots. On OpenRC, `start_tenant_openrc` always tries
both services and adds running services to the default runlevel.

#### Impact

Health may report an intentionally disabled service as skipped while the init
system continues spending CPU, logs, and restart budget on it. Restarting a
tenant or host can reactivate WeeChat despite the registry saying it is
disabled. Command naming and release notes imply stronger lifecycle behavior
than implemented.

#### Recommended fix

Make `enable_weechat` authoritative for rendering, dependencies, enablement,
startup, and health. `disable-weechat` should stop both units and disable their
systemd links or remove their OpenRC runlevel entries. `enable-weechat` should
render/reconcile, enable, and start them explicitly. Preserve idempotency on
both init systems.

#### Required validation

- Disable a crash-looping systemd tenant and prove restart activity stops.
- Disable an OpenRC tenant and prove `start-tenant` and reboot intent do not start WeeChat.
- Re-enable on both systems and prove units are rendered, enabled, and started.
- Test existing tenants, missing units, repeated enable/disable, and adapter dependency cleanup.

### ISSUE-007: Multi-tenant systemd health can report healthy when discovery fails

**Severity:** High
**Status:** Confirmed real — initial discovery checks in `9d372f5`; completed by
the 2026-07-31 follow-up with per-tenant user, bus, enumeration, and loaded-unit
invariants
**Affected code:** `ic-infrastructure-health-check/health-systemd.sh:23-27`,
`ic-infrastructure-health-check/health-systemd.sh:48-55`,
`ic-infrastructure-health-check/health-systemd.sh:93-104`,
`ic-infrastructure-health-check/health-systemd.sh:114-142`,
`ic-infrastructure-health-check/health-systemd.sh:214-216`

#### Observed behavior

Any readable tenant-registry path sets `IS_MT=true`, even when JSON is invalid.
That suppresses missing single-node base units. Tenant discovery runs only when
`jq` exists. Registry parsing uses `|| true`, and tenant user-manager commands
also convert every error to empty output. Empty or failed `show` output is then
skipped because `LoadState` is not `loaded`.

The 24-second deadline can stop the tenant sweep without recording a degraded
or critical issue. No final invariant requires at least one tenant or one unit
to have been observed.

#### Failure modes

- Missing `jq` plus readable registry: base units skipped, tenant block skipped, result healthy.
- Malformed JSON: tenant iterator empty, result healthy.
- Missing `timeout`, denied `sudo -n`, unavailable user bus, or failed `systemctl --user`: probes become empty and units disappear from output.
- Deadline exceeded before useful discovery: partial result can remain healthy.

#### Impact

A host can report healthy while every tenant daemon is down or completely
unobservable. Self-heal receives no failing unit to remediate, making monitoring
failure indistinguishable from service health.

#### Recommended fix

Validate `jq`, registry JSON shape, required commands, tenant users, and user-bus
access before entering multi-tenant mode. Track discovered tenants, successful
buses, and loaded units. Emit a synthetic degraded or critical result when
discovery is incomplete, times out, or observes zero expected units. Preserve
the distinction between an absent unit and a failed probe.

#### Required validation

- Cover missing `jq`, malformed JSON, missing registry keys, denied sudo, missing user, unavailable bus, missing `timeout`, and deadline exhaustion.
- Assert each case is non-healthy and contains a diagnostic issue.
- Verify a deliberately empty but valid registry has an explicit documented status.

## Important Correctness And Security Issues

### ISSUE-008: Expired automatic disable cannot rearm until every strike decays

**Severity:** Medium
**Status:** Confirmed real — FIXED in `0d4228c`
**Affected code:** `ic/src/gate/violation_store.rs:145-171`,
`ic/src/gate/violation_store.rs:238-262`

#### Observed behavior

An automatic disable is created only when `disabled_until_secs.is_none()`.
After the timestamp expires, `status_of` stops reporting disabled but does not
clear the timestamp. `prune_decayed` clears it only when every strike record has
decayed and the agent is not manually disabled.

With the documented defaults, disable lasts 24 hours while strikes last seven
days. During the remaining six days, new strikes cannot create a second disable
window because the expired timestamp is still `Some`.

#### Impact

An agent can repeatedly violate policy after its first disable expires and
remain at `FinalWarning` without being disabled again. Enforcement weakens
precisely for repeat offenders.

#### Recommended fix

Normalize expired state while holding the store lock before adding or evaluating
strikes. Clear an expired automatic timestamp independently of strike decay,
then let the threshold rule create a new window. Decide whether rearming should
occur immediately when old active strikes already meet threshold or only after
a new violation, and encode that policy explicitly.

#### Required validation

- Use an injectable clock rather than sleeping.
- Reach strike three, advance beyond disable duration but not decay duration, record a new violation, and assert a new disable window.
- Verify manual disable remains unaffected.

### ISSUE-009: Coordination admin mutation endpoints have no authentication

**Severity:** Medium
**Status:** Confirmed real (security weakness) — initial route auth in `82b81be`;
completed by the 2026-07-31 follow-up with tenant token provisioning, per-tenant
admin ports, and shared agent-ID validation
**Affected code:** `ic/src/channels/web/handlers/coord.rs:1-12`,
`ic/src/channels/web/handlers/coord.rs:79-136`,
`ic/src/channels/web/handlers/coord.rs:139-159`

#### Observed behavior

The dedicated server binds only to `127.0.0.1`, but every route is available
without credentials. Any local process can clear strikes, disable agents, or
re-enable them. A service vulnerable to server-side request forgery can also
reach loopback. Agent path values have no length or format bound and mutation
creates or persists arbitrary store keys.

Loopback binding limits remote network exposure; it is not an authorization
boundary on a multi-user or multi-service host.

#### Impact

A compromised low-privilege local service can bypass coordination enforcement
or deny service to agents. Unbounded identifiers can grow the persistent JSON
store and logs.

#### Recommended fix

Prefer an authenticated Unix socket with restrictive ownership and mode. If TCP
is required, require a dedicated bearer token loaded through the existing
secret system, compare it safely, and reject missing or malformed credentials.
Validate agent IDs with a conservative length and character policy. Keep the
server loopback-only as defense in depth.

#### Required validation

- Assert all mutation routes reject missing and invalid credentials.
- Assert authorized status and mutation calls still work.
- Test unauthorized local callers, oversized IDs, invalid encodings, and SSRF-shaped requests.

### ISSUE-010: Declarative coordination rules apply to every channel

**Severity:** Medium
**Status:** Confirmed real — initial channel guard in `90405e0`; completed by the
2026-07-31 follow-up with `COORD_ENABLED` gating and exact escaped
`COORD_CHANNELS` scope generation
**Affected code:** `ic/config/hooks/coordination.hook.json:3-68`,
`ic/src/hooks/bundled.rs:439-477`

#### Observed behavior

The shipped JSON rules define hook points and content regexes but no channel
predicate. `RuleHook::execute` extracts primary content and evaluates only the
content guard. Once copied into a workspace as documented, newline rewriting,
single-line rejection, the 420-character limit, and secret-pattern rejection
run on every channel event handled by that workspace.

This bundle is independent of `COORD_ENABLED` and `COORD_CHANNELS`; installing
the file is enough to activate it globally.

#### Impact

Normal CLI, web, XMPP DM, non-coordination room, and other channel responses can
be modified or blocked under rules intended only for pager-style coordination
traffic.

#### Recommended fix

Extend declarative hook schema with a channel or policy-scope predicate, such as
an allowlist or `when_channel_regex`, and evaluate it before content actions.
Alternatively, remove generic formatting rules from the bundle and register
them through coordination-aware Rust hooks that already understand configured
destinations.

#### Required validation

- Load the real workspace bundle and send identical content through configured and unconfigured destinations.
- Assert only configured coordination destinations are modified or rejected.
- Verify behavior with `COORD_ENABLED=false` is explicitly documented and tested.

### ISSUE-011: A response containing only an allowed tag can panic

**Severity:** Medium
**Status:** Confirmed real — FIXED in `0137177`
**Affected code:** `ic/src/hooks/coordination.rs:589-601`

#### Observed behavior

`starts_with_tag` proves that the leading slice equals an allowed tag, then
indexes `trimmed.as_bytes()[tag.len()]` to require a colon. When content is
exactly `"STATUS"`, `"ALERT"`, or another allowed tag, `tag.len()` equals the
byte length and indexing is out of bounds.

#### Impact

Malformed but ordinary model output can panic the hook task instead of producing
a controlled policy rejection. This can abort current response processing;
process-level behavior then depends on the configured panic policy.

#### Recommended fix

Use checked access, for example comparing `trimmed.as_bytes().get(tag.len())` to
`Some(&b':')`, after the case-insensitive prefix comparison. Avoid direct byte
indexing on untrusted response text.

#### Required validation

- Test every exact allowed tag with no colon.
- Test tag plus colon, tag plus space, empty content, shorter content, and non-ASCII prefixes.
- Assert malformed input is rejected without panic.

### ISSUE-012: WeeChat registry updates can race and health can retain an obsolete registry

**Severity:** Medium
**Status:** Confirmed real — FIXED in `346c9f1`
**Affected code:** `ic/scripts/lunarwing-mt-admin.sh:1554-1583`,
`ic/scripts/lunarwing-mt-admin.sh:1615-1619`,
`ic/scripts/lunarwing-mt-admin.sh:7108-7160`

#### Observed behavior

`ports_set_weechat` performs a read-modify-rename without acquiring the ports
registry lock. Other registry mutations, including tenant removal, do use that
lock. Two concurrent writers can each read old state and the later rename can
discard the other update. Assignment also does not revalidate tenant existence
under lock, so a concurrent removal can be followed by recreation of a partial
tenant object.

Health configuration is write-once except for
`LUNARWING_SERVICE_MANAGER`. If `PORTS_REGISTRY` changes after first install,
`SELF_HEAL_TENANTS_FILE` keeps pointing to the old file while admin commands
update the new one.

#### Impact

Concurrent operations can lose allocations or feature flags, resurrect a ghost
tenant, or leave health classification disconnected from the registry modified
by `enable-weechat` and `disable-weechat`.

#### Recommended fix

Acquire the shared ports lock before the initial read, verify the tenant exists,
perform one atomic write, and release through the existing cleanup path. During
health installation, reconcile `SELF_HEAL_TENANTS_FILE` with the active registry
using the same targeted-line update used for service manager, while preserving
file ownership, mode, and unrelated operator settings.

#### Required validation

- Pause two registry writers after read, resume them in opposite order, and assert both updates survive.
- Race tenant removal against WeeChat disable and assert the tenant stays removed.
- Install health with registry A, rerun with registry B, and assert health reads B.

### ISSUE-013: First-responder claims are process-local, not host-wide or team-wide

**Severity:** Medium
**Status:** Confirmed real (limitation) — addressed in `7a5495f`: feature re-documented as process-local; shared compare-and-insert claim backend explicitly deferred (doc-accuracy path per the recommended fix)
**Affected code:** `ic/src/hooks/coordination.rs:155-180`,
`ic/src/hooks/coordination.rs:472-545`,
`ic/src/hooks/coordination.rs:738-763`

#### Observed behavior

Each LunarWing process creates a new `CoordinationState` during hook
registration. Claims live in an in-memory `HashMap` shared only among hooks in
that process. Tests model multiple agents by sharing one state manually, which
does not match separate tenant daemons receiving the same MUC message.

#### Impact

Two agents in separate processes can both observe no claim, both claim locally,
and both respond. The feature does not provide first-responder deduplication for
the normal multi-agent deployment shape. Documentation calls current behavior
"per-host," but no host-shared storage exists.

#### Recommended fix

Use an atomic shared claim backend with TTL semantics: host-local SQLite or a
locked file for same-host tenants, or a broker/database operation for cross-host
teams. Claim must be one compare-and-insert operation keyed by stable message or
correlation ID. If this is deferred, describe the feature as process-local and
remove claims of deployed multi-agent deduplication.

#### Required validation

- Start two independent agent processes against one shared claim backend.
- Deliver the same message concurrently and assert exactly one claim succeeds.
- Verify TTL expiry, process crash recovery, and unrelated correlation IDs.

## Test Reliability

### ISSUE-014: Violation-store tests share temporary paths under parallel execution

**Severity:** Low
**Status:** Confirmed real — FIXED in `0d4228c`
**Affected code:** `ic/src/gate/violation_store.rs:318-368`

#### Observed behavior

`test_store` builds its directory from process ID and second-resolution Unix
time. Tests in one binary share a PID and commonly start within the same second,
so they open the same `violations.json`. A parallel focused run produced one
failure in `test_persistence_roundtrip`; the same test passed alone.

#### Impact

CI can fail nondeterministically, and tests can pass or fail based on records
written by a neighboring test. This weakens confidence in persistence and
escalation coverage.

#### Recommended fix

Give every test an owned `tempfile::TempDir` or UUID/nanosecond-suffixed path.
Keep the temporary directory alive for the full store lifetime and avoid one
shared `.tmp` rename target.

#### Required validation

- Run the violation-store suite repeatedly with six test threads.
- Add a stress invocation that creates stores concurrently and asserts unique paths.

## Merge And Build Validation

- Reviewed branch was 39 commits ahead and 0 behind `origin/master` at `a80c4f4`.
- All surviving merged source refs were ancestors of the reviewed head.
- Re-merge inspection found conflict resolutions only in `docs/ops/AGENT_GOALS_2.0.4.0.md`; no source conflict resolutions were detected.
- Workspace and related LunarWing package versions consistently resolve to `2.0.4`.
- `taskset -c 0-5 cargo check -j6 --locked --all-features`: passed.
- `taskset -c 0-5 cargo clippy -j6 --all --benches --tests --examples --all-features -- -D warnings`: passed.
- `taskset -c 0-5 cargo fmt --all -- --check`: passed.
- `git diff --check`: passed.
- Coordination hook tests passed `20/20`; coalesce tests passed `13/13`; gate tests passed `4/4`.
- Systemd health tests passed `23/23`; OpenRC health tests passed `43/43`.
- Passing health tests cover classification fixtures, not full lifecycle behavior described in ISSUE-006 and discovery failures described in ISSUE-007.
- Satellite WASM lockfiles changed, but the crate audit says those crates were not rebuilt during the audit. CI currently builds XMPP and WeeChat artifacts, not every changed satellite.

## Release Recommendation

Superseded by the 2026-07-31 verification pass: ISSUE-001 through ISSUE-007
(and 008-012, 014) are fixed and tested; ISSUE-013 is documented as
process-local with the shared backend deferred. Remaining pre-release work
from this review: runtime end-to-end coverage that starts the same channel,
hook, gate, init, and health paths used in production — component-only tests
and a clean compile cannot validate these integration boundaries.
