# LunarDepth External-Worker Lane

**Status:** proposed design for LunarWing 2.2.0.0, WI#316 part 1.
**Implementation:** WI#316 part 2 only after this design merges and the selected LunarDepth source ref contains the reviewed external-worker compatibility and TensorMoon routing implementations.

## 1. Purpose and scope

Add an opt-in external worker named `lunardepth`, using the DeepSeek Harness fork at `lunarwing/lunardepth`. The lane follows the Kimi and OpenCode2 lanes for immutable provenance, non-root bridge/task separation, tenant lifecycle, exposure control, and protected publication. It consumes LunarDepth's native `lunarwing-agent-v1` implementation rather than adapting ACP stdio.

This item produces no lane code. Part 2 implements the contracts below. This design does not authorize:

- a new public listener, tunnel, relay, pairing service, or third-party session-sharing path;
- a moving `master`, branch, tag, npm, or container-tag dependency;
- `USER 0:0`, a root bridge, or a task process that receives the hub credential;
- direct daemon-to-LunarDepth connections that bypass the lane bridge;
- runtime steering, LunarWing tool proxying, task resume, or per-job environment injection, none of which the reviewed LunarDepth worker supports;
- implicit reuse of a tenant's TensorZero endpoint, model, or credential as though it were a TensorMoon route;
- a change to the daemon's generic external-worker protocol or job registry; or
- changes to the existing opencode, opencode2, kimi, or pebble pins and exposure policies.

The daemon resolves external-worker names generically. `create_job(mode: "lunardepth")` therefore needs configuration and mt-admin wiring, not a new Rust execution backend.

## 2. Reviewed baseline and !2319 remediation

### 2.1 Current source baseline

This revision is based on LunarDepth `master` at **`da06c33b2091546dc38c61b95452ce27a4ce32d5`** (2026-11-09), not the older `4b8b2f72` baseline reviewed in LunarWing !2319. The commit is interface evidence, not a moving source pin. Part 2 must select and review a full commit SHA and re-check the then-current `master` before choosing it.

Two landed LunarDepth goal items are mandatory:

- **Item 4:** Node `^22.19 || >=24` and exactly pnpm `11.7.0`, enforced by repository-root `scripts/check-toolchain.mjs` before dependency installation. pnpm 9 is forbidden because it rewrites `deepseek-harness/pnpm-lock.yaml` into an older format.
- **Item 9:** merge `17aef23e05cc074c8621d38d159d77d25cb348ca` and its !23 remediation provide `@lunardepth/dsh-lunarwing-worker` and the `lunarwing-worker` profile. !20's AL-1 gap is fixed: the token is required by the schema and resolver, and even a directly constructed, misconfigured server rejects a bearer upgrade with HTTP 401 rather than throwing. !23's AO-1 operational note remains binding: a token-less CLI can stay alive with no listener, so process existence is never readiness.

The newer baseline also contains LunarDepth item 5 through merge `8c610180c84a2f4c27fb2958e6e7dab9e48d476b`. Its `tensormoon` provider, `tensormoon::function_name::lunarwing` model route, `baseURLEnv` support, and `allowInsecureHTTP` transport exception are real at this ref. In particular, `deepseek-harness/packages/llm/llm-pi-ai/src/config.ts` validates `allowInsecureHTTP` and rejects non-loopback cleartext HTTP unless that field is explicitly true. This is the secure-routing minimum marker for the lane.

The following worker behavior must be re-verified at every selected ref:

- `dsh --profile lunarwing-worker` loads `@lunardepth/dsh-lunarwing-worker` and its `lunarwing-worker` bundle;
- the worker serves `/health` and `/ready`, defaults to `127.0.0.1:9090`, and accepts only the exact `/ws/agent` upgrade with a required bearer and `lunarwing-agent-v1`;
- it sends `ready`, answers `ping`/`pong`, executes sequential `task_request` envelopes, and returns bounded `task_progress` plus one terminal `task_result`;
- each request creates a fresh Agent and Session, with real-path workspace confinement and bounded user/assistant conversation history;
- cancellation, deadlines, and partial committed output are supported;
- inbound/outbound frames are at most 16 MiB, final output at most 15 MiB, progress chunks at most 64 KiB, and supplied history is bounded to 50 messages, 10,000 characters per message, and 50,000 characters total;
- `task_prompt`, proxied `tool_call`/`tool_list`, environment injection, task resume, and the removed `ironclaw-agent-v1` alias are unsupported; and
- missing local authentication prevents readiness, and bad authentication returns 401 without crashing the process.

The existing LunarWing contracts in `docs/ops/WORKER-CONTAINERS.md` remain authoritative: WI#454 non-root bridge/task pairs, WI#357 no worker self-exposure, WI#424 per-tenant egress filtering, WI#527 pre-stop candidate builds and rollback records, and WI#561 APT/snapshot hardening.

### 2.2 Changes required by the !2319 review

