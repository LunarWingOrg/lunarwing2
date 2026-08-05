# Migrate an IronClaw PostgreSQL Instance to Multi-Tenant LunarWing

This runbook moves one PostgreSQL-backed IronClaw instance into a tenant managed by `ic/scripts/lunarwing-mt-admin.sh`. Rehearse with a dump and copied state before the final cutover.

For a single-instance target, see [`MIGRATE_IRONCLAW_TO_LUNARWING.md`](MIGRATE_IRONCLAW_TO_LUNARWING.md). For a libSQL source, see [`MIGRATE_IRONCLAW_LIBSQL_TO_MT.md`](MIGRATE_IRONCLAW_LIBSQL_TO_MT.md).

## Safety Rules

- Keep the source service and database intact until the target passes verification.
- Use a disposable target tenant for rehearsal.
- Do not copy source `.env`, generated MT environment files, service units, sockets, PIDs, or old WASM binaries.
- Treat database dumps and state copies as secret-bearing; create them with restrictive permissions.
- Use MT admin for tenant lifecycle, status, build, backup, and restore on both systemd and OpenRC hosts.
- Do not use bare system-bus `systemctl` or hand-written `rc-service` probes for tenant units.

## 1. Inventory the Source

Record:

- IronClaw version and commit;
- PostgreSQL host, port, database, and server version;
- base directory and file ownership;
- source secrets key mechanism;
- XMPP identity and OMEMO state;
- enabled channels, tools, routines, workers, tunnels, and webhooks;
- model/provider identifiers and custom workspace templates.

Do not print connection strings, tokens, passwords, or the secrets master key into the migration record.

## 2. Prepare the MT Host

Follow [`MT-ADMIN-QUICKSTART.md`](MT-ADMIN-QUICKSTART.md) and the operating-system package guide. Confirm MT admin's prerequisites and host health before adding a tenant.

Create the target with the desired identity and feature flags. For example:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant <tenant> \
  --docker-group \
  --xmpp-jid '<existing-or-new-jid>'
```

MT admin allocates the primary and extended port registry entries, creates the tenant user and rootless PostgreSQL service, and renders the correct systemd-user or OpenRC services. Do not copy the old guide's abbreviated port table; query the current registry or run:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh list-tenants
sudo ic/scripts/lunarwing-mt-admin.sh status <tenant>
```

Keep the LunarWing daemon stopped until the source database has been restored.

## 3. Create a Rehearsal Dump

A custom-format archive works with MT admin's guarded restore path:

```bash
umask 077
pg_dump --format=custom --no-owner --no-acl \
  --file=ironclaw-rehearsal.dump \
  <source_database>
pg_restore --list ironclaw-rehearsal.dump >/dev/null
```

Use explicit `--host`, `--port`, and `--username` values as required by the source, without embedding a password in the command line.

`pg_dump` takes a consistent MVCC snapshot, so rehearsal can normally happen while IronClaw is running. The final cutover still needs a fresh dump after writes are quiesced.

## 4. Restore Through MT Admin

The restore command is intentionally destructive to the **target tenant database**: it verifies the archive format, requires `--yes`, refuses a running daemon, and uses a single transaction.

```bash
sudo ic/scripts/lunarwing-mt-admin.sh stop-tenant <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh restore-tenant \
  <tenant> /absolute/path/ironclaw-rehearsal.dump --yes
```

Do not pipe a plain SQL dump into the container or assume a fixed PostgreSQL password. Current tenants use managed credentials, and direct restore commands bypass MT admin's stop check and all-or-nothing restore behavior.

## 5. Copy Only Required State

The MT tenant state directory is `/home/<tenant>/lunarwing/state/`. Review the source tree and copy only data that is not already in PostgreSQL:

- `workspace-template/` when custom identity/bootstrap templates must be preserved;
- project or working repositories that are intentionally part of the deployment;
- XMPP/OMEMO state only when retaining the same XMPP identity and after confirming the bridge's current layout;
- other explicitly documented integration state.

Example for one reviewed directory:

```bash
sudo cp -a <source-base>/workspace-template \
  /home/<tenant>/lunarwing/state/
sudo chown -R <tenant>:<tenant> \
  /home/<tenant>/lunarwing/state/workspace-template
```

Do not copy source `.env`, `config.toml`, `ironclaw.db`, PID/socket files, CLI history, generated service files, `tools/`, or `channels/` over MT-managed paths. Merge needed non-secret settings into the generated configuration and rebuild extensions from the current registry.

Workspace memory documents, conversations, routines, and jobs are database data; the old statement that `projects/` contains all memory documents is not accurate for the current workspace system.

## 6. Handle Encrypted Secrets

The restored `secrets` rows can only be decrypted with the source master key. MT admin generates a new protected `SECRETS_MASTER_KEY` for a new tenant, so a raw database restore does not automatically make old ciphertext usable.

Choose one reviewed path:

1. securely provision the exact source master key into the target's protected secret configuration before first use; or
2. leave the generated target key in place and re-enter every required secret through the onboard/web/extension setup flows.

Do not paste the key into this runbook, command history, logs, or a workspace file. Verify secret availability by name and by a real integration test, never by printing values.

## 7. Merge Current Configuration

MT admin owns `/home/<tenant>/lunarwing/env/` and generated service configuration. Compare source settings without displaying protected environment values, then deliberately carry over only current equivalents.

Common changes include:

- tenant-specific PostgreSQL, gateway, proxy, bridge, orchestrator, worker, WeeChat, and vision ports;
- `tensorzero::function_name::lunarwing` as the current default TensorZero function;
- per-tenant XMPP identity and bridge token;
- registry-built WASM extensions and capability sidecars;
- current sandbox, approval, and outbound-network policy;
- ports-registry discovery for OCR health instead of hardcoded `127.0.0.1:8088`.

## 8. Build, Start, and Migrate

```bash
sudo ic/scripts/lunarwing-mt-admin.sh build-tenant <tenant> --with-wasm
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh status <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh doctor <tenant>
```

On first start, LunarWing applies every pending migration in the current `ic/migrations/` chain. Do not check only for the historical V18 routine migration; compare the highest applied version with the migration files in the deployed commit and inspect logs for failures.

## 9. Verify the Rehearsal

- Authenticate to the tenant gateway without recording its bearer token.
- Verify users, identities, conversations, settings, memory search, routines, jobs, and extension state.
- Exercise XMPP and every enabled channel with test traffic.
- Verify worker creation, completion, cancellation, and disconnect handling.
- Verify Gotify/webhooks/tunnels with isolated credentials.
- Verify port-registry-aware host health, including `vision_service` and `vision_health` where enabled.
- Run the relevant checks in [`TESTING_GUIDE.md`](TESTING_GUIDE.md).

Keep external delivery disabled or isolated during rehearsal so the source and target cannot both act on production messages or schedules.

## 10. Final Cutover

1. Stop source message ingress, routines, workers, and IronClaw.
2. Take and validate a fresh final custom-format dump.
3. Stop the target tenant and restore the final dump through `restore-tenant ... --yes`.
4. Reapply only reviewed filesystem changes made since rehearsal.
5. Start the target through MT admin and repeat status, doctor, migration, and integration checks.
6. Keep the old service and database disabled but recoverable until the rollback window closes.

## Rollback

Stop the target with MT admin, disable target ingress, and restart the untouched source service against its original database and state. Never point an old IronClaw binary at the forward-migrated tenant database.

Do not purge the target tenant or delete source backups during the rollback window. Removal with `--purge` is irreversible and is not a migration cleanup shortcut.
