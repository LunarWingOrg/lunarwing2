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

### mt-admin: an unsafe health library no longer bricks the CLI (WI#703)

`lunarwing-mt-admin.sh` used to run the installed-health-library guard as a bare
statement while it was still sourcing its libraries. On a host where
`/usr/local/lib/lunarwing-health` (or any parent) is not root-owned, or is
group/other-writable, that guard aborted **every** subcommand before dispatch —
including `--help` and `ensure_health_pipeline`/`add-tenant`/`upgrade-tenant`,
the commands that repair the pipeline.

The CLI now loads, reports the refusal with the failing path component and the
repair, and refuses only OpenRC operations (exit 126) until the tree is fixed.
Non-OpenRC commands keep working. Repair the library with
`ensure_health_pipeline`, `add-tenant`, or `upgrade-tenant`, or by hand:
`chown -R root:root /usr/local/lib/lunarwing-health && chmod -R go-w
/usr/local/lib/lunarwing-health` (and the named parent, if any). A host on the
pre-WI#687 layout therefore stays usable for non-OpenRC administration but
cannot render or validate OpenRC units until the library is re-owned.

The checkout's `ic-infrastructure-health-check/openrc-proxy-unit-contract.sh`
is also refused when it is a symbolic link or group/other-writable, since root
sources it; OpenRC commands then refuse with the file named instead of loading
a writable contract.

See the [v2.1.9.0 release notes](RELEASE-v2.1.9.0.md) for the previous release.
