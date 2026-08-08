# MCP Overall Status

**Status as of 2026-08-03:** MCP work is partial overall. MCP Overhaul Part 1 is
implemented and its focused automated runtime validation passes. Later package,
placement, sandbox, adapter, and gateway phases remain deferred.

## Part 1 Status

Part 1 covers host-local MCP installation and lifecycle. Integration review found
and fixed additional ownership, persistence, shutdown, and dead-stdio defects after
checkpoint commit `9da4f5c`.

Implemented scope includes:

- HTTP/HTTPS, host-local stdio, and Unix socket transports.
- Explicit install, activate, deactivate, update, remove, auth, and diagnostics paths.
- Owner-scoped secret references with restricted child environment inheritance.
- Whole-runtime stdio recovery with fresh config and secret resolution.
- Per-server cancellable recovery tasks, bounded jittered retries, a 30-second
  stability window, and a five-attempt circuit breaker.
- Generation-safe lifecycle serialization and all-or-none tool registration.
- Persistence-before-teardown and conditional activation/update rollback.
- Bounded recovery and child shutdown, including agent error-return cleanup.
- Deterministic WASM/MCP tool-name ownership and duplicate server/process rejection.
- Persistent circuit-open state and recovery of live children whose stdio reader ended.
- API and browser status for recovering and terminal circuit-open states.

Completed verification:

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

- Worker-local MCP placement.
- Automatic npm/pip package installation.
- Publisher signatures, trusted signing keys, dependency locking, upgrade
  diffs, and marketplace revocation distribution. (Operator-pinned executable
  digests, package identity, trust state, and revocation for host-local stdio
  servers are implemented as of 2026-08-07.)
- Device-level sandboxing and mandatory default sandbox profiles. (Opt-in
  host sandboxing for stdio servers — resource limits, core-dump control,
  process-group isolation, no_new_privileges, seccomp ptrace/process_vm
  denial, Landlock read-only root, and network namespace isolation — is
  implemented as of 2026-08-08.)
- A generic runtime-adapter boundary.
- MCP-over-WASM session/runtime support.
- A dedicated or shared MCP gateway.
- Configurable per-server startup, request, and shutdown timeouts.

Automatic npm/pip installation must remain blocked until provenance, dependency
locking, containment, audit, revocation, health, and rollback policies exist.

## Related Documents

- [`MCP_ADDITIONS.md`](MCP_ADDITIONS.md): detailed implementation and security scope.
- [`V2_MCP_EXTENSION_ARCHITECTURE_EXPLORATION.md`](../proposals/V2_MCP_EXTENSION_ARCHITECTURE_EXPLORATION.md): later-phase architecture.
- [`ROADMAP_2026.md`](ROADMAP_2026.md): release targets for MCP overhaul phases.
