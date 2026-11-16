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
  IPv6 neighbour discovery; DNS (`udp`/`tcp` 53) only to the provider-specific
  permitted resolver; an explicit drop of `169.254.0.0/16`; and, last, packets
  carrying the provider's socket mark. Everything else is dropped. There is no
  default DNS channel to the runtime resolvers (WI#724). WireGuard's
  optional `SANDBOX_TUNNEL_DNS` is deliberately **not** passed to the provider:
  it needs runtime DNS to resolve hostname endpoints before the tunnel exists.
  Instead, a non-secret mode-`0644` resolver file is atomically installed and
  mounted `ro,z` into joined runs only (runtimes reject `--dns` with `--network
  container`), and every direct uplink DNS rule is removed. The configured
  resolvers must be reachable through `wg0`. NetBird instead permits only its
  learned `wt0` listener and rejects `SANDBOX_TUNNEL_DNS` during validation,
  before any runtime call; a public name resolves there only through an
  operator nameserver group whose upstream is reachable through the overlay.

The mark is how the tunnel's own control plane keeps using the uplink without
an address allowlist that would have to track management, signal, relay and
STUN hosts: the NetBird client is expected to set `0x1bd00` on every socket it
opens (advanced routing, the Linux default with `NET_ADMIN`), and a WireGuard
device stamps its encapsulated packets with its fwmark (the sidecar sets
`0x4c57` on a device whose config left it off and keeps wg-quick's full-route
mark otherwise). Setting a socket mark needs `NET_ADMIN`, which joiners do not
have. A joined command can leave only through the tunnel interface: a split
tunnel reaches its overlay CIDRs, a full-route tunnel reaches everything through
the overlay. UID- and cgroup-based
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
closed. Without custom DNS, WireGuard's runtime resolvers remain a
low-bandwidth, query-based exfiltration channel; `SANDBOX_TUNNEL_DNS` removes
that direct channel for both joiners and the sidecar itself. Bootstrap DNS still
works, but post-start sidecar lookups have no unmarked uplink path. NetBird has
no runtime-resolver exception: its only DNS destination is the `wt0` listener.
NetBird's own firewall is disabled with `NB_DISABLE_FIREWALL=true`: its 0.82.0
interface allower would otherwise insert and continually reapply an `iifname
"wt0" accept` in this exact input chain. Advanced routing and its socket marks
remain active. The same setting means NetBird evaluates no policies on this
peer, so the input chain is the only inbound filter here; NetBird ACLs bound
egress only, enforced by the destination and routing peers.

WI#695 measurement note **17394** found that the unchanged sidecar left its
resolver file at the runtime value through startup and later management DNS
changes. Joined public lookups used that uplink resolver and peer/custom-zone
names failed. Direct queries to NetBird's listener resolved the peer and
in-overlay primary upstream with zero uplink port-53 packets. This ruled out
both leaving the file unmanaged and treating the old runtime path as an
acceptable fallback.

The selected design stages the runtime resolver before sidecar creation with a
credential-free helper using the inspected hashed image ID and `--pull=never`.
Two non-secret mode-`0644` files are mounted `rw,z` at `/etc/resolv.conf` and
`/etc/resolv.conf.original.netbird`; joined runs mount the resolver `ro,z`.
Within the existing provider timeout, startup waits for exactly one nameserver
equal to the `wt0` IPv4 listener. Any other, duplicate, or malformed nameserver,
including the initial runtime resolver or DNS management left off, fails closed
before firewall installation. Installation and all later health verification
derive the expected DNS rules from `wt0` rather than rereading the file, so a
later rewrite retaining the listener cannot cause recovery churn.

`SANDBOX_TUNNEL_DNS` remains rejected for NetBird; operators configure
upstreams through NetBird nameserver groups. An upstream outside the overlay
fails closed because NetBird does not mark resolver sockets. Static
control-plane hosts were not selected because later-learned names and address
changes remain uncovered, and a sidecar-only cgroup exception would weaken the
joined-run boundary. WireGuard custom DNS is unchanged and still leaves joined
runs with no uplink DNS path.

**Shared-loopback decision (WI#592):** concurrent commands of one daemon share
loopback, listeners, and port space. This is accepted because one daemon/tenant
is one trust domain; separate daemons use separate sidecars. Per-run netns/veth
pairs or per-run enrollment were rejected as routing/capability complexity and
credential/peer churn that would not strengthen the cross-tenant boundary. Do
not schedule mutually untrusted workloads in one tenant. NetBird ACLs still
bound what a sandbox can reach, because the destination and routing peers
enforce them; they are not evaluated on the sidecar peer and do not filter
inbound there. WireGuard `AllowedIPs` are enforced on the sidecar's receiving
`wg0` interface: a packet whose source address is outside the sending peer's
`AllowedIPs` is dropped by the sidecar itself. LunarWing's input chain then
filters what remains in both cases; for NetBird, it is the sidecar's only
inbound filter. Overlay membership is not application auth.

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
- NetBird readiness is `netbird status --check startup` followed by the strict
  staged-resolver check within the same provider deadline. Peerless WireGuard
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
  provider the full configured startup budget (capped at 600 seconds); image
  pull, runtime start, and provider readiness have separate bounds rather than
  one cold wall-clock cap.
  Per-command recovery gives a fresh provider at most 20 seconds (or the lower
  configured timeout) and has a 60-second budget. A provider that needs longer
  after recreation requires a daemon restart, which the returned error names.
  Initial and final health verification use their individual probe deadlines
  without a second aggregate ceiling: seven probes totaling 70 seconds for
  WireGuard, six totaling 60 for NetBird. The health paths never pull an image
  (WI#651): a missing image fails the check fast and the single-flight recovery
  pulls it under its per-run budget, using the smaller of the configured
  startup timeout and the remaining recovery budget, with a pull-specific
  timeout diagnostic. Initial verification,
  recovery, and final verification therefore total at most 200 and 180 seconds,
  image present or not, plus the wait for the lifecycle lock, which precedes
  the recovery budget and has no timeout of its own. Only the queued cleanup
  of a cut-off start can hold it ahead of a command, for up to about 40
  seconds (45 with a retried spawner probe). The 30-second failure back-off
  starts when that cleanup takes the lock, so a command normally waits at most
  about 10 to 15 seconds, and about 50 to 55 when two cleanups are queued;
  joining a cold
  start can exceed these per-run totals because cold-start phases have separate
  bounds. A slow health check does not record a recovery failure or blame a
  recovery that never started. The returned `SandboxError::TunnelUnavailable`,
  mapped to `[sandbox:sandbox_unavailable]`, names the failed probe. Only an
  actual failed recovery is cached for 30 seconds. Calls inside that window
  return the reason and next-attempt delay immediately; after it, one call
  retries. Recovery runs in a sidecar-owned task whose completion is shared
  through a watch channel. Caller cancellation only drops that caller's wait:
  the task still cleans up a partial start, records its result, and wakes any
  later waiters. A channel that closes without a result is matched to its own
  attempt and recorded as a failure, so a dead task cannot block later recovery
  or let a stale waiter clear a new attempt.
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
| `tunnel` | private overlay and optionally full-route egress | kernel-enforced sidecar input + output chains; reachability = the tunnel's routes/ACLs plus provider-specific DNS; NetBird uses its `wt0` resolver and rejects custom DNS, WireGuard custom DNS closes uplink DNS |
| `proxy` (legacy) | public web through env-var proxy | advisory only |

A full-route WireGuard config (`AllowedIPs = 0.0.0.0/0`, plus IPv6 where used) or
NetBird default route sends all outbound traffic through the overlay. A split
tunnel reaches only its overlay CIDRs: the sidecar's pasta/slirp uplink is
closed to joined commands by the output chain in both cases, except for
WireGuard's default runtime-resolver DNS. WireGuard `SANDBOX_TUNNEL_DNS` removes
that direct exception; NetBird has no runtime-resolver exception.
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
validation, NetBird custom-DNS rejection, strict runtime-resolver refusal,
managed-resolver waiting, file-independent firewall construction, mode-`0644`
staging and keep-id mount arguments, image/digest parsing, instance naming,
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
peer-name DNS, the staged file's mode and read-only mount, `wt0`-only DNS rules,
old-resolver refusal, public-IP/metadata denial, the exact mark rule, startup
health, management restart with a post-reconnect joined run, stable peer
identity, and ingress refusal with a proven-live listener. Optional
`LUNARWING_E2E_NETBIRD_DNS_NAME` / `..._DNS_ADDRESS` and
`..._PUBLIC_ADDRESS` variables enable an in-overlay `dnsmasq` fixture and
match-domain or primary-group assertions. WI#695 note **17394** records the
pre-implementation measurements and captures; the final-head live summary must
be posted separately on that item.

WI#600's 2026-11-11 live run passed the main lane against NetBird 0.82.0 and
captured the custom-DNS `SERVFAIL` used for the WI#603 decision. Uplink packet
captures distinguished control-plane, DNS and encrypted peer UDP from forbidden
destinations. Throwaway peers and credentials must be revoked and removed
afterward. Any future podman-capable CI job remains open; a skip without
external settings is not a live-validation pass, and no public-cloud or CI
execution is claimed.