| Review finding | Binding resolution in this revision |
| --- | --- |
| YM-1: task-writable overlay can restore exposure and approval settings | mt-admin renders the effective overlay on the host. A root-owned `0555` directory and root-owned `0444` regular file are mounted read-only into the task. Host, port, model route, permission, approval, and telemetry values are literals; only secret references remain environment-based. Tests prove neither the overlay nor its directory is writable by uid `10005`, and the effective composition cannot be overridden by a later user/profile row. |
| YM-2: TensorMoon and `allowInsecureHTTP` did not exist at the old pin | Re-based to `da06c33b`, where both exist, and added the item-5 merge as a second ancestry gate. The overlay uses a lane-owned `LUNARDEPTH_API_KEY` credential reference, omits `baseURLEnv`, and writes a literal validated `baseURL`. mt-admin and the task launcher independently enforce HTTPS for non-loopback URLs and the explicit `LUNARDEPTH_ALLOW_INSECURE_HTTP=true` exception; upstream validation is defense in depth, not the only enforcement point. |
| YM-3/YM-4: attribution, evidence, paths, and history ownership | This attempt and its trail are Applejack's. Verification names actual commands. Every LunarDepth path below is repository-relative: root `scripts/check-toolchain.mjs`; `deepseek-harness/package.json`, `deepseek-harness/pnpm-lock.yaml`, and `deepseek-harness/scripts/build-exe-for-python-sdk.ts`. The 12-message/2,000-character/12,000-total defaults belong to the daemon's `PromptContextLimits`, not mt-admin. |
| YM-5: retained public fetch conflicts with the egress boundary | v1 disables model-facing web fetch and native/model-facing search. It does not advertise a public-fetch capability that normal tenant egress would reject. A future fetch enablement needs an explicit host allowlist and a separate exposure review. |

## 3. Decisions at a glance

| Contract | Binding choice |
| --- | --- |
| Lane directory and mode | `lunardepth4lunarwing/`, `create_job(mode: "lunardepth")` |
| Image and release tag | `lunarwing-worker-lunardepth:latest`, `$CI_COMMIT_TAG-lunardepth` |
| Runtime identities | task `10005:10005`, bridge `10105:10005` |
| Container names | `lunarwing-lunardepth-<tenant>` and `lunarwing-lunardepth-task-<tenant>` |
| Hub listener | bridge only, container `9090`, published to host loopback as `lunardepth_wss` |
| Health listener | bridge only, container `8443`, published to host loopback as `lunardepth_health` |
| Harness agent API | task container only, `127.0.0.1:19091/ws/agent`, never published |
| Workspace | shared at `/workspace`, with existing rootful inode and rootless keep-id guards |
| IPC | `lunarwing-lunardepth-<tenant>-ipc-v2`, runner Unix socket only; no tool-proxy socket |
| Model configuration | required explicit `LUNARDEPTH_BASE_URL` and `LUNARDEPTH_MODEL`, rendered as one complete TensorMoon-compatible provider/model route |
| Model credential | task-only `LUNARDEPTH_API_KEY`; never the bridge, overlay, argv, logs, or examples |
| Hub credentials | `supports_credentials = false`; per-job environment/credential grants fail instead of being ignored |
| Port registry | v18, two ports per tenant from `52000-52999`, disabled unless selected |
| Initial concurrency | one active harness task per worker pair; additional pairs provide concurrency |

## 4. Source ref and build provenance

### 4.1 Immutable source contract

The Dockerfile and `.gitlab/scripts/build-worker-image.sh` carry the same reviewed values:

- `LUNARDEPTH_REPOSITORY=https://git.lunarwing.org/lunarwing/lunardepth.git`;
- `LUNARDEPTH_REF=<full 40-character commit SHA>`;
- `LUNARDEPTH_COMPATIBILITY_REF=17aef23e05cc074c8621d38d159d77d25cb348ca`; and
- `LUNARDEPTH_MODEL_ROUTING_REF=8c610180c84a2f4c27fb2958e6e7dab9e48d476b`.

A human-readable source version, `0.2.1-alpha.1` at the reviewed baseline, may be recorded for UI and metadata, but it is never a fetch input. If no release tag is selected, use the source package version plus the full SHA rather than inventing a release.

The builder must:

1. fetch the canonical repository without submodules, check out `LUNARDEPTH_REF` detached, and require `git rev-parse HEAD` to equal it exactly;
2. require both reviewed markers to be ancestors of `LUNARDEPTH_REF` with `git merge-base --is-ancestor`, so an old or rebased pin cannot drop item 9 or secure TensorMoon routing silently;
3. require these repository-relative inputs at their reviewed paths:
   - `scripts/check-toolchain.mjs`;
   - `deepseek-harness/package.json` and `deepseek-harness/pnpm-lock.yaml`;
   - `deepseek-harness/scripts/build-exe-for-python-sdk.ts`;
   - `deepseek-harness/packages/api/lunarwing-worker/`;
   - `deepseek-harness/packages/bundle/lunarwing-worker/`; and
   - `deepseek-harness/packages/llm/llm-pi-ai/src/config.ts`;
4. verify `packageManager` is `pnpm@11.7.0`, the Node engine still accepts the image runtime, and the TensorMoon provider plus `allowInsecureHTTP` schema remain present;
5. record the full source SHA, source package version, `deepseek-harness/pnpm-lock.yaml` SHA-256, canonical repository, and both compatibility markers in `worker-inputs.sha256` and image metadata;
6. stamp LunarWing's checkout SHA separately as `org.opencontainers.image.revision`, and the LunarDepth SHA as `org.lunarwing.lunardepth.revision`; and
7. remove `.git`, build caches, test fixtures, deployment staging directories, and package-manager metadata from the final image.

