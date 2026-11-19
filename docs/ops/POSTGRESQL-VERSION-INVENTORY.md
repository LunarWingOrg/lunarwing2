# PostgreSQL version inventory (WI#744)

Survey date: 2026-11-17. Host: `ajhermes-z`. Branch:
`aj/kimi-wi744-20261117-0900`.

This is a point-in-time inventory of the PostgreSQL versions LunarWing runs
and accepts. It is not a policy change: the production pin and the security
floor it encodes were set by the
[2026-11-12 CVE audit](POSTGRESQL-2026-11-12-CVE-AUDIT.md) and remain as
described there. Re-run the checks in [How to check](#how-to-check) before
relying on the live rows.

## Short answer

There is no single version. The surfaces differ, and only one is enforced:

| Surface | Reference | Version | Enforced by |
|---|---|---|---|
| Daemon / setup prerequisite | any server, plus `vector` available | PostgreSQL **15 or later** | `ic/src/db/mod.rs:252`, `ic/src/setup/wizard.rs:944` |
| Multi-tenant production default | `docker.io/pgvector/pgvector:pg16@sha256:484e8380…` | **16.16**, pgvector **0.8.7** | `ic/scripts/lunarwing-mt-admin.sh:163-164`; floor gate `ic/scripts/mt-admin-lib/11-postgres.sh:424-437` |
| CI PostgreSQL integration lane | same pinned digest as production | **16.16** | `.gitlab/ci/rust.yml:177`; `.gitlab/scripts/validate_config.py:28-31,3098-3120` |
| Dev compose (`ic/docker-compose.yml:4`) | `pgvector/pgvector:pg16` | floating tag — whatever the registry currently serves | nothing |
| XMPP test env (`ic/scripts/lunarwing-xmpp-test-env.sh:744`) | `pgvector/pgvector:pg16` | floating tag | nothing |
| Multica local test server (`ic/scripts/multica-local-test-server.sh:53`) | `pgvector/pgvector:pg17` | 17.x | nothing; separate multica schema, not LunarWing |

So: **the daemon only requires 15**, while the reviewed tenant image is
**PostgreSQL 16.16 with pgvector 0.8.7**, and the multi-tenant refresh path
refuses anything that is not major 16 with `server_version_num >= 160016`.

## Why those two floors exist

- **15 is the schema floor.** `ic/migrations/V21__fix_null_agent_id_unique_constraint.sql:32`
  uses `UNIQUE NULLS NOT DISTINCT`, which PostgreSQL added in 15.
  `V1__initial.sql:6` creates the `vector` extension, so the server must also
  ship pgvector. The daemon and the setup wizard both check
  `SHOW server_version` and reject a major below 15.
- **16.16 is the security floor.** The 2026-11-12 PostgreSQL release fixed 43
  CVEs in the 16 line, and the tenant refresh path (`refresh-pg-image`) gates
  the image preflight and the running server on major 16 and
  `server_version_num >= 160016`. The emergency rollback knob
  (`LUNARWING_MT_PG_ALLOW_UNPATCHED`) acknowledges an older 16.x for one exact
  tenant; see [the audit](POSTGRESQL-2026-11-12-CVE-AUDIT.md).

The CI lane and mt-admin are pinned to the *same* digest, and
`validate_config.py` fails the pipeline if either reference drifts from the
other. The digest resolves to PostgreSQL 16.16 (Debian 16.16-1.pgdg12+1) and
pgvector 0.8.7; the pull-and-report evidence is in
[the audit](POSTGRESQL-2026-11-12-CVE-AUDIT.md#scope-and-image-verification).

## Live inventory on this host

Tenants come from `/etc/lunarwing/ports.json`; both were inspected read-only
with mt-admin.

| Tenant | `server_version` | `server_version_num` | pgvector | Container image reference | Image ID | Quadlet `Image=` |
|---|---|---|---|---|---|---|
| `antelope` | 16.16 (Debian 16.16-1.pgdg12+1) | 160016 | 0.8.7 | `docker.io/pgvector/pgvector@sha256:484e8380…` | `b0108689bf51…` | pinned digest |
| `hermes` | 16.14 (Debian 16.14-1.pgdg12+1) | 160014 | 0.8.6 | `docker.io/pgvector/pgvector:pg16` | `8e5355e9ff39…` | floating `pg16` tag |

`antelope` is at the reviewed pin. `hermes` is **below the 160016 security
floor** and still carries a floating reference, so the next `podman pull`
would change its server version without any LunarWing-side record of the
transition.

### Findings

1. **`hermes` needs `refresh-pg-image`.** Its quadlet and container still use
   the legacy floating `pg16` tag, so it was never moved onto the reviewed
   digest and is running 16.14 with pgvector 0.8.6. Refresh is deliberately
   explicit: the quadlet fixture asserts that a legacy floating reference is
   retained, not silently switched to the pin
   (`ic/scripts/tests/test-mt-admin-pg-quadlet-image.sh:44`). A fresh
   `pgvector/pgvector:pg16` pull *today* may resolve to a patched 16.x, but
   that is not the reviewed 16.16 digest and must not be treated as patched.
2. **Dev-only surfaces are unpinned by design.** `ic/docker-compose.yml` and
   the XMPP test env use the floating `pg16` tag; `hermes` is what that drift
   looks like after an image is left alone. That is acceptable for throwaway
   databases, but a dev database is not evidence of the production version.
3. **The multica test server is not a LunarWing database.** It runs
   `pgvector/pgvector:pg17` for the Lunartica/multica bridge schema; do not
   read that as a LunarWing 17 migration or copy it into tenant setup.
4. **pgvector follows the data directory, not the image.** A database created
   under an older image keeps its extension version (0.8.6 on `hermes`); only
   fresh databases get the image's packaged 0.8.7. `refresh-pg-image`
   deliberately does **not** run `ALTER EXTENSION vector UPDATE`.

## Re-verification 2026-11-19 (WI#777)

The 2026-11-17 rows above were recorded on `ajhermes-z` and are not re-verified
here. This section records what a second host could prove.

Host `colgatev` (OpenRC `0.63.3`, rootless Podman `6.1.4`), read-only
`pg-image-status` before any change:

| Tenant | Before | After | Disposition |
|---|---|---|---|
| `dash` | 16.15, floating `pgvector/pgvector:pg16`, image `f1447071e6c8…`, pgvector 0.8.7 | 16.16, pinned `…@sha256:484e8380…`, image `b0108689bf51…`, pgvector 0.8.7 | `refresh-pg-image dash` on 2026-11-19; the daemon and PostgreSQL service were started before and after, and the named data volume was retained |
| `antelope` | 16.16, pinned digest, image `b0108689bf51…`, pgvector 0.8.7 | unchanged | already at the reviewed pin; no refresh needed |
| `mcpvalidation` | — | — | not present in `/etc/lunarwing/ports.json` at re-verification; recorded as removed, not left uninspected |

`dash` is the WI#664 row that was left uninspected: it was below the `160016`
floor and still carried a floating reference, so it was exposed to the CVEs
fixed between 16.15 and 16.16 until this refresh. `hermes` and `ajhermes-z`
were not reachable from this host; their 16.14 record remains unverified and
is a per-host follow-up, not an assumed closure.

The pgvector extension question is answered with evidence in
[the audit](POSTGRESQL-2026-11-12-CVE-AUDIT.md#re-verification-2026-11-19-wi777):
a 0.8.6 catalog survives the image swap on the new library, and the shipped
`ALTER EXTENSION vector UPDATE` path changes catalog comments only.

## How to check

```bash
# Per-tenant image reference, image ID, and live SELECT version()
sudo ic/scripts/lunarwing-mt-admin.sh pg-image-status <tenant>

# Machine-comparable version and extension version
sudo ic/scripts/lunarwing-mt-admin.sh psql <tenant> -Atc \
  "SHOW server_version_num; SELECT extversion FROM pg_extension WHERE extname='vector'"

# What the tenant's unit will pull next
sudo grep '^Image=' /home/<tenant>/.config/containers/systemd/lunarwing-pg-<tenant>.container
```

`pg-image-status` prints `server_version=unavailable` when the container is
down; that means *uninspected*, not patched.

## Bumping the reviewed version

Keep the three references identical and land them in one change:

1. `PG_IMAGE` default in `ic/scripts/lunarwing-mt-admin.sh:164`.
2. The `rust-test-postgres-integration` service in `.gitlab/ci/rust.yml:177`.
3. `REVIEWED_PG_IMAGE` in `.gitlab/scripts/validate_config.py:28-31`.

Then run `refresh-pg-image <tenant>` per tenant and record the before/after
`pg-image-status` output as the audit asks. Do not change the 15 floor in
`ic/src/db/mod.rs` / `ic/src/setup/wizard.rs` unless the schema floor itself
moves; the production 16.16 gate lives in mt-admin, not in the daemon.
