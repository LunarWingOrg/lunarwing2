# Release Notes for LunarWing v2.0.9.0 — FIRST DRAFT

## Codename: Inazuma (稲妻)

**Release date:** TBD

> *Inazuma* (稲妻) means lightning. LunarWing v2.0.9.0 concentrates on fast,
> decisive containment: untrusted tool output is neutralized before it can cross
> an LLM role boundary, restricted mission threads see only the actions their
> leases permit, self-repair reaches a terminal state instead of cycling, and
> multi-tenant operators get earlier warnings when ports are already occupied.

> **Draft status (2026-08-26):** This is the first release-note pass over the
> changes currently present after `v2.0.8.0`. Reconcile it with all commits and
> remaining items in `docs/ops/AGENT_GOALS_2.0.9.0.md` before tagging the
> release. It is intentionally not a claim that v2.0.9.0 is release-ready.

> **TODO** add the real intro from SB

## Overview

LunarWing v2.0.9.0 is a tool-output security, self-repair reliability,
least-privilege, and tenant-operations release. It closes tool-result safety
bypasses exposed by work item #75, including the Engine V2 structured-JSON path;
makes the repair watchdog stop retrying terminal failures while recognizing
healthy built-in tools; seeds newly provisioned Study Notes tenants once without
touching upgraded tenants; warns about host and registry port collisions; and
prevents mission threads from being offered `create_job` when their capability
leases do not permit it.

The release and tag use `2.0.9.0`; LunarWing-owned Rust crates and channel
packages use `2.0.9`. Rust 2024 and MSRV `1.96.1` are unchanged. There are no
new SQL migrations (`V26__study_notes_knowledge.sql` remains latest) and no new
multi-tenant port-registry migration (v16 remains current).

See the [v2.0.8.0 release notes](docs/releases/RELEASE-v2.0.8.0.md) for the
previous compatibility baseline.

## Highlights

- **Tool-result integrity across both agent engines.** Oversized output is now
  truncated before leak, policy, and prompt-injection scanning; tool failures go
  through the same sanitize-and-wrap path as successes; and ChatML-family role
  token openers such as `<|im_start|>` are neutralized unconditionally.
- **Engine V2 defense in depth.** Structured JSON successes are neutralized
  without losing JSON shape, failures are sanitize-wrapped, and every action
  result is neutralized again at the history-formatting sink. The sink also
  repairs unsafe role-token openers in previously persisted action history when
  that history is formatted again.
- **Self-repair converges instead of looping.** A tool in terminal
  `ManualRequired` state is surfaced once per boot and then removed from the
  retry set; repair-attempt counters start fresh once per daemon boot; healthy
  built-in tools self-verify without being sent to the WASM builder; and a
  successful tool call clears stale failure and repair counters.
- **Discriminated-union WASM tools advertise usable schemas.** If a compact
  advertised schema keeps the action discriminator but omits required fields
  from action variants, LunarWing now includes the `tool_info` hint. This fixes
  action-only calls such as `gitlab_tool` requests that omitted `project`.
- **One-shot Study Notes bootstrap for new tenants.** A tenant created with
  `add-tenant --with-study-mcp` can import the synced lw2studymcp note corpus
  into its private knowledge store on first startup, preserving canonical
  provenance and lifecycle metadata. Existing content is never overwritten,
  completed seeds do not repopulate, and upgrading an existing tenant does not
  arm the seed.
- **Port-collision visibility.** Fresh allocations warn when any assigned port
  already has a loopback listener, and `mt-admin doctor` reports duplicate port
  assignments inside the registry. Both checks are diagnostic and non-fatal;
  the allocator's stdout contract remains the bare base port.
- **Lease-scoped mission actions.** Engine V2 execution prompts expose only
  actions covered by the thread's valid capability leases. Mission and other
  restricted threads therefore no longer see or attempt `create_job`, while
  foreground threads retain it and late tool activation still works.
- **Safer default workspace guidance.** Profile updates now explicitly require
  `memory_write` with `append: false` and a complete merged JSON document, daily
  logs point to `daily/YYYY-MM-DD.md`, and the memory tool warns that appending
  to JSON can corrupt the document.

## Compatibility

