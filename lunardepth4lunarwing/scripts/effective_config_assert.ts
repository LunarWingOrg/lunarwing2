import { createHash } from "node:crypto"
import { readFileSync, realpathSync, writeFileSync } from "node:fs"
import { createRequire } from "node:module"
import { isAbsolute, join, relative } from "node:path"
import { spawnSync } from "node:child_process"
import { isDeepStrictEqual } from "node:util"

import { isRecord } from "./lunarwing_runtime.js"

export const EXPECTED_ROW_IDS = new Set(`tool-plugin-manager plugin-manager timer hmr llm deepseek-llm-api-extensions session
session-log-deepseek typert typert-loader typert-gateway session-title session-title-llm user-questions agent
plugin-package-inventory-deepseek agent-default-model jobs llm-retry config-editor settings authorization credentials llm-pi-ai
session-persistence-jsonl attachment-local session-query-sqlite session-projection storage storage-json storage-domain
session-projection-cache otel session-telemetry-otel subprocess sandbox sandbox-policy bash-sandbox pwsh-sandbox approval permission
shell-env tool-bash tool-pwsh tool-jobs fs-observation-policy tool-fs tool-fs-search agent-instructions skill skill-filesystem
skill-badge tool-skill commands command-feedback goal goal-round-driver command-goal plan-mode token-meter compaction-basic
command-compact subagent subagent-spawn-in-process subagent-fork-in-process tool-subagent-control tool-subagent-list-agents
tool-subagent tool-subagent-fork ptc-runtime workflow-ptc tool-workflow timeout-policy spill-local spill-policy
session-checkpoint-policy tool-result-pruner image-offload tool-todo tool-goal tool-ralph repeat-tool-reminder web
web-search-deepseek web-fetch-http tool-web mcp-resources tools system-prompt agent-loop fs-sandbox llm-deepseek lunarwing-worker`
  .split(/\s+/).filter(Boolean))

interface Row {
  id?: unknown
  name?: unknown
  disabled?: unknown
  config?: unknown
}

interface GateState {
  manifestSha256: string
  profileSchemaVersion: number | "unversioned"
}

export function profileManifest(path: string): Record<string, unknown> {
  const value: unknown = JSON.parse(readFileSync(path, "utf8"))
  if (!isRecord(value) || !isRecord(value.dsh) || !isRecord(value.dsh.profile)) {
    throw new Error(`${path}: invalid profile manifest`)
  }
  return value
}

export function assertProfileSchema(path: string, exportedVersion: number | undefined): void {
  const manifest = profileManifest(path)
  const dsh = manifest.dsh as Record<string, unknown>
  const profile = dsh.profile as Record<string, unknown>
  const actual = profile.schemaVersion
  if (exportedVersion === undefined && actual !== undefined) {
    throw new Error(`${path}: schemaVersion ${String(actual)} is unsupported by the pinned source`)
  }
  if (exportedVersion !== undefined && actual !== exportedVersion) {
    throw new Error(`${path}: schemaVersion must equal pinned PROFILE_SCHEMA_VERSION ${exportedVersion}`)
  }
}

export function manifestSha256(path: string): string {
  return createHash("sha256").update(readFileSync(path)).digest("hex")
}

export function assertManifestHash(path: string, expected: string): void {
  const actual = manifestSha256(path)
  if (actual !== expected) throw new Error(`${path}: profile package.json was rewritten (${expected} != ${actual})`)
}

export function assertCompositionRows(rows: Row[], overlay: Row[]): void {
  const ids = new Set<string>()
  for (const row of rows) {
    if (typeof row.id !== "string" || !EXPECTED_ROW_IDS.has(row.id) || ids.has(row.id)) {
      throw new Error(`composition contains an unknown or duplicate row: ${String(row.id)}`)
    }
    ids.add(row.id)
    if (typeof row.name === "string" && /dsh-bash-local|experimental-auto-review|dsh-web-app/.test(row.name)) {
      throw new Error(`forbidden package in composition: ${row.name}`)
    }
    assertNoDangerousValue(row.config)
  }
  for (const expected of EXPECTED_ROW_IDS) {
    if (!ids.has(expected)) throw new Error(`composition is missing allowlisted row ${expected}`)
  }
  for (const patch of overlay) {
    const row = rows.find(candidate => candidate.id === patch.id)
    if (row === undefined) throw new Error(`overlay target is missing: ${String(patch.id)}`)
    if (patch.name !== undefined && row.name !== patch.name) throw new Error(`overlay name mismatch on ${String(patch.id)}`)
    if (patch.disabled !== undefined && row.disabled !== patch.disabled) {
      throw new Error(`overlay disabled mismatch on ${String(patch.id)}`)
    }
    if (patch.config !== undefined && !isDeepStrictEqual(row.config, patch.config)) {
      throw new Error(`overlay config drift on ${String(patch.id)}`)
    }
  }
}

