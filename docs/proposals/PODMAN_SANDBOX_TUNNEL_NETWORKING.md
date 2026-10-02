# Podman Sandbox Tunnel Networking (NetBird + WireGuard)

**Status:** IMPLEMENTED (branch `sandboxtunmode`, 2026-10-02)
**Work items:** [#248](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/248)
(explore wireguard tun for podman sandbox), lineage [#144](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/144)
(ft tunnel support podman sandbox) and the "Third Network Layer" discussion on
[#153](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/153).
**Operator guide:** `docs/guides/SANDBOX_TUNNEL_NETWORKING.md`

## Problem

The generic sandbox's egress tiers end at "public internet via the allowlist
proxy". There is no way for a sandboxed command to reach services that live
behind a VPN overlay — a NetBird mesh (private registry, TensorZero, private
Postgres on a peer) or a plain WireGuard tunnel. `SANDBOX_NETWORK=none` blocks
everything; `proxy`/`proxy-enforced` reject non-public destinations by design
(`src/sandbox/proxy/egress.rs`). Operators who need sandbox→overlay
connectivity have no supported mechanism.

## Design

New egress mode `SANDBOX_NETWORK=tunnel`, implemented with the **tunnel
sidecar** pattern (the k8s pod sidecar, expressed with podman CLI):

```text
┌──────────────────────────── host (tenant user, rootless podman) ──────────┐
│                                                                            │
│  lunarwing-sandbox-tunnel (long-lived sidecar, label lunarwing.sandbox-tunnel)
│    own netns; --cap-drop=ALL --cap-add=NET_ADMIN [--device /dev/net/tun]   │
│    netbird:  netbird up (mgmt URL + setup key via 0600 --env-file)         │
│    wireguard: wg-quick up /etc/wireguard/wg0.conf (0600 conf, :ro mount)   │
│      interfaces: lo, pasta uplink, wg0/wt0                                 │
│                         ▲ shares netns                                      │
│  lunarwing-sandbox-<uuid> (per command, unchanged hardening)               │
│    --network container:lunarwing-sandbox-tunnel                            │
│    --cap-drop=ALL --cap-add=CHOWN --read-only --user 1000:1000 …           │
└────────────────────────────────────────────────────────────────────────────┘
```

- One sidecar per daemon (tenant user), started health-gated at
  `SandboxManager::initialize()` and stopped at `shutdown()`. Sandbox
  containers join its network namespace with `--network container:<name>`
  and keep every existing hardening flag — the join is orthogonal to
  `--userns keep-id`, `--cap-drop=ALL`, `--read-only`, `--pids-limit`.
- **NetBird** (`SANDBOX_TUNNEL_PROVIDER=netbird`): the sidecar runs the
  upstream `netbirdio/netbird` image with `up`; the management URL and setup
  key are delivered through a mode-0600 `--env-file` (podman reads it
  host-side) so the key never appears in the podman argv (which the systemd
  transient unit would otherwise persist in the journal).
- **WireGuard** (`SANDBOX_TUNNEL_PROVIDER=wireguard`): the sidecar runs
  `wg-quick up` against an operator-supplied wg-quick-format config
  (validated: mode 0600, contains `PrivateKey`), mounted `:ro,z`. Kernel
  WireGuard works in a rootless container netns because the netns is owned
  by the container's user namespace — no host privilege required.
- Egress policy inside the tunnel netns is whatever the tunnel provides
  (WireGuard `AllowedIPs`, NetBird routes/ACLs). For a restricted profile,
  use `AllowedIPs = 0.0.0.0/0` (or NetBird's default-route route) so the
  default route rides the tunnel; otherwise the sidecar's pasta uplink
  remains reachable and the mode is advisory like legacy `proxy`.
- Fail closed: configuration is validated at resolve time (missing
  management URL/setup key/config file → `ConfigError` at startup), and a
  sidecar that fails its health gate within the startup timeout is removed
  and reported as `[sandbox:sandbox_unavailable]` (new
  `SandboxError::TunnelUnavailable`), never degraded to another mode.

Config surface (env-only, like the reaper knobs — mt-admin renders tenant
env files): `SANDBOX_TUNNEL_PROVIDER` (`netbird`|`wireguard`, default
`netbird`), `SANDBOX_TUNNEL_IMAGE` (per-provider default),
`SANDBOX_TUNNEL_NETBIRD_MANAGEMENT_URL`, `SANDBOX_TUNNEL_NETBIRD_SETUP_KEY`,
`SANDBOX_TUNNEL_WIREGUARD_CONFIG`, `SANDBOX_TUNNEL_STARTUP_TIMEOUT_SECS`
(default 90).

## Live spike evidence (dev VM, rootless podman 6.1.2, kernel 6.18.54)

All mechanics validated before implementation, as unprivileged user without
subuid ranges, using a scratch-built image:

1. `ip link add wg0 type wireguard` inside a rootless container netns with
   `--cap-drop=ALL --cap-add=NET_ADMIN` — kernel module reachable from the
   userns-owned netns, interface created.
2. `ip tuntap add mode tun` with `--device /dev/net/tun` + `NET_ADMIN` — the
   NetBird path (userspace TUN) works rootless.
3. A joiner with the full sandbox flag set
   (`--network container:<sidecar> --userns keep-id:uid=1000,gid=1000
   --user 1000:1000 --cap-drop=ALL --cap-add=CHOWN
   --security-opt=no-new-privileges --read-only`) sees exactly the sidecar's
   interfaces (`lo`, pasta uplink, `wg0`) — proving netns join composes with
   the keep-id userns ladder and capability drop.

## Rejected alternatives

- **Join the host network namespace** (`--network host`): exposes every host
  listener to sandboxed commands; breaks the sandbox isolation model
  outright.
- **Per-run tunnel (wireguard handshake per execution):** NetBird enrollment
  takes seconds and setup keys are single-use; a long-lived sidecar avoids
  per-command latency and credential churn.
- **Create the tunnel on the host, route the host proxy through it
  (`SO_BINDTODEVICE`):** binding sockets to an interface requires
  `CAP_NET_ADMIN`/`CAP_NET_RAW` in the *host* netns — unavailable to the
  unprivileged daemon; also changes host-global routing.
- **nftables allowlist inside the sidecar netns:** viable later (the netns
  owner may install nftables rules) but adds a per-provider firewall engine
  and image requirements; `AllowedIPs = 0.0.0.0/0` achieves the restricted
  profile with stock tooling. Recorded as the follow-up if per-CIDR
  sandbox egress policy over the tunnel is wanted.
- **Reusing `src/tunnel/` (the gateway `netbird expose` abstraction):**
  wrong layer — that publishes an inbound HTTP URL for the web gateway;
  the sandbox needs outbound overlay transport. Kept separate on purpose.

## Security model

Overlay is a *transport, not a trust boundary* (the L1–L4 framing recorded
on WI#153). Concretely:

- The sidecar is the only credential holder (setup key / wg private key);
  sandboxed commands never see them — they only share the sidecar's network
  namespace. Same principle as the egress proxy holding API credentials.
- All other sandbox hardening is untouched: mount guard, masks, caps,
  read-only rootfs, pids/memory limits, non-root user, fail-closed error
  envelopes. `none` and `proxy-enforced` behave exactly as before.
- NetBird ACLs / WireGuard `AllowedIPs` decide what the tunnel can reach;
  application-layer auth (bearer tokens, mTLS) remains required on every
  reached service.
- The sidecar keeps `--cap-drop=ALL --cap-add=NET_ADMIN`
  (+ `--device /dev/net/tun` for netbird), `no-new-privileges`, memory and
  pids limits, and its own label namespace (`lunarwing.sandbox-tunnel`) so
  the existing reaper sweeps never mistake it for an orphaned one-shot
  sandbox container.

## Rollout

Default unchanged (`none`). Operators opt in per tenant via env; mt-admin
re-renders preserve it like any operator-set `SANDBOX_NETWORK` value. The
wireguard provider needs an operator-built image (guide ships a
Containerfile); the netbird provider pulls the upstream image.
