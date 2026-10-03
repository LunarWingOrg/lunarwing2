# Release Notes for LunarWing v2.1.4.0

## Codename: Kabuto (兜): The Helmet

**Release date:** 2026-10-03

> Beneath the helmet  
> the head that planned every plate  
> armor remembers. 兜

After Yoroi laced the plates and Ō-Yoroi closed the box, **Kabuto crowns the
suit**: the helmet protects the mind behind the armor.

## Overview

LunarWing v2.1.4.0 focuses on recovery, dependency security, and engine
reliability. It repairs mission execution and history lookup, bounds voice
inference admission, adds tenant system-prompt editing and tool-runtime
estimates, and strengthens daily-log and curated-memory writes. XMPP gains a
durable outbound spool, while provider finish metadata and message
acknowledgements describe their outcomes more accurately.

The release also updates Wasmtime/WASI, refreshes dependencies and worker
images, restores strict dependency checks in CI, and improves operator
diagnostics. No SQL or port-registry migration is required.

See the [v2.1.3.0 release notes](docs/releases/RELEASE-v2.1.3.0.md) for the
previous compatibility baseline.

## Highlights

- **More reliable missions.** Bounded recovery after interrupted provider
  responses, corrected global listings and daily rollover, and a dedicated
  `mission_history` tool.
- **Safer workspace memory.** Injection scanning covers the curated-memory
  append path; full replacements gain shadow backups; daily logs support
  narrowly scoped quoted context and quarantine of pre-existing poison.
- **Durable XMPP outbound messages.** Messages are persisted before queue
  acknowledgement and replayed after reconnect or restart. Acknowledgement
  means accepted for delivery, not delivered.
- **Better tenant controls and diagnostics.** System-prompt editing, opt-in
  tool-runtime hints, bounded voice inference, and clearer mt-admin status
  and failure reporting.
- **Security and build hardening.** Wasmtime/WASI 36.0.17, 121
  semver-compatible external crate updates, a full dependency re-audit, and
  CI checks that fail rather than silently skip registry-index failures.

> Fire bent the steel,  
> thread drew the plates tight together,  
> now the helm takes form. 兜

## Compatibility

| Area | v2.1.4.0 |
|---|---|
| Release / owned packages | Release `2.1.4.0`; all eleven LunarWing-owned release packages use `2.1.4` |
| Rust edition / MSRV | Rust 2024 / `1.96.1`, unchanged |
| SQL migrations | None added; `V27__memory_index_outbox.sql` remains latest |
| Multi-tenant port registry | v17, unchanged |
| WASM runtime | Wasmtime/WASI 36.0.17; matching Cranelift family 0.123.17 |
| Channel registry | darkirc 0.1.10, multica 0.1.13, weechat 0.2.11, xmpp 0.1.16 |
| Tool registry | cavepony-tool 0.4.4, gitlab 0.1.7, gotify 0.1.1, llm-context 0.1.2, multica-bridge 0.1.3, ssh 0.1.2, vision-analyze 0.1.4, voice-transcribe 0.1.2, web-search 0.2.3 |
| External workers | opencode2 `v2.0.21`; kimi-code `2.1.1`, opencode `v1.18.25`, and Pebble `2d0f3786…` otherwise unchanged |
| Bundled skills | Seven product skills, unchanged; no skill migration |

Registry version bumps require rebuilding/reinstalling the affected WASM
extensions. The channel version bumps themselves do not introduce protocol
changes. The pairing storage format remains per-channel JSON; there is no
secrets-store migration in this release.

## Changes

### Provider Finish Reasons and Mid-Stream Recovery

Blocking responses and the deployed OpenAI-compatible streaming path preserve
actual provider finish metadata instead of inferring successful completion.
Absent or unrecognized reasons become `Unknown`; actual tool calls can
establish `ToolUse`. The deployed `lunarwing-proxy.py` omits absent upstream
reasons rather than substituting `stop`.

The lightweight routine engine now classifies `Length` as
`TruncatedResponse` rather than `EmptyResponse`. Engine V2 receives the
metadata through the bridge, but this release does not add a finish-reason-driven
recovery policy.

