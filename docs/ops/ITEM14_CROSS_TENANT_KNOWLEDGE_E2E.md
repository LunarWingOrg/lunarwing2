# Item 14 — Cross-Tenant Knowledge Sharing E2E (Coordination Pipeline `KNOW:`)

**Date:** 2026-08-19 · **Branch:** `aj/kimi-14-20260819-0601` · **Host:** Arch Linux (systemd, user-manager tenants)

## Goal

Live end-to-end validation of cross-tenant knowledge sharing
(`ic/src/hooks/coordination_knowledge.rs`, merged under `aj/kimi-14-20260817-2100`;
review resolutions recorded in `docs/ops/AJ_ITEM_14_REVIEW.md`):

1. Spawn three new test tenants (`knowa`, `knowb`, `knowc`).
2. Have them share knowledge with each other over a shared coordination room
   using the `KNOW: <topic-slug> | <finding>` wire record.
3. Verify ingestion, provenance, trust boundaries, and retrieval in each
   tenant's own database (`shared` study-notes scope).
4. Record all results here.
5. Tear down the test tenants and all test infrastructure.

## Test architecture

- **Transport:** XMPP MUC. The production server `xmpp.sobe.world` does not
  offer in-band registration (probed 2026-08-19: post-TLS stream features
  advertise only SASL mechanisms, no `jabber.org/features/iq-register`), so a
  dedicated local Prosody container is used instead:
  - rootless podman container, published only on `127.0.0.1:5222`;
  - VirtualHost `localhost`; MUC component `conference.localhost`;
  - room `coord@conference.localhost`;
  - accounts: `knowa@localhost`, `knowb@localhost`, `knowc@localhost`
    (tenant agents), `intruder@localhost` (negative-test publisher).
- **TLS:** `tokio-xmpp` 6.0 verifies STARTTLS certificates against the system
  trust store (`rustls-native-certs`). A throwaway local test CA signs the
  prosody `localhost` certificate and is added to the host trust store for the
  duration of the test, then removed at teardown.
- **Tenants:** provisioned with `ic/scripts/lunarwing-mt-admin.sh add-tenant`
  (clones this branch, per-tenant PostgreSQL container, per-tenant
  `xmpp-bridge`). Tenant env additions beyond the mt-admin defaults:

  ```ini
  COORD_ENABLED=true
  COORD_AGENT_ID=<tenant>
  COORD_CHANNELS=coord@conference.localhost
  COORD_KNOWLEDGE_SHARE=true
  COORD_KNOWLEDGE_PUBLISH=true
  COORD_KNOWLEDGE_PUBLISHERS=coord@conference.localhost/knowa,coord@conference.localhost/knowb,coord@conference.localhost/knowc
  COORD_MAX_MESSAGES=50        # headroom for the test burst (default 2/hour)
  XMPP_ALLOW_ROOMS=coord@conference.localhost
  ```

  MUC occupant nick = JID localpart (daemon default), so the inbound
  `sender_id` seen by the knowledge hook is the full occupant JID
  `coord@conference.localhost/<nick>`; the publisher allowlist matches it
  case-insensitively.

## Test matrix

| # | Scenario | Expected |
|---|----------|----------|
| T1 | `knowa` sends `KNOW: <topic> \| <finding>` to the room | all 3 tenants ingest into own DB `shared` scope, status `needs-verification`, tag `coordination`, sender provenance `coord-knowa-<hash>` |
| T2 | `intruder` (not on any allowlist) sends `KNOW:` | no tenant ingests; room routing unaffected |
| T3 | malformed records (bad topic slug, finding below `COORD_KNOWLEDGE_MIN_CHARS`) | not ingested |
| T4 | `knowb` publishes its own topic | all 3 tenants ingest under `coord-knowb-<hash>` namespace |
| T5 | `knowa` re-sends the exact T1 record | exact duplicate → no-op (version count unchanged) |
| T6 | `knowa` updates its own T1 topic with new content | same path replaced, version history kept |

## Results

### Phase 1 — local XMPP infrastructure (DONE)

- prosody 0.11.9 (`docker.io/prosody/prosody:0.11.9`, rootless podman
  container `item14-prosody`) listening on `127.0.0.1:5222` only, no s2s.
- VirtualHost `localhost`, MUC `conference.localhost`, room creation open.
- Throwaway CA (`/tmp/item14-xmpp/certs/ca.crt`, 2-day validity) installed to
  `/etc/ca-certificates/trust-source/anchors/item14-test-ca.pem` +
  `update-ca-trust`; `openssl s_client -starttls xmpp -verify_hostname
  localhost` against the system bundle: **Verify return code: 0 (ok)**;
  python `ssl.create_default_context()` handshake + SASL feature negotiation
  OK (PLAIN, SCRAM-SHA-1).
- Accounts registered via `prosodyctl register`: `knowa`, `knowb`, `knowc`,
  `intruder` (passwords in `/tmp/item14-xmpp/accounts.txt` mode 600,
  destroyed at teardown; not recorded in the repo).

## Teardown record

_(filled in at teardown)_
