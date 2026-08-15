# MCP Overall Status

**Status as of 2026-08-11:** MCP work is partial overall. Part 1 host-local
lifecycle, the internal built-in runtime-dispatch boundary, stateful MCP-over-WASM,
dedicated/shared gateway client placement, and core Docker job-worker placement
are implemented. Package, marketplace, and remaining sandbox hardening stay open.

## Part 1 Status

Part 1 covers host-local MCP installation and lifecycle. Integration review found
and fixed additional ownership, persistence, shutdown, and dead-stdio defects after
checkpoint commit `9da4f5c`.

Implemented scope includes:

- HTTP/HTTPS, host-local stdio, and Unix socket transports.
- Explicit install, activate, deactivate, update, remove, auth, and diagnostics paths.
- Owner-scoped secret references with restricted child environment inheritance.
- Whole-runtime stdio and trapped-WASM recovery with fresh config and secret resolution.
- Per-server cancellable recovery tasks, bounded jittered retries, a 30-second
  stability window, and a five-attempt circuit breaker.
- Generation-safe lifecycle serialization and all-or-none tool registration.
- Persistence-before-teardown and conditional activation/update rollback.
- Bounded recovery and whole-runtime shutdown, including agent error-return cleanup.
- Deterministic WASM/MCP tool-name ownership and duplicate server/process rejection.
- Persistent circuit-open state and recovery of live children whose stdio reader ended.
- API and browser status for recovering and terminal circuit-open states.

## Part 2 Runtime Status

Implemented through 2026-08-11:

- `McpRuntimeAdapter` plus an internal closed registry dispatch HTTP, stdio, Unix,
  stateful WASM, and gateway activation through one validated factory path. This is
  not a production plugin API; generic external runtime descriptors remain deferred.
- MCP-over-WASM uses one persistent component actor per active server, a raw
  JSON-RPC WIT boundary, no WASI/host imports, single-open artifact loading,
  optional operator-pinned digest verification, terminal-trap recovery, and bounded
  memory, fuel, wall time, and message sizes.
- MCP-over-WASM is synchronous client-to-server tool transport only. LunarWing does
  not expose unsolicited server events, server-to-client requests, resources, prompts,
  or subscriptions through its MCP client surface. Source WAT coverage validates
  protocol envelopes, state, pagination, calls, limits, timeout, traps, and replacement.
- Client initialization advertises empty capabilities, strictly classifies inbound
  envelopes, and negotiates exactly `2024-11-05`. Discovery follows bounded pagination.
  Negotiated `tools.listChanged` signals flow through stream and persistent HTTP GET/SSE
  transports, including gateways, into generation-safe atomic wrapper refresh.
- Later-revision canonical tool annotations deserialize with spec defaults but remain
  untrusted hints under operator approval policy; they are not claimed as part of the
  negotiated `2024-11-05` revision.
- Gateway placement distinguishes dedicated and shared deployments. Shared
  endpoints require a validated route; mode and route headers are host-owned and
  cannot be overridden by configured request headers.
- CLI add/list/test/pin, doctor, startup, lifecycle management, conversational
  installation, extension status, and authenticated web installation support the
  new runtime types.
- LunarWing provides the gateway client/routing contract, not a bundled gateway
  control-plane service; operators may deploy and scale that service separately.
- Explicit `host` and `job_worker` placement separates daemon and job activation.
  Legacy configs default to `host`; worker placement accepts stdio only.
- Job creation takes an immutable owner-scoped snapshot of explicitly selected enabled
  worker names; none are selected by default. Restart state stores those exact names.
  Secret names become separate job-scoped grants, and values are fetched through a
  dedicated authenticated worker route instead of entering normal shell credentials.
- Core Docker workers activate only preinstalled commands, verify pinned provenance
  inside the worker, register the complete discovered tool set atomically, and tear
  it down before job completion. Startup, grant coverage, tool conflicts, approval
  requirements, or cleanup failures fail the job closed.
- Worker placement requires a mounted `/workspace`, is limited to eight servers and
  32 credential grants per job, and is not available to persistent external workers.
  Each job is limited to 256 discovered tools and 2 MiB of tool descriptors; stream
  frames are capped at 4 MiB. Existing worker images fail closed when the orchestrator
  requires MCP support.
- Worker children receive scrubbed environments. The Linux worker parent is
  non-dumpable, and MCP activation requires Yama `kernel.yama.ptrace_scope >= 1`
  so same-UID MCP and shell siblings cannot inspect each other's credential state.
  Completion and unexpected-exit events route through authoritative job ownership.
- Per-server worker supervisors rebuild failed stdio runtimes from immutable snapshots
  with generation-safe discovery and atomic replacement. Bounded retries persist terminal
  circuit state and remove dead wrappers.
- Pinned stdio execution is atomic on Linux: verified sealed native bytes execute through
  `memfd` plus `execveat`, with a 128 MiB cap. Scripts and non-Linux pinning fail closed;
  unpinned legacy stdio remains available.

Historical Part 2 verification on 2026-08-09:

- `taskset -c 0-5 cargo test -j6 --lib --all-features mcp -- --test-threads=6`:
  358 passed.