An override is accepted only as another full reviewed SHA on the canonical repository satisfying the same gates. A branch name, `HEAD`, `latest`, short SHA, moving tag, or alternate repository fails closed. A deliberate fork/repository change requires a new provenance review, not just a build-arg edit.

### 4.2 Compatibility is tested, not inferred from a checkbox

The source SHA and ancestry checks are necessary but insufficient. The image builder runs the pinned source's toolchain guard and the focused worker tests and typechecks documented in `docs/ops/lunarwing-external-worker.md`. It also runs lane contract tests against the real profile:

- missing local authentication prevents readiness;
- wrong authentication returns 401 without a process crash;
- the legacy subprotocol is rejected;
- health, readiness, ready/ping/pong, sequential tasks, cancellation, timeout, and terminal statuses retain the daemon's shapes;
- output/frame/history bounds remain at or below daemon limits;
- the complete TensorMoon provider and model row resolves with the lane-owned credential reference;
- remote cleartext HTTP fails without the explicit exception; and
- unsupported environment, tool-proxy, runtime-steering, and resume surfaces are neither advertised nor executed.

The builder fails on an unexpected skip of these worker suites. Historical test counts are diagnostic, not a stable assertion across source bumps.

## 5. Image layout and reproducible build

### 5.1 Lane contents and immutable effective configuration

Part 2 adds the following structure, following `kimicode4lunarwing/` and `opencode2.4lunarwing/`:

```text
lunardepth4lunarwing/
  AGENTS.md
  README.md
  Dockerfile
  docker-compose.yml
  .env.example
  agent_comm_protocol.json
  entrypoint.sh
  entrypoint-split.sh
  entrypoint-task-setup.sh
  health_server.py
  config/
    lunarwing-lane.patch.yml
  scripts/
    package.json
    package-lock.json
    tsconfig.json
    lunarwing_runtime.ts
    lunarwing_bridge.ts
    lunardepth_task_runner.ts
    task_runner_client.ts
    workspace_path.ts
    exposure_policy_assert.py
    image_config_policy.py
    runtime_smoke.py
    runtime_smoke_entrypoint.sh
    dsh-exposure-guard.sh
    *_test.ts
    *_test.py
```

`config/lunarwing-lane.patch.yml` is a root-owned template, not a credentials file. mt-admin validates and renders the **effective** overlay into a host root-owned `0555` directory as a root-owned, regular, non-symlink `0444` file outside `/workspace` and the IPC volume. The task receives that file through a read-only bind mount. It contains no API key or hub/local bearer.

The effective overlay is passed after the shipped `lunarwing-worker` profile and every user/profile patch source. An effective-composition test proves no later profile, `$DSH_HOME` file, environment variable, or CLI composition path can restore disabled exposure or approval settings. Another test runs as uid `10005` and proves neither the mounted overlay nor its directory is writable. mt-admin re-renders on managed configuration changes and recreates the pair; automatic container restarts retain the same immutable file.

The overlay writes literal values for the harness host/port/workspace, provider/model/base URL, insecure-transport decision, sandbox permission mode, approval policy, and disabled telemetry/web/search/HMR/plugin/settings rows. It must not use `!!js process.env` for those values. The sole dynamic bearer row is `authToken: !!js process.env.LUNARDEPTH_LOCAL_AUTH_TOKEN`; `apiKeyEnv: LUNARDEPTH_API_KEY` is a literal credential **name**, not a secret value. `baseURLEnv` is omitted so a task environment cannot replace the validated route.

### 5.2 Build stages

Use a digest-pinned multi-stage build:

1. **Source/build stage** — the same digest-pinned Node `22.23.4-bookworm-slim` runtime family as `CI_NODE_IMAGE`, with packages needed by the pinned native build. Activate exactly pnpm `11.7.0` through a verified, pinned package-manager path; run `node scripts/check-toolchain.mjs` from the LunarDepth repository root before any install; then use `pnpm install --frozen-lockfile` from `deepseek-harness/`. The lockfile must remain byte-identical after installation.
2. **Test and production-closure stage** — build the required native/client artifacts, run focused worker and lane tests, and deploy `dsh-python-runtime-closure` through `deepseek-harness/scripts/build-exe-for-python-sdk.ts --deploy-to=<staging>`. Use `--skip-build` only after the preceding build in the same stage. Do not deploy `@lunardepth/dsh` directly: LunarDepth's deployment study found that it omits required workspace peers.
3. **Runtime stage** — the same pinned Node runtime and Debian snapshot, with only the materialized production closure, root-owned lane scripts/configuration, and task utilities. Launch the built launcher, equivalent to `node .../@lunardepth/dsh/lib/bin.js --profile lunarwing-worker`, not `tsx`, a source checkout, or a development watcher.
4. **Assertion stage/final assertions** — run the real entrypoint smoke in `RUN --network=none`, verify effective identities and modes, verify the internal-only harness bind and authenticated protocol, and assert no unwanted residue before final `USER 10005:10005`.

