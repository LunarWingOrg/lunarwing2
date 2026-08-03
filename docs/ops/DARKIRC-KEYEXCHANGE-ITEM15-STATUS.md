# DarkIRC Secure Staged Key Exchange - Item 15 Status

- **Status:** Incomplete; reopened after completeness audit on 2026-08-03
- **Implementation commit:** `cae4770` (`fix(darkirc): harden Stage A exchange recovery`)
- **Branch:** `rarity/kimi-15-20260803-1201`

## Purpose

This document holds detailed status for item 15 in
`docs/ops/AGENT_GOALS_2.0.4.0.md`. Keep the goal entry short and update this
document as implementation, verification, or remaining risk changes.

Authoritative design specifications:

- `docs/proposals/DARKIRC_SECURE_KEY_EXCHANGE.md`
- `docs/proposals/DARKIRC_KEY_EXCHANGE_NEXT_STAGES.md`

Rejected `dik2/v2.0.4.0` code remains reference-only. Windows work remains out
of scope.

## Current Boundary

Phase 0/1 and most Stage A are implemented. Existing capabilities include:

- Public-only `prepare` and `respond` artifacts.
- Deferred `complete`, `cancel`, inspection, adoption, and recovery.
- Structured migration.
- Mandatory full-fingerprint authorization.
- Bounded pending state and private-key reuse checks.
- Durable tombstones and deferred activation.

Full automation remains incomplete. These surfaces stay disabled:

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
  isolation and live `/proc`, stdout, stderr, and journal leak tests remain open.
- Item 15 remains `[ ]`; its goal entry points to this status document.

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
- Captured stdout and stderr scans.
- Journal scans under a real systemd user manager and equivalent OpenRC
  execution.

## Audit Ledger

| ID | Severity | Status | Finding |
|---|---|---|---|
| H1 | High | Fixed; verified 2026-08-03 | Named artifact retries use lock-held semantic replay before one generator invocation. Linux publication is atomic and no-replace; safe identical retry restores durability after post-rename failure. |
| H2 | High | Fixed; verified 2026-08-03 | Mt-admin retains lifecycle and DarkIRC writer locks across readiness, writer stop, manifest pinning, and bundle seal. Deterministic contenders remain blocked until release. |
| H3 | High | Partially fixed | Six Stage A completion crash windows and unit-visible secret surfaces have regressions. Real two-user isolation and live process/log leak scans remain required. |
| M1 | Medium | Open | Daemon, helper, and compatibility attestation publish separately. Normal failures roll back, but publication is not reader-atomic or host-crash recoverable. |
| M2 | Medium | Open | Import pins bundle content but does not reject unsafe source ownership or group/world-readable mode. |
| M3 | Medium | Open; pre-existing | `upgrade-tenant.sh` performs legacy WeeChat `systemctl --user` cleanup directly and lacks OpenRC parity. Lifecycle belongs in mt-admin. |
| M4 | Medium | Open | DarkFi key generation has no process timeout while the core exchange lock is held. A hung generator can block tenant key operations. |
| L1 | Low | Open | `doctor` transaction-directory inspection uses ambient filesystem reads instead of capability-relative no-follow access. |
| L2 | Low | Partially fixed | `docs/README.md` still says Phase 0 is unimplemented, while publication documentation overclaims atomicity. |

## Validation

Validation completed after the H1/H2 implementation and H3 regression work:

- Core DarkIRC key-manager tests: 82 passed.
- DarkIRC key-helper tests: 16 passed.
- `cargo check`: passed.
- `cargo fmt --all -- --check`: passed.
- Targeted clippy with warnings denied: passed.
- Shell syntax checks: passed.
- ShellCheck: passed.
- Eight DarkIRC shell suites plus the Kawarimi suite: passed.
- `git diff --check`: passed.
- No full debug `cargo build` was used.

The original validation snapshot stated that no commit had been created. The
validated work was subsequently committed as `cae4770` and synchronized with
the branch remote before this status document was added.

## Live Validation Blocker

The validation workspace lacked passwordless sudo, an available systemd user
manager, and OpenRC. Those constraints prevent honest completion of real
two-user isolation and live `/proc`/journal tests here. Unit fixtures do not
substitute for that evidence.

## Next Steps

1. Complete H3 on a suitable host with real cross-user isolation and live
   process/output/journal secret-leak scans.
2. Make daemon, helper, and attestation publication reader-coherent and
   host-crash recoverable; correct atomicity wording meanwhile.
3. Reject migration bundles with unsafe source ownership or mode.
4. Bound DarkFi generator runtime while the exchange lock is held.
5. Move legacy wrapper lifecycle cleanup into mt-admin with systemd and OpenRC
   parity.
6. Convert `doctor` transaction inspection to capability-relative no-follow
   reads.
7. Reconcile stale DarkIRC status documents, then rerun targeted Rust, shell,
   format, clippy, check, and required CI validation.
8. After Stage A closes, implement Stage B, Stage C, Stage D, then optional
   delivery surfaces under their promotion gates.

## Relevant Files

- `ic/src/darkirc_key_manager.rs`: lock-held generation, semantic replay, and
  completion crash recovery.
- `ic/src/bin/lunarwing-darkirc-key-helper/unix.rs`: artifact publication and
  safe retry behavior.
- `ic/scripts/lunarwing-mt-admin.sh`: retained migration export boundary.
- `ic/scripts/export-tenant.sh`: passphrase FD handoff and archive cleanup.
- `ic/scripts/tests/test-darkirc-writer-lifecycle.sh`: retained-lock
  regressions.
- `ic/scripts/tests/test-darkirc-migration-safeguards.sh`: migration export and
  archive safety regressions.
- `ic/scripts/tests/test-kawarimi-import-flags.sh`: passphrase and export
  boundary regressions.
- `docs/ops/MT-MACHINE-MIGRATION.md`: operator-facing migration contract.
