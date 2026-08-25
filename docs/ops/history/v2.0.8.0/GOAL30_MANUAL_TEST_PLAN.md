# Goal 30 (v2.0.8.0): Manual test plan for new features and uncovered changes

Date: 2026-08-22 · Branch: `aj/kimi-30-20260822-2101`

Scope: every v2.0.8.0 (`Sekisho`) feature and behavioral change that automated
tiers (unit, integration, mock harness, CI source verification) do **not**
fully prove, and that therefore needs a manual, live-host pass before release.
The source of truth for "what changed" is `RELEASE-v2.0.8.0.md` (root); this
document turns each entry into executable manual checks with expected results.

## How to use this document

- Run the automated tiers first (see `docs/ops/PRE-RELEASE-TESTING.md` and
  `docs/guides/TESTING_GUIDE.md`). This plan starts where those stop.
- Record the tested commit hash, host init system (systemd / OpenRC), and
  tenant topology (single-node / multi-tenant) next to each section.
- Tick `[ ]` → `[x]` only when the expected result was observed live. Note
  failures inline and file them under `docs/bugs/` before release sign-off.
- Some checks are destructive or need root on a Gentoo/OpenRC host; use a
  sacrificial tenant where stated and follow the backup step in the release
  notes' Upgrade Notes before touching a production host.

Environment legend used below: **SN** = single-node install, **MT** =
multi-tenant host, **ORC** = OpenRC/Gentoo, **SD** = systemd.

---

## 1. Multi-tenant OpenRC/Gentoo cgroup isolation (MT, ORC; root required)

Why manual: the cgroup work touches PAM, `/etc/local.d`, `setpriv`, and the
real cgroup v2 hierarchy — none of it is exercisable by the mock suites.
Release notes claim live verification; re-verify on the upgrade path.

- [ ] **setup-cgroups render**: run `ic/scripts/lunarwing-mt-admin.sh
  setup-cgroups` on a Gentoo MT host. Expected:
  `/etc/local.d/lunarwing-cgroups.start` rendered from the ports registry and
  a `pam_exec` hook wired into `system-login`, `sshd`, `su`, and `login` —
  and **not** into `sudo`.
- [ ] **Boot persistence**: reboot (or run the local.d script manually).
  Expected: pre-delegated `lunarwing-<tenant>` subtrees exist for every
  tenant after boot, with correct ownership/delegation.
- [ ] **Login placement**: open an `su - <tenant>` session. Expected: the
  session process lands in `lunarwing-<t>/session`. A `sudo -u <tenant>`
  session must stay **out** of the tenant subtree (deliberate exclusion).
- [ ] **Bounded build**: run `lunarwing-mt-admin.sh build <tenant>`.
  Expected: build processes migrate into the tenant's `build` cgroup via the
  root-side `setpriv` rendezvous; `memory.high` throttling is observable
  under load; a deliberate memory hog is OOM-killed with rc=137
  (`memory.max`); swap is capped.
- [ ] **Exit-status propagation**: force a failing tenant build. Expected:
  real exit status propagates through the cgroup cleanup path; a genuine
  exit-2 build failure is **not** rerun unbounded.
- [ ] **Effective-limit verification**: misconfigure one tenant limit (e.g.
  a value the kernel rejects). Expected: startup/build setup fails safely
  and loudly; no silent unbounded fallback.
- [ ] **Operator override survival**: hand-edit a tenant conf.d override,
  then rerun the mt-admin operation that re-renders it. Expected: the
  override survives the atomic rerender.
- [ ] **Hybrid-hierarchy host**: on a host without unified cgroup v2 (or
  with delegation denied). Expected: build bounding falls back to
  nice-only and mt-admin reports the reason loudly — no hard-ceiling
  claims in output.
- [ ] **add/remove sync**: add a scratch tenant, then remove it. Expected:
  subtree and boot script entries appear on add and disappear on remove
  with no manual cleanup.
- [ ] **Module-split regression sweep**: after the mt-admin refactor into
  `mt-admin-lib/NN-*.sh`, run the full lifecycle once on a scratch tenant:
  `add-tenant` → `build` → `status` → `start` → `stop` → `remove-tenant`.
  Expected: CLI surface and entrypoint unchanged, every step succeeds.

