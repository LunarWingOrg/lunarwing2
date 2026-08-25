# Self-Heal Repair Followups (triaged)

> **Current status (reverified 2026-08-25): PARTIAL.** Notifier timeout,
> truncated-state recovery, cross-init scheduling/removal, escalation cooldown,
> systemd tenant-user-bus remediation, and bounded init probes have landed.
> Per-tenant application-level channel health (10) remains deferred; the newer
> base-daemon service watchdog deep check is a separate single-instance path.

Followups for the host-global health-check + self-heal pipeline
(`ic-infrastructure-health-check/`: `infrastructure-health-check.sh` →
`lunarwing-self-heal.sh`, paged by `send-notification.sh`, provisioned through
`ic/scripts/mt-admin-lib/12-health-pipeline.sh`).

**Triaged 2026-06-15** against the actual scripts. The original list mixed three
different kinds of work — real edits to the self-heal mechanism, ops/release
tasks, and live validation — and a couple of items were already done. This
rewrite separates them: **only 3 of the 10 are code changes to the self-heal
mechanism** (§A); the rest are tracked elsewhere (§B). Historical line anchors
below describe the pre-decomposition script; use the named functions and current
module paths above when applying follow-up work.

## At a glance

| # | Item | Self-heal code? | Difficulty | Action |
|---|------|:---:|---|---|
| 6c | Notifier timeout | ✅ | trivial | **FIXED** `2b462f95` |
| 6b | Truncated-state recovery | ✅ | easy | **FIXED** |
| 2 | systemd `.timer/.service` scheduling | ✅ | moderate | **FIXED** |
| 6a | Escalation cooldown / rate-limit | ✅ | moderate | **FIXED** |
| 10 | Per-tenant app-level health (Pattern C) | ✅ | involved | **defer** (own design pass) |
| 1 | Propagate 4 fixes to staging/prod | ❌ ops | n/a | mostly done — redeploy only |
| 3 | Version bump 1.1.3→1.1.4 + test sweep | ❌ release | n/a | release prep |
| 4 | Live escalation→page demo | ❌ validation | n/a | validation pass |
| 5 | Reboot re-validation | ❌ validation | n/a | validation pass |
| 7 | WeeChat relay configure/prune | ❌ channel cfg | n/a | channel config |
| 8 | Prune noisy WASM channels | ❌ channel cfg | n/a | channel config |
| 9 | Document pipeline + `health.env` toggles | ❌ docs | easy | docs task |

---

## §A. Self-heal mechanism changes

These are the only items that edit the pipeline. **Recommended order: 6c → 6b → 2 → 6a.**

### Quick wins — do now

#### 6c. Notifier timeout — *trivial (~3–6 lines)* ✅ FIXED (commit `2b462f95`)
The notifier is invoked unguarded at `lunarwing-self-heal.sh:474`. A hung Gotify endpoint stalls the
whole self-heal tick.

- **Change:** wrap the call —
  `timeout "${SELF_HEAL_ESCALATE_TIMEOUT:-30}" "$notify_script" "$status" "$report_path" >&2 || log WARNING …`
- Guard with `command -v timeout` (fall back to a bare call) — GNU `timeout` is absent on
  macOS/launchd by default.
- The existing `|| log WARNING` already absorbs exit 124, so no extra error handling is needed.
- Keep the `>&2` redirect: `escalate_service` runs inside `remediate_component`, whose **stdout is the
  returned state JSON** — stray stdout corrupts state (the documented `:474` hazard).
- *(curl itself is already bounded at `send-notification.sh:66` via `--connect-timeout 10 --max-time 15`.)*

#### 6b. Truncated-state recovery — *easy (~8 lines, mostly already there)* ✅ FIXED
Recovery already half-exists: `load_state` falls back to `{}` on parse failure and
`save_state` is atomic (mktemp + mv). The gap was that a corrupt file was **silently** discarded
— which is exactly what drops `escalated:true` and causes re-paging.

