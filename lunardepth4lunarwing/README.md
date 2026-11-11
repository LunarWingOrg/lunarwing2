# LunarDepth External Worker

`lunardepth4lunarwing` runs the LunarDepth fork of DeepSeek Harness as an opt-in LunarWing external worker. The daemon uses `create_job(mode: "lunardepth")`; no daemon-specific Rust backend is added.

The lane pins `https://git.lunarwing.org/lunarwing/lunardepth.git` at `da06c33b2091546dc38c61b95452ce27a4ce32d5` (source package `0.2.1-alpha.1`). The compatibility and TensorMoon routing ancestry gates are `17aef23e05cc074c8621d38d159d77d25cb348ca` and `8c610180c84a2f4c27fb2958e6e7dab9e48d476b`. Branches, short SHAs, moving tags, and alternate repositories are not accepted build inputs.

## Dispatch and identities

The task container runs as `10005:10005`; the bridge runs as `10105:10005`. Both use the same image with all capabilities dropped and `no-new-privileges`.

```text
daemon → host-loopback bridge :9090 → runner Unix socket → task-loopback harness :19091
```

Only the bridge publishes ports, and mt-admin binds both `9090` and health `8443` to host loopback. The harness accepts only `127.0.0.1:19091/ws/agent` with its random per-boot local bearer and `lunarwing-agent-v1`. Its unauthenticated health endpoints and protocol `version: "1"` never leave the task container. The bridge reports `lunardepth-worker-1.0.0` separately.

The bridge holds only the tenant hub credential. The task holds only the local transport credential and model key. Each pair runs one active task; a second request receives an explicit error. Jobs sharing a pair share uid, workspace, harness session history/logs, and the local credential. Use separate pairs for different confidentiality or grant domains.

## Required model configuration

The lane does not inherit TensorZero, another worker, `LLM_BASE_URL`, or `LLM_MODEL`. Configure an explicit TensorMoon-compatible API root and model:

```sh
ic/scripts/lunarwing-mt-admin.sh configure-lunardepth <tenant> \
  --base-url https://tensormoon.example.com/openai/v1 \
  --model tensormoon::function_name::lunarwing \
  --api-key-file /protected/tenant-lunardepth-key
ic/scripts/lunarwing-mt-admin.sh build-tenant <tenant> --with-lunardepth
ic/scripts/lunarwing-mt-admin.sh start-worker <tenant> lunardepth
```

The URL excludes `/chat/completions`. HTTPS is required beyond loopback. A private-network cleartext exception requires the distinct `--allow-insecure-http` option. A deliberately unauthenticated gateway requires the separate `--allow-unauthenticated-gateway` option and uses only LunarDepth's nonempty placeholder behavior. Clear those exceptions with `--disallow-insecure-http` and `--require-api-key`. Existing tenants receive their v18 lane ports, registry selection, and daemon endpoint during configuration; route and key values are written literally without Podman-env quotes.

The model key is **task-visible, including to the model**. Issue a different, independently revocable key per tenant and restrict it to that tenant's inference. It never belongs in the bridge, overlay, argv, logs, health output, examples, or export text.

## Configuration integrity

Three v1 choices are intentional:

1. **Unsandboxed readers and programmatic execution are disabled.** `tool-fs-search` is off because its ripgrep runs outside the sandbox and could read the model key through `/proc/<pid>/environ`. `ptc-runtime` and `workflow-ptc` are off as a second executor surface. The smoke checks confined bash against process environments instead.
2. **All inherited temp variables are `/tmp`.** `TMPDIR`, `TMP`, and `TEMP` are pinned in the frozen launch environment. This lets `mktemp` work under both built-in runners; a task-private temp directory outside the runner's write grants would break bash tools.
3. **The profile template is generated at image build by the pinned source.** `profile_template.ts` calls `initProfile` with the shipped `lunarwing-worker` template from `@lunardepth/dsh-app-boot`. There is no hand-written profile manifest in this lane. The current pin exports no `PROFILE_SCHEMA_VERSION`, so the recorded schema is `unversioned`.

At every start, task setup removes everything under `$DSH_HOME` except the real, task-owned `sessions`, `storages`, and `attachments` directories. It recreates the complete profile, home patch, and home `.env` from root-owned templates. The harness `HOME` is a separate root-owned empty directory. `WS_NO_BUFFER_UTIL=1`, `WS_NO_UTF_8_VALIDATE=1`, and a system-only `PATH` block home-directory module planting. The launch cwd is `/run/lunardepth-launch`, an empty root-owned directory outside the workspace and temp roots. Setup is sourced into the launcher so these values and the local/model credentials persist into the harness and runner.

