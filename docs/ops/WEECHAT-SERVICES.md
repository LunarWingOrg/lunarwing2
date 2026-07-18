# WeeChat Services for Multi-Tenant Deployments

How WeeChat IRC access is managed as init services in LunarWing multi-tenant deployments. Each tenant runs a WeeChat instance in a tmux session plus a Python WebSocket adapter that bridges WeeChat's relay API to an HTTP endpoint polled by the LunarWing WASM channel.

For general multi-tenant setup, see [`MULTITENANCY-PRODUCTION.md`](MULTITENANCY-PRODUCTION.md).

## Architecture

```
WASM channel (poll) ──GET──► ws_adapter.py (port base+9)
                              │
                              ▼
                          WebSocket
                              │
                              ▼
                         WeeChat relay (port base+5, 127.0.0.1)

WASM channel (send) ──POST──► WeeChat relay (direct)
```

Three components per tenant:

| Component | Process | Description |
|-----------|---------|-------------|
| WeeChat | `weechat` in tmux | IRC client, runs relay API on `127.0.0.1:<base+5>` |
| WS adapter | `ws_adapter.py` | Bridges WeeChat's WebSocket relay to a local HTTP API |
| WASM channel | `weechat_relay_channel` | Polls the adapter every 3s; sends directly to the relay |

The adapter script lives at `lunarwing_weechat_wss/weechat_relay/ws_adapter.py` in the source repo.

## Service Dependency Chain

```
lunarwing-weechat-<name> → lunarwing-weechat-adapter-<name> → lunarwing-<name>
```

WeeChat must be running before the adapter starts. The adapter must be running before the main daemon starts. The MT admin script wires this automatically via `Requires=`/`After=` (systemd) or `need`/`before` (OpenRC).

## Port Allocation

WeeChat uses two ports from each tenant's 10-port block (allocated from `/etc/lunarwing/ports.json`):

| Offset | Port name | Service |
|--------|-----------|---------|
| +5 | `weechat` | WeeChat relay API |
| +9 | `weechat_adapter` | WS adapter HTTP endpoint |

Example: a tenant with base port `10050` gets WeeChat relay on `10055` and adapter on `10059`.

Both ports bind to `127.0.0.1` only.

## Environment Variables

The following are written to `lunarwing.env` by the MT admin script:

| Variable | Value | Consumed by | Description |
|----------|-------|-------------|-------------|
| `RELAY_URL` | `http://127.0.0.1:<base+5>` | adapter **+ WASM channel** | WeeChat relay endpoint |
| `WS_ADAPTER_URL` | `http://127.0.0.1:<base+9>` | WASM channel | Full adapter URL the in-process WASM channel polls |
| `RELAY_PASSWORD` | auto-generated 32-char token | adapter + WeeChat **+ WASM channel** | Shared secret; the WASM authenticates to the adapter with it |
| `ADAPTER_PORT` | `<base+9>` | adapter | Bare HTTP port the standalone adapter listens on |
| `WEECHAT_ADAPTER_PORT` | `<base+9>` | adapter | Alias of `ADAPTER_PORT` |