## 2. Port registry v15 → v16 migration (MT)

Why manual: migration runs against a live `/etc/lunarwing/ports.json`; CI
only checks the script source.

- [ ] **Auto-migration**: on a v15 registry, run any mt-admin registry
  operation. Expected: `health_metrics` slot allocated (default port 50000,
  above all tenant ranges), registry version becomes 16, tenants untouched.
- [ ] **Standalone path**: run `ic/scripts/migrate-ports-v16.sh` directly on
  another v15 copy. Expected: same result, idempotent on a second run.
- [ ] **Collision refusal**: hand-edit a registry so a tenant occupies the
  would-be `health_metrics` port, then migrate. Expected: migration refuses
  with a clear error and leaves the registry unmodified.

## 3. Missions: budgets, dedup, and the skill-learning gate (SN or MT)

Why manual: budget visibility spans CLI **and** web surfaces, and the dedup
and gating semantics involve daemon restarts and real listener deliveries —
integration tests cover the engine logic, not the end-to-end surfaces.

- [ ] **Budget create/list**: `mission_create` with `max_threads_per_day`
  set, unset (expect default 10), and `0` (unlimited). Expected: CLI
  `mission_list` and the web mission list/detail pages show
  `threads_today`, `max_threads_per_day`, and `budget_exhausted` correctly
  for all three.
- [ ] **Exhaustion UX**: drive a mission to its daily cap. Expected: a
  warn-level log naming the mission and counters; `mission_fire` reports
  the precise not-fired reason (budget exhausted, noting the UTC-day
  reset); web/CLI surfaces show `budget_exhausted: true`.
- [ ] **Trigger dedup across restart**: let a conversation-insights batch
  fire a mission, then restart the daemon and re-deliver the same batch
  (or wait for the listener to re-deliver). Expected: zero new threads,
  zero budget spent, zero tokens — the fire is skipped as already
  processed.
- [ ] **Dedup rollback**: force a thread-spawn failure after the fire is
  reserved (e.g. transient provider outage). Expected: the dedup key rolls
  back and a later retry still fires the mission.
- [ ] **SKILL_SELF_IMPROVEMENT=0 gate**: boot a tenant with the flag off.
  Expected: skill-extraction and skill-maintenance missions are paused at
  bootstrap with the `skill_learning_gate_paused` marker; fires are
  rejected with the unlock path stated; the extraction trigger does not
  fire. Conversation-insights and expected-behavior missions are **not**
  gated.
- [ ] **Gate resume semantics**: with the gate on, pause one gated mission
  by hand, then set `SKILL_SELF_IMPROVEMENT=1` and restart. Expected:
  exactly the gate-paused missions resume; the operator-paused mission
  stays paused; `resume_mission` while still disabled is rejected with the
  unlock path.

## 4. MCP marketplace trust stack and gateway control plane (SN)

Why manual: fail-closed activation, revocation fetch over real HTTPS, and
the loopback control plane are only source-verified in CI; the enforcement
paths need a real `mcp-servers.json` and a real daemon startup.

Prerequisite: a scratch `mcp-servers.json` with the new `marketplace`
section populated (trust store dir, revocation cache, revocation URL,
publisher key).

- [ ] **Fail-closed on revoked server**: publish/sign a revocation list
  that revokes an enabled server, then load config / start the daemon.
  Expected: startup skips (or config load rejects) the revoked server with
  a clear error; the rest of the fleet still activates.
- [ ] **Fail-closed on corrupt trust material**: corrupt the trust store
  and the revocation cache (separately). Expected: config load fails
  closed with an explicit error in both cases — never a silent bypass.
- [ ] **Revocation fetch resilience**: point the revocation URL at an
  unreachable host with a warm cache. Expected: startup keeps the cached
  list and proceeds; newly-revoked packages are still skipped per the
  cache. With a cold cache, startup proceeds without revocations applied
  (documented behavior) and logs it.
- [ ] **Control plane enablement**: set `LUNARWING_MCP_GATEWAY_CONTROL_BIND`
  and `LUNARWING_MCP_GATEWAY_ADMIN_TOKEN` (≥16 chars), start the daemon.
  Expected: control plane bound on loopback only; unauthenticated requests
  rejected (constant-time bearer); listings redacted; routing headers
  stripped; non-loopback bind refused unless
  `LUNARWING_MCP_GATEWAY_CONTROL_ALLOW_NON_LOOPBACK=1`.
