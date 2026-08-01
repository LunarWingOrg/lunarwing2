# DarkIRC Key Exchange Next Stages

**Status (2026-08-01):** Phase 0/1 and Stage A implemented. Later stages open.

## Landed Boundary

- Contact-preserving, journaled baseline and ledger updates.
- Root-attested helper/binary compatibility; unprivileged disposable builds.
- Stable random tenant scope IDs and systemd-user/OpenRC mt-admin integration.
- Stage A `prepare`, `respond`, deferred `complete`, `cancel`, inspection, doctor,
  recovery, and structured migration commands.
- Public-only exchange artifacts with mandatory full-fingerprint confirmation.
- Bounded pending state, expiry/cancellation/installation tombstones, private-key
  reuse prevention, peer-key uniqueness, capability-relative filesystem access,
  and crash-finalized completion journals.
- Kawarimi export/import carries structured contacts and stable scope without
  copying host-specific DarkIRC runtime state.
- Standalone `darkirc-health <tenant> --strict [--json]` checks both init
  services and authenticated adapter connectivity without exposing bearer in
  argv. It is diagnostic only, not activation state.
- Named artifacts use capability-relative no-follow I/O. If delivery fails after
  pending state commits, helper reports committed `offer_id`; rerunning same
  prepare/respond operation reuses stored public artifact instead of generating
  another secret.
- Tenant mutation, export, import, removal, build, and upgrade paths serialize on
  per-tenant lifecycle lock. Shared DarkIRC publication restores previous binary
  and compatibility attestation if helper build or self-test fails.

`complete` installs deferred configuration state only. It does not activate or
claim peer verification. Installed TOML may be loaded by any later daemon
restart, so current surface is maintenance-only until Stage B health gate lands.

## Stage B: Local Activation

Implement only after dedicated design review:

1. Durable activation intent and deadline.
2. Daemon reload/restart through init-agnostic mt-admin lifecycle.
3. Bind existing strict local health diagnostic plus config fingerprint
   verification into durable activation transaction.
4. Crash recovery for every activation journal phase.
5. Automatic rollback when local health gate fails.

Acceptance requires systemd-user and OpenRC tests, injected crashes around every
commit/restart boundary, and proof no private material reaches logs or argv.

## Stage C: Peer Verification And Rollback

Add explicit peer-confirmed transcript/fingerprint verification. Nickname or IRC
presence never authorizes transition. Define verification deadline, overdue
state, operator recovery, rollback retention, and compromised-key rules before
code changes.

## Stage D: Rotation And Revocation

Rotation requires monotonic local generation, old/new transcript binding, replay
resistance, and rollback policy. Revocation requires durable tombstone before
config removal; compromised keys must never be restored.

## Deferred Delivery

Same-host pairing, browser APIs, QR encoding, and trusted-channel automation stay
out of scope until Stage B/C state machine and auth model pass separate security
review. Delivery layers may transport public artifacts only.