Separately, mission turns interrupted after partial text now attempt bounded
recovery for transient or unavailable providers. Permanent failures do not
qualify; existing retry caps and rate-limit exclusions remain enforced.
Failed partial responses never enter history or tool execution. Attempts
accumulate as provisional Live work without resetting per retry; only
successful responses are persisted, with usage accounted once.

### Daily-Log Scanning and Recovery

Daily-log writes can describe injection-shaped security text without
unnecessarily rejecting the entire entry. In `daily/*.md`, a High-severity
match fully enclosed in a short, single-line inline quotation of at most 200
bytes is treated as warn-only describing context. Supported quotations
include backticks, straight quotes, and curly quotes. Quote-boundary checks
prevent mismatched quotes from being re-paired around injection text.

Unquoted matches, oversized or multiline spans, and tokenizer-level patterns
still reject. Identity files and lorebooks retain the strict raw scan.

When a clean append encounters pre-existing poisoned daily content,
`Workspace::append()` preserves the old content at the first free
`daily/YYYY-MM-DD.poisoned-<n>.md` path and clears the original day file.
**The discovering append is rejected with a retry hint; the clean entry is
written only when retried.** Quarantine copies remain listed and readable by
exact path, but are excluded from search indexing and automatic daily-log
prompt injection.

A poisoned new entry or a pattern formed only across the append boundary
rejects without quarantining the file. Failed quarantine retains the original
rejection and can require operator repair. Scan errors now identify the
file and 1-based offending line.

### Wasmtime Security and Vendor Patch Preservation

Wasmtime/WASI advances from 36.0.16 to 36.0.17, with matching Cranelift
0.123.17, in the workspace and XMPP bridge lockfiles. The update addresses
RUSTSEC-2026-0321, RUSTSEC-2026-0322, and RUSTSEC-2026-0323, covering fuel
accounting, excessive host-memory use, and uninitialized padding exposure.
No advisory ignore was added.

`ic/scripts/refresh-vendor.sh` now checks the committed rig-core patch and
its marker before vendoring, then restores the patched tree and checksums
when the version is unchanged. Uncommitted patch edits or a rig-core version
change stop the refresh rather than silently discarding the finish-reason
patch. See [LUNARWING-PATCH.md](ic/vendor/rig-core/LUNARWING-PATCH.md).

### Mission Listings and History

Global mission listings now enumerate the missions map directly. Missions
whose `project.json` is missing or unparseable remain visible, allowing
active missions to receive startup/tick rollover and event/webhook dispatch
instead of leaving `threads_today_date` stuck at creation.

The new `mission_history` tool returns newest-first runs with character-safe
200-character summaries, a default limit of 10, and a limit range of 1–50.
Name lookup is scoped to owned and shared missions in the caller's project;
UUID lookup re-checks project and ownership after loading the mission.

Legacy `routine_history` falls back to Engine V2 mission-name lookup when
the v1 registry misses, searching the default project's owned/shared missions
before an ownership-filtered global scan. Summary truncation is character-safe,
including multilingual text. **Inside Engine V2 threads, use
`mission_history`: `routine_history` now returns an explicit pointer to it
instead of querying the wrong registry.** Mission names come from
`mission_list`.

### Voice Sidecar Inference Admission

STT, TTS, and VAD each admit one call before acquiring shared capacity.
Blocking workers retain both permits until settlement, so repeated TTS
retries cannot build up behind the model mutex or consume the remaining
STT/VAD capacity. Caller cancellation, timeout, and worker settlement share
a phase lock.

`/ready` and `/metrics` expose per-component activity, current and cumulative
abandonment, and permanent failure. Lock contention remains transient Busy;
poisoned model locks and native panics return `component_failed`/503 and keep
readiness degraded until restart. A native call that never returns still
retains one permit. The supported recovery is **`restart-tenant`, which
restarts the whole tenant**.

### Per-Tenant System Prompts and Self-Modification