- [ ] **Invalid control-plane config**: short token / malformed bind.
  Expected: control plane disabled with a warning; the daemon still
  starts.
- [ ] **External runtime descriptors**: register a server via `lunarwing
  mcp add --transport external`. Expected: descriptor validated at load,
  sandbox flags honored, pinned provenance digest verified by the adapter,
  `lunarwing doctor` silent. Then remove a runtime declaration and rerun
  `lunarwing doctor`: expected — it flags the undeclared runtime /
  unconfigured worker name.
- [ ] **Mandatory sandbox profiles**: enable `require_sandbox_profiles`
  and attempt to register a server missing mandatory controls. Expected:
  refusal naming `missing_mandatory_controls`; with the legacy default
  (flag off) the same registration succeeds as before.
- [ ] **Protocol 2025-03-26 acceptance**: connect to a server that answers
  initialize with revision `2025-03-26` (the managed study-notes server is
  one). Expected: activation succeeds; an unknown third revision still
  fails before activation.

## 5. MCP gateway data plane (Part 3) (SN)

Why manual: true SSE streaming, permit-pool isolation, and deadline
behavior are timing/concurrency properties that need a live gateway and
upstream.

- [ ] **SSE streaming passthrough**: route a notifications-heavy SSE
  upstream through the bundled gateway. Expected: events stream
  incrementally (no buffering to completion), `listChanged` passthrough
  works, and the stream has no cumulative lifetime cap but does enforce
  the per-read idle timeout (default 300 s).
- [ ] **Permit-pool isolation**: hold a long-lived SSE stream open while
  issuing tool-call POSTs. Expected: POSTs are not starved (separate
  transient and stream pools of 64 each).
- [ ] **Body read deadline**: send a request whose body trickles past the
  read deadline. Expected: HTTP 408; other requests unaffected.
- [ ] **Clean buffered errors**: force an upstream failure on a non-SSE
  call. Expected: clean 502 with a sanitized upstream URL (no userinfo,
  no credentials) in logs and client error.
- [ ] **Userinfo rejection**: register an upstream URL containing
  `user:pass@`. Expected: rejection at validation.
- [ ] **Bearer validation**: tokens outside 1..=4096 visible-ASCII.
  Expected: rejection before any upstream contact.

## 6. Engine V2 "always" approval persistence (SN, web or REPL gate UI)

Why manual: the fix is in the gate-resume path with a live flaky tool;
item 1's re-review verified the code mirrors v1, but the operator-visible
behavior needs one live pass.

- [ ] **Failed call still persists**: with Engine V2 active, invoke the
  GitLab WASM tool against an issue that 404s (or any flaky tool that
  returns `is_error`), approve with "always". Expected: the next call to
  the same tool does **not** re-prompt, even though the first call failed.
- [ ] **ssh exception**: approve an ssh call "always", make the command
  fail. Expected: a later ssh call still prompts (persistent grant only
  earned on genuine success).
- [ ] **Disallowed tools filtered**: pick a tool whose capabilities
  disallow persistent grants. Expected: "always" is not offered / not
  persisted regardless of success.

## 7. GitLab WASM tool 0.1.3 — Work Items API (SN, needs a real GitLab)

Why manual: behavior depends on a live GitLab 19.x instance with the Work
Items migration state and feature gating.

- [ ] **get_work_item / list_work_items**: run both against a project with
  Issues and Tasks. Expected: correct items returned via the unified Work
  Items API.
- [ ] **get_issue 404 fallback**: read an issue that classic-API 404s
  (migrated to Work Items). Expected: exact Work Items IID fallback finds
  it.
- [ ] **Pagination**: list against a large project (>1 page). Expected:
  bounded same-origin keyset pagination completes, stable-ID dedup, totals
  reconcile with the classic issue count — no stalls or miscounts.

## 8. Agent failure recovery with next steps (SN)

Why manual: these are operator/model-visible UX paths; unit tests assert
vocabulary, but a human should see each surface once.

- [ ] **Recoverable provider failure**: induce a transient provider error
  (e.g. brief network drop to the LLM endpoint). Expected: one bounded,
  model-visible recovery turn, then normal continuation; on persistent
  failure, a typed `LoopOutcome::Failed` whose terminal message states
  next steps.
