# Release Notes for LunarWing v2.0.5.0: Codename `Kagi (鍵)`

**Release Date:** 2026-08-05

> *Kagi* (鍵) means "key." LunarWing v2.0.5.0 hardens key exchange,
> explicit SSH trust, service recovery, coordination boundaries, and dependency
> security while keeping unfinished activation paths locked

## Overview

LunarWing v2.0.5.0 is a focused security, reliability, and operations release
built on v2.0.4.0. It hardens DarkIRC key exchange Stage A release selection and acceptance
tooling, expands the self-heal watchdog to v2.1.0, adds persistent user-selected
approval for the built-in SSH command tool, fixes coordination schema checks at
the inbound boundary, and refreshes dependencies across the repository.

The release and tag use `2.0.5.0`. LunarWing-owned Rust crates and channel
packages use `2.0.5`. Rust 2024 and the declared MSRV/toolchain of `1.96.1`
remain unchanged. No database migration is added in this release.

DarkIRC key exchange Stage A source hardening and local validation are complete, but release
acceptance is **still open**. Current immutable-release H3 runs on real OpenRC
and systemd-user hosts. Stage A does not activate contacts. Stages B-D and QR, web, or trusted-channel delivery
remain disabled.

See the [v2.0.4.0 release notes](docs/releases/RELEASE-v2.0.4.0.md) for the
previous compatibility baseline and the [release archive](docs/releases/README.md)
for older releases.

## Highlights

- Hardened DarkIRC Stage A around immutable, attested release publication,
  measured daemon execution, release selection, recovery inspection, legacy
  WeeChat cleanup, and real-host isolation/leak acceptance tooling.
- Expanded self-healing to v2.1.0 with per-service escalation cooldowns,
  retryable notification delivery, persistent flap history, matched systemd
  bus routing, LunarVision remediation, opt-in NetBird monitoring, and durable
  OpenRC worker stop intent.
- Added persistent explicit **Always** approval for the built-in `ssh` tool.
  Grants persist only after successful execution, remain unavailable to
  autonomous/shared-room contexts, and can be revoked through tool permissions.
- Fixed coordination so malformed inbound room traffic is rejected before model
  execution and violations are attributed to the actual sender rather than the
  receiving agent.
- Updated all tracked active Cargo lockfiles within current manifest constraints,
  bumped ten LunarWing-owned release packages to `2.0.5`, and upgraded the PDF
  parser stack to fix `RUSTSEC-2026-0187`.
- Hardened release and CI tests for DarkIRC on overlayfs runners, synthetic H3
  secret canaries, and gateway webhook polling under load.
- Fixed 25 of 26 web-gateway UI findings from `UI_FIX_SUGGESTIONS.md`,
  including vendored offline JavaScript dependencies, mobile and accessibility
  gaps, memory-editor safety, and an auth-expiry path; only the SSE
  token-in-URL item (L4) remains open.

## Compatibility

| Area | v2.0.5.0 behavior |
|---|---|
| Release version | `2.0.5.0` |
| Crate/package version | `2.0.5` for ten LunarWing-owned release packages |
| Rust edition / MSRV | Rust 2024 / `1.96.1` |
| New SQL migration | None |
| DarkIRC Stage A | Source and local gates complete; live-host release acceptance open; no contact activation |
| Built-in `ssh` approval | Explicit per-user **Always** grant available after first successful foreground call |
| `ssh_git` and WASM SSH approval | Explicit approval required for every call |
| Self-heal watchdog | v2.1.0; NetBird probe off and remediation disabled by default |
| Base systemd service scope | `SELF_HEAL_SYSTEMD_SCOPE=system` by default; `user` supported for single-node user units |
| Coordination schema | Inbound and outbound checks; actual inbound sender attribution |
| Web gateway UI | 25 of 26 suggested fixes shipped; L4 (SSE token in URL) open |
| CI scope | Source verification only; no package, release artifact, or tenant deployment job |

## Changes

### DarkIRC Stage A Acceptance Hardening

This release closes the known source-review findings around Stage A and adds
the acceptance machinery needed to validate the remaining deployment boundary.
It does not enable later key-exchange stages.

- Publishes daemon, helper, helper source revision, and attestation through one
  immutable, fsynced release layout. Readers pin one generation instead of
  mixing mutable paths.
- Executes the measured daemon through its validated descriptor and fails closed
  when release ownership, mode, links, digest, source revision, units, or systemd
  drop-ins do not match policy.
- Preserves digest-valid retained releases for rollback while requiring the
  current helper source for upgrades.
- Makes release selection robust on overlayfs-backed CI runners.
- Routes legacy WeeChat cleanup and adapter restart through mt-admin with
  systemd-user and OpenRC parity before upgrade work begins.
- Uses capability-relative, no-follow inspection for transaction directories;
  unsafe paths and I/O failures now fail closed.
