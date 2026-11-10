# Podman Sandbox Tunnel Networking (NetBird + WireGuard)

**Status:** IMPLEMENTED on `sweetiebot/kimi-3-20261109-0012` (WI#248)
**Lineage:** port of the maintainer's `sandboxtunmode` branch (`2ed3d0395`, !1336,
reviewed in note 5824) onto the v2.2.0.0 head, plus the egress-only and
lifecycle hardening described below. Related design
history: WI#144 and the overlay-as-transport discussion on WI#153.
**Operator guide:** `docs/guides/SANDBOX_TUNNEL_NETWORKING.md`

## Problem

The generic shell sandbox supports public-web egress through an allowlist proxy,
but has no supported way to reach private services on a NetBird or WireGuard
overlay. `none` blocks all egress; `proxy-enforced` deliberately rejects private
addresses; legacy `proxy` is advisory and also public-only. This item adds an
opt-in overlay tier without changing the default or the external-worker lanes.

## Design

`SANDBOX_NETWORK=tunnel` starts one long-lived sidecar per daemon instance.
The sidecar establishes NetBird or kernel WireGuard inside its own network
namespace. Its name is `lunarwing-sandbox-tunnel-<instance>`, with the suffix
derived from `LUNARWING_BASE_DIR`, and an instance label allows cleanup only by
the owning daemon. Each sandboxed command joins that exact namespace with
`--network container:lunarwing-sandbox-tunnel-<instance>`.

```text
lunarwing-sandbox-tunnel-<instance> (daemon-owned, long-lived)
  NetBird `up` or `wg-quick up`
  own netns, no published ports, no host networking
  CAP_DROP=ALL + CAP_NET_ADMIN, no-new-privileges, read-only rootfs
  nft input policy=drop; loopback + established/related return traffic only
                         │ shares network namespace only
lunarwing-sandbox-<uuid> (one command)
  CAP_DROP=ALL + CAP_CHOWN, no-new-privileges, read-only rootfs
  existing keep-id/user, pids/memory/timeout, mount guards and cleanup unchanged
```

The generic sandbox boundary remains unchanged. This code is under
`src/sandbox/`; it is not connected to orchestrator workers, worker exposure, or
`src/tunnel/` (which publishes the gateway inbound).

### Egress only, in both directions

Sharing a network namespace without a firewall has two gaps: a sandboxed
process could bind a port reachable through the tunnel or the sidecar uplink,
and it could use the sidecar's uplink to reach the public internet, the LAN,
host loopback services (`host.containers.internal`) and the cloud metadata
service, which is the WI#154 scenario that made `none` the default. The
original `sandboxtunmode` branch documented the second gap as "advisory unless
the default route rides the tunnel"; the review of !1336 (note 5824, T1)
reproduced it. This implementation requires `nftables` in every sidecar image
and installs an `inet lunarwing_sandbox_egress` table with two base chains:

- `input`, policy `drop`: loopback and `established,related` return traffic
  only, so nothing inside the namespace can accept a new inbound flow.
- `output`, policy `drop`: loopback; the tunnel interfaces `wg0` and `wt0`;
  IPv6 neighbour discovery; by default, DNS (`udp`/`tcp` 53) to the resolvers
  in the sidecar's `/etc/resolv.conf`; an explicit drop of `169.254.0.0/16`;
  and, last, packets carrying the provider's socket mark. Everything else is
  dropped. Setting `SANDBOX_TUNNEL_DNS` passes `--dns` to the sidecar, mounts an
  atomically replaced owner-only resolver file read-only into every joined run
  (runtimes reject `--dns` with `--network container`), and removes all uplink
  DNS rules; the configured resolvers must be reachable through the tunnel.

The mark is how the tunnel's own control plane keeps using the uplink without
an address allowlist that would have to track management, signal, relay and
STUN hosts: the NetBird client sets `0x1bd00` on every socket it opens
(advanced routing, the Linux default with `NET_ADMIN`), and a WireGuard
device stamps its encapsulated packets with its fwmark (the sidecar sets
`0x4c57` on a device whose config left it off and keeps wg-quick's full-route
mark otherwise). Setting a socket mark needs `NET_ADMIN`, which joiners do not
have, so a joined command can only leave through the tunnel interface: a split
tunnel reaches its overlay CIDRs and nothing else, a full-route tunnel reaches
everything through the overlay. UID- and cgroup-based allowances were
rejected: under rootless podman the sidecar's root and the keep-id sandbox
user map to the same host uid, and cgroup paths differ between cgroup
managers and runtimes.

