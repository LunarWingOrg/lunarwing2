# DarkIRC Key Exchange Next Stages

**Status (2026-08-03):** Phase 0/1 and Stage A implemented. Later stages remain disabled.

This document is the sequencing boundary for
[`DARKIRC_SECURE_KEY_EXCHANGE.md`](DARKIRC_SECURE_KEY_EXCHANGE.md). A later stage
must not be enabled merely because its data types or journal fields already
exist.

## Implemented Boundary

Phase 0/1 and Stage A provide:

- Contact-preserving, semantic, journaled baseline and ledger updates.
- Root-attested DarkIRC/helper compatibility with a pinned source revision,
  measured binary digest, generator parser self-test, and unprivileged
  disposable builds.
- Stable random tenant scope IDs and mt-admin lifecycle serialization for
  systemd-user and OpenRC.
- Fingerprint-only `list`, `status`, `doctor`, and metadata-only `adopt`.
- Structured `darkirc-contacts-v1` migration that excludes runtime transaction
  state, preserves the same tenant scope only from a stopped source, renders
  target-specific ports, and keeps credential staging outside `/tmp`.
- Public-only `prepare`, `respond`, deferred `complete`, `cancel`, `exchanges`,
  and `recover` commands with mandatory full-fingerprint authorization.
- Bounded pending count, record size, aggregate size, and lifetime; private-key
  reuse checks; durable expiry/cancellation/installation tombstones; and
  candidate-first fsynced pending creation with deterministic recovery.
- Capability-relative no-follow file access with tenant owner/group, exact
  `0700` directories, exact `0600` single-link files, bounded reads, directory
  fsync, and atomic same-directory replacement.
- Strict `darkirc-health <tenant> --strict [--json]` diagnostics covering both
  init systems and authenticated adapter connectivity without putting the
  bearer in argv.

`complete` installs deferred configuration and ledger state only. Its terminal
Stage A result is `Installed`. Mt-admin always forwards `--defer-apply`; it does
not restart, reload, health-gate, activate, roll back, or claim peer verification.

Public artifact delivery may fail after pending state is durable. The error
returns only the committed offer ID; repeating the same semantic operation
returns the stored byte-identical public artifact instead of generating another
private key.

## Stage B: Local Activation — Disabled

Implement only after a dedicated design and recovery review:

1. Durable activation intent, operation ID, and bounded deadline.
2. Init-agnostic restart or verified reload exclusively through mt-admin.
3. Strict service/adapter health plus active-config fingerprint verification.
4. Journal recovery for every pre-restart, restart, health, and cleanup phase.
5. Automatic local rollback with a bounded previous-config snapshot when health
   fails; no restart loop after rollback failure.
6. Exact systemd-user/OpenRC parity tests and secret-leak scans.

Stage B may advance `Installed` to `LocallyActivated`. It must not infer peer
installation or encrypted-message success from local health.

## Stage C: Peer Verification And Rollback — Disabled

Define and review before implementation:

- Explicit bidirectional encrypted-DM round-trip attestation bound to the same
  transcript and full peer fingerprint.
- Verification deadline, overdue state, operator recovery choices, and rollback
  snapshot retention/cleanup.
- Crash recovery for confirmation, keep, and rollback decisions.
- Compromised-key policy that can never restore a suspected key.

Only this stage may advance `LocallyActivated` to `PeerVerified`.

## Stage D: Rotation, Revocation, And Same-Host Pairing — Disabled

Rotation requires monotonic local generation, old/new transcript binding,
private-key non-reuse, explicit maintenance-window semantics, and delayed-message
impact documentation. Remote generation values remain untrusted hints.

Revocation requires a durable tombstone before config removal and must never
remove that tombstone merely to restore service. Same-host pairing requires
stable two-tenant lock ordering, two-sided staging, partial-commit recovery, and
proof that private material never crosses tenant boundaries.

## Delivery Surfaces — Disabled

QR, browser, onboarding, and trusted-channel automation require a separate
threat model. They may transport public artifacts only and must not:

- receive or return a private key;
- create another secret store;
- authorize by nickname, sender pairing, or message receipt;
- bypass the full independently obtained fingerprint;
- activate a contact or enable a later-stage command.

## Promotion Rule

A stage may be enabled only after its own source review, crash-injection tests,
secret-leak scans, systemd-user/OpenRC parity tests, documentation update, and
explicit release-goal approval. Passing Stage A tests is not evidence that a
later stage is safe.
