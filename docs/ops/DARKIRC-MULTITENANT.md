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
- `git`, `cargo`, `make`, `taskset`, `setpriv`, and `tar` are available on the host. `build-darkirc` uses a dedicated locked `lunarwing-build` account with no supplementary groups and a sanitized environment.

You do **not** need to clone DarkFi by hand. `build-darkirc` maintains one fixed root-owned clean checkout (default `/var/cache/lunarwing/darkfi-source`), archives the approved revision into a disposable unprivileged build tree, and never vendors DarkFi into the LunarWing repository.

## Port Registry Requirements

DarkIRC's three port fields were introduced by schema v7. The current registry
schema is **v14**: v13 dedicated `extended_ports.coord_admin`, and v14 assigns a
stable, unique, non-secret `darkirc_scope_id` to every DarkIRC-enabled tenant.
Normal `lunarwing-mt-admin.sh` mutating commands call `ports_registry_init()` and
migrate older registries through every version to v14 before operating. Do not
intentionally leave a production registry at an intermediate version.

| Historical schema | Adds | Standalone migration |
|-------------------|------|----------------------|
| v6 | `extended_range`, `extended_base`, `extended_ports.reserved_0..reserved_9` | `ic/scripts/migrate-ports-v6.sh` |
| v6.1 | `extended_ports.darkirc_adapter` from `reserved_0` | `ic/scripts/migrate-ports-v6.1.sh` |
| v7 | `extended_ports.darkirc_irc` and `extended_ports.darkirc_rpc` from `reserved_1` and `reserved_2` | `ic/scripts/migrate-ports-v7.sh` |
| v14 | Unique `darkirc_scope_id` for each enabled tenant | Integrated into `lunarwing-mt-admin.sh`; no standalone migration |

New tenants already receive all three DarkIRC ports and their stable scope
identity. Existing registries are migrated by the admin script; the standalone
v6/v6.1/v7 scripts are retained for explicit recovery or rehearsal on old
copies.

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

Use the admin script rather than stopping at the old DarkIRC-specific v7 step:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh list-tenants
jq -e '.version == 14' /etc/lunarwing/ports.json
jq -e '
  [.tenants | to_entries[]
   | select(.value.enable_darkirc == true)
   | .value.darkirc_scope_id] as $ids
  | all($ids[]; test("^[0-9a-f]{32}$") and . != "00000000000000000000000000000000")
    and (($ids | unique | length) == ($ids | length))
' /etc/lunarwing/ports.json
```

`list-tenants` calls `ports_registry_init()`, which acquires the registry lock and
runs the full v2 → v14 migration chain before printing tenants. Other normal
admin operations that initialize the registry do the same. Migration fails
closed if an existing DarkIRC scope ID is invalid or duplicated.

For a DarkIRC-only rehearsal on an old registry copy, the standalone v6,
v6.1, and v7 scripts remain useful:

```bash
cp -p /etc/lunarwing/ports.json /tmp/ports.darkirc-test.json
PORTS_REGISTRY=/tmp/ports.darkirc-test.json ic/scripts/migrate-ports-v6.sh
PORTS_REGISTRY=/tmp/ports.darkirc-test.json ic/scripts/migrate-ports-v6.1.sh
PORTS_REGISTRY=/tmp/ports.darkirc-test.json ic/scripts/migrate-ports-v7.sh
jq '.tenants | to_entries[] | {tenant: .key, extended_ports: .value.extended_ports}' /tmp/ports.darkirc-test.json
```

Those standalone scripts establish the DarkIRC fields but intentionally do not
advance the copy through later schemas. Do not use their final version as the
expected version of a live production registry.

## Trusted DarkIRC Source And Compatibility

`build-darkirc` uses one host-controlled source path. The default is:

```text
/var/cache/lunarwing/darkfi-source
```

The checkout and Git metadata must be root-owned, symlink-free, non-writable by
group or other, clean (including untracked files), and exactly at the approved
revision `a05956d412a091e8b54c1cd4f4264c33b941203d`. Per-tenant `DARKIRC_SOURCE`
values are not trusted build inputs.

The repository URL may be overridden for a controlled mirror, but the approved
revision may not be changed for managed key exchange:

```bash
sudo LUNARWING_MT_DARKIRC_REPO=https://mirror.example/darkfi \
  ic/scripts/lunarwing-mt-admin.sh build-darkirc --tenant <TENANT>