function assertNoDangerousValue(value: unknown): void {
  if (typeof value === "string" && /danger-full-access|dsh-bash-local/.test(value)) {
    throw new Error("unconfined execution value in composition")
  }
  if (Array.isArray(value)) {
    for (const entry of value) assertNoDangerousValue(entry)
    return
  }
  if (isRecord(value)) {
    if (Array.isArray(value.runnerCommand) && value.runnerCommand.length > 0) throw new Error("runnerCommand override is forbidden")
    if (Array.isArray(value.runnerFailureSignatures) && value.runnerFailureSignatures.length > 0) {
      throw new Error("runnerFailureSignatures override is forbidden")
    }
    for (const entry of Object.values(value)) assertNoDangerousValue(entry)
  }
}

function closureRequire(closure: string): NodeRequire {
  return createRequire(join(closure, "node_modules", "@lunardepth", "dsh", "package.json"))
}

function yamlModule(closure: string): { load: (text: string, options: { schema: unknown }) => unknown } {
  return closureRequire(closure)("js-yaml") as { load: (text: string, options: { schema: unknown }) => unknown }
}

function entrySchema(closure: string): unknown {
  const include = closureRequire(closure)("@lunardepth/cordis-plugin-include") as { entryListSchema: unknown }
  return include.entryListSchema
}

function parseRows(closure: string, text: string): Row[] {
  const value = yamlModule(closure).load(text, { schema: entrySchema(closure) })
  if (!Array.isArray(value) || value.some(row => !isRecord(row))) throw new Error("configuration dump is not a row list")
  return value as Row[]
}

function resolvePackages(closure: string, rows: Row[]): void {
  const requireFromClosure = closureRequire(closure)
  const root = realpathSync(join(closure, "node_modules"))
  for (const row of rows) {
    if (typeof row.name !== "string") throw new Error(`row ${String(row.id)} has no package name`)
    const resolved = realpathSync(requireFromClosure.resolve(row.name))
    const fromRoot = relative(root, resolved)
    if (fromRoot === ".." || fromRoot.startsWith("../") || isAbsolute(fromRoot)) {
      throw new Error(`row ${String(row.id)} resolves outside the deployed closure`)
    }
  }
  for (const name of ["@lunardepth/dsh-lunarwing-worker-app", "@lunardepth/dsh-lunarwing-worker"]) {
    requireFromClosure.resolve(name)
  }
}

async function exportedSchemaVersion(closure: string): Promise<number | undefined> {
  const path = join(closure, "node_modules", "@lunardepth", "dsh-app-boot", "lib", "index.js")
  const module = await import(path) as { PROFILE_SCHEMA_VERSION?: unknown }
  return typeof module.PROFILE_SCHEMA_VERSION === "number" ? module.PROFILE_SCHEMA_VERSION : undefined
}

function runDump(closure: string, overlay: string): Row[] {
  const launcher = join(closure, "node_modules", "@lunardepth", "dsh", "lib", "bin.js")
  const result = spawnSync(process.execPath,
    [launcher, "--profile", "lunarwing-worker", "--patch", overlay, "--dump-config"],
    { cwd: "/run/lunardepth-launch", env: process.env, encoding: "utf8", timeout: 60_000, maxBuffer: 32 * 1024 * 1024 })
  if (result.status !== 0) throw new Error(`configuration dump failed with status ${String(result.status)}`)
  if ((result.stderr ?? "").trim() !== "") throw new Error("configuration dump emitted a loader diagnostic")
  return parseRows(closure, result.stdout)
}

function paths() {
  const closure = process.env.LUNARDEPTH_CLOSURE_ROOT || "/opt/lunardepth"
  return {
    closure,
    manifest: process.env.LUNARDEPTH_PROFILE_MANIFEST || "/home/lunardepth/.dsh/profiles/lunarwing-worker/package.json",
    overlay: process.env.LUNARDEPTH_EFFECTIVE_OVERLAY || "/run/lunardepth-config/lunarwing-lane.patch.yml",
    state: process.env.LUNARDEPTH_COMPOSITION_STATE || "/run/lunarwing-shared/runner/composition-state.json",
  }
}