- Strengthens semantic retry, migration locking, import pinning, generator
  process bounds, and all six Stage A completion crash windows.
- Adds `darkirc-stage-a-h3.sh` plus a structured analyzer for real two-user
  exchanges. The harness checks process identity, measured helper/daemon
  execution, cross-user denial, and private-key canaries across argv,
  environment, command output, artifacts, and init logs.
- Adds a narrower scratch-user live suite requiring operator-supplied,
  independently digested, root-owned daemon and helper inputs.

Local validation recorded on the acceptance-hardening baseline includes 89 core
key-manager tests, 22 helper tests, the full shell harness, locked Cargo checks,
all-feature Clippy with warnings denied, rustfmt, 11 H3 analyzer tests,
ShellCheck, and a final read-only review.

### Self-Heal Watchdog v2.1.0

- Adds per-service escalation cooldown state using readable service prefixes and
  SHA-256 digests, preventing collisions and state-path escape.
- Arms cooldown only after confirmed notifier delivery. Missing credentials,
  timeout, failure, or cooldown deferral remains retryable.
- Keeps dry-run state-preserving, including corrupt-state evidence.
- Retains in-window restart history after recovery so repeated fail/recover
  cycles still trigger flap protection.
- Adds optional NetBird readiness through `netbird status --check startup`.
  Probe is off by default; enabled checks are monitor/escalation-only unless
  `SELF_HEAL_NETBIRD_REMEDIATE=true` explicitly delegates daemon restart.
- Uses `SELF_HEAL_SYSTEMD_SCOPE` consistently for base-service detection and
  remediation while tenant units continue through the correct user manager.
- Detects and remediates registry-discovered LunarVision services. Active but
  unhealthy systemd containers become critical; OpenRC unit-to-tenant mapping
  remains intact.
- Persists OpenRC worker stop/kill intent across tenant starts and reboot,
  removes stale direct or babysitter runlevel links, and prevents health repair
  from resurrecting paused or unselected workers.
- Keeps escalation, service-manager, and tenant-routing behavior shared across
  systemd and OpenRC rather than adding wrapper-local init assumptions.

The canonical aggregate health suite recorded 435 passed assertions and zero
failures across regression, matrix, chaos, OpenRC, systemd, LunarVision, and
NetBird suites.

### Persistent Built-In SSH Approval

- The first built-in `ssh` call still requires explicit user approval.
- Interactive users may now choose **Always**. The grant persists only after the
  approved command succeeds; tool errors do not create future trust.
- The grant is per user and covers future built-in `ssh` commands across all
  configured host aliases. It is intentionally not command-scoped or host-scoped.
- Research, mission, autonomous, and shared-room contexts cannot use or create
  the grant. Shared-room approvals remain blocked; grant creation requires a
  direct interactive foreground thread.
- Global auto-approval cannot bypass the intrinsically gated SSH path or create
  an **Always** grant.
- Revocation through tool permissions restores `ask_each_time`; revocation wins
  over cached state and persistence failures fail closed.
- Configured-host allowlisting, host-key verification, output sanitization,
  command limits, and rate limiting remain active after a grant.
- `ssh_git` and WASM tools with SSH capability continue to require approval on
  every invocation.

### Coordination Inbound Enforcement

- Runs tag-schema policy at `BeforeInbound` priority 14 as well as the existing
  response-transform boundary, stopping malformed coordination traffic before
  model generation.
- Carries optional `sender_id` through inbound hook events and uses the actual
  remote actor for entropy, engagement, and tag-schema violations.
- Preserves XMPP MUC sender identity separately from room/storage target across
  the installable channel, bridge, and OpenClaw port, with backward-compatible
  payload decoding.
- Adds a room-scoped prompt policy requiring one-line tagged final output and
  suppressing persona or Markdown preambles in coordination rooms.
- Keeps outbound violations attributed once to the local agent, avoiding double
  strikes.
- Documents authenticated operator recovery through the loopback coordination
  administration endpoint without exposing token values.

This fixes the observed failure where short untagged XMPP room messages reached
the model, three generated persona replies were charged to the receiving agent,
and the receiving agent disabled itself for 24 hours.

### Web Gateway UI Fixes

This release ships 25 of the 26 findings in `docs/ops/UI_FIX_SUGGESTIONS.md`.
Only L4 (SSE bearer token in the URL query string) remains open for a later
transport change.

- Vendors DOMPurify and marked under `static/vendor/` with a non-empty
  fallback and a tighter Content-Security-Policy, so the UI no longer breaks
  in offline or private-network installs (H1).
- Mobile and accessibility pass: the Logs tab and a connection indicator are
  reachable at ≤768px, popovers work on touch and keyboard, tabs implement
  ARIA keyboard navigation, jobs/routines tables are usable on narrow screens,
  and toasts are exposed to assistive technology (H2, M1, M13, L6, L10).
