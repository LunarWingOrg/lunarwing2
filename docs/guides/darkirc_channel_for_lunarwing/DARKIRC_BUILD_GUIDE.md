# DarkIRC Channel Build and Deployment Guide

LunarWing's DarkIRC integration has three components:

1. a pinned DarkIRC daemon built from the approved DarkFi source revision;
2. a per-tenant Python IRC/HTTP adapter;
3. the `darkirc` WASM channel, which polls and emits through that adapter.

Multi-tenant deployments use one daemon and adapter per tenant. Sharing a daemon would share DarkIRC key material and is not a supported isolation model.

## Source Layout

From the repository root:

- `darkirc_channel_for_lunarwing/darkirc/` — canonical WASM channel and adapter source;
- `ic/channels-src/darkirc` — symlink used by the extension registry;
- `ic/registry/channels/darkirc.json` — registry manifest;
- `ic/scripts/lunarwing-mt-admin.sh` — daemon build, tenant provisioning, service lifecycle, ports, and contact exchange;
- [`../../ops/DARKIRC-MULTITENANT.md`](../../ops/DARKIRC-MULTITENANT.md) — operator runbook for contact keys and tenant lifecycle;
- [`DARKIRC_MT_ADAPTER.md`](DARKIRC_MT_ADAPTER.md) — adapter architecture and implementation notes.

Do not patch `ic/src/tools/wasm/host.rs` to allow localhost HTTP. Current URL validation already permits loopback HTTP, and the channel capability explicitly allowlists `127.0.0.1` and `localhost`.

## Build the WASM Channel

Prefer the registry workflow from `ic/`:

```bash
lunarwing registry info darkirc
lunarwing registry install darkirc --build
```

For a repository-wide extension build:

```bash
cd ic
taskset -c 0-11 scripts/build-wasm-extensions.sh --channels
```

The source crate produces `darkirc_channel.wasm`; the installer publishes it as `darkirc.wasm` with `darkirc.capabilities.json` in the configured channel directory. Do not edit installed copies and then expect the source registry to retain those changes.

For a manual source build during channel development:

```bash
cd darkirc_channel_for_lunarwing/darkirc
taskset -c 0-11 cargo build --release -j12 --target wasm32-wasip2
```

Use the committed `build.sh` only after reviewing its install destination. It can install directly into `${LUNARWING_HOME:-$HOME/.lunarwing}/channels/`.

## Build the DarkIRC Daemon for MT

MT admin builds a pinned, approved DarkFi revision as a dedicated unprivileged build user, validates the root-owned source checkout, uses a disposable build tree, and publishes the daemon and trusted contact-key helper transactionally:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh build-darkirc
```

Do not replace this with an arbitrary `cargo build` from a mutable clone. The approved revision and artifact validation are part of the deployment boundary.

The build is resource-bounded by MT admin. Run long build/provision operations inside `tmux`.

## Provision an MT Tenant

Enable DarkIRC when adding the tenant:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant <tenant> \
  --enable-darkirc
sudo ic/scripts/lunarwing-mt-admin.sh build-tenant <tenant> --with-wasm
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh status <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh doctor
```

MT admin allocates `darkirc_irc`, `darkirc_rpc`, and `darkirc_adapter` ports from the registry, writes protected adapter/daemon configuration, and renders systemd-user or OpenRC services. Use MT admin for restart and status; do not probe tenant units through bare system-bus `systemctl`.

The adapter secret and URL are injected through `DARKIRC_ADAPTER_SECRET` and `DARKIRC_ADAPTER_URL`. The WASM channel receives the secret as a host-injected bearer credential. Never write it into workspace memory, a prompt, or a public capability file.

## Current Channel Capability

The source of truth is `darkirc_channel_for_lunarwing/darkirc/darkirc.capabilities.json`. It currently declares:

- optional `darkirc_adapter_secret`, sourced from `DARKIRC_ADAPTER_SECRET`;
- `adapter_url`, sourced from `DARKIRC_ADAPTER_URL`;
- loopback-only HTTP allowlist and credential host patterns;
- channel-scoped workspace prefix `channels/darkirc/`;
- polling enabled with a 3000 ms minimum interval;
- default DM policy `pairing`.

Do not copy an old JSON example over the current capability file. Schema and credential-location fields are versioned with the WIT/host interface.

## Direct Messages and Contact Keys

DarkIRC nick identity is RFC1459-case-folded and versioned, but it is not cryptographic identity; nick reuse can transfer conversational continuity. See [`../../architecture/IRC-SENDER-IDENTITY.md`](../../architecture/IRC-SENDER-IDENTITY.md).

Channel DM policy supports `open`, `allowlist`, and `pairing`. Handle pairing through the normal pairing CLI:

```bash
lunarwing pairing list darkirc
lunarwing pairing approve darkirc <code>
```

DarkIRC encrypted-contact configuration is a separate operator workflow. Each contact has `dm_chacha_public` and `my_dm_chacha_secret` values in protected daemon configuration. Use the typed MT admin exchange commands rather than copying private keys manually:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact prepare <tenant> <contact>
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact respond <tenant> <contact>
sudo ic/scripts/lunarwing-mt-admin.sh darkirc-contact complete <tenant> <contact>
```

Follow [`../../ops/DARKIRC-MULTITENANT.md`](../../ops/DARKIRC-MULTITENANT.md) for the full artifact and fingerprint exchange. Independently verify the peer fingerprint; do not accept one received over the same untrusted path as the exchange artifact.

## Message Limits

The host's channel budget asks the model to stay within six 390-byte DarkIRC messages. The adapter performs UTF-8-safe IRC chunking, ordering markers, framing limits, and paced outbound delivery. Its durable outbox location and limits are set by the tenant adapter environment.

Changing only the WASM response split does not change IRC framing or delivery pacing. Review the adapter source and the host outbound hook together when modifying limits.

## Verification

For a tenant managed by MT admin:

1. `status <tenant>` reports the daemon, adapter, and LunarWing healthy; host `doctor` reports required dependencies available.
2. The adapter `/health` endpoint reports an IRC connection on the tenant's registry port.
3. Authenticated gateway status reports healthy DarkIRC channel state.
4. A paired test nick can send and receive a direct message.
5. An unpaired nick follows the configured policy.
6. A contact-key exchange uses the expected independently verified fingerprint.
7. Restarting the tenant preserves durable outbox and contact state without duplicate delivery.

Redact adapter bearer tokens, contact private keys, service environment, and message content from evidence.

## Troubleshooting

- **Channel installed but inactive** — verify the adapter and daemon through MT admin, confirm the tenant's registry ports, then inspect tenant logs.
- **`Denied(InsecureScheme)`** — confirm the URL is loopback HTTP and matches the installed capability allowlist. Do not patch global URL validation.
- **Authentication failure** — verify MT admin preserved the adapter secret and that the installed capability maps it to a bearer credential; never print the value.
- **Pairing or approval issue** — inspect pending thread/approval state and sender identity. Do not enable global auto-approve as a workaround.
- **WASM trap** — rebuild from the current source and WIT, then inspect the exact host/guest error. Do not replace source with an old prebuilt artifact.
- **Database concern** — use supported backup and migration tools. Running onboarding is not a substitute for database recovery.