| Area | v2.0.9.0 behavior |
|---|---|
| Release version | `2.0.9.0` |
| Crate/package version | `2.0.9` for LunarWing-owned release packages |
| Rust edition / MSRV | Rust 2024 / `1.96.1` |
| New SQL migration | None; `V26__study_notes_knowledge.sql` remains latest |
| Multi-tenant port registry | v16; no schema change from v2.0.8.0 |
| Channel registry patches | darkirc 0.1.6, multica 0.1.8, weechat 0.2.7, xmpp 0.1.11 |
| Tool registry patches | No change from v2.0.8.0 (GitLab remains 0.1.3) |
| Worker image moving tag | `v2.0.9.0` (plus `latest` and immutable commit-SHA tags) |
| CI scope | Source verification; no package publication or tenant deployment |

## Changes

### Tool-Result Safety and Role-Token Neutralization

Work item #75 began with a report of mangled, repeated error output containing a
SYSTEM-like ChatML sequence. The exact model-side degeneration was not reliably
reproducible, but the audit found deterministic safety gaps in LunarWing's
handling of error and oversized tool output.

- `SafetyLayer::sanitize_tool_output` now applies its configured length bound
  first and then runs secret-leak detection, policy enforcement, and injection
  scanning over the bounded result. Output larger than 100 KiB can no longer
  bypass all safety checks merely because it is large.
- `wrap_for_llm` always changes the special-token opener `<|` to the visually
  equivalent `<\u200B|` form. This is independent of the optional injection
  scanner and breaks the contiguous token recognized by ChatML-family
  tokenizers. The transform is idempotent and reversible by the existing unwrap
  path.
- The v1 chat dispatcher and shared `process_tool_result` helper now sanitize and
  wrap tool errors just like successful results. That covers worker jobs,
  container execution, and approval-resume callers of the shared helper.
- Engine V2's bridge neutralizes sanitized content before parsing successful
  JSON, preserving `serde_json::Value` structure while removing intact role
  delimiters. Its error path now sanitize-wraps the error text.
- Engine V2's `format_action_output` applies the same neutralization before
  truncation for every action-result shape. This second boundary protects
  internal and bridge results and cleans unsafe openers from old persisted
  action history when reused.

The software-builder loop remains a narrower exception: it handles
host-internal read/write/shell tools and host-generated errors without threading
through a `SafetyLayer`. It is not an equivalent untrusted WASM/MCP result path,
but unifying it is still a possible follow-up.

Evidence and detailed threat analysis:

- [`docs/ops/v2.0.9.0_GOAL1_TOOL_RESULT_INTEGRITY.md`](docs/ops/v2.0.9.0_GOAL1_TOOL_RESULT_INTEGRITY.md)
- [`docs/ops/ROLE_TOKEN_SMUGGLE.md`](docs/ops/ROLE_TOKEN_SMUGGLE.md)

### Self-Repair and Tool Schema Reliability

Work item #73's four reported failure modes are addressed:

- Exhausted `ManualRequired` repairs emit one warning and owner alert per boot,
  then leave the active repair set instead of logging and retrying every minute.
- `ToolFailureStore::reset_tool_repair_attempts` resets repair-attempt counters
  at daemon startup, providing a fresh bounded cycle after a restart without
  erasing failure history.
- Repair recognizes registered non-WASM built-ins. If a tool such as
  `memory_read` is registered and functional, self-verification clears the
  broken flag instead of invoking a WASM builder that cannot rebuild it.
- Successful tool execution records last-known-good state and resets both the
  error count and repair-attempt count while retaining history.
- WASM schema advertisement detects when a compact discriminated-union schema
  omits variant-required fields and appends a `tool_info` hint. This preserves
  full invocation guidance for tools such as `gitlab_tool` without expanding
  every schema unconditionally.

### New-Tenant Knowledge Store Seeding

`add-tenant --with-study-mcp` now does more than configure a read-only MCP
server: it arms a one-shot import into that newly created tenant's private,
native knowledge store.

- `KNOWLEDGE_SEED_FROM_STUDY_MCP=1` is written only by the new-tenant
  `--with-study-mcp` provisioning path. `configure-study-mcp`, routine env
  regeneration, and `upgrade-tenant` do not newly enable it.
- Startup imports valid notes from
  `$LW2STUDYMCP_HOME/agent-notes-by-agent/<agent>/<topic>.md` in a background
  task and preserves status, confidence, study time, source commit, tags, stale
  threshold, status reason, and supersession metadata.