export function assertLaunchEnvironment(env: NodeJS.ProcessEnv): void {
  const expected: Record<string, string> = {
    HOME: "/home/lunardepth-home",
    DSH_HOME: "/home/lunardepth/.dsh",
    TMPDIR: "/tmp",
    TMP: "/tmp",
    TEMP: "/tmp",
    WS_NO_BUFFER_UTIL: "1",
    WS_NO_UTF_8_VALIDATE: "1",
    DSH_TELEMETRY_DISABLED: "1",
    LUNARDEPTH_WS_URL: "ws://127.0.0.1:19091/ws/agent",
  }
  for (const [name, value] of Object.entries(expected)) {
    if (env[name] !== value) throw new Error(`launch environment drift: ${name}`)
  }
  if (!env.LUNARDEPTH_LOCAL_AUTH_TOKEN) throw new Error("launch environment is missing the local credential")
  for (const name of ["NODE_OPTIONS", "DSH_PERMISSION_MODE", "LUNARDEPTH_LOCAL_AUTH_TOKEN_FILE"]) {
    if (env[name] !== undefined) throw new Error(`launch environment retains forbidden ${name}`)
  }
  if ((env.PATH ?? "").split(":").some(entry => !isAbsolute(entry))) {
    throw new Error("launch environment PATH contains a relative or empty entry")
  }
}

async function runGate(): Promise<GateState> {
  assertLaunchEnvironment(process.env)
  const current = paths()
  const before = manifestSha256(current.manifest)
  const schema = await exportedSchemaVersion(current.closure)
  assertProfileSchema(current.manifest, schema)
  const rows = runDump(current.closure, current.overlay)
  assertManifestHash(current.manifest, before)
  const overlay = parseRows(current.closure, readFileSync(current.overlay, "utf8"))
  assertCompositionRows(rows, overlay)
  resolvePackages(current.closure, rows)
  const state: GateState = { manifestSha256: before, profileSchemaVersion: schema ?? "unversioned" }
  writeFileSync(current.state, JSON.stringify(state), { mode: 0o600 })
  return state
}

async function postReady(): Promise<void> {
  const current = paths()
  const previous = JSON.parse(readFileSync(current.state, "utf8")) as GateState
  assertManifestHash(current.manifest, previous.manifestSha256)
  await runGate()
  const response = await fetch(process.env.LUNARDEPTH_READY_URL || "http://127.0.0.1:19091/ready", {
    signal: AbortSignal.timeout(5_000),
  })
  if (!response.ok) throw new Error(`LunarDepth readiness returned HTTP ${response.status}`)
  const bootLog = process.env.LUNARDEPTH_BOOT_LOG
  if (bootLog) {
    const diagnostics = readFileSync(bootLog, "utf8")
    if (/skipping profile bundle|inactive|loader warning|failed to (?:mount|load)|SANDBOX_UNAVAILABLE|partial enforcement/i.test(diagnostics)) {
      throw new Error("real boot emitted a forbidden activation diagnostic")
    }
  }
  const sandboxPath = process.env.LUNARDEPTH_SANDBOX_STATE || "/run/lunarwing-shared/runner/sandbox-state.json"
  const sandbox: unknown = JSON.parse(readFileSync(sandboxPath, "utf8"))
  if (!isRecord(sandbox) || sandbox.ready !== true || sandbox.enforcement !== "full" ||
      !["bwrap", "landlock"].includes(String(sandbox.runner)) || sandbox.workspace_write !== true ||
      sandbox.temp_write !== true || sandbox.escape_denied !== true || sandbox.tool_fs_search_disabled !== true ||
      sandbox.proc_credential_denied !== true) {
    throw new Error("sandbox readiness evidence is incomplete")
  }
  const readyPath = process.env.LUNARDEPTH_GATE_RESULT || "/run/lunarwing-shared/runner/lunardepth-ready.json"
  writeFileSync(readyPath, JSON.stringify({ ready: true, composition: true, sandbox: true, task: true }), { mode: 0o640 })
}

async function main(): Promise<void> {
  const phase = process.argv[2] || "gate"
  if (phase === "gate") await runGate()
  else if (phase === "post-ready") await postReady()
  else throw new Error(`unknown composition gate phase ${phase}`)
}

if (process.argv[1]?.endsWith("effective_config_assert.js") === true) {
  void main().catch(error => {
    console.error(`[lunardepth-config] ${error instanceof Error ? error.message : String(error)}`)
    process.exitCode = 1
  })
}
