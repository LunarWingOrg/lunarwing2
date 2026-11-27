# Managed Tunnel Provider Migration

Since LunarWing v2.0.3.0, the managed providers are NetBird Reverse Proxy and
custom tunnel commands. Managed Cloudflare Tunnel, ngrok, and Tailscale providers were removed. An
externally managed service from any vendor remains supported through a static
HTTPS `TUNNEL_URL`.

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

Set a pattern and have the command print its public HTTPS URL on stdout, on
a line of its own. The discovery code takes one URL from each line and tests
it for the substring. The URL extractor accepts both `https://` and `http://`.
If the pattern is unset or no URL matches during discovery, the provider
reports the local HTTP target. LunarWing substitutes the placeholders and
splits the command on whitespace before executing it directly.

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

1. Run `lunarwing doctor` with the same tunnel environment. Its file-based
   fallback is legacy `settings.json`, not `config.toml`; it does not resolve
   the daemon's TOML overlay or database settings. Static and custom checks
   validate configuration, not public reachability. Startup remains
   authoritative for TOML and DB-backed settings.
2. With CLI enabled and no `--message`, confirm the interactive boot screen
   reports the expected HTTPS URL. Shipped services and managed tenants set
   `CLI_ENABLED=false`, so they do not print that screen: inspect startup logs
   for failures and verify the configured provider's public endpoint instead.
3. Check the public endpoint, then test a protected route such as
   `GET /api/gateway/status` with the gateway bearer token or send a test
   webhook with its configured secret.
4. Confirm retired vendor exposure is disabled when no longer required.

NetBird daemon enrollment and lifecycle remain operator-managed on both systemd
and OpenRC. See [NETBIRD_TUNNEL.md](NETBIRD_TUNNEL.md).
