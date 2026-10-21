# Worker Container Images

Several container images provide sandboxed and persistent job execution for LunarWing.

Nanocode is retired and has no active source, build, or runtime image definition.
See [`NANOCODE-REMOVAL.md`](NANOCODE-REMOVAL.md) for upgrade behavior.

## Hub-handshake credential boundary (WI#373)

Every worker container receives `AGENT_AUTH_TOKEN` — the tenant-scoped
`WORKER_AUTH_TOKEN` (WI#210: mt-admin mints and persists a scoped token on the
first lifecycle pass; the gateway bearer is substituted only as a loudly warned
last resort when the worker env file is unwritable) that authenticates the
daemon→worker WebSocket handshake. Only the **bridge process** needs it. Task
code is untrusted, so no task process may inherit it: one value is shared by
all four worker lanes of the tenant, so a leaked token lets any job
impersonate the daemon toward **every** worker WebSocket port of that tenant
(and, where the last-resort fallback still applies, toward the whole gateway
surface).

What follows is **env hygiene, not a confidentiality boundary** (MR !1586
run-98 FY-1): the token still lives in the environment of long-lived
processes that run as the task's own uid — tini (PID 1), the entrypoint
shell, `health_server.py`, and the bridge (pebble runs as root in-container
on rootless hosts, and as the tenant uid on rootful hosts since WI#422) —
so task code can simply read `/proc/1/environ` (same-uid ptrace-read, which
Yama does not block) and get it back. The strip blocks the *accidental* routes —
env dumps into transcripts, logs, and LLM context, which is how WI#373 was
observed — and is the same trust posture as the authority-domain rule below
(one container = one uid = one authority domain). Real isolation (credential
out of the container env; tasks under a separate uid from the bridge)
landed as **WI#400** for the kimi/opencode/pebble lanes — see the
"Task-uid split (WI#400)" subsection below.

Each lane enforces the strip at its task-process boundary:

- **Kimi** — the executor deletes `AGENT_AUTH_TOKEN` from the assembled
  per-task env after all merges (bridge env spread, task-supplied
  `context.environment`, proxy `extraEnv`), so neither inheritance nor
  task-supplied env can deliver it. Pinned by the executor self-test. The
  one-shot `cli` mode execs under `env -u AGENT_AUTH_TOKEN` (shape pinned by
  the entrypoint config self-test).
- **OpenCode / OpenCode 2** — the entrypoint launches `opencode serve` via
  `env -u AGENT_AUTH_TOKEN`: the server process runs all task code (every
  bash tool child inherits its env), while the separately exec'd bridge keeps
  its own copy. The one-shot `cli` and `acp` modes exec the task process the
  same way. Pinned by the worker entrypoint harness, which asserts both
  directions (serve sees it absent, bridge sees it present) plus every
  one-shot mode.
- **Pebble** — `spawn_pebble` rebuilds the child env via `env_clear()` +
  `strip_bridge_credentials` (everything else preserved: PATH, locale,
  `PEBBLE_*` provider variables) and drops task-supplied re-injections.
  Pinned by a self-exec regression test that carries the token in the parent
  env and probes the real spawned environment through a stub. The one-shot
  `cli` mode execs under `env -u AGENT_AUTH_TOKEN` (harness-pinned) — this is
  the only mode today where the strip is total, because `exec env -u …`
  replaces PID 1 and no long-lived carrier process remains.

Residual, deliberate (run-98 FY-1): the strip does **not** hide the token
from task code that goes looking for it — in lanes without the WI#400 split
(opencode2 today; every lane's pre-WI#400 image), `/proc/1/environ` and the
bridge process's environ still contain it (see the env-hygiene paragraph
above and the authority-domain section below). The OpenCode serve password
(`OPENCODE_SERVER_PASSWORD`) is necessarily present in the serve process env —
the server enforces it itself, and task code already runs inside that trust
boundary. The LLM provider keys (`KIMI_API_KEY`, `TENSORZERO_API_KEY`, …) are
deliberately *not* stripped: task execution needs them.

### Task-uid split (WI#400)

The kimi, opencode and pebble lanes now implement WI#400's real isolation on
rootless podman (the multi-tenant default). The rootful pebble launch is the
one exception: it runs the whole container as the tenant uid with no
bridge/task split, by design — see the rootful paragraph at the end of this
section and "Launch identity (WI#422)" further down.

- **Credential out of the environment.** The container entrypoint (root)
  writes the hub token to `/run/lunarwing/agent_auth_token` (directory
  `0700` root, file `0400` root), unsets `AGENT_AUTH_TOKEN`, and exports
  only `AGENT_AUTH_TOKEN_FILE`; the bridge reads the file. Nothing
  long-lived carries the token in its environment any more.
- **Tasks under a separate uid.** The bridge stays container root; every
  spawned task process (and opencode's `serve`, which hosts task code) drops
  to the image's task uid/gid via
  `setpriv --reuid --regid --clear-groups --ambient-caps=-all --inh-caps=-all
  --no-new-privs --bounding-set=-all` (both capability clearances and
  `no_new_privs` are mandatory: without them the task keeps ambient
  SETUID/SETGID or regains privilege through a setuid/file-capability
  binary; the bounding-set clear is run-170 LP-7 belt-and-braces — with it
  the dropped task's bounding set is empty, and it needs CAP_SETPCAP in the
  still-root setpriv, which the trim below carries). `/proc/1/environ`,
  the bridge's environ, and the token file all give the task uid EACCES.
  The opencode bridge is exec-ed from the root-owned `/app`, never from the
  task-writable workspace: bun loads `bunfig.toml` preloads and `.env` from
  its working directory, so a task-planted file there would otherwise run
  as root at the next container start (run-170 LP-1). The opencode bridge's
  credential resolver fails closed when the file pointer is set but the
  file is unreadable or empty (run-170 LP-4), like the kimi and pebble ones.
  The pinned identities are validated everywhere they parse (the TS
  `taskIdentity()` helpers, both entrypoints, the pebble executor): zero or
  non-numeric ids fail closed, because a zero id would make the drop a
  no-op while callers keep treating the split as active (LP-17 — added to
  the WI#400 description 2026-10-20, numbered after the run-170 LP series;
  the helpers also refuse partially numeric ids like `1000x`, LV-18).
  Kimi task-side setup (`entrypoint-task-setup.sh`) runs as the task uid —
  never as root — because everything it writes (`~/.gitconfig`, `~/.ssh`,
  `$KIMI_CODE_HOME`) is task state.
- **Trimmed root bridge.** The worker containers run with
  `--cap-drop=ALL --cap-add=CHOWN --cap-add=DAC_OVERRIDE --cap-add=FOWNER
  --cap-add=KILL --cap-add=SETUID --cap-add=SETGID --cap-add=SETPCAP` plus
  `--security-opt no-new-privileges` (both the imperative `_ctr run` paths
  and the Quadlet `PodmanArgs`; the contracts record `cap-trim` so existing
  containers force-recreate). The bridge needs: CHOWN for the
  relay-socket/cron-leaf handoff, SETUID/SETGID for the drop, SETPCAP for
  the drop's `--bounding-set=-all` (run-170 LP-7: `PR_CAPBSET_DROP` requires
  it — verified empirically; SETPCAP can only shuffle capabilities within
  the container's bounding set, which `--cap-drop=ALL` already limits to the
  carried set), KILL for
  timeout/cancel of task-uid children, and DAC_OVERRIDE for cron workspace
  creation inside the task-owned workspace root (on keep-id rootless, the
  workspace presents as task-owned 2770). The run-166 root-bridge probe
  showed each of those five individually required on the pebble lane;
  FOWNER is not individually required there — the LP-13 socket handoff
  chmods before it chowns, so the mode change runs while root still owns
  the socket — but is retained for metadata
  passes (chmod/re-assert) over task-created files the bridge does not own
  — dropping it is on the table the next time the set is re-proved end to
  end on every lane.
- **Standing residual (run-184 ME-1, open):** the kimi and opencode image
  `PATH` entries start with directories the task uid owns
  (`/home/<lane>/.cargo/bin`, `/home/<lane>/go/bin`), and several
  root-side resolutions use that same `PATH` (the image `HEALTHCHECK`,
  the kimi bridge's per-task `setpriv` lookup, the root entrypoint on an
  in-place restart) — so task-planted code can get run as container root,
  which would undo WI#400's split on those lanes. Pebble is not affected
  (its PATH entries are root-owned). The run-184 fix (root-owned image
  PATH + task toolchain dirs only at the drop, absolute-path health
  probes, an owner-refusing `resolveFromBridgePath`) is recorded on the
  review and applies to the already-root lanes whenever they are next
  reworked; how those lanes come back in line with the maintainer's
  no-root policy is the maintainer's decision.
- **Cron workspace ownership.** The cron bootstrap (`create_cron_workspace`
  / the TS `resolveTaskWorkspace`) runs in the root bridge, so EVERY
  directory the walk creates — intermediates and leaf alike, or the task
  could not create siblings under a root-owned intermediate — is
  `fchown`'d to the task identity through the `O_DIRECTORY|O_NOFOLLOW`
  descriptor the walk just opened; pre-existing components are never
  re-chowned.
- **Tool-proxy socket directory.** The relay's socket directory stays
  root-owned `0711` — traversable by the task uid so its shim can connect,
  but neither listable nor writable, so task code cannot discover, rename,
  delete, or plant entries among the root bridge's sockets. A symlinked
  directory is refused instead of followed on every lane (the TS relays
  check with `lstat` and re-assert ownership through an `O_NOFOLLOW`
  directory descriptor, run-170 LP-5; pebble also refuses a foreign-owned
  directory, while the TS relays return a leftover task-owned one to root
  `0711`). Only the
  per-task socket is handed to the task identity (mode `0600`, right after
  bind and before the task spawns); with no identity pinned (host dev runs
  and tests) the socket is left untouched.
- **opencode2 is NOT migrated** (its image still runs `USER 10003:10003`
  and mt-admin's image-user guard continues to require exactly that). The
  !1829 attempt ported the root-launcher shape to this lane and was
  rejected: the maintainer's policy is that **no worker container should
  run as root**, which the `USER 0:0` migration violates — the same policy
  question now hangs over the kimi/opencode/pebble root bridges
  themselves. WI#449's remainder for this lane is a non-root split design
  (the run-184 review's sketch: a bridge container holding the hub
  credential under one uid plus a task container under another, sharing
  the workspace volume over a unix socket), which needs its own work item.

**Rootful pebble (run-164 KW-5, resolved by WI#422):** on rootful
runtimes there is no user namespace, so an in-container task uid (10004)
would be a bare host uid shared by every tenant's pebble lane, and a
container escape from one tenant's lane would land on write access to every
tenant's pebble workspace. That design is retired: rootful pebble now runs
the whole container as the tenant's own uid (`--user`, "Launch identity
(WI#422)" below) with `PEBBLE_TASK_UID=inherit` and
`PEBBLE_TASK_GID=inherit`, so each tenant's lane is its own host identity
and the workspace stays tenant-owned — `prepare_pebble_workspace` never
chowns it, and a workspace an older create had handed to a task uid is
refused rather than accepted (the run-166 LD-9 acceptance went away with
the handoff). The trade-off is explicit: there is no bridge/task split on
rootful hosts. Task code shares the bridge's identity and environment, so
the credential boundary WI#400 provides exists on the rootless lane only.
The entrypoint and the executor accept the `inherit` pair as that declared
shared-identity launch (a mixed pair fails closed) and keep refusing an
undeclared one. Rootless podman keep-id is unaffected on the task side (the
task uid maps to the owning tenant account), and the bridge side is pinned
by the image: the pebble image now sets `USER 0:0` like kimi and opencode,
because under keep-id a USER-less image would start the entrypoint and
bridge as the keep-id uid — the same uid every task drops to — making the
split a no-op (run-166 LD-8). mt-admin's image-user guard requires `0:0`
for pebble, and the websocket entrypoint plus the executor both refuse to
run when the bridge lands on the task uid without the `inherit`
declaration.

Note: before WI#400 the pebble image set no `USER`, so under rootless
keep-id the bridge ran as the tenant uid (not root) even though the
rootful path looked root-like; with `USER 0:0` the bridge side is now
container root wherever nothing overrides it — the rootful pebble launch
does override it with `--user <tenant uid>` (WI#422), which is why that lane
declares `PEBBLE_TASK_UID=inherit` instead of a split.

## WebSocket message limits

The daemon accepts external-worker messages and individual WebSocket frames
up to **16 MiB**, including envelope overhead. Fragmentation does not bypass
the total-message limit. This applies to every worker, even with an empty
`tool_allowlist`; exceeding the bound fails the job with a WebSocket size
error and drops the connection. Proxied `tool_call` messages additionally
have an independent **256 KiB** limit before dispatch.

Kimi's `WS_MAX_PAYLOAD_BYTES` defaults to 1,048,576 bytes and can be raised to
16,777,216 bytes; its serializer caps the whole envelope. The daemon limit
accommodates that maximum. OpenCode and Pebble bridges do not currently cap
their outgoing envelopes, so outputs above 16 MiB still exceed the transport
contract. Results/progress between 1 and 16 MiB are accepted (MR !1540 DX-1/DX-3).
Note the interplay with the WI#324 tool proxy (MR !1549 EF-9): raising the
hub's tool-output safety cap (`SAFETY_MAX_OUTPUT_LENGTH`, default 100,000
bytes) past the worker bridge's inbound `maxPayload` (Kimi default 1 MiB)
means a large `tool_result` trips the worker's own WS layer — ws 8.21.1
closes the connection with a 1009 (message too big) error, and the bridge
then aborts the task that was running on that socket (MR !1561 EV-5). Keep
the worker's `WS_MAX_PAYLOAD_BYTES` at or above the hub cap when tuning it.

## Worker-side tool proxy bridge (WI#324)

The kimi worker image ships the WI#324 worker-side bridge: a per-task unix
socket relay in `lunarwing_bridge.ts` plus a thin stdio MCP server
(`scripts/lunarwing_mcp_server.ts`) registered in `$KIMI_CODE_HOME/mcp.json`
by the entrypoint (write-if-absent; set `"enabled": false` there to opt out).
The kimi CLI spawns the shim per task session; the bridge injects
`LUNARWING_TASK_ID` / `LUNARWING_TOOL_PROXY_SOCKET` per task, and the shim
translates MCP `tools/list` / `tools/call` into `tool_list` / `tool_call`
frames on the existing hub WebSocket. Tools appear to the agent as
`mcp__lunarwing__<name>`; the hub stays deny-by-default until the worker's
`tool_allowlist` (or `cron.tool_allowlist` for tick jobs — MR !1506 CQ-10)
grants phase-1 names. On multi-tenant hosts the grant is rendered by
`lunarwing-mt-admin.sh configure-worker-tools <tenant> <worker> <tools|none>`
(it validates against the daemon's phase-1 set, refuses `mode = "cron"`
entries, and needs `restart-tenant` to load; editing `config.toml` by hand
works on single-node installs). `LUNARWING_TOOL_PROXY_TIMEOUT_MS` bounds one
proxied call (default 300000, capped 900000).

The opencode worker image ships the same relay in its
`scripts/lunarwing_bridge.ts`, but the in-task half is an opencode plugin
(`scripts/lunarwing_tool_plugin.ts`, registered by absolute path in
`config/opencode.json`) rather than an MCP shim: `opencode serve` owns MCP
servers per project Instance with the serve process's frozen environment,
so a per-task socket env cannot reach an MCP child. The plugin statically
registers the phase-1 tools as `lunarwing_<name>`; each call resolves the
session's task id from its title (`lunarwing-opencode-<task_id>`, with
subagent child sessions resolved through the root session via `parentID`)
through the plugin host's authenticated client (first-title-wins cached) and
relays over the task's socket. The static tools are visible even without a
grant — ungranted calls return the hub's denial as a tool error. The
first-title-wins cache is not an isolation boundary: any process in the
container can rename a session before its first call, create a session with
another task's title or `parentID` over the loopback serve API, or connect
to another task's socket directly — all of these stay inside the
container's single hub-enforced grant and change only which job a call is
attributed to. The opencode2 worker image ships the same relay plus a v2
serve-process plugin (`scripts/lunarwing_tool_plugin/index.ts`, a directory
plugin registered by absolute path in `config/opencode2.json`'s `plugins`
array — v2 drops configured plugin file paths): the v2 promise plugin API
registers the phase-1 tools via `ctx.tool.transform` as direct
(`codemode: false`) tools with plain JSON Schema inputs, each `execute()`
resolves the calling session's task id from its title
(`lunarwing-opencode2-<task_id>`) through the plugin context's in-process
session API, and relays over the task's socket with the same
first-title-wins caching and the same "not an isolation boundary" limits.

The pebble worker ships the same relay in Rust, inside the
`pebble4lunarwing` bridge binary (`src/tool_proxy.rs`): one relay per hub
connection, one per-task unix socket per accepted task, the same NDJSON IPC
shapes, identifier bounds and locally pre-checked 256 KiB `tool_call` frame
cap, and the same EF-2 never-fail lifecycle / EF-9+EV-4
fail-fast-on-hub-close semantics. The in-task half is a thin stdio MCP
server like the kimi lane's, also inside the same binary
(`pebble4lunarwing mcp-shim`, `src/mcp_shim.rs`): pebble discovers
`mcpServers.lunarwing` from the baked
`$PEBBLE_CONFIG_HOME/settings.json` (`/etc/lunarwing/pebble`; root-owned
`0555`/`0444`, and since the WI#400 split that DOES protect it from task
code, which now runs as the task uid) and spawns a
fresh shim for tool discovery and for each tool call, passing its process env through to the MCP child, so the
executor's per-task `LUNARWING_TASK_ID` / `LUNARWING_TOOL_PROXY_SOCKET`
injection (applied after task-supplied env, and stripped from both
inherited and task-supplied env alongside `AGENT_AUTH_TOKEN`) reaches the
shim. Tools appear as `mcp__lunarwing__<name>`; without the proxy env the
shim advertises zero tools. Note pebble merges project-scope
`.pebble/settings.json` from the task workspace, so workspace content can
shadow or disable the `lunarwing` entry — the hub stays authoritative, same
as the kimi loader (EF-10). The flip side of the read-only config home
(run-166 LD-16): pebble's own state writes into
`$PEBBLE_CONFIG_HOME` now fail under the split, so its model state
(`state.json`, `model-catalogs.json`, cached credentials) is not persisted
across runs; some call sites ignore those errors and some surface them.
Seeding a task-owned `~/.pebble` with the baked `settings.json` is the
known follow-up shape if that ever becomes load-bearing. See
[`docs/proposals/EXTERNAL-WORKER-TOOL-PROXY.md`](../proposals/EXTERNAL-WORKER-TOOL-PROXY.md)
for the wire contract.

**One worker container is one authority domain** (MR !1549 EF-1): every
concurrent and successive job in a container runs as the same uid, and any
process in it can connect to another task's relay socket and act with that
job's grant — including a detached leftover process waiting for later tasks.
Per-user isolation therefore requires per-user workers. Because a tick job
could borrow a concurrent interactive job's grant this way, a `mode = "cron"`
worker's worker-level `tool_allowlist` is stripped at manager load (logged
as an error): a cron worker container never holds more than its
`cron.tool_allowlist`, and ordinary dispatches to it get no proxied tools.
The strip is applied per worker entry, but the authority domain is the
container: never point an entry that has a `tool_allowlist` at the same
endpoint as a `mode = "cron"` entry — give a cron mission its own
container. Two cron entries pointing at the same container likewise
share one authority domain (one mission's tick can borrow the other's
`cron.tool_allowlist` through another tick's relay socket, concurrent
or later — a leftover process can wait for it), so the manager now
detects that pairing at load (WI#396): cron entries are grouped by
normalized endpoint key, and when a shared listener's entries carry
*differing* `cron.tool_allowlist` grants the manager strips every cron
grant on that listener with an `error!` (fail closed); identical grants
are the same authority on both sides and stay, with a `warn!`. The
strip is a single pass: an entry stripped for one listener can still,
on another listener it shares with identical-grant entries, borrow a
grant equal to its own configured list — no mission gains authority
beyond its configuration. (The manager also strips the worker-level list of a non-cron
entry whose endpoint shares a cron entry's container. The comparison uses
a normalized endpoint key — loopback spellings folded into one class,
host lowercased, scheme-default port applied, path and query ignored —
so one host:port that path-routes several containers strips all of them
(fails closed). The key names a host:port listener, not the container
itself: a container that is also published on a second host port, or
reached behind a proxy on another port, is not caught; a real host
literally named `loopback` folds into the loopback class and can only
over-strip, which fails closed. Those, and exotic aliases such as
`/etc/hosts` names, remain the operator's responsibility — the rule
stands on its own.) A borrowed
call is recorded in the `job_events` — and counted against the
100-frame budget — of the job whose relay socket was used, not of the
job that issued it.

## Generic shell sandbox network policy (WI#154/155)

The generic shell sandbox (`ic/src/sandbox/`) defaults to
`SANDBOX_NETWORK=none`, in code and in mt-admin-rendered multi-tenant envs.
`proxy-enforced` keeps `--network none` and exposes only a per-execution Unix
relay to the public-only allowlist proxy — local Linux runtime and a
Python-3-equipped image are required (the bundled `docker/sandbox.Dockerfile`
image already includes Python 3). `proxy` remains as the legacy advisory mode.

Rootless podman uses keep-id; rootless docker maps container root onto the
host user; rootful docker maps numeric UIDs and refuses writable workspaces
under a root daemon (never container root). The `--pids-limit` is pinned to
256; the runtime's default seccomp filter is the supported contract. The
cleanup path is the `ContainerCleanupGuard` detached kill/rm plus the
API-independent bounded CLI sweep. See
[`v2.1.2.0_ITEM26_WI179_SANDBOX_HARDENING.md`](history/v2.1.2.0_ITEM26_WI179_SANDBOX_HARDENING.md).

**`/tenant-logs` (WI#287).** The daemon writes a secret-scrubbed copy of its
own log to `<base>/logs/sandbox/daemon.log` (MT: `<tenant root>/state/logs/sandbox/`)
and sandboxed runs see it read-only at `/tenant-logs`. mt-admin renders
`SANDBOX_TENANT_LOGS=info` (INFO and above, whatever the runtime level) and
preserves an operator-set value on re-render; `upgrade-tenant` does not
re-render the env, so existing tenants keep the daemon default, which is also
`info`. `debug` follows the runtime level up to DEBUG (the tool-call lines);
`off` writes no copy and mounts nothing. An invalid value fails every
`lunarwing` command that loads the config. The copy is daemon-owned (`0700`
directory, `0600` file), so it is mounted only where the runtime maps the
sandbox user onto the tenant's uid: docker, podman 5+ (keep-id pinned to
1000), or podman 4.3+ when the tenant uid is 1000. Other podman 4.x tenants
get no mount. The raw `<tenant root>/logs` directory stays protected: it is
neither a valid `workdir` nor visible through a tenant-root mount.

## Worker exposure policy (WI#357)

**LunarWing owns every public exposure path** through `ic/src/tunnel/`
(providers `none` (default) / `netbird` / `custom`; `TUNNEL_URL` or
`TUNNEL_PROVIDER`). **External workers must never self-expose**: no tunnels,
relays, pairing links, or public listeners of their own, and no publishing
session content to third-party share services. A worker that needs a public
path routes it through the daemon's tunnel. The only legitimate instruction
channel into a worker is the daemon's job dispatch → the in-container bridge
→ the worker's agent API on loopback.

This policy originally held the opencode2 lane at **v2.0.24**: v2.0.26
bundles OpenTunnel remote access (`packages/cli/src/services/remote-tunnel.ts`,
`@opentunnel/client` → hosted relay `opentunnel.xyz`, no self-host option,
anonymous device identity in the agent-writable `$XDG_DATA_HOME/opentunnel`),
plus `opencode service set remote true` and `opencode pair` surfaces. !1490
(v2.0.26 bump) was closed. **WI#427 (2026-10-17) records the maintainer's
completed analysis and approval to cross that boundary at v2.0.32**, through
the existing compile-out, wrapper, config and image assertions. See the
v2.0.32 bump entry below for the source review and verification status; the
lane has since advanced to **v2.1.2** (2026-10-21, goals item 83) with every
gate re-verified against a fresh clone — see the v2.1.2 bump entry.

**Gate record for !1490 (run-79 EI-2):** the in-image layers below (ruleset,
wrapper, config pins, compile-out) are **naive-use guards, not a boundary**
— all of them miss whitespace/quote-obfuscated command text, the real
binary stays directly executable by the agent uid, and with egress open a
compromised agent can fetch an upstream opencode (tunnel included) or run
`@opentunnel/client` under bun against the loopback server, whatever the
pin. **Only control 1 (the egress allowlist) holds against a compromised
agent**. The maintainer-approved WI#427 bump deliberately crosses on
compile-out plus config alone; it does not turn the in-image guards into
an egress boundary. Control 1 remains open as **WI#424 for 2.1.7.0**.

Enforced controls (opencode2 lane unless noted):

1. **Egress allowlist (DEFERRED to WI#424, 2.1.7.0; maintainer's call):** mt-admin
   restricting worker egress to the configured LLM endpoint + git hosts.
   Not yet implemented; it is the only control that also covers future
   phone-home features and `webfetch`/shell exfiltration, and the only one
   that holds against a compromised agent (see the gate record above).
2. **No self-exposure commands (naive-use guard).** The bridge session
   ruleset (`opencode2.4lunarwing/scripts/opencode_task_executor.ts`,
   `sessionPermissions()`) denies `shell` on `*opencode service *`,
   `*opencode pair *`, `*opencode serve *` — after the allow-all rule,
   because upstream evaluates with findLast. The lane config
   (`config/opencode2.json` `permissions`) carries the same denies for
   cli/acp sessions the bridge did not create. And
   `scripts/opencode-exposure-guard.sh` is installed as
   `/usr/local/bin/opencode` (real binary at `/usr/local/lib/opencode2/opencode`),
   scanning **every argv token**: `service`/`pair` subcommands, the
   `--service` flag, and every `--hostname` occurrence (loopback only).
   Server-backed commands such as `run` and `session list` can spawn the real
   binary directly via `Service.ensure`, bypassing the wrapper. At each boot
   the entrypoint removes whatever sits at
   `${OPENCODE_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/opencode}/service.json`
   (upstream's resolution order, run-90 FB-6/FB-7) and writes
   `{"disabled": true}` there, so those commands default to private,
   command-scoped servers instead of a shared service. The removal also
   defeats an agent-planted FIFO/directory/symlink at the reset path, which
   would otherwise hang the boot, crash it, or redirect the write. This reset
   discards prior service settings; the bridge's explicit foreground
   `serve` still uses its loopback hostname and per-boot password. These layers
   are naive-use guards: the agent can rewrite its live service config or
   invoke the real binary directly.
3. **Session sharing pinned off; auto-update silenced.** `"share": "disabled"` in both
   `opencode2.4lunarwing/config/opencode2.json` and
   `opencode4lunarwing/config/opencode.json` (v1 schema supports it at
   v1.18.25), plus `"update": "disable"` in the v2 lane config so an upstream
   default flip cannot re-enable publication. **The `update` pin does not
   reach the updater** (run-158 KG-3 / !1699 JE-3): the updater's
   `readPolicy()` reads only the global config dir, never `OPENCODE_CONFIG`.
   `OPENCODE_DISABLE_AUTOUPDATE=1` in the image env is the switch the updater
   actually honours — it silences the background check — and the wrapper
   refuses the manual `upgrade`/`update` command (KG-1/JE-2), so neither the
   background nor the naive-use manual self-update path replaces the pinned
   binary with an upstream build. The config tree and boot files
   (`entrypoint.sh`, `health_server.py`, `agent_comm_protocol.json`) are
   root-owned and read-only in the image, including deployments that restart
   the same container filesystem. The entrypoint re-derives the default
   runtime config from the baked source at every boot. An override copy gets
   owner-write permission before the config writers run; the baked source
   stays read-only. Live runtime/project configs and operator-selected
   `OPENCODE_CONFIG` remain untrusted and mutable.
4. **CI guard.** `.gitlab/scripts/check-worker-exposure-policy.sh` (job
   `worker-exposure-policy-guard`, security stage) asserts the pins, the
   ruleset ordering, the wrapper (every-token scan), the Dockerfile wiring
   including the root-owned read-only boot files + `scripts/`+`config/`, and runs
   git-free (the ci-config image ships no git). The Dockerfile checks are
   anchored over comment-stripped, continuation-joined lines. The exact
   compile-out source COPY and RUN must occur in `AS builder`, in that order,
   before the native build RUN. The wrapper-install/binary-scan RUN and the
   read-only chmod RUN (with optional exec-bit prefix) must each match a whole
   instruction, so a suffix such as `|| true` cannot hide a failed scan.
   After the final `--image-filesystem` RUN, only `EXPOSE`, `HEALTHCHECK`,
   `USER`, `ENTRYPOINT`, `CMD`, `LABEL` and `STOPSIGNAL` are allowed
   (case-insensitively). Later COPY/ADD/RUN steps or new stages are refused
   regardless of command spelling; earlier filesystem changes are checked
   by the in-image assertion, subject to the trust limitation in control 5
   (run-96 FP-1, closing the run-90 FC-4 gaps).
   Run-97 FQ-1: the wrapper-install/binary-scan RUN must sit in the *same
   stage* as the final `--image-filesystem` assertion — build engines skip
   unused stages, so the exact line in a dead or builder stage satisfies
   presence checks while the shipped image has neither wrapper nor scan.
   Run-97 FQ-2/FQ-3: the last `USER` instruction must be exactly
   `USER 10003:10003`, and Dockerfile parser directives
   (`# escape=`, `# syntax=`, `# check=`) are refused outright, since a
   custom escape character would invalidate the guard's continuation
   joining. Run-103 GD-1: the directive refusal also covers the grammar's
   whitespace-around-`=` spellings (`# escape = \``) and a UTF-8 BOM prefix
   (Docker strips a leading BOM before parsing) — either hid a directive
   the old regex never matched. Run-106 GJ-1/GJ-2 close two additional
   parser-divergence spellings: any non-empty line whose first byte is not
   printable ASCII is refused (BuildKit trims Unicode whitespace such as
   U+00A0/U+0085/U+202F before directive detection; the guard's
   `[[:space:]]` trim is ASCII-only), and any line whose right-trimmed form
   ends in two backslashes is refused (an escaped escape token is not a
   continuation for BuildKit, but the guard — like buildah/podman — used to
   join it, hiding the next instruction). Run-109 GW-2 also refuses trailing
   whitespace (space/tab/CR) after a single continuation backslash and
   heredocs (`<<`, including a token split across continued lines). Before
   the assertion, those spellings can hide its RUN inside a previous
   instruction or heredoc body; the earlier "safe over-refusal" claim was
   incorrect. Run-110 GX-1: the BOM is stripped from the first line only, as
   BuildKit does, so a BOM on a later line reaches the printable-ASCII
   refusal, and NUL bytes are refused outright (bash `read` drops them,
   BuildKit keeps them). Either one used to make the guard join the next
   instruction into a `LABEL` while BuildKit ran it on its own. Run-111 GZ-1:
   a continuation backslash must be preceded by a space or tab — a backslash
   that is the line's whole content is refused too — because BuildKit keeps
   the character before the backslash and appends the next line with no
   separator, so `SHEL\` + `L [...]` used to
   reach the guard as the word `SHEL` (never matching the SHELL refusal) and
   `ARG LUNARWING_WRAPPER_\` + `SHA256=…` hid a protected name from the
   literal checks, while BuildKit parsed each pair as one instruction.
   Run-112 HD-1 narrows the accepted class to BuildKit's own (space/tab
   only, via literal bracket expressions that no locale widens) and refuses
   any line carrying a VT, FF or CR byte outright — bash's `[[:space:]]` is
   wider than BuildKit's even in the C locale and locale-dependent under
   C.UTF-8 (U+3000 et al. were accepted as glue there).
   GW-3 refuses `SHELL`, any instruction naming
   `LUNARWING_WRAPPER_SHA256`, `LUNARWING_REAL_OPENCODE` or `OPENCODE_BIN`
   except the exact `ARG LUNARWING_WRAPPER_SHA256=`, and ENV keys that are
   expanded or use legacy space-separated assignment syntax. Literal
   `key=value` assignments may still have quoted or expanded values. The
   anchor ARG must be in the final assertion's stage. These are drift
   checks: an adversarial Dockerfile can still rewrite the in-image checker.
   `.gitlab/tests/test_worker_exposure_policy.py` regression-covers it,
   including no-git lanes. The guard also runs the exposure/wrapper,
   compile-out, and non-root boot configuration suites on merge requests.
5. **Build-time assertion.** The opencode2 Dockerfile runs
   `scripts/exposure_policy_assert.py --binary …`: the image fails to build
   if the config lacks the pins, the entrypoint touches service mode or
   `--service`, `OPENCODE_BIN` bypasses the wrapper, the wrapper regressed
   to a first-token dispatch, the binary contains OpenTunnel client
   markers. A final `--image-filesystem` assertion checks actual ownership
   and modes of the boot files and `scripts/`+`config/` after all image steps;
   the image fails if they are not root-owned read-only or `/app` permits
   non-root replacement of its children. Run-103 GD-2: the v2
   `--image-filesystem` path additionally verifies the *installed wrapper*:
   `/usr/local/bin/opencode` must be a regular non-symlink, root-owned file
   with no group/other write bit and the executable bit set, byte-identical
   to `scripts/opencode-exposure-guard.sh`, and `shutil.which("opencode")`
   must resolve to it (no earlier `PATH` dir shadows it); the real binary
   at `/usr/local/lib/opencode2/opencode` is re-scanned for tunnel markers.
   These checks detect accidental drift after the wrapper-install scan,
   provided the checker and its environment remain intact — pinned by unit
   tests with injected paths, since the image build itself is a
   protected-pipeline gate. Run-106 GJ-3 hardens the
   same check: `LUNARWING_REAL_OPENCODE` may not retarget the wrapper at
   build time (only the default binary path passes, and any other value is
   also marker-scanned), a non-executable wrapper is refused with its own
   reason instead of the misleading "shadows" text, and the installed
   digest must match the `LUNARWING_WRAPPER_SHA256` build-arg anchor — the
   byte-identity source ships inside the image, so a root `RUN` could
   rewrite both files identically; the anchor is computed from the checkout
   by `build-worker-image.sh` and mt-admin's `build_opencode2_worker`, and
   a missing or mismatched anchor fails the build fail-closed. The anchor is
   **not unforgeable**: an adversarial Dockerfile edit can still defeat the
   check by replacing its environment or the checker itself (run-109 GW-3).
   The CI guard's ENV/SHELL refusals are an additional drift check.
   **WI#357 follow-up:** verify the built image out of band with the
   checkout's checker and wrapper bytes, including image-config `Env` and
   `Shell`: export the BuildKit filesystem via a local/tar output in CI,
   and use `podman create`/`podman cp` (or the Docker equivalents) plus
   checkout-side comparison in mt-admin. This durable check is deferred;
   it would also avoid relying on the partial Dockerfile parser for GW-2
   and the remaining binary/boot-path checks in GW-4. Note the
   `PATH` resolution is a *build-time* property: at runtime the agent-owned
   `~/.cargo/bin` and `~/go/bin` (five PATH entries — the four toolchain
   dirs plus an empty `/usr/local/sbin` — precede `/usr/local/bin`), so a
   runtime `PATH` `opencode` need not hit the wrapper; the entrypoint is
   unaffected (absolute `OPENCODE_BIN`) and the wrapper remains a
   naive-use guard.
6. **Runtime smoke (automated suite DEFERRED to WI#424, 2.1.7.0):** live
   `service.json`/`opentunnel` dir/listener assertions against a running
   container are required manually for WI#427 until that suite exists.
   Control 9 covers the listener half on live tenants.
7. **Bump checklist (REQUIRED for every worker version bump).** The bump
   entry in this file must record the upstream features that add network
   exposure or outbound publication — tunnels, pairing, sharing, remote MCP,
   auto-update — with the decision for each, not only the bridge call
   surface. For opencode2 bumps past v2.0.24 the compile-out guard (8) plus
   these guards cover the *shipped binary*; against a compromised agent
   only control 1 makes the crossing safe (see the gate record above).
8. **Compile the tunnel out.** `scripts/compile_out_remote_tunnel.py` runs
   in the builder stage: the historical v2.0.24 path verifies there is no
   tunnel surface; at the current v2.1.2 pin it replaces `remote-tunnel.ts`
   with a fail-closed stub (re-verified against the real v2.1.2 tree:
   importers still use exactly
   `run`/`ensure`/`hostname`). Drift detection scans
   `packages/{cli,server,schema,core}/src` (`*.ts` and `*.tsx`), matches
   any quoted `@opentunnel/*` specifier (static, dynamic `import()`,
   `require`), and collects `@opentunnel` deps from every
   `packages/*/package.json`; if upstream moves/renames the module or
   extends its surface, the build fails instead of silently shipping the
   tunnel. The runtime-stage binary marker scan is the backstop beyond
   even that.
9. **Live listener audit.** `mt-admin worker-listeners <tenant>` lists
   listening sockets inside each running worker container, read **host-side
   from `/proc/<container pid>/net/{tcp,tcp6,udp,udp6}`** (running `cat`
   inside the untrusted container would let a root-in-container agent forge
   the audit), TCP LISTEN (0A) and UDP UNCONN (07) alike. It fails on any
   non-loopback bind beyond bridge-WS + health — and the expected-port
   exemption is TCP LISTEN only: those are the ports mt-admin publishes, so a
   UDP bind on 8443 or the WS port is unpublished and still a finding, with
   the protocol labelled on every reported line (run-90 FC-2). It fails on
   any published mapping
   off host loopback — an empty/missing `HostIp` counts as all-interfaces
   (Docker's shape; podman reports `0.0.0.0`) — and an inspect failure is a
   finding, never a silent skip. Absence requires a failed inspect with a
   Docker/Podman missing-object/container diagnostic naming that exact
   container; account/runtime-directory errors are findings. Warning lines
   before a valid state line are tolerated, and a failed PortBindings inspect
   is also a finding. Re-run against the `scootaloo` tenant on
   2026-10-13 (rootless podman, after the run-79 EI-6 hardening):
   **clean** — all four lanes showed exactly the expected bridge-WS + 8443
   binds, all mappings host-loopback only.

**!1570 deployment note:** rebuild the worker image and recreate existing
opencode2 containers through mt-admin to receive the protected boot files.
Restarting a container on an older image does not apply the new ownership or
startup logic. !1490 was closed; WI#427 now authorizes the v2.0.32 crossing.
The service-config reset is not an egress boundary.

Control 11 audit of the other lanes (2026-10-12, against pinned sources;
re-checked 2026-10-13):

- **opencode v1 (v1.18.25):** no service mode, no pairing, no tunnel (all
  v2-era surfaces). Session sharing existed and is now pinned
  `"share": "disabled"`. v1.18.25's `serve` has **no auth support at all**
  (no password — the containment is the loopback bind plus host-loopback
  publishing, verified live under control 9).
- **kimi (2.1.1):** no server mode, pairing, sharing, or tunnel in the lane;
  `kimi -p` is spawned per task. `telemetry = false` is pinned in
  `config/config.toml`, and so is `max_output_size = 131072` on the baked
  model table — since 2.2.0 the CLI sends no `max_tokens` for an
  `openai`-type model without it (WI#412); the WI#206 fail-closed command
  guard is the only in-CLI enforcement point and stays mandatory.
- **pebble:** no listeners, pairing, or sharing in upstream
  (nanogpt-community/pebble) — the only `TcpListener` is test code.
  **Finding (widened per run-79 EI-10; resolved by WI#400/WI#422):** the
  image used to have no `USER` directive, so the agent ran as **root in
  the container** — beyond `pebble self-update` (fetches GitHub releases,
  replaces the binary), a root-in-container agent could replace the bridge
  binary itself and forge audit/bridge output (why control 9 reads
  `/proc` host-side). That shape is gone: the image now pins `USER 0:0`
  and the WI#400 split (see "Task-uid split (WI#400)" above) drops every
  spawned pebble process to the task uid via setpriv, so the bridge
  binary and its root-owned state are out of the agent's reach on the
  rootless lane; the rootful launch runs the whole container as the
  tenant uid ("Launch identity (WI#422)") — there the agent shares the
  bridge's uid, so it can still signal or trace the bridge and read its
  environment (no split, by design per WI#422). Residual: `self-update` still
  fetches over the network from the (now unprivileged) task uid, so the
  real fix for that half remains control 1 (egress allowlist). Keep the
  pinned `PEBBLE_LOCK_SHA256` build contract intact.

## LunarWing Worker

The main sandbox worker. Its runtime image includes Git, build-essential, Node.js/npm, Python 3, Rust, and GitHub CLI; it does not install a Claude Code executable. The orchestrator (`ic/src/orchestrator/`) manages container lifecycle, LLM proxying, and credential injection.

**Dockerfile:** `ic/Dockerfile.worker`

```bash
cd ic
docker build -f Dockerfile.worker -t lunarwing-worker:latest .
```

Runs as non-root user `sandbox` (UID 1000) in `/workspace`. Entrypoint is the `lunarwing` binary — the orchestrator passes the full command via Docker cmd.

### Generic sandbox egress default (WI#154)

The generic podman sandbox (`shell` tool, `ic/src/sandbox/`) defaults to
**`SANDBOX_NETWORK=none`** on multi-tenant hosts: mt-admin renders
`SANDBOX_NETWORK=none` into every tenant's `lunarwing.env` (an operator-set
`proxy` value is preserved on re-render). The advisory `proxy` mode lets a
sandboxed process that ignores proxy env vars reach host-loopback services —
worker WSS/health ports, tenant PostgreSQL, and cross-tenant loopback — so it
is an explicit opt-in (`SANDBOX_NETWORK=proxy`) for tenants that genuinely
need allowlisted web access. The enforced-proxy middle ground is tracked in
WI#155. See `ic/src/NETWORK_SECURITY.md` §5 for the tier table.

### Generic sandbox image + tenant defaults (WI#223)

For new multi-tenant tenants, the generic sandbox's image is the lean
**`lunarwing-sandbox:latest`**, built from `ic/docker/sandbox.Dockerfile`
(common build tooling only — no lunarwing binary, much smaller than
`lunarwing-worker:latest`). mt-admin renders these defaults into every
tenant's `lunarwing.env` during `add-tenant` (existing values are preserved
if that command is explicitly re-run):

```bash
SANDBOX_RUNTIME=podman
SANDBOX_IMAGE=lunarwing-sandbox:latest
SANDBOX_AUTO_PULL=false
```

`add-tenant` builds the image into the selected admin store when absent. For
rootless Podman it distributes the image into the tenant's own store
(`_ensure_tenant_image` semantics — a rebuilt admin image is re-distributed);
for Docker it checks the shared store. `start-tenant` also reconciles the
image before daemon startup, so a failed add-tenant build or distribution can
be retried. On hosts with both CLIs, the generic shell sandbox prefers Podman
even when mt-admin runs its worker containers with Docker; an explicit
`SANDBOX_RUNTIME=docker` still chooses Docker. Provisioning uses the tenant's
selected runtime and prepares its rootless Podman store when needed.
Only tenants configured with this managed image are reconciled;
an operator-selected custom image is left alone. `SANDBOX_AUTO_PULL=false` is
deliberate: the tag exists only in local stores, so an auto-pull would fail
against a registry that has never hosted it. Single-node hosts keep the
in-code defaults (`auto` runtime preference, `lunarwing-worker:latest`,
auto-pull on).

Manual build/recovery:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh build-sandbox-image <name>
sudo ic/scripts/lunarwing-mt-admin.sh start-tenant <name>
```

`build-sandbox-image <name>` builds into the admin store of that tenant's
selected sandbox runtime (the argument may be omitted for the new-tenant
Podman-first default); `start-tenant` makes it available in the daemon's
runtime store. If either step fails, mt-admin warns
and the generic shell sandbox fails closed with `ImageUnavailable`; base tenant
provisioning remains available. `upgrade-tenant` **does not** add these keys to
existing tenants or switch their existing sandbox image. To opt in an older
tenant, set the three variables in its `lunarwing.env`, build the image, then
`restart-tenant <name>` so both its daemon configuration and image store update.

The build context is `ic/docker/`, not `ic/`: the Dockerfile has no
COPY/ADD, and `ic/` carries the vendored crate tree that must stay
in-context for `Dockerfile.worker`'s offline build.

## Pebble Worker

Rust-based Pebble agentic coding harness. Bridges the NanoGPT community `pebble` binary to the WebSocket protocol. Health on 8443, WebSocket on 9090.

**Dockerfile:** `pebble4lunarwing/Dockerfile`

```bash
# Run from the repository root; the Dockerfile copies the local bridge crate.
docker build -f pebble4lunarwing/Dockerfile -t lunarwing-worker-pebble:latest .

# Or select the worker per tenant and build the shared image via mt-admin
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant <name> --with-pebble
sudo ic/scripts/lunarwing-mt-admin.sh build-pebble-worker
```

Persistent worker mode (`PEBBLE_MODE=websocket`). Requires `AGENT_AUTH_TOKEN` for WebSocket auth and optionally `NANOGPT_API_KEY` for the underlying NanoGPT backend (configure per-tenant via `configure-pebble <name> --nanogpt-api-key <key>`). Upstream Pebble v0.5.0 is fetched by its reviewed full commit, both Cargo builds use committed lockfiles, and base images/package indexes/toolchain downloads are pinned. See `pebble4lunarwing/` for details.

Launch identity (WI#422): the image sets `USER 0:0` (WI#400 LD-8, so without an override the container is root), and the runtime re-resolves the `/workspace` bind source at every container start, so whoever the container runs as owns the escalation surface of a tenant-swapped workspace symlink. On rootless podman the bridge runs as in-container root, which maps to the invoking tenant uid — a redirected bind grants nothing the tenant could not already reach. On rootful hosts (docker, or `LUNARWING_MT_ROOTLESS=false`) `start-tenant pebble` therefore launches with `--user <tenant uid>:<gid>` plus `--security-opt no-new-privileges` (the image ships Debian's standard setuid set; with the container non-root the option closes the setuid/file-capability route back to container root, making the uid drop one-way), with `HOME`, `GOPATH`, `GOCACHE`, and `CARGO_HOME` anchored inside the tenant-owned `/workspace` mount (the numeric uid has no in-image passwd entry, and the fat image's `/root/go` and read-only `/usr/local/cargo` are unusable for caches); the distinct `PEBBLE_ROOTFUL_LAUNCH_CONTRACT` (now `tenant-uid-user;workspace-toolchain-home;no-new-privileges;task-uid-inherit`) force-recreates containers created under an older rootful shape once on their next start-tenant. The OpenRC worker unit only `podman start`s the container mt-admin created, so the create-time `--user` is what it runs. One runtime difference on the rootful path: podman synthesizes a passwd entry for `--user` (so `git` can auto-detect an identity), docker does not — `git commit` inside a docker-rootful task therefore fails with "unable to auto-detect email address" until the workspace carries a git identity (a `/workspace/.gitconfig` with `user.name`/`user.email` is enough); `HOME` is set explicitly, so nothing else depends on the missing entry (the image has `git` but no `ssh`).

This is the rootful pebble identity model, and WI#400's bridge/task uid split applies to the rootless lane only. The two are reconciled (run-167 LH-1, run-170 LP-2): on rootful hosts mt-admin passes `-e PEBBLE_TASK_UID=inherit -e PEBBLE_TASK_GID=inherit`, which tells the bridge to run tasks as its own (tenant) identity — no `setpriv`, no root-only credential file (the credential stays in the container environment, the pre-WI#400 shape on this lane), no socket handoff — while the workspace stays tenant-owned (no task-uid `chown`). The pair is distinct from the image's numeric pins (the rootless keep-id split) and from the empty value (fail-closed), so the bridge still refuses a rootless keep-id launch that merely started as the task uid (LD-8). On rootless podman the image runs as container root under `--userns keep-id:uid=10004,gid=10004` and every task drops to 10004 through `setpriv`; both shapes are pinned in `test-mt-admin-worker-image-contract.sh`.

In the fat (`--with-toolchains`) image the Rust/Go toolchains are anchored against the `/etc/profile` PATH reset in upstream pebble's `/bin/sh -lc` task shell (WI#334): an `/etc/profile.d` snippet plus real-binary `/usr/local/bin` symlinks and a `rustup` wrapper keep `cargo`/`go` on PATH after the reset, proven at build time in both the real spawn mode and a fully sanitised shell, and re-probed by the smoke script at the end of every fat mt-admin build (which targets a candidate tag and re-tags `:latest` only after the probe passes, using the build's own container runtime and `--pull=never`). Re-check any built image or running worker with `pebble4lunarwing/scripts/smoke-task-shell-toolchain.sh image [ref]` / `container <name>`; see `docs/ops/PEBBLE-WORKER.md` "Task-Shell Toolchain PATH".

Upstream's pinned lockfile ships rustls 0.23.41. The reviewed override now pins **0.23.46** (WI#409), fixing RUSTSEC-2026-0285 and RUSTSEC-2026-0346/0347/0348/0349; `pebble4lunarwing/upstream/Cargo.lock` is copied over the checkout during the image build. It also retires upstream's yanked `chacha20 0.10.1` pin (0.10.2) and, for CVE-2026-111943 (2026-10-20), raises rustls-webpki to **0.103.16** (RUSTSEC-2026-0382) and webpki-roots to **1.0.10** (RUSTSEC-2026-0381) — the distrusted emSign intermediates pair. The Dockerfile supplies the canonical reviewed `PEBBLE_LOCK_SHA256` default for CI, mt-admin, Compose, and plain Docker builds. `scripts/verify_upstream_lock.py` verifies that digest and compares complete parsed package records (including additions/removals, source, checksum, and dependencies; records are keyed by `(name, version, source)` so multi-version crates such as `cpufeatures`/`getrandom` are fully covered), allowing changes only to rustls/rustls-webpki/webpki-roots/chacha20 and rejecting a no-op override so the input is retired once upstream ships its own fix. `.gitlab/deliverables.json` explicitly classifies this lock-only override under `build_inputs`, with ci-config regression checks and worker-image candidate coverage; it is not a standalone Cargo root. Rebuild the Pebble image and verify the protected candidate/scan before promotion; changing the lock alone does not patch deployed images.

## Opencode Worker

[opencode](https://opencode.ai) (anomalyco/opencode, formerly sst/opencode) integrated as a persistent LunarWing external worker. A Bun/TypeScript bridge wraps the `@opencode-ai/sdk` and translates between the opencode session/event model and the `lunarwing-agent-v1` WebSocket protocol. The opencode headless server runs internally on `127.0.0.1:4096` (not exposed). Health on 8443, WebSocket on 9090.

**Dockerfile:** `opencode4lunarwing/Dockerfile`

```bash
cd opencode4lunarwing
docker build -t lunarwing-worker-opencode:latest .

# Select the worker per tenant, then build the shared image via mt-admin
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant <name> --with-opencode
sudo ic/scripts/lunarwing-mt-admin.sh build-opencode-worker

# Or build the shared image as part of a per-tenant build
sudo ic/scripts/lunarwing-mt-admin.sh build-tenant <name> --with-opencode

# Include Rust, Go, and C/C++ compilers for build/test jobs
sudo ic/scripts/lunarwing-mt-admin.sh build-opencode-worker --with-toolchains
```

The default image is slim and does not contain Rust, Go, or C/C++ toolchains. Build with `--with-toolchains` when worker jobs must compile or test code; the toolchain image uses Rust 1.96.1. Modes: `--mode websocket` (default, persistent) or `--mode cli` (one-shot; prompt via `TASK_PROMPT`). Requires `AGENT_AUTH_TOKEN` for WebSocket auth. Supports per-tenant model/base-URL overrides via `configure-opencode <name> [--model <m>] [--base-url <url>]`, optional Paseo MCP integration (`PASEO_URL`/`PASEO_TOKEN`), and SSH agent socket bind-mounting. `configure-study-mcp <name> [--home <absolute-checkout>]` gives core and OpenCode the same lw2studymcp data. Without `--home` it uses the shared default checkout (`/var/cache/lunarwing/lw2studymcp`, override with `LUNARWING_MT_STUDY_NOTES_HOME`) and always syncs it to the latest upstream (`LUNARWING_MT_STUDY_NOTES_REPO`, default `https://git.lunarwing.org/cmc/lw2studymcp.git`) first — cloning when missing, fast-forwarding a clean checkout, and refusing to touch a dirty or non-git directory. `--sync` applies the same update to an explicit `--home`. `add-tenant <name> --with-study-mcp [--study-mcp-home <path>]` runs this auto-configuration (read-only) during provisioning so a new tenant starts with current knowledge of the LunarWing codebase. `--allow-write` enables approved core-agent mutations in separate state; OpenCode always receives code and state read-only at `/opt/lw2studymcp` and `/var/lib/lw2studymcp`. OpenCode is pinned to **v1.18.25**, fetched by reviewed full commit `cb7d8b2f5e44876ef98b661dc10590c915af3a9f` (reverted from v1.18.29 on 2026-09-07: that release crashes on model entries lacking full metadata shape and dies silently in its ai-sdk stream path — see the Dockerfile pin note; re-bump only after both regressions are fixed upstream); `OPENCODE_REF` accepts only a full SHA, the checkout is verified, and a frozen Bun lock is mandatory. The release and SHA are baked into `OPENCODE_IMAGE_REF` as `v1.18.25@<full-sha>` and reported together as `opencode_ref`. Base images, Debian indexes, and optional toolchain downloads are also pinned. See `opencode4lunarwing/README.md` for the full environment reference.

Agent-facing dispatch uses `create_job(title: "...", description: "...", mode: "opencode")` in both legacy and Engine V2 chat paths. `opencode` is a named job execution backend, not an extension, MCP server, skill, routine, or SSH host; no registry search or installation is involved.

**Known issue (WI#320 F4, tracked as WI#333; WI#374 reported the same crash on a 2.1.6.0 test tenant):** at the pinned v1.18.25 the `mode: "opencode"` lane is broken in the LunarWing worker shape — every task fails with `external worker 'opencode' failed: Unexpected server error` because `opencode serve` crashes in `SystemPrompt.environment` (`undefined is not an object (evaluating 'a.name')`) while assembling the prompt, before any model call, and answers the SDK with that bare HTTP 500 wrapper. No config remediation (base-URL override, MCP/plugin strip, git-init workspace) avoids it, and v1.18.29 is worse (see the pin note above), so the lane cannot be fixed from this repo until upstream ships a fix; the opencode2 lane below is the working one. To surface the root cause, the worker attaches a bounded, ref-correlated, error-level tail of serve's own structured log (`$XDG_DATA_HOME|~/.local/share/opencode/log/opencode.log` — verified at the pinned ref: serve logs 500 causes there as `level=ERROR` lines with a unique `ref=` id, and the 500 body carries the same ref) to these job errors — only the failing request's line attaches, and a 500 without a ref attaches nothing — so the crash's real stack line reaches `list_jobs`/`job_status`. The v1 lane is expected to be retired in v2.1.7.0 (goals item 34: "prob rm opencode v1 in .1.7.0").

## OpenCode 2 Worker (opencode2)

The OpenCode **2** lane, run as a separate external worker per
`docs/plans/OPENCODE2_EXTERNAL_WORKER_PLAN.md`. v1 stays the default and
keeps its v1.18.25 pin (last dispatch-verified release; see the known
issue above); v2 is opt-in per tenant until upstream's
2.0.x patch stack settles. Do not configure both lanes on the same
tenant during the transition (agent routing confusion — plan §5).

**Dockerfile:** `opencode2.4lunarwing/Dockerfile` — pinned to
**v2.1.2** at reviewed full commit
`a517687fde98a7329ada3ae644a01a9fc6c4e99e` (WI#427 boundary crossing first
crossed at v2.0.32/`843a6c514e606b436a7fe713422ace0a93ad3d61`); the v2 bridge uses
`@opencode/client` (promise API) instead of `@opencode-ai/sdk`, and the
v2 config file is `config/opencode2.json` (`providers` map,
`settings.baseURL`, `package`). Container UID/GID is **10003**
(opencode 10001, kimi 10002) so both opencode lanes keep separate
userns identities on one host.

```bash
cd opencode2.4lunarwing
docker build -t lunarwing-worker-opencode2:latest .

# Select the worker per tenant, then build the shared image via mt-admin
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant <name> --with-opencode2
sudo ic/scripts/lunarwing-mt-admin.sh build-opencode2-worker

# Or build the shared image as part of a per-tenant build
sudo ic/scripts/lunarwing-mt-admin.sh build-tenant <name> --with-opencode2

# Include Rust, Go, and C/C++ compilers for build/test jobs
sudo ic/scripts/lunarwing-mt-admin.sh build-opencode2-worker --with-toolchains
```

CI publication matches the other workers: on protected release tags and
protected default-branch pushes, `worker-opencode2-candidate` builds the
image through `.gitlab/scripts/build-worker-image.sh opencode2` (same
reproducible-candidate + clean-rebuild digest contract), `worker-opencode2-scan`
runs the Trivy CRITICAL gate + SBOM, and `worker-opencode2-promote` publishes
the immutable `$CI_COMMIT_TAG-opencode2` tag. `ci-config` cross-checks the
`OPENCODE_VERSION`/`OPENCODE_REF` ARG defaults against the build script's
opencode2 case block — including its provenance `source_ref` — and the Rust
toolchain pin.

The opencode2 worker applies a checked build-profile patch to the pinned
upstream CLI script: `splitting: false` and `bytecode: false`. This avoids
split-chunk identifiers and ahead-of-time bytecode in the shipped executable;
it keeps upstream version injection, plugins, and artifact verification. Bun
parses the bundled source at startup instead, so startup time may increase.
The patch fails if either expected upstream setting is missing or duplicated,
requiring review when upgrading OpenCode. Executable bytes are not rewritten.

Image self-tests use `/tmp/opencode2-build-tests`, removed by an EXIT trap in
the same Docker `RUN`, and disable Python bytecode writes. The workspace-path
test also removes its own `oc2-ws-*` directory in `finally` for local runs.
These address the two reported sources of opencode2 digest drift: per-build
chunk IDs in the executable and leftover randomly named test workspaces.
The candidate versus uncached-rebuild **whole-image digest comparison remains
mandatory**; a passing binary check alone does not establish image reproducibility.

The build-time self-test `RUN` steps also run under a throwaway
`HOME`/XDG environment rooted inside the trap-cleaned `/tmp/opencode2-build-tests`
tree, with `/root/.bun`, `/root/.local`, `/root/.cache`, and `/root/.config`
leak tripwires that fail the build. This is the work-item-#177 root cause fix:
OpenCode 2 appends a line stamped with the wall-clock time and a random run ID
(`timestamp=... run=...`) to `$HOME/.local/share/opencode/log/opencode.log` on
EVERY start — including `opencode --version` — so with the default root HOME
each build baked a unique log file into the runtime layer and the candidate
versus rebuild digests could never match (every opencode2 candidate job since
the lane landed failed the gate, e.g. pipelines #3862 and #3880; the v1 worker
is unaffected because opencode 1.x does not write that log). The compiled
executable itself was verified byte-identical across two patched-profile
builds, so the log was the only nondeterministic content in the image.

On a candidate/rebuild digest mismatch, `build-worker-image.sh` now also
prints a best-effort registry-side diagnosis before failing: it fetches both
pushed manifests with the job's registry credentials (busybox `wget` against
the GitLab `jwt/auth` token endpoint), reports both config digests and layer
counts, and diffs the layer digests — or, when all layers match, the two image
configs — pinpointing the first differing entry from the job log alone, which
is the layer-level comparison work item #177 called for.

Verification (2026-09-18, v2.0.5): built upstream commit
`79169fe966d58fb0e0a5e41133716184a0ab4ca6` with Bun 1.4.2 extracted from the
Dockerfile's pinned amd64 image, using `OPENCODE_VERSION=2.0.5 bun run build --
--single` in fresh build processes. Original-profile binaries differed in 3,295
bytes, including the reported random-prefix `C00000184`-style chunk keys.
Two patched-profile builds were byte-identical (`cmp`), SHA-256
`9f71bc51e6d778b0cceec8626a98d4815f3620e4f928358b403e4895f66baf69`.
This is host-build evidence, not a portable expected image/binary checksum.
The patched executable passed `--version` (`opencode v2.0.5`) and `serve --help`.

v2.0.8 bump (2026-09-19): upstream `packages/cli/script/build.ts` is
byte-identical between v2.0.5 and v2.0.8, so the checked build-profile patch
above applies unchanged; client/schema/protocol SDK changes in
`7673ed6bd6547ee0dcb81aab55f1392fb751d652` are additive against the bridge's
`@opencode/client/promise` usage. Whole-image reproducibility remains
CI-verified per revision.
(2026-10-11 correction, WI#329: this entry's "additive against the bridge's
`@opencode/client/promise` usage" claim was checked against protocol
identifier strings, not the generated promise client — the bridge then called
`session.message.list` / `session.permission.reply`, which never existed on
the promise client at v2.0.8 (WI#320 F7 observed the runtime TypeError at
v2.0.21). Both call paths were corrected in the v2.0.23 entry below.)
Bridge executor, result, workspace-path, build-profile, and study-notes tests
passed; workspace cleanup was also checked with an injected assertion failure.
Full image digest verification remains with CI: local Podman could not unpack
the base image because this account lacks sufficient subordinate UID/GID mappings.

v2.0.11 bump (2026-09-20): upstream `packages/cli/script/build.ts` is
byte-identical between v2.0.8 and v2.0.11, so the checked build-profile patch
above applies unchanged; client/schema/protocol SDK changes in
`9eb6902aaf3c35ce985b67c605a775992249066b` are additive against the bridge's
`@opencode/client/promise` usage — `OpenCode.make`, `session.interrupt`,
`session.execution.{succeeded,failed,interrupted}`, `session.idle`,
`session.message.list`, and `session.text.delta` all confirmed present.
Whole-image reproducibility remains CI-verified per revision.
(2026-10-10 correction, WI#329: this entry's `session.message.list`
"confirmed present" claim was checked against protocol/OpenAPI identifier
strings, not the generated promise client the bridge imports — `session.message`
exposes only `get`, so the method never existed at v2.0.11 (WI#320 F7 observed
the runtime TypeError at v2.0.21). The call path was corrected in the v2.0.23
entry below.)

v2.0.14 bump (2026-09-22): tag SHA `08462140ec0de1e4b17d4a353d8d5827f53cf7b0`
verified against both anomalyco/opencode and sst/opencode remotes. Upstream
`packages/cli/script/build.ts` is byte-identical between v2.0.11 and v2.0.14,
so the checked reproducible-build patch applies unchanged. The generated
promise client (`packages/client/src/promise/generated/client.ts`) is
byte-identical between the two tags; the only client/schema/protocol diffs are
an additive `method: "key" | "oauth"` field on `Connection.CredentialInfo` and
the version bumps — no removals against the bridge's `@opencode/client/promise`
usage (`OpenCode.make`, `session.create/prompt/interrupt`, `session.message.list`,
`session.permission.reply`, `session.execution.*`, `session.idle`,
`session.text.delta` all present at v2.0.14). The promise exports map is
unchanged. Whole-image reproducibility remains CI-verified per revision.
(2026-10-10 correction, WI#329: this entry's `session.message.list` /
`session.permission.reply` "present at v2.0.14" claim was checked against
protocol identifier strings, not the generated promise client the bridge
imports; neither method existed — `session.message` exposes only `get` and
there is no `session.permission` group at all (WI#320 F7). Both call paths
were corrected in the v2.0.23 entry below.)

v2.0.15 bump (2026-09-23): tag SHA `6f3639d82ed0760091792189b78f8eeb44f699b1`
verified against anomalyco/opencode. The upstream CLI build script and
`@opencode/client/promise` exports map are unchanged from v2.0.14, so the
checked reproducible-build patch still applies. The generated promise client
now preserves base-URL path prefixes and throws declared HTTP errors as
`Error` objects; neither change alters the bridge's localhost root-base-URL
calls. The new session metadata event and update field are additive to the
bridge's session execution, message, and permission calls. Whole-image
reproducibility must still pass the CI candidate-versus-clean-rebuild gate.
(2026-10-11 correction, WI#329: this entry's "additive to the bridge's
session execution, message, and permission calls" claim vouched for the
bridge surface as checked against protocol identifier strings, not the
generated promise client — the bridge then called `session.message.list` /
`session.permission.reply`, which never existed on the promise client at
v2.0.15 (WI#320 F7). Both call paths were corrected in the v2.0.23 entry
below.)

v2.0.17 bump (2026-09-26): tag SHA `04f4b0610c79e3692f3b91bd7e4489c2c7010e4b`
verified against the anomalyco/opencode remote (this release's checklist item #2
moves the lane off the beta-frequent 2.0.15 pin). The Dockerfile patches the
upstream CLI build script at that SHA; its `prepare_reproducible_build`
self-test guards the patch contract against a 2.0.17 fixture during the image
build. Host-side `prepare_reproducible_build_test.py` passed, including the
untouched-failure contract. Whole-image reproducibility must still pass the CI
candidate-versus-clean-rebuild gate before promotion.

v2.0.23 bump (2026-10-06, WI#303, re-attempt of the rejected MR !1438): tag
SHA `0fd7e2829449b052abf0078666669302923d77af` verified live against the
anomalyco/opencode remote (`git ls-remote`, this date). The `oven/bun` base
digest and `DEBIAN_SNAPSHOT` were reconsidered and stay unchanged. The
build-profile patch contract was **executed** host-side against the pinned
`packages/cli/script/build.ts` fetched at that SHA: exactly one
`bytecode: true` / `splitting: true` each, `prepare()` flips both, and a
double-apply is rejected. The `prepare_reproducible_build` self-test fixture
string moved to 2.0.23 — note the fixture is synthetic: the self-test never
reads upstream `build.ts`, so it guards the patch logic, not the pin (the
executed fetch-and-patch check above is what covers the pin).

Bridge call-path correction (WI#320 finding F7, fixed in this bump): the
generated promise client has **no** `session.message.list` and **no**
`session.permission.reply` — verified against the pinned tree
(`packages/client/src/promise/generated/client.ts`): `session.message`
exposes only `get`; session message listing is the top-level
`client.message.list` (`GET /api/session/:sessionID/message`, a paginated
`{ data, cursor }` envelope with an optional server-side `type` filter), and
permission replies are the top-level `client.permission.reply`. Earlier bump
entries (and MR !1438) claimed `session.message.list` was "present/unchanged"
because they checked protocol/OpenAPI identifier strings, not the client the
bridge imports — the method did not exist at v2.0.21 either, where the runtime
logged `sdk.session.message.list is not a function` and the executor silently
fell back to streamed deltas. `opencode_task_executor.ts` now calls
`client.message.list` (unwraps `.data`, `type: "assistant"`, bounded at 5
pages of 200) and `client.permission.reply`. Paging follows the server
contract (`packages/server/src/handlers/message.ts`): `order` is sent on the
first page only (`desc`, so the bound keeps the newest messages), cursor pages
carry no `order` — the server rejects cursor+order with 400
`InvalidCursorError` — and a short page ends the listing because `cursor.next`
is returned for every non-empty page; the pages are reversed into
chronological order and a later-page failure keeps the pages already fetched.
The self-test mock mirrors the real client shape and that server contract, so
the stale paths and a cursor+order request both fail the suite (the first
re-attempt sent `order` on every page and lost the canonical output on every
real session; !1452 review AL-1), and the image build now constructs the
client and asserts `typeof`
for every method path the bridge calls (`session.create/prompt/interrupt`,
`permission.reply`, `event.subscribe`, `message.list`), so this drift class
fails the build instead of the runtime. (WI#329 follow-up, 2026-10-10: the
asserted paths now come from the single committed list
`opencode2.4lunarwing/scripts/sdk_surface.ts`, which the build gate runs as
`scripts/sdk_surface_assert.ts` and which also drives the executor
self-test's mock-parity check, so the two gates cannot diverge; the stale
v2.0.8/v2.0.11/v2.0.14/v2.0.15/v2.0.20 bump entries carry dated WI#329
corrections. 2026-10-11, !1512 review CY-1/CY-2/CY-3/CY-5: the self-test now
also binds the list to the executor itself — it extracts the real
`sdk.*(...)` call sites and `event.type === "..."` literals from
`opencode_task_executor.ts` and asserts set equality with
`BRIDGE_SDK_METHODS`/`BRIDGE_SDK_EVENTS`, closing the F7-class gap where an
unlisted swallowed call passed both gates; `mockMethodPaths` flattens at any
nesting depth; and the build gate additionally asserts every bridge event
name is still a literal in the pinned `@opencode` client/schema/protocol
source shipped in the image. 2026-10-17, WI#369
(DF-1/DF-2/DF-4 + CY-2/CY-3 residues): the event gate now asserts each
bridge event name is a member literal of the pinned generated `V2Event`
union (`EventSubscribeOutput = V2Event`, what `event.subscribe` actually
yields) instead of a corpus-wide substring — a literal surviving only in
the v1 compat schema no longer counts; the source extractors run on
comment/string-blanked text (prose can no longer cause misleading
mismatches), bind optional-call/non-null/cast receivers and the Yoda event
form, and scan every non-test `scripts/*.ts`; the self-test additionally
wraps the mock in a recording Proxy so every client leaf path the suite
actually touches (alias, cast, bracket, destructure, optional-call no-op,
helper parameter of any name) must be declared in the list; the mock now
also emits `session.idle` and `session.execution.interrupted`; committed
negative tests pin the recursive mock flatten and fail if either build-gate
assertion loop is removed; and the builder stage runs `tsgo --noEmit` over
the SDK-facing scripts against the pinned generated types
(`scripts/tsconfig.sdk-surface.json`), covering call argument and event
payload shapes (the old CY-2 residue).)

Verified host-side with bun: the executor self-test passes against the real
client shape and fails with the pre-fix executor; the build-time assertion
snippet ran against the real pinned v2.0.23 promise source (all six paths
present, both stale paths absent) and fails closed against a drifted stub.
Paging fix verified 2026-10-06 on a local `podman build` of this lane at
v2.0.23 (`WITH_TOOLCHAINS=false`; every build-time self-test green) by a smoke
inside that image against a live `opencode serve` and a loopback fake
OpenAI-compatible model: the executor returns the canonical text from one
`message.list` call, cursor pages without `order` are accepted, newest-first
paging matches the server's chronological order, and the pre-fix executor's
cursor+order page is rejected with `InvalidCursorError` on the real server.
**Not yet executed:** no pipeline can build the v2.0.23 image before merge
(candidate jobs exist only on protected refs), so before promotion the
`worker-opencode2-candidate` job on the first protected pipeline containing
this pin must run green and be cited on WI#303, and a WI#320-style runtime
smoke against the candidate image should re-confirm the corrected executor
paths end-to-end.

### v2.0.32 bump — WI#427 (2026-10-17; verified and tenant-tested 2026-10-18, item 71)

**Authorization:** the maintainer's completed upstream analysis and explicit
approval are recorded in [WI#427](https://git.lunarwing.org/lunarwing/lunarwing2/-/work_items/427).
The tag was independently resolved with `git ls-remote` (and cross-checked via
the GitHub API) to `843a6c514e606b436a7fe713422ace0a93ad3d61` — a lightweight
tag resolving directly to the commit, same as prior pins. Both Dockerfile
defaults and the CI case block's version/ref pins move together. The pinned
Bun base image digest and Debian snapshot stay unchanged.

**Exposure inventory (v2.0.25–v2.0.32):** the following implementation
cross-check supplements that decision, using the pinned source diff from
`e7a34f09bfd9134dfade5a8ddb843f7030bc9a69` (re-verified against a fresh
v2.0.32 clone on this branch):

| Upstream surface | Worker decision / enforcement |
|---|---|
| OpenTunnel remote access (`0d07f915`, `53533fd0`, `19fb8918`): shared device identity, relay routes on random subdomains, reconnect/retry | Compile out `packages/cli/src/services/remote-tunnel.ts`. The three consumers remain `service-config.ts` → `ensure`, `server-process.ts` → `run`, and `commands/handlers/pair.ts` → `hostname`; the existing refusing stub applies unchanged. Both dynamic and type-only `@opentunnel/client/effect` imports disappear. The declared package stays in the upstream frozen lock; the built-binary marker scan remains mandatory. |
| `pair --remote`, `--yes` / `-y` consent (`38454c6e`), `service set remote true`; saved `remote: { route }` and `remoteAccepted` | Refused by the existing every-token `pair`/`service` wrapper and session/config denies. Tests explicitly cover remote pairing with non-interactive consent and the boot reset discarding both saved fields. No new listener/pairing CLI command outside the guarded surface was found. |
| Foreground serve, pairing addresses, remote URL advertisement; web-UI archive/compression changes | Keep explicit loopback serve and per-boot Basic auth. `serve --service` and non-loopback `--hostname` stay refused. Server-backed CLI commands still default to private servers via the boot-time `disabled: true` reset. |
| Session sharing / publication | `share: "disabled"` remains valid in `packages/schema/src/config.ts`; no additional publication switch was introduced. Keep the baked config and startup protections. |
| Remote MCP: capability negotiation, query-parameter compatibility and numeric timeouts (`fbf5f164`, `7b5f1bd9`, `a448d5b9`) | Existing explicitly configured outbound MCP clients remain supported. No new worker listener or automatic sharing default; these are not an egress boundary. |
| Auto-update: WinGet support and install-method detection changes (`d46588fe`, `e5a557ef`, `707de583`, `47e5e715`), embedded release highlights | `update: "disable"` remains a valid config literal, but it does not reach the updater (`readPolicy()` reads only the global config dir, never `OPENCODE_CONFIG` — run-158 KG-3). `OPENCODE_DISABLE_AUTOUPDATE=1` in the image env silences the background check (the switch the updater actually honours), and the wrapper refuses the manual `upgrade`/`update` command (KG-1) — including `upgrade --method curl`, which would install an unpinned tunnel-capable upstream build at `$HOME/.opencode/bin`. Keep the pin and root-owned binary. WinGet is outside this Linux image. |
| Experimental voice API (`packages/core/src/voice.ts`, `protocol/src/groups/voice.ts`) | Adds authenticated outbound voice-session creation using caller-supplied input and provider credentials, not another listener. The bridge does not call it or provision voice credentials; no startup publication is enabled. WI#424 remains the boundary work for arbitrary outbound use. |
| External provider credentials and top-level integration/tool policies | Azure CLI credentials become external references and policy support is additive. No new exposure config is enabled by this lane; existing tool denies remain in force. |

**SDK and build compatibility:** the generated promise client's six bridge
methods remain present, with both stale nested paths absent — asserted on this
branch by running `scripts/sdk_surface_assert.ts` (bun 1.4.2) directly against
the pinned v2.0.32 client/schema/protocol source: 6 methods present, 2 stale
absent, all 8 bridge event names present. `session.create/prompt/interrupt`,
`permission.reply`, `event.subscribe`, and `message.list` retain the shapes
used by the executor (including cursor paging without `order`). The CLI still
honors `OPENCODE_CONFIG`/`OPENCODE_CONFIG_DIR` (`server-process.ts:118,127`)
and the injected password (`Env.password` reads `OPENCODE_PASSWORD` then
legacy `OPENCODE_SERVER_PASSWORD`, `env.ts:10-11`). `packages/cli/script/build.ts`
still carries exactly one `bytecode: true,` and one `splitting: true,`, so the
checked reproducibility patch applies unchanged.

**WI#369 deferral:** this bump proceeds without WI#369, as explicitly
permitted by WI#427 gate 6. The existing gate still uses syntax-based call
binding and corpus-wide event-literal matching; it is not a type/payload or
V2-union proof. Goals item 64 remains open.
**(Superseded 2026-10-18, item 64:** WI#369 landed on this tree — the builder
stage now runs a `tsgo --noEmit` gate over the SDK-facing scripts against the
pinned generated client types, and `sdk_surface_assert.ts` asserts the eight
bridge event names against the pinned `V2Event` union. The Gentoo
build/rollout recorded below ran through that strengthened gate.)

**Verification (this branch, 2026-10-18):**

- Compile-out guard executed against a fresh `--depth 1` clone at the pinned
  commit: stubbed `remote-tunnel.ts`, importers use exactly
  `run`/`ensure`/`hostname`, zero `@opentunnel/*` specifiers remain.
- Lane suites: all 5 Python suites pass (67 tests); all 4 bun TS self-tests
  pass. Repo-wide `.gitlab/tests` discovery: **322 passed, 4 skipped** (the
  four Pebble `dash` tests need a shell absent on this host).
  `ic/scripts/tests/test-mt-admin-opencode2.sh` passes in full.
- Full local Podman image build passes from this checkout
  (`mt-admin build-opencode2-worker`): compile-out stub applied in-build,
  binary marker scan clean, `opencode --version` asserted `opencode v2.0.32`,
  all embedded self-tests and both exposure-policy asserts green, leak
  tripwires clean. Image `6b92273222ad…` (revision `09c4cb7ae…`), slim
  profile; rebuilt from the tenant clone during `build-tenant`
  (`df42032e6c15…`, revision `4db886f7b…` — digest differs only by the
  source-revision label).
- **Real tenant job (item 71):** temporary test tenant `spitfire` (WI#273
  name, non-animal) provisioned on this host (Arch Linux, systemd 262) with
  `--with-opencode2`, running this branch. Job
  `cbf1c213-2d35-4f5a-a25c-e5031f3f9efa` drove the full path (gateway chat →
  LLM loop → `create_job` → approval gate → resume → WS dispatch → v2.0.32
  executor) to `result {"content": "Completed: OC2_WORKER_OK"}`. Live exposure
  checks before and after: `service.json` = `{"disabled": true}`, no
  `opentunnel` identity dir, wrapper refuses `service set remote true` and
  `pair --remote --yes` (exit 77), `worker-listeners spitfire` clean. Full
  security writeup: `docs/ops/ITEM71_OPENCODE2_V2.0.32_PUBLIC_HTTPS_TUNNEL.md`.
- **Gentoo rollout + real jobs (item 66, 2026-10-18, host: Gentoo/OpenRC):**
  fat image rebuilt from this tree into the admin store
  (`2ee1fe9c9f3e…`, revision `01f3e69aa…`) via
  `mt-admin build-opencode2-worker --with-toolchains` — builder tsgo gate,
  compile-out stub, in-image `sdk_surface_assert` (6 methods present, 2
  stale absent, 8 event names in the pinned `V2Event` union), all TS/Python
  self-tests, binary marker scan and image-filesystem assert green — then
  `restart-tenant` recreated both Gentoo opencode2 containers
  (`_ensure_tenant_image` save|load + contract-hash recreate, the !1663
  path). `soarin` job `c66fc949-96ae-4aeb-ab20-7f4e8d262b2b` →
  `result {"content": "Completed: … exact stdout: OC2_WORKER_OK"}`
  (bridge `success (19886ms)`); `scootaloo` job
  `953f47bb-817f-4e40-9dfb-48de6ee1613d` →
  `result {"content": "Completed: OC2_WORKER_OK"}` (bridge
  `success (28704ms)`, approval-gate `action=always` path exercised).
  Exposure checks on both tenants, before and after the jobs: in-container
  `opencode --version` = `opencode v2.0.32`, `service.json` =
  `{"disabled": true}`, no `opentunnel` identity dir, wrapper refuses
  `service set remote true` / `pair --remote --yes` / non-loopback
  `serve --hostname` (exit 77), `worker-listeners <tenant>` clean.
  scootaloo's lane was found unconfigured (no `OPENCODE2_BASE_URL`;
  pre-dates the WI#330 inheritance) — fixed with
  `configure-opencode2 scootaloo --base-url <tenant LLM endpoint>` before
  its job.

**Remaining acceptance / rollout:** protected `worker-opencode2-candidate`
(including the uncached whole-image digest comparison) and
`worker-opencode2-scan` must still pass on the bump commit before promotion;
an MR pipeline or local build does not satisfy those gates. Roll out via
`mt-admin build-opencode2-worker` then `restart-tenant <tenant>` to recreate
opencode2 worker containers. Verify no remote service config or OpenTunnel
identity, effective sharing disabled, wrapper refusal of
`service set remote true`, and `worker-listeners <tenant>` clean before
driving jobs. (Done 2026-10-18 for the Gentoo host's tenants `soarin` and
`scootaloo` — see the item-66 rollout bullet above; the Arch host's item-71
tenant `spitfire` ran the same checks at provisioning time.)

**Rollback:** preserve each tenant's worker state before its first v2.0.32
boot. Upstream adds the `project.time_archived` column and migrates Azure
CLI credential records from OAuth tokens to external credentials. An image
downgrade alone is not a verified database rollback; restore the matching
pre-upgrade state with the prior image if rollback is needed.

### v2.1.2 bump — item 83 (2026-10-21)

**Authorization:** the WI#427 boundary crossing (OpenTunnel compile-out,
wrapper, config and image assertions) remains the standing control set for
every v2 pin; this bump re-verified each gate against the pinned source
rather than trusting the previous pin's review. The tag was independently
resolved with `git ls-remote` to
`a517687fde98a7329ada3ae644a01a9fc6c4e99e` (lightweight tag resolving
directly to the commit, same as every prior pin; the fresh clone's HEAD
matches). Dockerfile defaults, the CI case block, and the CI validator
fixtures moved together. The Bun base digest and `DEBIAN_SNAPSHOT` stay
unchanged (WI#361 owns those).

**Gate re-verification on the fresh v2.1.2 clone:**

- compile-out guard: `packages/cli/src/services/remote-tunnel.ts` unchanged
  in path and importer surface (`run`/`ensure`/`hostname` exactly);
  `@opentunnel/client` stays declared-but-unimported after stubbing — same
  shape as v2.0.32, and the runtime binary marker scan remains mandatory.
- reproducible-build anchors: `script/build.ts` still sets `bytecode`/`splitting`
  and takes `--single`; the patch script exits 0 on the pinned tree.
- SDK surface (`sdk_surface_assert` against the pinned client):
  6 bridge method paths present (`session.create/prompt/interrupt`,
  `permission.reply`, `event.subscribe`, top-level `message.list`),
  the 2 known-stale paths still absent (`session.message` exposes only
  `get`; `permission` is a top-level group), and all 8 bridge event names
  are member literals of the pinned generated V2Event union.
- builder-stage WI#369 typecheck gate: **binary rename** — v2.0.32's
  `@typescript/native-preview` shipped the native compiler as `tsgo`;
  v2.1.2 pins `@typescript/native` (npm:typescript@7.0.2) whose bin is
  `tsc`. The Dockerfile gate invocation moved with the pin and
  `tsc --noEmit -p tsconfig.sdk-surface.json` exits 0 against the pinned
  types (verified on the clone with a full `bun install`).
- behavior pins: the build-agent permission defaults still ship only
  `* allow` plus the `.env` asks (the KG-2 external_directory restore stays
  required); Code Mode `session_move`/`session_rename` still carry no
  permission assert of their own (KL-1/KL-2 deny rules stay required);
  `service.json` keeps the `{remote: {route}, remoteAccepted}` shape and
  the boot reset stays valid; `pair --remote` consent still exists and the
  wrapper refusals cover it; `updater.ts` still honours
  `OPENCODE_DISABLE_AUTOUPDATE` (`inspect()` gate reads it at v2.1.2).
- **new upstream surfaces at v2.1.2, looked at (the !1831 review's MG-3):**
  `SkillScan` (`packages/core/src/config/plugin/skill-scan.ts`, new)
  replaces v2.0.32's glob with an explicit breadth-first walk of the project
  tree to depth 6 collecting `SKILL.md` files plus depth-0 `.md` files and
  skipping dot-directories — in this lane the project tree is the
  task-writable workspace, so everything it discovers is task-authored
  input to the serve process, the trust class the lane already records
  (the EF-10 workspace-shadowing note); no new exposure. The Code Mode
  `todo` plugin tool (`packages/core/src/tool/plugin/todo.ts`, new, name
  `todowrite`) ships disabled behind `experimental.todo === true` — the
  lane config does not set it, so it stays dormant unless task-authored
  workspace config enables it, and unlike `session_move`/`session_rename`
  it carries its own `permission.assert` (`resources: ["*"]`,
  `save: ["*"]`), so it needs no extra deny rule; task-scoped todo state
  is not an exposure surface.
- in-host lane suites against the pinned client: bridge, executor, plugin,
  proxy, sdk_surface (x2), result, path, prompt self-check suites pass;
  `compile_out`, `entrypoint_config`, `exposure_policy_assert`,
  `prepare_reproducible_build`, and `study_notes_config` python suites pass;
  `.gitlab/tests` 234/234 and `validate_config.py` clean.

**Remaining acceptance / rollout:** unchanged from the v2.0.32 entry — the
protected `worker-opencode2-candidate` (whole-image digest comparison) and
`worker-opencode2-scan` jobs must pass on a protected pipeline before
promotion, then `mt-admin build-opencode2-worker` + `restart-tenant` per
tenant with the same live exposure checks (service.json reset, wrapper
refusals, `worker-listeners` clean). **Blocked at the pin's landing (the
!1831 review's MG-1):** the instance's `throttle_unauthenticated_web` rate
limit is blocking the registry push of every worker candidate build since
pipeline 6151 (the !1828 review's MD-1), so no image has been built from
this pin yet — the candidate/scan gate runs only once that limit is lifted
(maintainer action, not in-tree), and until then tenants keep their current
v2.0.32 images. Tenant rollouts are recorded below as they happen.

v2.0.24 bump (2026-10-07, v2.1.5.0 checklist item #2): tag SHA
`e7a34f09bfd9134dfade5a8ddb843f7030bc9a69` verified live against the
anomalyco/opencode remote (`git ls-remote`, this date; a lightweight tag
resolving directly to the commit, same as v2.0.23). The `oven/bun` base
digest and `DEBIAN_SNAPSHOT` were reconsidered and stay unchanged. The
v2.0.23→v2.0.24 upstream diff is large (~1036 files) but the lane's surface
is unchanged, re-verified against the pinned tree itself (not protocol
identifier strings): the generated promise client still has all six bridge
call paths (`session.create/prompt/interrupt`, `permission.reply`,
`event.subscribe`, top-level `message.list`); `session.message` still
exposes only `get` and there is still no `session.permission.reply`;
`message.list`'s query shape and envelope are unchanged (`limit` capped at
200 by the `SessionMessagesQuery` schema, `order`/`cursor`, the server-side
`type` filter including `assistant`, `{ data, cursor: { previous?, next? } }`),
and the server handler still rejects cursor+order with 400
`InvalidCursorError`; every event name the bridge matches
(`session.execution.{succeeded,failed,interrupted}`, `session.idle`,
`permission.asked`, `session.text.delta`, `session.tool.{success,failed}`)
is unchanged; `SessionCreateInput` still takes `{ title, permissions }`,
`SessionPromptInput` `{ sessionID, text }`, and `SessionCreateOutput` still
unwraps `.data`; `OPENCODE_CONFIG` is still honored
(`packages/cli/src/server-process.ts`); and the `@opencode/client` exports
map (`./promise` → `src/promise/index.ts`) plus the promise `make` shape
(raw groups spread, `event` wrapped by `SharedEvents` whose
`subscribe({ signal })` the executor uses) are unchanged. The visible
upstream drift is internal to the lane: a promise service-discovery
refactor (`service-probe.ts` new, `service-contender.ts` and
`promise/service.ts` reworked — the bridge constructs the client with an
explicit `baseUrl` and never calls `ensure`/`discover`) and an ACP-mode
internals rework (the lane runs websocket mode). `packages/cli/script/build.ts`
is byte-identical between the two tags; the build-profile patch contract was
still executed host-side against the pinned file (exactly one
`bytecode: true` / `splitting: true` each, `prepare()` flips both, a
double-apply is rejected), and the moved upstream `bun.lock` is consumed by
the Dockerfile's `bun install --frozen-lockfile` from the tag itself. The
`prepare_reproducible_build` self-test fixture string moved to 2.0.24 — the
fixture is synthetic (it guards the patch logic, not the pin; the executed
fetch-and-patch check above covers the pin). Host-side
`prepare_reproducible_build_test.py` passed (3/3) and the ci-config
`WorkerVersionPinTests` passed (15/15) against the new pins. **Not yet
executed:** no pipeline can build the v2.0.24 image before merge (candidate
jobs exist only on protected refs), so before promotion the
`worker-opencode2-candidate` job on the first protected pipeline containing
this pin must run green (whole-image candidate-versus-clean-rebuild digest
gate), and a runtime smoke against the candidate image should re-confirm
the executor paths end-to-end.

v2.0.21 bump (2026-10-01): tag SHA `8a8bd622a3d7dc29ccf30ec17f84e363ed95ed72`
verified against the anomalyco/opencode remote (v2.1.4.0 checklist item #2
repass). The Dockerfile patches the upstream CLI build script at that SHA
(exactly one `bytecode: true` / `splitting: true` each, so the patch contract
holds); the `prepare_reproducible_build` self-test fixture moved to 2.0.21 to
keep guarding the patch contract during the image build. The v2.0.20→v2.0.21
diff is additive for this lane's surface: `session.create` (with
`permissions`), `session.message.list`, `session.permission.reply`,
`session.interrupt`, `event.subscribe`, `OpenCode.make`, and every event name
the bridge matches (`session.execution.{succeeded,failed,interrupted}`,
`session.idle`, `permission.asked`, `session.text.delta`,
`session.tool.{success,failed}`) are unchanged — the only client/protocol
diffs are an additive optional `message` query param on `session.form.cancel`
(and matching optional `FormState.cancelled.message` /
`SessionFormCancelInput.message` generated-type fields), which the bridge
does not use. Host-side `prepare_reproducible_build_test.py` passed (3/3),
including the untouched-failure contract. Whole-image reproducibility must
still pass the CI candidate-versus-clean-rebuild gate before promotion.
(2026-10-06 correction: this entry's `session.message.list` /
`session.permission.reply` "unchanged" claim was checked against protocol
identifier strings, not the promise client the bridge imports; neither
method existed at v2.0.21 — WI#320 F7 observed the runtime TypeError. Both
call paths were corrected in the v2.0.23 bump above.)

v2.0.20 bump (2026-09-30): tag SHA `84c9be93a56304a108f1a22df0c5d62c26d5b6ca`
verified against the anomalyco/opencode remote (v2.1.4.0 checklist item #2).
The Dockerfile patches the upstream CLI build script at that SHA (exactly one
`bytecode: true` / `splitting: true` each, so the patch contract holds); the
`prepare_reproducible_build` self-test fixture moved to 2.0.20 to keep guarding
the patch contract during the image build. The v2.0.18→v2.0.20 diff is
additive for this lane's surface: `session.create` (with `permissions`),
`session.message.list`, `session.permission.reply`, `session.interrupt`,
`event.subscribe`, `OpenCode.make`, and every event name the bridge matches
(`session.execution.{succeeded,failed,interrupted}`, `session.idle`,
`permission.asked`, `session.text.delta`, `session.tool.{success,failed}`) are
unchanged — the new `credential.list`/`credential.create` client endpoints and
additive generated-type fields (e.g. `SessionStructuredError.response`,
`ConnectionStatus`) are not used by the bridge. Host-side
`prepare_reproducible_build_test.py` passed (3/3), including the
untouched-failure contract. Whole-image reproducibility must still pass the CI
candidate-versus-clean-rebuild gate before promotion.
(2026-10-10 correction, WI#329: this entry's `session.message.list` /
`session.permission.reply` "unchanged" claim was checked against protocol
identifier strings, not the generated promise client the bridge imports;
neither method existed at v2.0.20 (WI#320 F7 observed the runtime TypeError at
v2.0.21). Both call paths were corrected in the v2.0.23 entry above.)

v2.0.18 bump (2026-09-27): tag SHA `cd9a14a6b688d4021bee381dfd39d2cef9c0f862`
verified against the anomalyco/opencode remote (v2.1.3.0 checklist item #2).
The Dockerfile patches the upstream CLI build script at that SHA; the
`prepare_reproducible_build` self-test fixture moved to 2.0.18 to keep guarding
the patch contract during the image build. Host-side
`prepare_reproducible_build_test.py` passed (3/3), including the
untouched-failure contract. Whole-image reproducibility must still pass the CI
candidate-versus-clean-rebuild gate before promotion.

Goal-35 verification (2026-09-18): the #177 root cause was reproduced
host-side with the pinned Bun — two full self-test sequences with a scratch
HOME produced identical trees except `~/.local/share/opencode/log/opencode.log`
(unique `timestamp=`/`run=` values), while the patched-profile CLI binaries
built back-to-back were byte-identical (`80e50218...`), bun's transpiler caches
were byte-identical, and the base image disables bun's runtime caches
(`BUN_RUNTIME_TRANSPILER_CACHE_PATH=0`). The fixed `RUN` shape (throwaway
HOME/XDG + trap) ran the full self-test sequence twice leaving zero files
anywhere, so nothing per-run can reach the image layer. The mismatch-diagnosis
helper was exercised inside a sandbox running the pinned buildkit image's
busybox: token-failure, layer-diff, and config-diff paths all degrade or
report correctly. The definitive candidate-vs-rebuild gate rerun happens on
the next protected master pipeline (container jobs only exist on protected
refs).

Per-tenant overrides: `configure-opencode2 <name> [--model <m>]
[--base-url <url>]` (written as `OPENCODE2_MODEL`/`OPENCODE2_BASE_URL`).
Ports come from the dedicated registry range 51000-51999
(`opencode2_wss`/`opencode2_health`, ports-registry v17); units are
`lunarwing-opencode2-<tenant>` (systemd quadlet + OpenRC `-sup`).
Agent-facing dispatch uses `create_job(mode: "opencode2")`; the daemon
side is fully generic (one more `[[sandbox.external_workers]]` entry —
zero Rust changes).

Workspace confinement mirrors the kimi/pebble workers: the bridge
resolves the daemon-supplied `project_dir` against `WORKSPACE_ROOT`
(canonicalized; `..` and symlink escapes are rejected with a
`task_result` error envelope before anything spawns), and one task at a
time owns a workspace subtree — `workspacePathsOverlap`, so symlink
aliases of the same tree collide. The v2 session permissions ruleset
currently grants `read/write/shell: *`; the confinement is the boundary
that ruleset would enforce if it is ever scoped.

Multi-tenant worker configuration sets the task deadline to `3600000ms` (one hour), configurable for new tenant entries with `LUNARWING_MT_EXTERNAL_WORKER_TIMEOUT_MS`. Re-running mt-admin migrates exact legacy `300000ms` entries; set `LUNARWING_MT_MIGRATE_LEGACY_EXTERNAL_WORKER_TIMEOUTS=false` to preserve an intentional five-minute value. External jobs default to asynchronous dispatch (`wait=false`) so they outlive the current conversation turn; completion and progress remain available through job events/status. Explicit `wait=true` remains available for short tasks, but synchronous execution is still bounded by the parent conversation deadline.

## Kimi Code Worker

MoonshotAI/kimi-code is integrated as a persistent LunarWing external worker. A Node.js/TypeScript bridge spawns a `kimi -p` subprocess for each task and translates its streamed output to the `lunarwing-agent-v1` WebSocket protocol. Health is exposed on 8443 and WebSocket traffic on 9090.

**Dockerfile:** `kimicode4lunarwing/Dockerfile`

```bash
cd kimicode4lunarwing
docker build -t lunarwing-worker-kimi:latest .

# Or via the mt-admin lifecycle
sudo ic/scripts/lunarwing-mt-admin.sh build-kimi-worker

# Include same Rust/Go/C/C++/Python/native toolchains as OpenCode worker
sudo ic/scripts/lunarwing-mt-admin.sh build-kimi-worker --with-toolchains

# Or as part of tenant provisioning and build
sudo ic/scripts/lunarwing-mt-admin.sh add-tenant <name> --with-kimi
sudo ic/scripts/lunarwing-mt-admin.sh build-tenant <name> --with-kimi

# Configure per-tenant provider settings
sudo ic/scripts/lunarwing-mt-admin.sh configure-kimi <name> --model <model> --base-url <url> --api-key-file /secure/path/kimi-api-key
```

Default Kimi image is slim. `--with-toolchains` enables Rust 1.96.1, Go 1.24.3,
C/C++, Python headers, native build systems, and same development libraries as
OpenCode profile. Server mode requires `AGENT_AUTH_TOKEN`, even on loopback;
Compose rejects a missing value before container launch. `KIMI_API_KEY` remains
optional for OAuth-backed setups. mt-admin keeps both credentials out of process
arguments by materializing private `kimi-runtime.env`; API key input uses file
path rather than plaintext argv. The Kimi Code CLI and bridge are installed
with `npm ci` from committed integrity locks. `KIMI_CODE_VERSION` defaults to
`2.3.0` and must match `kimi-cli/package-lock.json` (the pin moved `0.42.0` →
`0.43.0` → `0.43.1` → `2.0.2` earlier in the v2.1.2.0/v2.1.3.0 cycle, then
`2.0.2` → `2.1.1` on 2026-09-26 for checklist item #2, adopting npm `latest`
again, `2.1.1` → `2.2.0` on 2026-10-15 for the v2.1.6.0 checklist item #4,
and `2.2.0` → `2.3.0` on 2026-10-21 for the v2.1.6.0 checklist item #83,
again npm `latest`; engine requirement unchanged at `node >=22.19.0`, satisfied by the
pinned node 22 base; the 2.x behavior changes the worker cares about are the
headless permission-contract findings below, verified on 2.0.x — the 2.3.0
live CLI contract gate (`kimi_cli_contract_test.ts`) runs during the worker
Dockerfile build; the 2.3.0 real-binary lane was additionally run on the
host (4/4: headless `-p --output-format stream-json` accepted, `--yolo`
rejected alongside `-p`, the baked `[[hooks]]` PreToolUse guard table
valid, and the `mcp.json` spawn + `initialize` → `tools/list` MCP
handshake), so the in-image gate is a re-confirmation of verified
behavior. 2.3.0's new env surface is
`KIMI_CODE_MODEL_MIGRATION_STATE_FILE_NAME` and
`KIMI_CODE_SESSION_RATING_PREVIEW` (survey state renamed); neither is
exported by the worker). **Blocked at the pin's landing (the !1831
review's MG-1):** the protected `worker-kimi-candidate` build is blocked by
the instance's `throttle_unauthenticated_web` rate limit (the !1828
review's MD-1), so no image has been built from the 2.3.0 pin yet —
tenants keep their current 2.2.0 images until the limit is lifted and
the candidate build runs.
An override without a matching reviewed lock fails closed. The pinned version is baked
into `KIMI_CODE_IMAGE_VERSION` and reported as `kimi_code_version`, and
`ic/scripts/tests/test-mt-admin-kimi.sh` cross-checks Dockerfile default, CI
build script, and committed lock version-agnostically. Base images,
Debian indexes, and optional toolchain downloads are also pinned.

For every persistent worker, `add-tenant --with-<worker>` controls the tenant's
selection in `/etc/lunarwing/ports.json`. `build-tenant --with-<worker>` and the
standalone build commands build shared images but do not select a worker for a
tenant.

### Headless permission contract (WI#195, WI#206)

The Kimi worker runs every task through `kimi -p --output-format stream-json`,
a headless batch invocation with no TTY. WI#206 investigation against the
then-pinned CLI (2.0.2, verified empirically with a mock provider through the real
`-p` path plus bundle reading) established what print mode actually enforces:

- `-p` forces the **auto** permission policy regardless of
  `default_permission_mode` (`--yolo`/`--auto` are rejected alongside `-p`).
- The upstream dangerous-command guard is **skipped entirely in auto mode**
  (upstream code returns early for auto) — so with the old
  `dangerous_command_guard = false` bake, dangerous-class commands executed
  with no approval boundary at all. (Guard on/off changes nothing for
  stalls either: WI#195's stall no longer reproduces on 2.0.2.)
- `[[permission.rules]]` from `config.toml` never load in this version —
  user-configured deny rules are dead config.
- **PreToolUse hooks fire before permission checks in every mode**: exit code
  2 blocks the call and the stderr reason is returned to the model as the
  tool error — a bounded, explicit denial.

The worker therefore ships a fail-closed policy instead of blanket
auto-approval:

- `config/config.toml`: `dangerous_command_guard = true` (inert under `-p`
  today, but a future upstream change then fails closed) plus a `[[hooks]]`
  `PreToolUse` / `Bash` entry invoking the baked guard.
- `hooks/headless_command_guard.mjs`: denies privilege escalation
  (`sudo`/`doas`), power/disk destruction (`shutdown`, `reboot`, `mkfs`,
  `wipefs`, block-device writes), fork bombs, remote-content-piped-to-shell,
  recursive `chmod`/`chown` on system roots, SSH-agent socket use, and
  recursive/forced `rm` outside temp roots. Unparseable payloads deny.
  `/app/config` and `/app/hooks` are read-only to the runtime user, so a
  task cannot edit its own guard.
- `entrypoint.sh`: injects the guard + hook into any `KIMI_CONFIG_SOURCE`
  that lacks them, never exports `KIMI_CODE_DANGEROUS_COMMAND_GUARD` or
  `KIMI_CODE_TRUST_WORKSPACE` (unsets both in CLI and bridge mode), and
  keeps `KIMI_CODE_PERMISSION_MODE_REMINDER=0` (prompt noise for a
  fixed-mode worker). `KIMI_CODE_TRUST_WORKSPACE` is kimi-code 2.2.0's
  workspace-trust switch: a truthy value (`1`, `true`, `yes`, `on`) makes
  the CLI spawn every MCP server listed in the repo's `.mcp.json` or
  `.kimi-code/mcp.json` at startup, before any model turn and outside the
  hook (WI#411).
- `kimi_task_executor.ts`: scrubs `KIMI_CODE_DANGEROUS_COMMAND_GUARD`,
  `KIMI_CODE_PERMISSION_MODE_REMINDER` and `KIMI_CODE_TRUST_WORKSPACE` from
  per-task environments after every merge, so task-controlled
  `context.environment` cannot change the approval policy or grant
  workspace trust.
- mt-admin no longer mounts the tenant SSH-agent socket into the kimi
  container (both the imperative `podman run` and the Quadlet), adds
  `no-new-privileges`, and bumps `KIMI_ROOTLESS_LAUNCH_CONTRACT` so existing
  containers force-recreate. Other worker lanes keep the socket.

Verified end-to-end on 2.0.2: `rm -rf <non-temp>`, `sudo`, `shutdown`, and
`curl … | sh` are denied in ~3s with an explicit reason; ordinary commands
(`echo`, `grep`, `git status`, builds, tests) execute unchanged.

## LLM endpoints for LLM-backed workers (WI#330)

The opencode, opencode2, and kimi worker images bake **placeholder** LLM
endpoints (`set-opencode-base-url.invalid`, `set-opencode2-base-url.invalid`,
`set-kimi-base-url.invalid` — the `.invalid` TLD is guaranteed NXDOMAIN).
Earlier images baked developer-network endpoints (an RFC1918 LAN address and a
NetBird MagicDNS name) that are unreachable from any other host; the MagicDNS
name also did not resolve inside the rootless worker containers on the MT
hosts where this was observed. Each entrypoint now **refuses to start** when
the placeholder survives to the effective runtime config (for opencode2 that
is `$OPENCODE_CONFIG` when the operator sets it), with a message naming the
missing env var, instead of dying at the first LLM request after N retries.

Multi-tenant provisioning has **no built-in LLM endpoint default**: `add-tenant`
fails loudly unless it is given `--tensorzero-url` (or fleet
`LUNARWING_MT_TENSORZERO_URL`), or `--llm-base-url` for a non-proxy tenant.
The tenant's TensorZero proxy uses its own `TENSORZERO_URL` from `proxy.env`,
not a fleet literal. systemd passes the validated URL as an argument; OpenRC
passes `--tensorzero-env` and loads the URL only after dropping to the tenant
(WI#376), so tenant data never becomes root-parsed init-script text. Existing
OpenRC deployments must first neutralize installed proxy units fleet-wide with
`render-units --all-proxies`, then update tenant proxy code before restarting.
The patched `upgrade-tenant` performs this mandatory neutralization before
service operations; see [the proxy URL boundary](MULTITENANCY-PRODUCTION.md#proxy-url-boundary-wi376).

mt-admin renders `OPENCODE_BASE_URL` / `KIMI_BASE_URL` / `OPENCODE2_BASE_URL`
into the tenant's `lunarwing.env`. Resolution order per worker:

1. Explicit per-worker flag (`add-tenant --opencode-base-url` /
   `--kimi-base-url` / `--opencode2-base-url`) or a later
   `configure-* --base-url` call.
2. An operator-set value already in `lunarwing.env` (any value without a
   `<KEY>_SOURCE=inherited` marker) — preserved across re-renders.
3. Inherited from the tenant's LLM endpoint: `--llm-base-url` /
   `LUNARWING_MT_LLM_BASE_URL` when container-reachable, else the tenant's
   TensorZero upstream normalized to `/openai/v1`. Inherited values are marked
   `<KEY>_SOURCE=inherited`, **re-derived on every render** (a TensorZero move
   reaches the workers without a per-worker `configure-*` call), and skipped
   by `export-tenant` so a migrated tenant re-derives them on the target host.
4. If none of those yields a URL and the worker is
   enabled, the render/upgrade fails loudly naming the fix — an unreachable
   default is never rendered.

Automatic inheritance excludes host loopback (`127.0.0.1`, `localhost`,
`[::1]` — inside a rootless container that is the container itself, and the
daemon's own proxy-loopback default is therefore never inherited) and known
overlay MagicDNS suffixes (`*.netbird.selfhosted`, `*.netbird.cloud`,
`*.ts.net`, `*.tailscale.net`). Use a numeric overlay/LAN IP or a public DNS
name. This is a hostname filter, not a network probe; operators must ensure
their chosen endpoint (including explicit per-worker overrides) is reachable.
Supplied URLs are shape-validated (http/https only, no whitespace or control
characters, no userinfo, no `%`) at `add-tenant` and `configure-*` time, and
written quoted.

**Upgrade note:** `upgrade-tenant` (and `patch-env`) backfills a missing
`*_BASE_URL` for every *enabled* worker from the tenant's endpoint —
`LLM_BASE_URL` when container-reachable, else the `proxy.env`
`TENSORZERO_URL`. Upgrade performs this preflight before stopping the tenant
or rebuilding worker images, and fails with services still running when no
endpoint is derivable. Pre-existing tenants therefore keep their workers
through the placeholder rollout; a tenant whose daemon endpoint
is itself loopback-only needs one explicit `configure-*` call.

Override knobs (no image rebuild; restart the worker afterwards):

```bash
sudo ic/scripts/lunarwing-mt-admin.sh configure-opencode  <name> --base-url <url>
sudo ic/scripts/lunarwing-mt-admin.sh configure-kimi      <name> --base-url <url>
sudo ic/scripts/lunarwing-mt-admin.sh configure-opencode2 <name> --base-url <url>
```

All three `configure-*` commands preserve omitted model/base URL values and
the URL's inherited marker. Model-only or Kimi API-key-only changes therefore
still follow future tenant endpoint changes and remain omitted from export;
only an explicit `--base-url` turns an inherited URL into an operator override.
Kimi trims model/URL overrides consistently while preserving API-key bytes.
For the standalone docker-compose developer path, the three `docker-compose.yml` files forward
the `*_MODEL`/`*_BASE_URL` keys from `.env`/the shell into the container; an
unset key still fails the entrypoint guard loudly.

The MR shell-harness lane exercises all three real entrypoint configuration
sections against the baked configs in isolated temporary directories (unset,
empty, whitespace, model-only, explicit URL, mounted config, and OpenCode 2's
effective `OPENCODE_CONFIG`). These checks stop before health/CLI startup;
whole-image builds remain the protected worker publication gate.

## Shared Protocol

All worker images use the `lunarwing-agent-v1` WebSocket subprotocol; the legacy `ironclaw-agent-v1` alias is no longer accepted. Messages are JSON envelopes with `id`, `type`, `timestamp`, `payload`. The worker sends `ready` on connect, receives `task_request`, streams `task_progress`, and sends a final `task_result`.

### Runtime control channel (WI#328)

External worker jobs are no longer fire-and-forget: while a task runs, the daemon can deliver follow-up prompts and text directives into it over the same per-task WebSocket connection. The channel is **capability-gated**: a worker advertises `capabilities: ["task_prompt"]` in its `ready` payload, and the daemon rejects prompts for jobs whose worker never advertised it (`ExternalWorkerControlUnsupported`, HTTP 400) instead of reporting a false "queued". Prompts submitted before the handshake completes are rejected as still-connecting (HTTP 409). Two envelope types extend the protocol:

- `task_prompt` (daemon → worker): `{task_id, seq, prompt, done}`. `prompt` is follow-up instruction text (TEXT directives only — validated daemon-side before enqueue; no shell/exec passthrough, 16 KiB cap, no NUL bytes). `seq` is monotonic per job for ordering and dedupe. `done: true` signals "no more prompts coming, wrap up cleanly" — it grants **one final wrap-up turn**: the daemon rejects every later prompt (HTTP 409) from the moment the wrap-up is accepted, and in the opencode worker the bridge closes the task's queue at accept time so a post-done frame is refused instead of queued-and-dropped. Workers must hold queued prompts until the current turn/LLM call ends — never mid-call.
- `task_prompt_ack` (worker → daemon): `{task_id, seq, accepted}` — a receipt meaning "the worker received and queued it". The daemon records acks only for seqs it actually delivered, and the persisted `control_ack` row carries the `accepted` flag (absent counts as accepted), so a worker-refused prompt is auditable as such rather than masquerading as delivered. A delivered seq whose ack never arrives (e.g. the worker's final-drain refusal races `task_result`) is persisted as an explicit `unacked` `control_prompt` row when the task ends, so the audit trail shows the gap instead of implying delivery.

Delivery is **at-most-once** and honest about it: the daemon's per-job control queue is bounded (32 in flight, 64 per job total — a full queue or cap rejects with HTTP 429), a failed send drops the prompt with a `delivery_failed` job event instead of killing the task, the queue is closed the moment the task's read loop exits (including at the start of the cancel handoff) so late senders get an explicit rejection (HTTP 409), and prompts still queued when the task ends are recorded as `dropped`. There is no redelivery; callers that need certainty should check the job event log for the `delivered`/`control_ack` rows. Every enqueue (with SHA-256 digest and length — never the prompt text), delivery, drop, and ack is persisted in the job event log (`control_prompt` / `control_ack` event types) for an auditable control history. Enqueued prompts do **not** extend the task's timeout/fuel budget.

From the agent side this is surfaced through `job_prompt`, which routes `external:`-mode jobs with a live task handle to the control channel instead of rejecting them — note that an "Always allow" approval grant on `job_prompt` now also reaches credentialed external workers, not just local sandbox jobs. From the browser side, `POST /api/jobs/{id}/prompt` routes the same way and job detail reports `can_prompt: true` only for running external jobs whose worker advertised the capability.

Worker support today: the **opencode** worker (`opencode4lunarwing`) consumes `task_prompt` — it validates frames bridge-side (positive integer seq, non-empty 16 KiB NUL-free prompt, per-task queue bound, seq dedupe), binds every queue to the connection that owns the task (a foreign socket's prompt or cancel is refused), injects queued follow-ups into the same opencode session at the turn boundary (session idle, gated against stale idle replays) framed as `FOLLOW-UP INSTRUCTION (seq=N) from the orchestrator`, closes the queue the moment a `done: true` prompt is accepted (the injected turn is the wrap-up turn), and closes it on every error/cancel/finish path so late frames are acked `accepted: false`. Its final `task_result` output accumulates every turn's canonical text — or the turn's streamed tool/text output when the assistant produced none — and a follow-up turn that fails or times out after the main turn banked completed output resolves as `success` with a `[control] follow-up seq=N failed/timed out` note (without completed output the failure/timeout stands — control notes alone cannot manufacture success), so a late "status?" can never turn finished work into Failed/TimedOut or replace the deliverable. The kimi, opencode2, and pebble bridges do not advertise the capability (pebble runs its CLI one-shot with no session to inject into) and therefore reject prompts explicitly.

`closed` seals **admission**, not consumption: already accepted prompts drain
in FIFO order through the final `done=true` turn. Any terminal failure, timeout
or cancellation records each remaining accepted prompt as
`[control] follow-up seq=N not run`. Permission-reply and SSE errors follow the
same policy as prompt errors. Preserving success requires actual completed
assistant/tool output; control notes alone cannot turn empty or permission-denied
work into success. Explicit cancellation remains `cancelled` and retains completed
output. The 16 MiB daemon WebSocket limit rejects oversized results; it does not
truncate accumulated output or guarantee delivery of an oversized deliverable.
The normal terminal path delivers the banked canonical turns joined. When no
turn was banked, the task resolves `error`; accumulated output is delivered as
diagnostics only (FG-1). Success is
preserved through a failed or timed-out follow-up only when a turn was banked
(canonical text, streamed text, or completed-tool output of that turn),
including tool output from a main attempt retried successfully with no text;
a later failed follow-up preserves that main turn's success (ER-1). Raw
accumulated output feeds the delivered text on the failure paths but never
the success decision itself.

The daemon bounds prompts in UTF-8 bytes; the OpenCode bridge's additional
length check uses 16,384 UTF-16 units. Its queue holds at most 64 pending prompts.
Deduplication applies while admission is open; closed queues refuse redelivery
too. Invalid instructions retain valid task/sequence identifiers in refusal
acks; invalid identifiers themselves cannot be correlated.

Interop: pre-WI#328 workers that never send `task_prompt_ack` still function — the daemon tolerates missing acks and ignores unknown message types, and newer workers must likewise ignore `task_prompt` if they do not implement prompting (they simply omit the capability, and the daemon never sends them the frame).

### Rebuilt image rollout (image-ID contract)

A worker container is bound to the image ID it was created from. `start-tenant` refreshes the tenant's rootless store with a rebuilt image (`_ensure_tenant_image`), but on OpenRC and the non-Quadlet fallback it then only `podman start`s an existing container, and an already-active Quadlet unit is not restarted, so a rebuilt image did not reach a running worker: the old container kept serving the old image until an operator removed it by hand (`restart-tenant` helped only on Quadlet hosts, where the unit re-runs `podman run`). This is how the kimi 2.2.0 image stayed in service without the WI#411/WI#412 fixes. Every worker lane (`pebble`, `opencode`, `opencode2`, `kimi`) now hashes the tenant-store image ID into its container contract (`_worker_launch_contract`, the same mechanism as the LunarVision/LunarVoice sidecar contracts), so `start-tenant` and `restart-tenant` force-recreate the container whenever the image ID differs from the one it was created from, on both init systems. The procedure after any worker change is therefore:

```bash
sudo ic/scripts/lunarwing-mt-admin.sh build-kimi-worker        # or build-<worker>-worker / build-tenant --with-<worker>
sudo ic/scripts/lunarwing-mt-admin.sh restart-tenant <tenant>  # logs "config changed for lunarwing-kimi-<tenant>; force-recreating"
```

Rollout note: the first `start-tenant` after upgrading mt-admin to this version recreates every worker container once, because the stored contract hash gains the image segment. The replaced image stays in the tenant store until the next rebuild's prune. `ic/scripts/tests/test-mt-admin-worker-image-contract.sh` pins the behaviour for all four lanes on both paths.

mt-admin builds also stamp `org.opencontainers.image.revision` with the source checkout's commit (the `SOURCE_REVISION` build-arg every worker Dockerfile declares and CI already passed; mt-admin builds left it `unknown`). `-dirty` is appended when the worker directory has uncommitted or untracked changes and `unknown` is kept outside a git checkout, so a deployed image can be traced to a target commit:

```bash
podman image inspect -f '{{index .Config.Labels "org.opencontainers.image.revision"}}' lunarwing-worker-kimi:latest
```

### Upgrade ordering

Since the `ironclaw-agent-v1` alias was removed (2.1.1), a 2.1.1 daemon offers only `lunarwing-agent-v1` and cannot connect to a worker container that still only accepts the legacy name — every dispatch to such a worker fails the WebSocket upgrade (HTTP 400, no echoed subprotocol) and surfaces as an external-worker connection failure whose reason includes the `lunarwing-agent-v1` / `ironclaw-agent-v1` hint. Workers are protocol-compatible in the other direction (2.1.1 workers accept the primary name only, which pre-2.1.1 daemons also offer), so the safe upgrade order is **workers first, then the daemon**, or — simplest — recreate worker containers in the same maintenance window as the daemon. mt-admin managed upgrades (`build-tenant`, `build-pebble-worker`, `build-opencode-worker`, `build-opencode2-worker`, `build-kimi-worker`, each followed by `restart-tenant`) rebuild the image and, through the image-ID contract above, recreate the worker containers; only manual deployments can strand a legacy-only worker.

## Proxy Note

On networks with TLS-intercepting proxies (e.g., corporate networks), Docker builds will fail with "server certificate not trusted" errors. Either build on a network without MITM proxies, or add the proxy CA certificate to each Dockerfile before the `apt-get`/`apk` steps.
