# Coordination Knowledge Sharing (KNOW:) Setup

Cross-tenant knowledge sharing lets independent LunarWing daemons exchange durable findings over a shared XMPP MUC room. An agent publishes a finding with a `KNOW:` line in a coordination room; every other tenant watching that room ingests the finding into its own shared knowledge scope, where `knowledge_search` / `knowledge_get_topic` can find it.

The live check for WI#97 on 2026-09-10 verified publishing with full occupant JIDs.

The `KNOW:` parser and ingestion hook live in `ic/src/hooks/coordination_knowledge.rs`.

## Wire format

```text
KNOW: <topic-slug> | <one-line finding>
```

Example:

```text
KNOW: mt-admin-cgroups | Bounded builds use per-tenant build subtrees, see 13-cgroups-openrc.sh
```

Wire claims are untrusted: they are stored as `needs-verification`, without confidence or a source commit, under a `coord-` agent namespace with a stable sender hash. Ingestion is fail-open — a dropped or rejected share never rejects room traffic.

## How the pieces fit

Inbound path, in order:

1. **XMPP bridge.** The installable XMPP channel sends its `rooms` setup field
   as `allow_rooms` in `/v1/configure`. The bridge uses that list to join rooms
   and filter groupchat. The bridge merges in `encrypted_rooms` when
   normalizing the configuration. It drops groupchat outside the effective
   allowlist before any coordination hook runs. An empty effective list
   drops all groupchat. MT admin initializes the legacy room list empty
   when it renders a new tenant.
2. **Coordination hooks** classify inbound room traffic by scope (`COORD_CHANNELS`); the knowledge-share hook (`coordination.knowledge_share`, priority 13) parses `KNOW:` lines.
3. **Publisher allowlist** (`COORD_KNOWLEDGE_PUBLISHERS`) gates which senders may actually publish. Room occupancy is not authentication — any occupant can claim any nick the room forwards, so only operator-listed senders are ingested.
4. Ingest writes the note into the daemon-local shared knowledge scope, subject to per-sender and daemon-wide hourly budgets.

## Required configuration

For two tenants `example-a` and `example-b` sharing room `coordination@conference.example.org`:

### 1. Room allowlist — both tenants

For a tenant with the legacy XMPP daemon configuration, set this in
`lunarwing.env`:

```
XMPP_ALLOW_ROOMS=coordination@conference.example.org
```

At startup, `migrate_native_xmpp_to_installable` copies the resolved room list
into `extensions.xmpp.setup_fields.rooms` when the daemon has XMPP credentials,
the installable channel, and a settings store. The legacy `xmpp_allow_rooms`
setting is the fallback when `XMPP_ALLOW_ROOMS` is unset or empty. For a tenant
configured through extension setup alone, set the XMPP `rooms` field there.

For direct configure calls, `ic/scripts/xmpp-configure.sh` accepts
`XMPP_ALLOW_ROOMS_JSON` in its environment. Keep the same room in each tenant's
`xmpp-bridge.env` if you use the static `coord-doctor` check.

```dotenv
XMPP_ALLOW_ROOMS_JSON=["coordination@conference.example.org"]
```

### 2. Publisher allowlist — each tenant lists the OTHER agent's occupant JID

`sender_id` for groupchat is the **full occupant JID** (`room@server/nick`), so that is what the allowlist matches.

On **example-a** (`lunarwing.env`) — allow the other agent's occupant JID:

```
COORD_KNOWLEDGE_PUBLISHERS=coordination@conference.example.org/example-b
```

On **example-b** (`lunarwing.env`):

```
COORD_KNOWLEDGE_PUBLISHERS=coordination@conference.example.org/example-a
```

Three entry shapes, and which work:

| Entry shape | Example | Matches occupant sender? | Notes |
|---|---|---|---|
| Full occupant JID | `room@server/nick` | yes (case-insensitive) | Recommended. A nick change stops matching — treat nick changes as config changes. |
| Room bare JID | `room@server` | yes (room-wide) | Explicit opt-in: **every** occupant of that room may publish. Only for rooms whose entire membership you trust. |
| User bare JID | `user@server` | **no** | Never matches a MUC occupant sender. This is the misconfiguration that silently broke sharing for weeks. |