Joiner containers have no `NET_ADMIN`, so their processes cannot alter either
chain. There are no `-p`/`--publish` arguments, no host-network join, no port
forward, and no LunarWing listener. The provider is brought up first (it
resolves and reaches its endpoints through the still-open uplink while no
joiner exists), then the table is installed and verified; before every run the
listing is parsed and compared rule by rule with the expected policy. Missing
`nft`, an altered or missing chain, an extra accept (with or without a
comment) or a changed policy fails closed. Without custom DNS, the runtime
resolvers remain a low-bandwidth uplink channel; `SANDBOX_TUNNEL_DNS` closes it.

**Shared-loopback decision (WI#592):** concurrent commands of one daemon share
loopback, listeners, and port space. This is accepted because one daemon/tenant
is one trust domain; separate daemons use separate sidecars. Per-run netns/veth
pairs or per-run enrollment were rejected as routing/capability complexity and
credential/peer churn that would not strengthen the cross-tenant boundary. Do
not schedule mutually untrusted workloads in one tenant. NetBird ACLs and
WireGuard peer policy remain separate defense-in-depth; overlay membership is
not application auth.

### Lifecycle and failure posture

- Startup validates provider settings and resolves the local image ID with
  `<runtime> image inspect --format '{{.Id}}'`; that ID is part of the drift
  hash on start and every health check. NetBird's default is the digest-pinned
  0.82.0 OCI index, and `image@sha256:<64 hex>` overrides are accepted. With
  `SANDBOX_AUTO_PULL=false` the image must already be present locally.
- NetBird readiness is `netbird status --check startup`. Peerless WireGuard
  readiness is `wg0` presence; with peers it is a nonzero handshake at most 180
  seconds old, or a successful `SANDBOX_TUNNEL_WIREGUARD_CANARY` ICMP probe
  through the overlay. The firewall is installed after readiness. Probes, start
  and cleanup are bounded.
- `SandboxManager` health-gates initialization and calls `ensure_healthy()`
  before every container run. Provider failure, a dead sidecar, hash drift, or
  firewall drift returns `SandboxError::TunnelUnavailable`, mapped to the stable
  `[sandbox:sandbox_unavailable]` envelope, after one bounded attempt to
  recreate the sidecar. There is no fallback to `proxy`, `proxy-enforced`,
  direct egress, or host execution.
- The configuration label is a salted SHA-256 over provider, image reference,
  resolved image ID, DNS, canary, management URL, setup key, WireGuard path and
  file contents; the salt is a random 0600 file in the private runtime
  directory, so the label is not a plain digest of key material.
  Credential/content/image rotation recreates the sidecar.
- NetBird's peer identity and config live in an instance-scoped named volume
  keyed by the label hash, so a restart reuses the enrolled peer instead of
  spending another setup key; a rotation enrols a fresh peer and prunes only
  that instance's previous volume.
- `shutdown()` removes only the sidecar carrying this instance's ownership
  label (podman `rm -f -t`, Docker `stop` then `rm -f`). Its label namespace is
  separate from the one-shot `lunarwing.sandbox` reaper label.

### Credential handling

NetBird management URL and setup key are staged in a randomly named env file
created atomically with mode `0600`, passed by `--env-file`, and deleted after
the runtime start call. The key is also registered with LunarWing's log
scrubber. Neither value enters argv. WireGuard requires a regular, non-symlink,
owner-only (`0600`) wg-quick file with a non-empty `[Interface] PrivateKey`; it
is mounted read-only. LunarWing validates structure without printing contents.
Raw sidecar/runtime stderr is not copied into LunarWing errors, preventing a
runtime diagnostic from echoing credentials into the journal.

Sandbox joiners share no filesystem or environment with the sidecar and never
receive the setup key or private key.

## Network-tier placement

| Mode | Purpose | Enforcement |
|------|---------|-------------|
| `none` (default off Linux) | no egress | kernel `--network none` |
| `proxy-enforced` (WI#537 Linux default) | public web through domain allowlist | network-none + Unix relay + public-only proxy; no credential injection |
| `tunnel` | private overlay and optionally full-route egress | kernel-enforced sidecar input + output chains; reachability = the tunnel's routes/ACLs |
| `proxy` (legacy) | public web through env-var proxy | advisory only |

A full-route WireGuard config (`AllowedIPs = 0.0.0.0/0`, plus IPv6 where used) or
NetBird default route sends all outbound traffic through the overlay. A split
tunnel reaches only its overlay CIDRs: the sidecar's pasta/slirp uplink is
closed to joined commands by the output chain in both cases, except by default
for DNS to the runtime resolvers. `SANDBOX_TUNNEL_DNS` removes that exception.
Neither shape is equivalent to the public-only proxy
allowlist; what is reachable is decided by the tunnel's routes and the overlay's
ACLs, never by the host's network position.

Item 4 (WI#537) selects `proxy-enforced` as the Linux default and expands the
sandbox image's Python suite. Other platforms default to `none`. Item 3
preserves every tier's existing semantics and does not edit that image; an
operator's `none`, `proxy-enforced`, `tunnel`, or `proxy` selection still
survives mt-admin re-rendering.

## Provider requirements

Both providers need a local Linux runtime. Rootless Podman is the primary lane;
the Docker CLI fallback uses the same namespace-join arguments. Both images must
provide `nft` and the provider health tools.

- NetBird: the default is NetBird 0.82.0 at OCI index digest
  `sha256:eca5b274c635bfbb04ec6724a0712d1146190dd450eea199eeb83916fdd94965`.
  The image needs an upstream-compatible `netbird` entrypoint, `/dev/net/tun`,
  `NET_ADMIN`, and an HTTPS management URL. Only `NET_ADMIN` is added; images
  requiring `SYS_ADMIN`/other capabilities for optional eBPF features are not
  silently granted them and fail their health gate instead.
- WireGuard: `wg-quick`, `wg`, `ip`, `/bin/sh`, and `sleep`; `ping` is also
  required when the optional canary is configured, and `DNS =` needs
  `openresolv`. The host kernel's WireGuard module must already be loaded. The
  user namespace owns the netns, so no host capability or network change is
  required.

## Rejected alternatives

- **Host networking or joining the host netns:** exposes host and tenant
  listeners, violating the sandbox boundary.
- **Per-command enrollment:** slow, churns single-use NetBird setup keys, and
  makes cleanup of credentials and peers unreliable.
- **Host-global WireGuard routing or `SO_BINDTODEVICE`:** needs host capabilities
  and changes routing outside the tenant.
- **Relying only on NetBird ACLs or documentation to prevent inbound:** external
  policy is useful but cannot prove every config is ingress-free; the netns
  firewall is the local structural control.
- **Reusing `src/tunnel/`:** that layer deliberately exposes the web gateway
  inbound; this feature is sandbox egress only.
- **Dropping firewall verification for a per-start check:** a later crash or
  rule change would silently reopen exposure or break the tunnel between runs.

## Rollout and verification

Opt in per tenant with `SANDBOX_NETWORK=tunnel` and `SANDBOX_TUNNEL_*`; mt-admin
preserves operator-set network values on re-render. The tunnel requires no
migration and does not change WI#537's Linux `proxy-enforced` default. Unit tests
pin argument construction, secret exclusion, validation, image/digest parsing,
instance naming, handshake age, firewall shape, and error taxonomy. The
double-gated `ic/tests/podman_sandbox_tunnel_integration.rs` WireGuard lane uses
real rootless Podman to verify start, container-ID-stable reuse, distinct
instance names, custom DNS with no uplink DNS rules, image-ID rotation, namespace
join, and fail-closed behavior after a firewall rule is added.

The separately gated NetBird lane requires `LUNARWING_E2E_NETBIRD_TESTS=1`, a
real management service, `NB_MANAGEMENT_URL`, `NB_SETUP_KEY`,
`NB_PEER_SETUP_KEY`, and an image named by `LUNARWING_E2E_NETBIRD_IMAGE` with
NetBird, `nft`, and `nc`. It verifies startup health, management reconnect,
stable peer IP/state volume across recreation, distinct instance ownership, and
rejection of a peer-initiated connection. Revoke the two throwaway peers after
the run. A skip without these external settings is not a live-validation pass;
the peerless WireGuard lane does not claim NetBird interoperability.
