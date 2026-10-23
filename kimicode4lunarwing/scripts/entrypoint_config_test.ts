import assert from "node:assert/strict"
import { spawn, spawnSync } from "node:child_process"
import { chmodSync, copyFileSync, existsSync, lchownSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, readlinkSync, rmSync, statSync, writeFileSync } from "node:fs"
import { connect } from "node:net"
import { tmpdir } from "node:os"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"
import { test } from "node:test"

const entrypointPath = fileURLToPath(new URL("../entrypoint.sh", import.meta.url))
const entrypoint = readFileSync(entrypointPath, "utf8")
// WI#400: the entrypoint split into a root launcher (entrypoint.sh) and the
// task-side setup (entrypoint-task-setup.sh, run as the task uid). The
// config-materialization section lives in the setup script and runs whole.
const setupPath = fileURLToPath(new URL("../entrypoint-task-setup.sh", import.meta.url))
const setupScript = readFileSync(setupPath, "utf8")
const configSectionEnd = setupScript.length

// WI#400: the setup script refuses to run as root (it must run as the task
// uid in the image), but CI and the in-image npm test gate run as root.
// Mirror the launcher's `as_task` by running the setup as an unprivileged
// uid (nobody), handing the scratch tree over first so it can write.
const SETUP_RUN_ID = 65534
function handTree(path: string): void {
  lchownSync(path, SETUP_RUN_ID, SETUP_RUN_ID)
  if (!lstatSync(path).isDirectory()) return
  for (const name of readdirSync(path)) handTree(join(path, name))
}
function runSetup(options: { encoding: "utf8"; env: NodeJS.ProcessEnv }) {
  const asRoot = process.getuid?.() === 0
  if (asRoot && options.env.KIMI_CODE_HOME) handTree(dirname(options.env.KIMI_CODE_HOME))
  return spawnSync("bash", ["-c", setupScript.slice(0, configSectionEnd)], {
    ...options,
    ...(asRoot ? { uid: SETUP_RUN_ID, gid: SETUP_RUN_ID } : {}),
  })
}

test("baked Kimi defaults use the placeholder provider endpoint (WI#330)", () => {
  const config = readFileSync(fileURLToPath(new URL("../config/config.toml", import.meta.url)), "utf8")
  assert.match(config, /^default_model = "tensorzero\/FrontierCODEK"$/m)
  // The baked base_url is a guaranteed-NXDOMAIN placeholder that
  // entrypoint.sh refuses to run; mt-admin renders KIMI_BASE_URL per tenant.
  assert.match(config, /^base_url = "http:\/\/set-kimi-base-url\.invalid\/openai\/v1"$/m)
  assert.match(config, /^\[models\."tensorzero\/FrontierCODEK"\]$/m)
  assert.match(config, /^model = "tensorzero::function_name::FrontierCODEK"$/m)
  assert.match(config, /^dangerous_command_guard = true$/m)
  assert.match(config, /^command = "node \/app\/hooks\/headless_command_guard\.mjs"$/m)
})