```

Publication creates an immutable root-owned release under
`/usr/local/lib/lunarwing/darkirc/releases/<release-id>/`. Each release contains
the daemon, typed helper, and a mode-`0444` compatibility attestation binding
the release ID, approved profile/revision/key format, helper source revision,
and both executable digests. Mt-admin publishes one `current` selector with
same-directory atomic rename and directory fsync. Readers resolve that selector
once, pin one release, and the helper executes the same daemon descriptor it
measured. Publication retry re-syncs all release files and parent directories
before selection. `start-tenant` rejects missing, symlinked, stale,
multiply-defined, or systemd-drop-in-overridden DarkIRC daemon commands; run
`render-units <TENANT>` after upgrading old units.

Runtime validation accepts a retained immutable release when its attestation
shape and executable digests remain valid, so an intentional selector rollback
does not depend on the current repository `HEAD`. Upgrade wrappers call
`ensure-darkirc-release`; that gate compares the selected helper source revision
to current trusted source and publishes a new release when they differ.

Legacy `LUNARWING_MT_DARKIRC_BIN`, `LUNARWING_MT_DARKIRC_COMPAT_FILE`, and
`LUNARWING_MT_DARKIRC_KEY_HELPER` overrides now fail closed. Controlled
installations may relocate the complete release root with
`LUNARWING_MT_DARKIRC_RELEASE_ROOT`.

## Provision or Backfill a Tenant

For new tenants, `add-tenant` allocates DarkIRC ports, creates a stable random
`darkirc_scope_id`, and renders the DarkIRC env/config files. DarkIRC must be
explicitly enabled:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant <TENANT> --docker-group --enable-darkirc
```

Tenants created without `--enable-darkirc` have no DarkIRC services, env files,
or service units. `start-tenant`, `status`, and `patch-env` respect the persisted
flag.

For an existing enabled tenant, regenerate missing baseline settings with:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh patch-env <TENANT>
```

The DarkIRC baseline writer is semantic, locked, journaled, and
contact-preserving. Repeated provisioning and `patch-env` preserve existing
`[contact]` tables and unknown operator-owned fields; they do not truncate the
active TOML.

## Build DarkIRC

Build and atomically select an immutable shared daemon/helper/attestation
release:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh build-darkirc --tenant <TENANT>
```

The tenant argument selects the unprivileged account used for the generator
self-test; it does not select tenant-controlled source. Mt-admin:

- clones or verifies the fixed root-owned checkout at the approved revision;
- archives that revision into disposable build trees;
- builds DarkIRC and the helper as the dedicated locked `lunarwing-build` user
  under `setpriv --clear-groups --no-new-privs`, `env -i`, and bounded CPU/jobs;
- validates artifact owner, mode, link count, and executable type;
- self-tests the staged generator output and public/private relationship;
- fsyncs every release file and directory before publication;
- atomically switches one selector only after the complete release validates;
- retains prior complete releases, so interruption leaves either the old or new
  coherent generation selected.

Upgrade wrappers call `ensure-darkirc-release <TENANT>` before rendering or
starting DarkIRC units. `start-tenant` independently validates the selected
release even when the tenant config already exists.

DarkFi's first build also compiles `zkas` and proof circuits, so run this
long-lived operation inside `tmux`.

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
| `/home/<TENANT>/lunarwing/env/lunarwing.env` | Main daemon env, including `DARKIRC_ADAPTER_URL` and `DARKIRC_ADAPTER_SECRET`; it is not a trusted DarkIRC build-source selector |
| `/home/<TENANT>/lunarwing/env/darkirc-adapter.env` | Adapter env rendered atomically from `ic/scripts/templates/darkirc-adapter.env.template` |
| `/home/<TENANT>/lunarwing/state/darkirc/darkirc_config.toml` | Tenant-owned `0600` active daemon config, updated semantically and journaled |
| `/home/<TENANT>/lunarwing/state/darkirc/key-exchange/` | Tenant-owned `0700` Stage A pending, ledger, candidate, transaction, rollback-reserved, and lock subtree; files are exact `0600` single-link |
| `/home/<TENANT>/lunarwing/state/channels/darkirc.capabilities.json` | WASM channel capabilities installed by `install-wasm` |
| `/usr/local/lib/lunarwing/darkirc/releases/<release-id>/` | Immutable root-owned daemon, typed helper, and compatibility attestation generation |
| `/usr/local/lib/lunarwing/darkirc/current` | Atomically replaced root-owned selector for the complete active generation |
| `/home/<TENANT>/lunarwing/logs/` | Tenant daemon, adapter, and service logs; private/public raw exchange values are not logged |

