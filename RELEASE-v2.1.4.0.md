# Release Notes for LunarWing v2.1.4.0

## Codename: Kabuto (兜): The Helmet

**Release date:** 2026-10-03

> Beneath the helmet
> the head that planned every plate
> armor remembers. 兜

After Yoroi laced the plates and Ō-Yoroi closed the box, **Kabuto crowns the
suit**: the helmet protects the mind behind the armor

## Overview

LunarWing v2.1.4.0 is a recovery, dependency-audit, and engine-reliability
release – the cycle that repaired a red integration pipeline, put every
tracked lockfile and worker image through a full crate-security re-audit,
closed four defects deferred from v2.1.3.0 (mission-history lookup, voice
inference admission, mid-stream provider recovery, and tenant system-prompt
editing), fixed the global mission-listing defect found in-cycle, and landed
the first slice of EMA estimation wiring. The secrets-store pairing redesign
remains at design revision 3.

The cycle opened with the version sweep: all LunarWing-owned release packages
moved from `2.1.3` to `2.1.4` (daemon, five workspace crates, XMPP bridge,
four WASM channels), channel and tool registry versions advanced together, 121
external crates took semver-compatible updates across the workspace and
satellites, `ic/vendor/` was regenerated, and the tracked manifest/lock audit
confirmed no project-owned package remained at 2.1.3. The external-crate
effort doubles as the security audit: every advisory ignore in `ic/deny.toml`
was re-verified, the resolved worker-image graph was scanned
patched-vs-unpatched, a full-repo yanked-crate pass retired the single yanked
crate found anywhere in the tree (chacha20 0.10.1 in upstream Pebble's lock,
now pinned 0.10.2 with a refreshed `PEBBLE_LOCK_SHA256`), and the Pebble lock
guard gained multi-version record keying and no-op-override rejection.
A later look at the green pipeline 4811 found the cargo-deny yanked-crate
gate silently degrading to warnings under vendored source replacement; the
job now runs unvendored from the repo root and hard-fails on any
index-failure diagnostic.

Engine reliability dominates the feature work: mission execution recovers from
transient provider failures after partial text instead of aborting the thread,
global mission listings no longer drop missions whose project record is
missing so every active mission keeps its daily rollover, `routine_history`
resolves Engine V2 mission names, a dedicated `mission_history` tool joins the
bridge `mission_*` family with `routine_history` honestly blocked in v2
threads, and the voice sidecar bounds abandoned inference admission so a
timed-out or cancelled call cannot pin the model mutex or starve other
components. Shared tool execution learns bounded per-user EMA runtime
estimates surfaced as frozen advisory hints on approvals and pending gates
(first slice, opt-in via `LUNARWING_TOOL_ESTIMATES=1`). The workspace
prompt-injection write scan no longer false-positives on agent-authored daily
logs that merely quote injection-shaped security text: a High-severity match
fully enclosed in a short, single-line, inline quoted span demotes to
warn-only describing context, while unquoted and tokenizer-level matches still
reject. The curated-memory write contract closed its remaining gaps in the
same pass: the `append_memory` lane behind `memory_write(target: "memory",
append: true)` no longer bypasses the injection scan, every `memory_write`
lane rejects payloads over 256 KiB, and full-replace writes of prompt-visible
files keep a rolling shadow backup so an accidental clobber of curated memory
is recoverable in-place.

Late-cycle hardening carries the provider's real finish reason through
blocking and streaming responses instead of inventing a successful stop, and
quarantines pre-existing poisoned daily logs so a retry can resume the day's
entries. Wasmtime/WASI advances to 36.0.17 for three new RustSec advisories;
vendor refresh now restores the committed rig-core finish-reason patch
automatically for same-version refreshes.

The operator surface gained per-tenant system-prompt editing in Settings with
Engine V2 initialization snapshots, read-only tenant database and daemon-PID
diagnostics verbs in mt-admin (`psql`, `daemon-pid`) with a prompt-overlay
audit runbook, worker-image store-failure surfacing with quoted stderr and
per-lane image presence in `mt-admin status`, an opencode2 worker bump to
v2.0.21, and a pairing allowlist fix so approving a new sender no longer
erases the existing allowlist. New tenants now render
`ORCHESTRATOR_SELF_MODIFY=true` (stored orchestrator and prompt-overlay
updates), with the operator opt-out preserved across re-renders. The
secrets-store pairing redesign advanced to design revision 3 (both !1294
review P2s answered) with the review-required legacy-matching and concurrency
regressions landed ahead of the substrate change. Outbound delivery is now
honest about its contract: the `message` tool ack reads `Queued …` instead of
implying delivery, the XMPP channel refuses enqueue while no session is bound,
and shutdown gives the outbound queue a bounded drain window – and the XMPP
lane now persists every outbound message to a durable spool before acking, so
a restart or crash between ack and send no longer silently loses the message.

The full docs set was repassed for the cycle: all 32 active guides under
`docs/guides/` and all 10 active architecture docs re-verified against source
and stamped, the cycle's open work items were reconciled in a tracker audit
(initially 23 open items; the latest recorded audit covers 25 at 18:10 UTC on
October 2), Kabuto was selected from the codename shortlist, and stale
versioned ops docs were archived to `docs/ops/history/` with every inbound
link repaired.

The release identifier is `2.1.4.0`; LunarWing-owned Rust crates and channel
packages use `2.1.4`. Rust 2024 and MSRV `1.96.1` are unchanged. There are no
new SQL migrations (`V27__memory_index_outbox.sql` remains latest). The
multi-tenant port registry stays at v17.

See the [v2.1.3.0 release notes](docs/releases/RELEASE-v2.1.3.0.md) for the
previous compatibility baseline.

## Highlights

- **Provider finish reasons preserved.** Blocking responses and the deployed
  OpenAI-compatible streaming path now carry the actual
  stop/length/content-filter/tool-use metadata. Missing reasons remain
  `Unknown`; the deployed proxy no longer fabricates `stop`. The lightweight
  routine consumer can classify truncation correctly; an Engine V2
  finish-reason-driven recovery ladder is not part of this release.
- **Daily-log quarantine and retry.** A clean append encountering pre-existing
  poisoned daily content preserves it at `daily/YYYY-MM-DD.poisoned-<n>.md`,
  clears the day file, and rejects with a retry hint. The next clean append
  can succeed; rejection details now include offending line numbers. New
  poison and split-boundary patterns still reject without quarantining the
  file.
- **Wasmtime security and repeatable vendor refresh.** Wasmtime/WASI 36.0.17
  clears RUSTSEC-2026-0321/0322/0323 without new advisory ignores.
  Same-version vendor refreshes restore the committed rig-core patch and
  checksums; uncommitted patch edits or a rig-core version change require
  explicit maintainer action.
- **Recoverable mid-stream provider failures.** Mission executor turns that
  lose the provider after partial text now attempt bounded recovery for
  transient/unavailable failures instead of aborting the whole thread. Failed
  partial responses stay out of history and tool execution; existing retry caps and
  rate-limit exclusions remain enforced. Attempts accumulate as provisional
  Live work with no per-retry reset. All 263 executor tests and the 8
  `engine_v2_tensorzero_streaming` integration tests pass, including
  regressions that fail against the old implementation (interrupted-tool
  execution isolation, exhausted recovery, safe recovery context,
  successful-response-only persistence)
- **Global mission listings see every mission.** `HybridStore` never overrode
  `list_all_missions_global`/`list_all_active_missions_global`, so both
  inherited a trait default derived from `list_all_projects()`. A mission
  whose `project.json` was missing or unparseable loaded into the missions map
  but was invisible to every global listing – never bootstrapped into the
  active index, no startup/tick rollover, no event/webhook dispatch, and
  `threads_today_date` pinned at its creation date. Both globals now enumerate
  the missions map directly. Regression:
  `global_mission_listings_see_missions_whose_project_file_is_missing`
- **`routine_history` resolves Engine V2 mission names.** When the v1 routines
  lookup misses, the tool falls back to a read-only bridge query
  (`get_engine_mission_history_by_name`: default-project own+shared lookup,
  then a global ownership-filtered scan) rendering recent fires from
  `mission.thread_history`. Run summaries truncate by characters – never
  byte-indexing a `String` – so multilingual assistant text cannot panic the
  path. Not-found errors name both registries and point at
  `routine_list`/`mission_list`; limit clamped 1..=50
- **Dedicated `mission_history` tool.** Engine V2 missions gain a first-class
  history tool in the bridge `mission_*` family, rendering newest-first fires
  from `mission.thread_history` in the same shape as `routine_history`
  (character-safe summaries, limit clamped 1..=50, default 10) through a new
  `MissionManager::load_thread` read accessor. Name lookup is project-scoped
  own+shared like `mission_list`; the UUID path re-checks project and
  ownership after the store load and returns `access denied` otherwise.
  `routine_history` is now blocked in v2 threads with an honest "use
  mission_history" error instead of a wrong-registry fallthrough
- **Bounded inference admission in the voice sidecar.** STT/TTS/VAD each admit
  one call before taking shared capacity; blocking workers retain both permits
  until settlement, so repeated TTS retries cannot queue behind the model
  mutex or consume remaining STT/VAD capacity. Caller cancellation/timeout and
  worker settlement share a phase lock; `/ready` and `/metrics` report
  per-component activity, current/cumulative abandonment, and permanent
  failure. Poisoned model locks and panics return explicit
  `component_failed`/503 and keep readiness degraded until restart (supported
  whole-tenant recovery: `restart-tenant`). 23 sidecar tests per feature
  configuration, including six new regressions
- **Per-tenant system-prompt editing.** Settings → System Prompt provides
  primary-scope source editing, missing-file creation, unsaved-edit preservation, and
  a saved workspace preview with group/timezone controls. Authenticated
  `/api/prompts` routes retain workspace write validation/indexing and scope
  isolation. Engine V2 consumes the prompt as an initialization snapshot:
  activation requires `restart-tenant` followed by a new conversation;
  existing threads keep their System message. Five authenticated router
  regressions and five Chromium browser regressions
