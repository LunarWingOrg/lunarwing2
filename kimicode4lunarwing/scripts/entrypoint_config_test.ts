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
  assert.equal(readFileSync(join(home, "config.toml"), "utf8"), 'api_key = ""\n')
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
