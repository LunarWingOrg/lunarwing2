# LunarWing Logging and Diagnostics

LunarWing emits structured events through `tracing`. Do not rely on source line numbers when diagnosing a release; log locations move as the runtime changes.

**Revalidated against source through 2026-11-19 (v2.2.1.0).** The default filter
(`lunarwing=info,tower_http=warn`), runtime level endpoints, write-time
scrubbing, `SANDBOX_TENANT_LOGS`, and init-agnostic MT diagnostics path match
the current tree. Since v2.2.0.0 (WI#579), the daemon also logs
`Starting LunarWing v<package-version>` at INFO immediately after initializing
tracing; look for it when INFO is enabled to confirm the binary version.

## Choose a Log Level

The web gateway initializes the filter from `RUST_LOG`. Its default is:

```text
lunarwing=info,tower_http=warn
```

Useful temporary filters include:

```bash
RUST_LOG=lunarwing=debug,tower_http=warn
RUST_LOG=lunarwing=trace,tower_http=warn
```

`trace` can be noisy and may expose more operational metadata than normal. Capture only the shortest interval needed, review logs before sharing them, and never publish secrets or tenant data.

The authenticated web Logs view can change the LunarWing level at runtime to `trace`, `debug`, `info`, `warn`, or `error`. This updates the in-process filter only; it does not rewrite the service environment or survive a restart. The same authenticated API is exposed at `GET/PUT /api/logs/level`, with live events at `/api/logs/events` and a bounded download at `/api/logs/download`.

## Read Service Logs

For a single-node systemd user service:

```bash
systemctl --user status lunarwing.service
journalctl --user -u lunarwing.service --since "15 minutes ago"
journalctl --user -u lunarwing.service -f
```

For a multi-tenant installation, start with the init-agnostic admin command:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh status <tenant>
sudo ic/scripts/lunarwing-mt-admin.sh doctor
```

Tenant units run under the tenant account's systemd user manager on systemd hosts and as OpenRC services on Gentoo. Do not diagnose a tenant with bare system-bus `systemctl`; it can report an inactive unit while the tenant is healthy.

The daemon scrubs `tracing` events at write time (2.1.5.0, WI#287) for stderr,
the web Logs stream, and the dedicated copy at `<base>/logs/sandbox/daemon.log`
(multi-tenant: `<tenant home>/lunarwing/state/logs/sandbox/`, rotated to
`daemon.log.1` at 8 MiB). Where stderr lands depends on the layout: the journal
on systemd (single-node and multi-tenant), `<base>/logs/daemon.stderr.log`
under `lunarwing service install` on OpenRC and launchd,
`/var/log/lunarwing/lunarwing.err` with the committed OpenRC template, and
`<tenant home>/lunarwing/logs/lunarwing.err` on multi-tenant OpenRC. Scrubbing
covers known secret shapes, secret-named environment values, URL passwords,
and registered runtime secrets such as the gateway token and secrets-store
values. Standalone 64-hex values, including SHA-256 digests, are redacted.
Raw `println!`/panic output and other services' logs are not scrubbed, so
still review logs before sharing them.

The agent can read the copy from inside the shell sandbox at
`/tenant-logs/daemon.log` (read-only). `SANDBOX_TENANT_LOGS` applies an extra
filter to events that pass the daemon's runtime filter: `info` (the default
rendered by mt-admin) caps detail at INFO, `debug` caps it at DEBUG, and `off`
writes no copy and mounts nothing. Tool-call start and success events are
DEBUG. To see them in the sandbox, set `SANDBOX_TENANT_LOGS=debug` and also
enable DEBUG in the daemon (`PUT /api/logs/level` or `RUST_LOG=lunarwing=debug`).
On a single-node daemon shared by several users, use `off`: their agents
would otherwise read the same copy. `/tenant-logs` is omitted when the runtime
cannot map the sandbox user to the daemon's UID (Podman older than 4.3 or an
unknown version, or Podman 4.x when the daemon UID is not 1000).

## What to Capture

When reporting a problem, include:

- the LunarWing commit or release version;
- the active backend and init system, without credentials;
- the failing command or request and timestamp;
- a short log window covering the failure;
- the MT admin `status` or `doctor` result for multi-tenant hosts;
- reproduction steps and whether the issue survives a clean service restart.

Prefer stable event messages and structured fields such as session, thread, turn, job, or tool identifiers. Exact source file line numbers are not a durable troubleshooting interface.