- [ ] **Iteration-limit final turn**: drive a turn past the iteration
  limit. Expected: one text-only final turn with terminal guidance, not a
  bare cutoff.
- [ ] **Denied approvals resume the loop**: deny one call in a multi-call
  batch. Expected: the loop resumes with per-call denial tool-results
  (deferred batch calls resolved too) instead of the turn ending.
- [ ] **Unlock-path errors**: trigger disabled-tool, not-found,
  auth-required, rate-limited, and hook-rejection errors. Expected: each
  message tells the operator/model what unlocks the call; hook rejections
  name the unlock path.
- [ ] **No diagnostic leakage**: inspect the model-visible guidance for
  each case above. Expected: no raw provider diagnostics copied into
  guidance.
- [ ] **External worker recovery**: give an external worker an
  unavailable-capability result repeatedly. Expected: worker continues, a
  no-progress recovery turn occurs, and after 3 all-failed rounds a
  warning then a typed failure.
- [ ] **Lightweight routine one-shot recovery**: cause a single provider
  failure inside a lightweight routine. Expected: the mirrored one-shot
  recovery contract holds.

## 9. Cross-tenant knowledge sharing (MT, ≥2 tenants)

Why manual: the entire path is cross-daemon coordination over real rooms;
mock coverage cannot prove multi-tenant propagation, budgets, or
mixed-version behavior.

- [ ] **Opt-in propagation**: on tenant A set `COORD_KNOWLEDGE_PUBLISH=1`
  and `COORD_KNOWLEDGE_PUBLISHERS` on tenant B to allow A. Post a `KNOW:
  <topic> | <finding>` record. Expected: one durable finding lands in B's
  `shared` study-notes scope as `needs-verification`, with sender
  provenance (`coord-<readable>-<hash>` agent namespace); content wrapped
  and marked untrusted for model exposure.
- [ ] **Default deny**: with `COORD_KNOWLEDGE_PUBLISHERS` empty on B.
  Expected: nothing ingests (empty = deny all).
- [ ] **Budgets**: exceed the per-sender and daemon-wide ingest budgets.
  Expected: further KNOW records refused gracefully; fail-open hook never
  blocks the pipeline (priority 13).
- [ ] **Mixed fleet**: deliver a KNOW record to an older daemon (or one
  with publish gating off). Expected: schema-valid, ignored safely — no
  errors, no ingestion.
- [ ] **Sweep and sentinel**: strand a coordination wire note (wire
  namespace + sentinel + no metadata). Expected: the periodic sweep reaps
  it after the 24 h grace; `knowledge_list_stale` excludes coordination
  notes unless `include_coordination=true`; `knowledge_stats` shows the
  coordination count.
- [ ] **mt-admin env preservation**: upgrade/re-render a tenant that has
  all six `COORD_KNOWLEDGE_*` vars set. Expected: all preserved/backfilled
  in the regenerated env.

## 10. Study-notes knowledge store and tenant provisioning (MT)

Why manual: provisioning touches a shared flock-serialized checkout on a
live host, and the seven-tool parity should be exercised end-to-end once
against a real daemon.

- [ ] **Native seven-tool parity**: against the built-in layer, exercise
  all seven tools including `knowledge_list_topics` and
  `knowledge_update_status`. Expected: canonical merge semantics on
  metadata-only updates; `stale_after_days` (default 90) and
  `status_reason` present and honored by stale detection;
  `min_confidence` filters on both backends; tag filters require **every**
  listed tag.
- [ ] **--with-study-mcp provisioning**: `add-tenant --with-study-mcp` a
  scratch tenant. Expected: a read-only managed `study_notes` MCP server
  wired in; shared checkout (default `/var/cache/lunarwing/lw2studymcp`)
  cloned when missing and fast-forwarded when clean.
- [ ] **Dirty-checkout refusal**: dirty the shared checkout, then provision
  another tenant. Expected: refusal with a clear message (no silent reset
  of the shared repo).
- [ ] **configure-study-mcp**: run with `--home` omitted (uses default)
  and with `--sync` against an explicit home. Expected: `--sync` forces a
  latest upstream pull.

