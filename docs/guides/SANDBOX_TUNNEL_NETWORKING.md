# Sandbox Tunnel Networking (NetBird / WireGuard)

`SANDBOX_NETWORK=tunnel` lets generic shell-sandbox commands make **outbound**
connections through a NetBird or WireGuard sidecar. It does not expose the web
gateway, open a host port, or let overlay peers connect into the sandbox. It
gives NetBird sandboxes no DNS channel to the runtime resolvers: they resolve
names only through NetBird's own listener and whatever upstreams are reachable
through the overlay. WireGuard without `SANDBOX_TUNNEL_DNS` keeps port 53 to the
runtime resolvers as a residual uplink channel; setting it to an overlay
resolver removes that channel. Otherwise a sandboxed command cannot reach the
host, LAN, cloud metadata service, or public internet through the sidecar's own
uplink. Design and alternatives:
`docs/proposals/PODMAN_SANDBOX_TUNNEL_NETWORKING.md`.

The WI#592 residual-hardening contract has four operator-facing parts, each
with its own section below: per-instance sidecar isolation, image identity,
custom DNS, and WireGuard handshake health. The end-to-end NetBird procedure is
under NetBird live validation in Verification.

WI#537 selects `proxy-enforced` as the Linux default; other platforms default
to `none`, and `tunnel` remains an explicit opt-in. Existing `none` /
`proxy-enforced` / `proxy` behavior and all per-command sandbox hardening are
unchanged, and mt-admin preserves every operator-set tier on re-render.

## Tier selection

| `SANDBOX_NETWORK` | Intended use | Boundary |
|---|---|---|
| `none` (default off Linux) | no network | kernel-enforced `--network none` |
| `proxy-enforced` (Linux default) | allowlisted **public** HTTP(S) | network-none plus Unix relay to the validating proxy; no credential injection |
| `tunnel` | NetBird/WireGuard overlay egress | kernel-enforced nftables policy in the sidecar netns: new inbound dropped; outbound only through the tunnel interface, marked provider control traffic, and provider-specific DNS; WireGuard `SANDBOX_TUNNEL_DNS` moves joined-run DNS into the tunnel and removes its default direct uplink allowance; NetBird permits only its `wt0` resolver and rejects the setting |
| `proxy` | legacy public HTTP(S) | advisory env-var proxy; direct bypass remains possible |

