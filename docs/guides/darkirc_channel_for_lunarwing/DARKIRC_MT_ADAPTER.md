# DarkIRC Multi-Tenant Adapter Architecture

**Status:** As-built. LunarWing provisions a dedicated DarkIRC daemon and HTTP adapter for every DarkIRC-enabled tenant.

**Revalidated against source 2026-09-17 (v2.1.2.0).** Per-tenant topology,
mt-admin ownership commands, adapter env contract, and the 3000 ms polling
floor were re-checked against the current tree; no changes were needed.

This document describes the adapter boundary. Operational contact-key exchange and recovery procedures are in [`../../ops/DARKIRC-MULTITENANT.md`](../../ops/DARKIRC-MULTITENANT.md); build and installation steps are in [`DARKIRC_BUILD_GUIDE.md`](DARKIRC_BUILD_GUIDE.md).

## Per-Tenant Topology

```text
DarkIRC P2P network
        |
per-tenant darkirc daemon
  - tenant ChaCha contact keys
  - tenant datastore and host list
  - registry darkirc_irc / darkirc_rpc ports
        |
      IRC
        |
per-tenant darkirc_adapter.py
  - loopback HTTP
  - bearer authentication
  - bounded inbound queue
  - paced durable outbound queue
  - registry darkirc_adapter port
        |
versioned DarkIRC WASM channel
        |
per-tenant LunarWing daemon
```

A shared DarkIRC daemon is not supported. It would collapse tenant P2P identity and key isolation.

## Provisioning Ownership

`ic/scripts/lunarwing-mt-admin.sh` owns the MT integration:

- `add-tenant --enable-darkirc` marks the tenant DarkIRC-enabled and allocates required primary/extended ports;
- `build-darkirc` builds the approved pinned DarkFi revision and installs the trusted contact-key helper;
- `build-tenant --with-wasm` builds and installs the current channel artifact;
- `render-units` writes the correct systemd-user or OpenRC service definitions;
- `start-tenant`, `stop-tenant`, `restart-tenant`, `status`, and `doctor` manage and inspect the complete tenant service graph;
- `remove-tenant` removes managed services and configuration according to its selected retention/purge mode.

Do not call internal shell helpers such as `write_tenant_darkirc_adapter_env` or `ports_get` from an operator runbook. They are implementation details, not stable commands.

## Adapter Configuration

MT admin writes a protected `darkirc-adapter.env` containing tenant-specific values such as:

- daemon loopback host and `darkirc_irc` port;
- adapter loopback host and `darkirc_adapter` port;
- tenant bridge nick/user/real name;
- `ADAPTER_SECRET` bearer token;
- queue and message-size limits;
- paced outbound interval;
- durable outbox journal under tenant state;
- adapter log level.

It also writes `DARKIRC_ADAPTER_URL` and `DARKIRC_ADAPTER_SECRET` into the protected LunarWing environment. The channel setup schema sources the URL from the former and injects the latter as a host-side bearer credential.

Re-rendering or patching a tenant preserves the existing adapter secret when present. Never compare secret-bearing files with plain `grep`, `diff`, or captured terminal output.

## Service Management

On systemd hosts, DarkIRC tenant services run in the tenant user's systemd user manager. On Gentoo they are OpenRC services. MT admin abstracts both.

```bash
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh status <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh doctor
sudo ic/scripts/lunarwing-mt-admin.sh stop-tenant <tenant>
```

Do not use bare system-bus `systemctl` against tenant units. A system-bus probe can report inactive while the user-manager unit is healthy.

The adapter must be available before the LunarWing channel begins polling. Generated service dependencies and MT admin's ordered lifecycle enforce that relationship on both supported init families.

## HTTP Boundary

The adapter exposes loopback endpoints for health, polling/acknowledgement, and outbound delivery. The WASM capability restricts HTTP to `127.0.0.1` and `localhost` and maps `darkirc_adapter_secret` to bearer authentication.

The adapter:

- applies TCP backpressure when its inbound queue is full;
- returns bounded poll batches;
- leaves a batch unacknowledged when dispatch into the agent fails, allowing retry;
- normalizes IRC control characters;
- chunks by UTF-8 byte boundaries and IRC frame limits;
- paces outbound chunks;
- journals queued outbound messages when MT configuration enables the durable path.

The channel's 3000 ms polling minimum and host callback capacity are independent from IRC send pacing. Review both sides before changing throughput limits.

## Contact-Key Isolation

Each tenant daemon stores contact configuration with:

- `dm_chacha_public` — the peer's public key;
- `my_dm_chacha_secret` — this tenant's private key for that contact.

There is no `my_dm_chacha_public` assignment and no shared `darkirc_keypair.yaml` file. Use MT admin's typed `darkirc-contact` exchange commands. The trusted helper keeps private material in protected tenant state and emits public artifacts only.

Completion of a staged exchange does not remove the need to verify the full peer fingerprint through an independent channel. Follow [`../../ops/DARKIRC-MULTITENANT.md`](../../ops/DARKIRC-MULTITENANT.md) for prepare/respond/complete, migration, recovery, and lifecycle-state semantics.

## Health and Verification

A healthy deployment should satisfy all of the following:

1. MT admin `status` reports the tenant daemon, DarkIRC daemon, and adapter in the expected init state.
2. MT admin `doctor` finds no port, ownership, service, or contact-state fault.
3. The adapter's loopback `/health` response reports an IRC connection and expected nick without exposing the bearer token.
4. The authenticated gateway status reports healthy DarkIRC channel state.
5. Paired/allowlisted sender behavior matches `dm_policy`.
6. Restart preserves the durable outbox and does not duplicate acknowledged messages.
7. Two DarkIRC-enabled tenants use different daemon state directories, ports, adapter secrets, and contact private keys.

Use disposable tenants for failure testing. Stop or restart services through MT admin rather than mutating generated units directly.

## Common Failures

- **Adapter healthy, channel unhealthy:** verify the installed channel artifact, `DARKIRC_ADAPTER_URL`, bearer injection, and poll errors.
- **IRC disconnected:** inspect the tenant daemon first, then the adapter's configured IRC registry port and logs.
- **401 from adapter:** re-render/patch through MT admin and verify secret mapping without printing either secret-bearing file.
- **Port collision:** run MT admin `doctor` and inspect `/etc/lunarwing/ports.json`; do not hand-edit generated service ports while units are running.
- **Contact exchange blocked:** use `darkirc-contact status`/`doctor` and resolve the lifecycle state rather than editing TOML by hand.