Settings → System Prompt adds primary-scope source editing, missing-file
creation, preservation of unsaved edits after failed saves, and a saved
workspace preview with group/timezone controls. Authenticated `/api/prompts`
routes retain scope isolation, workspace write validation, and indexing.

Engine V2 consumes the prompt as an initialization snapshot. To activate an
edit, run **`restart-tenant` and start a new conversation**; existing threads
retain their System message. See
[TENANT-SYSTEM-PROMPTS.md](docs/guides/TENANT-SYSTEM-PROMPTS.md).

New tenants render `ORCHESTRATOR_SELF_MODIFY=true`, enabling stored
orchestrator and prompt-overlay updates. Explicit operator values, including
`false`, survive re-renders. Upgrading does not change existing tenants'
settings.

### Tool Runtime EMA Estimates

Bounded, per-user/tool exponential moving average (EMA) learning now uses
successful completions from shared tool execution, local jobs, and
lightweight routines. After a five-sample warmup, advisory runtime hints can
appear on v1 approvals and Engine V2 pending gates. Forecasts are frozen when
displayed, with numeric guards and bounded outlier influence. Failures,
timeouts, cancellations, and approval/authentication waits do not train
completed runtime.

`LUNARWING_TOOL_ESTIMATES` is read at startup: unset or `observe` records
only; `1` records and displays hints; `0` disables recording and new hints.
Learned models are in memory and reset on restart. Persisted pending gates
retain their original descriptions, including any hint.

Missing redacted parameter snapshots now display `null` rather than exposing
raw execution parameters. No database migration or new wire fields are
required.

### mt-admin Diagnostics and Worker-Image Reporting

New `psql <tenant>` and `daemon-pid <tenant>` verbs provide container-based
tenant database access and strict, tenant-associated PID-record validation.
They are intended for read-only diagnostics; **read-only use is operator
discipline, not an enforced restriction**.

The prompt-overlay audit runbook supports inspecting an agent's effective
system prompt without a Git diff or enabling self-modification. Failed
environment capture reports `uninspected (live gate)` with a nonzero exit,
not a false claim that the gate is off. Environment values are reduced to
presence/absence before printing.

Worker-image distribution captures stderr from both `podman save` and
`podman load`, distinguishing missing images from store or runtime failures.
User-namespace repair checks image presence for each selected worker lane
and reports degraded status when images are missing. `mt-admin status
<tenant>` adds a `Worker images:` block with `present`, `missing`, or
`not selected for tenant` per lane.

### Dependency Audit and CI Hardening

All 27 tracked lockfile roots were audited against the September 30 RustSec
database with an empty advisory ignore list and `unmaintained = "all"`.
The resolved worker-image dependency graph was checked with patched and
unpatched controls, and every registry-locked version was included in the
yanked-crate pass. The eight existing `ic/deny.toml` advisory ignores were
re-verified and remain unchanged; the configured unmaintained policy remains
`workspace`.

The tree's single yanked dependency, Pebble's chacha20 0.10.1, is replaced
with 0.10.2 and a refreshed `PEBBLE_LOCK_SHA256`. The Pebble lock guard now
keys records by `(name, version, source)`, compares complete records, rejects
duplicates, and rejects overrides that make no change.

The cargo-deny job now runs unvendored from the repository root through
`.gitlab/scripts/cargo-deny-check.sh`, with explicit manifest/config paths
and compact diagnostics. Any `index-failure` is fatal, preventing vendored
source replacement from silently disabling the yanked-crate check. Vendor
refresh uses the same wrapper.

CI recovery aligns all five Cavepony release-version fields at 0.4.4 and
moves the orphan-mission regression to the existing database test fixture,
without weakening architecture boundaries. Full validation covers
integration-target merge requests; scratch branches can opt in with
`LUNARWING_CI_FULL_VALIDATION=1`. Worker publication remains protected and
push-only, validation caches remain pull-only, and existing build, fuel,
artifact, and boundary gates are retained.

Release validation is complete, including remote CI, the published
worker-image Trivy scan, and the opencode2 candidate-versus-clean rebuild
digest check.
See the
[crate-security audit](docs/ops/v2.1.4.0_CRATE_SECURITY_AUDIT.md).

