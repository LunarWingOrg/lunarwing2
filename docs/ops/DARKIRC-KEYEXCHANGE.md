# DarkIRC Key Exchange Status

Phase 0/1 and the Stage A command surface are implemented for multi-tenant
DarkIRC through `ic/scripts/lunarwing-mt-admin.sh` and the typed Unix key helper.
Final Stage A acceptance awaits live two-user leak/isolation runs on real
systemd-user and OpenRC hosts.

Authoritative documents:

- [Secure Automated DarkIRC Key Exchange](../proposals/DARKIRC_SECURE_KEY_EXCHANGE.md)
- [DarkIRC Key Exchange Next Stages](../proposals/DARKIRC_KEY_EXCHANGE_NEXT_STAGES.md)
- [DarkIRC Multitenant Operations](DARKIRC-MULTITENANT.md)
- [Goal 1 Acceptance Status](DARKIRC-KEYEXCHANGE-GOAL1-STATUS.md)

The implemented surface is public-only `prepare`, `respond`, deferred
`complete`, `cancel`, inspection, adoption, recovery, and structured migration.
Full peer fingerprint authorization is mandatory. Completion stops at
`Installed`.

Activation, health-gated rollback, encrypted-DM round-trip confirmation,
rotation, revocation, same-host pairing, QR/browser delivery, and
trusted-channel automation remain disabled pending separate review.
