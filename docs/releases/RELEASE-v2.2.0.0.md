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

### mt-admin upgrades install the operator checkout's target (WI#708, WI#723)

Update the operator checkout and fetch the intended target before running
`upgrade-tenant`, whether through mt-admin or `ic/scripts/upgrade-tenant.sh`.
The target must resolve in that operator checkout; the upgrade checks out and
builds the operator-selected commit for the sandbox image and every enabled
worker lane. It no longer resolves the build revision from the tenant's clone.
For a named branch, `refs/remotes/origin/<name>` takes precedence over a local
branch, so a stale local branch cannot silently select an older installation.

### mt-admin: scoped health-library refusal and reachable repairs (WI#722)

Previously, an unsafe `/usr/local/lib/lunarwing-health` or parent directory
aborted every mt-admin subcommand while libraries were loading, including
`--help` and the commands needed to repair an old installation. mt-admin now
skips only the OpenRC proxy contract, reports the failing path component, and
keeps `--help` available. A checkout that is not root-owned gains no new check.
Where mt-admin's own directory is root-owned (a scheduler snapshot, a
root-owned checkout), the contract source is checked wherever it lies (WI#725);
a refused source is skipped in the same way, and its refusal names that
checkout, not the library, as the thing to correct. A refused source is also
never copied into a root-owned tree: the health library install and the
staging of the Study Notes scheduler snapshot stop with the same refusal, on
systemd and on OpenRC.

On OpenRC, `reinstall-health-library` is the only non-help command available
while the contract is refused; other commands remain available on positively
detected systemd hosts. For an unsafe library or entry, reinstall from a
separate trusted checkout (a fresh root-owned clone at a verified commit):

```bash
sudo ic/scripts/lunarwing-mt-admin.sh reinstall-health-library /path/to/trusted/checkout
```

The repair replaces the library without rendering or changing OpenRC units.
Installs refuse to copy from the installed library itself while it fails the
safety check, so mt-admin does not republish an unsafe tree as trusted. A
library path that is a symbolic link is repaired by the same reinstall, which
replaces the link. When a health library refusal names an unsafe parent
directory instead, run the printed non-recursive `chown root:root -- <parent>
&& chmod go-w -- <parent>` repair on that parent only; reinstalling cannot
repair its own parent. If that parent is a symbolic link, the health launcher
and the install say so instead: replace the link with a real root-owned
directory.

## Security and upgrade actions

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

### Root health library refresh and refusal diagnostics (WI#687, WI#696, WI#725)

A host whose `/usr/local/lib/lunarwing-health` was installed before WI#687 can
still contain files owned by the checkout user, and its old
`/usr/local/sbin/lunarwing-mt-health` launcher has no ownership or permission
check. **Updating the source checkout alone does not repair that host or close
this exposure.** Run `add-tenant` or `upgrade-tenant` through the updated
mt-admin, or run `ic/scripts/enable-health-fleet.sh`, on every affected host;
the refresh replaces the old library and launcher.

The refreshed installer uses a root-only staging tree, refuses linked or
non-regular sources, and does not inherit checkout modes, ACLs, or set-ID bits.
Every file the library runs is left executable, including a
`lunarwing-health-metrics` binary the checkout ships: earlier refreshes
installed it 0644, so a listener that could not start is repaired by the next
`add-tenant`, `upgrade-tenant` or `enable-health-fleet.sh`. An install under a
set-group-ID parent now succeeds instead of refusing its own stage; the stage
clears the inherited bit. The new launcher validates the installed tree before
loading its environment and reports the failing path component on stderr and to
syslog. Root-owned mt-admin snapshots apply the same source-tree check to
linked installed libraries and real-directory OpenRC contract siblings.

## Upgrade Notes

### Root-side Study Notes sync snapshot (WI#715)

The hourly root-side Study Notes source refresh runs from a private copy of
mt-admin under `/usr/local/lib/lunarwing-study-notes-sync/releases/<stamp>/`
(the stable `current` symlink), not from the operator checkout. Before this
release that copy was staged only by `configure-study-mcp
--enable-knowledge-sync` and `add-tenant --with-study-mcp`, so `upgrade-tenant`
delivered a new daemon but left the root-side sync running whatever mt-admin
code existed the day the opt-in was last enabled — including security fixes
such as the WI#445 trusted-home gate.

`upgrade-tenant` now restages the snapshot from the upgraded-from checkout when
a scheduler is already installed; a host without one still gets none. Each
release records the commit it was staged from in its `STAGED-FROM` file, and
`lunarwing-mt-admin.sh doctor` prints that commit and warns when it lags the
operator checkout.

**On a host that already has the scheduler:** the first `upgrade-tenant`
performed with this release restages it automatically. If you cannot upgrade
that host yet, or want the explicit re-arm, run
`configure-study-mcp <tenant> --enable-knowledge-sync` once after upgrading;
until then the hourly job keeps executing the old snapshot.

See the [v2.1.9.0 release notes](RELEASE-v2.1.9.0.md) for the previous release.
