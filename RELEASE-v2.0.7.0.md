# Release Notes for LunarWing v2.0.7.0: Codename `Mihari (見張り)`

**Status:** FIRST DRAFT (2026-08-12), review pass 2026-08-13 — pending final
update before tag

> *Mihari* (見張り) means "lookout" or "watchman." LunarWing v2.0.7.0 posts
> watchmen over its own machinery: supervised sidecars, health gates,
> strike-aware secret detection, deterministic responder election, and two
> self-audits of the dependency graph and the MCP runtime.

## Overview

LunarWing v2.0.7.0 is a reliability, supervision, and protocol-hardening
release built on v2.0.6.0. It fixes agent stalls after tool calls and
approval decisions, makes multi-agent coordination deterministic, closes the
audited MCP Part 2 gaps, and hardens LunarVision and LunarVoice lifecycle
management across both systemd and OpenRC hosts.

The release and tag use `2.0.7.0`; LunarWing-owned Rust crates and channel
packages use `2.0.7`. Rust 2024 and MSRV `1.96.1` remain unchanged. There are
no new SQL migrations — `V26` remains the latest — and the multi-tenant port
registry stays at v15.

See the [v2.0.6.0 release notes](docs/releases/RELEASE-v2.0.6.0.md) for the
previous compatibility baseline.

## Highlights

- Fixed agents occasionally getting stuck after tool calls or approvals:
  orchestrator v0 retains null/missing/interrupted structured results, denied
  approvals resolve the original call with an `ActionResult`, and
  `missing_tool_output` analysis pairs call IDs against the authoritative
  inference transcript.
- Hardened the multi-agent coordination protocol: deterministic cross-daemon
  responder election with tenant roster persistence, an auto-loaded managed
  seven-rule protocol bundle, strike-aware secret detection, and a uniform
  master switch across legacy and Engine V2 paths.
- Completed an MCP Part 2 gap audit and remediated all three P1 and five P2
  findings, including conservative protocol negotiation, bounded discovery
  refresh, generation-safe worker recovery, and atomic pinned stdio execution.
- Hardened LunarVision and LunarVoice across systemd and OpenRC: supervised
  health-gated lifecycles, dedicated operational listeners, crash-supervisor
  `-sup` units, and registry-aware port discovery with fail-loud container
  recreation.
- Fixed the GitLab WASM tool stalling on full work-item listings:
  `list_issues` now walks all pages in one bounded call (`all_pages`), and
  blank or repeated pagination headers no longer invite endless re-fetches.
- Migrated direct `reqwest` declarations from 0.12 to 0.13, collapsing the
  dependency graph to a single `reqwest 0.13.4` copy; refreshed lockfiles.
- Isolated universal WASM credential mappings from the built-in HTTP tool so
  one extension cannot trigger auth prompts or inject secrets for unrelated
  hosts; extension-local wildcard requests continue to work.

## Compatibility

| Area | v2.0.7.0 behavior |
|---|---|
| Release version | `2.0.7.0` |
| Crate/package version | `2.0.7` for LunarWing-owned release packages |
| Rust edition / MSRV | Rust 2024 / `1.96.1` |
| New SQL migration | None; `V26__study_notes_knowledge.sql` remains latest |
| Multi-tenant port registry | Unchanged at v15 |
| MCP negotiated version | Exactly `2024-11-05`; anything else fails before activation |
| MCP runtime registry | Internal closed dispatch boundary, not a plugin API |
| LunarVision health | Dedicated operational listener (`8089` single-node); API-port alias kept only for pre-v10 compatibility |
| LunarVoice health | Dedicated operational listener (`8091` single-node); per-tenant `voice_service`/`voice_health` ports from the registry |
| CI scope | Source verification; no package publication or tenant deployment |

## Changes

### Agent Stall and Approval Fixes

- Orchestrator v0 no longer drops null, missing, or interrupted structured
  tool results; denied approvals now resolve the original call with an
  `ActionResult` instead of leaving the turn hanging.
- `missing_tool_output` analysis pairs call IDs in the authoritative
  inference transcript, so only genuinely missing outputs are flagged.
- Monty normalizes tool results and closes interrupted action batches.
- Conversation-insight `thread_goals` now sort by `created_at` before the
  latest five are selected; `HybridStore::list_threads` previously iterated an
  unordered map, so the "recent" goals were arbitrary (work item #50; the
  2.0.5.0-era mission-goal pollution itself was already excluded by
  `cf4b1164`).
- Mission-only `__save_memory_doc__` access is confirmed as an intentional
  trust boundary; its real failure mode now raises explicit errors instead of
  returning silent `False`.
- Added embedded-Monty regression coverage plus an approval/auth matrix.

