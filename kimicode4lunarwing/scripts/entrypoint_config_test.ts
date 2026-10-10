import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import { mkdirSync, mkdtempSync, readFileSync, statSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import { test } from "node:test"

const entrypointPath = fileURLToPath(new URL("../entrypoint.sh", import.meta.url))
const entrypoint = readFileSync(entrypointPath, "utf8")
const configSectionEnd = entrypoint.indexOf("# ── always start the health server")

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

  const result = spawnSync("bash", ["-c", entrypoint.slice(0, configSectionEnd)], {
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
  const invalidResult = spawnSync("bash", ["-c", entrypoint.slice(0, configSectionEnd)], {
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

  const result = spawnSync("bash", ["-c", entrypoint.slice(0, configSectionEnd)], {
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
base_url = "http://100.88.232.146:3000/openai/v1"
api_key = ""

[models."tensorzero/FrontierCODE"]
provider = "tensorzero"
model = "tensorzero::function_name::FrontierCODE"
max_context_size = 859000
capabilities = ["tool_use"]
`)

  const result = spawnSync("bash", ["-c", entrypoint.slice(0, configSectionEnd)], {
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
  assert.match(config, /base_url = "http:\/\/100\.88\.232\.146:3000\/openai\/v1"/)
  assert.equal(config.match(/\[models\./g)?.length, 1)
})

test("entrypoint CLI mode forces print and stream-json", () => {
  assert.match(entrypoint, /exec kimi -p "\$\*" --output-format stream-json/)
})

test("entrypoint rejects missing config source instead of reusing stale config", () => {
  assert.ok(configSectionEnd > 0)
  const root = mkdtempSync(join(tmpdir(), "kimi-missing-config-"))
  const home = join(root, "home")
  const stale = join(home, "config.toml")
  mkdirSync(home)
  writeFileSync(stale, 'api_key = "stale-secret"\n', { mode: 0o600 })

  const result = spawnSync("bash", ["-c", entrypoint.slice(0, configSectionEnd)], {
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

  const hardened = spawnSync("bash", ["-c", entrypoint.slice(0, configSectionEnd)], {
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
  const noPolicy = spawnSync("bash", ["-c", entrypoint.slice(0, configSectionEnd)], {
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
  const ok = spawnSync("bash", ["-c", entrypoint.slice(0, configSectionEnd)], {
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

  const refused = spawnSync("bash", ["-c", entrypoint.slice(0, configSectionEnd)], {
    encoding: "utf8",
    env,
  })
  assert.notEqual(refused.status, 0)
  assert.match(refused.stderr, /KIMI_BASE_URL is unset and the baked config is a placeholder/)

  // A custom source config with its own endpoint is not the placeholder and
  // must start normally without KIMI_BASE_URL.
  writeFileSync(source, `default_model = "m"

[providers.p]
type = "openai"
base_url = "http://127.0.0.1:1/v1"
api_key = "k"
`)
  const custom = spawnSync("bash", ["-c", entrypoint.slice(0, configSectionEnd)], {
    encoding: "utf8",
    env,
  })
  assert.equal(custom.status, 0, custom.stderr)
})
