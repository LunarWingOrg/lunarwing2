# WeeChat Relay Channel for LunarWing

LunarWing reaches IRC networks through a per-instance WeeChat relay. The current integration has three components:

1. WeeChat exposes its version-4 `api` relay on loopback.
2. `ws_adapter.py` holds the WebSocket connection to WeeChat and exposes a local HTTP/long-poll API.
3. The `weechat` WASM channel consumes that adapter and sends replies through the relay.

The adapter-to-WeeChat hop is WebSocket; the sandboxed channel-to-adapter hop is HTTP. See [`../../architecture/WEECHAT-CHANNEL-ARCHITECTURE.md`](../../architecture/WEECHAT-CHANNEL-ARCHITECTURE.md) for the current message flow, configuration precedence, sender principals, latency model, and known limitations.

**Revalidated against source 2026-09-17 (v2.1.2.0); re-checked 2026-09-27 (v2.1.3.0), 2026-10-01 (v2.1.4.0), 2026-10-04 (v2.1.5.0), 2026-10-10 (v2.1.6.0), and 2026-10-25 (v2.1.7.0).** Source/registry paths,
capability schema, mt-admin bootstrap/recovery commands, and the preflight
script were re-checked against the current tree; no changes were needed (the
2.1.2.0 release only bumped the channel crate/registry versions to 0.2.9;
the 2.1.3.0 release again only bumped them to 0.2.10; the 2.1.4.0 release
bumped the registry manifest to 0.2.11 and the WASM channel crate to the
workspace `2.1.4` version; the 2.1.5.0 release bumped the registry manifest
to 0.2.12 and the WASM channel crate to `2.1.5`; the 2.1.6.0 release bumped
the registry manifest to 0.2.13 and the WASM channel crate to `2.1.6`; the
2.1.7.0 release bumped the registry manifest to 0.2.14 and the WASM channel
crate to `2.1.7`).

## Source and Registry

- `lunarwing_weechat_wss/weechat_relay/` — canonical channel and adapter source
- `ic/channels-src/weechat` — registry source symlink
- `ic/registry/channels/weechat.json` — extension manifest
- `lunarwing_weechat_wss/weechat_relay/weechat.capabilities.json` — setup and capability schema
- [`../../ops/WEECHAT-SERVICES.md`](../../ops/WEECHAT-SERVICES.md) — authoritative MT service/runbook documentation

The old nested install and troubleshooting guides were archived under `docs/guides/history/lunarwing_weechat_wss/`. They instructed operators to store passwords in capability JSON, edit database settings directly, and delete channel state; do not use those procedures.

## Multi-Tenant Setup

MT admin configures WeeChat by default when adding a tenant. It:

- allocates `weechat` and `weechat_adapter` ports from `/etc/lunarwing/ports.json`;
- generates a per-tenant relay password;
- writes only `RELAY_PASSWORD` to the protected `env/weechat.env` consumed by WeeChat;
- stores `${env:RELAY_PASSWORD}` literally in `relay.conf` rather than the resolved value;
- binds the relay to `127.0.0.1` and disables relay IPv6 before binding;
- renders WeeChat, adapter, and LunarWing dependencies for systemd user managers or OpenRC;
- injects `RELAY_URL`, `WS_ADAPTER_URL`, and `RELAY_PASSWORD` into the host/channel setup path.

Typical flow:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh build-tenant <tenant> --with-wasm
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh status <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh doctor
```

Use `--no-weechat-bootstrap` only when the relay should not be configured during tenant creation. Recover later with:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-weechat-relay <tenant>
sudo ic/scripts/lunarwing-weechat-preflight.sh <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh restart-tenant <tenant>
```

The bootstrap and recovery command preserve any non-empty existing WeeChat configuration rather than overwriting it.

After upgrading an existing tenant, use MT admin's supported reconciliation paths when needed:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh patch-env <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh render-units <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh restart-tenant <tenant>
```

Do not call bare system-bus `systemctl` for tenant units. On systemd hosts they live in the tenant user's user manager; on Gentoo they are OpenRC services.

## Build or Install the Channel

Prefer the registry workflow from `ic/`:

```bash
lunarwing registry info weechat
lunarwing registry install weechat --build
```

For a repository-wide channel build:

```bash
cd ic
taskset -c 0-11 scripts/build-wasm-extensions.sh --channels
```

For manual channel development:

```bash
cd lunarwing_weechat_wss/weechat_relay
taskset -c 0-11 cargo build --release -j12 --target wasm32-wasip2
```

The installer publishes the channel WASM and current capability sidecar. Do not patch global URL validation for loopback HTTP; current validation allows localhost, and the capability file explicitly allowlists `127.0.0.1` and `localhost`.

## Single-Instance Configuration

For a non-MT instance, configure WeeChat's `api` relay on loopback and run the adapter under a real service manager. Provide its password through a protected environment file, not as a command-line argument or in shell history.

Install the WASM channel, then use the authenticated extension setup flow for:

- required secret `weechat_relay_password`;
- `relay_url` (defaults to `http://127.0.0.1:9001`);
- `connection_mode`: `auto`, `websocket`, or `http`;
- `ws_adapter_url` (defaults to `http://127.0.0.1:6681`);
- network allowlist;
- `dm_policy`, `group_policy`, and sender allowlist;
- optional diagnostic logging fields.

The host injects `weechat_relay_password` as HTTP Basic authentication with username `plain`. Never add a `relay_password` field to the capability `config` object.

## Policy and Pairing

The channel supports:

- DM policy: `open`, `allowlist`, or `pairing`;
- group policy: `open`, `allowlist`, or `deny`;
- sender patterns as nicks or `nick!user@host` values;
- network include and exclude filters.

Approve a DM pairing through the normal CLI in the tenant context:

```bash
lunarwing pairing list weechat
lunarwing pairing approve weechat <code>
```

IRC identity is not cryptographic. WeeChat sender principals are network-qualified and case-folded; owner routing and continuity rules are documented in [`../../architecture/IRC-SENDER-IDENTITY.md`](../../architecture/IRC-SENDER-IDENTITY.md).

## Verification

1. MT admin `status <tenant>` reports WeeChat, adapter, and LunarWing healthy; host `doctor` reports required dependencies available.
2. The preflight script confirms the relay type, bind address, port, and protected password wiring.
3. The adapter health endpoint responds on the tenant's `weechat_adapter` port.
4. A configured IRC connection receives a direct message and channel message according to policy.
5. Pairing rejects an unknown sender until approved.
6. Outbound replies reach the correct network-qualified buffer.
7. Restart restores WeeChat upgrade state and does not replay or mirror-loop prior messages.

Use test networks/rooms and redact relay credentials and message contents from evidence.

## Troubleshooting

- **Wrong tenant ports:** run `patch-env`, `render-units`, then `doctor`; do not edit the settings table or capability JSON to fight generated MT values.
- **Relay authentication loop:** run the secret-safe preflight and relay recovery command. Do not use `ic_sm` scripts or print the master key.
- **Adapter disconnected:** verify WeeChat is running first, the relay type is `api`, and the loopback relay port matches the registry.
- **No inbound messages:** inspect adapter health, network/sender policy, pairing, and the architecture guide's long-poll/fallback state.
- **Duplicate or self-triggered messages:** inspect the current source's tag filters and adapter event cursor; do not delete channel workspace state as a first response.
- **`Denied(InsecureScheme)`:** verify the endpoint is an allowlisted loopback HTTP URL. Never apply the old host-source patch.
- **Upgrade restart loses buffers:** render current units so the committed WeeChat start/stop helpers use `/upgrade -quit` and `--upgrade` together.