The final image contains no pnpm store, npm cache, LunarDepth `.git`, build-only compilers, or development dependencies unless a separately reviewed toolchain profile explicitly requires them. `WITH_TOOLCHAINS` cannot change Node or pnpm from their required pins.

### 5.3 Debian, Node, and determinism

`DEBIAN_SNAPSHOT` initially equals the current `CI_DEBIAN_SNAPSHOT`, `20261105T082236Z`. Future refreshes move every worker Dockerfile and CI together through the existing validator. LunarDepth becomes a fifth tied lane; comments and tests that currently say "all four" must be updated rather than leaving it outside the gate. The `CI_NODE_IMAGE` comment and digest checks expand from Kimi-only to Kimi plus LunarDepth.

Preserve WI#561 exactly:

- HTTPS snapshot sources with certificate verification and no HTTP fallback;
- the reviewed CA-bundle bootstrap before APT;
- the old client's update and apt-only install at `Acquire::Retries=0`;
- a Bookworm APT floor of `2.6.2` before other packages;
- fixed-client retries bounded at 3, HTTPS timeouts, strict `--error-on=any` updates, and the WI#559 process timeouts; and
- no insecure mirror, second package-index path, or claim that second-host package-hash verification exists.

Node `22.23.4` satisfies `^22.19 || >=24` and retains the reviewed Brotli fix. pnpm remains exactly `11.7.0`; the build fails before lockfile installation on any other version.

Set `SOURCE_DATE_EPOCH` before account creation and assert the corresponding shadow day in that layer. Normalize generated files and closure mtimes, remove smoke state and caches, and keep build tests under a trap-cleaned temporary HOME. CI retains the whole-image candidate-versus-uncached-clean-rebuild digest comparison. A reproducible application tarball alone is not acceptance.

## 6. Non-root bridge/task design

### 6.1 Why an adapter is required

LunarDepth's shipped server combines the authenticated WebSocket endpoint and Agent execution. Pointing mt-admin directly at that process would either put `AGENT_AUTH_TOKEN` in the task container or expose the harness API beyond loopback. Both violate the lane requirements.

The lane separates the **hub credential** from a **task-local transport credential** and inserts a small protocol adapter:

- the bridge container owns the tenant-scoped hub credential and the only host-published WebSocket listener;
- the task container runs LunarDepth on `127.0.0.1:19091/ws/agent` with a different, random per-boot local credential; and
- a task-side runner translates the existing bounded Unix-socket run protocol to LunarDepth's loopback WebSocket API.

The local credential is named `LUNARDEPTH_LOCAL_AUTH_TOKEN`, not `AGENT_AUTH_TOKEN`. The complete overlay's worker row reads only this local variable; there is no fallback to `AGENT_AUTH_TOKEN`.

The task launcher generates at least 256 bits of randomness without putting it in argv, stores it as a regular non-symlink mode-`0400` file in a task-private state directory outside `/workspace` and the IPC volume, and supplies it only to the harness and task runner. The file is never mounted into the bridge. Same-uid task code can potentially read this local credential, so it is not isolation between jobs; it protects the loopback API from naive unauthenticated use, while the hub credential remains behind the separate-container boundary.

### 6.2 Dispatch sequence

1. The daemon resolves the generic `lunardepth` external-worker entry and connects to host-loopback `lunardepth_wss` with the tenant-scoped bearer and `lunarwing-agent-v1`.
2. The bridge authenticates the daemon, answers hub `ping`, and emits its own `ready` with no optional capabilities. It does not forward upstream capabilities blindly.
3. For each task, the bridge validates the envelope and workspace mapping, rejects unsupported grants, and opens one bounded Unix connection to `task-runner.sock`.
4. The task runner validates the run and workspace again, then connects to `ws://127.0.0.1:19091/ws/agent` using `LUNARDEPTH_LOCAL_AUTH_TOKEN` and the same subprotocol.
5. The runner waits for LunarDepth readiness, forwards only `task_id`, `prompt`, `timeout_ms`, `project_dir`, and supported conversation context, handles local `ping`/`pong`, and translates committed progress and the terminal result to the bridge.
6. The daemon remains the owner of job persistence, approval state, job events, and asynchronous `wait=false` behavior.
7. Hub cancellation travels on the owning bridge/run connection; the runner sends LunarDepth `cancel` for the same task id. The outer runner deadline adds only the existing bounded grace. Terminal `cancelled` and `timed_out` retain committed partial output.
8. A task-runner or harness disconnect fails the active run explicitly. It is never reported as success, and runner/container shutdown cleans up remaining task processes.

The first implementation serializes harness execution per worker pair. A second concurrent request receives a bounded explicit error without interrupting the active request; it is not silently queued. Sequential requests may reuse a healthy local connection. A future concurrency change needs separate workspace/authority-domain review.

### 6.3 Identity, mounts, and IPC

Both roles use the same image with explicit `LUNARWING_CONTAINER_ROLE=split-bridge|split-task`. Wrong, equal, missing, or root identities fail closed. Both run with all capabilities dropped and `no-new-privileges`; there is no runtime `setpriv` switch into a shared identity.