### Coordination Protocol

- Added deterministic cross-daemon responder election with explicit-routing
  and replay safety, persisted per tenant in a responder roster.
- The managed seven-rule protocol bundle now auto-loads; manual deployment is
  historical.
- Secret detection is strike-aware across explicit and entropy rules, with
  entropy checks deferred to the stateful hook; the coalescer validates its
  configuration.
- Legacy and Engine V2 paths honor disables and the master switch uniformly;
  agent targeting is safe by default with derived safe aliases.

### HTTP Credential Isolation

- Bare `*` credential mappings remain local to their owning WASM sandbox and
  are no longer published into the built-in HTTP tool's shared credential
  registry. Exact hosts and `*.domain` mappings retain shared auto-injection.
- This fixes the reproduced issue #48 failure: unrelated HTTP requests from
  WeeChat entered Engine V2's missing-credential flow because another
  extension had registered a universal host mapping. Tool execution and
  response routing were not channel-specific failure points.
- Regression coverage spans shared matching, HTTP approval classification,
  and Engine V2 auth preflight. Tool-activity status visibility remains tracked
  separately by issue #47.

### GitLab WASM Tool

- `list_issues` accepts `all_pages: true`, walking all pages in one bounded
  call (caps: 20 pages / 2000 items / 3 MiB source JSON) with
  `fetched_pages`/`truncated`/`resume_page` in the merged pagination object.
  Previously, "list all work items" forced one approved call per page, so
  bursts stalled mid-sequence (work item #49).
- Blank `x-next-page` headers on the last page are omitted instead of being
  surfaced as `"next_page": ""`, and a repeated next-page value terminates
  the fetch — together these close the silent re-fetch loop that the
  "always allow" approval setting had been masking. ("always" is not the
  fix; it only hides the prompts.)
- Registry entry bumped to 0.1.2; README documents `all_pages`.

### MCP Part 2 Remediation

- Initialization advertises empty client capabilities, and inbound JSON-RPC
  envelopes are classified strictly so server requests cannot be mistaken for
  responses. Negotiated version is exactly `2024-11-05`.
- Worker recovery: failed rebuilds no longer reset the stability window; the
  bounded circuit persists terminal state and atomically removes dead
  wrappers.
- Pinned Linux stdio activation executes verified, sealed native bytes via
  `memfd` + `execveat` with a 128 MiB streaming limit; shebang scripts and
  non-Linux pinning fail closed. Unpinned legacy stdio remains available.
- `tools/list` follows bounded pagination; negotiated `tools.listChanged`
  signals flow through stream transports and one bounded persistent HTTP
  GET/SSE listener into generation-safe wrapper refresh.
- MCP-over-WASM scope is explicit (one synchronous client-to-server tool
  actor) with a real source WAT fixture covering envelopes, state, pagination,
  resource limits, timeouts, and traps.
- Later-revision annotation fields deserialize canonically but remain
  untrusted advisory input; operator approval policy stays authoritative.

### LunarVision and Multi-Tenant Operations

- The API and dedicated operational listeners now fail together; JSON metrics
  are exposed beside `8089/health`, and single-node/legacy registry health
  discovery was corrected.
- mt-admin requires distinct registry ports and a healthy sidecar, recreates
  containers on stale image/env/port/runtime contracts, fails loudly on
  missing images or startup failures, preserves idempotent starts, refreshes
  managed assets through `render-units`, and stops supervisors before
  container mutation or migration.
- systemd uses health-aware tenant Quadlets with a supervised older-Podman
  user-unit fallback; OpenRC pairs endpoint-aware direct units with
  `podman wait` `-sup` crash supervisors.
- Standalone Compose/systemd/OpenRC assets bind loopback, keep tokens in
  validated env files, and health-gate startup.

### LunarVoice

- LunarVoice received the same supervision contract as LunarVision: mt-admin
  requires distinct valid `voice_service`/`voice_health` registry ports before
  any image or container mutation, tracks a launch contract plus stored config
  hash (image, env, ports, model store, runtime, init) so drifted containers
  are force-recreated, and health-gates every start path against the
  dedicated 8091 listener with loud failure otherwise.
- systemd tenants get a health-aware Quadlet plus a supervised `podman wait`
  fallback user unit; OpenRC tenants get an endpoint-aware direct unit paired
  with a `-sup` crash supervisor. Supervision stops before container mutation,
  stops verify the container actually went down, and
  disable/stop/uninstall/remove clean up the fallback unit, `-sup` supervisor,
  Quadlet, container, and stored hash on both inits.
- `render-units` refreshes existing voice assets without starting services
  and removes managed units when voice is disabled. Standalone assets were
  hardened to match (bounded restarts, sandboxing, mode-0600 secret-env
  validation, start_post health gate, Compose 8091 healthcheck).

### CI, Dependencies, and Housekeeping

- Shell harness fixes: the LunarVision init-template suite reports SKIP for
  the `systemd-analyze` parser check when the tool is absent, and the DarkIRC
  log-rotation test no longer depends on filesystem inode recycling.
- Fixed the ShellCheck SC2218 failures in the LunarVoice mt-admin tests with
  scoped annotations for the calls that intentionally use implementations
  imported from `lunarwing-mt-admin.sh`.
- Opencode external worker bumped to opencode `v1.18.9`; the build-time clone
  and all live references now point at the canonical `anomalyco/opencode`
  repository after the upstream transfer.
- Fixed CI secret-detection false positives and refreshed the coordination
  canary fingerprint.
- Crate security audit recorded no newly fixable advisories; direct `reqwest`
  declarations moved to 0.13, leaving one `reqwest 0.13.4` in the graph.
- Bumped LunarWing crates, WASM channels, and channel registry patch versions
  to 2.0.7 with refreshed lockfiles; ran `cargo update` and lockfile
  reconciliation.

## Upgrade Notes

1. Back up PostgreSQL, tenant env files, `/etc/lunarwing/ports.json`,
   workspace state, rendered units, worker data, voice models, and channel
   state.
2. No new SQL or port-registry migrations ship in this release; startup
   migration state from v2.0.6.0 (V26, registry v15) carries over unchanged.
3. Re-render LunarVision and LunarVoice units (`render-units`) so
   health-aware Quadlets, OpenRC `-sup` supervisors, and managed assets match
   the new contracts; stale containers are recreated automatically on the
   next start.
4. Rebuild the opencode external-worker image so the pinned opencode
   `v1.18.9` build from the canonical `anomalyco/opencode` repository takes
   effect.
5. Review MCP server pins: pinned stdio servers on Linux now execute through
   the sealed `memfd` path, and version negotiation accepts exactly
   `2024-11-05`. Servers pinned to other revisions must be re-pinned or run
   unpinned.
6. Coordination responder election is deterministic and roster-persisted;
   re-render tenant environments so the managed seven-rule bundle auto-loads.

## Known Issues

### Security, Runtime, and Release Limitations

- Seven accepted RustSec advisories remain in the shipped graph, blocked by
  libSQL's legacy TLS stack, unpatched RSA via russh, and the vendored Signal
  wrapper. See `docs/ops/CRATE_SECURITY_AUDIT_v2.0.7.0.md`.
- DarkIRC Secure Key Exchange Stages B-D and automated QR/web/trusted-channel
  delivery remain disabled; Stage A live-host release acceptance remains a
  release/deployment gate rather than proof of activated contact security.
- MCP does not automatically install npm/pip packages. Publisher signatures,
  dependency locks, marketplace revocation distribution, mandatory sandbox
  profiles, persistent external-worker placement, a bundled gateway
  control-plane service, and generic external runtime descriptors remain
  deferred. Custom MCP HTTP header values are still stored directly rather
  than through owner-scoped secret references.
- MCP verification gaps remain: bidirectional MCP-over-WASM and real guest
  interoperability, end-to-end shared gateway behavior, and cross-route
  session/OAuth isolation still lack live smoke coverage.
- Full release test aggregation remains blocked by unavailable real channel
  WASM artifacts and existing skill-selection/TensorZero streaming
  expectations.
- The web UI still places its bearer token in native `EventSource` query
  strings. Keep access/proxy logs protected until short-lived SSE tickets or
  cookie-scoped sessions replace this transport.
- Onboarding remains a trusted local console without user accounts or TLS.
  Keep it loopback-bound with token authentication and use an SSH tunnel
  remotely.
- OpenRC has portable process-tree cleanup but not systemd-equivalent hard
  memory, CPU, and task ceilings.
- `ALLOW_PRIVATE_IPS=1` weakens the public-address attachment boundary; use it
  only for trusted private networks.
- Engine output safety still truncates oversized prefixes before later
  redaction/policy transforms and does not re-cap every later transformation.
- Kawarimi remains PostgreSQL-only.
- Current GitLab CI verifies source but does not publish release artifacts,
  packages, or tenant deployments.

### Resolved Since the First Draft (2026-08-12)

The three items flagged as in progress at first-draft time are now done and
covered in the Changes sections above: the shell-harness `init-templates`
suite is green (24/24, with the `systemd-analyze` parser check skipping when
the tool is absent), the opencode external-worker polishing and 1.18.9 update
landed, and the LunarVoice systemd/OpenRC stability work landed.