- Direct completion-owner routing regression: passed.
- `taskset -c 0-5 cargo check -j6 --all-features`: passed.
- `taskset -c 0-5 cargo clippy -j6 --all --benches --tests --examples
  --all-features -- -D warnings`: passed.
- `taskset -c 0-5 cargo fmt --all -- --check`: passed.
- `node --check src/channels/web/static/app.js`: passed.
- `git diff HEAD --check`: passed.

Remediation verification on 2026-08-11:

- All-feature serialized library suite: 4,715 passed, 4 ignored.
- Engine V2 MCP compatibility: 3 passed.
- Rustfmt, all-feature/PostgreSQL/libSQL checks, and zero-warning all-feature clippy:
  passed with six-core affinity.
- Full release aggregation remains blocked outside the MCP diff: six real channel WASM
  tests need artifacts that could not be built because `rustup`/`wasm32-wasip2` are
  unavailable; existing skill-selection and TensorZero streaming expectations also fail.

Completed verification for the host-local lifecycle checkpoint:

- `cargo fmt --all -- --check`
- `cargo check -j2`
- `cargo check -j2 --tests`
- `cargo clippy -j2 --all --benches --tests --examples -- -D warnings`
- `node --check src/channels/web/static/app.js`
- `git diff --check`
- Full `lunarwing_engine` suite: 471 passed.
- Focused MCP recovery suite: 11 passed.
- MCP config suite: 49 passed; process suite: 6 passed.
- Focused shutdown, failed-update, and WASM ownership regressions: passed.
- `cargo check -j6`: passed.

## Part 1 Remaining Validation

- Optionally add coverage for startup conflicts across database and disk backends,
  forced supervisor/child shutdown timeout, config-read timeout or unavailability,
  and SSE/browser `recovering -> active/failed` transitions.
- Run backend-specific and all-feature checks if required by the release gate.
- Run a live smoke test that kills a managed stdio child, observes recovery and
  circuit opening, then verifies deactivate and daemon shutdown leave no child.

These are validation and coverage tasks, not known production-code defects.

## Deferred MCP Work

- Automatic npm/pip package installation.
- A signed-package marketplace *install flow* (CLI install/upgrade against a
  marketplace catalog). The underlying primitives are implemented and wired:
  ed25519 publisher signatures and trust stores, signed dependency lockfiles
  pinning exact versions and digests, manifest upgrade diffs, and signed
  revocation lists with monotonic sequences. Revocation enforcement is
  operator-reachable via the `marketplace` section of `mcp-servers.json`
  (`trust_store_path`, `revocation_cache_path`, `revocation_url`,
  `revocation_publisher`, `revocation_public_key_hex`): configuration loads
  fail closed on revoked packages, daemon startup pulls the newest signed list
  (`refresh_marketplace_revocations`), and manual activation re-checks the
  cached list. (Operator-pinned stdio executable and WASM component digests,
  package identity, trust state, and revocation are implemented.)
- Device-level sandboxing beyond the current baseline. (Opt-in host sandboxing
  for stdio servers — resource limits, core-dump control, process-group
  isolation, no_new_privileges, seccomp ptrace/process_vm denial, Landlock
  read-only root, and network namespace isolation — is implemented, and
  `require_sandbox_profiles = true` in `mcp-servers.json` makes the mandatory
  baseline deployment-wide.)
- Cross-daemon HA policy and centralized cross-tenant credential ownership.
  The bundled gateway control-plane service is implemented and opt-in via
  environment (`LUNARWING_MCP_GATEWAY_CONTROL_BIND`,
  `LUNARWING_MCP_GATEWAY_ADMIN_TOKEN`, optional
  `LUNARWING_MCP_GATEWAY_ROUTES_PATH`; loopback-only unless
  `LUNARWING_MCP_GATEWAY_CONTROL_ALLOW_NON_LOOPBACK=1`). The dedicated/shared
  client placement contract is implemented.
- Configurable startup and request timeouts for every transport. MCP-over-WASM
  per-message deadlines and bounded transport/process shutdown are implemented.
- Persistent external-worker MCP activation delivery: the `external_worker`
  placement vocabulary, worker selection exclusion from job snapshots, and
  `lunarwing doctor` cross-checks (unknown worker names, undeclared external
  runtimes) are implemented, but the `lunarwing-agent-v1` worker protocol has
  no MCP registration message yet, so external-worker servers are not pushed
  to workers. Generic external runtime descriptors are implemented and
  reachable: declare adapters in the `external_runtimes` section of
  `mcp-servers.json` and attach servers with `lunarwing mcp add --transport
  external --external-runtime <name> [--external-options '<json>']`; empty
  registries fail closed with an unknown-runtime error.

Automatic npm/pip installation must remain blocked until provenance, dependency
locking, containment, audit, revocation, health, and rollback policies exist.

## Related Documents

- [`MCP_ADDITIONS.md`](MCP_ADDITIONS.md): detailed implementation and security scope.
- [`V2_MCP_EXTENSION_ARCHITECTURE_EXPLORATION.md`](../proposals/V2_MCP_EXTENSION_ARCHITECTURE_EXPLORATION.md): later-phase architecture.
- [`ROADMAP_2026.md`](ROADMAP_2026.md): release targets for MCP overhaul phases.