| Resource | Bridge | Task |
| --- | --- | --- |
| uid:gid | `10105:10005` | `10005:10005` |
| Home | `/home/lunardepth-bridge` | `/home/lunardepth` |
| Hub token | private `0400` file, environment removed after staging | absent, including `_FILE` variants |
| LLM credential | absent, including `LUNARDEPTH_API_KEY` and `_FILE` variants | protected task env only |
| Effective overlay | absent | host root-owned `0444`, read-only mount |
| `/workspace` | shared, setgid | shared, setgid |
| IPC volume | runner client only | runner socket owner |
| SSH agent / Study Notes | absent | off in v1; future mounts need separate review |
| Listener | `9090` WS and `8443` health | loopback `19091` only |

The `-ipc-v2` volume preserves the reviewed sticky/setgid ownership model. Only `runner/` is required in v1; no `tool-proxy/` directory or relay is created. The runner socket is bounded and checked for type, ownership, symlink replacement, and stale entries. One Unix connection owns one run and receives accepted/progress/one terminal result.

Reuse the existing workspace-path semantics. The bridge maps daemon paths into `/workspace`; the runner and LunarDepth realpath checks both reject absolute escapes, `..`, and symlink escapes. Overlapping workspace subtrees collide. Rootful starts verify the recorded device/inode before credential staging or task setup; rootless Podman uses `keep-id:uid=10005,gid=10005`.

### 6.4 Health and supervision

Bridge `/health` reports only non-sensitive bridge identity/version/liveness. Bridge `/ready` is true only when all of these hold:

- the bridge hub listener is healthy;
- the task runner answers its bounded JSON pong;
- LunarDepth `/ready` returns HTTP 200 on task loopback; and
- no role is shutting down.

The image healthcheck and mt-admin startup use bounded readiness, not `/health` alone. If the local token or model route is absent, LunarDepth may remain alive without listening; the task supervisor terminates that failed composition after a bounded startup timeout and makes the unit unhealthy. Exit of the harness, runner, or bridge terminates the corresponding supervised role rather than leaving a half-ready pair.

LunarDepth's task `/health` and `/ready` endpoints are unauthenticated and expose worker ID, version, uptime, readiness, and connection count. They stay on task loopback and are never host-published. Its WebSocket upgrade does not authorize by `Origin`; the lane permits no non-loopback/browser-accessible deployment. Any future such deployment requires a new reverse-proxy origin-allowlist design, not just a bind override.

Systemd user Quadlets start task before bridge with the existing `Requires=`/`After=` pattern. OpenRC uses paired health and crash-supervisor units. Neither init system publishes the task API, and neither uses bare system-bus `systemctl` or a second lifecycle implementation outside mt-admin.

## 7. Protocol and capability boundary

| Capability | Lane behavior |
| --- | --- |
| `ready`, `ping`, `pong` | Supported at both authenticated hops; bridge advertises no optional features. |
| `task_request` / progress / result | Supported through the runner adapter with the daemon's 16 MiB transport ceiling. |
| Output bounds | LunarDepth's 15 MiB final-output and 64 KiB progress limits remain enforced; oversized success is an explicit error, not truncation. |
| Conversation history | The daemon's `PromptContextLimits` defaults are at most 12 messages, 2,000 characters each, 12,000 total, below upstream hard limits. mt-admin must not raise them beyond either side's caps. |
| `project_dir` | Mapped into `/workspace` and independently realpath-confined by adapter and upstream. |
| `cancel` / `timeout_ms` | Forwarded on the owning connection with bounded runner cleanup and preserved partial output. |
| `task_prompt` | Not advertised or accepted; daemon rejects steering as unsupported. |
| `tool_call` / `tool_list` | Not advertised, registered, or relayed. `tool_allowlist` remains empty. |
| `context.environment` and `create_job.credentials` | Rejected by the bridge and by daemon `supports_credentials = false`; never silently discarded. |
| Task resume/session reuse | Not supported; each request gets a fresh Agent and Session. |
| Cron | Opt-in only after a dedicated cron review; v1 creates no cron worker entry or tool grant. |

Unknown extension frames never become execution requests. Malformed known frames, binary messages, invalid identifiers, oversized messages, and a second active harness task fail in bounded time.

The existing authority-domain warning still applies: jobs sharing one task container share uid, workspace, harness state, local transport credential, and any configured model credential. Use separate worker pairs for different users or grant domains.

## 8. Model routing, exposure, and egress

### 8.1 Explicit TensorMoon-compatible route

Use LunarDepth's current native route shape: provider `tensormoon`, API `openai-completions`, and a model id of the reviewed form `tensormoon::<function_name>::lunarwing`. The overlay replaces the complete `llm-pi-ai` provider row and the complete `agent-default-model` row; a Cordis patch replaces a row's whole `config`, so partial inheritance is not accepted.

mt-admin owns two required non-secret settings:

- `LUNARDEPTH_BASE_URL`, the complete OpenAI-compatible API root, including a path such as `/openai/v1` but excluding `/chat/completions`; and
- `LUNARDEPTH_MODEL`, the complete reviewed TensorMoon model identifier.