The template placeholders use `__UPPERCASE__` form, not shell `${VAR}` form. Secrets are generated at runtime and are not stored in templates.

## Service Lifecycle

`start-tenant`, `stop-tenant`, `restart-tenant`, and `status` include DarkIRC services.

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

### 3. Check Strict DarkIRC Health

Use mt-admin so the bearer is read owner-scoped and sent to `curl` over header
stdin rather than argv:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-health <TENANT> --strict --json
```

This requires both DarkIRC services to be active through the host's systemd-user
or OpenRC abstraction, then requires authenticated adapter JSON with
`status="ok"` and `irc_connected=true`. Expected output:

```json
{"status":"ok","tenant":"<TENANT>","irc_connected":true}
```

This is diagnostic only in Stage A; it does not activate an installed contact.

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

DarkIRC contact key material belongs inline under `[contact."nickname"]` in the
active TOML. Each contact contains exactly the peer's public key and this
tenant's local private key:

```toml
[contact."nickname"]
dm_chacha_public = "<peer public key>"
my_dm_chacha_secret = "<local private key>"
```

There is no `my_dm_chacha_public` field and no generated YAML keypair file.

For a managed Stage A exchange, both sides must independently obtain and compare
the full `sha256:<64 lowercase hex>` public fingerprint. Nicknames, sender
pairing, or receipt of a message do not authorize a key.

Initiator:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact prepare \
  <TENANT_A> <CONTACT_B> --out offer.json --json
```

Transport `offer.json` through an independently authenticated out-of-band
channel. It contains public material only. The responder runs:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact respond \
  <TENANT_B> <CONTACT_A> \
  --in offer.json --out response.json \
  --expect-peer-fingerprint 'sha256:<INITIATOR_FINGERPRINT>' --json
```

Transport `response.json` back. Each side completes with its own local exchange
ID and the full independently obtained fingerprint of the peer:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact complete \
  <TENANT_A> <CONTACT_B> \
  --in response.json --exchange-id <INITIATOR_OFFER_ID> \
  --expect-peer-fingerprint 'sha256:<RESPONDER_FINGERPRINT>' --defer-apply --json

sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact complete \
  <TENANT_B> <CONTACT_A> \
  --in offer.json --exchange-id <RESPONDER_OFFER_ID> \
  --expect-peer-fingerprint 'sha256:<INITIATOR_FINGERPRINT>' --defer-apply --json
```

Mt-admin enforces deferred completion even if `--defer-apply` is omitted. The
terminal Stage A state is `Installed`; these commands do **not** reload,
restart, health-gate, activate, roll back, or claim a successful encrypted DM.
Do not manually restart a production tenant merely to activate Stage A state;
Stage B activation is intentionally disabled pending separate recovery review.

Inspection and recovery commands are:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact list <TENANT>
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact status <TENANT> <CONTACT>
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact exchanges <TENANT>
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact doctor <TENANT>
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact recover <TENANT>
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact cancel \
  <TENANT> --exchange-id <EXCHANGE_ID> --json
```

If public artifact delivery fails after pending state commits, the sanitized
error reports its `offer_id`. Repeat the same semantic `prepare` or `respond`;
the helper returns the stored byte-identical public artifact rather than
creating another private key.

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

Common failures are fail-closed compatibility or source-custody checks:

- **Build account invalid.** `lunarwing-build` must be locked, use `nologin`, have
  no supplementary groups, and use the configured build home.
- **Trusted checkout invalid.** The fixed checkout or its Git metadata is dirty,
  symlinked, non-root-owned, writable by group/other, contains a gitlink, or is
  not exactly at the approved revision. Repair custody rather than bypassing the
  check.
- **Toolchain unavailable.** Verify host `cargo`, `make`, `taskset`, `setpriv`,
  and `tar`; the build runs with a sanitized environment rather than a tenant's
  shell profile.
- **Generator self-test failed.** Unknown output, stderr output, malformed or
  mismatched X25519 keys, digest mismatch, or unsupported profile prevents
  selector publication. The previous complete release remains selected.
- **Controlled mirror unavailable.** If `LUNARWING_MT_DARKIRC_REPO` points to a
  mirror, it must still provide the approved revision; changing
  `LUNARWING_MT_DARKIRC_REV` is rejected.

### DarkIRC daemon fails to start with `permission denied` on its config

The daemon and helper require the DarkIRC root and all managed subdirectories to
be tenant-owned exact mode `0700`; managed files must be tenant-owned,
single-link, non-symlink exact mode `0600`. Run diagnostics before repair:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact doctor <TENANT>
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact recover <TENANT>
```

