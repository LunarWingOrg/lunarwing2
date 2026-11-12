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
the owning daemon. Each sandboxed command joins the exact container ID returned
by health/firewall verification with `--network container:<verified-id>`. A
per-instance lifecycle mutex serializes recreation; if a verified ID disappears
before the run starts, the runtime fails instead of joining a replacement before
its firewall is ready.

```text
lunarwing-sandbox-tunnel-<instance> (daemon-owned, long-lived)
  NetBird `up` or `wg-quick up`
  own netns, no published ports, no host networking
  CAP_DROP=ALL + CAP_NET_ADMIN (+ NET_RAW/NET_BIND_SERVICE for NetBird)
  no-new-privileges, read-only rootfs
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
  dropped. WireGuard's optional `SANDBOX_TUNNEL_DNS` is deliberately **not**
  passed to the provider: it needs runtime DNS to resolve hostname endpoints
  before the tunnel exists. Instead, a non-secret mode-`0644` resolver file is
  atomically installed and mounted `ro,z` into joined runs only (runtimes reject
  `--dns` with `--network container`), and every direct uplink DNS rule is
  removed. The configured resolvers must be reachable through `wg0`. NetBird
  rejects this setting during validation, before any runtime call.

The mark is how the tunnel's own control plane keeps using the uplink without
an address allowlist that would have to track management, signal, relay and
STUN hosts: the NetBird client is expected to set `0x1bd00` on every socket it
opens (advanced routing, the Linux default with `NET_ADMIN`), and a WireGuard
device stamps its encapsulated packets with its fwmark (the sidecar sets
`0x4c57` on a device whose config left it off and keeps wg-quick's full-route
mark otherwise). Setting a socket mark needs `NET_ADMIN`, which joiners do not
have. Apart from the default DNS residual, a joined command can leave only
through the tunnel interface: a split tunnel reaches its overlay CIDRs, a
full-route tunnel reaches everything through the overlay. UID- and cgroup-based
allowances were rejected: under rootless podman the sidecar's root and the
keep-id sandbox user map to the same host uid, and cgroup paths differ between
cgroup managers and runtimes.

Joiner containers have no `NET_ADMIN`, so their processes cannot alter either
chain. There are no `-p`/`--publish` arguments, no host-network join, no port
forward, and no LunarWing listener. The provider is brought up first using
bootstrap DNS; no verified container ID is returned until the table is
installed and verified. Before every run the listing is parsed and compared
rule by rule with the expected policy. Missing `nft`, an altered or missing
chain, an extra accept (with or without a comment), or a changed policy fails
closed. Without custom DNS, the runtime resolvers remain a low-bandwidth,
query-based exfiltration channel; `SANDBOX_TUNNEL_DNS` removes that direct
channel for both joiners and the sidecar itself. Bootstrap DNS still works, but
post-start sidecar lookups have no unmarked uplink path. NetBird's own firewall
is disabled with `NB_DISABLE_FIREWALL=true`: its 0.82.0 interface allower would
otherwise insert and continually reapply an `iifname "wt0" accept` in this exact
input chain. Advanced routing and its socket marks remain active.

The WI#600 live run measured NetBird's local resolver on the sidecar's overlay
IP. Its upstream writes are unmarked and fail with `operation not permitted`,
so the joined lookup returns `SERVFAIL` and no query reaches the uplink. WI#603
therefore selects explicit unsupported-combination enforcement:
`TunnelSidecarConfig::validate()` rejects nonempty `SANDBOX_TUNNEL_DNS` with
NetBird before image inspection or container start. Static control-plane hosts
cannot cover signal, relay, and STUN names learned later or address changes;
NetBird does not mark resolver sockets; and a new sidecar-only cgroup DNS
exception would add kernel/nft/runtime assumptions without improving the
joined-run boundary. Default runtime DNS is the only supported NetBird mode.
WireGuard custom DNS is unchanged and still leaves joined runs with no uplink
DNS path.

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
  0.82.0 OCI index, and `image@sha256:<64 hex>` overrides are accepted. The
  WireGuard default is an operator-built local `latest` tag; mutable references
  warn when auto-pull is enabled. With `SANDBOX_AUTO_PULL=false` the image must
  already be present locally. A separate mt-admin image-build command is a
  documented won't-fix for this item in favor of the existing per-tenant manual
  build/load path.
- NetBird readiness is `netbird status --check startup`. Peerless WireGuard
  readiness is `wg0` presence; with peers it is a nonzero handshake at most 180
  seconds old, or a successful `SANDBOX_TUNNEL_WIREGUARD_CANARY` ICMP probe
  through the overlay. Peered configs without a canary require at least one
  `PersistentKeepalive` of 1–60 seconds. Config parsing normalizes comments,
  whitespace, and section case like `wg`; decorated peer headers cannot bypass
  this gate, and the last digits-only keepalive in each peer wins. Malformed
  health-relevant lines fail with their line number. DNS and canary validation
  rejects loopback, link-local, broadcast, unspecified, and multicast addresses,
  and the canary is re-probed after firewall installation. Probes, start, and
  cleanup are bounded.
- On systemd hosts the detached `run -d` client uses a deterministic
  `lunarwing-tunnel-<instance>` transient service with `KillMode=process` so its
  pasta helper outlives the client. The unit stays loaded while a helper lives
  and `systemd-run` refuses a loaded unit name, so every sidecar
  removal/start-failure path frees it once the container is gone: a bounded
  wait for the helpers to exit, then `systemctl --user kill` (SIGTERM, then
  SIGKILL) for leftovers. `stop` is not used, because it does nothing to a unit
  whose client has already exited. Helpers of a container that still exists
  are never signalled. Foreground workload cleanup is unchanged.
- `SandboxManager` serializes initialization, health-gates it, and joins only
  the verified container ID returned for a run. It creates one instance handle
  and retains it after a failed or cancelled cold start so later commands share
  the same recovery state. Independently constructed handles for the same
  container name also share recovery state and a fair lifecycle operation lock.
  Starts, recovery, stops, and cancelled-start cleanup therefore exclude each
  other, with stale cleanup queued before the next start. Cold starts use the
  same 6-second adoption re-probe as per-command recovery, then give a fresh
  provider the full configured startup budget; image pull, runtime start, and
  provider readiness have separate bounds rather than one cold wall-clock cap.
  Per-command recovery gives a fresh provider at most 20 seconds (or the lower
  configured timeout). A provider that needs longer after recreation requires
  a daemon restart, which the returned error names. The initial health check,
  recovery, and final verification share one 60-second budget, including image
  and runtime probes. The returned `SandboxError::TunnelUnavailable`, mapped to
  `[sandbox:sandbox_unavailable]`, names the failed probe and is cached for 30
  seconds. Calls inside that window return the reason and next-attempt delay
  immediately; after it, one call retries. Recovery runs in a sidecar-owned
  task whose completion is shared through a watch channel. Caller cancellation
  only drops that caller's wait: the task still cleans up a partial start,
  records its result, and wakes any later waiters. A channel that closes without
  a result is matched to its own attempt and recorded as a failure, so a dead
  task cannot block later recovery or let a stale waiter clear a new attempt.
  There is no fallback to `proxy`, `proxy-enforced`, direct egress, or host
  execution.
- An already verified firewall table is left unchanged. A required installation
  sends `add table`, `delete table`, and the complete replacement definition to
  one `nft -f -` invocation, so netlink applies it atomically rather than
  exposing a running command to an unfiltered delete/rebuild interval.
- The configuration label is a salted SHA-256 over provider, image reference,
  resolved image ID, DNS, canary, management URL, setup key, WireGuard path and
  file contents; the salt is a random 0600 file in the private runtime
  directory, so the label is not a plain digest of key material.
  Credential/content/image rotation recreates the sidecar.
- NetBird's peer identity volume uses a separate salted hash of only instance,
  management URL, and setup key. Its 32-byte identity salt is created
  atomically as a regular, non-symlink mode-0600 file at
  `<base>/sandbox-tunnel-identity.salt`; read/create failure is unavailable,
  with no runtime-directory fallback. Image, DNS, runtime-directory, and host
  reboot changes retain the peer; URL/key rotation enrols a fresh peer and
  prunes only that instance's previous volume. Moving from the old runtime salt
  computes a new name once, so upgrade requires one deliberate re-enrolment and
  old-peer revocation rather than guessing how to adopt the old volume.
- `SandboxManager::shutdown()` marks shared recovery stopping, waits up to 15
  seconds for the active result and another 15 seconds for the lifecycle lock,
  then removes only the sidecar carrying this instance's ownership label
  (podman `rm -f -t`, Docker `stop` then `rm -f`). Recovery checks stopping
  before and after a start and removes a late container even after the wait
  expires. An explicit later cold start clears the stop and its cached failure;
  ordinary per-run recovery refuses a stopped handle. The normal daemon exit
  path does not call shutdown, so docs do not claim automatic exit cleanup. The
  operator guide records manual removal of the legacy fixed-name
  container/volumes and revocation of its old peer on upgrade.

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
| `tunnel` | private overlay and optionally full-route egress | kernel-enforced sidecar input + output chains; reachability = the tunnel's routes/ACLs plus default runtime DNS; core NetBird 0.82.0 behavior is live-validated, NetBird custom DNS is rejected, WireGuard custom DNS closes uplink DNS |
| `proxy` (legacy) | public web through env-var proxy | advisory only |

A full-route WireGuard config (`AllowedIPs = 0.0.0.0/0`, plus IPv6 where used) or
NetBird default route sends all outbound traffic through the overlay. A split
tunnel reaches only its overlay CIDRs: the sidecar's pasta/slirp uplink is
closed to joined commands by the output chain in both cases, except by default
for DNS to the runtime resolvers. WireGuard `SANDBOX_TUNNEL_DNS` removes that
direct exception. The WI#600 run confirmed that NetBird's unmarked upstream DNS
writes failed closed rather than leaking queries, leading to WI#603's explicit
rejection of that provider/setting combination.
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
  and an HTTPS management URL. `NET_ADMIN` supports advanced routing,
  `NET_RAW` its raw sockets, and `NET_BIND_SERVICE` its local resolver on port
  53; all three and only those additions are granted. `SYS_ADMIN` and other
  capabilities for optional eBPF features are not silently granted and fail
  their health gate instead.
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
preserves operator-set network values on re-render. The tunnel does not change
WI#537's Linux `proxy-enforced` default. Upgrades from the fixed-name sidecar use
the operator guide's manual legacy-container/volume and peer-revocation steps.
Unit tests pin argument construction, secret exclusion, address/keepalive
validation, NetBird custom-DNS rejection, image/digest parsing, instance naming,
peer-identity stability, handshake age, firewall shape, and error taxonomy.
Fake-runtime concurrency tests cover one shared start across handles, caller
cancellation, a waiter with a different budget, shutdown interruption, and
explicit restart; channel units cover abandoned and stale recovery completion.
The double-gated `ic/tests/podman_sandbox_tunnel_integration.rs` WireGuard lane
uses real rootless Podman to verify start, verified-ID joins, stable reuse,
distinct instances, independent tamper/image-rotation cases, resolver provenance
and read-only mounts, old-resolver refusal, hostname-endpoint bootstrap,
handshake failure, and detached-helper cleanup.

The separately gated NetBird lane requires all three podman/tunnel/NetBird
gates, `LUNARWING_E2E_NETBIRD_IMAGE`,
`LUNARWING_E2E_NETBIRD_MANAGEMENT_URL`, and
`LUNARWING_E2E_NETBIRD_SETUP_KEY_FILE`. It starts a plain second peer from the
same reusable key and checks detailed peer connectivity, peer-hosted HTTP and
public-IP/metadata denial, the exact mark rule, startup health, management
restart with a post-reconnect joined run, stable peer identity, and ingress
refusal with a proven-live listener. WI#603's validation gate replaces the old
custom-DNS live case, which must no longer start the forbidden configuration.

WI#600's 2026-11-11 live run passed the main lane against NetBird 0.82.0 and
captured the custom-DNS `SERVFAIL` used for the WI#603 decision. Uplink packet
captures distinguished control-plane, DNS and encrypted peer UDP from forbidden
destinations. Throwaway peers and credentials must be revoked and removed
afterward. Any future podman-capable CI job remains open; a skip without
external settings is not a live-validation pass, and no public-cloud or CI
execution is claimed.