- **Tool runtime estimates with bounded EMA learning (first slice).**
  Registry-owned, user/tool-scoped, in-memory calibration feeds the existing
  estimator from real tool completions across shared execution, local jobs,
  and lightweight routines (five-sample warmup, bounded outliers;
  failures/timeouts/cancellation/approval waits do not train). Frozen advisory
  runtime hints appear on both v1 approval paths and the v2 pending-gate
  insertion path. Display is opt-in (`LUNARWING_TOOL_ESTIMATES=1`; unset
  records only, `0` disables). Missing redacted parameter snapshots now render
  `null` on both display paths instead of exposing execution parameters. 114
  targeted tests; helper p95 overhead 0.260 ms
- **mt-admin read-only diagnostics and the prompt-overlay audit.** New verbs
  `psql <tenant>` (init-agnostic, container-based read-only DB access) and
  `daemon-pid <tenant>` (strict PID-record validation with tenant association
  – malformed records are rejected, not converted into a PID). The audit
  runbook lets an operator determine whether an agent's effective system
  prompt differs from the shipped prompt without a Git diff and without
  enabling self-modification; a failed environment capture reports
  `uninspected (live gate)` with a non-zero exit rather than certifying "gate
  off"
- **mt-admin worker-image store surfacing.** The `podman save | podman load`
  image distribution now captures both halves' stderr and quotes the first
  lines in the WARNING, so store corruption vs a missing admin-side image vs
  conmon-rot no longer collapse into one generic line. The userns-repair
  post-verify checks worker-image presence per selected lane and reports a
  degraded `repair complete … (probe OK; worker image(s) missing)` instead of
  a clean bill, and `mt-admin status <tenant>` gained a read-only `Worker
  images:` block reporting `present`/`missing`/`not selected for tenant` per
  lane
- **Crate security audit and supply chain.** All 27 tracked lockfile roots
  re-scanned against a fresh RustSec db (2026-09-30) with an empty ignore list
  and `unmaintained = "all"`; the resolved worker-image graph scanned patched
  vs unpatched (control trips RUSTSEC-2026-0285); every accepted-advisory
  upstream blocker re-verified against live crates.io metadata; a full-repo
  yanked-crate pass covered every registry-locked version. Fixed this cycle:
  yanked chacha20 0.10.1 retired from upstream Pebble's lock (now pinned
  0.10.2, refreshed `PEBBLE_LOCK_SHA256`), the Pebble lock guard keys
  multi-version records by (name, version, source) and rejects no-op
  overrides, and multi-version record-edit negative tests cover the reviewer's
  mutated-checksum probe. The eight `ic/deny.toml` advisory ignores remain
  (re-verified; unchanged id set). Evidence:
  [v2.1.4.0_CRATE_SECURITY_AUDIT.md](docs/ops/v2.1.4.0_CRATE_SECURITY_AUDIT.md)
- **CI recovery and integration-MR validation coverage.** Pipeline #4679's two
  failures – Cavepony release metadata drift (registry `0.4.3` vs
  Cargo/capabilities `0.4.2`; later `version-policy` required `0.4.4`) and the
  orphan-mission regression importing a concrete libSQL backend outside the DB
  boundary – were fixed without bypassing gates. Full validation now also
  covers integration-target MRs, with `LUNARWING_CI_FULL_VALIDATION=1` opt-in
  for scratch branches. Evidence: the v2.1.4.0 CI recovery validation report
- **cargo-deny yanked-crate gate restored.** The green `cargo-deny` CI job was
  green dishonestly: run from inside `ic/`, vendored source replacement hid
  the registry index, so `yanked = "deny"` degraded to one
  `warning[index-failure]` per crate (738 in pipeline 4811) while the job
  still exited 0 – and the diagnostics flooded the trace past the 4 MB log
  cap, truncating the verdict away. A repo-root wrapper now runs cargo-deny
  with explicit manifest/config paths, keeps the trace compact, and hard-fails
  on any index-failure so the yanked check can never be silently skipped again
- **Pairing allowlist preservation.** Approving a new sender previously opened
  the allowlist store with `truncate(true)` before deserialization, silently
  erasing every previously approved sender when the read failed. The store now
  reads under the lock with truncate(false) and rewrites through the same
  locked handle (set_len + seek + write + sync), so concurrent approvers
  cannot bypass the lock and shorter payloads cannot leave trailing bytes
- **Outbound message delivery honesty.** The `message` tool's success ack was
  an enqueue ack dressed as a delivery receipt – the contract gap behind a
  scheduled mission recording "Delivered via XMPP" in memory for a stanza
  silently lost in a rebuild window. The ack now reads `Queued message to
  <channel>:<target>` with a tool-description warning that success means
  acceptance for delivery; the XMPP channel refuses enqueue while no session
  is bound; and `shutdown()` gives the client task a bounded 2s drain window,
  WARNing with the pending depth when the queue cannot drain. Documented in
  [OUTBOUND-DELIVERY-SEMANTICS.md](docs/architecture/OUTBOUND-DELIVERY-SEMANTICS.md)
- **Durable XMPP outbound spool.** The channel-side implementation of the
  long-term outbound-delivery fix: every `queue_outbound*` message is
  persisted to `<omemo_store_dir>/spool/` (atomic tmp-write + fsync + rename,
  dir 0700 / files 0600) *before* the mpsc handoff is acked – a spool write
  failure refuses the ack instead of queueing a losable message. The client
  task claims records by atomic rename, deletes them on success, and replays
  all pending records on every `Event::Online` bind, so a restart/crash
  between ack and send no longer loses the message. Post-enqueue failures
  retry with exponential backoff (30s base, 1h cap) and park as
  `<key>.failed.json` after 24 attempts. Live-verified on test tenant derpy:
  19/19 messages delivered exactly once, including a spooled record surviving
  `systemctl stop` and replaying on the next boot
- **Daily-log injection scan demotes quoted describing context.**
  Agent-authored daily logs (`daily/*.md`) were being rejected by the
  workspace prompt-injection write scan for merely *quoting* injection-shaped
  security phrasing. A High-severity match fully enclosed in a short
  (≤200-byte), single-line, inline quoted span (`` `code` ``, `"double"`,
  `'single'`, curly quotes) now demotes to warn-only describing context; a
  closing quote that fails its right-boundary check ends the span instead of
  staying open, so mismatched quotes cannot be eagerly re-paired to bracket
  injection text (the !1317 review P2). Unquoted matches, oversized/multiline
  spans, and tokenizer-level patterns still reject, and identity files and
  lorebooks keep the strict raw scan
- **Curated-memory write contract hardened.** The default
  `memory_write(target: "memory", append: true)` lane did zero
  prompt-injection validation – the earlier hardening scanned every sibling
  lane but skipped this one – so a `<|im_start|>` payload could reach a v1
  session prompt undefused. `append_memory` now scans the entry and the
  combined candidate pre-commit, mirroring `append()`. Every `memory_write`
  lane also rejects payloads over 256 KiB (the incident's ~48 KB context dump
  clobbered a curated `MEMORY.md`), matching the web prompt editor's
  whole-file cap. And a full-replace `write()` of a prompt-visible file with
  non-empty differing prior content now persists the previous version to a
  rolling single-slot `backups/<path>.bak` shadow copy first – exact-path
  readable, never search-indexed, and backup failures never block the write
- **Orchestrator self-modification for new tenants.** add-tenant now renders
  `ORCHESTRATOR_SELF_MODIFY=true` so new tenants adopt stored orchestrator and
  prompt-overlay updates; an explicit operator opt-out is preserved across
  re-renders. Existing tenants are untouched
- **External worker: opencode2 v2.0.21.** The opencode2 lane advances v2.0.18
  → v2.0.21 at reviewed full commit `8a8bd622a3d7dc29ccf30ec17f84e363ed95ed72`
  (lightweight tag `v2.0.21`; tag SHA verified live). Dockerfile pins, CI
  source_ref/build args, and the reproducible-build fixture moved together;
  the `bytecode: true`/`splitting: true` patch contract and bridge surface
  verified unchanged at both pins this cycle (each diff is additive). kimi
  stays at 2.1.1, opencode v1 at `v1.18.25`, pebble at `2d0f3786…`
- **Documentation parity.** Seven docs re-verified against the tree for
  changes not covered by each feature's own updates: the mt-admin command
  reference (new verbs, stale worker list), MT-ADMIN-QUICKSTART
  (prompt-overlay diagnostics tier), TENANT-CONFIGURATION
  (`LUNARWING_TOOL_ESTIMATES`), TESTING_GUIDE (four v2.1.4.0 manual tiers),
  releases/docs indexes, and contributor onboarding documentation
- **Pairing secrets-store design revision 3.** Both P2 findings from the
  rejected !1294 review are answered in
  [PAIRING_SECRETS_STORE_DM_AUTH.md](docs/proposals/PAIRING_SECRETS_STORE_DM_AUTH.md):
  a source-tagged matching contract (strict byte-exact canonical-principal
  lookup, plus a legacy matcher applying today's exact comparison only to
  `kind:"legacy"` grants – no cross-kind broadening) and keyed HMAC-SHA256
  grant names domain-separated from the per-secret encryption subkey, so
  database-read-only dictionary attacks against candidate IRC principals/JIDs
  no longer terminate without the master key. The review-required regressions
  are landed executable tests ahead of the substrate change: legacy
  `@nick`/username and trimmed-exact-id matching (`ic/src/pairing/store.rs`),
  concurrent-grant preservation (8 concurrent approvers), and the wasm
  `pairing_is_allowed` host-path regression
  (`ic/src/channels/wasm/wrapper.rs`)
- **Tracker audit and release reconciliation.** Initially 23 open tracker items were reconciled
  against the tree in
  [v2.1.4.0_OPEN_WORK_ITEMS_STATUS.md](docs/ops/v2.1.4.0_OPEN_WORK_ITEMS_STATUS.md).
  The initial snapshot recorded 5 genuinely open release issues: mission
  capability advertisement, daily-log scan false positives, outbound delivery
  claims, the missing `mission_history` tool, and worker-image infrastructure
  diagnostics. It also recorded the rejected pairing secrets-store review
  (!1294), 15 items deferred by label, and closure evidence for two
  empty-bodied tracker entries. Every code-facing disposition was verified
  against source at `03f5650fe`, not taken from tracker bodies. A later audit
  recorded 25 open items at `150e05ca9`; subsequent landings and the
  source-based closure of the mission-capability concern are documented
  below; the counts refer to their recorded audit snapshots
