# DarkIRC Multitenant Operations Runbook

Guide for deploying, migrating, operating, and troubleshooting DarkIRC in a production multi-tenant LunarWing environment.

DarkIRC support in multi-tenant mode uses a per-tenant stack:

```text
LunarWing WASM channel
  -> darkirc adapter HTTP API on 127.0.0.1:<darkirc_adapter>
  -> tenant darkirc daemon IRC port on 127.0.0.1:<darkirc_irc>
  -> darkirc P2P network / seeds / optional Tor hidden service
```

Each tenant gets its own daemon, adapter, ports, config, datastore, logs, and adapter secret. The daemon is not shared across tenants because DarkIRC DM encryption depends on per-identity ChaCha key material.

For adapter architecture and QA scenarios, see [DarkIRC Multitenant Adapter](../guides/darkirc_channel_for_lunarwing/DARKIRC_MT_ADAPTER.md). For general multi-tenant operations, see [Production Multi-Tenant Deployment](MULTITENANCY-PRODUCTION.md).

## Prerequisites

- Production multi-tenancy is already managed with `ic/scripts/lunarwing-mt-admin.sh`.
- **DarkIRC must be explicitly enabled** when creating tenants by passing `--enable-darkirc` to `add-tenant` or `add-tenants`. Without this flag, no DarkIRC services, env files, or service units are created for the tenant. The flag state is persisted per-tenant in `/etc/lunarwing/ports.json` as `enable_darkirc: true/false`; existing tenants without the field default to disabled.
- `jq` is installed for `/etc/lunarwing/ports.json` migrations and inspection.
- Python 3 with `aiohttp` is available for `darkirc_adapter.py`.
- `git`, `cargo`, and `make` are installed. Source checkouts remain root-owned, non-group/world-writable, symlink-free, clean, and pinned. Build recipes run as dedicated unprivileged `lunarwing-build` from disposable copies.

You do **not** need to clone DarkFi by hand: `build-darkirc` auto-clones its pinned source into `/var/cache/lunarwing/darkfi-source` by default. Tenant-owned checkouts are not trusted as shared-binary build inputs.

## Port Registry Requirements

DarkIRC requires current schema v14 in `/etc/lunarwing/ports.json`.

| Schema | Adds | Script |
|--------|------|--------|
| v6 | `extended_range`, `extended_base`, `extended_ports.reserved_0..reserved_9` | `ic/scripts/migrate-ports-v6.sh` |
| v6.1 | `extended_ports.darkirc_adapter` from `reserved_0` | `ic/scripts/migrate-ports-v6.1.sh` |
| v7 | `extended_ports.darkirc_irc` and `extended_ports.darkirc_rpc` from `reserved_1` and `reserved_2` | `ic/scripts/migrate-ports-v7.sh` |
| v14 | Stable random `darkirc_scope_id` for each enabled tenant | inline `ports_migrate_v14` |

New tenants created after these changes already receive all three DarkIRC ports. Existing tenants need the migrations.

### Inspect Current Schema

```bash
jq -r '.version // 0' /etc/lunarwing/ports.json
```

Check one tenant:

```bash
jq '.tenants["<TENANT>"].extended_ports' /etc/lunarwing/ports.json
```

Expected DarkIRC fields:

```json
{
  "darkirc_adapter": 20010,
  "darkirc_irc": 20011,
  "darkirc_rpc": 20012
}
```

The exact numbers depend on the tenant's base port. Existing primary ports are not moved.

### Migrate a Live Registry

Run migrations in order from the repo root:

```bash
sudo ic/scripts/migrate-ports-v6.sh
sudo ic/scripts/migrate-ports-v6.1.sh
sudo ic/scripts/migrate-ports-v7.sh
```

Each script:

- accepts `PORTS_REGISTRY=/path/to/copy.json` for dry runs on a copy;
- requires root only when operating on `/etc/lunarwing/ports.json`;
- writes a timestamped backup next to the registry;
- validates port uniqueness before replacing the registry;
- swaps the new registry into place atomically.

Dry-run against a copy first:

