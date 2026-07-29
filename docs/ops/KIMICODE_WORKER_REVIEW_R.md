# Kimi Code Worker Container Review R

- Review date: 2026-07-29
- Reviewed at commit: `b150b1b` (worker added in `a60e133`, same day)
- Reviewer: Rarity (autonomous agent, item #23 of `docs/ops/AGENT_GOALS_2.0.3.0.md`)

## Scope

This was a code-first review of the new Kimi Code external worker container and
its integration points:

- `kimicode4lunarwing/` — `Dockerfile`, `entrypoint.sh`, `docker-compose.yml`,
  `health_server.py`, `config/config.toml`, `.env.example`, `README.md`,
  `AGENTS.md`, `HOWTOCHANGECONFIG.md`, and all of `scripts/*.ts`.
- The mt-admin lifecycle integration in `ic/scripts/lunarwing-mt-admin.sh`
  (`build-kimi-worker`, `configure-kimi`, `start_tenant_kimi`/`stop_tenant_kimi`,
  `render_worker_quadlet`, port-registry v12 migration, `add-tenant --with-kimi`).
- The daemon-side external-worker contract (`ic/src/orchestrator/mod.rs`,
  `ic/src/config/sandbox.rs`, `ic/src/tools/builtin/job.rs`) — generic by worker
  name, so no kimi-specific daemon code was required or expected.
- Official Kimi Code CLI documentation (env vars, `kimi` command reference,
  changelog) to validate the CLI invocation and credential flow.

Claims marked **verified** were reproduced locally on this machine
(node v24.14.0, npm 11.9.0, typescript installed per the Dockerfile pins where
possible). Claims sourced from upstream docs cite the page.

## Verdict summary

The architecture is sound and consistent with the sibling workers (nanocode,
pebble, opencode): same `lunarwing-agent-v1` subprotocol, same health/readiness
pattern, same mt-admin lifecycle shape, good per-task isolation via a fresh
`kimi -p` subprocess, sensible prompt-hygiene for untrusted history, and proper
loopback+token wiring in the production (quadlet) path.

However, **the worker cannot be built or run from the committed sources**.
Two independent build blockers (a nonexistent npm pin and a TypeScript
configuration that does not compile) mean the `Dockerfile` builder stage fails.
On top of that, three runtime paths are broken by upstream CLI semantics:
the spawned command line combines flags the CLI rejects (`-p` with `--yolo`),
the per-tenant API key is delivered through an environment variable the CLI
documentedly does not read, and the SIGKILL escalation after SIGTERM can never
fire. None of the test/smoke tooling is runnable as committed. The commit
message says "add Kimi Code external worker", and the mt-admin/quadlet side is
genuinely good work — but the container payload itself appears to have landed
without a single successful `docker build` or `npm test`.

Findings are ordered by severity. Fixing F1–F4 is required before the worker
can dispatch a single task; F5–F7 should follow before any tenant enables it.

## Findings

### High — F1: Build blocker: `typescript@5.5.0` does not exist on npm

References:

- `kimicode4lunarwing/Dockerfile:26`

The builder stage runs
`npm install --no-save typescript@5.5.0 @types/node@22.0.0 @types/ws@8.5.0`.
**Verified** against the npm registry: there is no stable `typescript@5.5.0`
release (only `5.5.0-beta`/`5.5.0-dev.*` prereleases; stable 5.5 starts at
`5.5.2`). `npm install typescript@5.5.0` fails with `E404 No match found for
version 5.5.0`. The sibling pins (`@types/node@22.0.0`, `@types/ws@8.5.0`)
resolve fine.

Fix: pin `typescript@5.5.4` (or newer) — and note that even with a resolvable
version the build still fails for the reasons in F2.

### High — F2: Build blocker: `npx tsc -p tsconfig.json` does not compile the committed sources

References:

- `kimicode4lunarwing/Dockerfile:27`
- `kimicode4lunarwing/scripts/tsconfig.json`
- `kimicode4lunarwing/scripts/lunarwing_bridge.ts:14-27,313,318,325`
- `kimicode4lunarwing/scripts/kimi_task_executor.ts:13-21,111,163`
- `kimicode4lunarwing/scripts/*_test.ts`

**Verified** locally with `typescript@5.5.4`, `@types/node@22.0.0`,
`@types/ws@8.5.0` (the Dockerfile pins, substituting 5.5.4 for the nonexistent
5.5.0): `npx tsc -p tsconfig.json` exits 2 with 17 errors. Root causes:

1. `moduleResolution: "NodeNext"` requires explicit file extensions on relative
   ESM imports, but the sources import extensionless
   (`from "./lunarwing_runtime"` → TS2835 in both `lunarwing_bridge.ts` and
   `kimi_task_executor.ts`).
2. The tsconfig `exclude` lists `*.test.ts` / `*.test.mjs`, but the test files
   are named `*_test.ts` — so the exclude matches nothing and the tests are
   compiled. They import `./kimi_result.ts`-style paths, which needs
   `allowImportingTsExtensions` (TS5097).
3. `lunarwing_bridge.ts` references `MessageEvent`/`CloseEvent` and ws event
   overloads that don't exist with `lib: ["ES2022"]` (no DOM) — TS2304/TS2769.
4. `kimi_task_executor.ts:163` spreads `process.env`
   (`{ [key: string]: string | undefined }`) into `Record<string, string>`
   (TS2322), and line 111 has a `string | undefined` assignment error; the
   bridge also has two implicit-`any` callback parameters (TS7006, strict mode).

Consequence: `docker build` (and therefore
`lunarwing-mt-admin.sh build-kimi-worker`) fails at the `npx tsc` step even
after F1 is fixed. The sibling opencode worker runs its TypeScript under Bun
(no `tsc` step), which is why this class of error doesn't exist there.

Fix options (pick one):

- Mirror the opencode worker: run the bridge with Bun directly from `.ts`
  sources and drop the compile step entirely; or
- Make the Node build honest: use `.js` extensions on relative imports (or
  `module: commonjs` + `moduleResolution: node10`), fix the `exclude` globs to
  `*_test.ts`, add `DOM` to `lib` (or type the ws listeners with the `ws`
  types), and type the env record as `Record<string, string | undefined>`.

### High — F3: `kimi -p … --yolo` is a rejected flag combination; every task spawn fails fast

References:

- `kimicode4lunarwing/scripts/kimi_task_executor.ts:162`
- `kimicode4lunarwing/README.md:82`
- `kimicode4lunarwing/config/config.toml:5`

The executor spawns
`kimi -p <prompt> --output-format stream-json --yolo`. The official `kimi`
command reference states under Flag Conflict Rules:

> `--prompt` cannot be used with `--yolo`, `--auto`, or `--plan` —
> non-interactive mode uses `auto` permission by default.

(https://www.kimi.com/code/docs/en/kimi-code-cli/reference/kimi-command.html)

So with the pinned CLI (0.30.0, the current release as of this review) the
process exits immediately with a flag-conflict usage error on every
`task_request`. The flag is also redundant: print mode already runs under the
`auto` permission policy with no human approval, and the baked
`config.toml` additionally sets `default_permission_mode = "yolo"`. (Verified
against upstream docs, not against the binary — a one-line
`kimi -p x --yolo` smoke run post-F1/F2 would confirm the exact 0.30.0
behavior.)

Fix: drop `--yolo` from the spawn args; document that print mode implies
auto-approval. Keep `default_permission_mode` as-is or align it deliberately.

### High — F4: `KIMI_API_KEY` never reaches the CLI — `configure-kimi --api-key` is a silent no-op

References:

- `kimicode4lunarwing/entrypoint.sh:75-114` (materializes only `KIMI_MODEL` /
  `KIMI_BASE_URL`)
- `kimicode4lunarwing/config/config.toml:11` (`api_key = ""`)
- `kimicode4lunarwing/.env.example:17-18`, `AGENTS.md:26`, `HOWTOCHANGECONFIG.md:38-45`
- `ic/scripts/lunarwing-mt-admin.sh:4745,4861,5869` (writes/passes `KIMI_API_KEY` as env)

The official env-var documentation is explicit:

> Credential variables such as `KIMI_API_KEY` … are **not** read automatically
> from shell environment variables. Running `export KIMI_API_KEY=xxx` … does
> not give any provider its key — they must be written in `config.toml` under
> `[providers.<name>]` or the `[providers.<name>.env]` sub-table.

(https://www.kimi.com/code/docs/en/kimi-code-cli/configuration/env-vars.html)

`entrypoint.sh` never writes `KIMI_API_KEY` into the materialized
`config.toml` (only model/base-url overrides are applied), and the baked
config ships `api_key = ""`. The mt-admin path faithfully threads
`KIMI_API_KEY` from `configure-kimi` into the container environment — where
the CLI ignores it. Net effect: a tenant configured per the documentation
authenticates with an empty key and gets 401s. `HOWTOCHANGECONFIG.md` and
`AGENTS.md` both claim "entrypoint.sh writes it into the runtime config.toml",
which is false as committed.

Fix options:

- In `entrypoint.sh`, when `KIMI_API_KEY` is set, write it into the
  materialized config's `api_key` field of the selected provider (same
  python3 heredoc pattern as the model/base-url overrides); or
- Use the supported `[providers."managed:kimi-code".env]` sub-table; or
- Use the env-native channel the CLI *does* honor: set
  `KIMI_MODEL_NAME`/`KIMI_MODEL_API_KEY` (+ optional `KIMI_MODEL_BASE_URL`),
  which synthesizes a provider from the environment (same docs page).

### Medium — F5: SIGKILL escalation is dead code — a SIGTERM-ignoring `kimi` hangs the task forever

References:

- `kimicode4lunarwing/scripts/kimi_task_executor.ts:148-158,200-212,304-310`
- `kimicode4lunarwing/README.md:37-40`, `.env.example:23-24`

`killProc()` sends SIGTERM, then schedules a SIGKILL after
`KIMI_KILL_GRACE_MS` guarded by `if (proc && !proc.killed)`. Node sets
`ChildProcess.killed = true` as soon as a signal is *successfully delivered*
— not on exit — so after SIGTERM the guard is always false and the SIGKILL is
never sent. The same wrong guard in `finally` (`if (proc && !proc.killed)
proc.kill("SIGKILL")`) is likewise inert.

Consequence: if the child ignores SIGTERM, no `close` event fires, the
executor promise never settles, and `onResult` is never called — the task
hangs worker-side until the daemon's own timeout declares it failed, and the
orphaned process is never reaped (tini only reaps after the bridge exits).
The README and `.env.example` both document a SIGTERM→SIGKILL escalation that
does not happen.

Fix: track actual exit (`proc.exitCode !== null || proc.signalCode !== null`,
or a flag set on `close`/`exit`) instead of `proc.killed` in both places.

### Medium — F6: Test and smoke tooling is unrunnable as committed

References:

- `kimicode4lunarwing/scripts/package.json:9`
- `kimicode4lunarwing/scripts/tsconfig.json:18`
- `kimicode4lunarwing/scripts/kimi_result_test.ts:5-17` vs `kimi_result.ts:6-15`
- `kimicode4lunarwing/docker-compose.yml:58-71`

All verified locally:

1. `npm test` runs `node --test kimi_result_test.mjs …` — but `tsc` emits
   `.js`, never `.mjs`, so the command fails immediately with "Could not find
   'kimi_result_test.mjs, …'".
2. `node --test kimi_result_test.ts` (run directly) **fails**: the test
   "extractAssistantText returns the last non-empty assistant message" asserts
   `"final answer"`, but the implementation joins *all* assistant texts and
   returns `"thinking...\nfinal answer"`. Test and implementation disagree;
   the last-non-empty semantics live in `kimi_task_executor.collectOutput`,
   so the test's expectation matches the *intended* design and the helper is
   the outlier.
3. `task_prompt_test.ts` (3) and `path_expand_test.ts` (5) pass when run
   directly. `kimi_task_executor_test.ts` uses a constructor parameter
   property, which Node's strip-only TS mode rejects — that failure is an
   artifact of running un-transpiled and would disappear under a working tsc
   build, but with F2 there is no working tsc build.
4. `smoke_test.ts` is the one file `tsconfig.json` actually excludes, so
   `/app/scripts/smoke_test.js` is never emitted — yet the `agent-smoke`
   compose profile runs exactly that file. Even with F1/F2 fixed, the smoke
   profile exits `MODULE_NOT_FOUND`.
5. No CI or Dockerfile step runs the tests, which is why all of this shipped
   green.

Fix: align the exclude globs with the `*_test.ts` naming, decide on `.js` vs
running tests via `node --test` against sources, reconcile
`extractAssistantText` with its test (or fix the test to the join semantics
and keep last-non-empty only in `collectOutput`), compile `smoke_test.ts`,
and add a build-time `npm test` (or Dockerfile `RUN`) gate.

### Medium — F7: `project_dir` is not confined to the workspace root

References:

- `kimicode4lunarwing/scripts/workspace_path.ts:1-6`
- `kimicode4lunarwing/scripts/path_expand_test.ts:15-17` (enshrines
  "absolute path preserved")
- `kimicode4lunarwing/scripts/lunarwing_bridge.ts:55-57`,
  `kimi_task_executor.ts:321-323`

`expandWorkspacePath` passes absolute paths through unchanged and joins
relative ones onto `WORKSPACE_ROOT` without normalizing `..` — after
`resolve()`, `../../etc` or `/home/kimi` escape the workspace. Any client of
the worker socket can therefore point a task (running with auto-approved tool
calls, see F3) at any path the container user can write — including
`/home/kimi/.kimi-code` (the CLI config holding any baked credentials),
`/app/results`, and `/var/log/lunarwing`. The same helper exists in the
opencode worker, so this is consistent rather than a regression — but
"consistent" here means the whole worker family treats workspace confinement
as advisory. The workspace single-writer lock (good) serializes per *resolved
path*, so aliased paths (`/workspace`, `/workspace/`, `~/`) are treated as
distinct workspaces and can interleave writes.

Exposure is bounded by the fact that only the authenticated LunarWing daemon
should reach the socket — but defense in depth argues for confining
`project_dir` to `WORKSPACE_ROOT` (reject absolute paths and `..` after
resolution, canonicalize before the ownership check).

### Low — F8: Per-task env injection mutates shared `process.env`

References:

- `kimicode4lunarwing/scripts/kimi_task_executor.ts:105-113,163,311-317`

`context.environment` entries are written into the bridge's own
`process.env` before spawn and restored in `finally`. Between the mutation
and the spawn, any *concurrent* task (different workspace) snapshots
`{...process.env}` and inherits keys it never asked for; restore order under
interleaving is also not guaranteed to converge. The daemon is the only
legitimate source of these values, so this is hygiene rather than a boundary
break, but the fix is cheap: build the child env as
`{ ...process.env, ...request.context.environment }` and never touch
`process.env`.

### Low — F9: Dev-mode compose exposure; no frame size cap; non-constant-time token compare

References:

- `kimicode4lunarwing/docker-compose.yml:20,47-49`
- `kimicode4lunarwing/scripts/lunarwing_bridge.ts:38,210-234`

- `AGENT_AUTH_TOKEN` defaults to empty and compose publishes
  `0.0.0.0:9090`/`8443` — on a shared LAN that's unauthenticated remote task
  dispatch into an auto-approving shell agent. The production quadlet path
  does this right (loopback-only publish, gateway token injected); the dev
  default deserves a loud warning in the README at minimum.
- The `WebSocketServer` sets no `maxPayload` (ws default 100 MiB) — a
  memory-pressure DoS vector from any authenticated peer.
- The bearer check uses `!==` string comparison; timing side channel is
  negligible on loopback/LAN but trivial to make constant-time.

### Low — F10: Documentation drift inside `kimicode4lunarwing/`

References:

- `HOWTOCHANGECONFIG.md:45`, `AGENTS.md:26` — claim entrypoint writes
  `KIMI_API_KEY` into the runtime config (it doesn't; see F4).
- `README.md:37-40`, `.env.example:23-24` — claim SIGTERM→SIGKILL escalation
  (dead code; see F5).
- `README.md:16-22`, `docker-compose.yml:73-82` — "CLI mode (one-shot)" passes
  the prompt as a bare positional argument (`kimi "Write a hello world…"`);
  the upstream CLI documents no positional-prompt form, so this either errors
  or drops into the interactive TUI — which the repo's own `AGENTS.md`
  explicitly forbids in-container. One-shot mode should be
  `kimi -p "$TASK_PROMPT"`.

### Low — F11: Dockerfile falls back to insecure apt repositories

References:

- `kimicode4lunarwing/Dockerfile:10-18,44-49,64-70`

On mirror signature failure the build re-points apt at snapshot.debian.org
and finally retries with `Acquire::AllowInsecureRepositories=true`, which
permits installing unauthenticated packages. It's a deliberate escape hatch
for broken-mirror dev networks, but it should at least be opt-in (build arg)
rather than automatic, and it must never be used for release images.

## What is done well

- **mt-admin integration is thorough and correct**: dedicated port range
  (v12 registry migration, 30000–39999, block 2), reserved tenant-name
  prefixes, quadlet rendering with `UserNS=keep-id:uid=10002,gid=10002`,
  image-user validation before start, loopback-only port publishing,
  `AGENT_AUTH_TOKEN` wired from the tenant gateway token, start/stop/status
  integration, and per-tenant `KIMI_MODEL`/`KIMI_BASE_URL` overrides.
- **Protocol handling**: primary + legacy subprotocol negotiation that echoes
  the offered value, duplicate `task_id` guard, per-socket task abort on
  disconnect, ping keepalive, `unhandledRejection` survival, and a
  single-writer lock per resolved workspace.
- **Prompt hygiene**: conversation history is capped (50 messages / 10k code
  points per message / 50k total), restricted to user/assistant roles,
  JSON-escaped (`<`, `>`, `&`), and wrapped in an explicit untrusted-data
  banner with the task marked authoritative.
- **Stream mapping matches upstream docs**: assistant/tool JSONL on stdout,
  meta/progress/thinking to stderr; `KIMI_CODE_NO_AUTO_UPDATE=1` and
  `KIMI_DISABLE_TELEMETRY=1` are both honored variables (verified against the
  env-var reference).
- **Image hygiene**: multi-stage build, non-root `USER 10002:10002`, `tini`
  as PID 1, baked `HEALTHCHECK`, pinned CLI version via build arg,
  `--omit=dev --ignore-scripts` npm install, optional toolchain layer.
- **Health/readiness**: `/health` vs `/ready` split with a state file written
  by the bridge — matches the pattern the host self-heal pipeline expects.
- **Per-task subprocess model**: fresh `kimi -p` session per task avoids
  cross-task state leakage by construction.

## Recommended fix order

1. **F1 + F2** — make `docker build` succeed at all (pin a real TypeScript
   version; repair or remove the tsc step).
2. **F3** — drop `--yolo` from the spawn args.
3. **F4** — deliver the API key through a channel the CLI reads (write it
   into the materialized `config.toml`, or the `KIMI_MODEL_*` family).
4. **F5** — fix the SIGKILL guard to track process exit, not `proc.killed`.
5. **F6** — repair `npm test`/`tsconfig` excludes/smoke compilation and gate
   the image build on the tests; reconcile `extractAssistantText` with its
   test.
6. **F7–F11** — hardening and doc fixes; can land in any order.

Until 1–4 land, `--with-kimi` should be treated as non-functional for tenant
provisioning.
