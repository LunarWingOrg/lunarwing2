# DarkIRC Secure Staged Key Exchange - Goal 1 Status

- **Status:** Stage A source and fixture gates closed; dedicated live-host acceptance pending
- **Implementation baseline:** `cae4770` (`fix(darkirc): harden Stage A exchange recovery`)
- **Current closure branch:** `darkirckeyexch8/cmc/v2.0.4.0`

## Purpose

This document holds detailed status for goal 1 in
`docs/ops/AGENT_GOALS_2.0.4.0.md`. Keep the goal entry short and update this
document as implementation, verification, or remaining risk changes.

Authoritative design specifications:

- `docs/proposals/DARKIRC_SECURE_KEY_EXCHANGE.md`
- `docs/proposals/DARKIRC_KEY_EXCHANGE_NEXT_STAGES.md`

Rejected `dik2/v2.0.4.0` code remains reference-only. Windows work remains out
of scope.

## Current Boundary

Phase 0/1 and the Stage A command surface are implemented. Existing capabilities include:

- Public-only `prepare` and `respond` artifacts.
- Deferred `complete`, `cancel`, inspection, adoption, and recovery.
- Structured migration.
- Mandatory full-fingerprint authorization.
- Bounded pending state and private-key reuse checks.
- Durable tombstones and deferred activation.

Installed Stage A records remain secret-bearing pending state until Stage B can
health-gate activation. They intentionally continue to block migration and
managed upgrade. They no longer consume the separate 128-entry active exchange
quota; retained installed state has its own contact-count and byte bounds.

Later lifecycle automation remains outside this goal and stays disabled:

- Stage B activation and rollback.
- Stage C peer verification.
- Stage D rotation, revocation, and same-host pairing.
- QR, web, and trusted-channel delivery.

## Result

- H1 fixed and verified: lock-held replay, single generation, atomic no-replace
  publication, and safe identical retry.
- H2 fixed and verified: lifecycle and writer locks remain held through sealed
  export; passphrases cross the boundary by file descriptor only; failed
  archives are removed.
- H3 improved: six completion crash windows are covered. Real two-user
  isolation and live `/proc`, stdout, stderr, and journal leak tests now have a
  fail-closed harness. Exec-delay tracing makes process sampling deterministic,
  and missing mt-admin, helper, or measured-daemon evidence fails the run. Real
  systemd-user and OpenRC hosts are still required.
- M1 fixed: daemon, helper, and attestation publish as one immutable release
  selected by a single atomic durable symlink. Readers pin one generation; the
  helper executes the same daemon descriptor it measured. Retry durably syncs
  every visible release component and parent before selection. Attestation
  binds helper source revision so upgrades cannot silently retain old helper
  code. Tenant start rejects stale, multiply-defined, or systemd-drop-in
  overridden daemon service commands.
- M3 fixed: legacy WeeChat cleanup and adapter restart route through mt-admin
  with systemd-user/OpenRC parity before wrappers rebuild or restart tenants.
- L1 fixed: `doctor` inspects transaction directories through its pinned
  capability with no-follow traversal and fail-closed errors.
- L2 fixed: active status, proposal, index, publication, and issue wording now
  distinguish implemented Stage A code from pending live acceptance.
- DarkFi generator execution now has a fixed deadline and combined output cap.
  Timeout or oversized output kills the isolated process group and reaps the
  child before exchange locks are released; focused tests pass.
- Goal 1 remains `[ ]`; its goal entry points to this status document.

## High-Severity Work

### H1: Retry and Publication Safety

- Prepare/respond semantic replay now occurs while the core exchange lock is
  held and before key generation.
- Concurrent equivalent prepare/respond requests invoke the generator once and
  return the same prepared result.
- Linux artifact publication uses atomic `renameat2` with
  `RENAME_NOREPLACE`.
- Retry after final rename but before directory sync accepts only a safe,
  owner-only, single-link, byte-identical artifact, then syncs its directory.
- Differing files, symlinks, unsafe modes, and arbitrary hardlinks are rejected
  without overwrite.
- Non-Linux Unix keeps the no-overwrite hardlink fallback and fails closed when
  an unsafe link count is observed.

### H2: Atomic Migration Export Boundary

- Mt-admin owns `migration-export-boundary` and retains the tenant lifecycle
  lock through export completion.
- DarkIRC-enabled tenants also retain the DarkIRC writer lock through readiness
  validation, writer stop, manifest pinning, and final bundle seal.
- Child validation checks inherited lock identity and active contention instead
  of trusting environment markers alone.
- Passphrases are acquired before locks, sent through an anonymous inherited
  descriptor, and removed from `KAWARIMI_PASS` and related environment sources.
- Disabled tenants with legacy DarkIRC contact state fail before lifecycle
  mutation.
- Export uses `umask 077`; tar, 7z, or chmod failure removes partial output.
- Deterministic lock contenders verify both locks remain unavailable until the
  export boundary exits.

### H3: Crash Recovery and Secret Surfaces

Regression coverage now injects crashes at:

1. `AfterCandidates`
2. `AfterJournal`
3. `AfterLedgerRename`
4. `AfterLedgerCommit`
5. `AfterConfigRename`
6. `AfterConfig`

Each test verifies deterministic recovery, candidate and journal cleanup,
config and ledger state, pending state, durable tombstones, idempotent retry,
and absence of private key material from public artifacts, ledger, journal,
status, and error surfaces.