- A `knowledge_seed_state.json` marker records completion. Later boots skip the
  pass, including when the shared checkout gains more notes.
- Existing note paths are not overwritten. Recovery is limited to rows stranded
  by the seed's own interrupted write sequence, and the reserved `coord-*`
  namespace is never imported.
- Live verification covered both primary init families: a fresh OpenRC tenant
  and a fresh systemd tenant were populated, while tenants created on an older
  branch and then upgraded remained unseeded with their prior workspace state
  unchanged.

Evidence:

- [`docs/ops/v2.0.9.0_GOAL3_KNOWLEDGE_SEED.md`](docs/ops/v2.0.9.0_GOAL3_KNOWLEDGE_SEED.md)
- [`docs/ops/v2.0.9.0_GOAL4_KNOWLEDGE_SEED_SYSTEMD.md`](docs/ops/v2.0.9.0_GOAL4_KNOWLEDGE_SEED_SYSTEMD.md)

### Multi-Tenant Port-Collision Diagnostics

Work item #65 closes an observability gap without changing allocation semantics:

- After allocating a fresh tenant block, mt-admin probes every numeric assigned
  port in the primary, extended, Kimi, and voice blocks on loopback. A listener
  produces a precise stderr warning naming the tenant, slot, and port.
- The probe validates numeric values before using them and never changes the
  allocator's stdout, so existing command substitutions continue receiving only
  the base port.
- A read-only registry scan finds ports assigned to more than one tenant slot,
  including the host-global `health_metrics` slot.
- `mt-admin doctor` includes a `no duplicate ports in registry` check.

These are warnings, not reservations. A process can still bind after the probe,
and an existing collision must still be resolved by the operator.

### Engine V2 Capability-Lease Enforcement

Engine V2 now separates two action catalogs that previously shared one view:

- Execution prompts and action lookup use the active thread's valid leases and
  expose only actions those leases cover.
- `ThreadManager` retains an unscoped registered-action catalog only for
  thread-type-aware lease planning and late-tool provisioning.

This prevents mission threads from seeing and attempting the administrative
`create_job` action when their lease intentionally excludes full job containers.
Foreground access, registered late tools, and thread-type-aware lease planning
remain intact.

### Workspace Prompt and Memory Guidance

The default system-prompt stack was checked against the current workspace
implementation. The runtime preamble remained accurate; the following seeded
and deployed workspace guidance changed:

- `context/profile.json` updates must use `memory_write` with `append: false` and
  contain the complete merged JSON document. The old instruction could append
  bytes to valid JSON, after which profile parsing and personalization silently
  stopped working.
- Bootstrap's profile-write step carries the same replacement requirement.
- The `memory_write.append` schema text warns against appending to any JSON
  document.
- `SOUL.md` now points daily notes to `daily/YYYY-MM-DD.md`, matching the real
  workspace and memory-tool path rather than the obsolete `memory/` prefix.
- Setup documentation correctly identifies `BOOTSTRAP.md` as a runtime seed,
  not a file shipped in `deploy/workspace-template/`.

System prompt and workspace documents were also preserved byte-for-byte during a
live `upgrade-tenant` test on systemd; the test tenant was removed afterward.
See
[`docs/ops/v2.0.9.0_GOAL16_SYSTEM_PROMPT_UPGRADE_SYSTEMD.md`](docs/ops/v2.0.9.0_GOAL16_SYSTEM_PROMPT_UPGRADE_SYSTEMD.md).

### Skill Attenuation Visibility

The reported inability to write `memory.md` without an unspecified environment
variable was not reproducible and no environment variable gates
`memory_write`. Registration, default permissions, approval behavior, routine
availability, Engine V2 exposure, gateway writes, and workspace writes are all
environment-independent.

The plausible hidden cause was confirmed as the intentional trust ceiling for an
active registry-installed skill: it can retain memory read/search tools while
attenuating `memory_write`. LunarWing now logs this per-turn tool attenuation at
`INFO` once, making the security decision visible without weakening it.

### Documentation and Release Housekeeping

- `docs/ops/` was reorganized around a maintained index and versioned history;
  completed checklists, audits, transcripts, and superseded plans moved under
  `docs/ops/history/`, and local links were repaired.
