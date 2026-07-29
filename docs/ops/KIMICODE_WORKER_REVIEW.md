# Kimi Code Worker Container Review (kimicode4lunarwing)

**Date:** 2026-07-29
**Reviewer:** Rarity (autonomous agent), branch `rarity/kimi-23-20260729-1201`
**Goal:** AGENT_GOALS_2.0.3.0 item #23 — review the new worker container (kimicode)
**Scope:** `kimicode4lunarwing/` (Dockerfile, entrypoint, bridge/executor TypeScript, config, docs), its `ic/scripts/lunarwing-mt-admin.sh` integration, daemon-side protocol compatibility (`ic/src/orchestrator/external_worker.rs`), CI wiring (`.gitlab/ci/containers.yml`, `.gitlab/scripts/validate_config.py`), and parity with the opencode worker.

## Verdict

**Request changes before this reaches `master` — but the changes already exist.** The initial commit (`a60e133`) cannot build its Docker image at all (verified by reproduction below). Two follow-up commits on `origin/derpy/m24/v2.0.3.0` — `f65f0e1` (tsc build, tests, KIMI_API_KEY) and `12c29b6` (npm version pins) — fix every build/test break; I re-verified the fixed tree compiles clean and its suite passes 18/18. Neither the worker nor the fixes are in `origin/master` yet. **Land `a60e133` + `f65f0e1` + `12c29b6` together.** The remaining findings are non-blocking follow-ups.

Once the fix commits land, this is a well-built worker: the protocol handling, process lifecycle, prompt-safety model, and mt-admin integration are solid and match (in places exceed) the opencode worker's maturity.

---

## P1 — Build is broken as committed on `a60e133` (fixes verified on `derpy/m24/v2.0.3.0`)

Reproduced from a clean copy of `kimicode4lunarwing/scripts/` on this branch.

### 1. `typescript@5.5.0` does not exist on npm → builder stage dies with ETARGET

`Dockerfile:26` runs `npm install --no-save typescript@5.5.0 ...`. npm's 5.5 line only ever shipped prereleases (`5.5.0-beta`, `5.5.0-dev.*`, `5.5.1-rc`); the first stable is `5.5.2`. Verified against the public registry:

```
npm error code ETARGET
npm error notarget No matching version found for typescript@5.5.0.
```

Because this is a `RUN` in the builder stage, **the image build fails every time** — `build-kimi-worker`, `docker compose build`, and the `worker-kimi-image` CI job all break.

**Fix (verified):** `12c29b6` pins `typescript@5.9.3`, `@types/node@22.20.1`, `@types/ws@8.18.1`.

### 2. Sixteen tsc errors even with a valid TypeScript version

With `typescript@5.5.4` substituted, `npx tsc -p tsconfig.json` fails with 16 errors:

- `TS2835` in `lunarwing_bridge.ts` / `kimi_task_executor.ts` — extensionless relative imports (`"./lunarwing_runtime"`) are illegal under `moduleResolution: NodeNext`; must be `"./lunarwing_runtime.js"`.
- `TS5097` in all four `*_test.ts` files — they import with a `.ts` extension, which requires `allowImportingTsExtensions`.
- `TS2304` in `lunarwing_bridge.ts` — `MessageEvent` / `CloseEvent` referenced with `lib: ["ES2022"]` (no DOM); the `ws` types must be used instead.
- `TS2322` in `kimi_task_executor.ts:163` — `{ ...process.env }` is `ProcessEnv` (`string | undefined` values), not `Record<string, string>`.
- `TS7006` implicit-`any` task callbacks in `lunarwing_bridge.ts:165,169`; a `ws.addEventListener("error", ...)` overload mismatch (`TS2769`).

Two config bugs compound this:

- `tsconfig.json` `exclude: ["*.test.ts", ...]` never matches the actual `*_test.ts` naming, so test files are (unintentionally) compiled — which is where the `TS5097` errors come from.
- `package.json` scripts reference `.mjs` outputs (`node --test kimi_result_test.mjs ...`), but `tsc` emits `.js`. `npm test` could never have run.

**Fix (verified):** `f65f0e1` corrects imports, types, tsconfig exclude, and the `.mjs`→`.js` scripts. I rebuilt the fixed tree (`tsc` exit 0, all `.js` emitted) and ran the suite: **18/18 pass** (`kimi_result`, `kimi_task_executor` ×4, `path_expand` ×5, `task_prompt` ×3).

### 3. Real runtime bugs the (previously unrunnable) tests were meant to catch

Also fixed by `f65f0e1`, confirmed by reading the diff:

- `KIMI_BIN` / `KIMI_CODE_HOME` / `WORKSPACE_ROOT` were frozen at module import time (`kimi_task_executor.ts:23-25`), so per-run env overrides — including the executor's own mock-binary tests — silently used the real `kimi` binary. Now read lazily.
- `extractAssistantText` joined *all* assistant messages while `collectOutput` expects the last non-empty one; now consistent.

