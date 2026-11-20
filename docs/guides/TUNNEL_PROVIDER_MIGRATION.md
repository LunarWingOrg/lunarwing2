# Managed Tunnel Provider Migration

Since LunarWing v2.0.3.0, the managed providers are NetBird Reverse Proxy and
custom tunnel commands. Managed Cloudflare Tunnel, ngrok, and Tailscale providers were removed. An
externally managed service from any vendor remains supported through a static
HTTPS `TUNNEL_URL`.

**Source revalidated 2026-11-20 for v2.2.1.0 (`4192ee72b`).** The resolver,
custom command, settings write guard, doctor checks and V24 migrations were
checked against the tree. This is not a live migration test.

This guide concerns daemon/webhook exposure, not the separate opt-in sandbox
egress overlay in [`SANDBOX_TUNNEL_NETWORKING.md`](SANDBOX_TUNNEL_NETWORKING.md).

## Before Upgrading

1. Stop the previous LunarWing process cleanly so its managed child process can
   exit. Check vendor-side state afterward, especially persistent Tailscale
   Serve or Funnel configuration.
2. Choose NetBird, a custom command, or an externally managed static URL.
3. Record webhook and OAuth callback URLs that must be updated. Do not copy
   provider credentials into migration notes.
4. Revoke or rotate unused Cloudflare and ngrok credentials after cutover.

## Replacement Configuration

NetBird:

```env
TUNNEL_PROVIDER=netbird
TUNNEL_NETBIRD_NAME_PREFIX=lunarwing
# TUNNEL_NETBIRD_CUSTOM_DOMAIN=lunarwing.example.com
```

Custom managed command:

```env
TUNNEL_PROVIDER=custom
TUNNEL_CUSTOM_COMMAND=example-tunnel --port {port} --host {host}
TUNNEL_CUSTOM_URL_PATTERN=tunnel.example.com
```

Use a command that prints its public URL on stdout. The optional pattern is a
substring, not a regular expression. If unset or not matched, the custom
provider reports its local HTTP target instead. The command is split on
whitespace after placeholder substitution; it is not evaluated by a shell.

Externally managed HTTPS endpoint:

```env
TUNNEL_URL=https://tunnel.example.com
```

`TUNNEL_URL` takes precedence over managed provider settings. Without a static
URL, a legacy `TUNNEL_PROVIDER=cloudflare`, `ngrok`, or `tailscale` value stops
startup with migration guidance instead of silently running without webhook
exposure.

## Settings Cleanup

Database migration V24 removes these provider-specific keys for all users,
both as individual rows and as fields in the `tunnel` settings object:

- `tunnel.cf_token`
- `tunnel.ngrok_token`
- `tunnel.ngrok_domain`
- `tunnel.ts_funnel`
- `tunnel.ts_hostname`

V24 intentionally keeps `tunnel.provider` as a migration marker. Replace or
clear the legacy value through setup or configuration. If a persisted value
blocks startup, use a temporary valid provider (including `none`) or a static
`TUNNEL_URL` override to reach the web settings. Both PostgreSQL and libSQL
receive the cleanup.

Remove matching environment variables and values from local JSON/TOML config:

```text
TUNNEL_CF_TOKEN
TUNNEL_NGROK_TOKEN
TUNNEL_NGROK_DOMAIN
TUNNEL_TS_FUNNEL
TUNNEL_TS_HOSTNAME
```

Migration cannot scrub old backups, exported settings, shell history, or
operator-managed vendor services. Handle those separately. Rollback to a
release containing removed providers requires credential re-entry; V24 does not
retain plaintext token backups.

V24 runs once per database. If an older release recreates legacy settings after
a rollback, delete those rows or values before re-upgrading; current CLI and web
settings APIs reject their reintroduction.

## Verify

1. Run `lunarwing doctor` with the same tunnel environment or disk settings.
   Static and custom checks validate configuration, not public reachability.
   Startup remains authoritative for DB-backed settings.
2. Start LunarWing and confirm boot output reports the expected HTTPS URL.
3. Test authenticated webhook and gateway routes through that URL.
4. Confirm retired vendor exposure is disabled when no longer required.

NetBird daemon enrollment and lifecycle remain operator-managed on both systemd
and OpenRC. See [NETBIRD_TUNNEL.md](NETBIRD_TUNNEL.md).