There is **no automatic inheritance** from `LLM_BASE_URL`, `LLM_MODEL`, TensorZero, or another worker. This monorepo does not yet manage TensorMoon as a first-class component, and an OpenAI-compatible transport does not prove that the gateway accepts TensorMoon function/model naming or the same credential. Selecting the lane requires an explicit per-lane/fleet URL and model. An operator may deliberately reuse an existing endpoint only after confirming that compatibility through `configure-lunardepth`.

The effective provider row contains literal `baseURL` and model values, `apiKeyEnv: LUNARDEPTH_API_KEY`, no `baseURLEnv`, and only the selected model. The worker row names the same provider/model pair. `configure-lunardepth --api-key-file` stages the credential through the existing protected file flow. An explicitly unauthenticated gateway may use LunarDepth's documented nonempty placeholder behavior only by a separate operator opt-in. Keys never appear in argv, the overlay, health, logs, export text, or examples.

A placeholder `set-lunardepth-base-url.invalid`, an empty/malformed model, or a model without the reviewed `tensormoon::…::lunarwing` shape fails before readiness. HTTPS is required for non-loopback endpoints in **both** mt-admin's URL validator and the task launcher. A private-network cleartext exception requires exact `LUNARDEPTH_ALLOW_INSECURE_HTTP=true`; only then does the literal provider row contain `allowInsecureHTTP: true`. Otherwise it is false or absent. The exception is never inferred from a private IP, loopback, or another lane's setting. The launcher compares the rendered URL/exception with its validated environment before starting and fails closed on drift.

The final overlay also:

- disables model-generated session titles;
- disables HMR, runtime plugin installation/editing, settings/config editing, and web UI composition;
- disables `web-search-deepseek`, `web-fetch-http`, and model-facing `tool-web` fetch/search in v1;
- disables `session-telemetry-otel` with a literal `disabled: true` and sets `DSH_TELEMETRY_DISABLED=1`; and
- pins `sandbox-policy.mode: workspace-write`, `sandbox-policy.workspaceRoot: /workspace`, and `approval.policy: ask` as literals. `danger-full-access` is not a supported managed value, and operations needing unavailable human approval fail closed.

A source bump must re-audit these complete-row replacements. Configuration that re-enables DeepSeek account traffic, telemetry, sharing, remote plugins, a web listener, public fetch/search, or automatic updates fails the build even if the task protocol still passes.

### 8.2 Exposure controls

WI#357 applies unchanged. The only public exposure path is LunarWing's daemon tunnel. The following are forbidden:

- `LUNARWING_WORKER_HOST=0.0.0.0` or any non-loopback harness bind;
- host publication of task port `19091`;
- `dsh web`, tunnels, relays, pairing, public share services, remote plugin installation, and self-update in the worker;
- a third-party telemetry exporter, DeepSeek native-search request, or unreviewed public-fetch request; and
- any instruction path other than daemon job dispatch → bridge → runner → task-loopback harness API.

A root-owned `dsh-exposure-guard.sh` may refuse verified naive-use exposure commands and non-loopback flags. Like the OpenCode wrapper, it is not a security boundary: direct binary/Node invocation can bypass a wrapper. The real boundaries are the unpublished loopback-only harness, host-owned read-only effective overlay, read-only application/configuration trees, no host task-port mapping, host listener audits, and the per-tenant egress firewall.

### 8.3 Per-tenant egress allowlist

Both roles join `lw-egress-<tenant>` and use the existing namespace keeper, static host mappings, disabled recursive DNS, and fail-closed nftables verification. The bridge needs no Internet route. The task role may reach only:

- the resolved addresses and port for `LUNARDEPTH_BASE_URL`; and
- explicitly operator-listed hosts required for the tenant's jobs, such as approved Git hosts.

`worker_egress_target_records` gains LunarDepth parsing through the shared URL validator. The selected lane fails with the existing lane-local status 4 when its backend is missing, malformed, or unresolvable; the target is never silently omitted. `worker-egress <tenant>` displays the derived LunarDepth target, and host health repeats the verification.

No npm/pnpm registry, DeepSeek service, telemetry collector, models catalog, arbitrary MCP endpoint, public web destination, or update service is allowed at runtime by default. Build-time dependency access does not relax the runtime filter.

## 9. mt-admin integration

### 9.1 Registry and generated configuration

Ports registry v18 adds, without renumbering any existing tenant:

```text
lunardepth_range = 52000-52999
lunardepth_wss   = lunardepth_base + 0
lunardepth_health = lunardepth_base + 1
workers.lunardepth = false unless --with-lunardepth
```

Add corresponding keys to allocation, status, import/export, migration, health, and cleanup paths. Existing tenants migrate in place and remain unselected.

`ensure_external_worker_config` writes the standard entry with the tenant-scoped token, `timeout_ms` from the shared external-worker default (initially 3,600,000 ms), bounded prompt context, and:

```toml
supports_credentials = false
```

The endpoint is `ws://127.0.0.1:<lunardepth_wss>/ws/agent`. No daemon Rust change is expected.

### 9.2 Selection and model configuration

Add the conventional surface:

