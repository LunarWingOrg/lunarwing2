# Release Notes for LunarWing v2.1.4.0

## Codename: UNKNOWN (draft)

**Release date:** 2026-10-XX (not yet released; first draft 2026-10-01, repass 2026-10-01)

## Overview

LunarWing v2.1.4.0 is a recovery, dependency-audit, and engine-reliability
release — the cycle that repaired a red integration pipeline, put every
tracked lockfile and worker image through a full crate-security re-audit,
closed four of the defects v2.1.3.0 deferred to this release (WI#265,
WI#264, WI#244, WI#220 — WI#184 remains at design revision 3), fixed the
global mission-listing defect found in-cycle (WI#243), and landed the
first slice of WI#191's EMA estimation wiring.

The cycle opened with the version sweep: all LunarWing-owned release packages
moved from `2.1.3` to `2.1.4` (daemon, five workspace crates, XMPP bridge,
four WASM channels), channel and tool registry versions advanced together,
121 external crates took semver-compatible updates across the workspace and
satellites, `ic/vendor/` was regenerated, and the tracked manifest/lock audit
confirmed no project-owned package remained at 2.1.3. The external-crate
effort doubles as the security audit (item 3): every advisory ignore in
`ic/deny.toml` was re-verified, the resolved worker-image graph was scanned
patched-vs-unpatched, a full-repo yanked-crate pass retired the single yanked
crate found anywhere in the tree (chacha20 0.10.1 in upstream Pebble's lock,
now pinned 0.10.2 with a refreshed `PEBBLE_LOCK_SHA256`), and the Pebble
lock guard gained multi-version record keying and no-op-override rejection.
CI recovered from pipeline #4679's Cavepony release-metadata drift and
orphan-mission boundary violation; integration-target MRs now run the full
validation matrix. A follow-up CI audit then found the cargo-deny job green
dishonestly — vendored source replacement had silently degraded the
`yanked = "deny"` policy to per-crate warnings — and restored the gate so
it fails loudly instead (item 20).

Engine reliability dominates the feature work: mission execution recovers
from transient provider failures after partial text instead of aborting the
thread (WI#244), global mission listings no longer drop missions whose
project record is missing so every active mission keeps its daily rollover
(WI#243), `routine_history` resolves Engine V2 mission names (WI#265), and
the voice sidecar bounds abandoned inference admission so a timed-out or
cancelled call cannot pin the model mutex or starve other components
(WI#264). Shared tool execution learns bounded per-user EMA runtime
estimates surfaced as frozen advisory hints on approvals and pending gates
(WI#191 first slice, opt-in via `LUNARWING_TOOL_ESTIMATES=1`).

The operator surface gained per-tenant system-prompt editing in Settings with
Engine V2 initialization snapshots (WI#220), read-only tenant database and
daemon-PID diagnostics verbs in mt-admin (`psql`, `daemon-pid`) with a
WI#268 prompt-overlay audit runbook, an opencode2 worker bump to v2.0.20, and
a pairing allowlist fix so approving a new sender no longer erases the
existing allowlist. Outbound message delivery is also now honest about its
contract (WI#270): the `message` tool ack reports queue acceptance rather
than "delivered", the XMPP channel refuses to queue while disconnected, and
shutdown gives already-acked stanzas a bounded drain window. New tenants now
render `ORCHESTRATOR_SELF_MODIFY=true` (stored orchestrator and
prompt-overlay updates, WI#282), with the operator opt-out preserved across
re-renders. The WI#184 secrets-store pairing
redesign advanced to design revision 3 (both !1294 review P2s answered) with
the review-required legacy-matching and concurrency regressions landed ahead
of the substrate change.

The full docs set was repassed for the cycle: all 32 active guides under
`docs/guides/` and all 10 active architecture docs re-verified against source
and stamped, and the cycle's open work items were reconciled in a tracker
audit (23 open items, dispositions verified against the tree).

The release identifier is `2.1.4.0`; LunarWing-owned Rust crates and channel
packages use `2.1.4`. Rust 2024 and MSRV `1.96.1` are unchanged. There are
no new SQL migrations (`V27__memory_index_outbox.sql` remains latest). The
multi-tenant port registry stays at v17.

See the [v2.1.3.0 release notes](docs/releases/RELEASE-v2.1.3.0.md) for the
previous compatibility baseline.

## Highlights

- **Recoverable mid-stream provider failures (WI#244).** Mission executor
  turns that lose the provider after partial text now attempt bounded
  recovery for transient/unavailable failures instead of aborting the whole
  thread. Failed drafts stay out of history and tool execution; existing
  retry caps and rate-limit exclusions remain enforced. Attempts accumulate
  as provisional Live work with no per-retry reset. All 263 executor tests
  and the 8 `engine_v2_tensorzero_streaming` integration tests pass,
  including regressions that fail against the old implementation
  (interrupted-tool execution isolation, exhausted recovery, safe recovery
  context, successful-response-only persistence)
- **Global mission listings see every mission (WI#243).** `HybridStore`
  never overrode `list_all_missions_global`/`list_all_active_missions_global`,
  so both inherited a trait default derived from `list_all_projects()`. A
  mission whose `project.json` was missing or unparseable loaded into the
  missions map but was invisible to every global listing — never
  bootstrapped into the active index, no startup/tick rollover, no
  event/webhook dispatch, and `threads_today_date` pinned at its creation
  date. Both globals now enumerate the missions map directly. Regression:
  `global_mission_listings_see_missions_whose_project_file_is_missing`
- **`routine_history` resolves Engine V2 mission names (WI#265).** When the
  v1 routines lookup misses, the tool falls back to a read-only bridge query
  (`get_engine_mission_history_by_name`: default-project own+shared lookup,
  then a global ownership-filtered scan) rendering recent fires from
  `mission.thread_history`. Run summaries truncate by characters — never
  byte-indexing a `String` — so multilingual assistant text cannot panic the
  path. Not-found errors name both registries and point at
  `routine_list`/`mission_list`; limit clamped 1..=50
- **Bounded inference admission in the voice sidecar (WI#264).** STT/TTS/VAD
  each admit one call before taking shared capacity; blocking workers retain
  both permits until settlement, so repeated TTS retries cannot queue behind
  the model mutex or consume remaining STT/VAD capacity. Caller
  cancellation/timeout and worker settlement share a phase lock; `/ready`
  and `/metrics` report per-component activity, current/cumulative
  abandonment, and permanent failure. Poisoned model locks and panics return
  explicit `component_failed`/503 and keep readiness degraded until restart
  (supported whole-tenant recovery: `restart-tenant`). 23 sidecar tests per
  feature configuration, including six new regressions
- **Per-tenant system-prompt editing (WI#220).** Settings → System Prompt
  provides primary-scope source editing, missing-file creation, draft
  preservation, and a saved workspace preview with group/timezone controls.
  Authenticated `/api/prompts` routes retain workspace write
  validation/indexing and scope isolation. Engine V2 consumes the prompt as
  an initialization snapshot: activation requires `restart-tenant` followed
  by a new conversation; existing threads keep their System message. Five
  authenticated router regressions and five Chromium browser regressions
- **Tool runtime estimates with bounded EMA learning (WI#191, first
  slice).** Registry-owned, user/tool-scoped, in-memory calibration feeds
  the existing estimator from real tool completions across shared
  execution, local jobs, and lightweight routines (five-sample warmup,
  bounded outliers; failures/timeouts/cancellation/approval waits do not
  train). Frozen advisory runtime hints appear on both v1 approval paths and
  the v2 pending-gate insertion path. Display is opt-in
  (`LUNARWING_TOOL_ESTIMATES=1`; unset records only, `0` disables). Missing
  redacted parameter snapshots now render `null` on both display paths
  instead of exposing execution parameters. 114 targeted tests; helper p95
  overhead 0.260 ms
- **mt-admin read-only diagnostics and the WI#268 prompt-overlay audit.**
  New verbs `psql <tenant>` (init-agnostic, container-based read-only DB
  access) and `daemon-pid <tenant>` (strict PID-record validation with
  tenant association — malformed records are rejected, not converted into a
  PID). The audit runbook lets an operator determine whether an agent's
  effective system prompt differs from the shipped prompt without a Git
  diff and without enabling self-modification; a failed environment
  capture reports `uninspected (live gate)` with a non-zero exit rather than
  certifying "gate off"
- **Crate security audit and supply chain (item 3).** All 27 tracked
  lockfile roots re-scanned against a fresh RustSec db (2026-09-30) with an
  empty ignore list and `unmaintained = "all"`; the resolved worker-image
  graph scanned patched vs unpatched (control trips RUSTSEC-2026-0285);
  every accepted-advisory upstream blocker re-verified against live
  crates.io metadata; a full-repo yanked-crate pass covered every
  registry-locked version. Fixed this cycle: yanked chacha20 0.10.1 retired
  from upstream Pebble's lock (now pinned 0.10.2, refreshed
  `PEBBLE_LOCK_SHA256`), the Pebble lock guard keys multi-version records
  by (name, version, source) and rejects no-op overrides, and multi-version
  record-edit negative tests cover the reviewer's mutated-checksum probe.
  The eight `ic/deny.toml` advisory ignores remain (re-verified; unchanged
  id set). Evidence:
  [v2.1.4.0_CRATE_SECURITY_AUDIT.md](docs/ops/v2.1.4.0_CRATE_SECURITY_AUDIT.md)
- **CI recovery and integration-MR validation coverage (item 1).** Pipeline
  #4679's two failures — Cavepony release metadata drift (registry `0.4.3`
  vs Cargo/capabilities `0.4.2`; later `version-policy` required `0.4.4`)
  and the orphan-mission regression importing a concrete libSQL backend
  outside the DB boundary — were fixed without bypassing gates. Full
  validation now also covers integration-target MRs, with
  `LUNARWING_CI_FULL_VALIDATION=1` opt-in for scratch branches. Evidence:
  [v2.1.4.0_GOAL1_CI_RECOVERY.md](docs/ops/v2.1.4.0_GOAL1_CI_RECOVERY.md)
- **Pairing allowlist preservation (sbpatch1).** Approving a new sender
  previously opened the allowlist store with `truncate(true)` before
  deserialization, silently erasing every previously approved sender when
  the read failed. The store now reads under the lock with truncate(false)
  and rewrites through the same locked handle (set_len + seek + write +
  sync), so concurrent approvers cannot bypass the lock and shorter
  payloads cannot leave trailing bytes
- **Orchestrator self-modification for new tenants (WI#282).** add-tenant
  now renders `ORCHESTRATOR_SELF_MODIFY=true` so new tenants adopt stored
  orchestrator and prompt-overlay updates; an explicit operator opt-out is
  preserved across re-renders. Existing tenants are untouched
- **External worker: opencode2 v2.0.20 (item 2).** The opencode2 lane
  advances v2.0.18 → v2.0.20 at reviewed full commit
  `84c9be93a56304a108f1a22df0c5d62c26d5b6ca` (tag SHA verified live).
  Dockerfile pins, CI source_ref/build args, and the reproducible-build
  fixture moved together; the `bytecode: true`/`splitting: true` patch
  contract and bridge surface verified unchanged (diff is additive). kimi
  stays at 2.1.1, opencode v1 at `v1.18.25`, pebble at `2d0f3786…`
- **Documentation parity (item 12).** Seven docs re-verified against the
  tree for changes not covered by each goal's own updates: the mt-admin
  command reference (new verbs, stale worker list), MT-ADMIN-QUICKSTART
  (WI#268 diagnostics tier), TENANT-CONFIGURATION
  (`LUNARWING_TOOL_ESTIMATES`), TESTING_GUIDE (four v2.1.4.0 manual tiers),
  releases/docs indexes, and contributor onboarding freeze status
- **Pairing secrets-store design revision 3 (item 11, WI#184).** Both P2
  findings from the rejected !1294 review are answered in
  [PAIRING_SECRETS_STORE_DM_AUTH.md](docs/proposals/PAIRING_SECRETS_STORE_DM_AUTH.md):
  a source-tagged matching contract (strict byte-exact canonical-principal
  lookup, plus a legacy matcher applying today's exact comparison only to
  `kind:"legacy"` grants — no cross-kind broadening) and keyed
  HMAC-SHA256 grant names domain-separated from the per-secret encryption
  subkey, so database-read-only dictionary attacks against candidate IRC
  principals/JIDs no longer terminate without the master key. The
  review-required regressions are landed executable tests ahead of the
  substrate change: legacy `@nick`/username and trimmed-exact-id matching
  (`ic/src/pairing/store.rs`), concurrent-grant preservation (8 concurrent
  approvers), and the wasm `pairing_is_allowed` host-path regression
  (`ic/src/channels/wasm/wrapper.rs`)
- **Open work items audit (item 14).** All 23 open tracker items reconciled
  against the tree in
  [v2.1.4.0_OPEN_WORK_ITEMS_STATUS.md](docs/ops/v2.1.4.0_OPEN_WORK_ITEMS_STATUS.md):
  5 genuinely-open 2.1.4.0 items (#281, #280, #270, #269, #271), #184 with
  the !1294 rejection recorded, 15 deferred by label, and closeable evidence
  identified for #198/#199. Every code-facing disposition verified against
  source at `03f5650fe`, not taken from tracker bodies
- **Guides and architecture docs repass (items 16/17).** All 32 active
  guides under `docs/guides/` and all 10 active docs under
  `docs/architecture/` re-verified against current source and stamped for
  v2.1.4.0, recording the cycle's version moves (registry multica 0.1.13,
  darkirc 0.1.10, weechat 0.2.11, xmpp 0.1.16; WASM channel crates at
  workspace `2.1.4`) and confirming V27 remains the latest migration with
  the migration chain's `ironclaw-agent-v1` alias removed
- **Honest outbound delivery semantics (WI#270).** The `message` tool's
  success ack was an enqueue ack dressed as a delivery receipt — the gap
  behind scheduled mission PR Digest #59 recording "Delivered via XMPP" in
  memory while the queued stanza was silently lost in a rebuild window. The
  ack now reads `Queued message to <channel>:<target>` with a tool-description
  warning that success means acceptance for delivery; the XMPP channel
  refuses proactive enqueue while no session is bound; and `shutdown()`
  gives the client task a bounded 2s window to drain acked stanzas, WARNing
  with the pending depth when it cannot. Five new XMPP channel regressions;
  semantics and mission-spec discipline documented in
  [OUTBOUND-DELIVERY-SEMANTICS.md](docs/architecture/OUTBOUND-DELIVERY-SEMANTICS.md)
- **CI cargo-deny yanked gate restored (item 20).** The `cargo-deny` CI job
  was green dishonestly: running from inside `ic/` source-replaces crates.io
  with `ic/vendor/`, so the `yanked = "deny"` policy silently degraded to
  one `warning[index-failure]` per crate (738 in pipeline #4811) while the
  job exited 0 — and the flood pushed the trace past GitLab's 4 MB log cap,
  truncating the verdict away. The new
  `.gitlab/scripts/cargo-deny-check.sh` wrapper runs from the repo root with
  explicit `--manifest-path`/`--config`, keeps the trace compact, and
  hard-fails on any `index-failure` so the yanked check can never be
  silently skipped again; the `refresh-vendor.sh` post-vendor advisory gate
  routes through the same wrapper, and a validator rule plus a 6-case
  regression suite pin the contract

## Compatibility

| Area | v2.1.4.0 behavior |
|---|---|
| Release version | `2.1.4.0` |
| Crate/package version | `2.1.4` for all LunarWing-owned release packages (daemon, five workspace crates, XMPP bridge, four WASM channels) — tracked manifest/lock audit confirms none remain at 2.1.3 |
| Rust edition / MSRV | Rust 2024 / `1.96.1` (no change) |
| New SQL migration | None; `V27__memory_index_outbox.sql` remains latest |
| Multi-tenant port registry | v17 (no change) |
| Channel registry patches | darkirc 0.1.10, multica 0.1.13, weechat 0.2.11, xmpp 0.1.16 (behavior unchanged; rebuild required) |
| Tool registry patches | cavepony-tool 0.4.4 (release-metadata alignment; the `version-policy` gate requires all five version fields to advance together), gitlab 0.1.7, gotify 0.1.1, llm-context 0.1.2, multica-bridge 0.1.3, ssh 0.1.2, vision-analyze 0.1.4, voice-transcribe 0.1.2, web-search 0.2.3 |
| WASM runtime | Wasmtime / wasmtime-wasi 36.0.16 (no change from v2.1.3.0) |
| External crate updates | 121 semver-compatible updates across the ic workspace and satellite lockfile roots (notably hyper 1.11.1, reqwest 0.13.5, tokio-rustls 0.26.6, quinn 0.11.12, clap 4.6.7, thiserror 2.0.21, rkyv stack replaced by string_cache/web_atoms/zlib-rs in affected satellites); `ic/vendor/` regenerated |
| Worker image tool pins | kimi-code `2.1.1` (no change); opencode2 `v2.0.20` at `84c9be93a56304a108f1a22df0c5d62c26d5b6ca` (was v2.0.18); opencode `v1.18.25` (no change); pebble `2d0f3786…` with the reviewed upstream lock re-pinned for the chacha20 retirement (refreshed `PEBBLE_LOCK_SHA256`) |
| Engine provider recovery | Bounded per-turn recovery after partial text for transient/unavailable provider failures; retry caps and rate-limit exclusions unchanged; failed drafts never enter history or tool execution |
| Mission global listings | Missions whose project record is missing/unparseable remain globally enumerable; only Active ones reach the active-only listing |
| `routine_history` | Falls back to Engine V2 mission-name lookup when the v1 routines registry misses; character-based summary truncation |
| Voice sidecar admission | One in-flight call per component (STT/TTS/VAD) before shared capacity; blocked workers hold both permits until settlement; poisoned lock/panic = `component_failed`/503 until restart |
| Tool runtime hints | `LUNARWING_TOOL_ESTIMATES` read once at startup: unset/`observe` = record only, `1` = record + hints, `0` = off; restart resets learned models; no migration or new wire fields |
| Tenant system prompts | Settings editor writes primary-scope prompts consumed as Engine V2 initialization snapshots (activate via `restart-tenant` + new conversation); existing threads keep their System message |
| mt-admin verbs | New: `psql <tenant>`, `daemon-pid <tenant>` (read-only diagnostics); all existing verbs unchanged |
| New tenant env defaults | `ORCHESTRATOR_SELF_MODIFY=true` rendered at add-tenant (WI#282); explicit operator values preserved on re-render; existing tenants untouched |
| Pairing allowlist | Approvals no longer truncate the existing allowlist store before reading; concurrent approvals serialize under the file lock |
| Pairing store (WI#184) | Design revision 3 only — no substrate change this release; landed tests pin current legacy matching and concurrent-grant semantics ahead of the secrets-store migration |
| CI validation coverage | `.full-validation-context` includes integration-target MRs; scratch branches opt in with `LUNARWING_CI_FULL_VALIDATION=1`; worker publication remains protected/push-only |
| Deny policy | `ic/deny.toml` advisory ignores unchanged in id set (8), re-verified 2026-09-30 with blocking dependency chains documented |

## Changes

### Mid-Stream Provider Failure Recovery (WI#244)

Mission executor turns that lose the provider after partial text previously
aborted the thread — the deferred FrontierTEEC/TensorZero response-body
decode failure from v2.1.3.0. Recovery is now bounded and typed:

- Transient/unavailable failures after partial text admit a bounded retry;
  permanent failures do not.
- Failed drafts stay out of history and never reach tool execution.
- Existing retry caps and rate-limit exclusions remain enforced; attempts
  accumulate as provisional Live work with no per-retry reset (correcting
  the rejected !1258's stream-gap explanation).
- Successful responses are the only thing persisted, with usage accounted
  once.

Regression evidence: the new recovery/cap tests fail against the old
implementation; all 263 executor tests and all 8
`engine_v2_tensorzero_streaming` tests pass with the fix. Coverage includes
interrupted-tool execution isolation, exhausted recovery, safe recovery
context, and successful-response-only persistence/usage. Remote CI
acceptance from !1258 remains tracked against pipeline #4709.

### Global Mission Listings and Daily Rollover (WI#243)

The recurrence behind the dwarfstar "expected-behavior" evidence was a host
store defect, not an engine one. Missions load by directory scan
(`engine/projects/*/missions/*/mission.json`) while projects load from
`project.json`; `HybridStore`'s trait-default global listings derived from
`list_all_projects()`, so a mission whose project record was missing or
unparseable loaded into the missions map but was invisible to every global
listing — never bootstrapped into the active index, so no startup/tick
rollover, no event/webhook dispatch, and `threads_today_date` pinned at its
creation date. Both `list_all_missions_global` and
`list_all_active_missions_global` now enumerate the missions map directly
(the same semantics the engine test store already had). New libsql
regression `global_mission_listings_see_missions_whose_project_file_is_missing`;
full engine mission suite passes. The ENGINE-V2 scheduling contract is
updated. Live dwarfstar acceptance (inspecting persisted `mission.json`
after a day rollover) remains an operator step.

### `routine_history` Engine V2 Mission Fallback (WI#265)

Legacy `routine_history` could not resolve Engine V2 mission names — the
v2.1.3.0 known issue. The tool now falls back to a read-only bridge query
when the v1 routines lookup misses:

- `ic/src/bridge/router.rs` exports
  `get_engine_mission_history_by_name` — default-project own+shared lookup,
  then a global ownership-filtered scan — rendering recent fires from
  `mission.thread_history`.
- `ic/src/tools/builtin/routine.rs` uses it as the fallback, with
  not-found errors naming both registries and pointing at
  `routine_list`/`mission_list`; limit clamped 1..=50; schema canary synced.

Both !1256 review findings are addressed: run summaries truncate by
characters (`truncate_summary_chars`), never byte-indexing `String::truncate`
(the byte form panicked on multilingual assistant text — regressions cover
CJK, a 199-ASCII+`é` boundary, and 4-byte emoji), and the fallback test
takes the shared `ENGINE_STATE_TEST_LOCK` and resets state explicitly so it
cannot race the router's engine-state tests. 90 router tests, 26 routine
tool tests, 250 bridge tests, 1167 tools tests, and the e2e
`routine_history` trace test pass. ENGINE-V2.md's Learning Missions section
documents the behavior.

### Voice Sidecar Inference Admission and Recovery (WI#264)

The deferred defect from v2.1.3.0 — a timed-out Kokoro synthesis retaining
the sidecar's inference permit and TTS mutex — is fixed with component-first
admission:

- STT/TTS/VAD each admit one call before taking shared capacity; blocking
  workers retain both permits until settlement, so repeated TTS retries
  cannot queue behind the model mutex or consume remaining STT/VAD capacity.
- Caller cancellation/timeout and worker settlement share a phase lock.
- `/ready` and `/metrics` report per-component activity, current/cumulative
  abandonment, and permanent failure.
- `WouldBlock` remains transient Busy; poisoned model locks and panics
  return explicit `component_failed`/503 and keep readiness degraded until
  restart. The supported recovery procedure is the whole-tenant
  `restart-tenant` verb (documented with its whole-tenant impact); a
  never-returning native call still retains one permit and requires
  operator restart.

Both !1263 review findings are addressed (transient-vs-permanent failure
typing; the regression holds the actual production runner's component mutex
during panic, both with a waiting caller and after cancellation). 23 sidecar
tests pass in each feature configuration, including six new regressions:
repeated timeouts, cancellation, capacity isolation, normal/error drain
recovery, lock contention/poison, and panic/HTTP status/readiness.
Integration notes: !1275 was selected over the parallel !1278 after
comparing complete diffs — !1275 contains all four !1278 API regressions
plus the concurrent-settlement and contention/pre-poisoned-lock tests.

### Per-Tenant System-Prompt Management (WI#220)

The deferred per-tenant system-prompt editing work item landed: Settings →
System Prompt provides primary-scope source editing, missing-file creation,
draft preservation on failed saves, and a saved workspace preview with
group/timezone controls. Authenticated `/api/prompts` routes retain
workspace write validation/indexing and scope isolation (tenant/read-scope
isolation, rejected writes, group/timezone composition, initialized owner
bootstrap state). The Engine V2 consumption model is an initialization
snapshot: activation requires `restart-tenant` followed by a new
conversation; existing threads retain their System message. Operator guide:
[TENANT-SYSTEM-PROMPTS.md](docs/guides/TENANT-SYSTEM-PROMPTS.md).

### Tool Runtime EMA Estimates (WI#191, first slice)

Bounded, per-user EMA learning is wired into shared tool execution, local
jobs, and routines; frozen advisory runtime hints appear on both v1
approval paths and the v2 pending-gate insertion path:

- Cold-start in-memory learning with a five-sample warmup, optional actual
  cost, unadjusted-baseline ratios, frozen forecast errors, numeric guards,
  and bounded outlier influence. Failures, timeouts, cancellation, and
  approval/auth waits do not train completed runtime.
- `LUNARWING_TOOL_ESTIMATES` is read once per registry at startup: unset or
  `observe` records only; `1` records and shows hints after warmup; `0`
  disables recording and new hints. Restart resets learned models;
  persisted pending gates retain their original description, including any
  hint.
- Legacy v1/v2 recovery no longer falls back to raw parameters when a
  redacted snapshot is missing — `null` is rendered instead.

No database migration or new wire fields are required. Verified with 114
targeted tests plus the overhead benchmark (twelve-thread helper p95
0.260 ms); PostgreSQL test-target and libSQL library compile checks and
all-feature Clippy pass. LLM timing, whole-job/mission ETA, gate-wait
analytics, and durable aggregate caches remain later slices of the
integration plan. Evidence and rollout limits:
[v2.1.4.0_GOAL10_EMA_VALIDATION.md](docs/ops/v2.1.4.0_GOAL10_EMA_VALIDATION.md).

### Engine V2 Prompt-Overlay Audit and Read-Only Diagnostics (WI#268)

mt-admin gained two read-only diagnostics verbs: `psql <tenant>`
(container-based, init-agnostic tenant database access) and
`daemon-pid <tenant>` (strict PID-record validation with tenant
association — malformed records are rejected, not converted into a PID).
On top of them sits the WI#268 operator runbook for auditing database-backed
Engine V2 prompt overlays — determining whether an agent's effective system
prompt differs from the shipped prompt without a Git diff and without
enabling self-modification. Both !1261 review P2s are fixed on the tooling
itself: a failed environment capture reports `uninspected (live gate)` with
a non-zero exit rather than certifying "gate off" (the old pipeline's
failed `cat /proc/$pid/environ` collapsed into "absent"), and the PID is
validated as-written against the tenant's record before any association
claim. No intermediate environment reaches a group/world-readable log;
values are reduced to presence/absence before printing. A regression pins
that `prompt_additions` writes nothing while the self-modify gate is off.
The !1276 review's daemon-PID findings (as-written validation, tenant
association, fixture cleanup) are addressed in the same pass.

### Crate Security Audit and Supply Chain (item 3)

The rejected !1264 attempt was superseded by a rebuild on the merged !1262
consolidation, addressing every review finding:

- All 27 tracked Cargo lockfile roots scanned with an **empty advisory
  ignore list** and `unmaintained = "all"` against RustSec db `9b3a3b73a`
  (2026-09-30) — the policy itself keeps `unmaintained = "workspace"` plus
  the eight documented ignores.
- The actual pinned worker-image graph scanned patched vs unpatched (the
  control trip on RUSTSEC-2026-0285 proves the scan resolves the real
  graph, not the committed lock text).
- Every accepted-advisory upstream blocker re-verified against live
  crates.io metadata; a new full-repo yanked-crate pass covered every
  registry-locked version in every lockfile.
- Fixed this cycle: the single yanked crate anywhere in the tree
  (chacha20 0.10.1, inherited from upstream Pebble's own lock) is retired —
  the override now pins 0.10.2 with a refreshed `PEBBLE_LOCK_SHA256`; the
  Pebble lock guard keys multi-version records by `(name, version,
  source)`, rejects duplicates, compares complete records, and now fails
  when nothing changed (so the override is retired once upstream ships its
  own fixed lock); multi-version record-edit negative tests cover the
  reviewer's mutated `cpufeatures 0.2.17` checksum probe.
- A live `podman build` proves the digest gate and allowlist run inside
  the image build (compiling rustls 0.23.45).

All CI-side gates green locally (ci-config validator, 29-test `.gitlab/tests`
suite, full `cargo deny check`, lockfile-only guard, mt-admin pebble
regression, shellcheck). CI-only confirmations (remote pipeline, trivy scan
of the published candidate) remain, as with every release. Evidence:
[v2.1.4.0_CRATE_SECURITY_AUDIT.md](docs/ops/v2.1.4.0_CRATE_SECURITY_AUDIT.md).

### CI Recovery and Validation Coverage (item 1)

Pipeline #4679's failures were diagnosed and fixed without weakening any
gate:

- `cavepony-wasm`: registry version `0.4.3` disagreed with the CI
  expectation `0.4.2` (Cargo and capabilities also at `0.4.2`). The
  `version-policy` job later rejected `0.4.3` again because the MR base
  already carried that registry release — all five release version fields
  now advance together to `0.4.4`, verified against the actual MR base,
  with a shell regression covering metadata-only alignment and MR-base
  precedence.
- `architecture-boundaries`: the new orphan-mission regression imported a
  concrete libSQL backend outside the DB/testing boundary; it now uses the
  existing migrated `crate::testing::test_db()` fixture with no boundary
  exclusions.
- Full validation now covers integration-target MRs (previously only
  default-branch targets), so a green MR can no longer hide a red
  integration. Scratch branches opt in with
  `LUNARWING_CI_FULL_VALIDATION=1`; worker publication stays protected and
  push-only, and validation caches stay pull-only.

Locked builds, the fuel budget, required artifacts, and the unchanged
boundary ratchet are all retained. Evidence:
[v2.1.4.0_GOAL1_CI_RECOVERY.md](docs/ops/v2.1.4.0_GOAL1_CI_RECOVERY.md).

### Pairing Allowlist Preservation (sbpatch1)

`PairingStore` approval opened the allowlist store with `truncate(true)`
before deserializing, so any failed read silently removed every previously
approved sender before the new entry was written. The store now reads
under the lock with `truncate(false)`, and the rewrite goes through the
same locked handle (`set_len(0)` + seek + write + `sync_all`), so
concurrent approvers cannot bypass the lock and shorter payloads cannot
leave trailing bytes behind. This matches the documented "approved pairing
codes" semantics the stopgap assumed.

### Orchestrator Self-Modification for New Tenants (WI#282)

`write_tenant_lunarwing_env` now renders `ORCHESTRATOR_SELF_MODIFY=true`
for new tenants, adopting stored orchestrator and prompt-overlay updates by
default. The value is preserved across upgrade/rebuild/re-render: an
explicit operator opt-out (`false`) is never clobbered, and existing
tenants are not rewritten merely by upgrading. The WI#268 audit runbook
complements this with the inspection procedure for tenants that keep the
gate off.

### Outbound Message Delivery Honesty (WI#270)

The `message` tool's success ack was an enqueue ack dressed as a delivery
receipt: channels accept messages into in-memory outbound queues, and the
XMPP queue is dropped on restart. That contract gap is what let scheduled
mission PR Digest #59 record "Delivered via XMPP … Goal achieved" in memory
while the queued stanza was silently lost in a rebuild window. Three
hardening changes landed, following the fix ranking accepted on the work
item:

- The tool ack is now honest — `Queued message to <channel>:<target>` —
  and the tool description warns that success means acceptance for
  delivery, never "delivered".
- The XMPP channel refuses proactive enqueue while no session is bound
  (`XMPP is not connected (no bound session); message not queued`) instead
  of accepting into a doomed queue, so the failure surfaces to the caller.
- `shutdown()` gives the client task a bounded 2s window to drain already
  acked stanzas (pending-counter driven, mirroring the coalesce flush) and
  WARNs with the pending depth when it cannot, instead of dropping
  silently.

The WIT channel interface text is intentionally unchanged — its `Ok`
semantics are spelled out in the tool description and the new architecture
doc instead of a comment-only WIT edit that would force a package version
bump and registry-wide churn. Hard-exit loss remains
inherently possible with a memory-only queue; the durable outbound spool +
mission-run ledger stays tracked as the long-term fix (WI#270 fix #4,
platform halves in #277/#276). New architecture doc:
`docs/architecture/OUTBOUND-DELIVERY-SEMANTICS.md` also records the
mission-spec discipline (daily log → send → memory records acceptance;
update the run baseline each time). The PR Digest mission spec itself is
tenant workspace data, outside this repo.

### Version Bumps and Supply Chain (item 1)

All eleven LunarWing-owned release packages moved `2.1.3` → `2.1.4`: the
daemon, the five workspace crates (`lunarwing_common`, `lunarwing_engine`,
`lunarwing_skills`, `lunarwing_safety`, `lunarwing_health_metrics`), the
XMPP bridge, and the four WASM channels. Channel registry versions advanced
(darkirc 0.1.10, multica 0.1.13, weechat 0.2.11, xmpp 0.1.16) and all nine
tool registry entries advanced together; lockfiles were refreshed and the
tracked manifest/lock audit confirmed no project-owned package remains at
2.1.3. 121 external crates took semver-compatible updates across the ic
workspace and the repo-root satellite lockfile roots; `ic/vendor/` was
regenerated. Channel behavior is unchanged — the bumps are rebuild-only.

### External Workers: opencode2 v2.0.20 (item 2)

The opencode2 lane advances v2.0.18 → v2.0.20 at reviewed full commit
`84c9be93a56304a108f1a22df0c5d62c26d5b6ca` (tag SHA verified live against
the anomalyco/opencode remote). Dockerfile pins, CI source_ref/build args,
and the reproducible-build test fixture moved in step. Verified: the
upstream `build.ts` at the new SHA still has exactly one `bytecode: true` /
`splitting: true` (patch contract holds); the bridge surface is unchanged
(`OpenCode.make`, `session.create` with `permissions`,
`session.permission.reply`, `session.interrupt`, `event.subscribe`, all
matched event names); the diff is additive — new `credential.list/create`
endpoints are unused. Host-side reproducible-build preparation 3/3 OK.
Whole-image reproducibility must still pass the CI candidate-vs-clean
rebuild digest gate before promotion (CI-only, as with prior bumps). kimi
remains at 2.1.1, opencode v1 at `v1.18.25`, and pebble at `2d0f3786…`.

### Documentation Parity (item 12)

Seven files were re-verified against the tree for drift not already covered
by each goal's own doc updates: `docs/ops/MULTITENANCY-PRODUCTION.md`
(mt-admin command reference, stale worker list), `docs/guides/MT-ADMIN-QUICKSTART.md`
(WI#268 diagnostics tier with the uninspected-on-failure rule),
`docs/ops/TENANT-CONFIGURATION.md` (`LUNARWING_TOOL_ESTIMATES`),
`docs/guides/TESTING_GUIDE.md` (four v2.1.4.0 manual repass tiers),
`docs/releases/README.md` (indexed the shipped v2.1.3.0 notes),
`docs/README.md` (indexed TENANT-SYSTEM-PROMPTS.md), and
`docs/guides/CONTRIBUTOR-ONBOARDING.md` (freeze status). Checked and
intentionally left alone: ENGINE-V2.md, GATEWAY_VOICE_API.md + voice-sidecar
README, TENANT-SYSTEM-PROMPTS.md, estimation/tools READMEs,
WORKER-CONTAINERS.md (each already updated in-branch by its goal).

### Pairing Secrets-Store Design Revision 3 (item 11, WI#184)

The rejected !1294 attempt is superseded by design revision 3 at
[PAIRING_SECRETS_STORE_DM_AUTH.md](docs/proposals/PAIRING_SECRETS_STORE_DM_AUTH.md),
which answers both P2 findings from the review:

- **Legacy matching contract.** The rejected revision promised byte-exact
  digest lookup for every query while also claiming byte-verbatim legacy
  migration preserves today's matching — contradictory (a legacy `@Alice`
  entry would never be found by an id- or username-derived lookup).
  Revision 3 defines two explicit source-tagged matchers: a strict
  byte-exact canonical-principal lookup (equivalent to today's id
  comparison) and a legacy matcher applying today's exact comparison
  function **only** to `kind:"legacy"` grants — no cross-kind broadening in
  either direction.
- **Name privacy.** Unkeyed SHA-256 names are replaced with
  `hex(hmac_sha256(name_key, "lunarwing-pairing-name-v1"|channel|0x00|principal))[0..10]`
  where `name_key` is an HKDF-SHA256 master-key subkey domain-separated
  from the per-secret encryption subkey — database-read-only dictionary
  attacks against candidate IRC principals/JIDs no longer terminate without
  the master key. Key lifecycle equals master-key lifecycle (no independent
  rotation exists today; the re-key contract is stated).
- **Hot path (nonblocking fix).** The "decrypt-per-grant enumeration is
  operator-only" claim is withdrawn — Signal reads allowFrom per message,
  weechat per DM, web per activation render. Revision 3 specifies a bounded
  decoded-grant cache per channel: 15 s TTL, synchronous invalidation on
  daemon-side approve, revoke-latency-bounded-by-TTL documented for
  operators, 1024-grant bound with uncached fallback.

The regressions the review required are landed executable tests ahead of
the substrate change, so the current semantics cannot silently drift before
the migration lands: `test_legacy_username_entry_matches_by_username`,
`test_legacy_trimmed_id_entry_matches_exact_id`, and
`test_concurrent_add_allow_from_preserves_all_grants` (8 concurrent
approvers) in `ic/src/pairing/store.rs`, plus
`test_pairing_host_functions_preserve_legacy_matching` exercising the
`ChannelStoreData` `Host::pairing_is_allowed` path in
`ic/src/channels/wasm/wrapper.rs` against a legacy-shaped allowFrom file.
Verified: 27/27 pairing tests and 77/77 wasm wrapper tests pass; lib
Clippy with `--all-features --tests -D warnings` and rustfmt clean. This is
design-only — the secrets-store substrate implementation remains future
work as the WI intends.

### Open Work Items Audit (item 14)

All 23 open tracker items were fetched live from the GitLab API and
reconciled in [v2.1.4.0_OPEN_WORK_ITEMS_STATUS.md](docs/ops/v2.1.4.0_OPEN_WORK_ITEMS_STATUS.md)
following the v2.1.3.0 audit format. Key findings, each verified against
the tree at `03f5650fe` rather than taken from tracker bodies:

- 5 items carry the 2.1.4.0 label genuinely open (#281 engine preamble
  `mission_*` advertisement, #280 daily-log injection-scanner
  false-positives, #270 PR Digest false "delivered" memory record, #269
  missing `mission_history` tool, #271 inttest/infra investigation);
  #184 records the !1294 rejection with both P2 design findings quoted.
- 15 items are deferred to 2.1.5.0+ by label, several carrying
  source-verified root-cause maps.
- #198/#199 are empty-bodied but have closeable evidence in this cycle's
  audit docs.
- Tracker churn since the 2026-09-27 audit: 24 items created-and-closed,
  #220/#200 closed, open set grew 13 → 23.

### Guides and Architecture Docs Repass (items 16/17)

All 32 active guides under `docs/guides/` (excluding `history/`) and all 10
active docs under `docs/architecture/` were re-verified against current
source and stamped for v2.1.4.0:

- Guides: substantive corrections beyond stamps — the gotify-wasm README's
  deprecated-path mention now resolves to `ic/scripts/setup-instance.sh`;
  the multica/darkirc/weechat stamps record the 2.1.4.0 version moves
  (registry multica 0.1.13 / multica-bridge 0.1.3, darkirc 0.1.10, weechat
  0.2.11; WASM channel crates now at workspace `2.1.4`); distro package
  lists note the WI#268 `psql`/`daemon-pid` verbs add no host packages;
  GATEWAY_VOICE_API.md cites the speed-scaled pre-flight.
- Architecture: ENGINE-V2.md header revalidated for the cycle's engine work
  (WI#243/#244/#265/#268/#282); SEMANTIC-MEMORY-SEARCH.md confirms V27
  remains latest; IRC-SENDER-IDENTITY/WEECHAT docs re-verify principal
  formats in `ic/channels-src/{darkirc,weechat}/src/lib.rs`; XMPP transfer
  docs note registry 0.1.16 / bridge crate 2.1.4 with no protocol change;
  SELF_HEAL_COVERAGE_AUDIT re-listed live at 19 suites unchanged.
- Link checks: every relative link and cited `ic/`/`projects/` path in the
  active guides and architecture docs resolves.

## Upgrade Notes

1. Back up PostgreSQL, tenant environment files, `/etc/lunarwing/ports.json`,
   workspace state, rendered units, worker data, voice models, and channel
   state.
2. No SQL migration is introduced; `V27` remains latest. The port registry
   stays at v17 — no registry migration on upgrade.
3. Rebuild the daemon and installed WASM extensions through the normal
   deployment path. All four channel registry versions changed (darkirc
   0.1.10, multica 0.1.13, weechat 0.2.11, xmpp 0.1.16) — behavior is
   unchanged, but the channels must be rebuilt/reinstalled. All nine tool
   registry versions advanced; rebuild/reinstall them together.
4. **Vendor snapshot.** `ic/vendor/` was regenerated for the 121 external
   crate updates; if you maintain a local vendor override, refresh it with
   `ic/scripts/refresh-vendor.sh` before building.
5. **opencode2 rebuild.** Rebuild the opencode2 worker image at `v2.0.20`
   (`84c9be93a…`); pull worker images by the digest recorded in
   `worker-image-evidence.json`. Promotion still requires the CI
   candidate-vs-clean rebuild digest gate.
6. **Pebble worker.** The reviewed upstream lock re-pins chacha20 to 0.10.2
   (retiring the yanked 0.10.1); the refreshed `PEBBLE_LOCK_SHA256` gate
   verifies it during build. No operator action beyond the normal image
   rebuild.
7. **Tool estimates.** `LUNARWING_TOOL_ESTIMATES` is read once at startup
   — set `1` (per-tenant env or render) to surface runtime hints after
   warmup; unset keeps observation-only recording; `0` disables both.
   Learned models reset on restart; no migration is needed.
8. **Tenant system prompts (WI#220).** Editing a prompt in Settings writes
   the workspace file; Engine V2 consumes it as an initialization snapshot.
   After editing, run `restart-tenant` and start a new conversation to
   activate; existing threads keep their existing System message.
9. **New-tenant self-modification (WI#282).** New tenants render
   `ORCHESTRATOR_SELF_MODIFY=true`. Existing tenants are unchanged; set the
   variable explicitly to opt in or out, and re-renders preserve it.
10. **Voice sidecar (WI#264).** Deploy the updated sidecar image for
    bounded admission and per-component readiness. `component_failed`/503
    with degraded readiness indicates a poisoned lock or native panic —
    recover with the documented whole-tenant `restart-tenant` procedure.
11. **Provider recovery (WI#244).** No configuration change; recovery is
    bounded by the existing retry caps. Threads that previously aborted on
    transient provider failures now retry within those caps.
12. **Read-only diagnostics (WI#268).** `psql`/`daemon-pid` are available
    through the deployed mt-admin; treat them as read-only operator
    discipline (not enforced). The prompt-overlay audit runbook lives in
    [v2.1.4.0_WORK_ITEM_268_PROMPT_OVERLAY_AUDIT.md](docs/ops/v2.1.4.0_WORK_ITEM_268_PROMPT_OVERLAY_AUDIT.md).
13. The bundled-skills catalog is unchanged from v2.1.3.0 (exactly seven
    product skills); no skill migration is needed.
14. **Pairing (WI#184).** No runtime change this release — revision 3 is a
    design document with landed pinning tests. Existing per-channel pairing
    JSON files keep working; the secrets-store migration remains future work
    tracked in the WI.

### Security, Deployment, and Release Limitations (carried)

- DarkIRC Secure Key Exchange Stages B–D and automated QR/web/trusted-channel
  delivery remain disabled behind separate acceptance gates (carried
  unchanged).
- MCP does not auto-install npm/pip packages. Core MCP configuration is not
  automatically inherited by external workers (carried).
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
  pending upstream proof (carried; the opencode2 lane is on v2.0.20).
- Historical WI#159 behavior remains: terminal triggers
  (`thread_completed_with_issues`/`thread_completed_with_learnings`) still
  fire for Research child threads — only Mission threads are filtered
  (carried).
- CI-only confirmations that need GitLab/registry visibility remain, as
  with every release: the corrected-head full remote pipeline for item 1,
  the trivy scan of the published worker candidate, and the opencode2
  whole-image reproducibility digest gate.
- Live-tenant acceptance steps that remain operator-side: dwarfstar
  mission-day-rollover inspection (WI#243), engine mission-history
  fallback on a live tenant (WI#265 — covered by e2e traces), and
  browser/live-tenant rollout of the tool-estimate hints (WI#191).
- Item 11 (WI#184 secrets-store pairing redesign) landed as design revision
  3 with the review-required regressions; the secrets-store substrate
  implementation itself remains future work — the WI stays open for the
  migration. The WI#200 stopgap tracker item was closed 2026-10-01 on the
  landed allowFrom-preservation fix (!1283).
- WI#191 later slices (LLM timing, whole-job/mission ETA, gate-wait
  analytics, durable aggregate caches) are deferred beyond this release.
- Repo housecleaning (item 28) is tracked separately in the goals document
  and may land after this draft is finalized; the docs/guides (item 16) and
  docs/architecture (item 17) repasses landed after the first draft and are
  covered in the sections above.

### Resolved or Source-Fixed Since v2.1.3.0

- Bounded recovery for transient/unavailable provider failures after
  partial text; failed drafts never enter history or tool execution
  (WI#244 source-fixed; the v2.1.3.0 known issue is closed).
- Missions whose project record is missing/unparseable remain globally
  enumerable, keep rollover/event dispatch, and no longer pin
  `threads_today_date` (WI#243 source-fixed).
- `routine_history` resolves Engine V2 mission names with character-safe
  summaries and registry-pointing errors (WI#265 source-fixed; the
  v2.1.3.0 known issue is closed).
- The voice sidecar bounds abandoned inference admission with per-component
  readiness and explicit permanent-failure semantics; supported recovery
  via `restart-tenant` (WI#264 source-fixed; the v2.1.3.0 known issue is
  closed).
- Per-tenant system-prompt editing shipped with authenticated routes,
  scope isolation, and Engine V2 initialization-snapshot semantics
  (WI#220 source-fixed; the v2.1.3.0 deferral is closed).
- The WI#191 EMA tool-runtime slice is live: bounded learning across
  shared execution/local jobs/routines, frozen advisory hints on approvals
  and pending gates, opt-in display, and `null` for missing redacted
  snapshots.
- mt-admin `psql`/`daemon-pid` read-only diagnostics plus the WI#268
  prompt-overlay audit runbook with uninspected-on-failure semantics and
  as-written PID validation.
- Approving a pairing sender no longer truncates the existing allowlist;
  rewrites are locked, seek-corrected, and synced.
- New tenants default to `ORCHESTRATOR_SELF_MODIFY=true` with the operator
  opt-out preserved.
- All LunarWing-owned packages at `2.1.4`; no project-owned package remains
  at 2.1.3 (tracked manifest/lock audit). Cavepony release metadata aligned
  at 0.4.4 across all five version fields with MR-base precedence
  regression coverage.
- The crate security audit re-verified every advisory ignore, scanned the
  resolved worker-image graph, and retired the tree's single yanked crate
  (chacha20 0.10.1 → pinned 0.10.2); the Pebble lock guard gained
  multi-version keying and no-op-override rejection.
- Integration-target MRs now run the full validation matrix; CI recovered
  from #4679 without bypassing any gate.
- opencode2 worker at v2.0.20 (reviewed commit, verified patch contract and
  bridge surface); kimi 2.1.1, opencode v1, and pebble pins unchanged.
- Documentation parity for the cycle: seven docs synced, four manual test
  tiers added, releases/docs indexes updated.
- All 32 active guides and all 10 active architecture docs repassed for
  v2.1.4.0 with every cited path verified against source.
- WI#184 pairing design advanced to revision 3 with both !1294 review P2s
  answered; the legacy-matching, concurrent-grant, and wasm host-path
  regressions are landed executable tests ahead of the substrate change.
- The cycle's open work items were reconciled in
  `docs/ops/v2.1.4.0_OPEN_WORK_ITEMS_STATUS.md` with tree-verified
  dispositions for all 23 open items; WI#200 closed on the landed stopgap.

---

*Draft status: codename, release date, and pipeline numbers for final CI
confirmation will be filled in before the release is cut. This is the first
repass (item 27, 2026-10-01): it folds in the work merged after the first
draft — WI#184 design revision 3 with its landed regressions (item 11), the
open work items audit (item 14), and the guides/architecture docs repasses
(items 16/17). Open goal items at repass time (18–20, 22, 24, 25, 28, 29)
may add sections; this draft covers the merged state as of 2026-10-01.*