### 3. Coordination switches — both tenants

In each tenant's `lunarwing.env`:

```
COORD_ENABLED=true
COORD_CHANNELS=coordination@conference.example.org
COORD_KNOWLEDGE_SHARE=true
COORD_KNOWLEDGE_PUBLISH=true
```

- `COORD_CHANNELS` names the room(s) whose traffic the coordination hooks
  process. Registration requires `COORD_ENABLED=true`, a nonempty room list,
  and a valid `COORD_AGENT_ID` for each daemon. MT admin normally fills in
  `COORD_AGENT_ID` from the tenant name; verify the value on older tenants.
- `COORD_KNOWLEDGE_PUBLISH=true` enables emitting `KNOW:` shares; enable it only when every daemon in the room understands the format (mixed-version fleets should hold off).
- Optional tuning: `COORD_KNOWLEDGE_MAX_PER_HOUR` (default 30, max 1000), `COORD_KNOWLEDGE_MIN_CHARS` (default 12), `COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR` (default 300, max 10000).

### 4. Restart both engines and re-test

Room membership is established at startup. After editing the env files, restart both tenant services (see [MT-ADMIN-QUICKSTART.md](MT-ADMIN-QUICKSTART.md) for the supported lifecycle commands), then run the doctor below.

## Verify with coord-doctor

`mt-admin coord-doctor <tenant>` reports room configuration and bridge status:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh coord-doctor example-a
sudo ic/scripts/lunarwing-mt-admin.sh coord-doctor example-b
```

The report shows coordination switches, publisher shape checks, and whether
coordination rooms appear in the bridge environment file. It also prints
`configured_rooms` and `rooms_with_presence` when the bridge status is reachable,
and `KNOW:` drop counters when the admin endpoint is available.

A stale bridge environment can fail the static check even when the room is
joined. Check each room against the reported live presence. The script can exit
0 when that list is empty, and its comparison treats comma-joined room lists
as single values. Finish verification with a real share in a disposable room.

## Watch the live drop counters

When the coordination admin listener is enabled with `COORD_ADMIN_TOKEN`
and knowledge-share diagnostics are registered, `/coord/knowledge` exposes
body-free counters on the admin port. Use the configured `COORD_ADMIN_PORT`:

```bash
curl -sf -H "Authorization: Bearer $COORD_ADMIN_TOKEN" \
  http://127.0.0.1:$COORD_ADMIN_PORT/coord/knowledge
```

```json
{"dropped":[{"reason":"invalid_sender","count":0},
            {"reason":"not_allowed_publisher","count":12},
            {"reason":"sender_rate_limited","count":0},
            {"reason":"global_rate_limited","count":0}]}
```

A growing `not_allowed_publisher` count while shares "don't arrive" is the signature of a publisher allowlist mismatch — the room traffic reaches the daemon but no entry matches the occupant sender.

## Live wire-path test matrix

For release-sensitive changes to this path, run the four-case matrix (automated by `ic/scripts/tests/test-coord-knowledge-matrix.sh`):

1. **Cross-tenant happy path** — tenant A's agent emits `KNOW:`; the note appears in tenant B's shared knowledge with `needs-verification` status and `submitted_by` equal to A's full occupant JID.
2. **Non-allowlisted publisher is ignored** — an occupant not on B's allowlist emits `KNOW:`; nothing ingests on B and the `not_allowed_publisher` counter increments; room traffic is otherwise unaffected.
3. **Room-wide bare JID opt-in** — B allowlists the room bare JID instead; any occupant of the room now ingests, with raw sender provenance preserved.
4. **User-bare JID never matches** — B allowlists a user bare JID; occupant shares are all dropped as `not_allowed_publisher` (this pins the regression that motivated WI#97).

The script provisions two throwaway tenants against a local XMPP domain, drives all four cases through the real daemon, and fails on the first incorrect observation.

## Security notes

- Room occupancy is not authentication. Only list publishers you actually trust; prefer full occupant JIDs over room-wide bare JIDs unless room membership is itself controlled.
- The coordination admin server binds `127.0.0.1` only and requires a constant-time bearer token; loopback binding alone is not the authorization boundary on multi-user hosts.
- Ingested notes carry no confidence and start `needs-verification`; consuming agents should verify before promoting them.