```sh
mt-admin add-tenant <tenant> --with-lunardepth \
  --lunardepth-base-url <url> --lunardepth-model <model>
mt-admin configure-lunardepth <tenant> \
  [--base-url <url>] [--model <model>] [--api-key-file <path>]
mt-admin build-lunardepth-worker [--no-cache] [--with-toolchains]
mt-admin build-tenant <tenant> --with-lunardepth
mt-admin start-worker <tenant> lunardepth
```

Enabling the lane without an already staged explicit/fleet URL and model fails before tenant stop or image build. `configure-lunardepth` preserves omitted values and never prints key bytes. Explicit operator values survive re-render and export; there are no inherited endpoint markers because this lane deliberately has no implicit TensorZero inheritance. The insecure-HTTP exception and unauthenticated-gateway placeholder require distinct explicit options and survive re-render only when already operator-set.

### 9.3 Build, start, upgrade, and rollback

`--with-lunardepth` participates in `build-all`, pre-stop `build-worker-candidates --enabled`, `promote-worker-images`, rollback records, import/export, and `upgrade-tenant`. Standalone and candidate builds use the same clean-source, `git archive`, provenance, and immutable-image rules as existing lanes. A failed pre-stop candidate leaves the running tenant untouched.

The inline `RUN --network=none` smoke requires Podman 4.5 or newer, or Docker with BuildKit/buildx. Generalize the existing builder probe rather than duplicating it, and run LunarDepth preflight before `upgrade-tenant` or `stop-tenant-for-upgrade` stops anything. Older hosts may keep an already-built image but cannot rebuild it.

Start/restart follows the existing selected-worker order with LunarDepth appended after opencode2. It preserves lane-local status 3/4 behavior and does not hide failures in other selected lanes. Image ID and split launch-contract changes recreate both roles. `worker-listeners`, `worker-egress`, and `worker-smoke` cover both containers.

Upgrade requires matching lane scripts, image, mt-admin modules, unit definitions, and IPC contract. Rollback restores the recorded prior image ID and matching configuration; it does not rename or adopt an older writable IPC volume. Preserve the workspace and never print the rollback token or LLM key.

## 10. CI, scans, and promotion

### 10.1 MR-time gates

Extend existing validation rather than creating a parallel policy framework:

- `.gitlab/scripts/validate_config.py` cross-checks the Dockerfile/CI source SHA, repository, both compatibility markers, Node/pnpm pins, snapshot tie, lane input inventory, and generated job names;
- `worker-exposure-policy-guard` checks the root-owned configuration template, literal exposure/approval/telemetry settings, disabled fetch/search/sharing/update surfaces, loopback-only harness, split identities, no reserved task token, and no tool-proxy/capability advertisement;
- `worker-buildkit-parser-policy-guard` gains a declarative LunarDepth policy using BuildKit's parser, including final `USER 10005:10005`, root-owned read-only boot/configuration trees, exact network-isolated smoke placement, and no post-assert mutation; and
- normal CI configuration, Python, shell, Go, and mt-admin harnesses cover the new files and both init systems.

Do not weaken any OpenCode2 assertion while generalizing shared code. Parser or verifier behavior specific to OpenCode2 remains in its own policy.

### 10.2 Protected candidate, verification, scan, and promotion

Add the standard protected jobs:

| Job | Required behavior |
| --- | --- |
| `worker-lunardepth-candidate` | Build and push an immutable candidate, run the real split-role runtime smoke, export `lunardepth-rootfs.tar`, build again with no cache, and require equal whole-image digests. Publish the tar as a candidate artifact explicitly. |
| `worker-lunardepth-exposure-verify` | With checkout-owned tooling, validate tar ownership/modes and complete image config and compare checkout-owned files out of band. This job verifies the candidate; it does not claim to rerun its runtime smoke. |
| `worker-lunardepth-scan` | Scan the exact candidate digest with the existing CRITICAL gate and produce the CycloneDX SBOM. |
| `worker-lunardepth-promote` | Manually publish `$CI_COMMIT_TAG-lunardepth` from the verified digest; never rebuild and never publish `:latest`. |

Promotion hard-needs candidate, exposure verification, scan, `worker-exposure-policy-guard`, and the BuildKit parser guard. Use `worker-lunardepth-promotion` as the resource group. No new Trivy suppression is assumed; any finding requires remediation or a separately reviewed, dated exception under the existing policy.

The existing protected-ref rules stay intact. An MR pipeline proves source/configuration policy but receives no publication credentials and does not substitute for protected candidate/scan/exposure gates. Publication evidence names the exact source and image digests.

## 11. Test and acceptance plan

### 11.1 Unit and source-contract tests

- Bridge envelope validation, bearer handling, ready/ping/pong, sequential pooling, malformed/binary/oversized frames, and exactly one terminal result.
- Runner delegation, accepted-run bounds, disconnects, second-task refusal, cancellation, timeout, partial output, and process cleanup.
- Translation between daemon and LunarDepth field names without forwarding environment, metadata, cron, steering, or tool-proxy fields.
- Workspace mapping, overlap detection, symlink/absolute escapes, and rootful inode drift.
- Required token stripping, distinct local-token generation/mode, no token in argv/logs, and missing-local-token readiness failure.
- Host overlay ownership/mode/read-only mount, no uid-`10005` write access to file or directory, literal security values, and effective composition after every profile/user layer.
- Complete TensorMoon provider/model resolution with `LUNARDEPTH_API_KEY`, no `baseURLEnv`, HTTPS enforcement, and the exact insecure-HTTP exception in both mt-admin and the task launcher.
- LunarDepth's focused API/bundle suites and typechecks against the exact pinned ref, plus the runtime-closure verifier.

