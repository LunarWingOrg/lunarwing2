# Release Notes for LunarWing v2.2.0.0

## Codename: Shinkai (深海) — The Deep Sea

**Release Date:** Unreleased

## Compatibility

### Sandbox tunnel mode: no uplink DNS channel (WI#724)

`SANDBOX_NETWORK=tunnel` no longer has a DNS channel to the runtime resolvers.
NetBird (the default provider) allows DNS only to its own `wt0` listener, which
joined runs reach over loopback; the sidecar's firewall derives that address
from `netbird status --ipv4` and never from `/etc/resolv.conf`.

**Operator action before upgrading.** A tenant already on
`SANDBOX_NETWORK=tunnel` with NetBird resolves peer names only after this
upgrade unless a NetBird nameserver group covers the names it needs. Public
names such as package mirrors stop resolving at the first start after the
upgrade, because the runtime-resolver allowance is gone. Configure a nameserver
group (match domain or primary) whose upstream is reachable through the overlay
before upgrading, or accept peer-only resolution.

The same release adds a NetBird bump checklist to
`docs/guides/SANDBOX_TUNNEL_NETWORKING.md`. The closure depends on NetBird not
marking its resolver sockets: a version that does reopens the path through which
the client falls back to the backup file's nameservers.

### SSH private keys and host keys: Ed25519 or ECDSA (WI#368, WI#616)

LunarWing's SSH harness accepts only Ed25519 and ECDSA private keys. RSA key
material is rejected by its parsed algorithm before an identity can be loaded,
including for worker mode and `ssh_git`.

The russh-based client used by built-in `ssh`, WASM `ssh`, and `ssh_git`'s
AcceptFirst pin-learning preflight offers only Ed25519 and ECDSA host key
algorithms. **RSA host keys are not supported on these paths.** An RSA-only
server now fails during algorithm negotiation with an error that names the RSA
host key, instead of the misleading `WrongServerSig`. Configure an Ed25519 or
ECDSA host key and replace any `ssh-rsa` `known_host_key` or learned pin.

Existing encrypted RSA host key pins are not deleted automatically. Loading one
logs a warning that it cannot authenticate a russh connection. Worker-mode
OpenSSH and `ssh_git`'s post-pin OpenSSH transport retain their own algorithm
policies; the negotiation change above applies specifically to the shared russh
client.

Russh's optional `rsa` feature remains disabled, and the `rsa` crate remains out
of LunarWing's dependency graph.

See the [v2.1.9.0 release notes](RELEASE-v2.1.9.0.md) for the previous release.