- **Change:** in `load_state`, on jq parse failure log a WARNING and rename the file to a **fixed**
  single-slot `"$STATE_FILE.corrupt"` (no timestamp) before emitting `{}`, so corruption is visible and
  preserved for forensics while being last-wins / bounded — it cannot accumulate across recurring
  corruption. (An earlier draft used `.corrupt.<ts>`, which the "won't accumulate" comment contradicted.)
- **Tests:** Section O in `test-self-heal-matrix.sh`, rewritten to actually exercise the path —
  O1: non-empty truncated state → detect + rename + recover + complete; O2: a 0-byte file is corrupt,
  quarantined, and replaced with `{}`; O3: single-slot rename leaves exactly one `.corrupt` after repeated corruption. Seeds
  corrupt bytes directly (not via `seed_state`, whose `jq -n` rejects malformed JSON to an empty file).
  Dry-run reports the quarantine action but preserves the original file; live recovery performs it.

#### 2. Wire systemd `.timer/.service` scheduling — ✅ FIXED

The original monolithic implementation steps are now historical. Current
ownership is `ic/scripts/mt-admin-lib/12-health-pipeline.sh`:

- `ensure_health_pipeline()` installs shared scripts and the launcher, then
  selects OpenRC cron/fcron or `_install_health_systemd_timer()`.
- systemd uses a root, system-level `lunarwing-mt-health` oneshot/timer because
  one run discovers and remediates multiple tenants. OpenRC uses the matching
  host-global scheduler path.
- `remove_health_pipeline()` tears down the schedule for either supported init.
- The write-if-absent `health.env` remains operator-editable, but subsequent
  provisioning reconciles `LUNARWING_SERVICE_MANAGER` and the active tenants
  registry path rather than retaining stale routing values.

The former systemd follow-up is also complete. `health-systemd.sh` discovers
per-tenant user units, and `lunarwing-self-heal.sh` probes/restarts them as the
tenant with `XDG_RUNTIME_DIR=/run/user/<uid>` and `systemctl --user`. Calls that
could hang are bounded by `SELF_HEAL_PROBE_TIMEOUT` (the older
`SELF_HEAL_SYSTEMD_PROBE_TIMEOUT` spelling remains an alias). This does not close
item 10: those checks establish unit/container liveness, not each tenant's
authenticated gateway `channel_health` semantics.

### Needs one design choice — do now

#### 6a. Escalation cooldown / rate-limit — ✅ FIXED

Implemented as collision-resistant per-service sidecars under self-heal state.
Cooldown starts only after notifier confirms delivery; missing Gotify token,
notifier failure, timeout, or cooldown deferral leaves escalation pending for a
later tick instead of setting the permanent `escalated` gate. Flap history now
survives recovery, so repeated fail/recover cycles reach flap guard correctly.

- Sidecars are named `.last_escalation_page.<safe-prefix>.<sha256>` so distinct
  service names cannot collide after filename sanitization.
- Sidecar state stays outside `state.json`, where recovery/pruning would erase it.
- Notification and cooldown logs remain on stderr; stdout remains state JSON.

### Defer — needs its own design pass

#### 10. Per-tenant app-level health (Pattern C) — *involved*
The host-global pipeline checks service **liveness** only. `health-gateway.sh` today doesn't curl
anything — it just stats local session files (`health-gateway.sh:9,55-64,107-120`). True app-level
self-heal means deep-checking each tenant's authed `/api/gateway/status` `.channel_health` (endpoint
confirmed at `server.rs:514 → 2528`, returns `Option<HashMap<name,{healthy,error}>>`; **authed**, so a
per-tenant token is required or it 401s).

- **Clean half (straightforward):** a new `health-tenant-gateway.sh` modeled on the tenant loop in
  `health-systemd.sh:91-118` — iterate `ports.json` (`.tenants[name].ports.gateway`, `.user`), read
  each tenant's `GATEWAY_AUTH_TOKEN` from `…/lunarwing/env/lunarwing.env`, curl the status endpoint,
  emit a `tenant_gateway` component. Keying targets to `lunarwing-<tenant>` routes through the existing
  `unit_tenant()/tenant_user()` restart path (`:169-184`) unchanged.