```bash
cp -p /etc/lunarwing/ports.json /tmp/ports.darkirc-test.json
PORTS_REGISTRY=/tmp/ports.darkirc-test.json ic/scripts/migrate-ports-v6.sh
PORTS_REGISTRY=/tmp/ports.darkirc-test.json ic/scripts/migrate-ports-v6.1.sh
PORTS_REGISTRY=/tmp/ports.darkirc-test.json ic/scripts/migrate-ports-v7.sh
jq '.tenants | to_entries[] | {tenant: .key, extended_ports: .value.extended_ports}' /tmp/ports.darkirc-test.json
```

### Inline Migration via Admin Script

`lunarwing-mt-admin.sh` also contains inline migration functions:

- `ports_migrate_v6`
- `ports_migrate_v6_1`
- `ports_migrate_v7`
- `ports_migrate_v14`

The top-level `ports_migrate()` dispatcher runs them in order. This means normal admin-script flows can bring stale registries forward, while the standalone scripts remain available for explicit operator-controlled upgrades.

## Configure the DarkIRC Source Path (optional)

`build-darkirc` resolves the DarkFi source path in this order:

1. host environment value:
   ```bash
   export LUNARWING_MT_DARKIRC_SOURCE=/path/to/darkfi
   ```
2. default: `/var/cache/lunarwing/darkfi-source` (root-owned).

If resolved path does not exist, `build-darkirc` clones it as root and checks out exact approved revision. Existing checkout must already be clean, root-owned, and at that revision.

Clone URL is configurable. Revision override is accepted only when it equals
compiled approved revision (default shown); arbitrary revisions fail closed:

```bash
export LUNARWING_MT_DARKIRC_REPO=https://github.com/darkrenaissance/darkfi
export LUNARWING_MT_DARKIRC_REV=a05956d412a091e8b54c1cd4f4264c33b941203d
```

To use custom trusted checkout:

```bash
sudo chown -R root:root /path/to/darkfi
sudo chmod go-w /path/to/darkfi
sudo LUNARWING_MT_DARKIRC_SOURCE=/path/to/darkfi \
  ic/scripts/lunarwing-mt-admin.sh build-darkirc --tenant <TENANT>
```

## Provision or Backfill a Tenant

Build and attest shared DarkIRC binary/helper before first DarkIRC-enabled provision. Then `add-tenant` allocates DarkIRC ports and renders env/config files. DarkIRC services remain explicit opt-in:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant <TENANT> --docker-group --enable-darkirc
```

Tenants created without `--enable-darkirc` will not have DarkIRC services, env files, or service units. The `start-tenant`, `status`, and `patch-env` commands automatically respect the persisted flag — they skip DarkIRC operations for tenants where it is disabled.

For an existing tenant after migration, patch the env/config files:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh patch-env <TENANT>
```

This backfills DarkIRC-related values, writes `darkirc-adapter.env`, and sends generated baseline TOML through tenant-scoped typed helper. Helper preserves existing `[contact]` tables, validates current and generated TOML, locks all writers, and commits config plus metadata ledger through fsynced journaled replacements. Missing/unsafe helper, missing attestation, malformed active TOML, or binary digest mismatch fails closed.

## Build DarkIRC

Build and install the DarkIRC daemon for one tenant:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh build-darkirc --tenant <TENANT>
```

`build-darkirc`:

- resolves root-owned DarkFi source path and clones pinned source if absent;
- rejects dirty, symlinked, non-root-owned, writable, or wrong-revision source before building;
- copies approved source into a disposable build tree and runs `make darkirc` as unprivileged `lunarwing-build`;
- requires exact approved source revision rather than attesting an arbitrary existing checkout;
- builds helper as `lunarwing-build` from disposable archive of trusted admin checkout, then installs root-owned `/usr/local/libexec/lunarwing-darkirc-key-helper` before shared daemon publication;
- installs resulting binary to shared `DARKIRC_BIN` (default `/usr/local/bin/darkirc`);
- runs generator compatibility self-test inside helper without exposing raw generator output;
- writes root-owned mode-0600 `/etc/lunarwing/darkirc-compat.json`, binding approved profile, source revision, key format, and installed binary digest;
- restores previous shared binary and attestation if helper build, self-test, or attestation publication fails.

Without `--tenant`, build uses invoking sudo user only as generator self-test identity. Build recipes still run as dedicated unprivileged build user:

```bash
sudo LUNARWING_MT_DARKIRC_SOURCE=/path/to/darkfi \
  ic/scripts/lunarwing-mt-admin.sh build-darkirc
