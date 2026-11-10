# Sandbox Tunnel Networking (NetBird / WireGuard)

`SANDBOX_NETWORK=tunnel` lets generic shell-sandbox commands make **outbound**
connections through a NetBird or WireGuard sidecar, and nothing else. It does
not expose the web gateway, open a host port, or let overlay peers connect
into the sandbox, and it does not let a sandboxed command reach the host, the
LAN, the cloud metadata service or the public internet through the sidecar's
own uplink. Design and alternatives:
`docs/proposals/PODMAN_SANDBOX_TUNNEL_NETWORKING.md`.

WI#537 selects `proxy-enforced` as the Linux default; other platforms default
to `none`, and `tunnel` remains an explicit opt-in. Existing `none` /
`proxy-enforced` / `proxy` behavior and all per-command sandbox hardening are
unchanged, and mt-admin preserves every operator-set tier on re-render.

## Tier selection

| `SANDBOX_NETWORK` | Intended use | Boundary |
|---|---|---|
| `none` (default off Linux) | no network | kernel-enforced `--network none` |
| `proxy-enforced` (Linux default) | allowlisted **public** HTTP(S) | network-none plus Unix relay to the validating proxy; no credential injection |
| `tunnel` | NetBird/WireGuard overlay egress | kernel-enforced nftables policy in the sidecar netns: new inbound dropped; outbound only through the tunnel interface, plus the tunnel's own marked control traffic and DNS to the sidecar's resolvers on the uplink |
| `proxy` | legacy public HTTP(S) | advisory env-var proxy; direct bypass remains possible |

