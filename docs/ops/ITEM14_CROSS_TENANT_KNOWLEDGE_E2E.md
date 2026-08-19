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

### Phase 2 — tenant provisioning (DONE)

- `add-tenant knowa|knowb|knowc --xmpp-jid <name>@localhost --xmpp-password …`
  (passwords match the prosody accounts; never written to the repo).
  knowa got port block 10020–10029 (bridge 10022, postgres 10023,
  coord_admin 20029); knowb/knowc took the next sequential blocks.
  Per-tenant PostgreSQL quadlet containers are up (`lunarwing-pg-<name>`).
- Env edits per the table above applied to all three
  `/home/<t>/lunarwing/env/lunarwing.env` **before first boot** (env→DB
  settings migration happens on first daemon start). mt-admin auto-set
  `COORD_AGENT_ID=<tenant>` at provisioning.
- XMPP for tenants is the bridge-backed WASM channel model (`xmpp.wasm` →
  loopback `xmpp-bridge`), so tenant builds run as
  `build-tenant <t> --with-wasm` (release profile, flock-serialized, 12 jobs).

### Phase 3 — builds (DONE)

- All three `build-tenant --with-wasm` runs completed (log
  `/tmp/item14-builds.log`): release `lunarwing` binary, release
  `xmpp-bridge` binary, and 13 WASM extensions (4 channels incl. `xmpp.wasm`,
  9 tools) installed per tenant. No build failures.
- `start-tenant knowa|knowb|knowc` (mt-admin, systemd user units): all
  `lunarwing-<t>.service` and `xmpp-bridge-<t>.service` active; per-tenant
  PostgreSQL containers running. Each bridge logged `XMPP connected
  jid=<t>@localhost/<t>` and `XMPP room join requested
  room=coord@conference.localhost nick=<t>`; each daemon logged `Coordination
  hooks registered agent_id=<t> channels=["coord@conference.localhost"]
  knowledge_share=true`.

### Phase 4 — live test matrix

Helper scripts (in `/tmp/item14-xmpp/`, not committed): `send-room.sh
<tenant> <content>` posts a groupchat message through the tenant's loopback
bridge API; `know-db.sh <tenant> [sql]` queries the tenant's postgres
`memory_documents`; `wait-note.sh` polls for a path; `xmpp_inject.py` is a
stdlib XMPP client for the intruder account.

**Design correction discovered during bring-up (affects T1/T4
expectations):** the XMPP layer drops *self-authored* MUC reflections —
`ic/src/channels/xmpp/mod.rs:1543` compares the inbound occupant nick with
the room's tracked `self_nick` (set at join for plain rooms too) and drops
the echo. Therefore the *publishing* tenant never ingests its own `KNOW:`
record; only the watching tenants do. Evidence for knowa below: its daemon
log shows zero sightings of the T1 content and its DB has zero study-note
rows, while both watchers ingested.

#### T1 — knowa publishes; watchers ingest (PASS, with the self-drop note above)

Sent (via knowa's bridge): `KNOW: mt-tenant-builds | Bounded tenant builds
run serially under a shared flock with twelve jobs`

- knowb daemon log: `coordination knowledge note ingested
  path=study-notes/coord-coord-conference-localhost-knowa-b11877a387a8d59426d57106/mt-tenant-builds.md
  sender="coord@conference.localhost/knowa"`.
- knowb + knowc DBs (identical rows):

  ```
  study-notes/coord-coord-conference-localhost-knowa-b11877a387a8d59426d57106/mt-tenant-builds.md
    status       = needs-verification
    agent        = coord-coord-conference-localhost-knowa-b11877a387a8d59426d57106
    submitted_by = coord@conference.localhost/knowa
    tags         = ["coordination"]
    confidence/studied_at/source_commit = null   (untrusted wire claim)
  ```

  The agent slug matches the independently computed
  `sender_agent_slug("coord@conference.localhost/knowa")` (SHA-256
  `b11877a387a8d59426d57106…`, 24 hex chars retained).
- knowa DB: `(0 rows)` — publisher self-drop, by design (see above).
- Shared scope confirmed: rows live in `memory_documents` under the daemon's
  `shared` workspace scope (`user_id='shared'`), i.e. the native
  `knowledge_search`/`knowledge_get_topic` tools can read them.

#### T2 — intruder (non-allowlisted publisher) ignored; routing unaffected (PASS)

`intruder@localhost` joined the room via `xmpp_inject.py` and sent:
`KNOW: intruder-topic | this finding comes from an unlisted publisher`

- All three tenant DBs: `SELECT count(*) … WHERE path LIKE '%intruder%'` →
  **0 rows** in knowa, knowb, knowc.
- Room routing itself was unaffected (fail-open design): knowb's daemon log
  shows the message received and queued normally —
  `WASM emit_message called user_id=coord@conference.localhost/intruder
  user_name=Some("intruder")` → `Prepared polled message for agent queue`.
  The knowledge hook declined to ingest it: the publisher allowlist is
  enforced on the full occupant JID (`coord@conference.localhost/intruder`
  is listed nowhere), and the ignore path logs at debug level only.

#### T3 — malformed records rejected (PASS)

Both sent by knowa (an allowlisted publisher, so rejection is attributable to
record parsing, not the allowlist):

1. Bad topic slug: `KNOW: Not_A_Slug | this finding has a deliberately
   invalid topic slug` (uppercase + underscore fails `is_valid_slug`).
2. Finding below `COORD_KNOWLEDGE_MIN_CHARS=12`: `KNOW: tiny-finding | short`
   (5 chars).

Results — study-note row counts unchanged in every tenant
(knowa 0, knowb 1, knowc 1; the 1 is the T1 row) and `malformed_rows = 0`
everywhere. Neither record produced a document.

#### T4 — knowb publishes its own topic; watchers ingest (PASS)

Sent (via knowb's bridge): `KNOW: bridge-polling-model | The xmpp wasm
channel polls the loopback bridge for inbound room messages`

- knowa + knowc DBs: ingested at
  `study-notes/coord-coord-conference-localhost-knowb-06dd29f7aee42fd2c546fbb2/bridge-polling-model.md`
  with `status = needs-verification`,
  `submitted_by = coord@conference.localhost/knowb`. The agent slug matches
  the independently computed `sender_agent_slug` for knowb.
- knowb DB: `self_ingested = 0` — publisher self-drop again, as designed.

#### T5 — exact duplicate is a no-op (PASS)

knowa re-sent the byte-identical T1 record. Before/after comparison on the
`mt-tenant-builds` row in both watcher DBs:

| tenant | updated_at before | updated_at after | len | rows | versions |
|--------|-------------------|------------------|-----|------|----------|
| knowb  | 08:15:02.681321+00 | 08:15:02.681321+00 | 155 | 1 | 0 |
| knowc  | 08:15:02.834714+00 | 08:15:02.834714+00 | 155 | 1 | 0 |

`updated_at` is bit-identical — the hook's duplicate check (content +
metadata equality, `coordination_knowledge.rs:436`) skipped the write
entirely. No duplicate rows, no version churn.

## Teardown record

_(filled in at teardown)_