- Memory editor safety: reads can no longer pair one file's content with
  another file's path, switching files warns on unsaved edits, and a tree load
  failure no longer looks like an empty workspace (H3, M12, L1).
- Routes native `alert()`/`confirm()`/`prompt()` calls through the app's modal
  and toast system and moves hardcoded English strings to i18n (M2, M3, L9).
- Logs auto-scroll only sticks when the user is near the top, and history
  messages show real server timestamps instead of page-load time (M4, M6).
- 401/403 responses route back to the auth screen instead of infinite
  reconnects and raw error toasts (M7).
- The restart progress modal gains a watchdog escape hatch (M8).
- `escapeHtml()` is now attribute-safe inside quoted attributes, and
  server-provided links use `rel="noopener"` (M9, L8).
- Retrying a failed message keeps its image attachments, and multi-file
  selection enforces the five-image limit (M10, M11).
- Aligns the jobs table header with its five-column rows (M5).
- Small cleanups: debounced log target filter, reconciled mobile tab-bar
  overflow rules, dead `hljs` loading removed, and background polling pauses
  in hidden tabs (L2, L3, L5, L7).

### Dependencies and Package Versions

- Bumped the main daemon, four workspace crates, XMPP bridge, and four WASM
  channel packages to `2.0.5`, including internal dependency constraints and
  channel registry versions.
- Refreshed all 18 tracked lockfiles. A final dry-run sweep of all 17 active,
  non-vendored lockfile roots reported no remaining compatible updates.
- Updated `pdf-extract` from `0.7.12` to `0.12.0` and `lopdf` from `0.34.0` to
  `0.42.0`, adding bounded nested-object parsing and fixing the untrusted-PDF
  stack-overflow denial of service in `RUSTSEC-2026-0187`.
- Removed the obsolete `RUSTSEC-2026-0187` exception from `ic/deny.toml`.
- Applied compatible transitive updates including
  `android_system_properties 0.1.6` and `keccak 0.2.1`.
- Recorded 42 direct dependencies that remain behind newer stable lines because
  each requires a manifest/API compatibility migration. High-value follow-ups
  include Wasmtime/WASI, reqwest/TLS, RustCrypto key crates, WebSockets, libSQL,
  and WIT guest bindings.

### CI, Tests, and Documentation

- Allows known synthetic H3 analyzer canaries in Gitleaks fixtures without
  weakening production secret scanning.
- Makes DarkIRC release-selector tests tolerate expected overlayfs behavior on
  CI runners.
- Makes gateway webhook workflow polling more tolerant of loaded CI hosts.
- Consolidates DarkIRC acceptance state into one current status ledger and
  updates operator, proposal, migration, SSH, health, and testing documents.
- Archives v2.0.4.0 release notes under `docs/releases/` according to release
  archive policy.

## Security Fixes

- Fixed `RUSTSEC-2026-0187` in the exposed PDF extraction path by moving to
  `lopdf 0.42.0` through `pdf-extract 0.12.0`.
- Hardened DarkIRC binary publication, attestation, measured execution,
  transaction inspection, migration locking, crash recovery, and live secret
  leak detection.
- Prevented global auto-approval or autonomous execution from manufacturing
  persistent SSH trust; grants require a successful explicitly approved call.
- Enforced coordination schema before model execution and attributed hostile or
  malformed inbound traffic to its actual sender.
- Prevented OpenRC health remediation from reviving intentionally stopped
  workers.
- Made escalation state collision-resistant and dependent on confirmed
  notification delivery.

## Upgrade Notes

1. **Back up tenant state before checkout.** Preserve PostgreSQL, tenant env,
   `/etc/lunarwing/ports.json`, workspace/state, rendered units, bridge/OMEMO
   state, worker data, and DarkIRC state. Normal upgrade wrappers do not back up
   every surface.
2. **No database migration is added.** Existing v2.0.4.0 schema remains valid,
   but all nodes should still be upgraded before mixed-version operational
   changes.
3. **Reinstall or re-render host health assets.** Review
   `SELF_HEAL_SYSTEMD_SCOPE`; multi-tenant host-global installs use system scope,
   while single-node user units can select `user`.
4. **Keep NetBird operator-owned unless explicitly delegated.** NetBird probing
   is off by default. Enabling the probe does not authorize restart unless
   `SELF_HEAL_NETBIRD_REMEDIATE=true` is also set.
5. **Review persisted SSH permissions.** Built-in `ssh` **Always** grants cover
   every configured alias for that user. Set `tool_permissions.ssh` to
   `ask_each_time` when broad persistent access is not intended.
6. **Recover coordination agents disabled by the old attribution bug.** After
   deployment, use the authenticated loopback coordination admin endpoint to
   clear affected agent state, then test with valid tagged input.