The host-rendered overlay remains root-owned and read-only. Because rootless `keep-id` does not show host UID `0` as container UID `0`, the task launch contract carries the overlay's SHA-256; setup requires that exact digest and mode `0444` instead of making a rootless-owner assumption. Study Notes and SSH-agent mounts are off in v1.

Before readiness, the lane:

- compares the composed rows against the exact allowlist and every final overlay value without evaluating `!!js`;
- resolves every row and both LunarDepth worker packages from the root-owned deployed closure;
- rejects loader diagnostics, unknown packages, skipped bundles, missing rows, exposure changes, and unconfined fallbacks;
- requires `package.json` to remain byte-identical before the dump, after the dump, and after real readiness;
- requires the manifest schema to equal `PROFILE_SCHEMA_VERSION` whenever a future pin exports it, and records that version in `worker-inputs.sha256` and image metadata;
- runs a real built-in bwrap/Landlock bash probe requiring `full` enforcement, successful workspace and `/tmp` writes, a denied escape, and no model-key disclosure through process environments; and
- repeats composition and readiness checks on the real boot and keeps composite health dependent on fresh harness/runner heartbeats.

A pin bump must regenerate the template and repeat these checks. The profile template and `LUNARDEPTH_REF` move together.

The builder installs `musl-tools` and runs the full native build so the deployed closure contains `landlock-run`. Container `/proc` restrictions make bwrap unusable here; Landlock needs no added capabilities. The host kernel must probe `full`, not `partial`, for every ABI the pinned launcher uses.

## Permission and network limits

The complete permission row contains only `workspace-write`/`ask` with literal `defaultPreset: workspace-write`. `danger-full-access`, `auto`, `dsh-bash-local`, `runnerCommand`, and `runnerFailureSignatures` are forbidden. Headless approval requests resolve unavailable and never execute an escalation. Runtime smoke asserts the fresh session's actual `permission/preset`, `sandbox/mode`, and `approval/policy` events.

LunarDepth's sandbox confines file effects only. It does not provide a network boundary; the per-tenant `lw-egress-<tenant>` nftables allowlist is the only network boundary. Runtime defaults allow the configured model endpoint and explicitly approved job hosts, not npm, DeepSeek account services, telemetry, public fetch/search, model catalogs, or update services.

No tunnel, relay, pairing, public sharing, web UI, remote plugin installation, settings/config editing, HMR, self-update, task resume, steering, LunarWing tool proxy, per-job environment, credential grant, or cron entry is supported. `/usr/local/bin/dsh` refuses verified naive-use exposure commands; direct Node invocation can bypass that wrapper and is not treated as a security boundary.

## Build and verification

Use Node `22.23.4` and exactly pnpm `11.7.0`. The Debian snapshot is tied to the other workers. The build checks the source SHA and both ancestry markers, runs the source toolchain guard before installation, preserves the pnpm lock byte-for-byte, runs focused worker tests/typechecks, and materializes `dsh-python-runtime-closure` through `build-exe-for-python-sdk.ts --deploy-to`.

```sh
ic/scripts/lunarwing-mt-admin.sh build-lunardepth-worker --no-cache
ic/scripts/lunarwing-mt-admin.sh worker-listeners <tenant>
ic/scripts/lunarwing-mt-admin.sh worker-egress <tenant>
ic/scripts/lunarwing-mt-admin.sh worker-smoke <tenant> lunardepth
```

Podman 4.5+ or Docker BuildKit/buildx is required for the inline `RUN --network=none` split-role smoke. Protected candidate builds additionally require an uncached clean rebuild with the same whole-image digest, out-of-band rootfs/config verification, a CRITICAL vulnerability gate, and manual promotion of the verified digest. Local tests do not replace those protected jobs or live TensorMoon/OpenRC acceptance.

For development checks:

```sh
npm --prefix scripts ci --ignore-scripts
npm --prefix scripts run build
npm --prefix scripts test
python3 -m unittest discover -s scripts -p '*_test.py'
python3 health_server_test.py
python3 scripts/exposure_policy_assert.py --source .
```

The task-visible model key, shared session-history authority, lack of network confinement inside LunarDepth's file sandbox, and single-task serialization are operational limits, not hidden isolation guarantees.