What a sandboxed command can reach in `tunnel` mode is exactly what the tunnel
routes, plus the provider-specific DNS path described below. With a split tunnel (`AllowedIPs =
10.231.0.0/16`, or NetBird routes) that is those overlay CIDRs; with a
full-route tunnel (`AllowedIPs = 0.0.0.0/0, ::/0`, or a NetBird exit node) it
is everything, always through the overlay. The `output` chain drops every
packet that does not leave through `wg0`/`wt0`, is not permitted DNS on the
uplink, and does not carry the provider's socket mark. Joined commands have no
`NET_ADMIN`, so they cannot set that mark. Link-local IPv4 (`169.254.0.0/16`:
the cloud metadata service and podman's `host.containers.internal` alias) is
dropped even for marked traffic. New inbound flows are dropped from every
direction.

**Residual DNS channel:** with WireGuard's default DNS, UDP/TCP port 53 to the
runtime resolvers is the one uplink channel open to joined commands. A recursive
resolver can carry query-based exfiltration even though arbitrary uplink
connections are dropped. Setting `SANDBOX_TUNNEL_DNS` installs overlay resolvers
only in joined runs and removes every direct uplink DNS accept. The sidecar
keeps its runtime resolver file so hostname endpoints resolve during bootstrap,
before the firewall is installed. Once that firewall is in, the sidecar's own
new lookups are dropped too: its resolver sockets are unmarked and no uplink
port-53 rule remains.

NetBird has no runtime-resolver fallback. Its firewall permits UDP/TCP port 53
only to NetBird's own `wt0` listener; validation still rejects
`SANDBOX_TUNNEL_DNS` before any container-runtime call. Configure upstreams as
NetBird nameserver groups reachable through the overlay. An upstream outside
the overlay fails closed because NetBird's resolver sockets are unmarked. That
last clause is the whole closure: NetBird registers the backup file's
nameservers as its last-resort upstream for names no group covers, so a public
lookup asked of the listener does make the client try the runtime resolver —
and the query is dropped only because the client opens it unmarked. See the
bump checklist below before moving the pinned client. A skipped NetBird test is
not a pass.

**Shared-loopback decision (WI#592):** concurrent commands of one daemon share
one network namespace and its loopback. A listener one command opens on
`127.0.0.1` is reachable by the others and ports can collide. This is accepted:
one daemon/tenant is one trust domain, while separate daemons now use separate
sidecars. Per-run netns/veth pairs or per-run enrollment would add routing and
credential/peer churn without improving the cross-tenant boundary. Do not run
mutually untrusted workloads in one tenant, and do not treat loopback listeners
as private between its commands. Overlay membership is not authentication:
keep TLS, tokens, and mTLS in force. NetBird ACLs still limit what a sandbox
can reach, because the destination peers and any routing peers enforce them;
they are not evaluated on the sandbox sidecar peer and do not filter inbound
there. WireGuard `AllowedIPs` are enforced on the sidecar's receiving `wg0`
interface: a packet whose source address is outside the sending peer's
`AllowedIPs` is dropped by the sidecar itself. LunarWing's input chain then
filters what remains in both cases; for NetBird, it is the sidecar's only
inbound filter.

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
  `[sandbox:sandbox_unavailable]`; it never falls back to another network mode.
  A sidecar that died or drifted gets one bounded recovery attempt.

A manager cold start re-probes an existing sidecar for at most **6 seconds**,
then retains the full configured startup timeout — capped at **600 seconds** —
for a fresh provider. Image
pull, runtime start, and provider readiness have separate configured/probe
bounds rather than one cold-start wall-clock cap. With the default 90-second
setting those three phases alone can take 270 seconds, plus the 6-second
re-probe and bounded inspection, firewall, and cleanup probes. A shell caller
can time out first; the sidecar-owned attempt still finishes and records its
result.

Each per-command health-verification chain uses the individual runtime-probe
deadlines rather than a second aggregate ceiling. WireGuard has seven probes
with 10-second deadlines, so a healthy chain can take **70 seconds**; NetBird
has six, so its chain can take **60 seconds**. The health path never pulls an
image (WI#651): a missing image fails the check fast with a pointer at the
single-flight recovery, which is the path that pulls it under its per-run
budget. Recovery re-probes an existing sidecar for at
most **6 seconds**, waits at most **20 seconds** for a fresh provider (or the
configured startup timeout when lower), and has a **60-second budget**. During
recovery, an existing sidecar that cannot prove its health inside the 6-second
window is removed and recreated even when it is healthy but only slow. A pull
inside recovery uses the smaller of that configured timeout and the remaining
recovery budget, and its timeout error names the image pull. Final verification
runs right after recovery, never pulls, and keeps the same probe deadlines as
the initial check.

Consequently, initial verification, recovery, and final verification can take
at most **200 seconds for WireGuard** or **180 seconds for NetBird**, image
present or not: auto-pull no longer extends the per-run chain, because the only
pull happens inside the recovery budget described above. These
totals include the full recovery budget. The lifecycle operation lock is
acquired before that budget starts, and taking it has no timeout of its own: a
command waits until whatever holds the lock is done. The only holder that can
be ahead of a recovering command is the cleanup that a cut-off start leaves
queued, for example a recovery whose budget ran out in the middle of a start.
Such a cleanup removes the container (up to 15 seconds), asks the runtime
whether the container is gone (up to 10 seconds) and releases the helper unit
(up to 15 seconds). It can therefore hold the lock for up to about 40 seconds,
or 45 when a retry of the workload spawner's probe falls due; without systemd
service mode it ends after the removal. The cleanup takes the lock when the
cut-off recovery records its failure. That is also the start of the 30-second
back-off, during which commands are refused at once and do not wait. So the
next command that recovers normally waits at most about **10 to 15 seconds**
for the lock. A start whose `run` command was refused while a matching
sidecar was already running, and that is cut off while it waits for that
sidecar's health, leaves two cleanups queued; then the wait can reach about
**50 to 55 seconds** (WI#651). A command that joins an already-running
cold start can instead wait for that attempt's independently bounded image
pull, runtime start, and provider readiness, plus its probes and cleanup, so
the per-run totals above do not cap that join. A shell caller can time out
first; the sidecar-owned attempt still finishes and records its result.

The per-run error names the failed image, runtime, handshake, canary, `netbird
status`, or firewall probe. Only an actual failed recovery is cached for **30
seconds**: commands inside that window return the original reason and the time
until the next attempt immediately, without more container churn. After the
window, one command tries again. If a provider needs longer than the 20-second
cap after recreation, the error and this contract require a daemon restart so
the cold-start budget applies. Cancelling a short shell command or stopping it
does not cancel recovery, its cleanup, or recording of its result. Later
callers await that same attempt and then use its recorded back-off. If the
task ends without publishing a result, the closed channel is also converted
into a recorded failure instead of wedging later commands.

Provider readiness and health re-probing retain the last completed provider
failure when a later provider probe is cut by the deadline. No new provider
probe starts after the budget expires, and readiness polling sleeps are clamped
to the time left. In the 6-second re-probe, the final retry waits halfway through
the remaining last polling interval instead of duplicating the previous probe
immediately; provider and firewall checks still share the original deadline.
An unsuccessful final retry returns without sleeping away the unused time.
If no provider probe completed with an error, the timeout stays generic rather
than inventing a failure reason. When the provider answers healthy but the
firewall check is cut by that deadline, the error names the egress firewall
check instead of claiming that the sidecar remained unhealthy. The unverified
sidecar is still removed.

Within one daemon, manager initialization is single-flight. Sidecar starts,
recoveries, stops, and cancelled-start cleanup also share a fair lifecycle lock
keyed by container name, so independently constructed handles cannot remove or
recreate each other's active sidecar. Shutdown marks the shared state stopping,
waits up to 15 seconds for active recovery and another 15 seconds for the
lifecycle lock, and checks before and after a start so a late sidecar is
removed. A later explicit cold start clears the stop request and its cached
failure. An already correct firewall table is not rewritten. When installation
is needed, one `nft -f -` invocation replaces the complete table atomically,
eliminating the former delete-to-rebuild interval in which a joined command
could have used the uplink without its egress filter.

The sidecar runs with all capabilities dropped except `NET_ADMIN`; NetBird
also receives `NET_RAW` for its raw sockets and `NET_BIND_SERVICE` for its
local resolver on port 53. It never receives `SYS_ADMIN`. All sidecars use
`no-new-privileges`, a read-only root filesystem, memory/process limits, no
published ports, and no host networking. Joined commands retain their existing
capability drop, non-root user/keep-id mapping, read-only root, mount masks,
resource limits, and cleanup; they receive none of the sidecar additions.

## Configuration

```env
SANDBOX_NETWORK=tunnel
SANDBOX_TUNNEL_PROVIDER=netbird          # netbird (default) | wireguard
SANDBOX_TUNNEL_STARTUP_TIMEOUT_SECS=90   # must be greater than zero, at most 600
# WireGuard-only overlay resolvers (rejected with NetBird):
# SANDBOX_TUNNEL_DNS=100.100.0.53,fd00::53
```

### Settings reference

| Variable | Default | Accepted values |
|---|---|---|
| `SANDBOX_NETWORK` | `proxy-enforced` on Linux, `none` elsewhere | `tunnel` enables everything on this page |
| `SANDBOX_TUNNEL_PROVIDER` | `netbird` | `netbird` or `wireguard` (`wg` also accepted); case-insensitive |
| `SANDBOX_TUNNEL_IMAGE` | NetBird 0.82.0 digest index; `localhost/lunarwing-tunnel-wireguard:latest` | any image reference; digest form must be `image@sha256:<64 hex characters>` |
| `SANDBOX_TUNNEL_STARTUP_TIMEOUT_SECS` | `90` | integer from 1 through 600 |
| `SANDBOX_TUNNEL_DNS` | unset | comma-separated unicast IPs; WireGuard only |
| `SANDBOX_TUNNEL_WIREGUARD_CONFIG` | unset, required for `wireguard` | regular, non-symlink file, at most 1 MiB, no group/other permission bits (normally `0600`), nonempty `PrivateKey` |
| `SANDBOX_TUNNEL_WIREGUARD_CANARY` | unset | one unicast IP; WireGuard only |
| `SANDBOX_TUNNEL_NETBIRD_MANAGEMENT_URL` | unset, required for `netbird` | `https://` URL without userinfo, query, or fragment |
| `SANDBOX_TUNNEL_NETBIRD_SETUP_KEY` | unset, required for `netbird` | one token without whitespace; secret |
| `SANDBOX_AUTO_PULL` | `true` in the daemon; `false` in mt-admin tenants | boolean |

Three tunnel settings are parsed in every network mode, before the mode is
consulted: `SANDBOX_TUNNEL_DNS` must be a comma-separated list of valid IPv4 or
IPv6 addresses, `SANDBOX_TUNNEL_WIREGUARD_CANARY` must be one valid IPv4 or IPv6
address, and `SANDBOX_TUNNEL_STARTUP_TIMEOUT_SECS` must be an integer of at most
600. A value that fails one of those checks is a configuration error at startup
even on a `proxy-enforced` or `none` tenant. The other five — provider,
image reference, management URL, setup key, and WireGuard config path/file —
and the remaining semantic checks (non-unicast resolver or canary addresses,
`SANDBOX_TUNNEL_DNS` with NetBird, a canary without WireGuard, and a timeout of
zero) run only when `SANDBOX_NETWORK=tunnel`. A bad value there takes effect at
the next start once the network mode changes.

DNS and canary addresses must be specified unicast IPs. Loopback, link-local,
broadcast, unspecified, and multicast addresses are rejected: none is a valid
overlay health target, and loopback would share a listener or fake canary with
other commands in the tenant namespace.

## Per-instance sidecar isolation (WI#592)

A daemon owns exactly one tunnel sidecar. Its name is
`lunarwing-sandbox-tunnel-<instance>`, where `<instance>` is the first 16
hexadecimal characters of SHA-256 over the daemon's `LUNARWING_BASE_DIR` path
string. On a multi-tenant host `mt-admin` gives each tenant its own
`LUNARWING_BASE_DIR` (`$state_dir`), so tenants do not share a sidecar even when
their container stores, uids, or ports collide.

The container carries three labels: `lunarwing.sandbox-tunnel` (provider),
`lunarwing.sandbox-tunnel-hash` (the salted configuration drift hash), and
`lunarwing.sandbox-tunnel-instance` (the instance id above). A start adopts an
existing container only when it is running, its hash label matches the current
salted hash, and its instance label matches this daemon. Removal requires that
instance label, so one daemon can neither adopt nor remove another daemon's
sidecar. Startup, recreation, stop, and cancelled-start cleanup are
serialized by a fair lifecycle lock keyed by container name, and every run
joins the exact container ID that passed health and firewall verification —
never a name, and never a container created after verification. If recreation
removes that ID first, the run fails closed instead of joining a new,
not-yet-filtered namespace.

Where the workload spawner uses systemd services, the detached `run` client's
network helpers stay in the transient unit `lunarwing-tunnel-<instance>`, so
the sidecar reuses that unit name; a start that finds the unit still loaded
fails closed rather than racing it, and one instance never signals another
instance's unit. Without systemd service mode there is no helper unit; the
names and labels still keep instances apart.

`podman ps --filter label=lunarwing.sandbox-tunnel` should show one sidecar per
daemon instance. The pre-WI#592 fixed-name container and its volumes are not
removed automatically; see the upgrade note below.

## Image identity

`SANDBOX_TUNNEL_IMAGE` overrides the provider default. Digest references use
`image@sha256:<64 hex characters>` and are recommended for registry images.
Mutable tags remain accepted for operator-controlled local images, but startup
logs a warning when auto-pull is enabled with a non-digest reference. NetBird's
default is digest-pinned; WireGuard's default remains the locally built
`localhost/lunarwing-tunnel-wireguard:latest`, with resolved-image-ID hashing
providing drift detection rather than registry immutability. Prefer a digest
for production overrides.

The reference alone does not identify the image. Every start and health check
runs `image inspect --format {{.Id}}` locally, requires the resolved ID to be 64
hexadecimal characters (the `sha256:` prefix is accepted but optional), and
feeds that resolved ID into the salted drift hash. Re-pointing a tag,
rebuilding the local tag in place, or otherwise changing what the reference
resolves to therefore recreates the sidecar: the container, its network
namespace, and its firewall rules are rebuilt from the image the reference now
resolves to. Provider identity does not live in the image, so it survives that
recreation: a NetBird sidecar mounts its existing state volume and keeps the
enrolled peer, because the image is not part of that volume's identity (see
NetBird below), and a WireGuard sidecar mounts the same host config file with
the same private key. The hash covers the provider,
image reference, resolved image ID, resolver and canary settings, the WireGuard
config path and contents, the management URL, and the setup key; the published
label is salted, so it is a drift marker rather than a digest of the key. The
startup timeout and `SANDBOX_AUTO_PULL` are not hashed: changing either applies
at the next start without recreating a healthy sidecar.

Pre-build or pre-pull the image for the same tenant's container store. Every
image must include `nftables`; startup deliberately fails if it is absent, and
the error names `nftables` rather than falling back to host networking. A
dedicated mt-admin `build-tunnel-image` step is intentionally not added in
WI#592: provider images are operator-specific and already have a manual
per-tenant build/load path, while `SANDBOX_AUTO_PULL=false` fails closed if
that step was skipped. The health path never pulls either (WI#651): a missing
image fails fast and the single-flight recovery performs the one pull under
its per-run budget. On multi-tenant hosts `mt-admin` renders no tunnel
defaults and preserves every operator-set `SANDBOX_TUNNEL_*` value verbatim on
re-render (`docs/ops/TENANT-CONFIGURATION.md`).

## Custom DNS

`SANDBOX_TUNNEL_DNS` is WireGuard-only and takes a comma-separated list of
unicast resolver addresses. NetBird rejects a nonempty value during
configuration validation and uses its managed resolver instead: its firewall
accepts port 53 only to the `wt0` listener, and upstreams are configured as
NetBird nameserver groups reachable through the overlay. There is no
runtime-resolver fallback for NetBird, so an outside upstream fails closed.

With WireGuard and the setting unset (the default), joined commands resolve
names through the sidecar's runtime resolvers over the uplink. Only port 53 to
those resolvers is accepted, and that remains the residual DNS channel
described above: a recursive resolver can carry query-based exfiltration even
though arbitrary uplink connections are dropped.

Setting it changes both halves of the path:

- Joined runs mount a daemon-staged resolver file read-only at
  `/etc/resolv.conf`.
- Every direct uplink DNS accept is removed, including the rule that would
  otherwise let the sidecar's own post-bootstrap lookups out. The sidecar keeps
  its runtime resolver file so provider bootstrap and hostname endpoints still
  resolve before the firewall is installed.

The file is `<container>.resolv.conf` in the daemon's private runtime
directory, mode `0644`, written atomically from a temp name and refreshed on
start and every health check. `SandboxManager::shutdown()` stops the sidecar,
and the stop removes the file. Joined runs mount it `ro,z` because Podman and
Docker reject `--dns` together with `--network container`; the shared SELinux
label is safe for concurrent runs and the mode works with every supported
keep-id mapping.
Use resolvers reachable through `wg0`, including after a reconnect. Stock
`wg-quick` resolves its endpoint once at start, when the sidecar still has its
runtime resolver file, but a later re-resolve script — or anything else that
re-resolves the endpoint after the firewall is installed — is affected by the
removed uplink DNS rules. Do not expect NetBird to accept this setting during a
hostname-based reconnect; it is rejected up front instead.

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
New salts are written and synced under a unique temporary name in the same
directory, published to the final name with a no-replace hard link, and followed
by a directory sync. The final name therefore never exposes a partially written
salt, and concurrent creators converge on one complete file.

A damaged existing salt is still refused rather than silently replaced. To
recover from a truncated or corrupt salt, stop the daemon or tenant, remove the
instance's NetBird sidecar container so it cannot be adopted, and delete only
`$LUNARWING_BASE_DIR/sandbox-tunnel-identity.salt` as the daemon's user before
restarting it. This deliberately creates a new peer identity: the next start
uses a new state volume and re-enrols once, so a valid reusable or replacement
setup key is required and the overlay address can change. The previous
instance-scoped volume is pruned; revoke the old peer in NetBird after recovery.

The uplink allowance relies on NetBird's advanced routing: its control-plane
and WireGuard sockets carry the `0x1bd00` mark, which the sidecar's `output`
chain lets onto the uplink. Do not set `NB_USE_LEGACY_ROUTING`,
`NB_SKIP_SOCKET_MARK` or `NB_DISABLE_CUSTOM_ROUTING` in the sidecar image or
environment; if the client logs "falling back to legacy routing", the firewall
blocks its management connection and the health gate fails closed. LunarWing
sets `NB_DISABLE_FIREWALL=true` because NetBird 0.82.0's interface allower
otherwise inserts an `iifname "wt0" accept` into LunarWing's own input chain and
reapplies it through a netlink monitor. Advanced routing remains enabled, while
LunarWing's exact input/output chains are the only packet filter.
`NB_DISABLE_FIREWALL=true` also means NetBird enforces no access control on this
peer: the policies configured in NetBird are not evaluated here, and any peer
that can reach this one over the tunnel is filtered only by the host's own
rules. LunarWing's `input` chain is those rules — `policy drop`, with loopback
and `ct state established,related` only — and it is the only inbound filter for
this sidecar peer. A change that adds any inbound accept there has nothing
behind it.

Readiness requires `netbird status --check startup` to succeed (management and
signal connected, plus relay availability when relays are configured). NetBird
0.82.0 needs `NET_ADMIN`, `NET_RAW`, and `NET_BIND_SERVICE`; `SYS_ADMIN` is not
granted for optional eBPF features.

Before sidecar creation, a credential-free helper reads the runtime resolver
from the inspected hashed image ID with `--pull=never`. LunarWing stages that
non-secret content in mode-`0644` resolver and backup files, then mounts them
`rw,z` at `/etc/resolv.conf` and `/etc/resolv.conf.original.netbird` so NetBird
can manage DNS despite the read-only root filesystem. Joined runs mount the
same resolver file `ro,z`; its mode works with every supported keep-id mapping.

The firewall does not trust that file for its allowance. Within the same
bounded provider-startup budget, LunarWing waits until the file contains exactly
one `nameserver`, equal to the `wt0` IPv4 listener reported by NetBird. A runtime
resolver, another address, a duplicate, a malformed line, or DNS management left
off fails closed without a firewall or joined run. Installation and every later
health check derive the expected UDP/TCP port-53 accepts from `wt0`, not from the
file, so a later NetBird rewrite that retains the listener does not change the
expected table or force recovery. There is no runtime-DNS fallback; restore
managed DNS or use another supported provider after such a startup failure.

The host file is checked again before every run, and before a running sidecar is
adopted: a missing file, or one that does not name the `wt0` listener exactly,
refuses the run and recreates the sidecar, which stages the file again. That
gate closes the window where the file comes apart from the sidecar's copy — a
re-stage under an adopted sidecar, a stop that removed the file while the
container survived, or NetBird writing the runtime resolver back after managed
DNS is turned off on the server. Both messages are in the troubleshooting table
below. The same `netbird status --ipv4` answer feeds the firewall's expected
policy and this check, so a run asks NetBird once.

WI#695 measurement note **17394** recorded the unchanged-target behavior: the
file never changed, joined public lookups used the runtime resolver over the
uplink, peer/custom-zone names failed, and direct queries to NetBird's listener
resolved the peer and in-overlay primary upstream with zero uplink DNS packets.
Those measurements motivated the staged design; they are not a claim that the
staged design itself passed at that measurement head. WI#600's 2026-11-11 live
run separately proved management/signal/relay/STUN readiness, peer connectivity
and HTTP, public-IP and metadata denial, management restart, stable identity,
and ingress refusal. Its custom-outside-upstream `SERVFAIL` was historical
fail-closed evidence, not a supported configuration. No CI or public-cloud run
is claimed.

NetBird ACLs still matter, but only in the outbound direction: they limit what
the sandbox can reach, enforced by the destination peers and by routing peers,
not by this peer. Use restrictive ACLs for that reachability control, and do not
count them as a second layer behind LunarWing's input chain — there is none.
Do not reuse the gateway-exposure NetBird peer or setup key from
`docs/guides/NETBIRD_TUNNEL.md`; that is a separate, inbound gateway feature.

**Operator fact.** Without a NetBird **primary** nameserver group whose
upstream is reachable through the overlay, a NetBird sandbox resolves no public
names: `example.com`, package mirrors, and every other public name fail. It
still resolves peer names, and with a match-domain group the names under that
group's domains. A tenant that already runs `SANDBOX_NETWORK=tunnel` with
NetBird loses public-name resolution at its first start after upgrading to this
release, because the runtime-resolver allowance it used until then is gone.
Configure a primary nameserver group with an overlay-reachable upstream before
upgrading to keep public names, or accept peer and match-domain names only.

### NetBird bump checklist

Before moving the pinned NetBird image, confirm each of these against the new
version; the DNS boundary rests on them:

- **The client writes only its own listener into `/etc/resolv.conf` when a
  backup file is present.** NetBird 0.82.0 does this with or without a
  nameserver group (`client/internal/dns/file_unix.go`). If a new version
  appends upstreams instead, the startup check fails closed on every start and
  the sidecar never becomes healthy rather than allowing them.
- **The listener binds the `wt0` address on port 53** and `netbird status
  --ipv4` reports that address (`service_listener.go`). LunarWing derives the
  whole allowance from that answer.
- **Resolver sockets are not marked.** NetBird registers the backup file's
  nameservers as its last-resort upstream. Those queries are dropped only
  because the client opens them unmarked, so the sidecar's `output` chain has no
  accept for them. A version that marks resolver sockets reopens an uplink DNS
  channel; check `client/internal/dns/upstream_general.go` for a socket mark or
  `SO_MARK` on the upstream client before pinning.
- **Advanced routing stays on.** The control-plane accept depends on the
  `0x1bd00` socket mark. `NB_USE_LEGACY_ROUTING`, `NB_SKIP_SOCKET_MARK`, and
  `NB_DISABLE_CUSTOM_ROUTING` must stay unset.

Run the gated NetBird lane after any bump. It asks the listener for a public name
in the runs without `LUNARWING_E2E_NETBIRD_PUBLIC_ADDRESS` and requires that
lookup not to resolve, with the uplink port-53 capture at zero.

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
exists (the lifecycle e2e shape).

### Handshake health

A config with at least one peer is ready only when a peer has a nonzero
WireGuard handshake no more than **180 seconds** old. The probe runs
`wg show wg0 latest-handshakes` inside the sidecar as one of the seven
10-second probes in the per-command chain, so it runs before every command and
again after recovery. Three answers fail closed: no peer with a nonzero
timestamp (`WireGuard handshake probe found no completed handshake`), a stale
timestamp (`WireGuard handshake is Ns old, exceeding the 180s liveness
budget`), and a timestamp ahead of the local clock (`WireGuard handshake probe
returned a future timestamp`). An un-handshaked or stale peer returns
`sandbox_unavailable` within the bounded recovery described above, and later
commands inside the 30-second back-off fail immediately instead of timing out.

Without a canary, a peered config is valid only when at least one peer sets
`PersistentKeepalive` from 1 through 60 seconds. That requests periodic
handshakes but cannot guarantee that the remote peer answers. Alternatively
set `SANDBOX_TUNNEL_WIREGUARD_CANARY` to an ICMP-reachable overlay address; the
image then needs `ping`. Its `ping -c 1 -W 2 <address>` probe generates
handshake traffic and is repeated after the egress firewall is installed, so a
response through the host uplink cannot make the sidecar healthy. When a canary
is set, its result replaces the handshake-timestamp check. Application-level
reachability still depends on routes and services.

For these health-relevant fields, LunarWing follows `wg` parsing: `#` starts a
comment, whitespace is removed, and section names are case-insensitive, so
`[Peer] # office` and `[ Peer ]` both count as peers. `PersistentKeepalive`
accepts `off` or digits only from 0 through 65535, and the last value in each
peer wins; the health-gate requirement remains 1–60 on at least one peer.
Semicolons are not comments. Malformed sections and health-relevant values are
rejected with a line number rather than silently degrading to a peerless probe.

### Firewall mark

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
commands use the runtime-resolver channel described under Custom DNS.

Setting `SANDBOX_TUNNEL_DNS` is also described under Custom DNS: joined runs
mount the staged resolver file `ro,z` while the sidecar keeps its runtime
resolver for bootstrap, and all direct uplink DNS rules are removed. Stock
`wg-quick` resolves its endpoint once at start, but any later re-resolve script
is affected by those removed rules. NetBird rejects this setting during
validation rather than attempting hostname reconnects without a resolver path.

## Rotation and lifecycle

The daemon stores a salted SHA-256 configuration digest on the sidecar, not key
material; the salt is a random mode-0600 file in the daemon's runtime
directory. Changing provider, image reference, resolved image ID, DNS, management
URL, setup key, WireGuard path, or WireGuard file contents causes recreation
instead of stale reuse. Restart the tenant after rotating the external peer/key
and revoking the old one.

The instance-scoped names, ownership labels, verified-container-ID join, and
per-instance lifecycle lock are documented under Per-instance sidecar
isolation.

`SandboxManager::shutdown()` marks active recovery stopping and performs the
bounded waits described above before removing only its own sidecar; start checks
remove a late container even if those waits expire. On systemd hosts it then
frees the deterministic `lunarwing-tunnel-<instance>` transient helper
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
another egress path. The next eligible command performs the bounded recovery
described above, unless a 30-second failure back-off is still active. Cancelling
the caller does not cancel the sidecar-owned recovery task: it still removes a
partial sidecar on failure and records the result for later commands. No
container ID becomes joinable before the complete firewall verifies.

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
Rolling back after that first start requires the same caution: runtime-salted
code computes a different name again, and its stale-volume pruning can remove
the newer volume, so another valid setup-key use and old-peer cleanup may be
required.

## Verification

1. Start the tenant and check that the daemon logs sidecar started/healthy,
   with no credential values.
2. Inspect both chains from the trusted host:

   ```bash
   podman exec lunarwing-sandbox-tunnel-<instance> \
     nft list table inet lunarwing_sandbox_egress
   ```

   `input` must show `policy drop`, loopback accept and
   `ct state established,related accept`; for a NetBird sidecar this chain is
   the only inbound filter, because `NB_DISABLE_FIREWALL=true` leaves no
   NetBird ACL layer behind it. `output` must show `policy drop`,
   accepts for `lo`, `wg0` and `wt0`, the three neighbour-discovery ICMPv6
   accepts, `ip daddr 169.254.0.0/16 drop`, and one `meta mark … accept`.
   NetBird has `udp`/`tcp dport 53` accepts only for its `wt0` IPv4 listener.
   WireGuard without `SANDBOX_TUNNEL_DNS` instead allows each runtime resolver;
   with that setting, no uplink DNS accept is present. Anything else makes the
   daemon refuse every run.
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
   uplink resolver. A peered localhost-endpoint case proves the sidecar retains
   its runtime resolver file and `wg-quick` configures the endpoint; localhost
   comes from `/etc/hosts`, so this is not an external-DNS assertion. A separate
   manager-level dead-peer case uses a decorated `[Peer]` header and checks the
   bounded first failure, sub-second cached failure, retry after back-off, and
   cleanup plus a recorded failure after caller cancellation. A delayed-provider
   image takes about 25 seconds to become ready and proves that a cold start can
   use a longer configured budget than the 20-second per-run recovery cap. A
   dual-interface WireGuard case completes a real `wg0`/`wg1` handshake over
   loopback, disables the peer until the handshake ages out, and keeps the
   recreated peer dead while checking the 6-second re-probe, 20-second fresh
   provider cap, and sub-second cached failure through the manager. The peerless
   config still does not test peer encryption or NetBird's control plane.
5. For the NetBird end-to-end lane, follow NetBird live validation below.
6. On a real NetBird/WireGuard overlay, verify an outbound connection from the
   sandbox to an allowed private service. Then run a temporary high-port
   listener in the sandbox and attempt to connect from another overlay peer:
   the inbound attempt must fail. Remove the temporary listener afterward.

### NetBird live validation (WI#592)

This lane is the only test that proves the NetBird half of the boundary end to
end. The test returns without doing anything only when one of the three gates is
closed; each gate accepts `1` or `true` in any case. With all three open it
fails rather than skipping when `LUNARWING_E2E_NETBIRD_IMAGE`,
`LUNARWING_E2E_NETBIRD_MANAGEMENT_URL`, or the setup-key file is missing or
unusable. A skip is not a live-validation pass.

1. Prepare an isolated, throwaway NetBird management deployment and one
   dedicated reusable setup key. Use an image containing NetBird, `nft`, `nc`,
   `wget`, `ping`, and `nslookup`; the lane starts its own plain second peer
   with the same key. Do not point this at the production management server or
   reuse the gateway-exposure peer and key from `docs/guides/NETBIRD_TUNNEL.md`.
2. Store the setup key in a mode-`0600` file outside the repository. Do not
   print the key, the key file, or the tenant env file while debugging.
3. Run the gated lane:

   ```bash
   cd ic
   LUNARWING_E2E_PODMAN_TESTS=1 LUNARWING_E2E_TUNNEL_TESTS=1 \
   LUNARWING_E2E_NETBIRD_TESTS=1 \
   LUNARWING_E2E_NETBIRD_IMAGE=<netbird-image> \
   LUNARWING_E2E_NETBIRD_MANAGEMENT_URL=https://nbtest.example.org \
   LUNARWING_E2E_NETBIRD_SETUP_KEY_FILE="$HOME/.nbtest-setup-key" \
     taskset -c 0-11 cargo test -j12 --test podman_sandbox_tunnel_integration \
     tunnel_sidecar_netbird_live -- --exact --nocapture --test-threads=1
   ```

4. Read the lane in the order it asserts:
   1. **Fresh start.** The sidecar starts under the configured startup budget;
      the lane prints `LUNARWING-NETBIRD-FRESH-START-SECONDS` for comparison
      with the 20-second per-run provider cap.
   2. **Peer connectivity.** The lane pings both overlay addresses, then runs
      `netbird status -d` in each container and requires the other peer's block —
      the one that names that peer's overlay IP — to carry a `Status: Connected`
      line. `Connected` on the `Management:` block alone does not count; the
      lane's own unit test rejects a block without the peer IP.
   3. **Firewall and join.** Exactly one `0x1bd00` control-plane mark accept and
      only the `wt0` DNS allowances; then a real joined command sees `wt0`,
      reaches peer-hosted HTTP, resolves the peer name, and observes the
      mode-`0644` resolver file mounted read-only. It also proves refusal of
      every former resolver and denies public-IP and metadata destinations.
   4. **Identity.** Stopping the sidecar and deleting the runtime-directory
      label salt models a reboot; recreation must return the same peer IP and
      the same state volume.
   5. **Reconnect.** A `netbird down`/`up` cycle, or the optional server
      restart, is followed by three consecutive full health checks and another
      joined command.
   6. **Ingress refusal.** An attempt against a proven-live loopback listener
      in the sidecar must fail.
   7. **Per-run recovery.** The lane kills the sidecar and requires
      `container_for_run()` to replace it inside the per-run budget, printing
      `LUNARWING-NETBIRD-RUN-RECOVERY-SECONDS`.
5. To restart the throwaway management deployment instead of only reconnecting
   the client, set `LUNARWING_E2E_NETBIRD_SERVER_CONTAINER` to a space-separated
   server/proxy list. The lane requires stable health and another joined
   command afterward.
6. For an in-overlay nameserver group, also set
   `LUNARWING_E2E_NETBIRD_DNS_NAME=probe.lwtest` and
   `LUNARWING_E2E_NETBIRD_DNS_ADDRESS=10.99.0.1`, then distribute that group to
   the sidecar and plain peer with the plain peer's overlay IP as its upstream.
   The lane moves the plain peer's own DNS listener to loopback and starts
   `dnsmasq` for that zone. For a primary group, additionally set
   `LUNARWING_E2E_NETBIRD_PUBLIC_ADDRESS=10.99.0.2` and remove the group's match
   domain; the lane then requires `example.com` to resolve to that address too.
   A match-only group omits the public-address variable. These are optional live
   fixtures, not operator configuration through `SANDBOX_TUNNEL_DNS`.
7. Without `LUNARWING_E2E_NETBIRD_PUBLIC_ADDRESS` the lane also runs the ADN-1
   negative probe: it asks the listener for `example.com` and requires that the
   lookup not resolve (`LUNARWING-NETBIRD-PUBLIC-DNS-LEAK` must be absent). This
   covers the one path where an old uplink DNS channel could survive — NetBird's
   last-resort upstream taken from the backup file, which is dropped only
   because the client opens that socket unmarked. Pair the probe with a
   host-side capture of the uplink port 53 in the same run; it must stay at
   zero packets.
8. Record the fresh-start seconds, the recovery seconds, the lane summary, and
   the capture count in the work item. WI#600's 2026-11-11 local run passed the
   main lane against NetBird 0.82.0, captured the custom-DNS failure that led to
   the WI#603 rejection, and used an uplink `tcpdump` to check for forbidden
   destinations. Any podman-capable CI job with protected masked credentials
   remains future work; neither provider lane currently runs in CI, and no
   public-cloud run is claimed.

## Troubleshooting

| Symptom | Check |
|---|---|
| `[sandbox:config]` at startup | provider name, required env values, HTTPS management URL, WireGuard file type/mode/`PrivateKey`, nonzero timeout, peer keepalive of 1–60 seconds or a canary, non-local unicast DNS/canary addresses, and no `SANDBOX_TUNNEL_DNS` with NetBird |
| `sandbox_unavailable` mentioning `nftables` | install `nft` in the sidecar image; do not switch to direct/host networking |
| `sandbox_unavailable` mentioning the image | `SANDBOX_AUTO_PULL=false` and the image is not in the tenant's store; build or load it there |
| `sandbox_unavailable` mentioning the runtime directory | `XDG_RUNTIME_DIR` is unset or not owned by the daemon user; the credential file and label salt are never placed in a shared `/tmp` |
| sidecar start rejected | image/entrypoint, rootless runtime, `/dev/net/tun` for NetBird, kernel WireGuard module, supported sysctls |
| NetBird never becomes ready | management URL and setup-key validity, signal/relay reachability, image version's `status --check startup` support |
| NetBird connects, then loses management after a while | the client fell back to legacy routing (no socket marks): check its log and remove `NB_USE_LEGACY_ROUTING` / `NB_SKIP_SOCKET_MARK` |
| a fresh provider exceeds the 20-second per-run cap | restart the daemon so recreation uses the full cold-start budget; investigate why the provider is slow |
| names do not resolve in the sandbox | for NetBird, check that managed DNS is enabled, the staged file names only `wt0`, and the nameserver group is reachable through the overlay; for WireGuard, check the mounted file and overlay resolver reachability, or the runtime resolvers when custom DNS is unset |
| `the tunnel resolver file joined runs mount is missing; recovery restages it` or `... no longer names NetBird's wt0 listener; recovery restages it` | the host file joined runs mount was deleted or rewritten after the sidecar started (a re-stage under an adopted sidecar, a stop that left the container, or NetBird writing the runtime resolver back, typically after DNS management was turned off on the server). The daemon refuses the run and recreates the sidecar so the file is staged again; if it recurs, check whether managed DNS is still enabled for the peer |
| `sandbox_unavailable` naming the WireGuard handshake | no peer completed a handshake within 180 seconds: check peer keys, the remote endpoint, and `PersistentKeepalive = 1..=60` on at least one peer, or configure `SANDBOX_TUNNEL_WIREGUARD_CANARY`; a timestamp ahead of the local clock also fails the probe |
| `sandbox_unavailable` naming the WireGuard canary | the `ping` probe to `SANDBOX_TUNNEL_WIREGUARD_CANARY` failed: confirm `ping` exists in the image, the canary address is routed through `wg0`, and the peer answers ICMP |
| a healthy sidecar is recreated after an image rebuild | expected: the resolved local image ID is part of the drift hash, so re-pointing or rebuilding the tag rebuilds the container, its network namespace, and its firewall; the NetBird peer identity and the WireGuard private key are retained |
| `wg0` exists but private service is unreachable | peer keys/endpoint/AllowedIPs, NAT keepalive, MTU, DNS, service authentication |
| command fails after a firewall change | the daemon recreates a drifted sidecar once; if the drift returns, find what edits the table (another agent, a host tool) and stop it |