- Architecture, active bug reports, and proposals were re-verified against
  source. Corrections include Engine V2 leases, knowledge seeding, self-repair,
  XMPP attachment bounds, worker/SSH security boundaries, and the in-repo Rust
  OCR sidecar deployment path.
- LunarWing-owned crates and channel packages moved from 2.0.8 to 2.0.9;
  registry patches moved to darkirc 0.1.6, multica 0.1.8, weechat 0.2.7, and
  xmpp 0.1.11.
- Container CI's moving worker tag is now `v2.0.9.0`; `latest` and immutable
  commit-SHA tags remain in use.

## Upgrade Notes

1. Back up PostgreSQL, tenant env files, `/etc/lunarwing/ports.json`, workspace
   state, rendered units, worker data, voice models, and channel state before an
   upgrade.
2. No SQL migration or port-registry migration is introduced. `V26` and port
   registry v16 remain the current formats.
3. Rebuild the daemon, WASM channels, and selected worker images through the
   normal deployment path. Registry channel versions changed, and CI worker
   images now use the moving `v2.0.9.0` tag alongside `latest` and a commit SHA.
4. Existing tenants are deliberately **not** bulk-populated from lw2studymcp.
   One-shot native knowledge seeding is armed only for a newly created tenant
   using `add-tenant --with-study-mcp`. Do not manually add
   `KNOWLEDGE_SEED_FROM_STUDY_MCP=1` to an established tenant unless that import
   is explicitly desired and its existing note paths have been reviewed.
5. Port-listener findings during a fresh allocation are warnings. Run
   `ic/scripts/lunarwing-mt-admin.sh doctor`, inspect the named listener or stale
   registry entry, and resolve the collision before relying on the tenant.
6. Tool output may now be truncated, wrapped, redacted, or role-token-neutralized
   where v2.0.8.0 exposed raw error or oversized content. Integrations must not
   depend on model-visible tool errors remaining byte-for-byte unchanged.
7. Custom workspace instructions that update `context/profile.json` should match
   the new default: send one complete JSON document with `append: false`.
8. No operator action is needed for the mission lease correction or self-repair
   counter reset. After the first v2.0.9.0 restart, repair attempts begin a new
   bounded boot cycle and restricted threads receive the corrected action list.

## Known Issues

This list combines the active canonical bug tracker re-verified on 2026-08-25
with release limitations carried from v2.0.8.0 and checked against the current
v2.0.9.0 draft. The canonical status and reproduction details live in
[`docs/bugs/README.md`](docs/bugs/README.md).

### Active Runtime and Integration Follow-ups

- **Synchronous worker calls still serialize ordinary turns.** A built-in Docker
  `create_job` call with the default `wait=true` occupies its originating turn;
  priority interrupts work and ordinary messages enter a bounded FIFO, but
  ordinary turns do not execute concurrently. External workers default to
  `wait=false`. See
  [`BUG-agent-worker-lifecycle.md`](docs/bugs/BUG-agent-worker-lifecycle.md).
- **Some E2E coverage remains incomplete.** Bootstrap E2E scenarios are ignored
  because of a harness race, and clipboard runtime behavior remains unverified.
  See [`BUG-e2e-test-bugs.md`](docs/bugs/BUG-e2e-test-bugs.md).
- **External-worker token persistence lacks a final live re-check.** Source and
  round-trip tests preserve worker auth tokens across provisioning, merges, and
  TOML rewrites, but a live daemon restart plus authenticated worker request is
  still required before archiving the report. See
  [`BUG-external-worker-config-persistence.md`](docs/bugs/BUG-external-worker-config-persistence.md).
- **Kawarimi flag parity is incomplete.** Import still rejects an explicit
  `--with-wasm` even though the import path adds that behavior internally. See
  [`BUG-kawarimi-import-flag-parity.md`](docs/bugs/BUG-kawarimi-import-flag-parity.md).
- **SSH Git can accept an empty checkout after a bare-remote HEAD mismatch.**
  Null-like refs are fixed, but the remaining mismatch is not detected as a
  structured failure. See
  [`BUG-ssh-git-ref-and-remote-head.md`](docs/bugs/BUG-ssh-git-ref-and-remote-head.md).