## 11. Healthpoint metrics exporter and listener (MT + SN; SD and ORC)

Why manual: the exporter runs from a live cron pipeline writing real
`.prom` files, Gotify dedup is time-based, and the listener binds a real
socket per init system. Mock suites prove logic, not wiring.

- [ ] **Textfile export**: run the health pipeline via `cron-wrapper.sh`.
  Expected: atomic `metrics.prom` with status 0/1/2/3,
  `component_enabled` (a disabled component is not "failing"), per-check
  `duration_seconds`, per-tenant lunarvision/lunarvoice labels discovered
  from the ports registry (never hardcoded 8088/8089), and self-heal
  restart/escalation/backoff/flapping gauges. Numeric/boolean leaves only
  — grep the output for any secret-shaped strings.
- [ ] **Threshold layer + Gotify dedup**: configure
  `THRESHOLD_<metric>="warn:>N crit:>M"` and force a warn then a crit.
  Expected: `lunarwing_health_early_warning` series emitted and one
  Gotify notification per metric+severity within the cooldown — not a
  storm.
- [ ] **Contrib assets**: load the shipped Prometheus rules and Grafana
  dashboard from `ic-infrastructure-health-check/contrib/`. Expected:
  rules evaluate against the real exposition.
- [ ] **Listener install (per init)**: run
  `ic/scripts/install-health-metrics-listener.sh` on systemd, OpenRC, and
  (if available) launchd after the v16 migration. Expected: loopback-only
  bind with peer enforcement; constant-time bearer required; exposition
  served over HTTP on the registry `health_metrics` port.
- [ ] **Watchdog scope (SD)**: set `LUNARWING_WATCHDOG_SYSTEMD_SCOPE=user`
  on a single-node user-manager install and let the hourly watchdog run.
  Expected: it sees and can restart the user unit; invalid values warn and
  fall back to `system`.
- [ ] **Bounded user-bus probes (SD)**: simulate a hung tenant user
  manager. Expected: self-heal's per-tenant probes time out at
  `SELF_HEAL_SYSTEMD_PROBE_TIMEOUT` (default 15 s) instead of stalling the
  sweep.
- [ ] **Timer blind spot (SD)**: create an active/waiting timer with no
  computable next elapse. Expected: `health-systemd.sh` reports degraded
  with an explicit issue.
- [ ] **Health oneshot timeout (SD, MT)**: inspect the mt-admin-rendered
  `lunarwing-mt-health.service`. Expected: explicit `TimeoutStartSec`
  present (≥ interval).
- [ ] **OpenRC watchdog bounds (ORC)**: wedge a supervise-daemon-managed
  service. Expected: every `rc-service` call in the watchdog is bounded
  (`lunarwing_watchdog_probe_timeout`, default 15 s); the flock-holding
  run cannot hang.
- [ ] **OpenRC cron blind spot (ORC)**: install the health hook with no
  cron daemon running. Expected: `health-openrc.sh` reports degraded
  (report-only), never downgrading a worse verdict.

## 12. WASM channel tool activity (SN or MT; needs live XMPP + WeeChat + DarkIRC)

Why manual: rendering is guest-side WASM behavior over live relays; no
automated channel-artifact E2E exists this release.

- [ ] **Default off**: with neither `TOOL_ACTIVITY` nor
  `channels.tool_activity` set, run a tool-using conversation. Expected:
  no tool-activity events reach any channel guest; guest CPU unchanged.
- [ ] **Flag on, startup and hot-activate**: enable the flag, restart, run
  tool calls; then hot-activate a channel and repeat. Expected:
  ToolStarted/ToolCompleted/ToolResult rendered in both cases — XMPP DMs
  get preformatted tool messages (groupchat stays suppressed), DarkIRC and
  WeeChat render `[status]` lines.
- [ ] **Native channels unaffected**: native channels keep their own
  per-channel `/debug` toggles and ignore the global flag.

## 13. WeeChat outbound integrity (SN or MT; live relay)

Why manual: the bug was relay-path truncation visible only in a real
WeeChat client.

- [ ] **Multi-line survival**: produce a multi-line tool result and agent
  reply. Expected: every line arrives in the operator's client; nothing
  after the first newline is lost; `\r`, NUL, and control chars stripped.