### Pairing Allowlist Preservation

Approving a sender no longer truncates the allowlist before reading it.
Reads and rewrites use the same locked file handle, with explicit truncation,
seek, write, and sync during replacement. Existing grants survive new and
concurrent approvals, and shorter payloads cannot leave trailing bytes.
Regression tests also pin legacy username/trimmed-ID matching and the WASM
`pairing_is_allowed` host path.

### Outbound Message Acknowledgements and Durable XMPP Spool

The `message` tool now returns `Queued message to <channel>:<target>`.
**Success means acceptance for delivery, not confirmation that a recipient
received the message.** Mission records must reflect that distinction.

XMPP rejects proactive enqueue when no session is bound. Shutdown provides a
bounded two-second drain window and warns about messages left spooled.
Outbound messages are persisted under `<omemo_store_dir>/spool/` before
queue acknowledgement, using atomic writes, fsync, and rename with directory
permissions 0700 and file permissions 0600. A spool-write failure refuses
acknowledgement.

The client atomically claims records before sending, removes successful
records, and replays pending messages on every Online bind after room joins
and OMEMO publication. Post-enqueue failures retry with exponential backoff
from 30 seconds to one hour. After 24 attempts, records are parked as
`<key>.failed.json` with the last error for operator inspection/replay;
invalid JIDs are parked immediately. Internal protocol messages such as
pairing prompts are not spooled, and replay does not reapply the rate limiter.

**Delivery is at-least-once, not exactly-once.** A crash after wire-send but
before record removal can duplicate a whole message. The spool needs no new
configuration and protects queued XMPP messages across restart or rebuild.
See [OUTBOUND-DELIVERY-SEMANTICS.md](docs/architecture/OUTBOUND-DELIVERY-SEMANTICS.md).

### Curated-Memory Write Hardening

`memory_write(target: "memory", append: true)` now scans both the new entry
and combined candidate before committing, closing the `append_memory` scan
bypass. Every `memory_write` lane rejects payloads over **256 KiB**.

Full replacements of prompt-visible files with nonempty, differing prior
content first attempt to preserve that content at `backups/<path>.bak` in
the same workspace scope. These are rolling, single-slot shadow backups:
each replacement supersedes the previous backup. They are readable by exact
path, never search-indexed, and are not rescanned, allowing poisoned content
to be replaced. Backup failures are logged and do not block the write.

### Package and External-Worker Updates

All eleven LunarWing-owned release packages advance to `2.1.4`: the daemon,
five workspace crates, XMPP bridge, and four WASM channels. The release
includes 121 semver-compatible external crate updates across the workspace
and satellite lockfiles, with a regenerated `ic/vendor/` tree.

The opencode2 worker advances from v2.0.18 to **v2.0.21**, pinned at
`8a8bd622a3d7dc29ccf30ec17f84e363ed95ed72`. Dockerfile pins, CI build
arguments, and reproducibility fixtures moved together. The required
`bytecode: true` / `splitting: true` patch contract and used bridge endpoints
remain compatible. Other worker versions are listed in Compatibility; the
Pebble lock override changes without changing its source pin.

### Pairing Secrets-Store Design

Design revision 3 specifies separate canonical-principal and legacy
matchers, keyed HMAC-SHA256 grant names with key separation, and a bounded
per-channel decoded-grant cache. The design documents a 15-second cache TTL,
approval invalidation, and a 1,024-grant bound with uncached fallback.

**This is a design update, not a storage implementation or migration.**
Current matching and concurrent-grant behavior are protected by regression
tests. Details:
[PAIRING_SECRETS_STORE_DM_AUTH.md](docs/proposals/PAIRING_SECRETS_STORE_DM_AUTH.md).

### Documentation and Repository Maintenance

All 32 active guides and 10 active architecture documents were checked
against source and updated for this release. Operator references, tenant
configuration, testing guidance, onboarding, and release indexes were
synchronized. Tracker dispositions were reconciled against the source tree.