- **Guides and architecture docs repass.** All 32 active guides under
  `docs/guides/` and all 10 active docs under `docs/architecture/` re-verified
  against current source and stamped for v2.1.4.0, recording the cycle's
  version moves (registry multica 0.1.13, darkirc 0.1.10, weechat 0.2.11, xmpp
  0.1.16; WASM channel crates at workspace `2.1.4`) and confirming V27 remains
  the latest migration with the migration chain's `ironclaw-agent-v1` alias
  removed

> Fire bent the steel,<br>
> thread drew the plates tight together,<br>
> now the helm takes form. 兜

## Compatibility

| Area | v2.1.4.0 behavior |
|---|---|
| Release version | `2.1.4.0` |
| Crate/package version | `2.1.4` for all LunarWing-owned release packages (daemon, five workspace crates, XMPP bridge, four WASM channels) – tracked manifest/lock audit confirms none remain at 2.1.3 |
| Rust edition / MSRV | Rust 2024 / `1.96.1` (no change) |
| New SQL migration | None; `V27__memory_index_outbox.sql` remains latest |
| Multi-tenant port registry | v17 (no change) |
| Channel registry patches | darkirc 0.1.10, multica 0.1.13, weechat 0.2.11, xmpp 0.1.16 (behavior unchanged; rebuild required) |
| Tool registry patches | cavepony-tool 0.4.4 (release-metadata alignment; the `version-policy` gate requires all five version fields to advance together), gitlab 0.1.7, gotify 0.1.1, llm-context 0.1.2, multica-bridge 0.1.3, ssh 0.1.2, vision-analyze 0.1.4, voice-transcribe 0.1.2, web-search 0.2.3 |
| WASM runtime | Wasmtime / wasmtime-wasi 36.0.17 (security update from 36.0.16 for RUSTSEC-2026-0321/0322/0323); workspace and XMPP bridge lockfiles plus vendor tree updated |
| External crate updates | 121 semver-compatible updates across the ic workspace and satellite lockfile roots (notably hyper 1.11.1, reqwest 0.13.5, tokio-rustls 0.26.6, quinn 0.11.12, clap 4.6.7, thiserror 2.0.21, rkyv stack replaced by string_cache/web_atoms/zlib-rs in affected satellites); `ic/vendor/` regenerated |
| Worker image tool pins | kimi-code `2.1.1` (no change); opencode2 `v2.0.21` at `8a8bd622a3d7dc29ccf30ec17f84e363ed95ed72` (was v2.0.18, via v2.0.20); opencode `v1.18.25` (no change); pebble `2d0f3786…` with the reviewed upstream lock re-pinned for the chacha20 retirement (refreshed `PEBBLE_LOCK_SHA256`) |
| Engine provider recovery | Bounded per-turn recovery after partial text for transient/unavailable provider failures; retry caps and rate-limit exclusions unchanged; failed partial responses never enter history or tool execution |
| Provider finish metadata | Real blocking/compatible-stream finish reasons propagated; missing reasons remain Unknown; deployed lunarwing-proxy.py omits absent upstream reasons instead of defaulting to stop |
| Daily-log recovery | Clean appends encountering pre-existing poison quarantine the old content and reject with a retry hint; retry starts a fresh day file, with file/line rejection detail |
| Vendor maintenance | Commit rig-core patch edits and checksums before refresh; same-version refresh restores the committed patch, while a rig-core version change requires manual patch porting |
| Mission global listings | Missions whose project record is missing/unparseable remain globally enumerable; only Active ones reach the active-only listing |
| `routine_history` / `mission_history` | `routine_history` falls back to Engine V2 mission-name lookup when the v1 routines registry misses, with character-based summary truncation; in v2 threads it is blocked with an honest pointer to the new `mission_history` tool (project-scoped own+shared, ownership re-checked on the UUID path) |
| Voice sidecar admission | One in-flight call per component (STT/TTS/VAD) before shared capacity; blocked workers hold both permits until settlement; poisoned lock/panic = `component_failed`/503 until restart |
| Tool runtime hints | `LUNARWING_TOOL_ESTIMATES` read once at startup: unset/`observe` = record only, `1` = record + hints, `0` = off; restart resets learned models; no migration or new wire fields |
| Tenant system prompts | Settings editor writes primary-scope prompts consumed as Engine V2 initialization snapshots (activate via `restart-tenant` + new conversation); existing threads keep their System message |
| mt-admin verbs | New: `psql <tenant>`, `daemon-pid <tenant>` (read-only diagnostics); `status <tenant>` reports per-lane worker-image presence; all existing verbs unchanged |
| New tenant env defaults | `ORCHESTRATOR_SELF_MODIFY=true` rendered at add-tenant; explicit operator values preserved on re-render; existing tenants untouched |
| Pairing allowlist | Approvals no longer truncate the existing allowlist store before reading; concurrent approvals serialize under the file lock |
| XMPP outbound delivery | `message` tool ack reports `Queued …` (acceptance, not delivery); enqueue refused while no session is bound; bounded 2s shutdown drain; durable spool persists every outbound message before ack with replay on bind, backoff retry, and `.failed.json` parking after 24 attempts – at-least-once semantics (post-send crash can duplicate at whole-message granularity) |
| Workspace write scanning | Daily logs (`daily/*.md`) demote High-severity injection matches fully enclosed in short (≤200-byte) single-line inline quoted spans to warn-only describing context; unquoted matches and tokenizer-level patterns still reject at the same floor; identity files and lorebooks keep the strict raw scan |
| Workspace memory writes | The `append_memory` lane (`memory_write(target: "memory", append: true)`) is prompt-injection scanned pre-commit like every sibling lane; all `memory_write` lanes reject payloads over 256 KiB; full-replace writes of prompt-visible files persist the prior content to a rolling `backups/<path>.bak` shadow copy (exact-path readable, never search-indexed) before replacing it |
| Pairing store | Design revision 3 only – no substrate change this release; landed tests pin current legacy matching and concurrent-grant semantics ahead of the secrets-store migration |
| CI validation coverage | `.full-validation-context` includes integration-target MRs; scratch branches opt in with `LUNARWING_CI_FULL_VALIDATION=1`; worker publication remains protected/push-only; the cargo-deny job runs unvendored from the repo root via `.gitlab/scripts/cargo-deny-check.sh` and fails on any `index-failure` diagnostic |
| Deny policy | `ic/deny.toml` advisory ignores unchanged in id set (8), re-verified 2026-09-30 with blocking dependency chains documented |

## Changes

### Real Provider Finish Reasons

The adapter previously inferred `Stop` or `ToolUse` from response content,
discarding the provider's terminal metadata. Blocking extraction now reads
`choices[0].finish_reason` or Anthropic-shaped `stop_reason`, using the shared
canonical mapping. Tool calls in blocking content still take precedence over
mislabeled wire metadata; absent or unrecognized metadata becomes `Unknown`
rather than a fabricated successful stop.

For streaming, the local rig-core 0.40.0 patch carries stop, length, and
content-filter reasons through aggregation into the final response and exposes
them through `GetTokenUsage::finish_reason()`. With no reported reason, the
adapter falls back to tool use only when tool calls actually streamed,
otherwise to unknown. The deployed
`tensorzero-proxy-configurations/lunarwing-proxy.py` likewise omits an absent
upstream reason rather than substituting `stop`.

This lets the lightweight routine engine distinguish `Length` as
`TruncatedResponse` instead of misclassifying it as `EmptyResponse`. Engine V2
receives the metadata through the bridge, but does **not** use it to
choose a length-retry versus stop-terminal recovery path. The outbound web
OpenAI-compatible surface still maps Unknown to `stop`;
deprecated/experimental proxies were not changed. Historical validation of
this change records 64 rig-adapter, 612 LLM, and 13 bridge-adapter tests,
including scripted blocking and TensorZero SSE regressions.

### Daily-Log Quarantine and Rejection Detail

When the combined append candidate fails its injection scan, but the new entry
is clean and the existing daily log itself trips the gate,
`Workspace::append()` copies the existing content to the first free
`daily/YYYY-MM-DD.poisoned-<n>.md` slot and clears the original day file. The
discovering append still returns an error naming the quarantine path and
asking the caller to retry; the clean entry is written only on retry.

Quarantine copies remain listed and exact-path readable, with no search chunks
and no automatic daily-log prompt injection. A poisoned new entry or a pattern
formed only across the append boundary is rejected without moving the file.
Failed quarantine retains the original rejection, so operator repair can still
be necessary. Both strict and daily-log rejection details now include the
1-based offending line. The broader detector redesign and an operator-approved
repair/overwrite tool remain outside this change. Quarantine validation
records 241 workspace and 24 memory-tool tests, including quarantine →
rejection → successful retry, slot preservation, boundary-split rejection, and
line-number coverage.

### Wasmtime Security Update and Vendor Patch Preservation

Master pipeline 4922 superseded the earlier green pipeline-4907 snapshot: its
cargo-deny job failed on three newly published advisories against
wasmtime-wasi 36.0.16. The fix advances the Wasmtime family to 36.0.17 (and
matching Cranelift family to 0.123.17) in the workspace and XMPP bridge locks
and regenerates the vendor tree:

- RUSTSEC-2026-0321: preview-0 `poll_oneoff` bypassing fuel consumption.
- RUSTSEC-2026-0322: excessive host memory when guests lack stdio.
- RUSTSEC-2026-0323: `fd_readdir` exposing uninitialized padding to guests.

No advisory ignore was added. The exact unvendored CI wrapper and both
workspace/bridge compile checks passed. Final remote release-head validation
is complete.

