# DarkIRC Secure Staged Key Exchange - Item 15 Status

- **Status:** Incomplete; reopened after completeness audit on 2026-08-03.
  Live H3 isolation and leak evidence gathered on an OpenRC host 2026-08-05;
  only the systemd-user-manager journal scan remains open.
- **Implementation commit:** `cae4770` (`fix(darkirc): harden Stage A exchange recovery`)
- **Live evidence:** branch `rarity/kimi-7-20260805-1803`,
  `ic/scripts/tests/test-darkirc-live-isolation.sh`
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

Installed Stage A records remain secret-bearing pending state until Stage B can
health-gate activation. They intentionally continue to block migration and
managed upgrade. They no longer consume the separate 128-entry active exchange
quota; retained installed state has its own contact-count and byte bounds.

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
- H3 improved: six completion crash windows are covered. Live two-user
  isolation, live `/proc` argv/environment scans, captured stdout/stderr
  scans, and OpenRC-supervised execution log scans were completed on a Gentoo
  OpenRC host on 2026-08-05 (see "Live H3 Evidence" below). The
  systemd-user-manager journal scan remains open.
- DarkFi generator execution now has a fixed deadline and combined output cap.
  Timeout or oversized output kills the isolated process group and reaps the
  child before exchange locks are released; focused tests pass.
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

- ~~Real isolation between two OS users.~~ Done 2026-08-05 (OpenRC host).
- ~~Live `/proc` argv and environment scans.~~ Done 2026-08-05.
- ~~Captured stdout and stderr scans.~~ Done 2026-08-05.
- Journal scans under a real systemd user manager — **still open** (requires a
  systemd host).
- ~~Equivalent OpenRC execution.~~ Done 2026-08-05.

## Live H3 Evidence (2026-08-05, Gentoo OpenRC host)

`ic/scripts/tests/test-darkirc-live-isolation.sh` performs a full live Stage A
exchange and leak scan. It self-elevates through passwordless sudo, creates
two scratch OS users (`dklive-a`, `dklive-b`), installs a root-owned
pure-Python RFC 7748 X25519 generator at the attested `/usr/local/bin/darkirc`
path (fresh genuine keypair per call; the helper's own public-from-secret
validation must accept each pair), installs the built helper at
`/usr/local/libexec/lunarwing-darkirc-key-helper` and the root-owned
`/etc/lunarwing/darkirc-compat.json` attestation, then drives
prepare/respond/deferred-complete exactly the way `lunarwing-mt-admin.sh`
does (`sudo -u <tenant> env -i HOME=... PATH=...` with attestation and
`--expected-uid/--expected-gid` pinning). All assets and users are removed on
exit. Results on this host (run three times, identical outcome):

- Full exchange succeeded: prepare as `dklive-a`, respond as `dklive-b`,
  deferred complete as `dklive-a`.
- 12 live `/proc/<pid>/cmdline` and `/proc/<pid>/environ` snapshots of the
  helper, generator, and sudo processes were captured while generation ran;
  none contain private key material.
- All captured helper stdout/stderr (including expected rejection surfaces:
  missing-config, missing-ledger, scope-conflict, and cross-user filesystem
  errors) is free of private key material.
- Public offer/response artifacts, both tenant ledgers, and
  `/var/log/messages` (which records the sudo argv of every invocation) are
  free of private key material.
- A one-shot OpenRC service (`supervise-daemon`-style `command_background`
  with init-captured `output_log`/`error_log`) ran a further secret-bearing
  prepare as `dklive-a`; the init-captured log contains only the public JSON
  result, no private key material.
- Cross-user isolation is real: `dklive-b` cannot read `dklive-a`'s pending
  secret records, cannot operate the helper on `dklive-a`'s tenant state, and
  no path under either tenant's DarkIRC state tree carries group/other
  permission bits. Pending records remain mode-0600 per design.

Secrets for the scans were extracted as root from the mode-0600 pending
records after the flow (3 distinct pending secrets across 3 generator calls)
and were never printed or logged by the suite. The suite skips cleanly (exit
0) without passwordless sudo, jq, python3, or a built helper binary, so
fixture-only environments and CI stay green. The systemd-user-manager journal
scan is not claimed: this host has no systemd.

## Audit Ledger

