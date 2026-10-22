import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import { lchownSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, statSync, writeFileSync } from "node:fs"
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
  assert.match(entrypoint, /as_task env -u AGENT_AUTH_TOKEN -u AGENT_AUTH_TOKEN_FILE \\\n\s*WORKSPACE_ROOT="\$WORKSPACE_ROOT" \\\n\s*node \/app\/scripts\/task_runner\.js "\$RUNNER_SOCKET" &/)
  assert.match(entrypoint, /export LUNARWING_TASK_RUNNER_SOCKET="\$RUNNER_SOCKET"/)
  // The runner spawn must precede the bridge spawn.
  const runnerAt = entrypoint.indexOf("node /app/scripts/task_runner.js")
  const bridgeAt = entrypoint.indexOf("node /app/scripts/lunarwing_bridge.js")
  assert.ok(runnerAt > 0 && bridgeAt > runnerAt, "the runner must start before the bridge")
  // The runner gate is the credential handoff (host runs without a token
  // file keep local execution), and KIMI_TASK_RUNNER=skip is the documented
  // escape hatch.
  assert.match(entrypoint, /\[ -n "\$\{AGENT_AUTH_TOKEN_FILE:-\}" \] && \[ "\$\{KIMI_TASK_RUNNER:-\}" != "skip" \]/)
  // The runner directory is handed to the task uid (the runner creates its
  // socket there), and the cleanup tears the runner down with the bridge.
  assert.match(entrypoint, /chown "\$KIMI_TASK_UID:\$KIMI_TASK_GID" "\$RUNNER_DIR"/)
  assert.match(entrypoint, /kill "\$RUNNER_PID"/)
})