```

The build runs from an external DarkFi checkout and does not copy DarkFi source into the LunarWing repo. (Note: DarkFi's `make darkirc` also compiles `zkas` + proof circuits, so the first build can take a while.)

## Install WASM Channel Artifacts

Build tenant binaries and WASM artifacts, then install the channel into the tenant state directory:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh build-tenant <TENANT> --with-wasm
sudo ic/scripts/lunarwing-mt-admin.sh install-wasm <TENANT>
```

The DarkIRC channel capabilities file declares env-backed adapter settings. The tenant's `lunarwing.env` supplies:

```bash
DARKIRC_ADAPTER_URL=http://127.0.0.1:<darkirc_adapter>
DARKIRC_ADAPTER_SECRET=<generated-secret>
```

Do not paste the secret into tickets, logs, or chat. Treat it like any other tenant credential.

## Generated Files

For tenant `<TENANT>`, the admin script manages:

| File | Purpose |
|------|---------|
| `/home/<TENANT>/lunarwing/env/lunarwing.env` | Main daemon env, including `DARKIRC_ADAPTER_URL`, `DARKIRC_ADAPTER_SECRET`, and optional `DARKIRC_SOURCE` |
| `/home/<TENANT>/lunarwing/env/darkirc-adapter.env` | Adapter env rendered from `ic/scripts/templates/darkirc-adapter.env.template` |
| `/home/<TENANT>/lunarwing/state/darkirc/darkirc_config.toml` | Daemon config rendered from `ic/scripts/templates/darkirc_config.toml.template` |
| `/home/<TENANT>/lunarwing/state/darkirc/key-exchange/` | Tenant-owned mode-0700 transaction, ledger, and pending-state root |
| `/home/<TENANT>/lunarwing/state/channels/darkirc.capabilities.json` | WASM channel capabilities installed by `install-wasm` |
| `/home/<TENANT>/lunarwing/logs/` | Tenant daemon, adapter, and service logs |

The template placeholders use `__UPPERCASE__` form, not shell `${VAR}` form. Secrets are generated at runtime and are not stored in templates.

Generic machine migration now handles managed DarkIRC through structured
`darkirc-contacts-v1` manifest. Export holds tenant lifecycle and writer locks
through bundle seal,
excludes host-specific DarkIRC runtime/config state, and carries stable
`darkirc_scope_id`. Import validates manifest through installed typed helper
before provisioning, passes scope explicitly to `add-tenant`, then stages and
imports manifest through typed helper while holding same tenant lifecycle lock.
Manifest env merges run as tenant through same-directory atomic replacements;
root does not open tenant env destinations for writing. Never copy `key-exchange/`
directly or delete it to bypass migration checks.

Manual structured commands remain available for diagnostics:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact export-migration <TENANT>
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact stage-migration <TENANT> < darkirc-contacts-v1.json
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact import-migration <TENANT>
```

## Service Lifecycle

`start-tenant`, `stop-tenant`, and `restart-tenant` hold per-tenant lifecycle lock
across full operation and include DarkIRC services. `status` remains read-only.

```bash
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant <TENANT>
sudo ic/scripts/lunarwing-mt-admin.sh status <TENANT>
sudo ic/scripts/lunarwing-mt-admin.sh restart-tenant <TENANT>
sudo ic/scripts/lunarwing-mt-admin.sh stop-tenant <TENANT>
```

### Automatic Adapter-Secret Seeding

When `start-tenant` runs for a DarkIRC-enabled tenant, it reads `DARKIRC_ADAPTER_SECRET` from the tenant's `lunarwing.env` and submits it through the authenticated loopback gateway setup API into the encrypted SecretsStore. The secret is never written to plaintext database files and is never passed as a command-line argument. This keeps the adapter credential available to the WASM channel at runtime without exposing it in process arguments or logs.

- **Best-effort:** failures during seeding emit a warning but do **not** abort tenant startup. The main daemon and adapter may still come up; only the credential path to the WASM channel is affected.
- **Idempotent:** re-running `start-tenant` re-reads `DARKIRC_ADAPTER_SECRET` and refreshes the active WASM credentials. This is the supported way to rotate the secret post-start without manual database edits.

### systemd

Per-tenant user units:

```text
lunarwing-darkirc-<TENANT>.service
lunarwing-darkirc-adapter-<TENANT>.service
```

Check status and logs:

```bash
sudo -u <TENANT> XDG_RUNTIME_DIR=/run/user/$(id -u <TENANT>) \
  systemctl --user status lunarwing-darkirc-<TENANT>.service

