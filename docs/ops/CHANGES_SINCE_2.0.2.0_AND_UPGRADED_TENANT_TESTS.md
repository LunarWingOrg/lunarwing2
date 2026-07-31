# Changes Since v2.0.2.0 and Upgraded-Tenant Test Plan (v2.0.3.0 "Sashiko")

**Date:** 2026-07-29
**Author:** Rarity (agent)
**Scope:** All changes landing in v2.0.3.0 since tag `v2.0.2.0` (227 non-merge
commits), plus what to test thoroughly on testing tenants that were
**provisioned on v2.0.2.0 and upgraded in place** to v2.0.3.0 via
`lunarwing-mt-admin.sh upgrade-tenant`.

**Sources:** `RELEASE-v2.0.3.0.md` (repo root, reviewed under goals #39/#40/#42),
`git log v2.0.2.0..HEAD`, and `ic/scripts/lunarwing-mt-admin.sh` usage. Command
verbs below (`status`, `upgrade-tenant`, `render-units`, `restart-tenant`,
`doctor`, `list-backups`, `start-worker`/`stop-worker`, …) were verified against
the current mt-admin script. Per the AGENTS.md hard rule, **all** tenant
lifecycle/status commands go through `lunarwing-mt-admin.sh` — never bare
`systemctl`/`rc-service` against tenant units.

---

## Part 1 — Inventory of Changes Since v2.0.2.0

### 1.1 Engine V2 security and lifecycle hardening

- `evaluate_with_provenance()` wired into the active dispatch path — taint-based
  capability rules now govern `LlmGenerated` content in production.
- `mission_delete` enforces ownership/admin checks and **hard-deletes** the
  record (was: soft-complete, callable by anyone knowing the UUID).
- `mission_update` name-field mismatch fixed (parser now reads the field the
  schema advertises).
- Background sweep for expired pending gates, with configurable expiry
  (default unchanged: 30 minutes) replacing traffic-driven cleanup.
- `event:` and `webhook:` mission cadences now actually dispatch through the
  scheduler (previously parsed but never fired).
- Startup loads all active missions across projects, not only the configured
  owner project.
- Mission ticker fault-isolated: one malformed/failed mission no longer aborts
  later work in the same cycle.
- Mission persistence and gate recovery hardened against crash-between-write
  states.

### 1.2 Tunnel provider replacement (breaking)

- Cloudflare, ngrok, and tailscale tunnel providers **removed**; new self-hosted
  NetBird provider added (`ic/src/tunnel/netbird.rs`).
- New migration `V24__remove_legacy_tunnel_settings.sql` (PostgreSQL + libSQL)
  drops legacy tunnel settings rows automatically at startup.
- Web networking settings panel updated: provider selector is now
  `none` / `netbird` / `custom` with NetBird domain/name-prefix and custom
  command/health-URL/URL-pattern fields (English + Chinese).
- Docs: `docs/guides/NETBIRD_TUNNEL.md`, `docs/guides/TUNNEL_PROVIDER_MIGRATION.md`.

### 1.3 WeeChat session-restore handoff (upgrade-critical)

- FIFO payload corrected to `*/upgrade -quit` (was the broken `*:/upgrade -quit`).
- New start helper `ic/scripts/lunarwing-weechat-start.sh` launches WeeChat with
  `--upgrade` so buffers are restored after stop/reboot.
- Stale-FIFO writes bounded so a dead relay cannot block the stop path.
- OpenRC stop fixed to run the graceful helper as the tenant user via `--chuid`
  (previously ran as root and silently could not reach the UID-scoped tmux socket).
- **Tenants must re-render units on v2.0.3.0** to pick up both halves (this
  applies even to tenants already re-rendered on v2.0.2).

### 1.4 External worker hardening (opencode / pebble / kimi)

- Bounded context so oversized prompts cannot exhaust the worker context window.
- Retry logic for transient OpenCode routing failures and provider timeouts.
- Extended external worker job runtime; hardened cancellation so a cancelled job
  releases terminal and routing resources; terminal-state handling no longer
  leaks sessions.
- OpenCode worker results and reconnects fixed; default plugin configuration added.
- Upgrade path: OpenCode workspace ownership migrated, worker start limits reset
  after upgrade, selected workers rebuilt during `upgrade-tenant`, stale worker
  images tolerated when a rebuild is impossible.
- OpenCode worker Rust toolchain updated; Podman build storage reduced.
- External job diagnostics now persist across restarts.
- Kimi Code added as a persistent external worker option
  (`--with-kimi`, `build-kimi-worker`, `configure-kimi`; see
  `docs/ops/WORKER-CONTAINERS.md`).
- Nanocode retired (v2.0.4.0). Provision/build/config surfaces reject it; tenant
  upgrade stops legacy supervision and removes routing/env entries without
  deleting data (see `docs/ops/NANOCODE-REMOVAL.md`).

### 1.5 MCP management

- In-place config updates: `lunarwing mcp update` (CLI),
  `tool_update` (conversational), `POST /api/extensions/{name}/update` (web).
  Active servers require explicit restart confirmation.
- `lunarwing doctor` MCP diagnostics: config validation, transport/enabled
  reporting, stdio command resolution through PATH.
- Stricter HTTP header validation: case-insensitive duplicates and custom
  `Mcp-Session-Id` / `Content-Type` / `Accept` headers are now rejected.
- Owner-scoped DB setting is authoritative for MCP state when DB is enabled;
  disk config migrates once into the configured owner.

### 1.6 Self-improving skills repair (machinery was inert; now functional)

- Terminal `StateChanged` thread event now broadcast **after** persist, so
  event-driven learning missions fire; infrastructure failures transition the
  thread to `Failed` with a terminal event.
- `state["trigger_payload"]` seeded across the mission spawn chain; gate-paused
  missions retain it for resume.
- Capability-lease leak fixed: terminal-thread revoke, spawn/resume rollback,
  single-execution on concurrent resume, startup sweep of orphaned leases.
- New mission-gated `__save_memory_doc__` host function persists Skill /
  fix-pattern / orchestrator docs into the workspace memory-doc store
  (`shared=True` writes to the shared space).
- Conversation-insights cadence scoped per user (`project_id:user_id`).
- Mission meta-prompt truncation budget raised 1000 → 4000 bytes.
- Defaults unchanged: `SKILL_SELF_IMPROVEMENT` stays off unless enabled.
- Root-cause writeup: `docs/ops/SIS_FIX_PROPOSAL.md`.

### 1.7 Study Notes skill and MCP integration

- New read/write `study-notes` skill (`ic/skills/study-notes/SKILL.md`) with an
  approved write path through the normal approval flow.
- Read-only MCP integration for external MCP clients.
- State handling and tunnel resets fixed.

### 1.8 DarkIRC delivery hardening

- Paced adapter outbox replaces direct-send (no more server flooding).
- Agent message budget integration; durable, frame-safe delivery across adapter
  restarts; adapter survives malformed/control-char IRC lines.
- Note: secure contact-key exchange remains **manual** (item #20 not landed).

### 1.9 Web gateway UI

- In-progress streaming answers survive thread switches and SSE reconnects
  (per-thread stream snapshots stashed/restored).
- Rendering overhead reduced: no per-flush DOM attribute churn, deferred/capped
  activity-log entries during bursts, auto-scroll only near the bottom.
- Static assets served with gzip + deferred script loading (faster first load).
- Networking panel matches the NetBird replacement (see 1.2).
- Tony Soprano option added to the Cavepony mode selector (EN + ZH).

### 1.10 Cavepony

- New Tony Soprano speech mode (dictionaries, validation, capability sidecar,
  WASM default-fuel execution covered by tests).

### 1.11 Toolchain, versioning, dependencies

- Crate/package versions `2.0.2` → `2.0.3`; release is `2.0.3.0`.
- Toolchain pinned to Rust `1.96.1` everywhere (rust-toolchain.toml, Docker
  builders, CI); Cargo MSRV reconciled `1.92` → `1.96.1`.
- `base64` 0.21 → 0.22; `tokio-xmpp` 5 → 6; `xmpp-parsers` 0.22 → 0.23;
  168-package `cargo update`; unblocked `deny.toml` advisory ignores removed.
- Observational nonblocking `rust-msrv-check` CI job on Rust 1.92.
- `--strict` mode added to `ic/scripts/build-wasm-extensions.sh` (fail-closed CI
  and upgrade builds). `build-lunarwing.sh --wasm` is a no-op — do not rely on it.

### 1.12 Documentation

- 6 of 9 architecture docs refreshed, 3 audited clean; bug docs re-verified;
  `docs/README.md` index rebuilt (75 entries added); root README updated.
- New: Arch Linux MT package list; Gentoo Python packages added;
  `XMPP_FT_REVIEW_A/R.md`; `SIS_FIX_PROPOSAL.md`; `REFLEX_COMPILER_V2_UPGRADE_PLAN.md`;
  `MULTIAGENTTEAMORCHESTRATION_INTEGRATION_PLAN_v2.0.4.0.md`; `CODENAME_IDEAS_v2.0.3.0.md`.

### 1.13 Explicitly NOT in this release (do not test as features)

- DarkIRC secure key exchange automation (manual still).
- mt-admin decomposition (proposal superseded; meta4 lib split unmerged).
- Package publishing / deployment automation (GitLab CI remains phase-one
  source verification only).
- The six known `#[ignore]`d tests (2 bootstrap readiness, 4 per-user
  system-prompt isolation) remain unresolved.

---

## Part 2 — What to Test Thoroughly on Upgraded Testing Tenants

Preconditions for every test below: the tenant was provisioned on v2.0.2.0 (or
earlier 2.x) and upgraded with
`sudo lunarwing-mt-admin.sh upgrade-tenant <name> --target <v2.0.3.0-ref>`
(run inside tmux; keep the default PostgreSQL backup and unit rendering on).
Verify the backup exists first: `sudo lunarwing-mt-admin.sh list-backups <name>`.

### T0 — Upgrade smoke (do first, gates everything else)

1. `sudo lunarwing-mt-admin.sh status <name>` → all expected units active;
   no crash-looping units.
2. Confirm the running binary is the new build: daemon reports version `2.0.3`
   (gateway UI footer / `status` output), and the checkout is on the target ref.
3. Hit the web gateway login and one trivial chat turn end-to-end.
4. Confirm the health pipeline sees the tenant healthy (no self-heal restart
   churn in the first 30 minutes).

### T1 — V24 migration and tunnel replacement (breaking area)

1. After first v2.0.3.0 startup, check the tenant DB: legacy tunnel settings
   rows (cloudflare/ngrok/tailscale keys) are **absent**.
2. If the tenant previously used a tunnel: configure NetBird (or `none` /
   `custom`) via the web networking settings panel; save; restart; confirm the
   tunnel comes up or fails with a clear error — not a silent fallback.
3. Regression: a tenant that never used tunnels must start clean with V24
   already applied (idempotent migration).
4. Fleet rule check: no v2.0.2 node is still running against the same DB —
   old code can recreate the removed rows (release-notes Upgrade Note 2).

### T2 — WeeChat restore handoff (the highest-risk upgrade item)

1. Confirm units were re-rendered: the rendered stop unit invokes the graceful
   helper and the start path uses `lunarwing-weechat-start.sh` with `--upgrade`.
   If `upgrade-tenant` was run with `--skip-render`, run
   `sudo lunarwing-mt-admin.sh render-units <name>` now and restart.
2. Open several WeeChat buffers (join channels, open DMs) on the testing tenant.
3. `sudo lunarwing-mt-admin.sh restart-tenant <name>` → after start, WeeChat
   has restored the same buffers (session restore, not a fresh empty client).
4. Repeat with a full machine reboot if the test host allows it.
5. Gentoo/OpenRC host specifically: verify stop runs the helper as the **tenant
   user** (not root) and actually reaches the tmux socket — check for a clean
   `*/upgrade -quit` in logs, not a tmux kill fallback.
6. Negative check: with the relay dead, stop must not hang on FIFO writes
   (bounded write, forced tmux fallback within the ~20 s helper window).

### T3 — Engine V2 mission behavior (newly-active code paths)

1. **Inventory before restart**: list all missions on the tenant. Any with
   `event:` or `webhook:` cadences will fire for the first time — confirm each
   is intended, or pause it before upgrade.
2. Create a test `event:` mission and a `webhook:` mission; drive their
   triggers; confirm they dispatch through the scheduler.
3. `mission_delete`: as a non-owner, attempt to delete another owner's mission
   → refused. As the owner, delete → record is gone from list output and the
   DB (hard delete), not soft-completed.
4. Pending-gate sweep: create a mission that parks on a gate; let it sit past
   the configured expiry with no traffic; confirm the background sweep expires
   it and the waiting thread unblocks/errors cleanly.
5. Cross-project loading: create a mission owned by a non-default project,
   restart the tenant, confirm it resumes firing.
6. Ticker isolation: seed one malformed mission alongside a healthy one;
   confirm the healthy mission still runs in the same cycle.
7. Timeout hierarchy: if the tenant sets `AGENT_HANDLE_MESSAGE_TIMEOUT_SECS`
   anywhere (env/DB), raise it to ≥ 630 or remove the override; verify a long
   Engine mission (near 600 s) completes instead of being killed at 400 s.

### T4 — External workers

For each worker type the tenant selected (`--with-opencode`, `--with-pebble`,
`--with-kimi`):

1. After upgrade, `sudo lunarwing-mt-admin.sh status <name>` shows the worker
   container up; if the worker was rebuilt during upgrade, confirm the new
   image is running; if the rebuild was skipped (stale image tolerated),
   confirm the tenant still started and the worker answers.
2. Submit a bounded job through the agent (`create_job`), let it finish, and
   confirm results return (OpenCode results/reconnect path was fixed).
3. Submit a job with an oversized prompt → worker rejects/truncates per the new
   bounded-context behavior rather than exhausting its context window.
4. Cancel a running job → terminal + routing resources are released; no leaked
   session visible in the worker; diagnostics for the job survive a daemon
   restart.
5. OpenCode specifically: confirm workspace file ownership is correct post-
   upgrade (ownership migration ran) and start limits were reset (no
   spurious start-rate-limit failure on first post-upgrade start).
6. Kimi worker (new): if selected, run one real job end-to-end; this worker
   option has the least field mileage.

### T5 — MCP lifecycle

1. `lunarwing doctor` (as the tenant user, in the tenant checkout): MCP section
   reports each server's transport + enabled state; enabled stdio servers with
   missing commands are flagged.
2. `lunarwing mcp update` on an inactive stdio server (change an arg) →
   persisted; reactivate → new argv in effect.
3. Update an **active** server via the web route or conversational `tool_update`
   → explicit restart confirmation is required; after confirming, the server
   reactivates with the new config.
4. Deactivate → tools disappear from the active set and the managed child is
   gone; Activate → fresh handshake, tools rediscovered.
5. Header validation: attempt to save an HTTP MCP config with duplicate or
   reserved headers (`Mcp-Session-Id`, `Content-Type`, `Accept`) → rejected.
6. Reminder for testers: CLI `mcp update`/`mcp remove` are persistence-only —
   restart the tenant (or use web/conversational surfaces) for runtime effect.
7. Owner scope: if the tenant uses a non-default owner, confirm MCP state
   migrated into that owner's scope and did not strand under `default`.

### T6 — Self-improving skills (newly functional — test on ONE testing tenant)

1. Enable `SKILL_SELF_IMPROVEMENT` on one testing tenant only.
2. Run a normal Engine V2 thread to completion → confirm the terminal
   `StateChanged` event fires (thread persisted `Done` first) and any
   event-triggered learning mission fires on it.
3. Confirm the learning mission thread state contains `trigger_payload`
   (inspect mission thread state), including across a gate pause/resume.
4. Let an extraction mission run; confirm a `source: "extracted"` Skill doc is
   actually persisted via `__save_memory_doc__` (previously impossible) and is
   visible to memory search / skill listing.
5. Lease audit: after terminal threads, no still-valid capability leases remain
   for them; restart the tenant and confirm the startup sweep logs/audits any
   historical strays while retaining files.
6. Two users in one shared project: user A's completed threads must not consume
   user B's conversation-insights cadence.
7. Known boundary to respect while testing: v1 harness / main assistant
   sessions / external workers emit no learning triggers — that is expected,
   not a failure.

### T7 — Study Notes

1. Via chat, create and update a study note through the skill → approval flow
   fires for writes; note persists in the workspace.
2. Via the read-only MCP integration, read notes from an external MCP client →
   reads work; any write attempt is refused.
3. Toggle the tenant tunnel (or restart it) and confirm Study Notes state
   recovers cleanly (state/tunnel-reset fix).

### T8 — DarkIRC (tenants provisioned with `--enable-darkirc`)

1. Send a burst of outbound messages → observe paced delivery (outbox), no
   flood-triggered server disconnect.
2. Restart the darkirc adapter mid-burst → queued messages survive and deliver
   after restart (durability).
3. Inject/observe malformed or control-char IRC lines → adapter survives and
   keeps processing.
4. Confirm outbound respects the agent message budget under a constrained
   turn budget.

### T9 — Web gateway UI

1. Start a long streaming answer; switch threads mid-stream; switch back →
   the in-progress answer is still there and continues.
2. Kill the network/SSE connection mid-stream (or wait out a proxy timeout) →
   on reconnect the stream snapshot is restored; history reconciles once
   (no duplicated messages).
3. During a burst, scroll up to read history → position holds (no forced
   auto-scroll); activity log stays capped and responsive.
4. Hard-reload with devtools open → static assets gzip-compressed, scripts
   deferred, first paint visibly faster than v2.0.2.0 baseline if comparable.
5. Networking settings panel renders the `none`/`netbird`/`custom` selector
   with the new fields in both English and Chinese.
6. Cavepony selector shows the Tony Soprano mode (EN + ZH) and the mode works
   in a chat turn.

### T10 — Regression sweep (standard tenant operations)

1. XMPP DM round trip; XMPP file attachment (XEP-0363) send/receive if the
   tenant uses it; LunarVision OCR enrichment on an image if deployed (probe
   via the registry `vision_health` port — **not** hardcoded 8088).
2. Gotify notifications still deliver (health pipeline + agent notifications).
3. `sudo lunarwing-mt-admin.sh stop-tenant <name>` / `start-tenant <name>`
   cycle clean; `restart-tenant` clean.
4. Worker stop/start individually: `stop-worker <name> <worker>` /
   `start-worker <name> <worker>`.
5. Memory tools: write/search/read a memory doc; confirm byte-budget
   truncation behaves (no errors on large docs, Unicode-safe truncation).
6. SSH harness (if enabled): one outbound git op succeeds — watch for the
   known inherited bare-remote-HEAD edge (BUG-ssh-git-ref-and-remote-head).

### T11 — Known-issue watchlist (expected, not regressions)

- `create_job(wait=true)` still blocks the originating agent turn
  (BUG-agent-loop-blocks-on-worker-jobs, open).
- Full TOML config rewrites can drop worker bearer tokens
  (BUG-external-worker-config-persistence, partial) — re-check worker auth
  after any manual config rewrite.
- The six `#[ignore]`d tests stay ignored; do not treat as upgrade failures.
- MCP same-name servers cannot be active for two owners; tool-name collisions
  skip rather than replace; `doctor` reads disk config, not owner-scoped DB.
- Pending-gate expiry default remains 30 minutes — tune per workload.

---

## Sign-off Checklist

| # | Area | Tester | Result | Date |
|---|------|--------|--------|------|
| T0 | Upgrade smoke | | | |
| T1 | V24 + NetBird tunnels | | | |
| T2 | WeeChat restore handoff | | | |
| T3 | Engine V2 missions | | | |
| T4 | External workers | | | |
| T5 | MCP lifecycle | | | |
| T6 | Self-improving skills | | | |
| T7 | Study Notes | | | |
| T8 | DarkIRC outbox | | | |
| T9 | Web gateway UI | | | |
| T10 | Regression sweep | | | |

Record findings in `docs/ops/` and file new entries under `docs/bugs/` for any
confirmed defect before tagging v2.0.3.0 as production-ready.
