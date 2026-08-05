# DarkIRC Secure Staged Key Exchange Status

- **Status:** Stage A source hardening integrated; current live-host acceptance pending.
- **Implementation baseline:** `cae4770` (`fix(darkirc): harden Stage A exchange recovery`).
- **Current integration branch:** `rarity/kimi-7-20260805-1803`.
- **Historical live evidence:** OpenRC run on 2026-08-05, before immutable release v2.

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
| H3 | High | Harnesses ready; current live evidence pending | Six completion crash windows have regressions. Real-host harnesses cover cross-user isolation, process argv/environment, stdout/stderr, public artifacts, and systemd/OpenRC logs. |
| M1 | Medium | Fixed; locally verified 2026-08-05 | Daemon, helper, helper source revision, and attestation publish as one immutable, fsynced release. Readers pin one generation and helper executes the measured daemon descriptor. Upgrades require current helper source while digest-valid retained releases remain rollback-capable. Stale units and systemd drop-ins fail closed. |
| M2 | Medium | Fixed; verified 2026-08-03 | Import pins one validated descriptor, rejects unsafe ownership/links/modes, and resumes interrupted imports only with matching protected state and stopped writers. |
| M3 | Medium | Fixed; locally verified 2026-08-05 | Legacy WeeChat cleanup and adapter restart now route through mt-admin with systemd-user/OpenRC parity before upgrade starts. |
| M4 | Medium | Fixed; verified 2026-08-03 | Generator runtime and combined output are bounded; timeout or overflow kills and reaps the isolated process group. |
| L1 | Low | Fixed; locally verified 2026-08-05 | `doctor` uses capability-relative, no-follow transaction-directory inspection and fails closed on unsafe paths or I/O errors. |
| L2 | Low | Fixed | Active docs distinguish implemented Stage A source from pending live acceptance and later lifecycle stages. |

## H3 Harnesses

`ic/scripts/live-tests/darkirc-stage-a-h3.sh` is the acceptance harness. It
requires an explicit destructive-test acknowledgement, two real DarkIRC-enabled
tenants, root, a real systemd or OpenRC boot, and `strace` exec-delay injection.
It drives mt-admin through a real Stage A exchange and fails unless `/proc`
captures observe mt-admin, the attested helper, and the measured daemon. It
scans exact private-key canaries across process argv/environment, command
stdout/stderr, public artifacts, and systemd journals or OpenRC logs.

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

Current v2.0.5 integration passed:

- 89 core DarkIRC key-manager tests.
- 22 key-helper tests.
- Full GitLab shell harness.
- Locked `cargo check`, rustfmt, and all-feature Clippy with warnings denied.
- Eleven H3 analyzer tests, ShellCheck, and `git diff --check`.

The optional scratch-user live suite skipped because trusted real daemon/helper
inputs were not supplied. That skip is not H3 acceptance. No full debug build
was used.

## Remaining Gates

1. Run the release-v2 H3 acceptance harness on a disposable OpenRC host and record only its secret-free summary.
2. Run the same harness under a real systemd user manager and capture journal evidence.
3. Run final release CI, including ShellCheck.
4. Close Stage A only after all three gates pass; keep Stages B-D and delivery separate.

## Relevant Files

- `ic/src/darkirc_key_manager.rs`
- `ic/src/bin/lunarwing-darkirc-key-helper/unix.rs`
- `ic/scripts/lunarwing-mt-admin.sh`
- `ic/scripts/live-tests/darkirc-stage-a-h3.sh`
- `ic/scripts/live-tests/darkirc_h3_analyzer.py`
- `ic/scripts/tests/test-darkirc-live-isolation.sh`
- `ic/scripts/tests/test-darkirc-release-selector.sh`
- `ic/scripts/tests/test-darkirc-attestation.sh`
- `docs/proposals/DARKIRC_SECURE_KEY_EXCHANGE.md`
- `docs/proposals/DARKIRC_KEY_EXCHANGE_NEXT_STAGES.md`