sudo -u <TENANT> XDG_RUNTIME_DIR=/run/user/$(id -u <TENANT>) \
  systemctl --user status lunarwing-darkirc-adapter-<TENANT>.service

sudo -u <TENANT> XDG_RUNTIME_DIR=/run/user/$(id -u <TENANT>) \
  journalctl --user -u lunarwing-darkirc-<TENANT>.service --no-pager -n 80

sudo -u <TENANT> XDG_RUNTIME_DIR=/run/user/$(id -u <TENANT>) \
  journalctl --user -u lunarwing-darkirc-adapter-<TENANT>.service --no-pager -n 80
```

### OpenRC

Per-tenant system services:

```text
lunarwing-darkirc-<TENANT>
lunarwing-darkirc-adapter-<TENANT>
```

Check status and logs:

```bash
sudo rc-service lunarwing-darkirc-<TENANT> status
sudo rc-service lunarwing-darkirc-adapter-<TENANT> status

tail -80 /home/<TENANT>/lunarwing/logs/darkirc.log
tail -80 /home/<TENANT>/lunarwing/logs/darkirc-adapter.log
```

## Verify End-to-End

### 1. Check Ports

```bash
sudo ic/scripts/lunarwing-mt-admin.sh list-tenants
jq '.tenants["<TENANT>"].extended_ports | {darkirc_adapter, darkirc_irc, darkirc_rpc}' /etc/lunarwing/ports.json
```

### 2. Check Config Render

```bash
sudo test -s /home/<TENANT>/lunarwing/state/darkirc/darkirc_config.toml
sudo test -s /home/<TENANT>/lunarwing/env/darkirc-adapter.env
```

Confirm the rendered config uses tenant ports:

```bash
sudo grep -E '^(irc_listen|rpc_listen|listen)' /home/<TENANT>/lunarwing/state/darkirc/darkirc_config.toml
sudo grep -E '^(DARKIRC_PORT|ADAPTER_PORT)=' /home/<TENANT>/lunarwing/env/darkirc-adapter.env
```

Do not print `ADAPTER_SECRET`.

### 3. Check Adapter Health

Use mt-admin's authenticated strict diagnostic. It verifies both init services,
reads bearer under tenant identity, keeps bearer out of process argv, and requires
adapter response `status=ok` plus `irc_connected=true`:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-health <TENANT> --strict --json
```

This command is diagnostic only. It does not activate contacts, restart services,
or satisfy deferred Stage B activation/rollback requirements. Expected shape:

```json
{
  "status": "ok",
  "tenant": "<TENANT>",
  "irc_connected": true
}
```

### 4. Check Gateway Channel Health

Get the gateway port and token:

```bash
gateway_port=$(jq -r '.tenants["<TENANT>"].ports.gateway' /etc/lunarwing/ports.json)
token=$(sudo grep -s '^GATEWAY_AUTH_TOKEN=' /home/<TENANT>/lunarwing/env/lunarwing.env | cut -d= -f2-)
```

Query status:

```bash
curl -sf -H "Authorization: Bearer ${token}" \
  "http://127.0.0.1:${gateway_port}/api/gateway/status" \
  | jq '.channel_health.darkirc'
```