Do not use recursive `chown` or `chmod` across pending/journal state: that can
normalize an attacker-controlled link or obscure a partial transaction. Stop
the tenant and inspect the exact failing inode. If no managed transaction state
exists, `patch-env <TENANT>` can safely regenerate the baseline through the
typed helper.

### Adapter `/health` returns `{"error": "unauthorized"}`

`/health` requires the adapter bearer secret. Do not expose it manually; use
mt-admin strict health diagnostics (see [Check Strict DarkIRC Health](#3-check-strict-darkirc-health)).

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

### Temporarily Stop DarkIRC for One Tenant

There is no supported `disable-darkirc` verb. The commands below are a temporary
maintenance stop only: `enable_darkirc=true` remains in the registry,
`start-tenant` will start the services again, and host health/self-heal still
expects them. Coordinate the maintenance window with that health policy so an
automatic repair does not undo the stop.

Systemd user manager:

```bash
sudo -u <TENANT> XDG_RUNTIME_DIR=/run/user/$(id -u <TENANT>) \
  systemctl --user stop lunarwing-darkirc-adapter-<TENANT>.service lunarwing-darkirc-<TENANT>.service
```

OpenRC:

```bash
sudo rc-service lunarwing-darkirc-adapter-<TENANT> stop
sudo rc-service lunarwing-darkirc-<TENANT> stop
```

If the WASM channel remains installed while the adapter is down, channel health
reports degraded/unhealthy. Restore the services after maintenance. Permanent
removal requires an explicit teardown plan for channel activation, units and
boot persistence, registry state, env/config, contacts, and key material; do not
flip `enable_darkirc` manually and leave the other surfaces inconsistent.

## Operator Checklist

For each production tenant:

- [ ] `/etc/lunarwing/ports.json` is current schema v14.
- [ ] `darkirc_adapter`, `darkirc_irc`, and `darkirc_rpc` exist under `.extended_ports`.
- [ ] The root-owned DarkFi checkout is clean, non-writable, symlink-free, and at the approved revision.
- [ ] The locked `lunarwing-build` account and host `cargo`/`make`/`taskset`/`setpriv` toolchain are available.
- [ ] `ensure-darkirc-release <TENANT>` reports one valid selected immutable release.
- [ ] Selected daemon/helper files are root-owned mode `0555`, single-link, and match the release attestation digests.
- [ ] The registry entry has unique `darkirc_scope_id` plus `darkirc_adapter`, `darkirc_irc`, and `darkirc_rpc` ports.
- [ ] `build-tenant <TENANT> --with-wasm` and `install-wasm <TENANT>` completed.
- [ ] `darkirc_config.toml`, `darkirc-adapter.env`, and the key-exchange subtree have exact tenant ownership and `0600`/`0700` modes.
- [ ] `DARKIRC_ADAPTER_URL` exists in `lunarwing.env`; adapter secrets are synchronized without appearing in argv or logs.
- [ ] `start-tenant <TENANT>` starts daemon, adapter, and main LunarWing service.
- [ ] `darkirc-health <TENANT> --strict --json` reports `status=ok` and `irc_connected=true` on the host init system.
- [ ] `darkirc-contact doctor <TENANT>` reports no unsafe reuse, ledger mismatch, or transactional-state issue before migration/exchange.
- [ ] Operators understand Stage A stops at `Installed`; activation, rollback, rotation, revocation, same-host pairing, QR, and web delivery remain disabled.
- [ ] Automatic secret seeding succeeded (no post-start 401 from WASM channel to adapter; rerun `start-tenant` to refresh if needed).
- [ ] Tor/hidden-service plan is documented if onion connectivity is required.