That refresh also exposed a vendor-maintenance defect: `cargo vendor`
overwrites the local rig-core finish-reason patch even on no-op refreshes,
making the original post-vendor tripwire fail every time.
`ic/scripts/refresh-vendor.sh` now checks for a clean committed patch and its
marker **before** vendoring, then restores the committed patched tree and
matching checksums from Git when the version is unchanged. A rig-core version
change fails explicitly so a maintainer can port the patch onto the new
sources, regenerate checksums, commit, and retry. See
[LUNARWING-PATCH.md](ic/vendor/rig-core/LUNARWING-PATCH.md). Vendor-refresh
validation records a successful full no-op refresh, clean vendor diff,
advisory gate, compile check, and dirty-patch rejection.

### Mid-Stream Provider Failure Recovery

Mission executor turns that lose the provider after partial text previously
aborted the thread – the deferred FrontierTEEC/TensorZero response-body decode
failure from v2.1.3.0. Recovery is now bounded and typed:

- Transient/unavailable failures after partial text admit a bounded retry;
  permanent failures do not.
- Failed partial responses stay out of history and never reach tool execution.
- Existing retry caps and rate-limit exclusions remain enforced; attempts
  accumulate as provisional Live work with no per-retry reset (correcting the
  rejected !1258's stream-gap explanation).
- Successful responses are the only thing persisted, with usage accounted
  once.

Regression evidence: the new recovery/cap tests fail against the old
implementation; all 263 executor tests and all 8
`engine_v2_tensorzero_streaming` tests pass with the fix. Coverage includes
interrupted-tool execution isolation, exhausted recovery, safe recovery
context, and successful-response-only persistence/usage. Remote CI validation
is complete.

### Global Mission Listings and Daily Rollover

The recurrence behind the dwarfstar "expected-behavior" evidence was a host
store defect, not an engine one. Missions load by directory scan
(`engine/projects/*/missions/*/mission.json`) while projects load from
`project.json`; `HybridStore`'s trait-default global listings derived from
`list_all_projects()`, so a mission whose project record was missing or
unparseable loaded into the missions map but was invisible to every global
listing – never bootstrapped into the active index, so no startup/tick
rollover, no event/webhook dispatch, and `threads_today_date` pinned at its
creation date. Both `list_all_missions_global` and
`list_all_active_missions_global` now enumerate the missions map directly (the
same semantics the engine test store already had). New libsql regression
`global_mission_listings_see_missions_whose_project_file_is_missing`; full
engine mission suite passes. The ENGINE-V2 scheduling contract is updated.
Live dwarfstar validation is complete, including inspection of persisted
`mission.json` after a day rollover.

### `routine_history` Engine V2 Mission Fallback

Legacy `routine_history` could not resolve Engine V2 mission names – the
v2.1.3.0 known issue. The tool now falls back to a read-only bridge query when
the v1 routines lookup misses:

- `ic/src/bridge/router.rs` exports `get_engine_mission_history_by_name` –
  default-project own+shared lookup, then a global ownership-filtered scan –
  rendering recent fires from `mission.thread_history`.
- `ic/src/tools/builtin/routine.rs` uses it as the fallback, with not-found
  errors naming both registries and pointing at `routine_list`/`mission_list`;
  limit clamped 1..=50; schema canary synced.

Both !1256 review findings are addressed: run summaries truncate by characters
(`truncate_summary_chars`), never byte-indexing `String::truncate` (the byte
form panicked on multilingual assistant text – regressions cover CJK, a
199-ASCII+`é` boundary, and 4-byte emoji), and the fallback test takes the
shared `ENGINE_STATE_TEST_LOCK` and resets state explicitly so it cannot race
the router's engine-state tests. 90 router tests, 26 routine tool tests, 250
bridge tests, 1167 tools tests, and the e2e `routine_history` trace test pass.
ENGINE-V2.md's Learning Missions section documents the behavior.

### `mission_history` Tool and the `routine_history` v2 Block

The missing piece behind the `routine_history` fallback: Engine V2 missions
had no first-class history tool at all. `mission_history` now joins the bridge
`mission_*` family (`ic/src/bridge/effect_adapter.rs`), rendering newest-first
fires from `mission.thread_history` in the same output shape as
`routine_history` – character-safe 200-character summaries, limit clamped
1..=50, default 10 – so the LLM can switch tools without relearning the
schema:

- Resolution accepts `name` or `id` (also positional), scoped like
  `mission_list` (own + shared missions in the caller's project). The UUID
  path re-checks `project_id` and ownership after the raw store load –
  `resolve_mission_id` accepts any well-formed UUID, so without this check a
  thread in project A could read project B's mission history (the !1302 review
  P2, carried into this implementation).
- A new `MissionManager::load_thread` read accessor
  (`ic/crates/lunarwing_engine/src/runtime/mission.rs`) backs the run entries;
  the tool is registered beside `mission_list`/`mission_update` in
  `ic/src/bridge/router.rs`.
- `routine_history` joins the v2-only block list: in Engine V2 threads it now
  returns an honest "use mission_history" error (mission names come from
  `mission_list`) instead of falling through to the wrong registry and
  shadowing mission names.

Verified with 59 `bridge::effect_adapter` tests (including the UUID-path
scoping regression and the never-fired empty-history case) and 90
`bridge::router` tests. ENGINE-V2.md documents both resolution paths.

### Voice Sidecar Inference Admission and Recovery

The deferred defect from v2.1.3.0 – a timed-out Kokoro synthesis retaining the
sidecar's inference permit and TTS mutex – is fixed with component-first
admission:

- STT/TTS/VAD each admit one call before taking shared capacity; blocking
  workers retain both permits until settlement, so repeated TTS retries cannot
  queue behind the model mutex or consume remaining STT/VAD capacity.
- Caller cancellation/timeout and worker settlement share a phase lock.
- `/ready` and `/metrics` report per-component activity, current/cumulative
  abandonment, and permanent failure.
- `WouldBlock` remains transient Busy; poisoned model locks and panics return
  explicit `component_failed`/503 and keep readiness degraded until restart.
  The supported recovery procedure is the whole-tenant `restart-tenant` verb
  (documented with its whole-tenant impact); a never-returning native call
  still retains one permit and requires operator restart.

Both !1263 review findings are addressed (transient-vs-permanent failure
typing; the regression holds the actual production runner's component mutex
during panic, both with a waiting caller and after cancellation). 23 sidecar
tests pass in each feature configuration, including six new regressions:
repeated timeouts, cancellation, capacity isolation, normal/error drain
recovery, lock contention/poison, and panic/HTTP status/readiness. Integration
notes: !1275 was selected over the parallel !1278 after comparing complete
diffs – !1275 contains all four !1278 API regressions plus the
concurrent-settlement and contention/pre-poisoned-lock tests.

### Per-Tenant System-Prompt Management

Settings → System Prompt provides per-tenant, primary-scope source editing,
missing-file creation,
unsaved-edit preservation on failed saves, and a saved workspace preview with
group/timezone controls. Authenticated `/api/prompts` routes retain workspace
write validation/indexing and scope isolation (tenant/read-scope isolation,
rejected writes, group/timezone composition, initialized owner bootstrap
state). The Engine V2 consumption model is an initialization snapshot:
activation requires `restart-tenant` followed by a new conversation; existing
threads retain their System message. Operator guide:
[TENANT-SYSTEM-PROMPTS.md](docs/guides/TENANT-SYSTEM-PROMPTS.md).

### Tool Runtime EMA Estimates (first slice)

Bounded, per-user EMA learning is wired into shared tool execution, local
jobs, and routines; frozen advisory runtime hints appear on both v1 approval
paths and the v2 pending-gate insertion path:

- Cold-start in-memory learning with a five-sample warmup, optional actual
  cost, unadjusted-baseline ratios, frozen forecast errors, numeric guards,
  and bounded outlier influence. Failures, timeouts, cancellation, and
  approval/auth waits do not train completed runtime.
- `LUNARWING_TOOL_ESTIMATES` is read once per registry at startup: unset or
  `observe` records only; `1` records and shows hints after warmup; `0`
  disables recording and new hints. Restart resets learned models; persisted
  pending gates retain their original description, including any hint.
- Legacy v1/v2 recovery no longer falls back to raw parameters when a redacted
  snapshot is missing – `null` is rendered instead.

No database migration or new wire fields are required. Verified with 114
targeted tests plus the overhead benchmark (twelve-thread helper p95 0.260
ms); PostgreSQL test-target and libSQL library compile checks and all-feature
Clippy pass. This release covers tool-runtime estimates; LLM timing,
whole-job/mission ETA, gate-wait analytics, and durable aggregate caches are
not included. Evidence: the v2.1.4.0 EMA validation report.

### Engine V2 Prompt-Overlay Audit and Read-Only Diagnostics

mt-admin gained two read-only diagnostics verbs: `psql <tenant>`
(container-based, init-agnostic tenant database access) and `daemon-pid
<tenant>` (strict PID-record validation with tenant association – malformed
records are rejected, not converted into a PID). On top of them sits the
operator runbook for auditing database-backed Engine V2 prompt overlays –
determining whether an agent's effective system prompt differs from the
shipped prompt without a Git diff and without enabling self-modification. Both
!1261 review P2s are fixed on the tooling itself: a failed environment capture
reports `uninspected (live gate)` with a non-zero exit rather than certifying
"gate off" (the old pipeline's failed `cat /proc/$pid/environ` collapsed into
"absent"), and the PID is validated as-written against the tenant's record
before any association claim. No intermediate environment reaches a
group/world-readable log; values are reduced to presence/absence before
printing. A regression pins that `prompt_additions` writes nothing while the
self-modify gate is off. The !1276 review's daemon-PID findings (as-written
validation, tenant association, fixture cleanup) are addressed in the same
pass.

### mt-admin Worker-Image Store Surfacing

Tenant worker-image failures no longer hide behind generic lines:

- `_ensure_tenant_image` now captures both halves' stderr from the `podman
  save | podman load` distribution into mktemp files and quotes the first
  lines in the WARNING – store corruption vs a missing admin-side image vs the
  conmon-rot class no longer collapse into one generic line; the temp files
  are cleaned on the success, failure, and mktemp-failure paths.
- The `repair_tenant_podman_userns` post-verify checks worker-image presence
  per selected lane via the tenant-context container wrapper (new shared
  `_worker_image_refs` helper mirroring the four worker refs, using the same
  gate `start_tenant` uses), so repair no longer concludes "complete (probe
  OK)" with the pebble image absent – it warns with remediation and reports
  `repair complete … (probe OK; worker image(s) missing)`, the file's existing
  degraded-but-repaired idiom.
- `mt-admin status <tenant>` gained a `Worker images:` block reporting
  `present`/`missing` per selected lane and `not selected for tenant` for the
  rest – read-only, init-agnostic, with no false "missing" for unselected
  lanes.

Verified with the mt-admin regression suites (tenant-image 10/10 with the new
stderr-quoting cases, fix-podman-userns cases 9–10, openrc-status `Worker
images:` assertions) plus repo-wide shell syntax/shellcheck. Container
recreation based on image-ID changes is outside this update's scope.

### Crate Security Audit and Supply Chain

The rejected !1264 attempt was superseded by a rebuild on the merged !1262
consolidation, addressing every review finding:

- All 27 tracked Cargo lockfile roots scanned with an **empty advisory ignore
  list** and `unmaintained = "all"` against RustSec db `9b3a3b73a`
  (2026-09-30) – the policy itself keeps `unmaintained = "workspace"` plus the
  eight documented ignores.
- The actual pinned worker-image graph scanned patched vs unpatched (the
  control trip on RUSTSEC-2026-0285 proves the scan resolves the real graph,
  not the committed lock text).
- Every accepted-advisory upstream blocker re-verified against live crates.io
  metadata; a new full-repo yanked-crate pass covered every registry-locked
  version in every lockfile.
- Fixed this cycle: the single yanked crate anywhere in the tree (chacha20
  0.10.1, inherited from upstream Pebble's own lock) is retired – the override
  now pins 0.10.2 with a refreshed `PEBBLE_LOCK_SHA256`; the Pebble lock guard
  keys multi-version records by `(name, version, source)`, rejects duplicates,
  compares complete records, and now fails when nothing changed (so the
  override is retired once upstream ships its own fixed lock); multi-version
  record-edit negative tests cover the reviewer's mutated `cpufeatures 0.2.17`
  checksum probe.
- A live `podman build` proves the digest gate and allowlist run inside the
  image build (compiling rustls 0.23.45).

Validation is complete for the ci-config validator, 29-test `.gitlab/tests`
suite, full `cargo deny check`, lockfile-only guard, mt-admin pebble
regression, and shellcheck, together with the remote pipeline and Trivy scan
of the published worker image. Evidence:
[v2.1.4.0_CRATE_SECURITY_AUDIT.md](docs/ops/v2.1.4.0_CRATE_SECURITY_AUDIT.md).

### CI Recovery and Validation Coverage

Pipeline #4679's failures were diagnosed and fixed without weakening any gate:

- `cavepony-wasm`: registry version `0.4.3` disagreed with the CI expectation
  `0.4.2` (Cargo and capabilities also at `0.4.2`). The `version-policy` job
  later rejected `0.4.3` again because the MR base already carried that
  registry release – all five release version fields now advance together to
  `0.4.4`, verified against the actual MR base, with a shell regression
  covering metadata-only alignment and MR-base precedence.
- `architecture-boundaries`: the new orphan-mission regression imported a
  concrete libSQL backend outside the DB/testing boundary; it now uses the
  existing migrated `crate::testing::test_db()` fixture with no boundary
  exclusions.
- Full validation now covers integration-target MRs (previously only
  default-branch targets), so a green MR can no longer hide a red integration.
  Scratch branches opt in with `LUNARWING_CI_FULL_VALIDATION=1`; worker
  publication stays protected and push-only, and validation caches stay
  pull-only.

Locked builds, the fuel budget, required artifacts, and the unchanged boundary
ratchet are all retained. Evidence: the v2.1.4.0 CI recovery validation
report.

### CI cargo-deny Yanked-Crate Gate

Pipeline 4811 was green 35/35 – but the `cargo-deny` job was green
dishonestly. Running from inside `ic/` source-replaces crates.io with
`ic/vendor/`, so the `yanked = "deny"` policy silently degraded to one
`warning[index-failure]` per crate (738 of them in 4811) while the job still
exited 0; worse, those diagnostics pushed the trace past the 4 MB log cap,
truncating the verdict away. The runner egress contract already allows the
crates.io index, so the fix restores the intended gate rather than relaxing
it:

- New `.gitlab/scripts/cargo-deny-check.sh` runs cargo-deny from the repo root
  with explicit `--manifest-path ic/Cargo.toml` and `--config ic/deny.toml`,
  hides inclusion graphs to keep the trace compact, and hard-fails if any
  `index-failure` diagnostic appears – the yanked check can never be silently
  skipped again.
- `.gitlab/ci/security.yml` uses the wrapper; `ic/scripts/refresh-vendor.sh`
  routes its post-vendor advisory gate through the same wrapper instead of the
  old vendored in-`ic/` invocation.
- `.gitlab/scripts/validate_config.py` gained `validate_cargo_deny_gate`
  (rejects a job bypassing the wrapper or a wrapper that loses its guards)
  with a six-case regression suite in `.gitlab/tests/test_cargo_deny_gate.py`;
  `.gitlab/README.md` documents the unvendored gate and the pipeline-4811
  incident.

Post-repair confirmation: the next latest master pipeline, 4867, was verified
job-by-job – all 35 jobs `success`, zero failed/canceled – with the
`cargo-deny` job confirmed at the pipeline's exact SHA to run this wrapper, so
its green is the honest mode rather than the silent-degradation mode fixed
above (the 42-second runtime matches the compact trace; the broken mode blew
past the 4 MB log cap).

### Pairing Allowlist Preservation

`PairingStore` approval opened the allowlist store with `truncate(true)`
before deserializing, so any failed read silently removed every previously
approved sender before the new entry was written. The store now reads under
the lock with `truncate(false)`, and the rewrite goes through the same locked
handle (`set_len(0)` + seek + write + `sync_all`), so concurrent approvers
cannot bypass the lock and shorter payloads cannot leave trailing bytes
behind. This matches the documented "approved pairing codes" semantics the
stopgap assumed.

### Orchestrator Self-Modification for New Tenants

`write_tenant_lunarwing_env` now renders `ORCHESTRATOR_SELF_MODIFY=true` for
new tenants, adopting stored orchestrator and prompt-overlay updates by
default. The value is preserved across upgrade/rebuild/re-render: an explicit
operator opt-out (`false`) is never clobbered, and existing tenants are not
rewritten merely by upgrading. The prompt-overlay audit runbook complements
this with the inspection procedure for tenants that keep the gate off.

### Outbound Message Delivery Honesty

The `message` tool's success ack was an enqueue ack dressed as a delivery
receipt: channels accept messages into in-memory outbound queues, and the XMPP
queue is dropped on restart. That contract gap is what let scheduled mission
PR Digest #59 record "Delivered via XMPP … Goal achieved" in memory while the
queued stanza was silently lost in a rebuild window. Three hardening changes
landed, following the agreed order of fixes:

- The tool ack is now honest – `Queued message to <channel>:<target>` – and
  the tool description warns that success means acceptance for delivery, never
  "delivered".
- The XMPP channel refuses proactive enqueue while no session is bound (`XMPP
  is not connected (no bound session); message not queued`) instead of
  accepting into a doomed queue, so the failure surfaces to the caller.
- `shutdown()` gives the client task a bounded 2s window to drain already
  acked stanzas (pending-counter driven, mirroring the coalesce flush) and
  WARNs with the pending depth when it cannot, instead of dropping silently.

The WIT channel interface text is intentionally unchanged – its `Ok` semantics
are spelled out in the tool description and the new architecture doc instead
of a comment-only WIT edit that would force a package version bump and
registry-wide churn. The XMPP half of the long-term durable-spool fix then
landed in the durable outbound spool (next section), closing the restart-loss
window for the XMPP lane; the mission-run ledger is not included. New
architecture doc: `docs/architecture/OUTBOUND-DELIVERY-SEMANTICS.md` also
records the mission-spec discipline (daily log → send → memory records
acceptance; update the run baseline each time). The PR Digest mission spec
itself is tenant workspace data, outside this repo.

### Durable XMPP Outbound Spool

The durable spool completes the channel-platform half of the long-term
outbound-delivery fix. The XMPP outbound queue was memory-only, so a
restart/crash/rebuild between the acked enqueue and the wire send silently
lost the message – the exact PR Digest #59 incident class. The channel now
runs a durable spool (`ic/src/channels/xmpp/spool.rs`):

- Every `queue_outbound*` message is persisted to
  `<omemo_store_dir>/spool/<millis>-<seq>-<uuid>.json` (atomic tmp-write +
  fsync + rename, dir 0700 / files 0600) *before* the mpsc handoff is acked –
  a spool write failure refuses the ack instead of queueing a losable message,
  and a dead client task discards the record (no ack, no replay).
- The client task claims a record by atomic rename before sending (a spool
  replay and a late in-memory copy can never both send) and deletes it on
  success; every `Event::Online` bind replays all pending records after room
  joins and OMEMO publish.
- Post-enqueue failures (send_stanza, OMEMO/room prep, XEP-0363 upload) are no
  longer bare warn-and-drop: the record retries with exponential backoff (30 s
  base, 1 h cap), and after 24 attempts parks as `<key>.failed.json` with the
  last error; invalid JIDs park immediately as permanent. The connection-gate
  and shutdown-drain semantics are preserved – the undrained-shutdown WARN now
  says messages remain spooled, not lost.
- Internal protocol sends (pairing prompts) are deliberately unspooled, and
  the rate limiter is not re-applied to replays (already counted at the
  original ack).

Semantics are at-least-once: a crash between wire-send and record-clear
replays, so a duplicate is possible at whole-message granularity. Verified
with 93 `channels::xmpp` lib tests (12 new spool unit + 4 new channel-level,
including a restart-window replay across two channel instances sharing a data
dir) and live on test tenant derpy: 19/19 test messages delivered exactly
once, including a spooled (acked-but-unsent) record that survived `systemctl
stop` and replayed on the next boot's Online drain. End-to-end delivery
receipts (XEP-0198) and the engine-side mission-run ledger are not included
in this release.

### Daily-Log Injection Scan Quoted Demotion

The workspace prompt-injection write scan was rejecting agent-authored daily
logs that merely *described* injection-shaped security content – the
false-positive class addressed here. Two complete implementations (!1317,
!1327) were closed unmerged on process grounds before this landing. Daily-log
writes (`daily/*.md` only, via the pre-existing `is_daily_log_file` gate) now
demote a High-severity match fully enclosed in a short (≤200-byte),
single-line, inline quoted span (`` `code` ``, `"double"`, `'single'`,
`“curly”`, `‘curly’`) to warn-only describing context (`quoted_spans` +
`reject_daily_log_if_injected` in `ic/src/workspace/mod.rs`):

- Tokenizer-level patterns (`<|`, `|>`, `[INST]`, `[/INST]`, null bytes) and
  unquoted matches still reject at the same High floor as the strict path;
  identity files and lorebooks keep the strict raw scan.
- The !1317 review P2 is closed inside the same change: a closing quote that
  fails its right-boundary check ends the span instead of staying open, so
  mismatched quotes cannot be eagerly re-paired to bracket injection text
  (`"a"ignore previous …"` stays unquoted and rejects).
- Oversized or multiline quoted spans stay strict; the demotion is
  daily-log-only.

Verified with 230 `workspace::` lib tests including the eleven dedicated
regressions (span pairing/boundaries, apostrophes never open spans,
mismatched-closer drops span, quote-bracketed injection still rejected, quoted
phrase allowed, unquoted/live/tokenizer-level still rejected,
oversized/multiline stays strict, demotion is daily-log-only). Documented in
the workspace README's new Prompt-Injection Write Scanning section. The change
landed via !1332 (pipeline #4880 green) re-applying the reviewed !1327 head,
whose review had zero findings – it had died only on release-planning file
merge conflicts.

### Curated-Memory Write Contract

The curated-memory incident was initially marked "fixed" after data recovery
only. The underlying gaps behind a session's ~48 KB context dump clobbering a
curated `MEMORY.md` are now closed in a follow-up hardening pass, landed via
!1341/!1344:

- **The `append_memory` scan bypass is closed (the MED-HIGH finding).** The
  default `memory_write(target: "memory", append: true)` lane did zero
  validation – the earlier hardening commit scanned every sibling lane but
  skipped this one. `append_memory()` in `ic/src/workspace/mod.rs` now scans
  the entry and the combined candidate before commit, mirroring `append()`
  (pre-commit, so no compensation path is needed). A `<|im_start|>` payload
  through this lane into a v1 session no longer reaches the prompt undefused.
- **`memory_write` payload cap.** The tool accepted unbounded content; every
  lane now rejects payloads over 256 KiB (`MAX_MEMORY_WRITE_BYTES` in
  `ic/src/tools/builtin/memory.rs`), matching the web prompt editor's
  whole-file cap.
- **Shadow backup for prompt-file full replaces.** `Workspace::write()` on a
  prompt-visible file with non-empty differing prior content now persists the
  previous version to a rolling single-slot `backups/<path>.bak` doc before
  replacing it – the clobber class is recoverable in-place via exact-path
  `read()` instead of manual re-curation. Backups carry an empty chunk
  projection (never search-indexed), are not rescanned (so poisoned content
  can still be replaced), and backup failures log-and-continue rather than
  blocking the write.

The stale `memory.rs` comment claiming `append_memory` scanning was handled by
`write()`/`append()` was corrected, and the spec is updated in the workspace
README (scan lane, shadow-backup section, cap note). The broader daily-log
and curated-memory detector redesign (line-start role labels plus a
"resembles the live system prompt" provenance heuristic) is not included
in this release.

Verified: 6 new workspace integration tests (injection entry rejected
pre-commit with no doc created, append over poisoned existing content rejected
without touching it, benign appends land with the `\n\n` separator, backup
created/rolling/scoped-to-prompt-files, backup unindexed) plus a tool-level
cap test (oversized rejected on all lanes, at-limit passes the gate) –
`workspace::` suite 236/236 and `tools::builtin::memory` 24/24 pass, with
clippy `-D warnings`, rustfmt, and the panic-policy scanner clean.

### Version Bumps and Supply Chain

All eleven LunarWing-owned release packages moved `2.1.3` → `2.1.4`: the
daemon, the five workspace crates (`lunarwing_common`, `lunarwing_engine`,
`lunarwing_skills`, `lunarwing_safety`, `lunarwing_health_metrics`), the XMPP
bridge, and the four WASM channels. Channel registry versions advanced
(darkirc 0.1.10, multica 0.1.13, weechat 0.2.11, xmpp 0.1.16) and all nine
tool registry entries advanced together; lockfiles were refreshed and the
tracked manifest/lock audit confirmed no project-owned package remains at
2.1.3. 121 external crates took semver-compatible updates across the ic
workspace and the repo-root satellite lockfile roots; `ic/vendor/` was
regenerated. Channel behavior is unchanged – the bumps are rebuild-only.

### External Workers: opencode2 v2.0.21

The opencode2 lane moved twice this cycle, landing at v2.0.21. The first
update advanced v2.0.18 → v2.0.20 at
`84c9be93a56304a108f1a22df0c5d62c26d5b6ca`; the second advanced v2.0.20 →
v2.0.21 at reviewed full commit `8a8bd622a3d7dc29ccf30ec17f84e363ed95ed72`
(lightweight tag `v2.0.21`; both tag SHAs verified live against the
anomalyco/opencode remote). Dockerfile pins, CI source_ref/build args, and the
reproducible-build test fixture moved in step at each bump. Verified at the
final pin: the upstream `build.ts` still has exactly one `bytecode: true` /
`splitting: true` (patch contract holds); the bridge surface is unchanged on
every endpoint the worker uses (`OpenCode.make`, `session.create` with
`permissions`, `session.prompt`, `session.message.list`,
`session.permission.reply`, `session.interrupt`, `event.subscribe`, all eight
matched event names) – the v2.0.20 → v2.0.21 surface diff is additive only (an
optional `message` param on the unused `session.form.cancel` endpoint and an
`x-opencode-session-id` header; the `permission.reply` path is
byte-identical), as was the v2.0.18 → v2.0.20 diff (unused
`credential.list/create` endpoints). Host-side reproducible-build preparation
3/3 OK. Whole-image reproducibility validation is complete through the CI
candidate-vs-clean rebuild digest gate. kimi remains at 2.1.1, opencode v1
at `v1.18.25`, and pebble at `2d0f3786…`.

### Documentation Parity

Seven files were re-verified against the tree for drift not already covered by
each feature's own doc updates: `docs/ops/MULTITENANCY-PRODUCTION.md`
(mt-admin command reference, stale worker list),
`docs/guides/MT-ADMIN-QUICKSTART.md` (prompt-overlay diagnostics tier with the
uninspected-on-failure rule), `docs/ops/TENANT-CONFIGURATION.md`
(`LUNARWING_TOOL_ESTIMATES`), `docs/guides/TESTING_GUIDE.md` (four v2.1.4.0
manual repass tiers), `docs/releases/README.md` (indexed the shipped v2.1.3.0
notes), `docs/README.md` (indexed TENANT-SYSTEM-PROMPTS.md), and
`docs/guides/CONTRIBUTOR-ONBOARDING.md`. Checked and
intentionally left alone: ENGINE-V2.md, GATEWAY_VOICE_API.md + voice-sidecar
README, TENANT-SYSTEM-PROMPTS.md, estimation/tools READMEs,
WORKER-CONTAINERS.md (each already updated alongside its
corresponding change).

### Pairing Secrets-Store Design Revision 3

The rejected !1294 attempt is superseded by design revision 3 at
[PAIRING_SECRETS_STORE_DM_AUTH.md](docs/proposals/PAIRING_SECRETS_STORE_DM_AUTH.md),
which answers both P2 findings from the review:

- **Legacy matching contract.** The rejected revision promised byte-exact
  digest lookup for every query while also claiming byte-verbatim legacy
  migration preserves today's matching – contradictory (a legacy `@Alice`
  entry would never be found by an id- or username-derived lookup). Revision 3
  defines two explicit source-tagged matchers: a strict byte-exact
  canonical-principal lookup (equivalent to today's id comparison) and a
  legacy matcher applying today's exact comparison function **only** to
  `kind:"legacy"` grants – no cross-kind broadening in either direction.
- **Name privacy.** Unkeyed SHA-256 names are replaced with
  `hex(hmac_sha256(name_key,
  "lunarwing-pairing-name-v1"|channel|0x00|principal))[0..10]` where
  `name_key` is an HKDF-SHA256 master-key subkey domain-separated from the
  per-secret encryption subkey – database-read-only dictionary attacks against
  candidate IRC principals/JIDs no longer terminate without the master key.
  Key lifecycle equals master-key lifecycle (no independent rotation exists
  today; the re-key contract is stated).
- **Hot path (nonblocking fix).** The "decrypt-per-grant enumeration is
  operator-only" claim is withdrawn – Signal reads allowFrom per message,
  weechat per DM, web per activation render. Revision 3 specifies a bounded
  decoded-grant cache per channel: 15 s TTL, synchronous invalidation on
  daemon-side approve, revoke-latency-bounded-by-TTL documented for operators,
  1024-grant bound with uncached fallback.

The regressions the review required are landed executable tests ahead of the
substrate change, so the current semantics cannot silently drift before the
migration lands: `test_legacy_username_entry_matches_by_username`,
`test_legacy_trimmed_id_entry_matches_exact_id`, and
`test_concurrent_add_allow_from_preserves_all_grants` (8 concurrent approvers)
in `ic/src/pairing/store.rs`, plus
`test_pairing_host_functions_preserve_legacy_matching` exercising the
`ChannelStoreData` `Host::pairing_is_allowed` path in
`ic/src/channels/wasm/wrapper.rs` against a legacy-shaped allowFrom file.
Verified: 27/27 pairing tests and 77/77 wasm wrapper tests pass; lib Clippy
with `--all-features --tests -D warnings` and rustfmt clean. This is
a design update only; the secrets-store substrate implementation is not
included in this release.

### Tracker Audit and Release Reconciliation

All 23 open tracker items were fetched live from the GitLab API and reconciled
in
[v2.1.4.0_OPEN_WORK_ITEMS_STATUS.md](docs/ops/v2.1.4.0_OPEN_WORK_ITEMS_STATUS.md)
following the v2.1.3.0 audit format. Key findings, each verified against the
tree at `03f5650fe` rather than taken from tracker bodies:

- 5 genuinely open items carried the 2.1.4.0 label: engine-preamble
  `mission_*` advertisement, daily-log injection-scanner false positives, the
  PR Digest's false "delivered" memory record, the missing `mission_history`
  tool, and integration-test/infrastructure investigation. The pairing
  secrets-store entry recorded the !1294 rejection with both P2 design
  findings quoted.
- 15 items were deferred to 2.1.5.0+ by label, several carrying
  source-verified root-cause maps.
- Two empty-bodied tracker entries had supporting evidence for closure in this
  cycle's audit docs.
- Tracker churn since the 2026-09-27 audit: 24 items created-and-closed, the
  tenant system-prompt editing and pairing-stopgap entries closed, and the
  open set grew 13 → 23.

The audit is a point-in-time record at `03f5650fe`. A 22:10Z refresh folded in
the worker-image diagnostics and durable XMPP spool landings, along with the
closure of the earlier daily-log scan implementation in !1327. Four of the
five genuinely open release issues have since landed: `mission_history`,
outbound delivery honesty, mt-admin worker-image store surfacing, and
daily-log quoted demotion. The mission-capability advertisement concern was
subsequently closed through the source review described below.

**The mission-capability advertisement concern was closed on 2026-10-02 after
source review, not a new binding implementation.** The closing discussion
confirms mission functions are foreground-leased Engine V2 effect-capability
actions, bound in the CodeAct sandbox and dispatched through the bridge before
tool execution. `tool_info` and the ordinary tool list enumerate a different
registry; their "not registered" result is a discovery false negative, not
proof that `mission_create(...)` is unavailable. Worker job contexts do not
gain mission effects from this clarification. The tool-discovery discrepancy
remains; this release does not claim a new `tool_info` fix.

### Guides and Architecture Docs Repass

All 32 active guides under `docs/guides/` (excluding `history/`) and all 10
active docs under `docs/architecture/` were re-verified against current source
and stamped for v2.1.4.0:

- Guides: substantive corrections beyond stamps – the gotify-wasm README's
  deprecated-path mention now resolves to `ic/scripts/setup-instance.sh`; the
  multica/darkirc/weechat stamps record the 2.1.4.0 version moves (registry
  multica 0.1.13 / multica-bridge 0.1.3, darkirc 0.1.10, weechat 0.2.11; WASM
  channel crates now at workspace `2.1.4`); distro package lists note the new
  `psql`/`daemon-pid` verbs add no host packages; GATEWAY_VOICE_API.md cites
  the speed-scaled pre-flight.
- Architecture: ENGINE-V2.md header revalidated for the cycle's engine work
  (global mission listings, provider recovery, mission-history lookup,
  prompt-overlay diagnostics, and new-tenant self-modification);
  SEMANTIC-MEMORY-SEARCH.md confirms V27 remains latest;
  IRC-SENDER-IDENTITY/WEECHAT docs re-verify principal formats in
  `ic/channels-src/{darkirc,weechat}/src/lib.rs`; XMPP transfer docs note
  registry 0.1.16 / bridge crate 2.1.4 with no protocol change;
  SELF_HEAL_COVERAGE_AUDIT re-listed live at 19 suites unchanged.
- Link checks: every relative link and cited `ic/`/`projects/` path in the
  active guides and architecture docs resolves.

### Kabuto: The Helmet

Ten release-codename candidates grounded in the cycle's actual work are
shortlisted in
[v2.1.4.0_CODENAME_IDEAS.md](docs/ops/v2.1.4.0_CODENAME_IDEAS.md), with no
previous codename repeated and every prior shortlist checked against the
history. We selected **Kabuto (兜, the helmet)** on 2026-10-02,
continuing `Yoroi` (2.1.2.0) → `Ō-Yoroi` (2.1.3.0): the helmet crowns the
armor. The longer lineage runs Tatara → Kiyome → Togishi → Sashiko → Odoshi →
Yoroi → Ō-Yoroi → Kabuto. The three haikus interspersed in these notes are
reproduced from the release-codename discussion. The selected [release
artwork](https://blog.lunarwing.org/assets/Kabuto.png) depicts a cyberpunk
kabuto bearing LunarWing's crescent-in-hexagon sigil.

### Repo Housecleaning and Agent-Doc Drift

Fourteen stale versioned docs moved from `docs/ops/` to `docs/ops/history/`
via `git mv` (rename tracking preserved) – the eleven `v2.0.9.0_*`
implementation-evidence docs plus three `v2.1.3.0_*` audit/status docs –
matching the convention prior releases already used, and the placeholder
`docs/ov/` directory was removed. Every inbound reference was repaired in the
same pass (guides, prior release notes, the v2.1.4.0 audit doc, the moved
docs' self-references, and a `ic/deny.toml` comment), and a repo-wide grep
confirms zero stale `docs/ops/v2.0.9.0_` / `docs/ops/v2.1.3.0_` paths remain.
The root and `ic/` `AGENTS.md` files also took a drift pass: the deeper-docs
lists now name only the five specs that exist in the tree, every relative path
resolves from its file's own directory, and the watchdog asset lists include
the launchd assets with consistent init-system guidance (systemd + OpenRC
first, launchd a distant third).

## Upgrade Notes

1. Back up PostgreSQL, tenant environment files, `/etc/lunarwing/ports.json`,
   workspace state, rendered units, worker data, voice models, and channel
   state.
2. No SQL migration is introduced; `V27` remains latest. The port registry
   stays at v17 – no registry migration on upgrade.
3. Rebuild the daemon and installed WASM extensions through the normal
   deployment path. All four channel registry versions changed (darkirc
   0.1.10, multica 0.1.13, weechat 0.2.11, xmpp 0.1.16) – behavior is
   unchanged, but the channels must be rebuilt/reinstalled. All nine tool
   registry versions advanced; rebuild/reinstall them together.
4. **Vendor snapshot.** `ic/vendor/` was regenerated for the 121 external
   crate updates and the later Wasmtime 36.0.17 security fix. Deploy a rebuilt
   daemon/bridge to pick up that runtime fix. Use
   `ic/scripts/refresh-vendor.sh` for dependency refreshes: rig-core carries a
   committed local patch, so commit its edits and regenerated checksums first;
   same-version refreshes restore it, while version bumps require porting the
   patch. Do not replace it with pristine registry sources.
5. **opencode2 rebuild.** Rebuild the opencode2 worker image at `v2.0.21`
   (`8a8bd622…`); pull worker images by the digest recorded in
   `worker-image-evidence.json`. The release image has completed the CI
   candidate-vs-clean rebuild digest validation.
6. **Pebble worker.** The reviewed upstream lock re-pins chacha20 to 0.10.2
   (retiring the yanked 0.10.1); the refreshed `PEBBLE_LOCK_SHA256` gate
   verifies it during build. No operator action beyond the normal image
   rebuild.
7. **Tool estimates.** `LUNARWING_TOOL_ESTIMATES` is read once at startup –
   set `1` (per-tenant env or render) to surface runtime hints after warmup;
   unset keeps observation-only recording; `0` disables both. Learned models
   reset on restart; no migration is needed.
8. **Tenant system prompts.** Editing a prompt in Settings writes the
   workspace file; Engine V2 consumes it as an initialization snapshot. After
   editing, run `restart-tenant` and start a new conversation to activate;
   existing threads keep their existing System message.
9. **New-tenant self-modification.** New tenants render
   `ORCHESTRATOR_SELF_MODIFY=true`. Existing tenants are unchanged; set the
   variable explicitly to opt in or out, and re-renders preserve it.
10. **Voice sidecar.** Deploy the updated sidecar image for bounded admission
    and per-component readiness. `component_failed`/503 with degraded
    readiness indicates a poisoned lock or native panic – recover with the
    documented whole-tenant `restart-tenant` procedure.
11. **Provider recovery.** No configuration change; recovery is bounded by the
    existing retry caps. Threads that previously aborted on transient provider
    failures now retry within those caps.
12. **Read-only diagnostics.** `psql`/`daemon-pid` are available through the
    deployed mt-admin; treat them as read-only operator discipline (not
    enforced). See the v2.1.4.0 prompt-overlay audit runbook for the
    inspection procedure.
13. The bundled-skills catalog is unchanged from v2.1.3.0 (exactly seven
    product skills); no skill migration is needed.
14. **Pairing.** No runtime change this release – revision 3 is a design
    document with landed pinning tests. Existing per-channel pairing JSON
    files keep working; this release does not migrate them to the secrets store.
15. **Outbound delivery semantics.** The `message` tool ack now reports
    `Queued message to <channel>:<target>`; success means the channel accepted
    the message for delivery, never that it arrived. Mission specs and
    operators must record acceptance, not "delivered" – see
    [OUTBOUND-DELIVERY-SEMANTICS.md](docs/architecture/OUTBOUND-DELIVERY-SEMANTICS.md).
    The XMPP channel refuses proactive enqueue while disconnected, and
    shutdown drains the outbound queue within a bounded 2s window. The XMPP
    lane now persists every outbound message to a durable spool before acking
    (see the XMPP outbound spool note below).
16. **Mission history.** In Engine V2 threads, use `mission_history` (mission
    names come from `mission_list`); `routine_history` now returns an honest
    pointer error there instead of a wrong-registry fallthrough. No
    configuration change.
17. **XMPP outbound spool.** No configuration change; the spool lives under
    the tenant's existing `omemo_store_dir` (`spool/` subdirectory). Delivery
    is at-least-once – a crash between wire-send and record-clear can replay a
    duplicate – and a message that exhausts its 24 retry attempts parks as
    `<key>.failed.json` with the last error for operator inspection/replay.
    The spool is XMPP-local; other channels keep memory-only queues.
18. **Daily-log injection scanning.** No configuration change. Agent daily
    logs may now quote short (≤200-byte) single-line injection-shaped text
    inline without a write rejection (warn-only describing context). Unquoted
    matches and tokenizer-level patterns still reject, and
    identity-file/lorebook writes are unchanged.
19. **Curated-memory writes.** No configuration change. `memory_write`
    payloads over 256 KiB are now rejected on every lane – agents that
    previously dumped large context blobs into curated memory must chunk
    instead. Full replaces of prompt-visible files keep the prior content at
    `backups/<path>.bak` in the same workspace scope; restore by reading that
    exact path. Backups are single-slot (each replace rolls the previous one
    away) and never search-indexed.
20. **Daily-log recovery.** If an append error says existing poison was
    quarantined, retry the clean entry to start the fresh day file; the
    rejected entry was not written automatically. Inspect the named
    `.poisoned-<n>.md` path to recover prior content. File/line detail helps
    manual repair when quarantine fails. No configuration change.
21. **Finish metadata.** Deploy the updated daemon and deployed
    `lunarwing-proxy.py` together for end-to-end preservation. Consumers must
    tolerate Unknown when a provider omits the reason; it no longer implies
    successful completion. This does not enable a new Engine V2 recovery
    ladder or change the outbound web compatibility mapping.

### Security, Deployment, and Release Limitations (carried)

- DarkIRC Secure Key Exchange Stages B–D and automated QR/web/trusted-channel
  delivery are disabled in this release (unchanged).
- MCP does not auto-install npm/pip packages. Core MCP configuration is not
  automatically inherited by external workers (carried).
- The onboarding web app remains a trusted local console without user accounts
  or TLS; keep its loopback binding and use an SSH tunnel for remote access
  (carried).
- OpenRC hosts without usable cgroup v2 delegation fall back to nice-only
  build scheduling (carried).
- `ALLOW_PRIVATE_IPS=1` opts out of public-address SSRF restrictions; enable
  only for intentionally trusted private-network deployments (carried).
- Kawarimi remains PostgreSQL-only (carried).
- Outbound delivery beyond XMPP still runs memory-only queues: the durable
  spool is XMPP-channel-local, so restart-loss of acked messages remains
  possible on other channels. On XMPP the spool is at-least-once – a crash
  between wire-send and record-clear can replay a duplicate (whole-message
  granularity). End-to-end delivery receipts (XEP-0198) and the engine-side
  mission-run ledger are not included in this release.
- The opencode lane stays on `v1.18.25`; OpenCode v1.18.29+ is not adopted
  in this release. The separate opencode2 lane is on v2.0.21.
- Historical terminal-trigger behavior remains: terminal triggers
  (`thread_completed_with_issues`/`thread_completed_with_learnings`) still
  fire for Research child threads – only Mission threads are filtered
  (carried).
- The secrets-store pairing redesign is included as design revision 3 with
  regression tests; this release does not implement the secrets-store
  substrate or migrate existing pairing data. The pairing
  stopgap tracker entry was closed on 2026-10-01 after the
  allowFrom-preservation fix landed (!1283).
- LLM timing, whole-job/mission ETA, gate-wait analytics, and durable
  aggregate caches are not included in this release's tool-runtime estimates.
- The broader daily-log detector redesign (line-start role labels plus a
  "resembles the live system prompt" provenance heuristic) is not included
  in this release's scan hardening.
  Daily-log recovery now quarantines pre-existing poison and permits a clean
  retry; quarantine failures can still require operator repair. The
  discovering append is rejected, and no operator-gated repair/overwrite verb
  was added.
- The provider-adapter update propagates finish metadata but does not
  implement a finish-driven Engine V2 recovery ladder. Unknown still maps to
  `stop` on the outbound web OpenAI-compatible surface;
  deprecated/experimental proxies retain their prior behavior.
- Source review confirmed mission effects were already callable in foreground
  Engine V2 threads; `tool_info` still cannot discover those effect actions.
  Tracker closure is not a claim that discovery was fixed.

### Resolved Since v2.1.3.0

- Real provider finish reasons survive blocking and compatible-stream
  adaptation and the deployed proxy; absent metadata remains Unknown, with
  no new finish-driven Engine V2 recovery policy included.
- Pre-existing daily-log poison is preserved in quarantine and a clean retry
  can resume the day; scan errors name offending lines.
- Wasmtime/WASI 36.0.17 clears RUSTSEC-2026-0321/0322/0323; same-version
  vendor refresh preserves the committed rig-core patch.
- The mission-capability concern closed after confirming existing Engine V2
  effect bindings; no runtime binding or tool-discovery fix is claimed.
- Bounded recovery for transient/unavailable provider failures after partial
  text; failed partial responses never enter history or tool execution (the
  v2.1.3.0 known issue is closed).
- Missions whose project record is missing/unparseable remain globally
  enumerable, keep rollover/event dispatch, and no longer pin
  `threads_today_date`.
- `routine_history` resolves Engine V2 mission names with character-safe
  summaries and registry-pointing errors (the v2.1.3.0 known
  issue is closed).
- Engine V2 missions have a first-class `mission_history` history tool with
  project/ownership-scoped resolution, and `routine_history` is honestly
  blocked in v2 threads with a pointer to it.
- Outbound delivery no longer over-reports: the `message` tool ack says
  `Queued …`, the XMPP channel refuses enqueue without a bound session, and
  shutdown drains the outbound queue in a bounded window with a WARN on
  residue. The channel-side durable spool then landed as described below; the
  engine-side mission-run ledger is not included in this release.
- XMPP outbound messages survive restarts: a durable spool persists every
  message before the ack, replays pending records on every Online bind,
  retries post-enqueue failures with bounded backoff, and parks exhausted
  records as `.failed.json` – verified live on derpy with 19/19 deliveries,
  including a restart-window replay.
- Daily-log writes no longer false-positive on quoted security phrasing: the
  workspace injection scan demotes High-severity matches fully enclosed in
  short, single-line, inline quoted spans to warn-only describing context,
  with the quote-pairing bypass closed, while unquoted and tokenizer-level
  matches still reject (via !1332).
- Curated-memory write hardening closes the append scan bypass and adds
  recovery for accidental full replaces: the `append_memory` lane is scanned
  pre-commit like every sibling lane, `memory_write` payloads are capped at
  256 KiB, and full replaces of prompt-visible files keep a rolling
  `backups/<path>.bak` shadow copy recoverable by exact-path read
  (via !1341/!1344).
- The voice sidecar bounds abandoned inference admission with per-component
  readiness and explicit permanent-failure semantics; supported recovery via
  `restart-tenant` (the v2.1.3.0 known issue is closed).
- Per-tenant system-prompt editing shipped with authenticated routes, scope
  isolation, and Engine V2 initialization-snapshot semantics (the v2.1.3.0 deferral is closed).
- The first EMA tool-runtime slice is live: bounded learning across shared
  execution/local jobs/routines, frozen advisory hints on approvals and
  pending gates, opt-in display, and `null` for missing redacted snapshots.
- mt-admin `psql`/`daemon-pid` read-only diagnostics plus the prompt-overlay
  audit runbook with uninspected-on-failure semantics and as-written PID
  validation.
- mt-admin surfaces tenant worker-image store failures with quoted stderr,
  verifies per-lane image presence after userns repair, and reports a `Worker
  images:` block in `status`.
- Approving a pairing sender no longer truncates the existing allowlist;
  rewrites are locked, seek-corrected, and synced.
- New tenants default to `ORCHESTRATOR_SELF_MODIFY=true` with the operator
  opt-out preserved.
- All LunarWing-owned packages at `2.1.4`; no project-owned package remains at
  2.1.3 (tracked manifest/lock audit). Cavepony release metadata aligned at
  0.4.4 across all five version fields with MR-base precedence regression
  coverage.
- The crate security audit re-verified every advisory ignore, scanned the
  resolved worker-image graph, and retired the tree's single yanked crate
  (chacha20 0.10.1 → pinned 0.10.2); the Pebble lock guard gained
  multi-version keying and no-op-override rejection.
- Integration-target MRs now run the full validation matrix
- opencode2 worker at v2.0.21 (reviewed commit, verified patch contract and
  bridge surface at both of the cycle's pins); kimi 2.1.1, opencode v1, and
  pebble pins unchanged.
- Documentation parity for the cycle: seven docs synced, four manual test
  tiers added, releases/docs indexes updated.
- All 32 active guides and all 10 active architecture docs repassed for
  v2.1.4.0 with every cited path verified against source.
- The pairing secrets-store design advanced to revision 3 with both !1294
  review P2s answered; the legacy-matching, concurrent-grant, and wasm
  host-path regressions are landed executable tests ahead of the substrate
  change.
- The cycle's open work items were reconciled in
  `docs/ops/v2.1.4.0_OPEN_WORK_ITEMS_STATUS.md` with tree-verified
  dispositions for the original 23 open items, then reviewed twice more (25
  open at the later source snapshot). Subsequent changes and the
  mission-capability concern's closure are recorded above; the pairing stopgap
  entry closed after its fix landed.
- The cargo-deny yanked-crate CI gate is honest again: it runs unvendored from
  the repo root and hard-fails on any `index-failure` diagnostic, closing the
  silent-skip hole found in pipeline 4811.
- Repo housecleaning archived fourteen stale versioned ops docs to
  `docs/ops/history/` with every inbound link repaired and removed the
  placeholder `docs/ov/`; the root and `ic/` `AGENTS.md` files took a drift
  pass so their spec lists and relative paths match the tree.

---

> What the helmet guards<br>
> was never the metal's strength:<br>
> the one who keeps watch.

兜
