# Security Report: opencode2 v2.0.32 "Public HTTPS Tunnel" (OpenTunnel remote access)

**Item:** AGENT_GOALS_2.1.6.0 #71 (branch `aj/kimi-71-20261018-0601`, Rarity, 2026-10-18)
**Pin under review:** `v2.0.32` = commit `843a6c514e606b436a7fe713422ace0a93ad3d61`
(lightweight tag, resolved independently with `git ls-remote` against
`github.com/anomalyco/opencode` and cross-checked with the GitHub API on
2026-10-18; matches the MR !1699 pin).
**Authorization:** WI#427 records the maintainer's completed upstream analysis
and explicit approval to cross the v2.0.26 boundary at v2.0.32. WI#357
("workers must never self-expose") remains the standing policy;
`docs/ops/WORKER-CONTAINERS.md` carries the full control set.

This report is the item-71-required full writeup of the security properties of
upstream's new "public HTTPS tunnel" feature — what it is, what it would mean
for a LunarWing worker if it shipped live, and exactly why the shipped controls
make the v2.0.32 pin acceptable while remaining naive-use guards rather than a
boundary.

## 1. What the feature is

OpenCode v2.0.26 introduced **remote access** built on **OpenTunnel**
(`@opentunnel/client`, declared `0.4.0` in the pinned tree's
`packages/cli/package.json`). It is still present, essentially unchanged in
shape, at the pinned v2.0.32 (`packages/cli/src/services/remote-tunnel.ts`,
70 lines, read in full for this report).

OpenTunnel itself (upstream README, `github.com/anomalyco/opentunnel`) is a
**blind TLS tunnel**: each client device receives a unique
`<id>.opentunnel.xyz` hostname and terminates TLS **locally**, so the hosted
relay never holds the certificate private key and never sees HTTP plaintext.
The hosted server is a single Rust binary (`crates/opentunnel-server`) run by
the vendor on Fly.io (`iad`) with PlanetScale MySQL state and ZeroSSL/ACME
DNS-01 certificates over a Cloudflare-hosted zone. The server image is
publishable-source, but the opencode integration has **no supported self-host
option** — the SDK anchors device identity to the hosted `opentunnel.xyz`
zone.

Mechanics at the pinned commit (all line-verified):

- `RemoteTunnel.ensure()` — creates the device's tunnel and issues its
  certificate (foreground, up to 5 minutes, resumable). The device identity is
  persisted in the agent-writable `$XDG_DATA_HOME/opentunnel` under a shared
  `default` profile that the standalone `opentunnel` CLI would also use.
- `RemoteTunnel.run({ route, target, onURL })` — attaches a random-subdomain
  route on the device tunnel and yields the public URL
  `https://<route>.<id>.opentunnel.xyz`. The SDK reconnects through network
  failures by itself; the wrapper retries setup failures on an exponential
  schedule capped at 30 s and stops only on rejected-token / certificate
  failure.
- `RemoteTunnel.hostname(route)` — reads the persisted identity once the
  certificate is ready, so `pair --remote` can display the URL.
- Consumers (exactly three, verified by the compile-out guard):
  `services/service-config.ts` → `ensure` (on `service set remote true`),
  `server-process.ts` → `run` (service mode attaches the tunnel in the
  background), `commands/handlers/pair.ts` → `hostname`.

User-facing surface:

- `opencode service set remote true` persists `remote: { route: <random> }` in
  `service.json` (legacy boolean `remote: true` is auto-migrated). The route is
  treated as a secret — `service get` redacts it ("as sensitive as the
  password") — because it is the unguessable half of the public address.
- `opencode pair --remote` prints/renders the pairing code plus the **public**
  URL(s) (non-loopback URLs are separated from loopback ones). First-time use
  shows the consent warning — *"This will expose your OpenCode server to the
  internet. Traffic is end-to-end encrypted, and access requires your server
  password."* — and persists `remoteAccepted: true`; **`--yes` / `-y` gives
  that consent non-interactively**, so a scriptable agent can enable remote
  access with zero human in the loop.
- Route names never appear in public certificate logs (the wildcard
  certificate covers subdomains), so the address is unguessable — but
  upstream's own README is blunt: **"anyone you share it with can reach the
  service: it is not authentication."** The URL plus the per-boot server
  password (or a pairing code) is the whole of the access control.

## 2. Why this is dangerous inside a LunarWing worker

WI#357: LunarWing owns every public exposure path through the daemon's tunnel
(`ic/src/tunnel/`); workers must never self-expose. A live OpenTunnel inside
the opencode2 worker breaks that in a specific, bad way:

- **Outbound-only, listener-invisible.** The tunnel is established by the
  worker *dialing out* to the relay; visitor traffic arrives over that
  outbound bridge WebSocket. There is **no inbound listener** — so
  `mt-admin worker-listeners` (control 9, host-side `/proc` audit) cannot see
  an established tunnel at all. Prevention, not detection, is the only
  in-image option; this is the core reason the compile-out exists.
- **Prompt-injection-to-public-URL in one command.** A task carrying injected
  instructions could run `opencode pair --remote --yes` (non-interactive
  consent) or `opencode service set remote true` and print a public HTTPS URL
  into the worker's agent API — a remote-control channel reachable by whoever
  obtains the URL, with session content transiting a third-party relay
  operator's infrastructure (blind to plaintext, but not blind to metadata:
  connection timing, visitor IPs at the relay, device identity anchored in
  the hosted zone).
- **Anonymous, durable device identity in agent-writable storage.**
  `$XDG_DATA_HOME/opentunnel` is owned by the agent uid; the identity survives
  opencode restarts and is shared with any other SDK consumer on the machine.
- **Consent is one-time and persistent.** `remoteAccepted: true` suppresses
  the warning forever after; both it and the route live in the mutable
  `service.json`.
- **Pairing codes are bearer grants.** The rendered pairing payload
  (`{ code, urls }`) is sufficient to pair a remote UI against the exposed
  server.

## 3. Controls shipped with the v2.0.32 pin (all re-verified on this branch)