### 4. `KIMI_API_KEY` documented but not wired (docs said entrypoint writes it; it didn't)

`AGENTS.md`, `HOWTOCHANGECONFIG.md`, and `.env.example` all state `entrypoint.sh` writes `KIMI_API_KEY` into the runtime `config.toml`. As committed, `entrypoint.sh` only handles `KIMI_MODEL` / `KIMI_BASE_URL`; `KIMI_API_KEY` is passed into the container (compose, quadlet) but never used.

Functionally this is **not** an outage, which I verified against the pinned CLI (0.30.0, same version as `KIMI_CODE_VERSION`): the kimi binary natively resolves `apiKey: config.apiKey ?? firstEnv("KIMI_API_KEY")`, and the executor forwards `process.env` to the child. Live probe with a bogus key and the worker's baked `config.toml` (`api_key = ""`):

```
KIMI_API_KEY=sk-bogus kimi -p "..." --output-format stream-json
→ exit 1, stderr: provider.auth_error: 401 The API Key appears to be invalid...
```

So auth works via env fallback, the empty `api_key = ""` does not shadow it, and the executor's error path maps this correctly (nonzero exit → `status: "error"` with the stderr message). `f65f0e1` implements the documented config injection anyway (gating the override branch on all three vars and logging `api_key=set` without the secret), which makes the docs true and keeps the key out of process listings. Good.

---

## Verified good (no action needed)

- **Protocol compatibility, both directions.** The bridge's `handleProtocols` prefers `lunarwing-agent-v1` and echoes `ironclaw-agent-v1` only when offered (`lunarwing_bridge.ts:218-222`); the daemon offers both (`ic/src/orchestrator/external_worker.rs:30,34`). New and legacy peers interoperate. Envelope types match `agent_comm_protocol.json` and the daemon's expectations (`ready`, `task_request`, `task_progress`, `task_result`, `cancel`, `ping`/`pong`).
- **mt-admin integration is thorough and follows the hard rules.** Dedicated ports-registry range (v12 migration: `kimi_wss`/`kimi_health` from 30000–39999, block size 2); `build-kimi-worker` (podman `--network=host` and docker paths, `--kimi-code-version`, toolchains flag); `configure-kimi` with env-file rewrite; quadlet rendering publishes WSS and health on `127.0.0.1` only, sets `UserNS=keep-id:uid=10002,gid=10002` matching the Dockerfile's `USER 10002:10002` (and `start_tenant_kimi` hard-fails on an image built with a different user), passes `AGENT_AUTH_TOKEN` from the tenant's `GATEWAY_AUTH_TOKEN`, and mounts the per-tenant SSH-agent socket. Status/stop/restart/remove all know the `lunarwing-kimi-<tenant>` units; tenant names colliding with the `kimi-` prefix are rejected. No bare `systemctl` against tenant units anywhere in this path.
- **Bridge robustness.** `unhandledRejection` survival, per-message try/catch, duplicate `task_id` requests ignored (resend-safe), per-workspace mutual exclusion with a clear rejection error, cancel-on-socket-close, ping keepalive, bounded knobs (`KIMI_PROGRESS_HEARTBEAT_MS` clamped 0–300000, `KIMI_KILL_GRACE_MS` clamped 100–30000), SIGTERM→SIGKILL with grace and a final `SIGKILL` sweep in `finally`.
- **Prompt-safety model.** Conversation history is wrapped in an explicit UNTRUSTED block with `<>&` JSON escaping, filtered to user/assistant roles, and hard-bounded (50 messages / 10k code points per message / 50k total); `metadata`, `environment`, `user_id`, system prompts, and tool results never enter the prompt. Matches the documented daemon-side `prompt_context` contract.
- **Executor correctness against the real CLI.** Verified on 0.30.0: `-p`, `--output-format stream-json`, and `--yolo` all exist; `KIMI_CODE_NO_AUTO_UPDATE=1` / `KIMI_DISABLE_TELEMETRY=1` are set for determinism; per-task subprocess means no cross-task session leakage.
- **Image hygiene.** Non-root user, `tini` init, pinned CLI version with a `kimi --version` build-time check, slim-by-default with opt-in `WITH_TOOLCHAINS`, Docker `HEALTHCHECK` + compose healthcheck + quadlet `HealthCmd` all probing the same endpoint, health server is dependency-free stdlib Python with no error leakage.
- **CI wiring.** `worker-kimi-image` kaniko job is scoped to `kimicode4lunarwing/**` changes on the default branch plus manual runs; `validate_config.py` enforces the Rust-toolchain pin parity for this Dockerfile like the others.

---

## Residual findings (follow-ups; not fixed by `f65f0e1`/`12c29b6`)