What a sandboxed command can reach in `tunnel` mode is exactly what the tunnel
routes. With a split tunnel (`AllowedIPs = 10.231.0.0/16`, or NetBird routes)
that is those overlay CIDRs; with a full-route tunnel (`AllowedIPs =
0.0.0.0/0, ::/0`, or a NetBird exit node) it is everything, always through the
overlay. The sidecar's own pasta/slirp uplink is closed to joined commands:
the `output` chain drops every packet that does not leave through `wg0`/`wt0`,
is not DNS to the resolvers the runtime handed the sidecar, and does not carry
the provider's socket mark. Joined commands have no `NET_ADMIN`, so they cannot
set that mark. Link-local IPv4 (`169.254.0.0/16`: the cloud metadata service
and podman's `host.containers.internal` alias) is dropped even for marked
traffic. New inbound flows are dropped from every direction.

Residuals: a sandboxed command can still send DNS queries to the sidecar's
resolvers (a low-bandwidth channel), and every concurrent command of the
tenant shares one network namespace, so a listener one command opens on
loopback is reachable by the others. Overlay membership is not authentication:
keep TLS, tokens, mTLS and NetBird ACLs/WireGuard peer policy in force.

## Common requirements

- Linux with rootless Podman (primary) or Docker CLI fallback.
- A sidecar image containing `nft` (the `nftables` CLI) and the provider tools.
- The `nf_tables`, `nf_conntrack` and (for WireGuard) `wireguard` kernel
  modules loaded on the host; rootless containers cannot load modules.
- One tunnel configuration per tenant. Use a separate, revocable NetBird setup
  key/peer or WireGuard peer for each tenant.
- With `SANDBOX_AUTO_PULL=false` (the multi-tenant default) the sidecar image
  must already be in the tenant's container store; the start fails closed
  instead of pulling a mutable tag.
- The daemon validates configuration at startup and checks the provider and
  both firewall chains, rule by rule, before every command. A failure returns
  `[sandbox:sandbox_unavailable]`; it never falls back to another network
  mode. A sidecar that died or drifted is recreated once before the command is
  refused.

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
`nftables`; startup deliberately fails if it is absent. On multi-tenant hosts
`mt-admin` renders no tunnel defaults and preserves every operator-set
`SANDBOX_TUNNEL_*` value verbatim on re-render (`docs/ops/TENANT-CONFIGURATION.md`).

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

The image's entrypoint starts the NetBird daemon and runs `netbird up` with
`NB_MANAGEMENT_URL` and `NB_SETUP_KEY` supplied through a randomly named,
atomically created mode-`0600` `--env-file` in the daemon's private runtime
directory (together with `NB_LOG_FILE=console`, because the read-only rootfs
has no writable `/var/log`). The file is deleted after the runtime start call;
the key and URL never enter argv. Raw runtime stderr is not copied into
LunarWing errors, and the setup key is registered with the daemon log scrubber.
Do not print the env file or tenant env file while debugging.

Peer identity and the client config live in a named volume
(`lunarwing-sandbox-tunnel-netbird-<hash>`), so a daemon restart reuses the
enrolled peer instead of spending another setup key. Rotating the key or URL
changes the hash, enrols a fresh peer and prunes the previous volume; revoke the
old peer in NetBird afterwards.

The uplink allowance relies on NetBird's advanced routing: every socket the
client opens carries its control-plane mark (`0x1bd00`), and that mark is what
the sidecar's `output` chain lets onto the uplink. Do not set
`NB_USE_LEGACY_ROUTING`, `NB_SKIP_SOCKET_MARK` or `NB_DISABLE_CUSTOM_ROUTING` in
the sidecar image or environment; if the client logs "falling back to legacy
routing", the firewall blocks its management connection and the health gate
fails closed.

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
with a nonempty `PrivateKey` in `[Interface]`. It is mounted read-only with a
private SELinux label at `/etc/wireguard/wg0.conf`; its contents never enter
argv or LunarWing logs.

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

After `wg-quick up` the daemon reads the device fwmark: a config that set none
gets `0x4c57`; a full-route config keeps the mark wg-quick assigned (its
routing table number). Encapsulated WireGuard packets carry that mark and are
the only WireGuard traffic the `output` chain lets onto the uplink. The sidecar
is created with `net.ipv4.conf.all.src_valid_mark=1`, which wg-quick's
full-route path needs and cannot set itself in a container.

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

That example is a split tunnel: joined commands reach `10.231.0.0/16` and
nothing else. For full outbound routing use `AllowedIPs = 0.0.0.0/0, ::/0`
when the peer supports both families; using only `0.0.0.0/0` leaves IPv6
dropped rather than routed. `wg-quick` policy routing, MTU, endpoint
reachability and DNS remain operator responsibilities: `DNS =` in the config
needs `openresolv` in the image, and without it joined commands resolve names
through the sidecar's runtime resolvers over the uplink (port 53 only).

## Rotation and lifecycle

The daemon stores a salted SHA-256 configuration digest on the sidecar, not key
material; the salt is a random mode-0600 file in the daemon's runtime
directory. Changing provider, image, management URL, setup key, WireGuard path,
or WireGuard file contents causes recreation instead of stale reuse. Restart
the tenant after rotating the external peer/key and revoking the old one.

`podman ps --filter label=lunarwing.sandbox-tunnel` should show one sidecar for
the tenant. Daemon shutdown removes it. A crash, provider health failure,
configuration mismatch, missing `nft`, or altered firewall blocks subsequent
sandbox commands rather than reopening another egress path; the next command
recreates a dead or drifted sidecar once, and refuses to run if that fails.

## Verification

1. Start the tenant and check that the daemon logs sidecar started/healthy,
   with no credential values.
2. Inspect both chains from the trusted host:

   ```bash
   podman exec lunarwing-sandbox-tunnel \
     nft list table inet lunarwing_sandbox_egress
   ```

   `input` must show `policy drop`, loopback accept and
   `ct state established,related accept`. `output` must show `policy drop`,
   accepts for `lo`, `wg0` and `wt0`, the three neighbour-discovery ICMPv6
   accepts, `udp`/`tcp dport 53` accepts for each resolver in the sidecar's
   `/etc/resolv.conf`, `ip daddr 169.254.0.0/16 drop`, and one
   `meta mark … accept`. Anything else makes the daemon refuse every run.
3. Ask the agent to run `cat /proc/net/dev`: the WireGuard `wg0` or NetBird
   `wt0` interface is visible because only the network namespace is shared;
   `/etc/wireguard`, NetBird state, and sidecar credentials are not mounted.
   Then ask it to run `wget -T 3 http://169.254.169.254/` and
   `wget -T 3 http://1.1.1.1/`: both must fail.
4. For WireGuard's gated lifecycle test:

   ```bash
   cd ic
   LUNARWING_E2E_PODMAN_TESTS=1 LUNARWING_E2E_TUNNEL_TESTS=1 \
     taskset -c 0-11 cargo test -j12 --test podman_sandbox_tunnel_integration \
     -- --nocapture --test-threads=1
   ```

   The test builds an Alpine image with `nftables` unless
   `LUNARWING_E2E_TUNNEL_IMAGE` names a suitable existing image. It checks real
   namespace joining, stable container-ID reuse, that a joined command reaches
   neither the public internet, the metadata service nor the host alias while
   DNS still works, refusal after an extra accept is injected into either
   chain, and recreation of a drifted sidecar. It does not test peer
   encryption or NetBird's control plane.
5. On a real NetBird/WireGuard overlay, verify an outbound connection from the
   sandbox to an allowed private service. Then run a temporary high-port
   listener in the sandbox and attempt to connect from another overlay peer:
   the inbound attempt must fail. Remove the temporary listener afterward.

## Troubleshooting

| Symptom | Check |
|---|---|
| `[sandbox:config]` at startup | provider name, required env values, HTTPS management URL, WireGuard file type/mode/`PrivateKey`, nonzero timeout |
| `sandbox_unavailable` mentioning `nftables` | install `nft` in the sidecar image; do not switch to direct/host networking |
| `sandbox_unavailable` mentioning the image | `SANDBOX_AUTO_PULL=false` and the image is not in the tenant's store; build or load it there |
| `sandbox_unavailable` mentioning the runtime directory | `XDG_RUNTIME_DIR` is unset or not owned by the daemon user; the credential file and label salt are never placed in a shared `/tmp` |
| sidecar start rejected | image/entrypoint, rootless runtime, `/dev/net/tun` for NetBird, kernel WireGuard module, supported sysctls |
| NetBird never becomes ready | management URL and setup-key validity, signal/relay reachability, image version's `status --check startup` support |
| NetBird connects, then loses management after a while | the client fell back to legacy routing (no socket marks): check its log and remove `NB_USE_LEGACY_ROUTING` / `NB_SKIP_SOCKET_MARK` |
| names do not resolve in the sandbox | the sidecar's `/etc/resolv.conf` lists no reachable nameserver; DNS is allowed only to those addresses |
| `wg0` exists but private service is unreachable | peer keys/endpoint/AllowedIPs, NAT keepalive, MTU, DNS, service authentication |
| command fails after a firewall change | the daemon recreates a drifted sidecar once; if the drift returns, find what edits the table (another agent, a host tool) and stop it |