### 11.2 Image and exposure tests

- Real production launcher starts without a source checkout; `/health`, `/ready`, authenticated upgrade, wrong-token 401, legacy-subprotocol rejection, and a deterministic fake-model task pass.
- Missing local token produces no listener and a supervised unhealthy exit; missing model URL/model likewise fails before readiness.
- Only bridge WS/health are non-loopback; task `19091` is loopback and unpublished. TCP and UDP are audited from the host.
- Runtime files and application dependencies are root-owned and not group/other-writable; no setuid/setgid, source metadata, package caches, development HOME residue, or smoke timestamps remain.
- Wrapper refusal tests cover every verified exposure spelling, while direct-binary tests document that the wrapper is only naive-use protection.
- Candidate and clean rebuild are byte-identical; the out-of-band tar/config verifier rejects identity, mode, listener, environment, and healthcheck drift.

### 11.3 mt-admin and CI harnesses

Add `test-mt-admin-lunardepth.sh` and extend the shared worker image-contract, startup, build-input, candidate/promotion/rollback, import/export, LLM entrypoint, port migration, listener, egress, and smoke harnesses. Cover:

- registry v17→v18 with unchanged existing ports and opt-in allocation;
- systemd user Quadlet and OpenRC unit shapes, task-before-bridge readiness, stop/quiescence, and missing/failed unit handling;
- rootless and rootful launch arguments, workspace ownership, image-ID recreation, and split-role env filtering;
- required explicit model/URL values, no TensorZero inheritance, API-key-file handling, HTTPS and private-network exception validation, unresolved backend status 4, and preserved operator overrides;
- generic daemon configuration with `supports_credentials = false` and zero Rust changes; and
- candidate build before tenant stop, promotion retry, rollback image IDs, and no cross-lane candidate tagging.

Run the repository's related CI validator, worker reproducibility/build-args/exposure suites, BuildKit Go fixtures, `check-mt-admin.sh`, shell syntax/ShellCheck, and Python checks on the implementation. No Rust build is needed for this design-only item.

### 11.4 Gated live acceptance for part 2

On a disposable tenant, with no production tenant mutation:

1. Build through mt-admin, start the pair, and verify systemd user-manager behavior on a systemd host and rendered/managed units on an OpenRC host.
2. Run `worker-listeners`, `worker-egress`, `worker-smoke`, and `status`; require only expected host-loopback publications and a proven live filter.
3. Dispatch `create_job(mode: "lunardepth", wait: false)` through the real daemon to a deterministic model, then make one real tenant-approved TensorMoon inference. Verify job id, progress, final output, workspace write, fresh session behavior, and job attribution.
4. Exercise cancellation and timeout, unsupported steering, credentials/environment rejection, a second concurrent task, workspace escape, and harness death. Each must have an explicit bounded outcome.
5. Attempt task-loopback and non-loopback exposure, direct wrapper bypass, arbitrary egress, telemetry/search/update access, and task-port publication. The network/audits must deny or flag them.
6. Upgrade and roll back through mt-admin, proving pre-stop candidates and restored image IDs without workspace or IPC adoption mistakes.

A local source test or an upstream keyless profile launch is not live LunarWing dispatch evidence. Any unavailable init system, real model, or container-runtime lane must be reported as an honest acceptance gap rather than claimed as verified.

## 12. Implementation map and completion contract

Part 2 is complete only when all of the following are true:

1. `lunardepth4lunarwing/` implements this design at a reviewed full LunarDepth SHA containing both required markers, with Node/pnpm and production closure verified.
2. The non-root bridge/task pair passes real protocol, cancellation, workspace, credential-isolation, immutable-overlay, health-supervision, and exposure tests.
3. mt-admin supports selection, explicit TensorMoon-compatible configuration, build/start/restart/upgrade/rollback, both init systems, v18 ports, listener/egress audits, and candidate promotion without daemon changes.
4. The generic exposure guard and LunarDepth candidate, out-of-band verification, scan, and protected publication jobs pass on the exact implementation head; the MR pipeline is green.
5. `docs/ops/WORKER-CONTAINERS.md` gains a LunarDepth section covering pin/ref provenance, dispatch, identities, model/URL configuration, unsupported capabilities, exposure/egress controls, rollout/rollback, and the version-bump exposure checklist.
6. `lunardepth4lunarwing/AGENTS.md`, `.gitlab/deliverables.json`, build/input inventories, and operator examples agree with the implementation and contain no real credentials.

If the selected LunarDepth ref changes any reviewed API, profile row, toolchain floor, runtime-closure path, TensorMoon transport behavior, telemetry/exposure surface, or protocol bound, part 2 must stop and reconcile this design rather than silently relaxing a gate.
