# Coordination Knowledge Sharing (KNOW:) Setup

Cross-tenant knowledge sharing lets independent LunarWing daemons exchange durable findings over a shared XMPP MUC room. An agent publishes a finding with a `KNOW:` line in a coordination room; every other tenant watching that room ingests the finding into its own shared knowledge scope, where `knowledge_search` / `knowledge_get_topic` can find it.

This guide is the production setup for the configuration that was live-disproved in work item #97 (bare user JIDs never match) and re-verified on 2026-09-10 with full occupant JIDs.

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

1. **XMPP bridge** joins the coordination MUC room and only accepts groupchat from allowlisted rooms (`XMPP_ALLOW_ROOMS_JSON`). A room not on that list is silently dropped — the daemon neither joins it nor receives from it.
2. **Coordination hooks** classify inbound room traffic by scope (`COORD_CHANNELS`); the knowledge-share hook (`coordination.knowledge_share`, priority 13) parses `KNOW:` lines.
3. **Publisher allowlist** (`COORD_KNOWLEDGE_PUBLISHERS`) gates which senders may actually publish. Room occupancy is not authentication — any occupant can claim any nick the room forwards, so only operator-listed senders are ingested.
4. Ingest writes the note into the daemon-local shared knowledge scope, subject to per-sender and daemon-wide hourly budgets.

The failure that motivated this guide: every one of steps 1–3 was misconfigured at once (empty room allowlist, publishers set to the room bare JID, later "fixed" to user bare JIDs that also never match), and each failure is silent — the room keeps working, shares just never arrive.

## Required configuration

For two tenants `sunburst` and `ruffles` sharing room `mepuhiy@conference.xmpp.sobe.world`:

### 1. Bridge room allowlist — both tenants

In each tenant's `xmpp-bridge.env`:

```json
XMPP_ALLOW_ROOMS_JSON=["mepuhiy@conference.xmpp.sobe.world"]
```

This makes the bridge join the room and accept its inbound groupchat. An empty list means the daemon drops all coordination room traffic before any hook runs.

The daemon's native XMPP channel drives the bridge's join/receive allowlist through the bridge's `/v1/configure` call, so also set the CSV form in `lunarwing.env`:

```
XMPP_ALLOW_ROOMS=mepuhiy@conference.xmpp.sobe.world
```

### 2. Publisher allowlist — each tenant lists the OTHER agent's occupant JID

`sender_id` for groupchat is the **full occupant JID** (`room@server/nick`), so that is what the allowlist matches.

On **sunburst** (`lunarwing.env`) — allow the other agent's occupant JID:

```
COORD_KNOWLEDGE_PUBLISHERS=mepuhiy@conference.xmpp.sobe.world/ruffles
```

On **ruffles** (`lunarwing.env`):

```
COORD_KNOWLEDGE_PUBLISHERS=mepuhiy@conference.xmpp.sobe.world/sunburst
```

Three entry shapes, and which work:

| Entry shape | Example | Matches occupant sender? | Notes |
|---|---|---|---|
| Full occupant JID | `room@server/nick` | yes (exact) | Recommended. A nick change stops matching — treat nick changes as config changes. |
| Room bare JID | `room@server` | yes (room-wide) | Explicit opt-in: **every** occupant of that room may publish. Only for rooms whose entire membership you trust. |
| User bare JID | `user@server` | **no** | Never matches a MUC occupant sender. This is the misconfiguration that silently broke sharing for weeks. |

### 3. Coordination switches — both tenants

In each tenant's `lunarwing.env`:

```
COORD_ENABLED=true
COORD_CHANNELS=mepuhiy@conference.xmpp.sobe.world
COORD_KNOWLEDGE_SHARE=true
COORD_KNOWLEDGE_PUBLISH=true
```

- `COORD_CHANNELS` names the room(s) whose traffic the coordination hooks process. Without it, no coordination hooks register at all.
- `COORD_KNOWLEDGE_PUBLISH=true` enables emitting `KNOW:` shares; enable it only when every daemon in the room understands the format (mixed-version fleets should hold off).
- Optional tuning: `COORD_KNOWLEDGE_MAX_PER_HOUR` (default 30, max 1000), `COORD_KNOWLEDGE_MIN_CHARS` (default 12), `COORD_KNOWLEDGE_GLOBAL_MAX_PER_HOUR` (default 300, max 10000).

### 4. Restart both engines and re-test

Room membership is established at startup. After editing the env files, restart both tenant services (see [MT-ADMIN-QUICKSTART.md](MT-ADMIN-QUICKSTART.md) for the supported lifecycle commands), then run the doctor below.

## Verify with coord-doctor

`mt-admin coord-doctor <tenant>` is a read-only, init-agnostic check of the whole operator surface:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh coord-doctor sunburst
sudo ic/scripts/lunarwing-mt-admin.sh coord-doctor ruffles
```

It validates:

- `COORD_ENABLED`/`COORD_CHANNELS` are usable together;
- the publisher allowlist is non-empty and every entry is a full occupant JID or a coordination room bare JID (user-bare entries FAIL);
- every coordination room is on the bridge allowlist;
- live bridge `/v1/status` shows presence for each configured room (i.e. the daemon actually joined);
- live `KNOW:` ingest drop counters from the tenant's coord admin endpoint.

Exit code 0 means no definitive misconfiguration; warnings still deserve operator attention (e.g. a room-wide bare JID entry). It never prints secrets and never rewrites room or trust policy.

## Watch the live drop counters

With `COORD_ADMIN_TOKEN` and `COORD_ADMIN_PORT` set, the daemon exposes body-free ingest diagnostics:

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