Remaining H3 evidence requires a suitable live host:

- Real isolation between two OS users.
- Live `/proc` argv and environment scans.
- Deterministic `/proc` evidence for mt-admin, the helper, and measured daemon.
- Captured stdout and stderr scans.
- Journal scans under a real systemd user manager and equivalent OpenRC
  execution.

## Audit Ledger

| ID | Severity | Status | Finding |
|---|---|---|---|
| H1 | High | Fixed; verified 2026-08-03 | Named artifact retries use lock-held semantic replay before one generator invocation. Linux publication is atomic and no-replace; safe identical retry restores durability after post-rename failure. |
| H2 | High | Fixed; verified 2026-08-03 | Mt-admin retains lifecycle and DarkIRC writer locks across readiness, writer stop, manifest pinning, and bundle seal. Deterministic contenders remain blocked until release. |
| H3 | High | Harness ready; live evidence pending | Six Stage A completion crash windows and unit-visible secret surfaces have regressions. Dedicated harness covers real two-user, process, output, and init-log surfaces but must run on both host types. |
| M1 | Medium | Fixed; local validation complete | Immutable complete releases bind daemon/helper digests, helper source revision, and attestation. One atomic fsynced selector publishes generations; readers pin and execute one measured generation; effective service overrides fail closed. |
| M2 | Medium | Fixed; verified 2026-08-03 | Import opens the source once, accepts only root or the validated invoking sudo user as owner, rejects links and group/world access, and pins bytes through the validated descriptor. Interrupted structured imports resume only with a matching protected marker and stopped writers. |
| M3 | Medium | Fixed; local validation complete | Legacy WeeChat cleanup and adapter restart are mt-admin operations with systemd-user/OpenRC parity; upgrade wrappers clean immediately after stop and contain no direct init lifecycle calls. |
| M4 | Medium | Fixed; verified 2026-08-03 | DarkFi generation has a fixed runtime and combined stdout/stderr cap; timeout or overflow kills the process group and reaps the child before returning. |
| L1 | Low | Fixed; local validation complete | `doctor` uses capability-relative no-follow directory inspection; missing paths remain nonfatal while unsafe paths and I/O errors fail closed. |
| L2 | Low | Fixed | Active documentation now describes atomic immutable releases and Stage A's pending live acceptance without claiming later-stage automation. |

## Validation

Validation completed on 2026-08-04 after all locally testable closure work:

- Core DarkIRC key-manager tests: 89 passed.
- DarkIRC key-helper tests: 22 passed.
- `cargo check`: passed.
- `cargo fmt --all -- --check`: passed.
- Full all-feature Clippy across libraries, benches, tests, and examples with
  warnings denied: passed.
- Full GitLab shell harness: passed.
- Shell syntax checks: passed.
- ShellCheck: passed before final closure review. Final rerun is deferred to
  release CI because this host lacks ShellCheck and its rootless Podman mapping
  cannot unpack the pinned CI image.
- H3 analyzer unit tests: 7 passed.
- `git diff --check`: passed.
- No full debug `cargo build` was used.

## Live Validation Blocker

The validation workspace lacks passwordless sudo, an available systemd user
manager, and OpenRC. One booted host also cannot provide genuine evidence for
both init systems. Run `ic/scripts/live-tests/darkirc-stage-a-h3.sh` on dedicated
disposable systemd-user and OpenRC hosts with two real tenants. The harness
performs the exchange, cross-user denial probes, `/proc` argv/environment scans,
stdout/stderr scans, and journal/log scans without printing private canaries.
It requires `strace` exec-delay injection and fails if required process coverage
is absent.
Unit fixtures do not substitute for those two runs.

## Next Steps

1. Run the H3 live harness on a disposable systemd-user host and record the
   secret-free pass summary.
2. Run the same harness on a disposable OpenRC host and record the summary.
3. Rerun required release CI against the final commit, then check goal item 1.
4. Treat Stage B, Stage C, Stage D, and optional delivery as separate future
   goals under their own promotion gates.

## Relevant Files

- `ic/src/darkirc_key_manager.rs`: lock-held generation, semantic replay, and
  completion crash recovery.
- `ic/src/bin/lunarwing-darkirc-key-helper/unix.rs`: artifact publication and
  safe retry behavior.
- `ic/scripts/lunarwing-mt-admin.sh`: retained migration boundary, immutable
  release selector, and init-agnostic lifecycle operations.
- `ic/scripts/export-tenant.sh`: passphrase FD handoff and archive cleanup.
- `ic/scripts/live-tests/darkirc-stage-a-h3.sh`: destructive real-host H3
  isolation and leak validation.
- `ic/scripts/live-tests/darkirc_h3_analyzer.py`: root-only canary extraction,
  process sampling, public-artifact transfer, and leak scanning.
- `ic/scripts/tests/test-darkirc-writer-lifecycle.sh`: retained-lock
  regressions.
- `ic/scripts/tests/test-darkirc-migration-safeguards.sh`: migration export and
  archive safety regressions.
- `ic/scripts/tests/test-kawarimi-import-flags.sh`: passphrase and export
  boundary regressions.
- `docs/ops/MT-MACHINE-MIGRATION.md`: operator-facing migration contract.