- [ ] **Slash escaping**: produce tool output whose line starts with `/`
  (e.g. `/msg bob hi`). Expected: the line renders literally (doubled
  leading slash) and is **not** executed as a client command; the
  intentional `/msg` DM fallback still sends raw.

## 14. XMPP bridge send body limit (SN or MT; live bridge)

Why manual: the regression test proves the handler is reached; the
end-to-end path (real bridge, real XMPP server, real photo) is manual.

- [ ] **Large photo send**: attach and send an image a few MiB in size via
  the agent. Expected: delivered; no HTTP 413.
- [ ] **Boundary sizing**: send near the host's 50 MiB outbound attachment
  cap. Expected: accepted (70 MiB JSON body limit covers base64
  inflation); beyond it, a clean rejection.
- [ ] **Other routes bounded**: POST >2 MiB to a non-send route. Expected:
  still rejected at the default limit.
- [ ] **Auth before buffering**: send a large body with a bad bearer.
  Expected: rejected by middleware before the body is buffered (no memory
  amplification).

## 15. External workers and orchestrator wire shape (MT or worker-capable SN)

Why manual: requires real orchestrator dispatch to real worker containers.

- [ ] **Kimi worker without project_dir**: dispatch a Kimi Code job with
  no explicit project directory. Expected: the TypeScript bridge accepts
  the payload (field omitted, not JSON `null`) and the job runs.
- [ ] **Worker pins**: rebuild the opencode and kimi worker images.
  Expected: `/health` reports `opencode_ref: v1.18.18` and
  `kimi_code_version: 0.37.2` respectively.
- [ ] **Three-worker acceptance**: run the same no-project-dir smoke job
  through Pebble, OpenCode, and Kimi. Expected: all three accept the wire
  shape.

## 16. Upgrade-path dress rehearsal (MT, one production-like tenant)

Why manual: nothing automated exercises the upgrade notes as a sequence.

- [ ] Follow `RELEASE-v2.0.8.0.md` Upgrade Notes 1–10 in order on a
  sacrificial tenant/host: backup → port-registry v16 migration →
  `setup-cgroups` (ORC) → marketplace section → gateway control plane env
  → `COORD_KNOWLEDGE_*` opt-in → worker image rebuilds → legacy
  `IRONCLAW_*` alias check (old envs still work, new envs lack them) →
  health-metrics listener opt-in → `SKILL_SELF_IMPROVEMENT=0` behavior
  note. Expected: every step behaves as documented; any deviation is a
  release-notes bug to fix before ship.

## 17. Deliberately out of scope for manual testing

Covered by automated tiers or already verified live during development;
listed so nobody re-spends effort:

- Rust unit/integration/clippy/fmt tiers and the WASM fixture CI fix
  (`docs/ops/PRE-RELEASE-TESTING.md`).
- Health-check mock suites (`ic-infrastructure-health-check/tests/`),
  including the new watchdog-systemd, watchdog-openrc, and metrics-unit
  suites — logic is proven there; only live wiring is in section 11.
- The XMPP body-limit regression test (handler reachability) — section 14
  covers the live remainder.
- Version bumps (crates/channels 2.0.7→2.0.8, registry patch bumps),
  cargo-deny/RustSec posture, Kaniko push retries, and CI branch-scope
  changes — CI-verified or ops-side only.
- Repo-hygiene moves and documentation refreshes — reviewable in diffs
  (`docs/ops/history/v2.0.8.0/REPO_HYGIENE.md`).

## Sign-off

| Section | Host / init | Commit | Tester | Date | Result |
|---|---|---|---|---|---|
| 1. cgroups (ORC MT) | | | | | |
| 2. ports v16 | | | | | |
| 3. missions | | | | | |
| 4. MCP trust | | | | | |
| 5. MCP data plane | | | | | |
| 6. Engine V2 approvals | | | | | |
| 7. GitLab tool | | | | | |
| 8. Failure recovery | | | | | |
| 9. KNOW sharing | | | | | |
| 10. Study-notes | | | | | |
| 11. Health metrics | | | | | |
| 12. Tool activity | | | | | |
| 13. WeeChat integrity | | | | | |
| 14. XMPP body limit | | | | | |
| 15. Workers | | | | | |
| 16. Upgrade rehearsal | | | | | |