- **Messy half (why it's involved):**
  - *Liveness-vs-app conflict:* the svcmgr check can report `lunarwing-<tenant>` healthy (process up)
    while the app check says degraded (dead channel); the report-as-truth recovery (`:714-724`) would
    clear the app-degraded state on the same tick. App-degraded must win for the same unit — the core
    work.
  - *Correct remedy map:* a dead `xmpp`/`weechat` channel usually means the per-tenant **bridge**
    (`xmpp-bridge-<tenant>`) is down, not the gateway — needs a channel→service map, new logic.
  - *Secrets:* must read per-tenant `GATEWAY_AUTH_TOKEN` (root + path convention) and never log it.
  - *Absence handling:* `channel_health` is `None` when no channel manager — treat as "no signal", not
    degraded, or channel-less tenants flap.
  - *Performance:* N tenants × curl with small `--max-time`, ideally parallel, under the orchestrator's
    30s timeout (`infrastructure-health-check.sh:68`).
- **Recommendation:** don't ride this into 1.1.4. Give it a dedicated change with tests in
  `ic-infrastructure-health-check/tests/test-self-heal-matrix.sh`. Note `SELF_HEAL_REMEDY_LOGICAL`
  (`:68`) already gates off the gateway→lunarwing logical map under MT, so the new check must map
  straight to `lunarwing-<tenant>` units.

---

## §B. Not self-heal mechanism changes (tracked elsewhere)

#### 1. Propagate the 4 health-stack fixes — *mostly done; ops redeploy only*
All four fix commits — `c2c50b6b` (lock-fd leak), `7f592a95` (dead notifier), `bb8bbc57`
(lost escalation state), `1314cca9` (OpenRC env-file) — are **already on `staging` / `origin/staging`**
(staging is an ancestor of this branch). The fixes live in init-agnostic scripts, and the env-file fix
is in the **ungated** `write_tenant_lunarwing_env` (called at `mt-admin.sh:2344` with no `INIT_SYSTEM`
gate), so the systemd leg is **already covered in source**.

> ⚠️ Outdated framing: "propagate to the canonical release/staging branch" is done, and "the systemd
> leg has the env-file bug latent and notifier/escalation bugs active" only holds for a host running a
> **stale checkout**. The residual is a redeploy of the fixed scripts to the systemd+Docker prod hosts
> — a deployment task, not a self-heal code change.

#### 3. Version bump + test sweep — *release prep*
Bump 1.1.3 → 1.1.4 (workspace + 5 WASM channel crates), then run `ic/scripts/release-test.sh`,
`cargo test`/`--features integration`, clippy/fmt, e2e. Mechanical; no self-heal edit.

#### 4. Definitive live escalation→page demo — *validation*
Stage a throwaway OpenRC unit (respawn disabled + always-fail command) so a real restart fails →
escalate → Gotify, closing the single staging gap. Exercises the existing path; doesn't modify it.

#### 5. Reboot re-validation — *validation*
Reboot the staging host; confirm the fcron `*/15` schedule, escalation, and weechat-on-all all return
cleanly (earlier reboot test predated them). Observational.

#### 7. Resolve WeeChat — *channel config*
Configure the relay per tenant (Quickstart Part 2) or prune it per the consistency proposal. Channel
config, not a self-heal edit.

#### 8. Prune noisy WASM channels — *channel config*
Disable/configure telegram/darkirc/multica (error every poll with no token/daemon) per the pruning
proposal to quiet the logs.

#### 9. Document the pipeline + `health.env` toggles — *docs (easy)*
Add pipeline docs to `docs/ops/MULTITENANCY-PRODUCTION.md` plus a `health.env` toggles reference.
Toggles confirmed real: `HEALTHCHECK_NOTIFY` (`infrastructure-health-check.sh:274`),
`SELF_HEAL_REMEDY_LOGICAL` (`lunarwing-self-heal.sh:68,687`), and the report-age toggle — whose env
var is actually **`SELF_HEAL_MAX_REPORT_AGE`** (`lunarwing-self-heal.sh:72,644-648`), not the
`MAX_REPORT_AGE` shorthand; get the spelling right in the reference.