| # | Control | State at v2.0.32 (this branch) |
|---|---|---|
| 8 | **Compile-out** (`scripts/compile_out_remote_tunnel.py`, builder stage) | `remote-tunnel.ts` is replaced with a fail-closed stub (`run`/`ensure` die; `hostname` reports none). **Verified against the real pinned tree on 2026-10-18**: guard stubbed the module, importers use exactly `run`/`ensure`/`hostname`, no quoted `@opentunnel/*` specifier survives anywhere under `packages/{cli,server,schema,core}/src`. Drift (module moved, new export in use, leftover import) fails the build. The declared `@opentunnel/client` dep remains in the upstream frozen lockfile but imports nothing; the runtime-stage **binary marker scan** (`exposure_policy_assert.py --binary`) is the backstop that proves it is not bundled. |
| 2 | **Wrapper** (`opencode-exposure-guard.sh` as `/usr/local/bin/opencode`) | Every-argv-token scan refuses `service`, `pair`, `--service`, and non-loopback `--hostname`. Test suite extended on this branch to lock refusal of `pair --remote`, `pair --remote --yes`, and `--print-logs pair --remote -y` (39/39 pass). |
| — | **Boot reset** (`entrypoint.sh:32-33`) | Every boot removes any planted `service.json` (FIFO/dir/symlink safe) and rewrites `{"disabled": true}`, discarding a planted `remote.route` and `remoteAccepted` — the v2.0.32 field shapes are now planted by `entrypoint_config_test.py` and the boot suite passes (7/7). |
| — | **Config pins** (`config/opencode2.json`, root-owned read-only) | `share: "disabled"` and `update: "disable"` re-verified as valid schema literals in the pinned `packages/schema/src/config.ts` (lines 39, 42). Session sharing/publication stays off; self-update stays off. |
| 2b | **Bridge permission denies** | `sessionPermissions()` in `opencode_task_executor.ts` denies shell forms of `opencode service/pair/serve` (ordering after the allow-all rule preserved). |
| 5 | **Ownership/mode + wrapper digest anchor** | `LUNARWING_WRAPPER_SHA256` build-arg anchor compared by the in-image `--image-filesystem` assert; guard files root-owned read-only. |
| 9 | **Live listener audit** | `mt-admin worker-listeners <tenant>` still runs on the tenant (see §5 caveat: it cannot see an *outbound* tunnel — it catches non-loopback *binds*, e.g. a direct `serve --hostname 0.0.0.0`). |
| — | **SDK gate** (WI#329) | `sdk_surface_assert.ts` executed locally against the pinned v2.0.32 client/schema/protocol source: 6 bridge methods present, 2 known-stale paths absent, all 8 bridge event names present. |

## 4. Residual risk — stated plainly

These controls are **naive-use guards, not a boundary against a compromised
agent**, unchanged in kind from the !1490 gate record:

- The real binary at `/usr/local/lib/opencode2/opencode` remains executable by
  uid 10003; an agent that execs it directly bypasses the wrapper.
- `service.json` is agent-writable between boots; the reset is per-boot.
- With open egress, a compromised agent can download a stock upstream opencode
  or run `@opentunnel/client` directly under bun and establish the tunnel
  anyway — no in-image control can stop that. **Only the deferred egress
  allowlist (now tracked as WI#424 for 2.1.7.0) is a real boundary**, and it
  remains unimplemented. The WI#427 crossing was approved explicitly on this
  basis.
- WI#369's SDK-gate blind spots (syntax-based call binding, corpus event
  matching rather than type/payload proof) remain open and are unchanged by
  this bump.
- Upstream migrates its database at this pin (`project.time_archived`, Azure
  CLI credential records moved to external credentials): **an image downgrade
  alone is not a verified rollback** — preserve per-tenant opencode2 state
  before first v2.0.32 boot.

## 5. Other exposure-relevant changes in v2.0.25–v2.0.32

Reviewed in the pinned source diff; none add a listener or an automatic
publication default:

- **Experimental voice API** (`packages/core/src/voice.ts`,
  `protocol/src/groups/voice.ts`): authenticated **outbound** session creation
  with caller-supplied credentials. The bridge does not call it and the lane
  provisions no voice credentials.
- **Remote MCP**: capability negotiation / query-param compatibility / numeric
  timeouts — outbound client behavior for explicitly configured MCP servers
  only.
- **Auto-update**: WinGet support and install-method detection (Windows-only;
  N/A in this Linux image). `update: "disable"` still honored.
- **Provider credentials**: Azure CLI credentials become external references;
  additive policy support. No new exposure config enabled by this lane.

## 6. Verification evidence (this branch, this host, 2026-10-18)

- Tag resolution: `git ls-remote https://github.com/anomalyco/opencode.git
  refs/tags/v2.0.32` → `843a6c514e606b436a7fe713422ace0a93ad3d61`; GitHub API
  `git/ref/tags/v2.0.32` agrees (lightweight tag → commit).
- Compile-out guard run against a fresh `--depth 1` clone at that commit:
  stubbed `remote-tunnel.ts`, importers `ensure`/`hostname`/`run` only, zero
  `@opentunnel/*` specifiers after stubbing.
- `packages/cli/script/build.ts` at the pin still carries exactly one
  `bytecode: true,` and one `splitting: true,` — the reproducible-build patch
  applies unchanged.
- SDK gate executed locally (bun 1.4.2) against the pinned client source:
  6 methods present, 2 stale absent, 8 event names present.
- Lane suites: all 5 Python suites pass (build profile 3, study-notes 8,
  exposure/wrapper 39, boot config 7, compile-out 10 = 67); all 4 bun TS
  self-tests pass (executor incl. call-site/event bindings, path_expand,
  opencode_result, task_prompt).
- Repo CI tests: `.gitlab/tests` discovery **322 passed, 4 skipped** (the four
  Pebble `dash` tests need a shell absent on this host);
  `ic/scripts/tests/test-mt-admin-opencode2.sh` passes in full;
  `bash -n .gitlab/scripts/build-worker-image.sh` clean.
- Image build and live-tenant evidence: see the v2.0.32 bump record in
  `docs/ops/WORKER-CONTAINERS.md` (filled in from this branch's build and the
  temporary test tenant), and §7 below.

## 7. Live-tenant evidence

Temporary test tenant **spitfire** (WI#273 list name, non-animal — exempt
from the animal-name teardown routine), provisioned on this Gentoo/OpenRC
host 2026-10-18 with `--with-opencode2`, port block 10020-10029
(`opencode2_wss` 51020), LLM endpoint `http://100.88.232.146:3000/openai/v1`
(NetBird TensorZero; `OPENCODE2_BASE_URL` inherited per WI#330). The tenant
clone tracks this branch (`aj/kimi-71-20261018-0601` @ `4db886f7b`), so the
exercised image is this bump.

- Image: `lunarwing-worker-opencode2:latest` built from the tenant clone
  (`mt-admin build-tenant spitfire --with-opencode2`, BUILD_EXIT=0); the
  standalone pre-build from this checkout (`mt-admin build-opencode2-worker`)
  also passed with every in-image gate green — compile-out stub applied
  (`importers use: ['ensure', 'hostname', 'run']`), binary marker scan clean,
  `opencode --version` asserted `opencode v2.0.32`, all embedded self-tests
  ran, leak tripwires clean, final `--image-filesystem` assert passed.
- Container boot state: `service.json` = `{"disabled": true}`;
  `$XDG_DATA_HOME` contains only `opencode` — **no `opentunnel` identity**
  created; in-container `opencode --version` → `opencode v2.0.32` (via the
  wrapper).
- Wrapper refusals live in the running container:
  `opencode service set remote true` → exit 77 refused;
  `opencode pair --remote --yes` → exit 77 refused.
- WI#357 audit: `mt-admin worker-listeners spitfire` → **clean**, before and
  after the job (only the expected bridge-WS 51020 + health 8443 binds, all
  published host-loopback only).
- Real job, full path (gateway chat → LLM loop → `create_job` → approval
  gate → resume → orchestrator WS dial → bridge → v2.0.32 executor →
  `job_events` → assistant reply): job `cbf1c213-2d35-4f5a-a25c-e5031f3f9efa`
  — bridge `success (6183ms)`; `job_events` stream
  `OC2` + `_WORKER_OK` deltas, result `{"content": "Completed:
  OC2_WORKER_OK"}`; the assistant relayed job id, status `completed`, output
  `OC2_WORKER_OK`. The approval gate parked the `create_job` call exactly as
  designed (pending gate observed in `state/pending-gates.json`, resumed with
  `action=always`).
- Post-job exposure state unchanged: `service.json` still
  `{"disabled": true}`, still no `opentunnel` dir, listeners still clean.

Caveats carried from item 72's A7: `/api/chat/history` mislabels the
in-flight turn as `Failed` while the engine has it gated/Running — cosmetic,
pre-existing, reproduced here. The tenant is **left running**
(boot-persistent under OpenRC) for maintainer inspection; decommission with
`sudo bash ic/scripts/lunarwing-mt-admin.sh remove-tenant spitfire --purge`.

## 8. Bottom line

The "Public HTTPS Tunnel" is a hosted-relay, blind-TLS, outbound-only remote
access feature with non-interactive enablement (`pair --remote --yes`), a
persistent agent-writable device identity, and URL-is-not-authentication
access control — categorically incompatible with WI#357 if shipped live. At
the v2.0.32 pin it is **compiled out of the worker image** (fail-closed stub,
drift-loud guard, binary-marker backstop), its enabling commands are
**refused by the wrapper and denied by the bridge ruleset**, and its
persisted enablement state is **discarded at every boot**. Against a fully
compromised agent with open egress none of these hold; that boundary is
WI#424 (2.1.7.0). Within that honestly-stated envelope, the v2.0.32 bump
keeps the worker's exposure posture identical to v2.0.24.
