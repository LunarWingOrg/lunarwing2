# Release Notes for LunarWing v2.2.0.0

## Codename: Shinkai (深海) — The Deep Sea

**Release Date:** Unreleased

## Compatibility

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

## Security

### Legacy `proxy` mode now injects the LunarWing Cloud key (WI#655, WI#721)

The sandbox HTTP proxy's credential resolver now reads the daemon's layered
environment: the process environment first, then runtime overrides, then
secrets injected from the secrets store. Before, the default credential
mappings could not resolve on a standard install — the daemon's keys live in
the secrets store, not in the process environment — so the proxy logged
`credential not found` and forwarded the request without a key.

A tenant whose `SANDBOX_NETWORK` is the legacy `proxy` mode therefore now gets
its **`LUNARWING_CLOUD_API_KEY`** injected as a bearer token into plain-HTTP
requests that sandbox code makes to `api.lunarwing.org`. As
`ic/src/NETWORK_SECURITY.md` documents, a legacy-mode injected secret is sent
on the cleartext network path, though it still never enters the container's
environment or filesystem. For a tenant already on that mode, the first
request after the upgrade is the first time this key leaves that way.
`OPENAI_API_KEY` and `ANTHROPIC_API_KEY` follow only when a provider in the
user's `providers.json`, or a dotenv value, puts them into an overlay.

Only the legacy `proxy` mode is affected. `proxy-enforced`, the Linux default,
is built with no credential mappings and is unchanged; `none` and `tunnel` do
not use this proxy. On rootless Podman with pasta the legacy mode cannot reach
the proxy at all (WI#717), so nothing is injected there; this applies where the
proxy is reachable, which is the Docker runtime and whatever WI#717's fix makes
work. Move a tenant to `proxy-enforced` if the outbound key is not wanted.

See the [v2.1.9.0 release notes](RELEASE-v2.1.9.0.md) for the previous release.