| ID | Severity | Status | Finding |
|---|---|---|---|
| H1 | High | Fixed; verified 2026-08-03 | Named artifact retries use lock-held semantic replay before one generator invocation. Linux publication is atomic and no-replace; safe identical retry restores durability after post-rename failure. |
| H2 | High | Fixed; verified 2026-08-03 | Mt-admin retains lifecycle and DarkIRC writer locks across readiness, writer stop, manifest pinning, and bundle seal. Deterministic contenders remain blocked until release. |
| H3 | High | Mostly fixed; live evidence 2026-08-05 | Six Stage A completion crash windows and unit-visible secret surfaces have regressions. Live two-user isolation, live `/proc` argv/environment scans, captured stdout/stderr scans, and OpenRC-supervised execution log scans pass on a Gentoo OpenRC host. The systemd-user-manager journal scan remains open. |
| M1 | Medium | Open | Daemon, helper, and compatibility attestation publish separately. Normal failures roll back, but publication is not reader-atomic or host-crash recoverable. |
| M2 | Medium | Fixed; verified 2026-08-03 | Import opens the source once, accepts only root or the validated invoking sudo user as owner, rejects links and group/world access, and pins bytes through the validated descriptor. Interrupted structured imports resume only with a matching protected marker and stopped writers. |
| M3 | Medium | Open; pre-existing | `upgrade-tenant.sh` performs legacy WeeChat `systemctl --user` cleanup directly and lacks OpenRC parity. Lifecycle belongs in mt-admin. |
| M4 | Medium | Fixed; verified 2026-08-03 | DarkFi generation has a fixed runtime and combined stdout/stderr cap; timeout or overflow kills the process group and reaps the child before returning. |
| L1 | Low | Open | `doctor` transaction-directory inspection uses ambient filesystem reads instead of capability-relative no-follow access. |
| L2 | Low | Partially fixed | `docs/README.md` still says Phase 0 is unimplemented, while publication documentation overclaims atomicity. |

## Validation

Validation completed after the H1/H2 implementation and H3 regression work:

- Core DarkIRC key-manager tests: 82 passed.
- DarkIRC key-helper tests: 20 passed.
- `cargo check`: passed.
- `cargo fmt --all -- --check`: passed.
- Targeted clippy with warnings denied: passed.
- Shell syntax checks: passed.
- ShellCheck: passed.
- Eight DarkIRC shell suites plus the Kawarimi suite: passed.
- `git diff --check`: passed.
- No full debug `cargo build` was used.

Additional validation on 2026-08-05 (Gentoo OpenRC host, branch
`rarity/kimi-7-20260805-1803`):

- Live isolation/leak suite `test-darkirc-live-isolation.sh`: passed (three
  consecutive runs).
- All nine DarkIRC shell suites plus the Kawarimi suite: passed again after
  the live suite was added.
- `bash -n` and ShellCheck on the new suite: passed.
- Helper built once as `cargo build --bin lunarwing-darkirc-key-helper`
  (dev profile, line-tables-only, `-j6` under `taskset -c 0-5`) to obtain the
  binary the live evidence requires; no full debug profile build was used.

The original validation snapshot stated that no commit had been created. The
validated work was subsequently committed as `cae4770` and synchronized with
the branch remote before this status document was added.

## Live Validation Blocker

Resolved in part on 2026-08-05: the Gentoo OpenRC dev host provides
passwordless sudo and real OpenRC, so the two-user isolation, live `/proc`,
stdout/stderr, system-log, and OpenRC-supervised execution scans are now
done. Remaining: a host with a real systemd user manager for the journal
scan. That scan must not be claimed from OpenRC evidence alone.

## Next Steps

1. ~~Complete H3 on a suitable host with real cross-user isolation and live
   process/output/journal secret-leak scans.~~ Done 2026-08-05 on a Gentoo
   OpenRC host except the systemd-user-manager journal scan, which still
   requires a systemd host.
2. Make daemon, helper, and attestation publication reader-coherent and
   host-crash recoverable; correct atomicity wording meanwhile.
3. Move legacy wrapper lifecycle cleanup into mt-admin with systemd and OpenRC
   parity.
4. Convert `doctor` transaction inspection to capability-relative no-follow
   reads.
5. Reconcile stale DarkIRC status documents, then rerun targeted Rust, shell,
   format, clippy, check, and required CI validation.
6. After Stage A closes, implement Stage B, Stage C, Stage D, then optional
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
- `ic/scripts/tests/test-darkirc-live-isolation.sh`: live two-user isolation
  and `/proc`/stdout/stderr/OpenRC-log leak-scan evidence suite (2026-08-05).
- `docs/ops/MT-MACHINE-MIGRATION.md`: operator-facing migration contract.
