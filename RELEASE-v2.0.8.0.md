# Release Notes for LunarWing v2.0.8.0 (FIRST DRAFT): Codename `TBD`

** Release Date: ** TBD (draft 2026-08-18)

> The codename for this release has not been chosen yet (see item 19 in
> `docs/ops/AGENT_GOALS_2.0.8.0.md`). This is a first draft; expect revision
> before the release ships.

## Overview

LunarWing v2.0.8.0 is a multi-tenant hardening, MCP marketplace-security, and
agent-resilience release built on v2.0.7.0 (`Mihari`). It brings hard cgroup
v2 resource ceilings and automated subtree delegation to OpenRC/Gentoo tenant
hosts, wires the MCP Part 2 trust stack (publisher signatures, dependency
locks, revocation distribution) into real activation paths, streams SSE
through the bundled MCP gateway, makes every agent termination path tell the
model what would unlock the call, and adds opt-in cross-tenant knowledge
sharing over the coordination pipeline.

The release and tag use `2.0.8.0`; LunarWing-owned Rust crates and channel
packages use `2.0.8`. Rust 2024 and MSRV `1.96.1` remain unchanged. There are
no new SQL migrations — `V26` remains the latest — and the multi-tenant port
registry stays at v15.

See the [v2.0.7.0 release notes](docs/releases/RELEASE-v2.0.7.0.md) for the
previous compatibility baseline.

## Highlights

- Automated OpenRC/Gentoo tenant cgroup isolation: `mt-admin setup-cgroups`
  renders boot-time pre-delegated cgroup v2 subtrees and a PAM login hook,
  `add-tenant`/`remove-tenant` keep them in sync, and tenant builds run
  bounded (memory.high throttle, memory.max OOM kill, capped swap) via a
  root-side `setpriv` migration — no tenant self-migration or chown needed.