test("entrypoint writes escaped, validated config atomically with mode 0600", () => {
  assert.ok(configSectionEnd > 0)
  const root = mkdtempSync(join(tmpdir(), "kimi-config-"))
  const home = join(root, "home")
  const source = join(root, "source.toml")
  writeFileSync(source, `default_model = "old"

[[providers]]
base_url = "https://old.invalid"
api_key = ""
`)

  const result = runSetup({
    encoding: "utf8",
    env: {
      ...process.env,
      KIMI_CODE_HOME: home,
      KIMI_CONFIG_SOURCE: source,
      KIMI_MODEL: "model\"\\name\nnext",
      KIMI_BASE_URL: "https://example.invalid/a\\b\nnext",
      KIMI_API_KEY: "key\"\\line\n\u0001",
    },
  })
  assert.equal(result.status, 0, result.stderr)
  const target = join(home, "config.toml")
  const config = readFileSync(target, "utf8")
  assert.match(config, /default_model = "model\\"\\\\name\\nnext"/)
  assert.match(config, /api_key = "key\\"\\\\line\\n\\u0001"/)
  assert.equal(statSync(target).mode & 0o777, 0o600)

  writeFileSync(source, "invalid = [toml")
  const invalidResult = runSetup({
    encoding: "utf8",
    env: { ...process.env, KIMI_CODE_HOME: home, KIMI_CONFIG_SOURCE: source },
  })
  assert.notEqual(invalidResult.status, 0)
  assert.equal(readFileSync(target, "utf8"), config)
})

test("entrypoint accepts a missing Kimi API key", () => {
  assert.ok(configSectionEnd > 0)
  const root = mkdtempSync(join(tmpdir(), "kimi-config-no-api-key-"))
  const home = join(root, "home")
  const source = join(root, "source.toml")
  writeFileSync(source, 'api_key = ""\n')
  const env: NodeJS.ProcessEnv = {
    ...process.env,
    KIMI_CODE_HOME: home,
    KIMI_CONFIG_SOURCE: source,
  }
  delete env.KIMI_API_KEY
  delete env.TENSORZERO_API_KEY

  const result = runSetup({
    encoding: "utf8",
    env,
  })

  assert.equal(result.status, 0, result.stderr)
  // The fail-closed policy (WI#206) is injected into any source config.
  const generated = readFileSync(join(home, "config.toml"), "utf8")
  assert.match(generated, /dangerous_command_guard = true/)
  assert.match(generated, /headless_command_guard\.mjs/)
})

test("entrypoint retargets the baked model entry when KIMI_MODEL is a new model id", () => {
  assert.ok(configSectionEnd > 0)
  const root = mkdtempSync(join(tmpdir(), "kimi-config-retarget-"))
  const home = join(root, "home")
  const source = join(root, "source.toml")
  writeFileSync(source, `default_model = "tensorzero/FrontierCODE"

[providers.tensorzero]
type = "openai"
base_url = "http://tensorzero.example.test:3000/openai/v1"
api_key = ""

[models."tensorzero/FrontierCODE"]
provider = "tensorzero"
model = "tensorzero::function_name::FrontierCODE"
max_context_size = 859000
max_output_size = 131072
capabilities = ["tool_use"]
`)

  const result = runSetup({
    encoding: "utf8",
    env: {
      ...process.env,
      KIMI_CODE_HOME: home,
      KIMI_CONFIG_SOURCE: source,
      KIMI_MODEL: "tensorzero::function_name::baud",
    },
  })
  assert.equal(result.status, 0, result.stderr)
  const config = readFileSync(join(home, "config.toml"), "utf8")
  assert.match(config, /default_model = "tensorzero::function_name::baud"/)
  assert.match(config, /\[models\."tensorzero::function_name::baud"\]/)
  assert.match(config, /model = "tensorzero::function_name::baud"/)
  assert.match(config, /provider = "tensorzero"/)
  // provider/base_url lines untouched, exactly one model entry renamed
  assert.match(config, /base_url = "http:\/\/tensorzero\.example\.test:3000\/openai\/v1"/)
  assert.equal(config.match(/\[models\./g)?.length, 1)
  // WI#412: the retarget renames only the table header and model id, so the
  // output cap pinned on the baked entry survives a per-tenant override.
  assert.match(config, /^max_output_size = 131072$/m)
})

test("shipped config pins max_output_size for every model under an openai-type provider (WI#412)", () => {
  // kimi-code 2.2.0 (upstream #4091) sends no max_tokens for a model without
  // max_output_size unless the provider type is "kimi"; the lane's provider is
  // "openai", so a missing cap means each downstream provider's default — the
  // Engine V2 4096-token failure class for a thinking model. Parse the real
  // TOML (python3 is provisioned in the lane) rather than regex-matching it.
  const configPath = fileURLToPath(new URL("../config/config.toml", import.meta.url))
  const probe = `
import json, sys, tomllib
with open(sys.argv[1], "rb") as fh:
    config = tomllib.load(fh)
providers = config.get("providers", {})
out = {}
for alias, table in config.get("models", {}).items():
    provider = providers.get(table.get("provider", ""), {})
    out[alias] = {"type": provider.get("type"), "max_output_size": table.get("max_output_size")}
print(json.dumps(out))
`
  const result = spawnSync("python3", ["-I", "-c", probe, configPath], { encoding: "utf8" })
  assert.equal(result.status, 0, result.stderr)
  const models: Record<string, { type: string | null; max_output_size: unknown }> = JSON.parse(result.stdout)
  assert.ok(Object.keys(models).length > 0, "no [models] tables parsed")
  for (const [alias, info] of Object.entries(models)) {
    if (info.type !== "openai") continue
    assert.ok(
      Number.isInteger(info.max_output_size) && (info.max_output_size as number) > 0,
      `${alias}: openai-type model without a positive integer max_output_size (WI#412)`,
    )
  }
  assert.equal(models["tensorzero/FrontierCODEK"]?.max_output_size, 131072)
})

test("entrypoint unsets the workspace-trust switch beside the guard switch in both modes (WI#411)", () => {
  // kimi-code 2.2.0's KIMI_CODE_TRUST_WORKSPACE auto-spawns a repo's .mcp.json
  // servers before any model turn, outside the WI#206 hook; neither the CLI
  // mode nor the bridge mode may inherit it from the container environment.
  assert.equal(entrypoint.match(/^\s*unset KIMI_CODE_DANGEROUS_COMMAND_GUARD KIMI_CODE_TRUST_WORKSPACE$/gm)?.length, 2)
})

test("entrypoint CLI mode forces print and stream-json", () => {
  // WI#373: the strip must stay in front of the exec — the one-shot prompt
  // runs untrusted agent code and must not inherit AGENT_AUTH_TOKEN. WI#400:
  // the credential-file pointer is stripped alongside, and the one-shot runs
  // as the task uid via setpriv with the mandatory cap-clearing flags plus
  // --no-new-privs (run-164 KW-2). WI#454/ME-1: the task's toolchain
  // directories are handed over at the drop (PATH="$TASK_PATH"), never
  // baked into the image PATH root resolves through.
  assert.match(entrypoint, /SETPRIV_FLAGS=\(--reuid="\$KIMI_TASK_UID" --regid="\$KIMI_TASK_GID" --clear-groups --ambient-caps=-all --inh-caps=-all --no-new-privs\)/)
  assert.match(entrypoint, /exec setpriv "\$\{SETPRIV_FLAGS\[@\]\}"/)
  assert.match(entrypoint, /env -u AGENT_AUTH_TOKEN -u AGENT_AUTH_TOKEN_FILE HOME=\/home\/kimi PATH="\$TASK_PATH" \\?\n?\s*kimi -p "\$\*" --output-format stream-json/)
})

test("entrypoint moves the hub credential into a root-only file before spawning (WI#400)", () => {
  // The handoff must come before any long-lived spawn: root-only 0700 dir,
  // 0400 file, env scrubbed, and the bridge pointed at the file instead.
  assert.match(entrypoint, /install -d -m 0700 "\$LUNARWING_TOKEN_DIR"/)
  assert.match(entrypoint, /chmod 0400 "\$_token_tmp"/)
  assert.match(entrypoint, /unset AGENT_AUTH_TOKEN/)
  assert.match(entrypoint, /export AGENT_AUTH_TOKEN_FILE="\$LUNARWING_TOKEN_DIR\/agent_auth_token"/)
  const handoffAt = entrypoint.indexOf("unset AGENT_AUTH_TOKEN")
  const bridgeAt = entrypoint.indexOf("node /app/scripts/lunarwing_bridge.js")
  assert.ok(handoffAt > 0 && bridgeAt > handoffAt, "credential handoff must precede the bridge spawn")
})

test("task-side setup refuses to run as root (WI#400)", () => {
  assert.match(setupScript, /entrypoint-task-setup\.sh runs as the task uid only \(WI#400\)/)
  // Setup writes task-owned state, so the launcher must invoke it as the
  // task uid, never directly.
  assert.match(entrypoint, /as_task \/app\/entrypoint-task-setup\.sh/)
})

test("entrypoint pins the root PATH to root-owned directories (WI#454/ME-1)", () => {
  // Since WI#400 the launcher runs as container root: an inherited PATH
  // whose first entries are task-owned directories would let the task uid
  // plant a file that root executes. The pin must land before any command
  // resolution (before the task-id gate), and the task toolchain moves to
  // the drop (TASK_PATH), not the image PATH.
  const pin = /export PATH=\/usr\/local\/cargo\/bin:\/usr\/local\/go\/bin:\/usr\/local\/sbin:\/usr\/local\/bin:\/usr\/sbin:\/usr\/bin:\/sbin:\/bin/
  assert.match(entrypoint, pin)
  const pinAt = entrypoint.search(pin)
  const gateAt = entrypoint.indexOf("KIMI_TASK_UID/KIMI_TASK_GID must be non-zero")
  assert.ok(pinAt > 0 && gateAt > pinAt, "the root PATH pin must precede the first command resolution")
  assert.match(entrypoint, /TASK_PATH="\/home\/kimi\/\.cargo\/bin:\/usr\/local\/cargo\/bin:\/usr\/local\/go\/bin:\/home\/kimi\/go\/bin:\$PATH"/)
  assert.match(entrypoint, /setpriv "\$\{SETPRIV_FLAGS\[@\]\}" env HOME=\/home\/kimi PATH="\$TASK_PATH" "\$@"/)
  // The pinned root PATH carries no task-owned directory.
  const pinned = entrypoint.match(pin)?.[0] ?? ""
  assert.ok(!pinned.includes("/home/kimi"), "the root PATH must not carry task-owned directories")
})

test("entrypoint rejects missing config source instead of reusing stale config", () => {
  assert.ok(configSectionEnd > 0)
  const root = mkdtempSync(join(tmpdir(), "kimi-missing-config-"))
  const home = join(root, "home")
  const stale = join(home, "config.toml")
  mkdirSync(home)
  writeFileSync(stale, 'api_key = "stale-secret"\n', { mode: 0o600 })

  const result = runSetup({
    encoding: "utf8",
    env: {
      ...process.env,
      KIMI_CODE_HOME: home,
      KIMI_CONFIG_SOURCE: join(root, "missing.toml"),
    },
  })

  assert.notEqual(result.status, 0)
  assert.match(result.stderr, /Kimi config source not found/)
  assert.equal(readFileSync(stale, "utf8"), 'api_key = "stale-secret"\n')
})

test("entrypoint injects the fail-closed policy into hostile sources (WI#206)", () => {
  assert.ok(configSectionEnd > 0)
  const root = mkdtempSync(join(tmpdir(), "kimi-policy-"))
  const home = join(root, "home")
  const source = join(root, "source.toml")
  // A custom KIMI_CONFIG_SOURCE that disables the guard and ships no
  // PreToolUse hook: the worker hardens it instead of running it as-is.
  writeFileSync(source, `default_model = "m"
default_permission_mode = "auto"

[providers.p]
type = "openai"
base_url = "http://127.0.0.1:1/v1"
api_key = "k"

[permission]
dangerous_command_guard = false
`)

  const hardened = runSetup({
    encoding: "utf8",
    env: { ...process.env, KIMI_CODE_HOME: home, KIMI_CONFIG_SOURCE: source },
  })
  assert.equal(hardened.status, 0, hardened.stderr)
  const generated = readFileSync(join(home, "config.toml"), "utf8")
  assert.match(generated, /dangerous_command_guard = true/)
  assert.doesNotMatch(generated, /dangerous_command_guard = false/)
  assert.match(generated, /headless_command_guard\.mjs/)
  // A hookless guard-disabled config gets both the guard and the hook.
  writeFileSync(source, `default_model = "m"

[providers.p]
type = "openai"
base_url = "http://127.0.0.1:1/v1"
api_key = "k"
`)
  const noPolicy = runSetup({
    encoding: "utf8",
    env: { ...process.env, KIMI_CODE_HOME: home, KIMI_CONFIG_SOURCE: source },
  })
  assert.equal(noPolicy.status, 0, noPolicy.stderr)
  const injected = readFileSync(join(home, "config.toml"), "utf8")
  assert.match(injected, /dangerous_command_guard = true/)
  assert.match(injected, /headless_command_guard\.mjs/)

  // With the full baked policy present, generation preserves both as-is.
  // The baked config's base_url is the WI#330 placeholder, so this lane sets
  // KIMI_BASE_URL the way mt-admin does for a real tenant.
  writeFileSync(
    source,
    readFileSync(fileURLToPath(new URL("../config/config.toml", import.meta.url)), "utf8"),
  )
  const ok = runSetup({
    encoding: "utf8",
    env: {
      ...process.env,
      KIMI_CODE_HOME: home,
      KIMI_CONFIG_SOURCE: source,
      KIMI_BASE_URL: "http://tensorzero.example.test/openai/v1",
    },
  })
  assert.equal(ok.status, 0, ok.stderr)
  const baked = readFileSync(join(home, "config.toml"), "utf8")
  assert.equal(baked.match(/dangerous_command_guard = true/g)?.length, 1)
  assert.equal(baked.match(/headless_command_guard\.mjs/g)?.length, 1)
  assert.match(baked, /base_url = "http:\/\/tensorzero\.example\.test\/openai\/v1"/)
})

test("entrypoint refuses to run the baked placeholder base_url (WI#330)", () => {
  assert.ok(configSectionEnd > 0)
  const root = mkdtempSync(join(tmpdir(), "kimi-placeholder-"))
  const home = join(root, "home")
  const source = join(root, "source.toml")
  // The real baked config with no KIMI_BASE_URL override must fail loudly
  // instead of dialing a guaranteed-NXDOMAIN placeholder at LLM time.
  writeFileSync(
    source,
    readFileSync(fileURLToPath(new URL("../config/config.toml", import.meta.url)), "utf8"),
  )
  const env: NodeJS.ProcessEnv = {
    ...process.env,
    KIMI_CODE_HOME: home,
    KIMI_CONFIG_SOURCE: source,
  }
  delete env.KIMI_BASE_URL

  const refused = runSetup({
    encoding: "utf8",
    env,
  })
  assert.notEqual(refused.status, 0)
  assert.match(refused.stderr, /KIMI_BASE_URL is unset and the baked config is a placeholder/)

  // A whitespace-only KIMI_BASE_URL is NOT an override: the placeholder must
  // survive and the guard must still fire (whitespace bypass regression).
  const whitespace = runSetup({
    encoding: "utf8",
    env: { ...env, KIMI_BASE_URL: "  " },
  })
  assert.notEqual(whitespace.status, 0)
  assert.match(whitespace.stderr, /KIMI_BASE_URL is unset and the baked config is a placeholder/)

  // A custom source config with its own endpoint is not the placeholder and
  // must start normally without KIMI_BASE_URL.
  writeFileSync(source, `default_model = "m"

[providers.p]
type = "openai"
base_url = "http://127.0.0.1:1/v1"
api_key = "k"
`)
  const custom = runSetup({
    encoding: "utf8",
    env,
  })
  assert.equal(custom.status, 0, custom.stderr)
})

test("entrypoint starts the task runner as the task uid before the bridge (WI#454)", () => {
  // The two-container rehearsal: with the credential handoff active, the
  // launcher must start the runner daemon (the task-container half) under
  // the task uid and export its socket to the bridge, so every task is
  // delegated out of the root bridge process.
  // The runner spawn must precede the bridge spawn, and both the first
  // start and the supervisor restart go through run_task_runner (NL-1/NL-4).
  const runnerAt = entrypoint.indexOf("      run_task_runner &")
  const bridgeAt = entrypoint.indexOf("node /app/scripts/lunarwing_bridge.js")
  assert.ok(runnerAt > 0 && bridgeAt > runnerAt, "the runner must start before the bridge")
  assert.equal((entrypoint.match(/^\s+run_task_runner &$/gm) ?? []).length, 2, "first start and restart must both use run_task_runner")
  assert.doesNotMatch(entrypoint, /node \/app\/scripts\/task_runner\.js "\$RUNNER_SOCKET"/, "the runner no longer binds its own socket path (NL-7)")
  // The bridge learns the socket via env: with it set, every task is
  // delegated to the unprivileged runner.
  assert.match(entrypoint, /export LUNARWING_TASK_RUNNER_SOCKET="\$RUNNER_SOCKET"/)
  // The runner IS the task identity — the split env vars are unset for it,
  // so executeTask never attempts a second (setpriv) drop inside the runner.
  // NL-1/NL-4/NL-7: every step is an absolute exec, the PATH handed over is
  // the root-owned launcher PATH (never TASK_PATH), and root binds the socket.
  const fn = runTaskRunnerFunction()
  assert.match(fn, /exec \/usr\/bin\/python3 -I \/app\/task_runner_bind\.py "\$RUNNER_SOCKET" -- \\\n/)
  assert.match(fn, /drop=\(\/usr\/bin\/setpriv "\$\{SETPRIV_FLAGS\[@\]\}" --\)/)
  assert.match(fn, /"\$\{drop\[@\]\}" \/usr\/bin\/env -u AGENT_AUTH_TOKEN -u AGENT_AUTH_TOKEN_FILE \\\n\s*-u KIMI_TASK_UID -u KIMI_TASK_GID \\\n\s*HOME="\$runner_home" PATH="\$PATH" WORKSPACE_ROOT="\$WORKSPACE_ROOT" \\\n\s*\/usr\/local\/bin\/node \/app\/scripts\/task_runner\.js\n/)
  assert.doesNotMatch(fn, /TASK_PATH|as_task/, "the runner must not inherit the task-first PATH (NL-1)")
  // The runner gate is the credential handoff (host runs without a token
  // file keep local execution), and KIMI_TASK_RUNNER=skip is the documented
  // escape hatch.
  assert.match(entrypoint, /\[ -n "\$\{AGENT_AUTH_TOKEN_FILE:-\}" \] && \[ "\$\{KIMI_TASK_RUNNER:-\}" != "skip" \]/)
  // The runner directory is handed to the task uid (the runner writes its
  // heartbeat state there), while the socket lives in its own root-only
  // directory (NL-7), and the cleanup tears the runner down with the bridge.
  assert.match(entrypoint, /RUNNER_SOCKET="\$\{KIMI_TASK_RUNNER_SOCKET:-\/run\/lunarwing-runner-sock\/task-runner\.sock\}"/)
  // NB-17: the supervisor may have restarted the runner, so cleanup reads
  // the LIVE pid from the pid file (with the spawn-time pid as fallback).
  assert.match(entrypoint, /chown "\$KIMI_TASK_UID:\$KIMI_TASK_GID" "\$RUNNER_DIR"/)
  assert.match(entrypoint, /RUNNER_PID_FILE=/)
  assert.match(entrypoint, /cat "\$RUNNER_PID_FILE"/)
  assert.match(entrypoint, /kill "\$RUNNER_LIVE_PID"/)
})

test("entrypoint exports the runner paths before the health server starts (NC-7)", () => {
  // NC-7: /ready can only report runner liveness if the health server — a
  // separate process that inherits this environment at spawn — was started
  // after both runner variables were exported. Moving either export back
  // below the health server start would silently disable the check.
  const socketExport = entrypoint.indexOf("export LUNARWING_TASK_RUNNER_SOCKET=")
  const stateExport = entrypoint.indexOf("export LUNARWING_TASK_RUNNER_STATE_FILE=")
  const healthAt = entrypoint.indexOf("as_task /usr/bin/python3 /app/health_server.py")
  const bridgeAt = entrypoint.indexOf("node /app/scripts/lunarwing_bridge.js")
  assert.ok(socketExport > 0 && stateExport > 0 && healthAt > 0 && bridgeAt > healthAt)
  assert.ok(socketExport < healthAt, "LUNARWING_TASK_RUNNER_SOCKET must be exported before the health server starts")
  assert.ok(stateExport < healthAt, "LUNARWING_TASK_RUNNER_STATE_FILE must be exported before the health server starts")
})

test("supervisor resets its attempt counter when a runner generation survives (NC-9)", () => {
  // NC-9: without the reset, the 6th runner death over a container's whole
  // lifetime — however far apart — stops the container. The counter must be
  // cleared only after the grace window proves the new generation lives.
  const supervisor = entrypoint.slice(
    entrypoint.indexOf("_attempts=0\n        _current="),
    entrypoint.indexOf("RUNNER_SUPERVISOR_PID="),
  )
  assert.ok(supervisor.length > 0, "supervisor block not found")
  const graceAt = supervisor.indexOf("sleep 30")
  const resetAt = supervisor.indexOf("_attempts=0", graceAt)
  assert.ok(graceAt > 0 && resetAt > graceAt, "the attempt counter must reset after the 30 s grace window")
  assert.match(supervisor, /if task_runner_alive "\$_current"; then\n\s*_attempts=0\n\s*fi/)
})

test("supervisor gives up by signalling the launcher, not a stale bridge pid (NI-3)", () => {
  // NI-3: the supervisor subshell forks before BRIDGE_PID is assigned, so its
  // copy is empty and `kill "$BRIDGE_PID"` was a no-op — the container kept a
  // root bridge that accepted tasks it could never complete. It must signal
  // the launcher ($$, unchanged inside a subshell) so the cleanup trap runs.
  const supervisor = entrypoint.slice(
    entrypoint.indexOf("_attempts=0\n        _current="),
    entrypoint.indexOf("RUNNER_SUPERVISOR_PID="),
  )
  assert.match(supervisor, /kill -TERM "\$\$"/)
  assert.doesNotMatch(supervisor, /kill "\$BRIDGE_PID"/)
})

test("runner pid file lives in the root-only token directory (NC-9)", () => {
  // NC-9: the runner runs as the task uid and owns RUNNER_DIR, so a pid file
  // there could be rewritten by any task — and cleanup() runs kill as root on
  // whatever it reads. It must sit in the root-only token directory.
  assert.match(entrypoint, /RUNNER_PID_FILE="\$\{KIMI_TASK_RUNNER_PID_FILE:-\$LUNARWING_TOKEN_DIR\/task-runner\.pid\}"/)
})

// ── health server /ready: runner liveness (NC-7, NI-2) ──────────────────────
const healthServerPath = fileURLToPath(new URL("../health_server.py", import.meta.url))
const READY_PROBE = `
import importlib.util, json, os, sys
health_path, root = sys.argv[1], sys.argv[2]
os.environ["KIMI_MODE"] = "websocket"
os.environ["WS_STATE_FILE"] = os.path.join(root, "ws.json")
os.environ["LUNARWING_TASK_RUNNER_SOCKET"] = os.path.join(root, "task-runner.sock")
os.environ["LUNARWING_TASK_RUNNER_STATE_FILE"] = os.path.join(root, "task-runner-state.json")
spec = importlib.util.spec_from_file_location("health_server_probe", health_path)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
body, status = mod._build_ready()
print(json.dumps({"status": status, "body": body}))
`
// The runner writes `new Date().toISOString()` — ask node for exactly that
// rather than hand-rolling a format the runner might not produce (NI-2).
function runnerHeartbeat(): string {
  const result = spawnSync(process.execPath, ["-e", "process.stdout.write(new Date().toISOString())"], {
    encoding: "utf8",
  })
  assert.equal(result.status, 0, result.stderr)
  return result.stdout
}
function readyProbe(root: string, env: NodeJS.ProcessEnv = {}) {
  const result = spawnSync("python3", ["-I", "-c", READY_PROBE, healthServerPath, root], {
    encoding: "utf8",
    env: { ...process.env, ...env },
  })
  assert.equal(result.status, 0, result.stderr)
  return JSON.parse(result.stdout) as { status: number; body: { ready: boolean; task_runner?: { ready: boolean; error?: string } } }
}
function readyFixture(heartbeat: string | null): string {
  const root = mkdtempSync(join(tmpdir(), "kimi-ready-"))
  writeFileSync(join(root, "ws.json"), JSON.stringify({ ready: true }))
  if (heartbeat !== null) {
    // Exactly the shape task_runner.ts writes (NL-8: liveness only).
    writeFileSync(join(root, "task-runner-state.json"), JSON.stringify({
      ready: true,
      heartbeat_at: heartbeat,
    }) + "\n")
  }
  return root
}

test("health server /ready reports a fresh runner heartbeat as ready (NC-7, NI-2)", () => {
  const fresh = readyProbe(readyFixture(runnerHeartbeat()))
  assert.equal(fresh.status, 200, JSON.stringify(fresh.body))
  assert.equal(fresh.body.task_runner?.ready, true)
  // Same answer under a non-UTC host clock: the stamp is UTC, and reading it
  // as local time would make a dead runner look fresh for hours (NI-2).
  const freshInNewYork = readyProbe(readyFixture(runnerHeartbeat()), { TZ: "America/New_York" })
  assert.equal(freshInNewYork.status, 200, JSON.stringify(freshInNewYork.body))
})

test("health server /ready fails on a missing or stale runner heartbeat (NC-7, NI-2)", () => {
  const missing = readyProbe(readyFixture(null))
  assert.equal(missing.status, 503)
  assert.equal(missing.body.task_runner?.error, "state unavailable")

  const staleStamp = new Date(Date.now() - 120_000).toISOString()
  const stale = readyProbe(readyFixture(staleStamp))
  assert.equal(stale.status, 503, JSON.stringify(stale.body))
  assert.equal(stale.body.task_runner?.error, "heartbeat stale")
  // The stale verdict must not flip under a host TZ hours away from UTC: the
  // old mktime parse turned the same stamp into a future time (NI-2).
  const staleInNewYork = readyProbe(readyFixture(staleStamp), { TZ: "America/New_York" })
  assert.equal(staleInNewYork.status, 503, JSON.stringify(staleInNewYork.body))
  assert.equal(staleInNewYork.body.task_runner?.error, "heartbeat stale")
})

// ── !1873 run-200 fixes (NL-1, NL-4, NL-6, NL-7, NL-8) ──────────────────────

function runTaskRunnerFunction(): string {
  const start = entrypoint.indexOf("run_task_runner() {\n")
  const end = entrypoint.indexOf("\n}\n", start)
  assert.ok(start > 0 && end > start, "run_task_runner not found in entrypoint.sh")
  return entrypoint.slice(start, end + 3)
}

function stateFixture(state: unknown): string {
  const root = mkdtempSync(join(tmpdir(), "kimi-ready-nl6-"))
  writeFileSync(join(root, "ws.json"), JSON.stringify({ ready: true }))
  writeFileSync(join(root, "task-runner-state.json"), typeof state === "string" ? state : JSON.stringify(state))
  return root
}

test("health server /ready requires a valid, present heartbeat (NL-6)", () => {
  const cases: Array<[string, unknown, string]> = [
    ["missing heartbeat_at", { ready: true }, "heartbeat missing"],
    ["empty heartbeat_at", { ready: true, heartbeat_at: "" }, "heartbeat missing"],
    ["blank heartbeat_at", { ready: true, heartbeat_at: "   " }, "heartbeat missing"],
    ["numeric heartbeat_at", { ready: true, heartbeat_at: 1767225600 }, "heartbeat missing"],
    ["null heartbeat_at", { ready: true, heartbeat_at: null }, "heartbeat missing"],
    ["unparseable heartbeat_at", { ready: true, heartbeat_at: "yesterday" }, "heartbeat unparseable"],
    ["future heartbeat_at", { ready: true, heartbeat_at: new Date(Date.now() + 120_000).toISOString() }, "heartbeat in the future"],
    ["ready false", { ready: false, heartbeat_at: new Date().toISOString() }, "runner not ready"],
    ["ready truthy non-boolean", { ready: "yes", heartbeat_at: new Date().toISOString() }, "runner not ready"],
    ["JSON list", [1, 2, 3], "invalid runner state file"],
    ["JSON number", 42, "invalid runner state file"],
    ["broken JSON", "{not json", "invalid runner state file"],
  ]
  for (const [label, state, error] of cases) {
    const probe = readyProbe(stateFixture(state))
    assert.equal(probe.status, 503, `${label}: ${JSON.stringify(probe.body)}`)
    assert.equal(probe.body.ready, false, label)
    assert.equal(probe.body.task_runner?.error, error, `${label}: ${JSON.stringify(probe.body)}`)
  }
  // A small clock skew is not "the future".
  const skewed = readyProbe(stateFixture({ ready: true, heartbeat_at: new Date(Date.now() + 2_000).toISOString() }))
  assert.equal(skewed.status, 200, JSON.stringify(skewed.body))
})

test("health server /ready publishes runner liveness only, not the socket path or pid (NL-8)", () => {
  const probe = readyProbe(stateFixture({
    ready: true,
    heartbeat_at: runnerHeartbeat(),
    socket: "/run/lunarwing-runner-sock/task-runner.sock",
    pid: 4242,
  }))
  assert.equal(probe.status, 200, JSON.stringify(probe.body))
  assert.deepEqual(Object.keys(probe.body.task_runner ?? {}).sort(), ["heartbeat_at", "ready"])
})

interface ProcRead<T> { value?: T; error?: string }
interface RunnerProc { exe: ProcRead<string>; environ: ProcRead<string[]>; fd3: ProcRead<string> }

const RUNNER_PROC_PROBE = `const fs = require("fs")
const pid = process.argv[1]
const read = (f) => { try { return { value: f() } } catch (err) { return { error: String(err && err.message ? err.message : err) } } }
process.stdout.write(JSON.stringify({
  exe: read(() => fs.readlinkSync("/proc/" + pid + "/exe")),
  environ: read(() => fs.readFileSync("/proc/" + pid + "/environ", "utf8").split(String.fromCharCode(0))),
  fd3: read(() => fs.readlinkSync("/proc/" + pid + "/fd/3")),
}))`

/**
 * The runner's /proc/<pid>/{exe,environ,fd/3}, read as the runner's own uid.
 * Reading another uid's entries needs CAP_SYS_PTRACE, which the rootless
 * BuildKit image build does not grant (master pipeline 6308). So under root
 * a capability-free probe runs as the runner's uid/gid (65534, the harness's
 * SETPRIV_FLAGS); on a host the runner already shares our uid.
 */
function runnerProc(pid: number): RunnerProc {
  const probe = [process.execPath, "-e", RUNNER_PROC_PROBE, String(pid)]
  const argv = process.geteuid?.() === 0
    ? ["/usr/bin/setpriv", "--reuid=65534", "--regid=65534", "--clear-groups", "--inh-caps=-all", "--no-new-privs", "--", ...probe]
    : probe
  const out = spawnSync(argv[0], argv.slice(1), { encoding: "utf8", timeout: 10_000 })
  assert.equal(out.status, 0, `runner /proc probe failed: ${out.error?.message ?? ""} ${out.stderr}`)
  return JSON.parse(out.stdout) as RunnerProc
}

test("run_task_runner execs the runner in place with the root PATH and a root-bound socket (NL-1, NL-4, NL-7)", async () => {
  // The real function text, run unprivileged on a host (or as root in CI,
  // where it drops to uid 65534 through setpriv). Only the image paths are
  // re-pointed at this checkout: /app/... at the scripts and the bind
  // helper, /usr/local/bin/node at this node.
  const work = mkdtempSync(join(tmpdir(), "kimi-runrunner-"))
  try {
    chmodSync(work, 0o755)
    const app = join(work, "app")
    mkdirSync(join(app, "scripts"), { recursive: true })
    const scripts = dirname(fileURLToPath(import.meta.url))
    for (const name of readdirSync(scripts)) {
      if ((name.endsWith(".js") && !name.endsWith("_test.js")) || name === "package.json") {
        copyFileSync(join(scripts, name), join(app, "scripts", name))
      }
    }
    copyFileSync(fileURLToPath(new URL("../task_runner_bind.py", import.meta.url)), join(app, "task_runner_bind.py"))
    for (const dir of [app, join(app, "scripts")]) chmodSync(dir, 0o755)
    const fn = runTaskRunnerFunction()
      .replaceAll("/app/", `${app}/`)
      .replaceAll("/usr/local/bin/node", process.execPath)
    const socketPath = join(work, "sock", "task-runner.sock")
    const pidFile = join(work, "runner.pid")
    const harness = `set -u
SETPRIV_FLAGS=(--reuid=65534 --regid=65534 --clear-groups --ambient-caps=-all --inh-caps=-all --no-new-privs)
export AGENT_AUTH_TOKEN=forbidden-in-runner AGENT_AUTH_TOKEN_FILE=/nonexistent KIMI_TASK_UID=10002 KIMI_TASK_GID=10002
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
TASK_PATH=/home/kimi/.cargo/bin:$PATH
RUNNER_SOCKET=${JSON.stringify(socketPath)}
WORKSPACE_ROOT=${JSON.stringify(work)}
${fn}
run_task_runner &
echo $! > ${JSON.stringify(pidFile)}
wait
`
    const launcher = spawn("bash", ["-c", harness], { stdio: ["ignore", "ignore", "pipe"], detached: true })
    let stderr = ""
    launcher.stderr.on("data", (chunk: Buffer) => { stderr += chunk.toString("utf8") })
    try {
      const deadline = Date.now() + 15_000
      while (!(existsSync(socketPath) && existsSync(pidFile)) && Date.now() < deadline) {
        await new Promise((resolve) => setTimeout(resolve, 50))
      }
      assert.ok(existsSync(socketPath), `runner socket never appeared: ${stderr}`)
      const pid = Number(readFileSync(pidFile, "utf8").trim())
      // NL-4: $! is the runner itself (python3 -> setpriv -> env -> node all exec).
      // Read as the runner's uid: see runnerProc.
      const nodeExe = readlinkSync("/proc/self/exe")
      let view = runnerProc(pid)
      for (let i = 0; i < 100 && view.exe.value !== nodeExe; i++) {
        await new Promise((resolve) => setTimeout(resolve, 50))
        view = runnerProc(pid)
      }
      assert.equal(view.exe.value, nodeExe, `$! must be the node runner, not a wrapper (got ${view.exe.value ?? view.exe.error})`)
      const environ = view.environ.value
      assert.ok(environ, `runner environ unreadable: ${view.environ.error}`)
      const envValue = (key: string) => environ.find((entry) => entry.startsWith(`${key}=`))?.slice(key.length + 1)
      // NL-1: the root-owned launcher PATH, not TASK_PATH.
      assert.equal(envValue("PATH"), "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")
      for (const key of ["AGENT_AUTH_TOKEN", "AGENT_AUTH_TOKEN_FILE", "KIMI_TASK_UID", "KIMI_TASK_GID"]) {
        assert.equal(envValue(key), undefined, `${key} must not reach the runner`)
      }
      // NL-7: the listener came in as fd 3, in an owner-only directory.
      assert.equal(envValue("LUNARWING_TASK_RUNNER_LISTEN_FD"), "3")
      assert.match(view.fd3.value ?? "", /^socket:/, `runner fd 3: ${view.fd3.value ?? view.fd3.error}`)
      assert.equal(statSync(dirname(socketPath)).mode & 0o777, 0o700)
      assert.equal(statSync(dirname(socketPath)).uid, process.geteuid?.())
      const reply = await new Promise<Record<string, unknown>>((resolve, reject) => {
        const sock = connect(socketPath)
        let buffer = ""
        sock.once("error", reject)
        sock.on("data", (chunk: Buffer) => {
          buffer += chunk.toString("utf8")
          if (buffer.includes("\n")) {
            sock.destroy()
            resolve(JSON.parse(buffer.slice(0, buffer.indexOf("\n"))) as Record<string, unknown>)
          }
        })
        sock.once("connect", () => sock.write(JSON.stringify({ op: "ping" }) + "\n"))
      })
      assert.equal(reply.op, "pong")
      assert.notEqual(reply.uid, 0, "the runner must never run as root")
      if (process.geteuid?.() === 0) assert.equal(reply.uid, 65534)
    } finally {
      try { process.kill(-launcher.pid!, "SIGKILL") } catch { /* gone */ }
      try { process.kill(Number(readFileSync(pidFile, "utf8")), "SIGKILL") } catch { /* gone */ }
    }
  } finally {
    rmSync(work, { recursive: true, force: true })
  }
})
