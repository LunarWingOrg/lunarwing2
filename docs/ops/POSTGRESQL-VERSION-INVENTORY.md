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

## Live inventory on this host, 2026-11-17 snapshot

This is the point-in-time row set from the WI#744 survey, not the current
state. `hermes` was refreshed and a third tenant appeared afterwards; the
current rows are in [the 2026-11-19 re-verification](#re-verification-2026-11-19-wi780).

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

1. **`hermes` needed `refresh-pg-image`.** Its quadlet and container still used
   the legacy floating `pg16` tag, so it was never moved onto the reviewed
   digest and was running 16.14 with pgvector 0.8.6. Refresh is deliberately
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
fixed between 16.15 and 16.16 until this refresh.

`antelope` in this table is the tenant of that name as this re-verification
recorded it on `colgatev`; the 2026-11-13 audit of that host listed only
`dash` and `mcpvalidation`. `ajhermes-z` has its own tenant named `antelope`
(the 2026-11-17 table above). The two rows describe different hosts, and the
`ajhermes-z` rows are not re-verified here: that host was not reachable from
`colgatev`. The 16.14 record of `hermes` there remains unverified and is a
per-host follow-up (WI#780), not an assumed closure. That follow-up is
recorded in [the WI#780 section](#re-verification-2026-11-19-wi780).

The pgvector extension question is answered with evidence in
[the audit](POSTGRESQL-2026-11-12-CVE-AUDIT.md#re-verification-2026-11-19-wi777):
a 0.8.6 catalog survives the image swap on the new library, and the shipped
`ALTER EXTENSION vector UPDATE` path changes catalog comments only.

## Re-verification 2026-11-19 (WI#780)

Host `ajhermes-z` (Arch Linux, systemd `262.2-1-arch`, kernel `6.18.54-2-lts`,
rootless Podman `6.2.0`, systemd user units). Tenant list from
`/etc/lunarwing/ports.json`: `hermes`, `antelope`, `barracuda`. Each was
inspected with `pg-image-status`, `psql`, and the rendered Quadlet; the
before/after rows below come from `pg-image-status` and
`SHOW server_version_num` plus `pg_extension`.

| Tenant | Before | After | Disposition |
|---|---|---|---|
| `hermes` | 16.14 / `160014`, floating `pgvector/pgvector:pg16`, image `8e5355e9ff39…`, pgvector 0.8.6 | 16.16 / `160016`, pinned `…@sha256:484e8380…`, image `b0108689bf51…`, pgvector installed 0.8.6 / available 0.8.7 | `refresh-pg-image hermes` on 2026-11-19; daemon and PostgreSQL were active before and after, and the named volume `lunarwing-pg-hermes` was retained |
| `antelope` | 16.16 / `160016`, pinned digest, image `b0108689bf51…`, pgvector 0.8.7 | unchanged | already at the reviewed pin; no refresh needed. Registry `created_at` is `2026-11-19T03:15:13Z`, so this row is newer than the 2026-11-17 snapshot above |
| `barracuda` | 16.16 / `160016`, pinned digest, image `b0108689bf51…`, pgvector 0.8.7 | unchanged | not present at the 2026-11-17 survey (registry `created_at` `2026-11-19T15:15:56Z`); already on the reviewed pin, no refresh needed |

All three Quadlet units now name the reviewed digest
(`docker.io/pgvector/pgvector:pg16@sha256:484e8380…`); no tenant on this host
keeps a floating `pg16` reference. The `hermes` data directory survived the
swap on the same volume: `public` still had 35 tables and `memory_chunks` 9
rows, the container mounts `lunarwing-pg-hermes` at
`/var/lib/postgresql/data`, and the daemon's `/health` returned `200`.

`refresh-pg-image` does not run `ALTER EXTENSION vector UPDATE`, and it was
not run here: `hermes` keeps a 0.8.6 catalog while the replacement image
packages 0.8.7, which [the audit](POSTGRESQL-2026-11-12-CVE-AUDIT.md#re-verification-2026-11-19-wi777)
showed loads the new `vector.so` and needs the SQL step only for catalog
comments. That step stays operator-run, after backup, per tenant.

### Other hosts

No other host was verified in this run. From `ajhermes-z` there is no fleet
host registry: `/etc/lunarwing/ports.json` describes only this host,
`lunarwing-mt-admin.sh` has no cross-host enumeration, and no remote mt-admin
access (SSH key or `LUNARWING_MT_ADMIN` target) is configured. `colgatev`
resolves over the NetBird mesh but refuses the available SSH identity, and its
tenants were already recorded in the WI#777 section above. Any host not named
in this document therefore stays **uninspected** — not patched, and not
covered by this host's result.

The open question on this item about the `colgatev` row — whether that host
really holds a tenant `antelope`, since the 2026-11-13 audit listed only
`dash` and `mcpvalidation` — cannot be settled from `ajhermes-z`. The
`antelope` rows here and in the WI#777 section are separate tenants on
separate hosts; if the `colgatev` row is wrong, that host's registry read must
be redone by an operator with access there, not inferred from this host.

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
