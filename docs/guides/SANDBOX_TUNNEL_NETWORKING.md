# Sandbox Tunnel Networking (NetBird / WireGuard)

**Revalidated against source 2026-10-02 (branch `sandboxtunmode`, WI#248).**
Design record: [`docs/proposals/PODMAN_SANDBOX_TUNNEL_NETWORKING.md`](../proposals/PODMAN_SANDBOX_TUNNEL_NETWORKING.md).

`SANDBOX_NETWORK=tunnel` gives sandboxed shell commands egress through a VPN
overlay: a long-lived **tunnel sidecar** container (`lunarwing-sandbox-tunnel`)
establishes a NetBird or kernel-WireGuard tunnel inside its own network
namespace, and every sandbox run joins that namespace with
`--network container:lunarwing-sandbox-tunnel` — keeping all of its own
hardening (`--cap-drop=ALL`, read-only rootfs, non-root user, pids/memory
limits). Only the network stack is shared.

The sidecar is the only credential holder (setup key / private key);
sandboxed commands never see tunnel credentials. Lifecycle is daemon-owned:
health-gated start on sandbox init, stop on shutdown, config-hash recreation
when settings change, fail-closed errors (`[sandbox:sandbox_unavailable]`)
when the tunnel cannot come up.

## When to use which mode

| `SANDBOX_NETWORK` | Egress | Enforcement |
|---|---|---|
| `none` (default) | nothing | kernel-enforced |
| `proxy-enforced` | allowlisted public web via host proxy | enforced |
| `tunnel` | whatever the tunnel provides | tunnel-defined (see below) |
| `proxy` | allowlisted public web via env-var proxy | advisory (legacy) |

Tunnel egress is **enforced to the VPN** when the tunnel carries the default
route (WireGuard `AllowedIPs = 0.0.0.0/0`, or a NetBird 0.0.0.0/0 route).
With a split tunnel (`AllowedIPs = 10.0.0.0/8` only), the sidecar's pasta
uplink stays reachable, so treat the mode as advisory unless the default
route rides the tunnel. Application-layer auth (bearer tokens, TLS) is still
required on every reached service — the overlay is a transport, not a trust
boundary.

## Configuration

All knobs are environment (env-only, like the reaper knobs — mt-admin
renders tenant env files):

```env
SANDBOX_NETWORK=tunnel
SANDBOX_TUNNEL_PROVIDER=netbird          # netbird (default) | wireguard
```

### NetBird

```env
SANDBOX_TUNNEL_PROVIDER=netbird
SANDBOX_TUNNEL_NETBIRD_MANAGEMENT_URL=https://netbird.example.com
SANDBOX_TUNNEL_NETBIRD_SETUP_KEY=<setup-key>     # secret; mode-0600 handling
# optional overrides:
# SANDBOX_TUNNEL_IMAGE=docker.io/netbirdio/netbird:latest
# SANDBOX_TUNNEL_STARTUP_TIMEOUT_SECS=90
```

- Uses the upstream `netbirdio/netbird` image; the sidecar runs `netbird up`
  with the management URL and setup key delivered through a mode-0600
  `--env-file` (never the podman argv / systemd journal).
- Peer readiness gates sandbox availability: `netbird status --check
  startup` must succeed within `SANDBOX_TUNNEL_STARTUP_TIMEOUT_SECS`
  (default 90s).
- The setup key never enters LunarWing logs. Rotate the key by changing the
  env var and restarting — the config hash changes and the sidecar is
  recreated.
- NetBird ACLs decide which peers/services this tenant's sandbox can reach.
  Restrict to a dedicated group; do not grant network-wide access.
- The host-side NetBird daemon is **not** required for this mode — the
  sidecar is its own peer. If you also run the gateway-expose tunnel
  (`docs/guides/NETBIRD_TUNNEL.md`), use a separate setup key/peer for it.

### WireGuard

```env
SANDBOX_TUNNEL_PROVIDER=wireguard
SANDBOX_TUNNEL_WIREGUARD_CONFIG=/etc/lunarwing/tenants/<t>/wg0.conf
# optional overrides:
# SANDBOX_TUNNEL_IMAGE=localhost/lunarwing-tunnel-wireguard:latest
# SANDBOX_TUNNEL_STARTUP_TIMEOUT_SECS=90
```

- The config is a standard wg-quick format file and **must be mode 0600**
  (it holds the private key; the daemon refuses group/other-readable
  files). It is mounted read-only at `/etc/wireguard/wg0.conf`.
- Requires a sidecar image with `wg-quick`, `ip` (iproute2), `/bin/sh`, and
  `sleep`. Build once per host (see below) or point
  `SANDBOX_TUNNEL_IMAGE` at your own.
- Health gate: the `wg0` interface must exist within the startup timeout
  (no handshake needed for the gate; the peer endpoint converges on its
  own).
- Kernel `wireguard` module must be loaded on the host (`modprobe
  wireguard`, usually automatic). Creation of the interface inside the
  rootless container needs no host privilege — the container's user
  namespace owns its network namespace.

Minimal sidecar image:

```dockerfile
FROM docker.io/library/alpine:latest
RUN apk add --no-cache iproute2 wireguard-tools
```

```bash
podman build -t lunarwing-tunnel-wireguard:latest <dir-with-Containerfile>
```

Example sandbox-scoped `wg0.conf` (default route rides the tunnel):

```ini
[Interface]
PrivateKey = <tenant-peer-private-key>
Address = 10.231.7.2/32

[Peer]
PublicKey = <relay-or-peer-public-key>
Endpoint = wg.example.com:51820
AllowedIPs = 10.231.0.0/16    # split: VPN CIDRs only (pasta uplink stays reachable)
# AllowedIPs = 0.0.0.0/0     # full tunnel: ALL sandbox egress rides the tunnel
PersistentKeepalive = 25
```

## Verification

1. `lunarwing doctor` — config sanity.
2. Start the daemon; logs must show `Sandbox tunnel sidecar started` then
   `Sandbox tunnel sidecar healthy` for the tenant.
3. Ask the agent to run `ip -o link show wg0` (wireguard) or `netbird
   status --check startup` visibility via a reached private service — a
   joined sandbox sees the sidecar's interfaces.
4. `podman ps --filter label=lunarwing.sandbox-tunnel` shows exactly one
   running sidecar; it disappears on daemon shutdown.

| Failure | Check |
|---|---|
| `[sandbox:config]` at startup | missing provider env (`MANAGEMENT_URL`/`SETUP_KEY`/`WIREGUARD_CONFIG`), bad provider name, non-0600 wg conf |
| `[sandbox:sandbox_unavailable]` tunnel did not become healthy | sidecar image contents (`wg-quick`/`netbird`, `ip`), `/dev/net/tun` availability (netbird), kernel `wireguard` module (wireguard), peer/endpoint reachability, raise `STARTUP_TIMEOUT_SECS` |
| Setup key rejected | key expired/single-use in NetBird management; rotate and restart |
| `wg0` visible but no connectivity | peer endpoint/AllowedIPs mismatch, NAT (add `PersistentKeepalive`), MTU (try 1280–1420 on low-MTU paths) |

## Multi-tenant notes

- One sidecar per tenant user (container names are per-user podman stores);
  each tenant should have **its own** WireGuard peer or NetBird setup key so
  sandbox reachability is per-tenant and revocable.
- mt-admin preserves operator-set `SANDBOX_NETWORK` values on env re-render;
  `tunnel` behaves like any other non-default opt-in there.
- Revocation is external: rotate the peer/key (or ACL it in NetBird) and
  restart the tenant — the sidecar is recreated with the new credential.
