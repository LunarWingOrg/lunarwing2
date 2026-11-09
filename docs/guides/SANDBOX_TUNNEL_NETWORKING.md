# Sandbox Tunnel Networking (NetBird / WireGuard)

`SANDBOX_NETWORK=tunnel` lets generic shell-sandbox commands make **outbound**
connections through a NetBird or WireGuard sidecar. It does not expose the web
gateway, open a host port, or allow overlay peers to connect into the sandbox.
Design and alternatives: `docs/proposals/PODMAN_SANDBOX_TUNNEL_NETWORKING.md`.

The default remains `none`. Existing `none` / `proxy-enforced` / `proxy`
behavior and all per-command sandbox hardening are unchanged.

## Tier selection

| `SANDBOX_NETWORK` | Intended use | Boundary |
|---|---|---|
| `none` (default) | no network | kernel-enforced `--network none` |
| `proxy-enforced` | allowlisted **public** HTTP(S) | network-none plus Unix relay to the validating proxy |
| `tunnel` | NetBird/WireGuard overlay egress | sidecar netns plus input-drop firewall; routes/ACLs govern outbound reachability |
| `proxy` | legacy public HTTP(S) | advisory env-var proxy; direct bypass remains possible |

A full-route tunnel sends outbound traffic through the overlay. A split tunnel
only routes its selected CIDRs; the sidecar's normal pasta/slirp uplink remains
available for other outbound destinations. In both cases new inbound flows are
dropped. Overlay membership is not authentication: keep TLS, tokens, mTLS, and
NetBird ACLs/WireGuard peer policy in force.

## Common requirements

- Linux with rootless Podman (primary) or Docker CLI fallback.
- A sidecar image containing `nft` (the `nftables` CLI) and the provider tools.
- One tunnel configuration per tenant. Use a separate, revocable NetBird setup
  key/peer or WireGuard peer for each tenant.
- The daemon validates configuration at startup and checks the provider and
  firewall before every command. A failure returns
  `[sandbox:sandbox_unavailable]`; it never falls back to another network mode.

The sidecar runs with all capabilities dropped except `NET_ADMIN`,
`no-new-privileges`, a read-only root filesystem, memory/process limits, no
published ports, and no host networking. Joined commands retain their existing
capability drop, non-root user/keep-id mapping, read-only root, mount masks,
resource limits, and cleanup.

## Configuration

```env
SANDBOX_NETWORK=tunnel
SANDBOX_TUNNEL_PROVIDER=netbird          # netbird (default) | wireguard
SANDBOX_TUNNEL_STARTUP_TIMEOUT_SECS=90   # must be greater than zero
```

`SANDBOX_TUNNEL_IMAGE` overrides the provider default. Pre-build or pre-pull the
image for the same tenant's container store. Every image must include
`nftables`; startup deliberately fails if it is absent.

## NetBird

```env
SANDBOX_NETWORK=tunnel
SANDBOX_TUNNEL_PROVIDER=netbird
SANDBOX_TUNNEL_NETBIRD_MANAGEMENT_URL=https://netbird.example.com
SANDBOX_TUNNEL_NETBIRD_SETUP_KEY=<dedicated-setup-key>
# Default: docker.io/netbirdio/netbird:latest
# Recommended once nftables is installed in a derivative image:
SANDBOX_TUNNEL_IMAGE=localhost/lunarwing-tunnel-netbird:latest
```

The management URL must be HTTPS and cannot contain userinfo, a query, or a
fragment. The setup key must be one nonempty token without whitespace.

The default is the upstream-compatible NetBird client image. If its version does
not ship `nft`, derive an image by installing `nftables` with that image's
package manager (for example `apk add --no-cache nftables` on Alpine, or the
`nftables` package on Debian) without replacing its `netbird` entrypoint. Pin the
result by digest or an operator-controlled tag for production. LunarWing checks
for the tool rather than assuming a package manager.

NetBird runs `up` with `NB_MANAGEMENT_URL` and `NB_SETUP_KEY` supplied through a
randomly named, atomically created mode-`0600` `--env-file`. The file is deleted
after the runtime start call; the key and URL never enter argv. Raw runtime
stderr is not copied into LunarWing errors, and the setup key is registered with
the daemon log scrubber. Do not print the env file or tenant env file while
debugging.

Readiness requires `netbird status --check startup` to succeed (management and
signal connected, plus relay availability when relays are configured). Optional
NetBird features that require `SYS_ADMIN` or other extra capabilities are not
enabled implicitly; use a configuration supported with `NET_ADMIN` only.

Use restrictive NetBird ACLs for defense-in-depth even though the local input
firewall drops inbound connections. Do not reuse the gateway-exposure NetBird
peer or setup key from `docs/guides/NETBIRD_TUNNEL.md`; that is a separate,
inbound gateway feature.