Fourteen stale versioned operations documents moved to `docs/ops/history/`
with inbound links repaired, and the placeholder `docs/ov/` directory was
removed. Root and `ic/` `AGENTS.md` references now match existing specs,
relative paths, and init-system assets.

## Upgrade Notes

1. **Back up state.** Preserve PostgreSQL, tenant environment files,
   `/etc/lunarwing/ports.json`, workspace state, rendered units, worker data,
   voice models, and channel state. SQL remains at V27 and the port registry
   at v17; neither needs migration.
2. **Deploy rebuilt components together.** Rebuild the daemon, XMPP bridge,
   installed WASM channels and tools, and deploy the updated voice sidecar.
   Rebuild/reinstall all four channel and nine tool registry updates through
   the normal deployment path. Deploy the updated `lunarwing-proxy.py`
   alongside the daemon to preserve provider finish metadata.
3. **Refresh worker images.** Rebuild opencode2 at v2.0.21 and the Pebble
   worker with its updated lock override. Pull worker images by the digest
   recorded in `worker-image-evidence.json`; the Pebble build verifies
   `PEBBLE_LOCK_SHA256`.
4. **Preserve the vendor patch.** Use `ic/scripts/refresh-vendor.sh` for
   dependency refreshes. Commit rig-core patch edits and regenerated
   checksums first; same-version refreshes restore them, while version
   changes require porting the patch. Do not replace the patched tree with
   pristine registry sources.
5. **Choose tenant settings.** Set `LUNARWING_TOOL_ESTIMATES=1` before
   startup to show runtime hints after warmup; unset records only and `0`
   disables both. Existing self-modification settings are unchanged; set
   `ORCHESTRATOR_SELF_MODIFY` explicitly to opt in or out. After changing
   a system prompt, run `restart-tenant` and start a new conversation.
6. **Apply the updated tool contracts.** Use `mission_history` in Engine V2
   threads and record outbound messages as queued, not delivered. Keep
   `memory_write` payloads within 256 KiB, splitting larger writes as needed.
   Retry a clean daily-log append when the error reports successful
   quarantine; the rejected entry was not written automatically. Read
   shadow backups and quarantined logs by their exact paths to recover prior
   content.

### Security, Deployment, and Release Limitations

**Delivery and channels.** Durable spooling is XMPP-local; other channels
retain memory-only queues and can lose acknowledged messages on restart.
End-to-end delivery receipts and an engine-side mission-run ledger are not
included. DarkIRC Secure Key Exchange Stages B–D and automated
QR/web/trusted-channel delivery remain disabled.

**Engine behavior.** The outbound web OpenAI-compatible surface still maps
`Unknown` to `stop`; deprecated and experimental proxies are unchanged.
Foreground Engine V2 mission effects were already callable through CodeAct
and the bridge, but `tool_info` and the ordinary tool list use a different
registry and cannot discover those effect actions. This release adds no new
mission binding or discovery fix, and worker job contexts do not gain mission
effects. Terminal triggers `thread_completed_with_issues` and
`thread_completed_with_learnings` still fire for Research child threads;
only Mission threads are filtered.

**Feature scope.** Runtime estimates do not cover LLM timing, whole-job or
mission ETA, gate-wait analytics, or durable aggregate caches. The broader
daily-log/curated-memory detector redesign and an operator-approved
repair/overwrite tool are not included. Worker-image diagnostics do not add
container recreation based on image-ID changes. The opencode lane remains
on v1.18.25; v1.18.29+ is not adopted.

**Deployment boundaries.** The onboarding web app is a trusted local console
without user accounts or TLS: retain loopback binding and use an SSH tunnel
for remote access. `ALLOW_PRIVATE_IPS=1` disables public-address SSRF
restrictions and is only for intentionally trusted private-network
deployments. MCP does not automatically install npm/pip packages, and
external workers do not automatically inherit core MCP configuration.
OpenRC hosts without usable cgroup v2 delegation use nice-only build
scheduling. Kawarimi remains PostgreSQL-only.

---

[Release artwork](https://blog.lunarwing.org/assets/Kabuto.png)

> What the helmet guards  
> was never the metal's strength:  
> the one who keeps watch.

兜
