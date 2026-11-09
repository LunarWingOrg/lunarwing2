# Podman Sandbox Tunnel Networking (NetBird + WireGuard)

**Status:** IMPLEMENTED on `sweetiebot/kimi-3-20261109-0012` (WI#248)
**Lineage:** fresh port of the unreviewed `sandboxtunmode` experiment (`2ed3d0395`),
plus the egress-only and lifecycle hardening described below. Related design
history: WI#144 and the overlay-as-transport discussion on WI#153.
**Operator guide:** `docs/guides/SANDBOX_TUNNEL_NETWORKING.md`

## Problem

The generic shell sandbox supports public-web egress through an allowlist proxy,
but has no supported way to reach private services on a NetBird or WireGuard
overlay. `none` blocks all egress; `proxy-enforced` deliberately rejects private
addresses; legacy `proxy` is advisory and also public-only. This item adds an
opt-in overlay tier without changing the default or the external-worker lanes.

## Design

`SANDBOX_NETWORK=tunnel` starts one long-lived sidecar per daemon/tenant user.
The sidecar establishes NetBird or kernel WireGuard inside its own network
namespace. Each sandboxed command joins that namespace with
`--network container:lunarwing-sandbox-tunnel`.

```text
lunarwing-sandbox-tunnel (daemon-owned, long-lived)
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

### Egress only, not overlay exposure

Sharing a network namespace without a firewall would let a sandboxed process
bind a port reachable through the tunnel or the sidecar uplink. The original
experiment left that gap open. This implementation requires `nftables` in every
sidecar image and installs an `inet lunarwing_sandbox_egress` input chain:

- default policy `drop` for IPv4 and IPv6;
- loopback traffic accepted for in-namespace operation;
- `established,related` return traffic accepted for outbound connections;
- no other input accept rules permitted.

Joiner containers have no `NET_ADMIN`, so their processes cannot alter the
chain. There are no `-p`/`--publish` arguments, no host-network join, no port
forward, and no LunarWing listener. The firewall is installed before the first
joiner and verified before every execution. Missing `nft`, an altered chain, or
an extra inbound accept fails closed. NetBird ACLs and WireGuard peer policy
remain separate defense-in-depth; overlay membership is not application auth.

### Lifecycle and failure posture

- Startup validates provider settings, reuses only a running sidecar with the
  current SHA-256 configuration label, and otherwise recreates it.
- NetBird readiness is `netbird status --check startup`; WireGuard readiness is
  the presence of `wg0`. Probes and cleanup are bounded.
- `SandboxManager` health-gates initialization and calls `ensure_healthy()`
  before every container run. Provider failure, a dead sidecar, hash drift, or
  firewall drift returns `SandboxError::TunnelUnavailable`, mapped to the stable
  `[sandbox:sandbox_unavailable]` envelope. There is no fallback to `proxy`,
  `proxy-enforced`, direct egress, or host execution.
- Configuration hashes cover provider, image, management URL, setup key,
  WireGuard path and file contents. Credential/content rotation therefore
  recreates the sidecar. Only the digest enters a label, never key material.
- `shutdown()` removes the sidecar. Its label namespace is separate from the
  one-shot `lunarwing.sandbox` reaper label.

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
| `none` (unchanged default) | no egress | kernel `--network none` |
| `proxy-enforced` | public web through domain allowlist | network-none + Unix relay + public-only proxy |
| `tunnel` | private overlay and optionally full-route egress | sidecar input firewall + tunnel routes/ACLs |
| `proxy` (legacy) | public web through env-var proxy | advisory only |

A full-route WireGuard config (`AllowedIPs = 0.0.0.0/0`, plus IPv6 where used) or
NetBird default route sends outbound traffic through the overlay. A split tunnel
leaves the sidecar's ordinary pasta/slirp uplink available for other outbound
destinations; this is intentional and documented, not equivalent to the
public-only proxy allowlist. The inbound-drop firewall applies in both cases.

Item 4 (WI#537) owns any change to the default network mode and the sandbox
image's Python suite. This item preserves `none` and does not edit that image.

## Provider requirements

Both providers need a local Linux runtime. Rootless Podman is the primary lane;
the Docker CLI fallback uses the same namespace-join arguments. Both images must
provide `nft` and the provider health tools.

- NetBird: upstream-compatible `netbird` entrypoint, `/dev/net/tun`, `NET_ADMIN`,
  and an HTTPS management URL. Only `NET_ADMIN` is added; images requiring
  `SYS_ADMIN`/other capabilities for optional eBPF features are not silently
  granted them and fail their health gate instead.
- WireGuard: `wg-quick`, `wg`, `ip`, `/bin/sh`, and `sleep`; the host kernel's
  WireGuard module must already be loaded. The user namespace owns the netns, so
  no host capability or host network change is required.

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
preserves operator-set network values on re-render. No migration and no default
change. Unit tests pin argument construction, secret exclusion, validation,
firewall shape, parsing, and error taxonomy. The double-gated
`ic/tests/podman_sandbox_tunnel_integration.rs` lane uses real rootless Podman
and kernel WireGuard to verify start, container-ID-stable reuse, health checks,
namespace join, the actual nft chain, and fail-closed behavior after a firewall
rule is added. A live NetBird management/peer environment remains the required
provider-specific crypto/ACL validation environment; the peerless WireGuard lane
does not claim to prove NetBird control-plane interoperability.