`RELAY_PASSWORD` is generated per tenant during `add-tenant` and must match the password configured inside WeeChat (see [WeeChat Relay Setup](#weechat-relay-setup)). On fresh tenants the automatic bootstrap writes the password as the literal `${env:RELAY_PASSWORD}` expression into `relay.conf` and provides the resolved value through a dedicated `env/weechat.env` file (mode `0600`).

> **Per-tenant ports & the in-process WASM channel.** The LunarWing daemon
> (which hosts the WeeChat WASM channel in-process) sources `relay_url`,
> `ws_adapter_url`, and `relay_password` from `RELAY_URL`, `WS_ADAPTER_URL`, and
> `RELAY_PASSWORD` at startup. Without these the channel falls back to the
> hardcoded `:9001`/`:6681` defaults and silently fails for every tenant whose
> ports differ. See the archived `WEECHAT-MULTITENANT-PORT-BUG.md` in `docs/internal/history/archive/ops/`.
> Existing tenants need `WS_ADAPTER_URL` backfilled — run `mt-admin patch-env <name>`.

## Generated Service Units

### Systemd (user-level)

Units are installed to `~/.config/systemd/user/` per tenant.

**`lunarwing-weechat-<name>.service`**

```ini
[Unit]
Description=WeeChat IRC client (<name>)
After=network.target

[Service]
Type=forking
ExecStart=/usr/bin/tmux -L weechat-<name> new-session -d -s weechat '/usr/bin/weechat --dir /home/<name>/.config/weechat'
ExecStop=/usr/bin/tmux -L weechat-<name> kill-session -t weechat
EnvironmentFile=<env_dir>/weechat.env
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
```

Uses `Type=forking` because tmux daemonizes after creating the session.

**`lunarwing-weechat-adapter-<name>.service`**

```ini
[Unit]
Description=LunarWing WeeChat WS adapter (<name>)
After=network.target lunarwing-weechat-<name>.service
Requires=lunarwing-weechat-<name>.service
PartOf=lunarwing-<name>.service

[Service]
Type=simple
WorkingDirectory=<repo>/lunarwing_weechat_wss/weechat_relay
EnvironmentFile=<env_dir>/lunarwing.env
ExecStart=/usr/bin/python3 <repo>/lunarwing_weechat_wss/weechat_relay/ws_adapter.py
Restart=on-failure
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=default.target
```

The `PartOf=lunarwing-<name>.service` means stopping the main daemon also stops the adapter.

**Main daemon unit** (`lunarwing-<name>.service`) includes WeeChat services in its dependency list:

```ini
After=... lunarwing-weechat-<name>.service lunarwing-weechat-adapter-<name>.service
Wants=... lunarwing-weechat-<name>.service lunarwing-weechat-adapter-<name>.service
```

### OpenRC (system-level)

Init scripts are installed to `/etc/init.d/` with conf.d files in `/etc/conf.d/`.

**`/etc/init.d/lunarwing-weechat-<name>`**

Runs WeeChat in a tmux session via `start-stop-daemon`. The `start()` function loads `env/weechat.env` (containing only `RELAY_PASSWORD`) before launching tmux, so the WeeChat process inherits the credential. `stop()` kills the session with `tmux kill-session`. Configurable via conf.d variables: `weechat_user`, `weechat_group`, `weechat_home`, `weechat_env_file`.

Dependency wiring:

```
depend() {
    need net
    use dns
    after firewall
    before lunarwing-weechat-adapter-<name> lunarwing-<name>
}
```

**`/etc/init.d/lunarwing-weechat-adapter-<name>`**

Uses `supervise-daemon` with automatic respawn (`respawn_delay=5`, `respawn_max=5`, `respawn_period=60`). Reads env from the tenant's `lunarwing.env`. Logs to `<log_dir>/weechat-adapter.log` and `<log_dir>/weechat-adapter.err`.

Dependency wiring:

```
depend() {
    need net lunarwing-weechat-<name>
    use dns
    after firewall lunarwing-weechat-<name>
    before lunarwing-<name>
}
```

**Conf.d for the main daemon** (`/etc/conf.d/lunarwing-<name>`) includes:

```
lunarwing_rc_need="xmpp-bridge-<name> lunarwing-proxy-<name> lunarwing-weechat-<name> lunarwing-weechat-adapter-<name>"
```

## WeeChat Relay Setup

### Automatic bootstrap (fresh tenants)

During `add-tenant`, after tenant environment files are written but before service rendering and startup, the MT admin script attempts a one-shot WeeChat relay bootstrap. This generates a complete relay configuration by invoking the `weechat` binary as the tenant user with a minimal, secret-safe command sequence:

```bash
--run-command '/set relay.network.password "\${env:RELAY_PASSWORD}"'
--run-command '/set relay.network.allow_empty_password off'
--run-command '/set relay.network.ipv6 off'
--run-command '/set relay.network.bind_address "127.0.0.1"'
--run-command '/relay add api <registry_weechat_port>'
--run-command '/save'
--run-command '/quit'
```

Key properties of the automatic bootstrap:

- **Secret-safe:** The relay password is stored as the literal expression `${env:RELAY_PASSWORD}` in `relay.conf`. The resolved password is passed only via environment inheritance — never as a positional argument, in a command string, in a systemd unit value, in log output, or in the generated WeeChat configuration.
- **Loopback-only:** IPv6 relay mode is disabled before binding to `127.0.0.1`, avoiding WeeChat's invalid-IPv6-bind rejection without widening the listener.
- **Preserve-and-fail:** If the target `~/.config/weechat` directory already contains any content, the bootstrap fails without modifying the existing configuration. The explicit recovery command also refuses to overwrite existing content.
- **Non-fatal:** If the automatic bootstrap fails for any reason, base tenant provisioning continues. A prominent warning is printed showing the exact recovery command.
- **Dedicated minimal env:** The WeeChat process receives only `RELAY_PASSWORD` through a dedicated, tenant-owned `env/weechat.env` file (mode `0600`). The full tenant `lunarwing.env` is never loaded into the WeeChat process, preventing unrelated DB, LLM, XMPP, and gateway secrets from being exposed.

No manual interaction is required for a fresh tenant to receive a working relay.

### Disabling relay bootstrap: `--no-weechat-bootstrap`

By default, `add-tenant` and `add-tenants` run the full WeeChat relay bootstrap. Pass `--no-weechat-bootstrap` to skip the WeeChat command execution and `relay.conf` generation:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant <name> --no-weechat-bootstrap
```

What the flag does:

- **No WeeChat process is started.** The one-shot `weechat --run-command` invocation is skipped entirely.
- **No `relay.conf` is generated.** The tenant's `~/.config/weechat` directory is not created or populated.
- **Minimal `weechat.env` is still written.** The tenant's existing `RELAY_PASSWORD` from `lunarwing.env` is copied into the dedicated `env/weechat.env` file (mode `0600`, tenant-owned). This lets the rendered WeeChat service start without loading the full `lunarwing.env`.
- **Services are still rendered.** The systemd user unit (`lunarwing-weechat-<name>.service`) or OpenRC init script (`/etc/init.d/lunarwing-weechat-<name>`) is generated and installed as usual. When the service starts, WeeChat launches but has no relay configured.
- **Base provisioning continues.** SSH, health, database, workspace, and all other tenant setup proceeds normally.

The `add-tenant` summary line reports one of three WeeChat relay states:

| Summary line | Meaning |
|---|---|
| `weechat relay:    configured` | Automatic bootstrap succeeded. `relay.conf` exists and is validated. |
| `weechat relay:    needs recovery` | Bootstrap failed. WeeChat will start but the relay is not configured. Run `configure-weechat-relay` (see below). |
| `weechat relay:    disabled (--no-weechat-bootstrap)` | Opt-out succeeded. `weechat.env` written, no relay config generated. Run `configure-weechat-relay` to add a relay later. |

If the minimal credential env could not be written (for example, `RELAY_PASSWORD` is absent from `lunarwing.env`), the summary shows `disabled (credential env setup failed)` and a warning is printed.

#### Recovering an opted-out tenant

To generate the relay configuration after a tenant was created with `--no-weechat-bootstrap`, run the explicit recovery command and then verify with preflight:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-weechat-relay <name>
sudo ic/scripts/lunarwing-weechat-preflight.sh <name>
```

`configure-weechat-relay` generates `relay.conf` using the same secret-safe one-shot WeeChat invocation as the automatic bootstrap. It refuses any existing non-empty `~/.config/weechat` without mutation. After it succeeds, restart the WeeChat service so the relay loads the new configuration:

```bash
# systemd
sudo -u <name> XDG_RUNTIME_DIR=/run/user/$(id -u <name>) \
  systemctl --user restart lunarwing-weechat-<name>.service

# OpenRC
rc-service lunarwing-weechat-<name> restart
```

#### Propagation across provisioning surfaces

The opt-out is wired through every new-tenant path. Kawarimi import does not expose it.

| Surface | Field or flag | Default |
|---|---|---|
| `add-tenant` / `add-tenants` (shell) | `--no-weechat-bootstrap` | Disabled (bootstrap runs) |
| Python onboarding CLI (`lunarwing_mt_onboard`) | `TenantConfig.no_weechat_bootstrap` | `False` (bootstrap runs) |
| Browser provision wizard (`lunarwing_mt_onboard_web`) | Checkbox: "Automatically configure WeeChat relay" | Checked (bootstrap runs) |
| OpenRC bulk provisioner (`lunarwing-mt-provision-openrc.sh`) | `ENABLE_WEECHAT_BOOTSTRAP=true` | `true` (bootstrap runs) |
| Kawarimi import (`import-tenant.sh`) | Not exposed | Normal bootstrap applies |

The Python CLI prompts "Automatically configure the WeeChat relay?" (default yes) and shows the choice in its summary table. The browser wizard defaults the checkbox to checked; unchecking it sets `no_weechat_bootstrap: true` in the provision request. The OpenRC bulk provisioner forwards `--no-weechat-bootstrap` to `add-tenants` when `ENABLE_WEECHAT_BOOTSTRAP` is set to `false`.

### Explicit recovery: `configure-weechat-relay`

If the automatic bootstrap failed or was skipped before creating configuration, an operator can generate the missing relay configuration explicitly:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-weechat-relay <tenant>
```

This command:

- Validates the tenant, environment file, and registry-derived WeeChat port.
- **Refuses any existing non-empty `~/.config/weechat` content** without mutation. This is a safety guarantee — existing configurations are never overwritten.
- Generates the relay configuration into a same-filesystem temporary directory beside the target, validates it, and atomically renames it into place.
- Writes the dedicated `env/weechat.env` file with only `RELAY_PASSWORD`, owned by the tenant user with mode `0600`.
- On success, reports `configured`; on conflict or failure, exits nonzero without modifying the target.

Rerunning the command on an already-configured tenant is safe (it will refuse the existing config and report the conflict) but is not necessary.

If preflight reports that an existing non-empty configuration has IPv6 enabled or missing, preserve the configuration and repair it inside the tenant's running WeeChat session:

```text
/set relay.network.ipv6 off
/save
```

The option change rebinds configured relays immediately; rerun `lunarwing-weechat-preflight.sh <tenant>` afterward.

### Manual relay setup (recovery only)

If both the automatic bootstrap and the explicit recovery command are unavailable or have failed, the relay can be configured manually inside WeeChat as a last resort. Attach to the tmux session and run:

```bash
/set relay.network.password "<RELAY_PASSWORD>"
/set relay.network.allow_empty_password off
/set relay.network.ipv6 off
/set relay.network.bind_address "127.0.0.1"
/relay add api <weechat_port>
/save
```

Replace `<weechat_port>` with the tenant's allocated relay port (base+5). The `RELAY_PASSWORD` value comes from `env/weechat.env` (or `lunarwing.env` on older tenants).

> **Warning:** Manual relay setup bypasses the preserve-and-fail guarantee. Do not use it on a tenant that already has a configured relay unless you intend to overwrite the existing configuration.

### Dedicated WeeChat environment file

The WeeChat service reads `RELAY_PASSWORD` from a dedicated file at `<env_dir>/weechat.env` (mode `0600`, owned by the tenant user). This file contains only:

```bash
RELAY_PASSWORD=<generated-token>
```

The systemd user unit loads this file with `EnvironmentFile=`. OpenRC starts
WeeChat through the root-owned env launcher after `start-stop-daemon --user`
drops privileges, so no root hook opens the tenant-controlled file. The adapter
and daemon continue to receive credentials from `lunarwing.env`; OpenRC loads
those files through the same post-drop launcher rather than shell-sourcing them
as root.

### Preflight checks

`ic/scripts/lunarwing-weechat-preflight.sh` is a read-only diagnostic that checks the generated relay configuration and minimal env without sourcing either file. It validates:

- `relay.conf` contains the literal `${env:RELAY_PASSWORD}` expression (never the resolved value), loopback bind, `[api]` section, and the registry-derived port.
- `env/weechat.env` exists and contains the password assignment.
- Adapter and capability checks are preserved from earlier versions.

Preflight reports the password only as `set (<N> chars)` or `missing` — it never prints the value. Absent relay config or missing minimal env are classified as FAIL with explicit recovery guidance.

## DM Access Control (`dm_policy`)

WeeChat's `dm_policy` defaults to `pairing`: an unpaired IRC sender receives pairing instructions and does **not** execute under owner scope. This matches the capabilities/setup prompt and the audit-corrected code default.

| `dm_policy` | Behavior |
|-------------|----------|
| `pairing` (default) | Only senders approved via `lunarwing pairing approve weechat <CODE>` or the gateway API may DM the agent. Each new sender gets pairing instructions on their first message. |
| `allowlist` | Only nicks/hostmasks in the configured `allow_from` list (plus any approved pairing codes) may DM. |
| `open` | Any IRC sender may DM the agent. **Caution:** with no configured owner actor, accepted senders execute under the instance owner's scope and inherit owner workspace/secrets. |

An unknown `dm_policy` string (typo, stale value) **fails closed** — the sender is rejected with a warning rather than treated as `open`. This prevents an accidental typo from granting open DMs.

To intentionally enable open DMs, set `"dm_policy":"open"` explicitly in the adapter config (`weechat_local_config.json`) or the DB `setup_fields` row. Existing persisted operator choices are preserved on upgrade — the new `pairing` default only applies when no stored value exists.

To approve a sender manually:

```bash
# Via CLI (source env first on MT hosts)
sudo -u <name> bash -c '
  set -a
  source /home/<name>/lunarwing/env/lunarwing.env
  set +a
  lunarwing pairing approve weechat <CODE>
'

# Or via gateway API
curl -sf -X POST http://127.0.0.1:<gateway_port>/api/pairing/weechat/approve \
  -H "Authorization: Bearer <GATEWAY_AUTH_TOKEN>" \
  -H "Content-Type: application/json" \
  -d '{"code":"<CODE>"}'
```

Note: With `ENGINE_V2=false`, the legacy path also honors `dm_policy`, so this setting protects both routing modes uniformly.

## Manual Operations

### Attach to WeeChat

Each tenant's WeeChat runs in a named tmux socket:

```bash
# As the tenant user
tmux -L weechat-<name> attach -t weechat

# As root
sudo -u <name> tmux -L weechat-<name> attach -t weechat
```

Detach with `Ctrl-b d` (standard tmux detach).

### Check service status

Systemd:
```bash
# As root (for any tenant)
sudo -u <name> XDG_RUNTIME_DIR=/run/user/$(id -u <name>) \
  systemctl --user status lunarwing-weechat-<name>.service \
                          lunarwing-weechat-adapter-<name>.service

# As the tenant user
systemctl --user status lunarwing-weechat-<name>.service
systemctl --user status lunarwing-weechat-adapter-<name>.service
```

OpenRC:
```bash
rc-service lunarwing-weechat-<name> status
rc-service lunarwing-weechat-adapter-<name> status
```

### View logs

Systemd:
```bash
sudo -u <name> XDG_RUNTIME_DIR=/run/user/$(id -u <name>) \
  journalctl --user -u lunarwing-weechat-<name>.service -f

sudo -u <name> XDG_RUNTIME_DIR=/run/user/$(id -u <name>) \
  journalctl --user -u lunarwing-weechat-adapter-<name>.service -f
```

OpenRC:
```bash
tail -f /home/<name>/lunarwing/logs/weechat.log
tail -f /home/<name>/lunarwing/logs/weechat-adapter.log
```

### Restart services

Restart WeeChat and the adapter together (the dependency chain handles ordering):

Systemd:
```bash
sudo -u <name> XDG_RUNTIME_DIR=/run/user/$(id -u <name>) \
  systemctl --user restart lunarwing-weechat-<name>.service
```

OpenRC:
```bash
rc-service lunarwing-weechat-<name> restart
rc-service lunarwing-weechat-adapter-<name> restart
```

## Adding WeeChat to an Existing Tenant

If a tenant was created before WeeChat services were added, re-render units and add the environment variables. The relay configuration can then be generated with `configure-weechat-relay`.

### 1. Add env vars to `lunarwing.env`

```bash
# Get the tenant's WeeChat ports
sudo ic/scripts/lunarwing-mt-admin.sh status <name>

# Edit the env file (as the tenant user or root)
# Add these lines:
RELAY_URL=http://127.0.0.1:<weechat_port>
RELAY_PASSWORD=<generate-a-token>
ADAPTER_PORT=<weechat_adapter_port>
WEECHAT_ADAPTER_PORT=<weechat_adapter_port>
WS_ADAPTER_URL=http://127.0.0.1:<weechat_adapter_port>
```

> `RELAY_URL`, `WS_ADAPTER_URL`, and `RELAY_PASSWORD` are what the in-process
> WASM channel reads — omitting `WS_ADAPTER_URL` makes the channel poll the
> hardcoded `:6681` default. `mt-admin patch-env <name>` adds `WS_ADAPTER_URL`
> (and `RELAY_URL` if missing) idempotently.

Generate a relay password:

```bash
openssl rand -hex 16
```

### 2. Create WeeChat config directory

```bash
sudo -u <name> mkdir -p /home/<name>/.config/weechat
```

### 3. Re-render service units

```bash
# Stop the tenant first
sudo ic/scripts/lunarwing-mt-admin.sh stop-tenant <name>

# Re-render (regenerates all units including WeeChat)
sudo ic/scripts/lunarwing-mt-admin.sh render-units <name>

# Reload and start
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant <name>
```

### 4. Configure the WeeChat relay

Run the explicit recovery command to generate the relay configuration:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-weechat-relay <name>
```

This uses the supported WeeChat command interface, preserves any existing config (refusing non-empty targets), and stores the password as the literal `${env:RELAY_PASSWORD}` expression. See [Explicit recovery: `configure-weechat-relay`](#explicit-recovery-configure-weechat-relay) for details.

## Troubleshooting

### Adapter fails to connect to WeeChat relay

**Symptom**: Adapter logs show connection refused or timeout errors.

1. Verify WeeChat is running: `tmux -L weechat-<name> list-sessions`
2. Verify the relay is configured inside WeeChat: attach and run `/relay list`
3. Confirm the relay port matches `RELAY_URL` in `lunarwing.env`
4. Confirm `relay.network.bind_address` is `127.0.0.1` (not `0.0.0.0` or empty)

### WeeChat relay not configured

**Symptom**: WeeChat is running but the adapter cannot authenticate.

On fresh tenants the relay is configured automatically during `add-tenant`. If the automatic bootstrap failed or was skipped, run the recovery command:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-weechat-relay <name>
```

If that also fails, fall back to manual relay setup inside WeeChat (see [Manual relay setup (recovery only)](#manual-relay-setup-recovery-only)).

### tmux session died

**Symptom**: `tmux -L weechat-<name> list-sessions` returns "no server running" or "no sessions".

Restart the WeeChat service:

```bash
# Systemd
sudo -u <name> XDG_RUNTIME_DIR=/run/user/$(id -u <name>) \
  systemctl --user restart lunarwing-weechat-<name>.service

# OpenRC
rc-service lunarwing-weechat-<name> restart
```

If the tmux socket file is stale (exists but no server), remove it first:

```bash
rm -f /tmp/tmux-$(id -u <name>)/weechat-<name>
```

### WASM channel reports no messages

**Symptom**: LunarWing is running but not receiving IRC messages.

1. Check the adapter is running and healthy (see [Check service status](#check-service-status))
2. Verify `WEECHAT_ADAPTER_PORT` is set in `lunarwing.env`
3. Confirm the `weechat_relay_channel` WASM module is installed in the tenant's `state/channels/` directory
4. Check the daemon logs for WASM channel load errors

### Password mismatch

**Symptom**: Adapter connects but authentication fails.

The `RELAY_PASSWORD` in `lunarwing.env` must exactly match the value set inside WeeChat via `/set relay.network.password`. Attach to WeeChat and verify:

```
/set relay.network.password
```

If they differ, update one to match the other and restart the adapter.

### CLI pairing approve fails with "no pairing file"

**Symptom**: `lunarwing pairing approve weechat <CODE>` returns `Invalid channel: no pairing file`, even though the pairing request is visible via IRC and the gateway API works.

The CLI resolves the pairing store from `LUNARWING_BASE_DIR`. In multi-tenant deployments this is set per-tenant in `lunarwing.env` (typically `/home/<name>/lunarwing/state`), but it is not exported into the tenant user's shell environment. Running `sudo -u <name> lunarwing pairing approve ...` without that variable causes the CLI to look in the wrong directory.

Either source the env file first, or pass `LUNARWING_BASE_DIR` explicitly:

```bash
# Option A: pass the variable inline
sudo -u <name> LUNARWING_BASE_DIR=/home/<name>/lunarwing/state \
  lunarwing pairing approve weechat <CODE>

# Option B: source the env file
sudo -u <name> bash -c '
  set -a
  source /home/<name>/lunarwing/env/lunarwing.env
  set +a
  lunarwing pairing approve weechat <CODE>
'
```

Alternatively, use the gateway API (no env vars needed):

```bash
curl -sf -X POST http://127.0.0.1:<gateway_port>/api/pairing/weechat/approve \
  -H "Authorization: Bearer <GATEWAY_AUTH_TOKEN>" \
  -H "Content-Type: application/json" \
  -d '{"code":"<CODE>"}'
```