Ordered by severity. None block merging the fix commits; #1 is worth doing before heavy multi-tenant use.

1. **Medium — env-injection race across concurrent tasks** (`kimi_task_executor.ts:105-113`). `request.context.environment` is applied by *mutating the bridge's shared `process.env`* and restoring it in `finally`. Two tasks in different workspaces run concurrently, so interleaved injection/restore can leak one task's variables into another's child or delete a still-needed value. Fix: build the child `env` object (`{ ...process.env, ...request.context.environment }`) and never touch `process.env`. (The protocol doc notes `environment` is never added to prompts; it is, however, currently added to the worker process environment — worth a doc clarification too.)
2. **Low — unbounded in-memory accumulation** (`kimi_task_executor.ts`). `stderrBuffer` and `output` grow for the whole task lifetime (default timeout 1 h). A verbose or wedged CLI run can balloon bridge memory. Cap both (e.g. keep the last N KB of stderr, stop appending `output` past a limit) — progress deltas are already streamed, so truncation loses nothing operationally.
3. **Low — workspace path is not confined to `WORKSPACE_ROOT`** (`workspace_path.ts`). Absolute paths pass through verbatim and relative paths with `..` escape after `resolve()` — `project_dir: "/etc"` or `"../../x"` is accepted as the task cwd. Inherited verbatim from the opencode worker. Real isolation comes from the container (quadlet mounts only the tenant workspace), so blast radius is the container filesystem, but a cheap `resolved.startsWith(WORKSPACE_ROOT + sep)` check would add defense in depth. Decide deliberately and keep both workers consistent.
4. **Low — dev compose publishes WS + health on all interfaces with auth optional** (`docker-compose.yml:47-49`, `AGENT_AUTH_TOKEN` defaults empty). On a LAN, anyone can submit tasks to a `--yolo` agent. mt-admin/quadlet correctly binds `127.0.0.1`; make compose do the same (`"127.0.0.1:${WS_PORT:-9090}:..."`) and/or warn loudly. Same inherited pattern exists in `opencode4lunarwing/docker-compose.yml`.
5. **Low — apt fallback permits insecure repositories** (`Dockerfile:14,48,68`): `apt-get update -o Acquire::AllowInsecureRepositories=true` runs if both the live mirror and snapshot fail. That silently downgrades supply-chain integrity during image builds. Prefer failing the build (or pinning a snapshot date). Inherited from the opencode Dockerfile; fix both together.
6. **Nit — no CI step runs the node test suite.** `containers.yml` only builds the image. The opencode worker runs its tests inside the image build (`bun scripts/*_test.ts`); the kimi builder could run `npm test` after `tsc` the same way, which would have caught finding #2 at commit time.
7. **Nit — small duplications.** `KIMI_VERSION` is exported twice in `entrypoint.sh` (harmless); the worker version string is duplicated in `lunarwing_runtime.ts` and `health_server.py`'s default (drift risk on bump).

---

## Reproduction notes

```bash
# Finding 1 (ETARGET)
cd kimicode4lunarwing/scripts && npm install --no-save typescript@5.5.0
# → npm error code ETARGET / notarget No matching version found for typescript@5.5.0
npm view typescript versions --json | grep '"5\.5\.'   # 5.5.0-beta…, 5.5.1-rc, 5.5.2 — no 5.5.0

# Finding 2 (tsc, with typescript@5.5.4, @types/node@22.0.0, @types/ws@8.5.0)
npx tsc -p tsconfig.json   # → 16 errors (TS2835 ×5, TS5097 ×4, TS2322 ×2, TS7006 ×2, TS2304 ×2, TS2769 ×1)

# Fix-branch verification (tree at 12c29b6, typescript@5.9.3 @types/node@22.20.1 @types/ws@8.18.1)
npx tsc -p tsconfig.json   # → exit 0
npm test                   # → 18 pass, 0 fail

# Finding 4 (CLI env fallback, pinned CLI 0.30.0, worker's baked config.toml)
KIMI_CODE_HOME=/tmp/probe KIMI_API_KEY=sk-bogus kimi -p "..." --output-format stream-json
# → exit 1; stderr: provider.auth_error: 401 ... (env key was used and rejected upstream)
```

## References

- Worker: `kimicode4lunarwing/` (initial commit `a60e133`; fixes `f65f0e1`, `12c29b6` on `origin/derpy/m24/v2.0.3.0`)
- mt-admin: `ic/scripts/lunarwing-mt-admin.sh` (`build_kimi_worker` ~:2240, `configure_kimi` ~:4725, `start_tenant_kimi` ~:4759, quadlet render ~:5740, ports v12 migration ~:1326)
- Daemon protocol: `ic/src/orchestrator/external_worker.rs`, `ic/src/settings.rs:858`
- CI: `.gitlab/ci/containers.yml:83`, `.gitlab/scripts/validate_config.py:18,138`