- **WeeChat's `rand_check` is still a stub.** The unused import was fixed, but
  the function always returns false and should be implemented or removed. See
  [`BUG-weechat-relay-rand-check.md`](docs/bugs/BUG-weechat-relay-rand-check.md).
- **Worker workspace path normalization is partial.** Structured OpenCode paths
  expand `~`; prompt-generated paths and Pebble paths remain open. See
  [`BUG-worker-workspace-path-expansion.md`](docs/bugs/BUG-worker-workspace-path-expansion.md).
- **XMPP/WASM delivery retains a bounded-queue backpressure risk.** Poll-loop
  crashes and stalls are supervised and health-visible, but a full downstream
  queue can leave dispatch awaiting capacity indefinitely. The stale-poll health
  detector exposes the condition; it does not automatically restart a merely
  blocked task. See
  [`BUG-xmpp-polling-and-backpressure.md`](docs/bugs/BUG-xmpp-polling-and-backpressure.md).
- **Historical OMEMO MUC fallback spam is still unverified.** Generic stuck
  `Processing` recovery is fixed, but the cross-JID/plaintext-fallback symptom
  still needs a current live reproduction or closure. See
  [`BUG-xmpp-omemo-warmup-and-processing.md`](docs/bugs/BUG-xmpp-omemo-warmup-and-processing.md).

### Security, Deployment, and Release Limitations

- Eight accepted RustSec advisories remain in `ic/deny.toml`: libSQL's legacy
  TLS/tonic/h2 chain, the vendored Signal/failure dependencies, and RSA's
  unpatched timing advisory block removal or upgrades. The h2 0.4 line is
  patched; the accepted h2 0.3.27 copy is reachable through libSQL's legacy
  chain.
- DarkIRC Secure Key Exchange Stages B-D and automated QR/web/trusted-channel
  key delivery remain disabled. Stage A acceptance does not imply that contact
  security is fully activated.
- MCP does not automatically install npm/pip packages, has no signed-package
  marketplace install/upgrade CLI, and does not deliver configured MCP servers
  to `lunarwing-agent-v1` external workers. Custom HTTP MCP header values are
  still stored directly rather than through owner-scoped secret references.
- The web UI still puts its bearer token in native `EventSource` query strings.
  Protect browser history and access/proxy logs until short-lived SSE tickets or
  cookie-scoped sessions replace that transport.
- Onboarding remains a trusted local console without user accounts or TLS. Keep
  it loopback-bound with token authentication and use an SSH tunnel for remote
  access.
- OpenRC hosts without unified cgroup v2 delegation fall back to nice-only build
  bounding rather than hard memory, CPU, and task ceilings. mt-admin reports
  the fallback reason.
- `ALLOW_PRIVATE_IPS=1` weakens public-address SSRF protections. Enable it only
  for an intentionally trusted private-network deployment.
- `cargo check --tests --no-default-features --features postgres` retains the
  pre-existing cfg(test) failures described in the v2.0.8.0 notes; CI does not
  run that exact feature/test shape. PostgreSQL production check coverage and
  default/libSQL test coverage are separate jobs.
- Kawarimi remains PostgreSQL-only.
- Full release-test aggregation still depends on real channel WASM artifacts and
  environment-specific skill-selection/TensorZero streaming expectations.
- GitLab CI verifies source but does not publish release packages or deploy
  tenants automatically.
- Large worker-image pushes can still fail with registry `499 Client Closed
  Request` until proxy-chain read timeouts are raised on the GitLab host. See
  [`docs/ops/REGISTRY_PUSH_499.md`](docs/ops/REGISTRY_PUSH_499.md).

### Resolved Since v2.0.8.0

- Oversized tool output no longer skips safety scanning, and tool failure text no
  longer bypasses sanitize-and-wrap handling in the chat/shared execution paths.
- Engine V2 structured JSON and action history no longer preserve intact
  ChatML-family role-token openers from tool output.
- Terminal `ManualRequired` repairs no longer retry forever, repair counters no
  longer remain exhausted across daemon restarts, and healthy built-in tools are
  no longer sent to the WASM repair builder.
- Fresh tenant allocation now reports occupied host ports, and `mt-admin doctor`
  reports duplicate assignments in the registry.
- Mission threads no longer see `create_job` when their capability lease denies
  it.
- The reported environment-variable dependency for `memory_write` was ruled
  out; intentional installed-skill attenuation is now visible at info level.
