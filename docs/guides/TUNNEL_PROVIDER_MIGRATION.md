# Managed Tunnel Provider Migration

Since LunarWing v2.0.3, the managed providers are NetBird Reverse Proxy and
custom tunnel commands. Managed Cloudflare Tunnel, ngrok, and Tailscale providers were removed. An
externally managed service from any vendor remains supported through a static
HTTPS `TUNNEL_URL`.

**Revalidated against source 2026-09-17 (v2.1.2.0); re-checked 2026-09-27 (v2.1.3.0).** `TUNNEL_URL` precedence,
`TUNNEL_CUSTOM_COMMAND`, and the V24 settings scrub were re-checked against
`ic/src/config/tunnel.rs` and `ic/migrations/V24__remove_legacy_tunnel_settings.sql`;
no changes were needed.

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
```

Externally managed HTTPS endpoint:

```env
TUNNEL_URL=https://tunnel.example.com
```

`TUNNEL_URL` takes precedence over managed provider settings. Without a static
URL, a legacy `TUNNEL_PROVIDER=cloudflare`, `ngrok`, or `tailscale` value stops
startup with migration guidance instead of silently running without webhook
exposure.

## Settings Cleanup

Database migration V24 removes these persisted provider-specific rows for all
users:

- `tunnel.cf_token`
- `tunnel.ngrok_token`
- `tunnel.ngrok_domain`
- `tunnel.ts_funnel`
- `tunnel.ts_hostname`

V24 intentionally keeps `tunnel.provider` as a migration marker. Replace or
delete its legacy value through setup or CLI. Web settings can perform recovery
only when LunarWing starts through a temporary valid tunnel environment or
static `TUNNEL_URL` override. PostgreSQL and libSQL receive equivalent cleanup.

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

1. Run `lunarwing doctor` with the same tunnel environment or disk settings and
   confirm NetBird, custom, or static URL status. Startup remains authoritative
   for DB-backed settings.
2. Start LunarWing and confirm boot output reports expected HTTPS URL.
3. Test authenticated webhook and gateway routes through that URL.
4. Confirm retired vendor exposure is disabled when no longer required.

NetBird daemon enrollment and lifecycle remain operator-managed on both systemd
and OpenRC. See [NETBIRD_TUNNEL.md](NETBIRD_TUNNEL.md).