7. **Do not treat DarkIRC Stage A as activated contact security.** Keep Stages
   B-D and automated delivery disabled. Pending Stage A records remain
   secret-bearing state and can block managed migration or upgrade.
8. **Do not claim DarkIRC release acceptance from local tests.** Run current
   immutable-release H3 on real OpenRC and systemd-user hosts, retain only
   secret-free evidence, then run final release CI.
9. **Re-test PDF ingestion policy.** Parser behavior now returns a bounded error
   for deeply nested malicious PDFs instead of risking process stack overflow.
10. **Do not mistake CI for release delivery.** Current GitLab CI validates
    source but does not publish packages, create release artifacts, or deploy
    tenants.

### New and Changed Configuration Quick Reference

| Setting | Default | Effect |
|---|---:|---|
| `SELF_HEAL_SYSTEMD_SCOPE` | `system` | Selects `system` or `user` bus for non-tenant base LunarWing services |
| `HEALTH_NETBIRD_ENABLED` | `false` | Enables host NetBird readiness monitoring through `netbird status --check startup` |
| `HEALTH_NETBIRD_TIMEOUT` | `5` | Bounds NetBird readiness probe time in seconds |
| `SELF_HEAL_NETBIRD_REMEDIATE` | `false` | Allows NetBird daemon restart after NetBird checks are separately enabled |
| `tool_permissions.ssh` | `ask_each_time` until explicit grant | `always_allow` records user-selected persistent built-in SSH approval |

## Known Issues

### Release Acceptance and Planned Work

- **DarkIRC later stages remain disabled.** Stage B activation/rollback, Stage C
  peer verification, Stage D rotation/revocation/same-host pairing, and QR,
  web, or trusted-channel delivery are not implemented.

### Active Bug Tracker

| Area | Status | Current limitation |
|---|---|---|
| Agent/worker lifecycle | Partial | Default `wait=true` jobs still occupy the originating turn; ordinary messages wait in a bounded FIFO |
| External-worker config | Fixed, live check pending | Persistence and token re-injection are fixed, but live daemon/worker authentication re-check remains pending |
| Kawarimi import flags | Open | Explicit `--with-wasm` is rejected even though import adds the flag internally |
| WeeChat relay | Partial | `rand_check` still always returns false |

### Security and Runtime Limitations

- **Self-heal retains five intentional gaps.** Logical/init aliases are not
  normalized; tenant-looking units absent from the registry fall back to base
  scope; authenticated per-tenant application/channel health is deferred;
  NetBird has mock rather than live staging coverage; OpenRC and launchd
  behavior remains mock-tested in the canonical suite.
- **Onboarding remains a trusted local console.** It has no user-account system
  or TLS. Keep loopback binding and token authentication enabled; use an SSH
  tunnel for remote access.
- **MCP stdio remains trusted host code.** Package installation, provenance,
  revocation, host sandboxing, worker-local placement, MCP-over-WASM, and gateway
  architecture remain deferred. Custom MCP HTTP header values remain persisted
  directly rather than using owner-scoped secret references.
- **Engine output-safety ordering retains a residual limitation.** Oversized
  prefixes are truncated before later redaction/policy stages, while later
  transformed normal-sized output is not re-capped.
- **OpenRC lacks systemd-equivalent resource ceilings.** Portable process-tree
  cleanup exists, but memory, CPU, and task controls remain deployment-specific.
- **Private attachment endpoints require an explicit exception.** Setting
  `ALLOW_PRIVATE_IPS=1` weakens the public-address SSRF boundary and should be
  limited to trusted private networks.
- **Kawarimi remains PostgreSQL-only.** Machine migration requires staged
  downtime and has no automatic rollback after failed checkout/import.
- **Current CI is not a delivery pipeline.** Package creation, release artifact
  publication, and tenant deployment remain outside GitLab CI.

## Verification Summary

- Self-heal aggregate suite: 435 passed, 0 failed across seven suites.
- DarkIRC local acceptance gates: 89 core tests, 22 helper tests, full shell
  harness, 11 analyzer tests, locked check, all-feature Clippy, rustfmt,
  ShellCheck, and diff check passed on the acceptance-hardening baseline.
- Dependency audit: all 17 active non-vendored lockfile roots reported no
  compatible update remaining; default/all-feature workspace checks, focused
  PDF extraction test, XMPP bridge check, rustfmt, and GitLab config validation
  passed.
- Coordination repair: 27 focused coordination tests, four bridge-contract
  tests, daemon/channel/bridge checks, rustfmt, Clippy, and diff check passed.
- Web gateway UI branch: default check, all-target Clippy with warnings
  denied, rustfmt, and the full test suite passed after merge; gitleaks clean
  including the vendored JavaScript assets.