## WireGuard

```env
SANDBOX_NETWORK=tunnel
SANDBOX_TUNNEL_PROVIDER=wireguard
SANDBOX_TUNNEL_WIREGUARD_CONFIG=/etc/lunarwing/tenants/<tenant>/wg0.conf
SANDBOX_TUNNEL_IMAGE=localhost/lunarwing-tunnel-wireguard:latest
```

The config must be a regular, non-symlink file of at most 1 MiB, mode `0600`,
with a nonempty `PrivateKey` in `[Interface]`. It is mounted read-only at
`/etc/wireguard/wg0.conf`; its contents never enter argv or LunarWing logs.

A minimal Alpine sidecar image:

```dockerfile
FROM docker.io/library/alpine:latest
RUN apk add --no-cache bash iproute2 nftables wireguard-tools
```

```bash
podman build -t localhost/lunarwing-tunnel-wireguard:latest \
  <directory-containing-Containerfile>
```

The host kernel's WireGuard module must already be loaded (normally automatic).
Interface creation happens in the sidecar's user-namespace-owned netns, without
host capabilities or host route changes. Readiness requires `wg0` to exist;
actual peer reachability still depends on endpoint, keys, routes, and NAT.

Example peer config:

```ini
[Interface]
PrivateKey = <tenant-peer-private-key>
Address = 10.231.7.2/32

[Peer]
PublicKey = <relay-or-peer-public-key>
Endpoint = wg.example.com:51820
AllowedIPs = 10.231.0.0/16
PersistentKeepalive = 25
```

That example is a split tunnel. For full outbound routing use
`AllowedIPs = 0.0.0.0/0, ::/0` when the peer supports both families; using only
`0.0.0.0/0` leaves IPv6 outside the full-route policy. `wg-quick` policy routing,
MTU, DNS, and endpoint reachability remain operator responsibilities.

## Rotation and lifecycle

The daemon stores a SHA-256 configuration digest on the sidecar, not key
material. Changing provider, image, management URL, setup key, WireGuard path,
or WireGuard file contents causes recreation instead of stale reuse. Restart
the tenant after rotating the external peer/key and revoking the old one.

`podman ps --filter label=lunarwing.sandbox-tunnel` should show one sidecar for
the tenant. Daemon shutdown removes it. A crash, provider health failure,
configuration mismatch, missing `nft`, or altered firewall blocks subsequent
sandbox commands rather than reopening another egress path.

## Verification

1. Start the tenant and check that the daemon logs sidecar started/healthy,
   with no credential values.
2. Inspect the actual input policy from the trusted host:

   ```bash
   podman exec lunarwing-sandbox-tunnel \
     nft list chain inet lunarwing_sandbox_egress input
   ```

   It must show `policy drop`, loopback accept, and
   `ct state established,related accept`, with no other accept rules.
3. Ask the agent to run `cat /proc/net/dev`. The WireGuard `wg0` or NetBird
   interface is visible because only the network namespace is shared;
   `/etc/wireguard`, NetBird state, and sidecar credentials are not mounted.
4. For WireGuard's gated lifecycle test:

   ```bash
   cd ic
   LUNARWING_E2E_PODMAN_TESTS=1 LUNARWING_E2E_TUNNEL_TESTS=1 \
     taskset -c 0-17 cargo test -j18 --test podman_sandbox_tunnel_integration \
     -- --nocapture --test-threads=1
   ```

   The test builds an Alpine image with `nftables` unless
   `LUNARWING_E2E_TUNNEL_IMAGE` names a suitable existing image. It checks real
   namespace joining, stable container-ID reuse, and refusal after an extra
   inbound accept is injected. It does not test peer encryption or NetBird's
   control plane.
5. On a real NetBird/WireGuard overlay, verify an outbound connection from the
   sandbox to an allowed private service. Then run a temporary high-port
   listener in the sandbox and attempt to connect from another overlay peer:
   the inbound attempt must fail. Remove the temporary listener afterward.

## Troubleshooting

| Symptom | Check |
|---|---|
| `[sandbox:config]` at startup | provider name, required env values, HTTPS management URL, WireGuard file type/mode/`PrivateKey`, nonzero timeout |
| `sandbox_unavailable` mentioning `nftables` | install `nft` in the sidecar image; do not switch to direct/host networking |
| sidecar start rejected | image/entrypoint, rootless runtime, `/dev/net/tun` for NetBird, kernel WireGuard module, supported sysctls |
| NetBird never becomes ready | management URL and setup-key validity, signal/relay reachability, image version's `status --check startup` support |
| `wg0` exists but private service is unreachable | peer keys/endpoint/AllowedIPs, NAT keepalive, MTU, DNS, service authentication |
| command fails after a firewall change | restore the expected chain or restart the tenant; LunarWing intentionally refuses the run |
