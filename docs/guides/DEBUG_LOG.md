# LunarWing Logging and Diagnostics

LunarWing emits structured events through `tracing`. Do not rely on source line numbers when diagnosing a release; log locations move as the runtime changes.

**Revalidated against source 2026-09-17 (v2.1.2.0); re-checked 2026-09-27 (v2.1.3.0), 2026-10-01 (v2.1.4.0), 2026-10-04 (v2.1.5.0), and 2026-10-14 (v2.1.6.0).** Default filter
(`lunarwing=info,tower_http=warn`), runtime level endpoints, and the
init-agnostic MT diagnostics path were re-checked against the current tree;
no changes were needed (the WI#287 write-time scrubbing and
`SANDBOX_TENANT_LOGS` contract below are unchanged in v2.1.6.0).

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

Every daemon log sink is secret-scrubbed at write time (2.1.5.0, WI#287): stderr, the web Logs stream, and a dedicated copy at `<base>/logs/sandbox/daemon.log` (multi-tenant: `<tenant home>/lunarwing/state/logs/sandbox/`, rotated to `daemon.log.1` at 8 MiB). Where stderr lands depends on the layout: the journal on systemd (single-node and multi-tenant), `<base>/logs/daemon.stderr.log` under `lunarwing service install` on OpenRC and launchd, `/var/log/lunarwing/lunarwing.err` with the committed OpenRC template, and `<tenant home>/lunarwing/logs/lunarwing.err` on multi-tenant OpenRC. Scrubbing covers known secret shapes, the values of secret-named environment variables, URL passwords, and secrets the daemon learns at runtime (the gateway token, secrets-store values). Any standalone 64-hex value, SHA-256 digests included, shows as `[REDACTED]`. Raw `println!`/panic output and other services' logs are not scrubbed, so still review logs before sharing them.

The agent reads the copy itself from inside the shell sandbox at `/tenant-logs/daemon.log` (read-only), so "what did the daemon actually do" questions can be answered without host access. `SANDBOX_TENANT_LOGS` controls it: `info` (default; mt-admin renders it) keeps INFO and above whatever the runtime level, `debug` follows the runtime level up to DEBUG, and `off` writes no copy and mounts nothing. Per-tool-call lines (including `memory_write` calls) are DEBUG: at the defaults the agent sees INFO and above only, so set `SANDBOX_TENANT_LOGS=debug` and raise the level (`PUT /api/logs/level` or `RUST_LOG=lunarwing=debug`) to expose them. On a single-node daemon shared by several users, use `off`: every user's agent would read the same copy. `/tenant-logs` is absent where the container runtime cannot read the daemon-owned copy (podman older than 4.3, or podman 4.x when the daemon uid is not 1000).

## What to Capture

When reporting a problem, include:

- the LunarWing commit or release version;
- the active backend and init system, without credentials;
- the failing command or request and timestamp;
- a short log window covering the failure;
- the MT admin `status` or `doctor` result for multi-tenant hosts;
- reproduction steps and whether the issue survives a clean service restart.

Prefer stable event messages and structured fields such as session, thread, turn, job, or tool identifiers. Exact source file line numbers are not a durable troubleshooting interface.
