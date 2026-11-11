# Sandbox Tunnel Networking (NetBird / WireGuard)

`SANDBOX_NETWORK=tunnel` lets generic shell-sandbox commands make **outbound**
connections through a NetBird or WireGuard sidecar. It does not expose the web
gateway, open a host port, or let overlay peers connect into the sandbox. Apart
from the default DNS allowance described below, it does not let a sandboxed
command reach the host, LAN, cloud metadata service, or public internet through
the sidecar's own uplink. Design and alternatives:
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
| `tunnel` | NetBird/WireGuard overlay egress | kernel-enforced nftables policy in the sidecar netns: new inbound dropped; outbound only through the tunnel interface, plus the tunnel's own marked control traffic and, by default, DNS to the sidecar's resolvers on the uplink; `SANDBOX_TUNNEL_DNS` moves joined-run DNS into the tunnel and removes that direct uplink allowance |
| `proxy` | legacy public HTTP(S) | advisory env-var proxy; direct bypass remains possible |

What a sandboxed command can reach in `tunnel` mode is exactly what the tunnel
routes, plus the default DNS residual. With a split tunnel (`AllowedIPs =
10.231.0.0/16`, or NetBird routes) that is those overlay CIDRs; with a
full-route tunnel (`AllowedIPs = 0.0.0.0/0, ::/0`, or a NetBird exit node) it
is everything, always through the overlay. The `output` chain drops every
packet that does not leave through `wg0`/`wt0`, is not permitted DNS on the
uplink, and does not carry the provider's socket mark. Joined commands have no
`NET_ADMIN`, so they cannot set that mark. Link-local IPv4 (`169.254.0.0/16`:
the cloud metadata service and podman's `host.containers.internal` alias) is
dropped even for marked traffic. New inbound flows are dropped from every
direction.

**Residual DNS channel:** by default, UDP/TCP port 53 to the runtime resolvers
is the one uplink channel open to joined commands. A recursive resolver can
carry query-based exfiltration even though arbitrary uplink connections are
dropped. Setting `SANDBOX_TUNNEL_DNS` installs overlay resolvers only in joined
runs and removes every direct uplink DNS accept. The sidecar itself keeps its
runtime resolvers so hostname endpoints and NetBird management/signal/relay
hosts still resolve before the tunnel exists. NetBird's local DNS listener and
whether its marked upstream forwarding leaves another path have not been
measured live; that residual and all live NetBird interoperability validation
remain explicitly open in WI#600. A skipped NetBird test is not a pass.

**Shared-loopback decision (WI#592):** concurrent commands of one daemon share
one network namespace and its loopback. A listener one command opens on
`127.0.0.1` is reachable by the others and ports can collide. This is accepted:
one daemon/tenant is one trust domain, while separate daemons now use separate
sidecars. Per-run netns/veth pairs or per-run enrollment would add routing and
credential/peer churn without improving the cross-tenant boundary. Do not run
mutually untrusted workloads in one tenant, and do not treat loopback listeners
as private between its commands. Overlay membership is not authentication:
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
# Optional comma-separated IPv4/IPv6 resolvers reachable through the tunnel:
# SANDBOX_TUNNEL_DNS=100.100.0.53,fd00::53
```

`SANDBOX_TUNNEL_IMAGE` overrides the provider default. Digest references use
`image@sha256:<64 hex characters>` and are recommended for registry images.
Mutable tags remain accepted for operator-controlled local images, but startup
logs a warning when auto-pull is enabled with a non-digest reference. NetBird's
default is digest-pinned; WireGuard's default remains the locally built
`localhost/lunarwing-tunnel-wireguard:latest`, with resolved-image-ID hashing
providing drift detection rather than registry immutability. Prefer a digest for
production overrides.

Pre-build or pre-pull the image for the same tenant's container store. Every
image must include `nftables`; startup deliberately fails if it is absent. The
daemon inspects the local image ID at every start and health check and includes
it in the drift hash, so re-pointing a tag recreates rather than reuses the
sidecar. A dedicated mt-admin `build-tunnel-image` step is intentionally not
added in WI#592: provider images are operator-specific and already have a
manual per-tenant build/load path, while `SANDBOX_AUTO_PULL=false` fails closed
if that step was skipped. On multi-tenant hosts `mt-admin` renders no tunnel
defaults and preserves every operator-set `SANDBOX_TUNNEL_*` value verbatim on
re-render (`docs/ops/TENANT-CONFIGURATION.md`).

DNS and canary addresses must be specified unicast IPs. Loopback, link-local,
broadcast, unspecified, and multicast addresses are rejected: none is a valid
overlay health target, and loopback would share a listener or fake canary with
other commands in the tenant namespace.

## NetBird

```env
SANDBOX_NETWORK=tunnel
SANDBOX_TUNNEL_PROVIDER=netbird
SANDBOX_TUNNEL_NETBIRD_MANAGEMENT_URL=https://netbird.example.com
SANDBOX_TUNNEL_NETBIRD_SETUP_KEY=<dedicated-setup-key>
# Default: NetBird 0.82.0, pinned to its OCI multi-architecture index:
SANDBOX_TUNNEL_IMAGE=docker.io/netbirdio/netbird@sha256:eca5b274c635bfbb04ec6724a0712d1146190dd450eea199eeb83916fdd94965
# Or pin your own derivative after installing nftables:
# SANDBOX_TUNNEL_IMAGE=localhost/lunarwing-tunnel-netbird@sha256:<64-hex-digest>
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
(`lunarwing-sandbox-tunnel-<instance>-netbird-<identity-hash>`). The salted
identity hash covers only the instance, management URL, and setup key. Its
32-byte identity salt is a regular, non-symlink mode-`0600` file at
`$LUNARWING_BASE_DIR/sandbox-tunnel-identity.salt`, separate from the ephemeral
label salt in the runtime directory. A daemon restart, host reboot, empty
runtime directory, image change, or DNS change therefore reuses the enrolled
peer instead of spending another setup key. Rotating the key or URL enrols a
fresh peer and prunes only this instance's previous volume; revoke the old peer
in NetBird afterwards. If the identity salt cannot be read or created, the
tunnel fails closed rather than falling back to a runtime or temporary path.

The uplink allowance relies on NetBird's advanced routing: every socket the
client opens is expected to carry its control-plane mark (`0x1bd00`), and that
mark is what the sidecar's `output` chain lets onto the uplink. Do not set
`NB_USE_LEGACY_ROUTING`, `NB_SKIP_SOCKET_MARK` or `NB_DISABLE_CUSTOM_ROUTING` in
the sidecar image or environment; if the client logs "falling back to legacy
routing", the firewall blocks its management connection and the health gate
fails closed.

Readiness requires `netbird status --check startup` to succeed (management and
signal connected, plus relay availability when relays are configured). Optional
NetBird features that require `SYS_ADMIN` or other extra capabilities are not
enabled implicitly; use a configuration supported with `NET_ADMIN` only. This
code path and its gated test lane have not yet been run against a real NetBird
management/signal/relay deployment; WI#600 tracks that validation and the local
DNS-forwarding measurement. Until then, NetBird interoperability is unproven,
not claimed as live-tested.

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
# Optional ICMP health canary for an outbound-only peer (requires ping):
# SANDBOX_TUNNEL_WIREGUARD_CANARY=10.231.0.1
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
host capabilities or host route changes. A peerless config is ready when `wg0`
exists (the lifecycle e2e shape). With peers, readiness requires a nonzero
WireGuard handshake no more than 180 seconds old, so an un-handshaked interface
fails fast instead of letting every command time out. Without a canary, a peered
config is valid only when at least one peer sets `PersistentKeepalive` from 1
through 60 seconds; this guarantees initial and continuing liveness traffic.
Alternatively set `SANDBOX_TUNNEL_WIREGUARD_CANARY` to an ICMP-reachable overlay
address. Its probe generates handshake traffic and is repeated after the egress
firewall is installed, so a response through the host uplink cannot make the
sidecar healthy. Application-level reachability still depends on routes and
services.

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

That example is a split tunnel: apart from the default DNS residual, joined
commands reach `10.231.0.0/16` only. For full outbound routing use `AllowedIPs
= 0.0.0.0/0, ::/0` when the peer supports both families; using only `0.0.0.0/0`
leaves IPv6 dropped rather than routed. `wg-quick` policy routing, MTU,
endpoint reachability and DNS remain operator responsibilities: `DNS =` in the
config needs `openresolv` in the image. Without `SANDBOX_TUNNEL_DNS`, joined
commands resolve names through the sidecar's runtime resolvers over the uplink
(port 53 only).

Setting `SANDBOX_TUNNEL_DNS` does **not** pass `--dns` to the sidecar: provider
bootstrap and hostname endpoints continue using runtime DNS. Instead, LunarWing
atomically installs a non-secret mode-`0644` resolver file in its private
runtime directory. Every joined run mounts it at `/etc/resolv.conf` with `ro,z`
because Podman and Docker reject `--dns` together with `--network container`;
the shared SELinux label is safe for concurrent runs and the mode works with
all supported keep-id mappings. The file is refreshed on start and every health
check and removed by `SandboxManager::shutdown()`. All direct uplink DNS rules
are removed; use resolvers reachable through `wg0`, including for joined-run
name resolution after reconnection.

## Rotation and lifecycle

The daemon stores a salted SHA-256 configuration digest on the sidecar, not key
material; the salt is a random mode-0600 file in the daemon's runtime
directory. Changing provider, image reference, resolved image ID, DNS, management
URL, setup key, WireGuard path, or WireGuard file contents causes recreation
instead of stale reuse. Restart the tenant after rotating the external peer/key
and revoking the old one.

The container name is `lunarwing-sandbox-tunnel-<instance>`, where the suffix is
derived from `LUNARWING_BASE_DIR`. An instance ownership label prevents one
daemon from removing another daemon's sidecar, even when two daemons share a
uid. `podman ps --filter label=lunarwing.sandbox-tunnel` should show one sidecar
per daemon instance. Startup and recreation are serialized per instance, and a
run joins the exact container ID that passed health and firewall verification;
if recreation removes that ID first, the run fails instead of joining a new,
not-yet-filtered namespace.

`SandboxManager::shutdown()` removes only its own sidecar. On systemd hosts it
then frees the deterministic `lunarwing-tunnel-<instance>` transient helper
unit: once the runtime reports the container gone, it waits up to three seconds
for pasta and conmon to exit, and sends anything still in the unit SIGTERM and
then SIGKILL (`systemctl --user kill`). It never signals the helpers of a
container that still exists, for example when removal failed because a joined
run is in flight. A new sidecar reuses the unit name, so a start that finds the
unit still loaded fails closed and names the unit; the manual remedy is
`systemctl --user kill lunarwing-tunnel-<instance>.service`. The normal daemon
exit path currently does **not** call
`SandboxManager::shutdown()`, so process exit can leave the sidecar running for
adoption on the next start; do not claim or rely on automatic daemon-exit
cleanup. Remove the instance container explicitly when that is not wanted. A
crash, provider health failure, configuration mismatch, missing `nft`, or
altered firewall blocks subsequent sandbox commands rather than reopening
another egress path; the next command recreates a dead or drifted sidecar once,
and refuses to run if that fails.

### Upgrade from the pre-WI#592 fixed name

The old fixed `lunarwing-sandbox-tunnel` container and
`lunarwing-sandbox-tunnel-netbird-*` volumes are not removed automatically.
After stopping the old daemon, run these as the affected tenant's container-store
user, then revoke the old NetBird peer in the management dashboard:

```bash
podman rm -f lunarwing-sandbox-tunnel
podman volume ls --filter name=lunarwing-sandbox-tunnel-netbird-
# Review the listed legacy names, then remove each one:
podman volume rm <legacy-volume-name>
```

The new instance-scoped names do not match that legacy volume prefix. Keep the
new peer's state volume unless deliberate re-enrollment is required.

### One-time NetBird re-enrolment for the WI#602 identity-salt fix

Releases before WI#602 derived the peer-volume name with a salt in the runtime
directory, so a reboot changed the name and lost the enrolled identity. The
first NetBird sidecar start after upgrading to the persistent identity salt
computes a new volume name once and prunes the previous instance-scoped volume.
It therefore re-enrols once: use a reusable setup key that is still valid, or
issue a replacement before upgrading, and revoke the old peer afterwards. Do
not try to adopt the old volume by renaming it or copying salts. Later daemon
restarts and host reboots retain the new volume and overlay identity as long as
the instance, management URL, setup key, and persistent salt stay unchanged.

## Verification

1. Start the tenant and check that the daemon logs sidecar started/healthy,
   with no credential values.
2. Inspect both chains from the trusted host:

   ```bash
   podman exec lunarwing-sandbox-tunnel-<instance> \
     nft list table inet lunarwing_sandbox_egress
   ```

   `input` must show `policy drop`, loopback accept and
   `ct state established,related accept`. `output` must show `policy drop`,
   accepts for `lo`, `wg0` and `wt0`, the three neighbour-discovery ICMPv6
   accepts, `ip daddr 169.254.0.0/16 drop`, and one `meta mark … accept`.
   Without `SANDBOX_TUNNEL_DNS`, there are also `udp`/`tcp dport 53` accepts
   for each runtime resolver; with it, no uplink DNS accept is present.
   Anything else makes the daemon refuse every run.
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
   namespace joining by verified container ID, stable reuse, clean recreation
   after independent input/output tamper cases, image-ID rotation with a clean
   firewall, distinct instance names, and helper-unit cleanup. Custom DNS checks
   the resolver-file marker, read-only mount, and refusal of queries to the old
   uplink resolver. A peered hostname-endpoint case proves bootstrap DNS works,
   then requires the unreachable peer to fail the handshake gate. The peerless
   config still does not test peer encryption or NetBird's control plane.
5. The NetBird lane requires an isolated throwaway management deployment, one
   dedicated reusable setup key, and an image containing NetBird, `nft`, `nc`,
   and `wget`. The lane starts its own plain second peer with that key. Store
   the key in a mode-`0600` file and do not print it or the tenant env file:

   ```bash
   LUNARWING_E2E_PODMAN_TESTS=1 LUNARWING_E2E_TUNNEL_TESTS=1 \
   LUNARWING_E2E_NETBIRD_TESTS=1 \
   LUNARWING_E2E_NETBIRD_IMAGE=<netbird-image> \
   LUNARWING_E2E_NETBIRD_MANAGEMENT_URL=https://nbtest.example.org \
   LUNARWING_E2E_NETBIRD_SETUP_KEY_FILE="$HOME/.nbtest-setup-key" \
     taskset -c 0-11 cargo test -j12 --test podman_sandbox_tunnel_integration \
     podman_tunnel_sidecar_netbird_live -- --exact --nocapture --test-threads=1
   ```

   It checks startup and detailed peer connectivity, a real joined run with
   `wt0`, exactly one `0x1bd00` mark accept, management reconnect, and ingress
   refusal. Its identity check stops the sidecar, deletes the runtime-directory
   label salt to model a reboot, and requires the same peer IP and state volume
   after recreation. The ingress check
   proves the listener is reachable over loopback before and after the peer
   attempt, so a dead listener cannot masquerade as a firewall pass. The lane
   never runs without all three gates and provider settings; a skip is not a
   live-validation pass. No live NetBird run has been performed for this item.
   WI#600 owns that run, the local-DNS/uplink-capture check, teardown evidence,
   and any future podman-capable CI job with protected masked credentials;
   neither provider lane currently runs in CI.
6. On a real NetBird/WireGuard overlay, verify an outbound connection from the
   sandbox to an allowed private service. Then run a temporary high-port
   listener in the sandbox and attempt to connect from another overlay peer:
   the inbound attempt must fail. Remove the temporary listener afterward.

## Troubleshooting

| Symptom | Check |
|---|---|
| `[sandbox:config]` at startup | provider name, required env values, HTTPS management URL, WireGuard file type/mode/`PrivateKey`, nonzero timeout, peer keepalive of 1–60 seconds or a canary, and non-local unicast DNS/canary addresses |
| `sandbox_unavailable` mentioning `nftables` | install `nft` in the sidecar image; do not switch to direct/host networking |
| `sandbox_unavailable` mentioning the image | `SANDBOX_AUTO_PULL=false` and the image is not in the tenant's store; build or load it there |
| `sandbox_unavailable` mentioning the runtime directory | `XDG_RUNTIME_DIR` is unset or not owned by the daemon user; the credential file and label salt are never placed in a shared `/tmp` |
| sidecar start rejected | image/entrypoint, rootless runtime, `/dev/net/tun` for NetBird, kernel WireGuard module, supported sysctls |
| NetBird never becomes ready | management URL and setup-key validity, signal/relay reachability, image version's `status --check startup` support |
| NetBird connects, then loses management after a while | the client fell back to legacy routing (no socket marks): check its log and remove `NB_USE_LEGACY_ROUTING` / `NB_SKIP_SOCKET_MARK` |
| names do not resolve in the sandbox | with default DNS, check the sidecar's runtime resolvers; with `SANDBOX_TUNNEL_DNS`, check the joined run's mounted file and overlay resolver reachability |
| `wg0` exists but private service is unreachable | peer keys/endpoint/AllowedIPs, NAT keepalive, MTU, DNS, service authentication |
| command fails after a firewall change | the daemon recreates a drifted sidecar once; if the drift returns, find what edits the table (another agent, a host tool) and stop it |