Do not print the token in shared logs.

## DarkIRC Channels, Seeds, and Contacts

The generated `darkirc_config.toml` ships baseline channels and seeds. The clearnet seeds use port **9600** (`tcp+tls://lilith0.dark.fi:9600`, `lilith1.dark.fi:9600`); the Tor seeds are onion addresses on port 25552.

Reachability note: a clearnet seed only bootstraps peer discovery — the seed hands out peer addresses and then closes the channel ("Channel stopped" in the daemon log is normal seed behaviour). Most public DarkIRC nodes are **Tor-only**, so a tenant with no Tor daemon can connect to a clearnet seed but may stay at "Waiting for some P2P connections…". For real peering, run Tor on the host (the config's `tor_socks5_proxy` already points at `127.0.0.1:9050`). See [Tor / Hidden Service Options](#tor--hidden-service-options).

Edit the config as the tenant when needed:

```bash
sudo -u <TENANT> editor /home/<TENANT>/lunarwing/state/darkirc/darkirc_config.toml
```

Restart DarkIRC services after edits:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh restart-tenant <TENANT>
```

### Contacts and DM Keypairs

DarkIRC contact key material belongs inline under `[contact."nickname"]` sections in the TOML. The daemon does not consume a separate generated keypair file.

A contact section should include the contact's public key and the tenant's own keypair for that contact. Exchange public keys out of band before enabling private DMs.

### Managed Staged Exchange

Current managed surface stops at deferred installation. It does not restart
DarkIRC, claim local health, or claim peer round-trip verification.
`complete` writes `Installed` contact into on-disk active TOML; any later daemon
restart can load it even though Stage B health gate is absent. Treat this as
maintenance-only until Stage B lands. Installed pending state intentionally
blocks migration and managed upgrade; do not delete it to bypass gate.

```bash
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact prepare <TENANT> <CONTACT> --out offer.json --json
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact respond <TENANT> <CONTACT> \
  --in offer.json --out response.json \
  --expect-peer-fingerprint sha256:<64-lowercase-hex> --json
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact complete <TENANT> <CONTACT> \
  --exchange-id <32-lowercase-hex> --in response.json \
  --expect-peer-fingerprint sha256:<64-lowercase-hex> --json
```

Only public artifacts enter `--in`, `--out`, or stdout. Output paths are
relative to tenant home, must not already exist, and use mode 0600. Private keys
stay inside generator pipe, tenant-owned pending state, and installed TOML.
Generic `--yes` never substitutes for full expected fingerprint.

Inspection and recovery:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact list <TENANT> --json
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact exchanges <TENANT> --json
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact doctor <TENANT> --json
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact recover <TENANT> --json
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact cancel <TENANT> \
  --exchange-id <32-lowercase-hex> --json
```

`list`, `status`, `doctor`, `exchanges`, `writers-active`, and strict health
inspection validate registry read-only; they do not create or migrate registry
state. Mutation commands retain registry migration and lifecycle locking.

## Tor / Hidden Service Options

DarkIRC can be paired with Tor in multiple ways. Choose one deliberately; the admin script does not currently provision Tor hidden services automatically.

| Option | When to use | Notes |
|--------|-------------|-------|
| SOCKS5 outbound-only | Tenant only needs to reach onion peers | Configure DarkIRC to use a local Tor SOCKS listener if supported by the selected DarkFi build/config. |
| Static `torrc` hidden service | Stable inbound onion identity per tenant | Create a per-tenant hidden service directory, bind it to the tenant's DarkIRC P2P port, and protect the private key directory. |
| Arti ephemeral onion | Experimental or temporary identities | Useful for testing; not a replacement for a stable production onion identity. |

Operational rules:

- keep Tor hidden-service private keys out of the repo;
- store tenant-specific Tor state under the tenant's state directory or another root-owned protected path;
- document onion addresses per tenant in an operator-only inventory;
- restart only the affected tenant after changing Tor routing.

## Troubleshooting

### `build-darkirc` fails

Source defaults to root-owned `/var/cache/lunarwing/darkfi-source`. `--tenant`
selects unprivileged generator self-test identity; it does not make tenant
checkout authoritative for shared binary.

- **No root Rust toolchain.** Shared daemon/helper build requires root toolchain:
  ```bash
  sudo sh -lc 'cargo --version && make --version'
  ```
- **Checkout trust rejection.** LunarWing and DarkFi checkouts must be root-owned, clean, symlink-free, and not writable by group/others. Commit local changes before production build.
- **Custom source path invalid / clone failed.** If `DARKIRC_SOURCE` / `LUNARWING_MT_DARKIRC_SOURCE` points at a non-DarkFi tree (no `Makefile`), fix or unset it to fall back to auto-clone. Override the clone target with `LUNARWING_MT_DARKIRC_REPO` / `LUNARWING_MT_DARKIRC_REV`.

### DarkIRC daemon fails to start with `permission denied` on its config

Daemon runs as tenant and must own state directory. Helper refuses unsafe ownership instead of recursively changing it. Inspect links first, then repair only verified tree:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh patch-env <TENANT>
sudo chown -R --no-dereference <TENANT>:<TENANT> /home/<TENANT>/lunarwing/state/darkirc
```

### Adapter `/health` returns `{"error": "unauthorized"}`

`/health` requires the adapter bearer secret — pass `Authorization: Bearer <ADAPTER_SECRET>` (see [Check Adapter Health](#3-check-adapter-health)).

### `darkirc_irc` or `darkirc_rpc` is missing

The registry is stale or migration was incomplete.

```bash
jq -r '.version // 0' /etc/lunarwing/ports.json
jq '.tenants["<TENANT>"].extended_ports' /etc/lunarwing/ports.json
```

Run:

```bash
sudo ic/scripts/migrate-ports-v6.sh
sudo ic/scripts/migrate-ports-v6.1.sh
sudo ic/scripts/migrate-ports-v7.sh
```

Then re-run:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh patch-env <TENANT>
```

### Adapter health returns `irc_connected: false`

Check that the tenant daemon is running and that `DARKIRC_PORT` in `darkirc-adapter.env` matches the tenant's `darkirc_irc` port.

```bash
jq -r '.tenants["<TENANT>"].extended_ports.darkirc_irc' /etc/lunarwing/ports.json
sudo grep -s '^DARKIRC_PORT=' /home/<TENANT>/lunarwing/env/darkirc-adapter.env
```

Then check daemon logs before restarting.

### WASM channel cannot reach adapter

Verify `DARKIRC_ADAPTER_URL` points at the tenant's adapter port:

```bash
jq -r '.tenants["<TENANT>"].extended_ports.darkirc_adapter' /etc/lunarwing/ports.json
sudo grep -s '^DARKIRC_ADAPTER_URL=' /home/<TENANT>/lunarwing/env/lunarwing.env
```

Confirm the capabilities file is installed and contains the DarkIRC channel metadata:

```bash
sudo test -s /home/<TENANT>/lunarwing/state/channels/darkirc.capabilities.json
```

Run `install-wasm` again if artifacts are missing.

### Secret mismatch between WASM channel and adapter

Regenerate or patch env files with the admin script rather than editing one side by hand:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh patch-env <TENANT>
sudo ic/scripts/lunarwing-mt-admin.sh restart-tenant <TENANT>
```

**Safe rotation:** when the WASM channel and adapter must share a new secret, keep `DARKIRC_ADAPTER_SECRET` (in `lunarwing.env`) and the adapter's `ADAPTER_SECRET` (in `darkirc-adapter.env`) synchronized through the `patch-env` / rendered env flow, then run `restart-tenant`. The rendered templates ensure both sides receive the same value without manual copying. Never print or log secret values during this process; verify by length or fingerprint if needed.

If manual repair is unavoidable, ensure the adapter service receives `ADAPTER_SECRET` and the main daemon receives `DARKIRC_ADAPTER_SECRET` with the same value. Do not print the value while comparing; use fingerprints or length checks.

> **Manual regression test:** there is no shell-test aggregator that runs automatically in CI. To validate the secret-seeding *contract* after code changes, run the dedicated test script manually:
>
> ```bash
> bash ic/scripts/tests/test-darkirc-adapter-secret-upload.sh
> ```
>
> This script sources mt-admin and stubs `curl`, port discovery, env paths, and the gateway wait, then asserts the seeding code path: guards and payload shape, selection of the gateway port, idempotent repeated invocation, and that the credential stays off `argv` and out of logs. It is a unit-level check of the seeding logic, not a live integration run. Proving real gateway, encrypted SecretsStore, and runtime behaviour requires a live `start-tenant` followed by post-refresh log checks (no 401s) and adapter-health verification.

### Port collision warning during migration

The standalone migration scripts leave the live registry untouched and print the candidate temp file path. Inspect the candidate and current registry, then resolve the stale or manually edited port entry before re-running the migration.

```bash
jq '[.tenants[] | ((.ports // {}) | to_entries[] | .value), ((.extended_ports // {}) | to_entries[] | .value)] | group_by(.) | map(select(length > 1))' /etc/lunarwing/ports.json
```

### OpenRC service starts but exits immediately

Check conf.d/init script paths and tenant ownership:

```bash
sudo rc-service lunarwing-darkirc-<TENANT> status
sudo rc-service lunarwing-darkirc-adapter-<TENANT> status
ls -l /etc/init.d/lunarwing-darkirc-<TENANT> /etc/init.d/lunarwing-darkirc-adapter-<TENANT>
ls -ld /home/<TENANT>/lunarwing /home/<TENANT>/lunarwing/env /home/<TENANT>/lunarwing/state
```

Fix ownership only for files that should belong to the tenant. Keep system init scripts root-owned.

## Rollback

### Port Registry Rollback

Each migration prints a backup path. To roll back immediately:

```bash
sudo cp /etc/lunarwing/ports.json.bak.<TIMESTAMP> /etc/lunarwing/ports.json
```

Restart affected tenants after rollback if services were already rendered or started with migrated ports.

### Disable DarkIRC for One Tenant

Stop the DarkIRC services while leaving the main tenant stack intact.

Systemd:

```bash
sudo -u <TENANT> XDG_RUNTIME_DIR=/run/user/$(id -u <TENANT>) \
  systemctl --user stop lunarwing-darkirc-adapter-<TENANT>.service lunarwing-darkirc-<TENANT>.service
```

OpenRC:

```bash
sudo rc-service lunarwing-darkirc-adapter-<TENANT> stop
sudo rc-service lunarwing-darkirc-<TENANT> stop
```

If the WASM channel is installed but the adapter is down, channel health will report degraded/unhealthy until either the service is restored or the channel is removed from the tenant's installed channels.

## Operator Checklist

For each production tenant:

- [ ] `/etc/lunarwing/ports.json` is schema v14 and tenant has unique `darkirc_scope_id`.
- [ ] `darkirc_adapter`, `darkirc_irc`, and `darkirc_rpc` exist under `.extended_ports`.
- [ ] Root has working `cargo` + `make`; trusted LunarWing/DarkFi checkouts pass ownership and clean-tree checks.
- [ ] `build-darkirc --tenant <TENANT>` completed; tenant used only for unprivileged generator self-test.
- [ ] `build-tenant <TENANT> --with-wasm` completed.
- [ ] `install-wasm <TENANT>` completed.
- [ ] `darkirc_config.toml` and `darkirc-adapter.env` exist and use tenant ports.
- [ ] `DARKIRC_ADAPTER_URL` exists in `lunarwing.env`.
- [ ] Adapter secret is present on both daemon env and adapter env sides without being exposed in logs.
- [ ] `start-tenant <TENANT>` starts daemon, adapter, and main LunarWing service.
- [ ] Adapter `/health` returns `status: ok` and `irc_connected: true`.
- [ ] Gateway status reports DarkIRC channel health.
- [ ] Automatic secret seeding succeeded for the tenant (no post-start 401 from WASM channel to adapter; rerun `start-tenant` to refresh if needed).
- [ ] Tor/hidden-service plan is documented if onion connectivity is required.
