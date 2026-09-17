# Self-Hosted NetBird Tunnel

LunarWing can publish its local HTTP or gateway port through NetBird Reverse
Proxy by managing an ephemeral `netbird expose` process. NetBird management,
proxy infrastructure, peer enrollment, and daemon lifecycle remain
operator-managed.

This integration targets self-hosted NetBird. It does not require NetBird Cloud
or a LunarWing-side management API token.

**Revalidated against source 2026-09-17 (v2.1.2.0).** `netbird expose`
process management, `TUNNEL_NETBIRD_NAME_PREFIX`/`TUNNEL_NETBIRD_CUSTOM_DOMAIN`
env knobs, and the V24 legacy-provider scrub migration were re-checked against
`ic/src/tunnel/`, `ic/src/config/tunnel.rs`, and `ic/migrations/`; no changes
were needed.

## Scope

LunarWing performs these actions:

- Waits for `netbird status --check startup` to report a ready peer.
- Starts `netbird expose <port> --protocol http`.
- Reads the generated public HTTPS URL.
- Keeps the expose process alive for session renewal.
- Sends `SIGTERM` during normal shutdown so NetBird removes the service.
- On Linux, asks the kernel to send `SIGTERM` to the expose process if
  LunarWing exits unexpectedly.

LunarWing does not perform these actions:

- Install or start the NetBird daemon.
- Run `netbird up` or `netbird down`.
- Store a NetBird setup key or management token.
- Create proxy clusters, DNS records, or account permissions.

## NetBird Prerequisites

Use a current self-hosted NetBird release where both commands work:

```bash
netbird status --check startup
netbird expose --help
```

NetBird Reverse Proxy is currently beta. Self-hosted deployments require:

- Traefik with TLS passthrough in front of NetBird.
- NetBird Reverse Proxy instance connected to management service.
- Proxy base domain and wildcard DNS pointing to proxy host.
- Public TCP port 443 for TLS and ACME TLS-ALPN-01.
- Public TCP port 80 when using ACME HTTP-01 instead.
- Peer Expose enabled for a dedicated group containing LunarWing peer.
- Inbound blocking disabled on exposing peer.

Follow current upstream documentation:

- [NetBird Reverse Proxy](https://docs.netbird.io/manage/reverse-proxy)
- [Expose from CLI](https://docs.netbird.io/manage/reverse-proxy/expose-from-cli)
- [Enable Reverse Proxy on an existing self-hosted deployment](https://docs.netbird.io/selfhosted/migration/enable-reverse-proxy)

## Enroll Host

Install NetBird using distribution-appropriate packaging. Run its daemon under
host init system. Both systemd and OpenRC are supported operational models;
LunarWing never calls either init system directly.

Enroll host outside LunarWing:

```bash
netbird up \
  --management-url https://netbird.example.com \
  --setup-key '<setup-key>'
```

Protect setup key as a secret. Do not place it in LunarWing settings or service
environment. Confirm peer readiness:

```bash
netbird status --check startup
```

Enable Peer Expose in NetBird dashboard under **Settings > Clients**. Restrict
permission to a dedicated peer group instead of granting it network-wide.

## Migrating from Removed Managed Providers

LunarWing no longer manages Cloudflare Tunnel, ngrok, or Tailscale tunnel
processes. Choose NetBird, a custom command, or an externally managed static
`TUNNEL_URL`. Database migration V24 scrubs old provider-specific settings and
credentials. Follow [TUNNEL_PROVIDER_MIGRATION.md](TUNNEL_PROVIDER_MIGRATION.md)
before upgrading production webhook deployments.

## Configure LunarWing

Minimum configuration:

```env
TUNNEL_PROVIDER=netbird
TUNNEL_NETBIRD_NAME_PREFIX=lunarwing
```

For stable production callback and OAuth URLs, configure and verify a custom
domain in NetBird, then set:

```env
TUNNEL_NETBIRD_CUSTOM_DOMAIN=lunarwing.example.com
```

Without custom domain, NetBird creates a generated cluster-domain URL. It can
change after LunarWing restarts.

Name prefix must contain 1-32 lowercase letters, digits, or hyphens. It must
start and end with a letter or digit.

Static `TUNNEL_URL` still takes precedence over managed provider settings.

## Listener Requirements

`netbird expose` forwards to IPv4 loopback on selected port. LunarWing listener
must bind one of these hosts:

- `127.0.0.1`
- `localhost`
- `0.0.0.0`

Specific non-loopback binds and IPv6-only binds are rejected because NetBird
cannot reach them through current expose CLI target semantics.

## Security

NetBird expose URLs are public unless NetBird-layer authentication is added.
LunarWing does not add NetBird PIN, password, or SSO flags because those methods
break machine webhooks and OAuth callbacks.

Keep LunarWing route authentication enabled:

- Set `HTTP_WEBHOOK_SECRET` for HTTP webhooks.
- Set `GATEWAY_AUTH_TOKEN` for stable gateway authentication. Gateway generates
  a random token when omitted, but explicit persistent value is better for a
  remotely exposed service.
- Keep CORS, origin validation, body limits, and rate limits enabled.
- Do not trust arbitrary `X-Forwarded-For` headers.

Normal shutdown sends `SIGTERM` and waits seven seconds before force-killing.
On Linux, kernel parent-death signaling also sends `SIGTERM` if LunarWing exits
without running shutdown cleanup. Host crashes, power loss, or `SIGKILL` can
still leave NetBird's last published session reachable until NetBird expires
it; treat that lease duration as deployment-specific rather than relying on a
fixed TTL. Route authentication remains required during this window. A custom
domain can remain reserved until session expiry, so an immediate crash restart
may need to wait before exposing same domain again.

## Verify

Run diagnostics:

```bash
lunarwing doctor
```

Start LunarWing and confirm boot screen reports provider `netbird` plus HTTPS
URL. Test authenticated health or webhook route through that URL. Stop
LunarWing, then verify NetBird removes ephemeral service.

Common failures:

| Failure | Check |
|---|---|
| `netbird` not found | Service `PATH` includes NetBird client binary |
| Peer not ready | Daemon running and connected to self-hosted management URL |
| `netbird expose` unavailable | Upgrade NetBird client and self-hosted stack |
| Permission denied | Peer Expose enabled and peer belongs to allowed group |
| No cluster domain | Proxy instance connected and proxy domain/DNS configured |
| Certificate pending/failed | Wildcard DNS, Traefik TLS passthrough, ACME ports |
| Public URL returns connection error | LunarWing listener uses supported bind host and expected port |