- OpenRC tenants now get hard cgroup v2 memory/CPU/task ceilings (work item
  #54), closing the gap where only systemd had hard enforcement; hosts where
  delegation is denied fall back to nice-only build bounding with a loud
  reason.
- Missions gain a creation-time daily thread budget (`max_threads_per_day`,
  default 10 kept as a cost guardrail, 0 = unlimited), remaining-budget
  visibility on CLI and web list/detail surfaces, warn-level exhaustion logs,
  and precise not-fired reasons (work item #56).
- MCP Part 2 trust stack is integrated, not just library code: ed25519
  publisher signatures, signed dependency lockfiles, and signed revocation
  lists are enforced fail-closed on config load and startup activation; the
  bundled gateway control plane can be enabled at startup; generic external
  runtime descriptors are registerable via `mcp add --transport external`.
- MCP Part 3 hardens the bundled gateway data plane: true SSE streaming
  passthrough, separate transient/stream permit pools, request-body read
  deadlines, upstream userinfo rejection and log sanitization, and stricter
  bearer-token validation.
- Agent failure recovery with next steps: recoverable provider failures get
  one bounded recovery turn, the iteration limit gets one text-only final
  turn, denied approvals resume the loop with per-call denial results, and
  disabled/not-found/auth-required/rate-limited errors all state what unlocks
  the call.
- Cross-tenant knowledge sharing: opt-in `KNOW: <topic> | <finding>` room
  records propagate one durable finding into every watching tenant's `shared`
  study-notes scope as `needs-verification`, behind publisher allowlists and
  ingest budgets, safe on mixed-version fleets.
- Opencode external worker bumped to opencode `v1.18.18`, with the pinned ref
  baked into the image and exposed on the worker `/health` endpoint.

## Compatibility

| Area | v2.0.8.0 behavior |
|---|---|
| Release version | `2.0.8.0` |
| Crate/package version | `2.0.8` for LunarWing-owned release packages |
| Rust edition / MSRV | Rust 2024 / `1.96.1` |
| New SQL migration | None; `V26__study_notes_knowledge.sql` remains latest |
| Multi-tenant port registry | Unchanged at v15 |
| MCP negotiated version | Exactly `2024-11-05`; anything else fails before activation |
| Channel registry patches | darkirc 0.1.4, multica 0.1.7, weechat 0.2.4, xmpp 0.1.9 |
| LunarVision health | Dedicated operational listener (`8089` single-node); MT hosts discover per-tenant `vision_health` from the registry |
| LunarVoice health | Dedicated operational listener (`8091` single-node); starts fail loudly if the sidecar never goes healthy |
| CI scope | Source verification; no package publication or tenant deployment |

## Changes

### Multi-Tenant OpenRC/Gentoo Cgroup Isolation

- New `mt-admin setup-cgroups` command automates the pre-delegated subtree
  design from `docs/ops/PREDELEGATED_SUBTREE_CGROUPS.md`: a boot-time
  `/etc/local.d/lunarwing-cgroups.start` rendered from the ports registry and
  a `pam_exec` login hook wired into `system-login`/`sshd`/`su`/`login`
  (deliberately never `sudo`). `add-tenant` and `remove-tenant` keep subtrees
  and the boot script in sync automatically.
- Tenant builds are placed in the tenant's `build` cgroup via a root-side
  `setpriv` migration behind a bounded go-file rendezvous; bounding works on
  every OpenRC host regardless of PAM configuration. Verified live: su
  sessions land in `lunarwing-<t>/session`, `sudo -u` sessions stay out,
  memory.high throttling and memory.max OOM kills (rc=137) engage, and cgroup
  cleanup propagates real exit statuses.
- Hard cgroup v2 ceilings for OpenRC tenants (work item #54): direct memory
  values are converted to bytes, startup verifies every configured effective
  limit, build setup fails safely when memory enforcement is absent, real
  build exit status 2 is never rerun unbounded, and operator conf.d overrides
  survive atomic rerenders. Hybrid cgroup mode is detected and documented;
  when self-migration is denied the build falls back to nice-only with the
  reason reported.

### Missions

- `mission_create` accepts `max_threads_per_day` (default 10, 0 = unlimited).
- `mission_list` and the web mission list/detail surfaces expose
  `threads_today`, `max_threads_per_day`, and `budget_exhausted`.
- Budget exhaustion logs at warn with the mission name and counters (was
  debug-only and silent), and `mission_fire` reports the precise not-fired
  reason (terminal / paused / budget exhausted, noting the UTC-day reset).

### MCP Marketplace Trust and Gateway (Parts 2 and 3)

- Publisher signatures (ed25519 `verify_strict` over canonical manifests), a
  PublisherTrustStore, signed lockfiles pinning exact version+sha256,
  ManifestDiff upgrade diffs, and signed revocation lists with monotonic
  sequence numbers and atomic fsync persistence.
- Activation-path enforcement: the new `marketplace` section in
  `mcp-servers.json` makes every config load fail closed on revoked enabled
  servers and on corrupt trust stores/caches; startup pulls the newest signed
  revocation list over bounded HTTPS (fetch failures keep the cache) and
  skips newly-revoked packages; extension-manager activation re-checks the
  cached list per server.
- The bundled gateway control plane is enable-able at startup via
  `LUNARWING_MCP_GATEWAY_CONTROL_BIND` + `LUNARWING_MCP_GATEWAY_ADMIN_TOKEN`
  (loopback-only unless explicitly overridden); invalid config disables with
  a warning. Constant-time bearer auth, 3-header allowlist, routing-header
  stripping, redacted listings, HTTPS-only upstreams, bounded bodies/timeouts.
- External runtime descriptors: the `external_runtimes` section is validated
  at load and threaded through startup, extension-manager, and `mcp test`;
  `lunarwing mcp add --transport external` registers external servers with
  sandbox flags honored; the adapter verifies pinned provenance digests.
  `lunarwing doctor` flags unconfigured worker names and undeclared runtimes.
- Mandatory sandbox profiles: `require_sandbox_profiles` +
  `missing_mandatory_controls`, with the legacy opt-in default preserved.
- Gateway data plane (Part 3): Content-Type-keyed SSE streaming passthrough
  with no cumulative lifetime cap, clean-502 buffered responses,
  connect/time-to-headers timeouts, separate transient (64) and stream (64)
  permit pools so notification SSE cannot starve tool-call POSTs, a
  request-body read deadline (`408` on timeout), upstream userinfo rejection,
  sanitized upstream URLs in logs/client errors, a production SSE per-read
  idle timeout (default 300 s), and 1..=4096 visible-ASCII bearer tokens.

### Agent Failure Recovery With Next Steps

- New safe `recovery_kind` / `recovery_instruction` / `terminal_guidance`
  vocabulary in LLM errors; provider diagnostics are never copied into
  model-visible guidance (asserted by tests).
- The agentic loop grants one model-visible recovery turn for recoverable
  provider failures and one text-only final turn past the iteration limit;
  typed `LoopOutcome::Failed` carries terminal next steps.
- Denied approvals resume the loop with per-call denial tool-results
  (deferred batch calls resolved too) instead of ending the turn.
- Disabled, not-found, auth-required, rate-limited, and
  autonomous-unavailable errors all state what unlocks the call; hook
  rejections name the unlock path.
- External workers continue after unavailable-capability results, get a
  no-progress recovery turn (3 all-failed rounds → warning → typed failure),
  and lightweight routines mirror the one-shot provider recovery contract.

### Cross-Tenant Knowledge Sharing

- Opt-in `KNOW: <topic-slug> | <finding>` coordination records propagate one
  durable finding to every watching tenant daemon, each writing into its own
  database's `shared` study-notes scope as `needs-verification` with bounded
  sender provenance (`coord-<readable>-<96bit-hash>` agent namespace).
- Ingestion is a priority-13 fail-open hook with a publisher allowlist
  (`COORD_KNOWLEDGE_PUBLISHERS`, empty = deny all), per-sender and
  daemon-wide ingest budgets, and cancellation-safe detached ingest with
  stranded-write repair and metadata-failure rollback.
- Mixed-fleet safe: KNOW is schema-valid everywhere but emission is gated by
  `COORD_KNOWLEDGE_PUBLISH` at the prompt, tag-schema hook, message tool, and
  channel delivery boundary; knowledge tools mark coordination notes
  untrusted and wrap content before model exposure. mt-admin preserves and
  backfills all six `COORD_KNOWLEDGE_*` tenant env vars.

### CI, Dependencies, and Housekeeping

- Fixed the `rust-test-lib` / `rust-test-libsql` CI failures: the stateful
  MCP WAT fixture's byte-exact `initialize` envelope is now templated with
  placeholders rendered from the same `McpRequest::initialize` code path the
  transport uses, so it tracks `CARGO_PKG_VERSION` forever (4738 lib tests
  green, libsql-feature 4725 green).
- Fresh RustSec audit of all 19 tracked lockfiles: no new advisories; the
  same 7 accepted findings remain with no available upgrade clearing any of
  them. Details in `docs/ops/CRATE_SECURITY_AUDIT_v2.0.8.0.md`.
- Version bump: all LunarWing crates and WASM channels 2.0.7 → 2.0.8,
  channel registry patch bumps (darkirc 0.1.4, multica 0.1.7, weechat 0.2.4,
  xmpp 0.1.9), full `cargo update` on the main lockfile (28 packages, MSRV
  1.96.1-compatible), cargo-deny clean.
- Opencode external worker: `OPENCODE_REF` pinned to `v1.18.18` as a single
  global-scope ARG baked into the runtime image as `OPENCODE_IMAGE_REF` and
  surfaced on `/health` as `opencode_ref`; build contract re-verified against
  the canonical `anomalyco/opencode` tag.
- Repo hygiene: root branding assets moved to owned, descriptive paths (README
  and onboarding references updated), stale root `PEBBLE.md` archived, empty
  `.codex` markers and a stale resume artifact removed. Full ledger in
  `docs/ops/v2.0.8.0_REPO_HYGIENE.md`.
- CI now runs the full master job set on `aragents/kimil00p/*` branches.

## Upgrade Notes

1. Back up PostgreSQL, tenant env files, `/etc/lunarwing/ports.json`,
   workspace state, rendered units, worker data, voice models, and channel
   state.
2. No new SQL or port-registry migrations ship in this release; startup
   migration state from v2.0.7.0 (V26, registry v15) carries over unchanged.
3. OpenRC/Gentoo multi-tenant hosts: run
   `ic/scripts/lunarwing-mt-admin.sh setup-cgroups` once to render the
   boot-time subtree script and PAM hook, then re-add or re-render tenants so
   bounded builds and hard ceilings take effect. Review
   `docs/ops/PREDELEGATED_SUBTREE_CGROUPS.md` first on hybrid cgroup hosts.
4. To enforce MCP marketplace trust, populate the new `marketplace` section
   in `mcp-servers.json` (trust store, revocation cache, revocation URL and
   publisher key). Once configured, revoked or corrupt entries fail closed at
   load and activation. Leave it unconfigured to keep the previous behavior.
5. To run the bundled MCP gateway control plane, set
   `LUNARWING_MCP_GATEWAY_CONTROL_BIND` and a
   `LUNARWING_MCP_GATEWAY_ADMIN_TOKEN` (≥16 chars); it binds loopback unless
   `LUNARWING_MCP_GATEWAY_CONTROL_ALLOW_NON_LOOPBACK=1` is set.
6. Cross-tenant knowledge sharing is off by default; set
   `COORD_KNOWLEDGE_PUBLISH=1` and `COORD_KNOWLEDGE_PUBLISHERS` per tenant to
   opt in. Older daemons ignore KNOW records safely.
7. Rebuild the opencode external-worker image so the pinned `v1.18.18` build
   takes effect; check `/health` for `opencode_ref` afterward.

## Known Issues

### Security, Runtime, and Release Limitations

- Seven accepted RustSec advisories remain in the shipped graph, blocked by
  libSQL's legacy TLS stack and the vendored Signal wrapper; the v2.0.8.0
  re-audit found no newly fixable advisories and no available upgrade that
  clears any of the seven. See `docs/ops/CRATE_SECURITY_AUDIT_v2.0.8.0.md`.
- DarkIRC Secure Key Exchange Stages B-D and automated QR/web/trusted-channel
  delivery remain disabled; Stage A live-host release acceptance remains a
  release/deployment gate rather than proof of activated contact security.
- MCP still does not automatically install npm/pip packages. Worker-protocol
  MCP delivery is deferred (external-worker servers are config-validated but
  not pushed to `lunarwing-agent-v1` workers), and there is no signed-package
  marketplace install/upgrade CLI yet — trust primitives are enforced, but
  catalog install is manual. Custom MCP HTTP header values are still stored
  directly rather than through owner-scoped secret references. A stdio MCP
  bridge tool can be installed to use HTTP MCP servers with proper secret
  management.
- WASM channels do not surface tool-activity status events yet (work item
  #47; re-verified unimplemented in this cycle — guests drop ToolStarted /
  ToolCompleted / ToolResult and no per-channel `/debug` toggle exists).
- The XMPP bridge still cannot attach and send images correctly (work item
  #42); no fix ships in this release.
- The web UI still places its bearer token in native `EventSource` query
  strings. Keep access/proxy logs protected until short-lived SSE tickets or
  cookie-scoped sessions replace this transport.
- Onboarding remains a trusted local console without user accounts or TLS.
  Keep it loopback-bound with token authentication and use an SSH tunnel
  remotely.
- OpenRC hosts without a unified cgroup v2 hierarchy (or where delegation is
  denied) get nice-only build bounding instead of hard memory/CPU/task
  ceilings; mt-admin reports the reason loudly.
- `ALLOW_PRIVATE_IPS=1` weakens the public-address attachment boundary; use it
  only for trusted private networks.
- Engine output safety still truncates oversized prefixes before later
  redaction/policy transforms and does not re-cap every later transformation.
- `cargo check --tests --no-default-features --features postgres` currently
  fails on three pre-existing cfg(test) breakages on master (libsql-gated
  `test_db` helpers and a multi-tenant test harness builder); CI never runs
  that shape (postgres runs check-only, tests run under libsql), but a fix is
  still owed.
- Kawarimi remains PostgreSQL-only.
- Full release test aggregation remains blocked by unavailable real channel
  WASM artifacts and existing skill-selection/TensorZero streaming
  expectations.
- Current GitLab CI verifies source but does not publish release artifacts,
  packages, or tenant deployments.

### Resolved Since v2.0.7.0

- OpenRC's missing hard memory/CPU/task ceilings: cgroup v2 ceilings and
  automated pre-delegated subtrees now ship for multi-tenant OpenRC hosts
  (work item #54).
- The opaque 10-threads-per-day mission limit: budgets are now configurable
  at creation, visible on list surfaces, and loudly logged on exhaustion
  (work item #56).
- MCP Part 2's library-only trust stack: signature, lockfile, and revocation
  checks now run on real config-load and activation paths, and the bundled
  gateway control plane can actually be enabled.
- The bundled MCP gateway's buffered data plane: SSE responses now stream,
  and `listChanged` passthrough is no longer structurally blocked.
- The `rust-test-lib` / `rust-test-libsql` CI failures caused by the
  version-locked WAT fixture envelope.
