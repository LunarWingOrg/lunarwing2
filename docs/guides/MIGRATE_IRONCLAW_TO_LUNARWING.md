# Migrate a Single IronClaw Instance to LunarWing

This runbook covers a PostgreSQL-backed, single-instance migration. LunarWing retains the IronClaw migration lineage and applies newer Refinery migrations at startup, but a forward migration is not a rollback plan. Test with a cloned database and copied state directory before touching the original instance.

For a multi-tenant target, use [`MIGRATE_IRONCLAW_TO_MT.md`](MIGRATE_IRONCLAW_TO_MT.md) instead.

## Prerequisites

- PostgreSQL client tools compatible with the source server.
- Access to the IronClaw base directory and service configuration.
- A LunarWing release binary, or Rust 1.96.1+ when building the release locally.
- Enough storage for a database dump and a full state-directory copy.
- A documented service rollback procedure for the host's init system.

Never expose `.env`, service environment, database URLs, master keys, or provider credentials in migration logs.

## 1. Inventory and Back Up

Record the source binary version, database server version, database name, base directory, init system, enabled bridges/channels, and external integrations.

Stop IronClaw and any coupled bridge through the host's existing service manager, then create backups:

```bash
pg_dump --format=custom --file=ironclaw-pre-lunarwing.dump <source_database>
cp -a <ironclaw-base-dir> <backup-base-dir>
```

Verify that `pg_restore --list ironclaw-pre-lunarwing.dump` succeeds. Protect both backups as secret-bearing data.

For the safest rehearsal, restore the dump into a new PostgreSQL database and copy the base directory to a new location. Point LunarWing at those copies; do not let the rehearsal migrate the only source database.

## 2. Prepare LunarWing

Use a published release artifact when available. If a local deployment build is required, run a bounded release build from `ic/`:

```bash
taskset -c 0-11 cargo build --release -j12 --bin lunarwing
```

Build the XMPP bridge or WASM extensions only when the instance uses them:

```bash
taskset -c 0-11 cargo build --release -j12 -p xmpp-bridge
scripts/build-wasm-extensions.sh
```

Release builds are appropriate here because the result is being deployed. Use `cargo check`, not a debug build, for compile-only verification.

## 3. Update Bootstrap Configuration

Set the preferred base-directory variable in the protected service environment:

```bash
LUNARWING_BASE_DIR=<copied-or-existing-base-dir>
RUST_LOG=lunarwing=info,tower_http=warn
```

`IRONCLAW_BASE_DIR` remains a legacy fallback, but `LUNARWING_BASE_DIR` wins when both are set. Remove conflicting values after the migration is stable.

Review, rather than blindly copy, the existing `config.toml` and database-backed settings:

- keep provider credentials in the encrypted secrets path or protected environment;
- use current channel and sandbox settings;
- update model identifiers to providers/functions that the target proxy actually exposes;
- the current TensorZero default is `tensorzero::function_name::lunarwing`, not the old `ironclaw` function name;
- preserve custom workspace identity files and templates after checking their ownership and permissions.

## 4. Run Database Migrations on the Rehearsal Copy

Point `DATABASE_URL` at the restored rehearsal database and start LunarWing once. The current PostgreSQL migration set is the ordered chain in `ic/migrations/`; do not rely on an old guide's hardcoded terminal migration number.

Check startup logs for every migration error before continuing. Verify representative data through authenticated LunarWing interfaces:

- users and identities;
- settings and encrypted-secret metadata without displaying values;
- conversations and messages;
- workspace memory documents and chunks;
- routines, jobs, and histories;
- installed extension state.

Do not rename the database merely for cosmetic consistency. If a rename is operationally required, disconnect clients first and update every protected connection setting atomically.

## 5. Install and Start the Service

LunarWing's service command supports systemd and OpenRC:

```bash
lunarwing service install
lunarwing service start
lunarwing service status
```

Review the rendered service before enabling it. The committed systemd production template expects the binary at `/usr/local/bin/lunarwing`, state under `/var/lib/lunarwing`, and protected environment at `/etc/lunarwing/lunarwing.env`; adjust the deployment consistently rather than editing only `ExecStart`.

For OpenRC, use the committed `.openrc` and `.confd` templates or `lunarwing service install`. Do not translate a systemd-only runbook into ad hoc OpenRC commands.

If the watchdog is required, install the current init-appropriate assets after the main service is healthy:

```bash
sudo ic/scripts/install-lunarwing-watchdog.sh
```

## 6. Update Integrations

- LunarWing's Unix socket is `lunarwing.sock`. `IRONCLAW_SOCKET` remains a legacy fallback, but clients should move to the current socket setting.
- External workers must negotiate `lunarwing-agent-v1`; the `ironclaw-agent-v1` compatibility subprotocol has been removed and is no longer offered or accepted.
- Rebuild and reinstall WASM extensions through the registry/build scripts instead of copying unknown old binaries.
- Revalidate XMPP, WeeChat, DarkIRC, Gotify, webhooks, tunnels, and worker endpoints individually.
- Recheck file ownership, directory modes, bearer authentication, and outbound URL restrictions.

## 7. Cut Over

After the rehearsal passes:

1. Schedule downtime and stop IronClaw.
2. Take a fresh final database dump and state-directory copy.
3. Point LunarWing at the final target database and state directory.
4. Start LunarWing and confirm migrations, service health, web authentication, and required channels.
5. Run the relevant checks from [`TESTING_GUIDE.md`](TESTING_GUIDE.md).
6. Keep the old service disabled, not deleted, until the rollback window closes.

## Rollback

Do not start an old IronClaw binary against a database already migrated by LunarWing. Stop LunarWing, restore the pre-migration dump into a clean database (or switch back to the untouched original), restore the backed-up base directory, restore the old protected service configuration, and only then start IronClaw.

A custom-format dump generally must be restored into a clean target or with an explicitly reviewed `pg_restore` cleanup strategy. Test the exact restore command during rehearsal; do not improvise it during an incident.
