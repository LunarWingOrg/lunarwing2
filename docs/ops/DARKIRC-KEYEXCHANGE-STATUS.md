# DarkIRC Secure Staged Key Exchange Status

- **Status:** Stage A implementation and source hardening committed; local validation complete; live-host acceptance pending.
- **Acceptance:** OPEN. Do not check off release goal item 7 yet.
- **Stage A implementation baseline:** `cae4770` (`fix(darkirc): harden Stage A exchange recovery`).
- **Acceptance-hardening baseline:** `fb548a6` (`fix(darkirc): harden Stage A acceptance gates`).
- **Current integration branch:** `rarity/kimi-7-20260805-1803`.
- **Last updated:** 2026-08-05.
- **Historical live evidence:** OpenRC run on 2026-08-05, before immutable release v2.

## Current Decision

Stage A source and acceptance hardening are ready for live validation. No known
source-review finding remains open. Release acceptance is not closed because no
current run has validated immutable release v2 and measured-descriptor execution
under either required init path. Historical OpenRC evidence remains supporting
evidence only. Final release CI must run after both current live-host H3 runs.

| Gate | Current state | Closure evidence |
|---|---|---|
| Stage A implementation | Complete | Public-only prepare/respond, deferred complete, cancellation, inspection, adoption, migration, recovery, and authorization are implemented. |
| Source-audit findings | Complete | H1/H2, M1-M4, and L1-L2 are fixed; final read-only review found no actionable issue. |
| Local validation | Complete | Rust, Python, shell, analyzer, lint, format, and diff checks pass on `fb548a6`. |
| OpenRC H3 | Pending | Run release-v2 harness on a disposable real OpenRC host and retain secret-free summary. |
| systemd-user H3 | Pending | Run same release-v2 harness with a real systemd user manager and retain journal evidence. |
| Final release CI | Pending | Run release pipeline on committed tree after both live H3 runs. |
| Stages B-D and delivery | Out of current scope | Keep disabled and close under separate review and acceptance gates. |

## Boundary

Phase 0/1 and Stage A provide public-only `prepare` and `respond` artifacts,
deferred `complete`, cancellation, inspection, adoption, structured migration,
mandatory full-fingerprint authorization, bounded pending state, private-key
reuse checks, durable tombstones, and deterministic recovery.

Installed Stage A records remain secret-bearing pending state. Stage A does not
activate contacts. These later surfaces remain separately disabled and gated:

- Stage B activation and rollback.
- Stage C peer verification.
- Stage D rotation, revocation, and same-host pairing.
- QR, web, and trusted-channel delivery.

## Audit Ledger

| ID | Severity | Status | Finding |
|---|---|---|---|
| H1 | High | Fixed; verified 2026-08-03 | Semantic retry stays under the exchange lock, invokes one generator, publishes no-replace, and safely repairs identical post-rename retries. |
| H2 | High | Fixed; verified 2026-08-03 | Migration export retains lifecycle and DarkIRC writer locks through bundle seal; passphrases use inherited descriptors and failed archives are removed. |
| H3 | High | Harnesses locally verified; current live evidence pending | Six completion crash windows have regression coverage. Real-host harnesses cover cross-user isolation, process argv/environment, stdout/stderr, public artifacts, and systemd/OpenRC logs. |
| M1 | Medium | Fixed; locally verified 2026-08-05 | Daemon, helper, helper source revision, and attestation publish as one immutable, fsynced release. Readers pin one generation and helper executes the measured daemon descriptor. Upgrades require current helper source while digest-valid retained releases remain rollback-capable. Stale units and systemd drop-ins fail closed. |
| M2 | Medium | Fixed; verified 2026-08-03 | Import pins one validated descriptor, rejects unsafe ownership/links/modes, and resumes interrupted imports only with matching protected state and stopped writers. |
| M3 | Medium | Fixed; locally verified 2026-08-05 | Legacy WeeChat cleanup and adapter restart now route through mt-admin with systemd-user/OpenRC parity before upgrade starts. |
| M4 | Medium | Fixed; verified 2026-08-03 | Generator runtime and combined output are bounded; timeout or overflow kills and reaps the isolated process group. |
| L1 | Low | Fixed; locally verified 2026-08-05 | `doctor` uses capability-relative, no-follow transaction-directory inspection and fails closed on unsafe paths or I/O errors. |
| L2 | Low | Fixed; locally verified 2026-08-05 | Active docs distinguish implemented Stage A source from pending live acceptance and later lifecycle stages. |

## H3 Harnesses

`ic/scripts/live-tests/darkirc-stage-a-h3.sh` is the acceptance harness. It
requires an explicit destructive-test acknowledgement, two real DarkIRC-enabled
tenants, root, a real systemd or OpenRC boot, and `strace` exec-delay injection.
It drives mt-admin through a real Stage A exchange and fails unless `/proc`
captures observe mt-admin, the attested helper, and the measured daemon. It
scans exact private-key canaries across process argv/environment, command
stdout/stderr, public artifacts, and systemd journals or OpenRC logs.

Process evidence is structured by PID and executable. Helper and daemon
cmdline/environment captures must come from the same observed PID, so unrelated
or stale processes cannot satisfy coverage. systemd capture starts from a
pre-restart journal cursor. OpenRC capture tracks device/inode plus byte offset,
so rotation cannot hide or replay log evidence. Cross-user probes distinguish
expected permission denial from missing or otherwise broken paths.

`ic/scripts/tests/test-darkirc-live-isolation.sh` creates two scratch OS users
for a narrower host test. It now requires a real operator-supplied DarkIRC
daemon (`DARKIRC_LIVE_DAEMON`, default `/usr/local/bin/darkirc`) plus an
explicitly trusted helper and matching source revision (`DARKIRC_LIVE_HELPER`,
`DARKIRC_LIVE_HELPER_SOURCE_REVISION`) plus its independently recorded digest
(`DARKIRC_LIVE_HELPER_SHA256`). The real daemon also requires an independently
recorded `DARKIRC_LIVE_DAEMON_SHA256`. Both source paths must be canonical,
root-owned, single-link, and non-writable. It copies both into a temporary
immutable release-v2 layout, runs the same public/deferred exchange, and checks
cross-user denial plus process/output/log surfaces. Missing destructive-test
prerequisites skip this optional shell-suite entry; a skip is not acceptance
evidence.

## Historical OpenRC Evidence

On 2026-08-05, the scratch-user suite passed three times on a Gentoo OpenRC
host. It observed 12 live process snapshots, captured helper output, public
artifacts, ledgers, `/var/log/messages`, and an OpenRC-supervised execution log;
three extracted private canaries appeared in none. Cross-user pending-state
reads and helper operations were denied, and tenant state had no group/other
permission bits.

That run used the former fixed-path compatibility-v1 publication and a
test-only Python X25519 generator. It remains useful evidence for core Stage A
secret handling, but does not validate immutable release v2, measured-descriptor
execution, or current mt-admin service wiring. Do not treat it as current H3
closure.

## Local Validation

Commit `fb548a6` on the v2.0.5 integration branch passed:

- 89 core DarkIRC key-manager tests.
- 22 key-helper tests.
- Full GitLab shell harness.
- Locked `cargo check`, rustfmt, and all-feature Clippy with warnings denied.
- Eleven H3 analyzer tests, ShellCheck, and `git diff --check`.
- Final read-only security/correctness review with no actionable findings.

The optional scratch-user live suite skipped on the integration VM because
trusted real daemon/helper inputs were not supplied. This was an expected
prerequisite skip, not a pass and not H3 acceptance. No full debug build was
used.

## Remaining Gates

1. Run `ic/scripts/live-tests/darkirc-stage-a-h3.sh` against two disposable DarkIRC-enabled tenants on a real OpenRC host using the immutable release-v2 selector. Record only its secret-free summary.
2. Run the same committed harness against two disposable tenants under a real systemd user manager. Retain secret-free process coverage and post-cursor journal evidence.
3. Run final release CI, including the full shell harness, Rust checks, Python tests, and ShellCheck, on the commit containing any live-evidence documentation updates.
4. Confirm both live runs exercised the attested helper and measured daemon from one immutable release, denied peer access, and found no private canary in argv, environment, output, artifacts, or init logs.
5. Check off goal item 7 only after gates 1-4 pass. Keep Stages B-D and delivery disabled and separately gated.

## Relevant Files

- `ic/src/darkirc_key_manager.rs`
- `ic/src/bin/lunarwing-darkirc-key-helper/unix.rs`
- `ic/scripts/lunarwing-mt-admin.sh`
- `ic/scripts/live-tests/darkirc-stage-a-h3.sh`
- `ic/scripts/live-tests/darkirc_h3_analyzer.py`
- `ic/scripts/tests/test-darkirc-live-isolation.sh`
- `ic/scripts/tests/test-darkirc-release-selector.sh`
- `ic/scripts/tests/test-darkirc-attestation.sh`
- `ic/scripts/tests/test-darkirc-h3-analyzer.sh`
- `ic/scripts/tests/test_darkirc_h3_analyzer.py`
- `ic/scripts/tests/test-cleanup-legacy-weechat.sh`
- `ic/scripts/tests/test-darkirc-shell-review-fixes.sh`
- `ic/scripts/upgrade-tenant.sh`
- `ic/scripts/upgrade-tenant-version.sh`
- `ic/scripts/upgrade-tenant-kageho.sh`
- `docs/proposals/DARKIRC_SECURE_KEY_EXCHANGE.md`
- `docs/proposals/DARKIRC_KEY_EXCHANGE_NEXT_STAGES.md`
