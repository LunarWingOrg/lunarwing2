import { spawnSync, type SpawnSyncReturns } from "node:child_process"
import { existsSync, readFileSync, writeFileSync } from "node:fs"
import { createRequire } from "node:module"
import { basename, join } from "node:path"
import { pathToFileURL } from "node:url"

interface ConfinedArgv {
  argv: string[]
  enforcement: string
}

interface SandboxProvider {
  confine: (argv: readonly string[], policy: { mode: "workspace-write"; workspaceRoot: string }) => Promise<ConfinedArgv>
}

interface Probe {
  confined: ConfinedArgv
  result: SpawnSyncReturns<string>
}

function closureRequire(closure: string): NodeRequire {
  return createRequire(join(closure, "node_modules", "@lunardepth", "dsh", "package.json"))
}

async function provider(closure: string): Promise<SandboxProvider> {
  const requireFromClosure = closureRequire(closure)
  const cordisModule = await import(pathToFileURL(requireFromClosure.resolve("@lunardepth/cordis")).href)
  const sandboxModule = await import(pathToFileURL(requireFromClosure.resolve("@lunardepth/dsh-sandbox-local")).href)
  const Context = cordisModule.Context as new () => unknown
  const Provider = sandboxModule.LocalSandboxProvider as new (context: unknown, config: {
    runnerCommand: string[]
    runnerFailureSignatures: string[]
    probeTimeoutMs: number
  }) => SandboxProvider
  return new Provider(new Context(), { runnerCommand: [], runnerFailureSignatures: [], probeTimeoutMs: 5_000 })
}

async function runProbe(current: SandboxProvider, script: string, workspace: string): Promise<Probe> {
  const confined = await current.confine(["/bin/bash", "-c", script, "bash", workspace], {
    mode: "workspace-write",
    workspaceRoot: workspace,
  })
  if (confined.enforcement !== "full") throw new Error("selected runner does not report full enforcement")
  const program = confined.argv[0]
  if (program === undefined || !["bwrap", "landlock-run"].includes(basename(program))) {
    throw new Error(`unexpected sandbox runner ${String(program)}`)
  }
  const result = spawnSync(program, confined.argv.slice(1), {
    cwd: workspace,
    env: {
      PATH: "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
      HOME: "/home/lunardepth",
      TMPDIR: "/tmp",
      TMP: "/tmp",
      TEMP: "/tmp",
      WS_NO_BUFFER_UTIL: "1",
      WS_NO_UTF_8_VALIDATE: "1",
    },
    encoding: "utf8",
    timeout: 15_000,
    maxBuffer: 1024 * 1024,
  })
  if (result.error !== undefined) throw result.error
  return { confined, result }
}

function requireSuccess(probe: Probe, label: string): void {
  if (probe.result.status !== 0) throw new Error(`${label} failed with status ${String(probe.result.status)}`)
}

function requireDenied(probe: Probe, label: string): void {
  if (probe.result.status === 0) throw new Error(`${label} unexpectedly succeeded`)
  if (!/read-only file system|permission denied|operation not permitted/i.test(probe.result.stderr)) {
    throw new Error(`${label} did not produce a confinement denial`)
  }
}

function runnerName(probe: Probe): string {
  const program = probe.confined.argv[0]
  if (program === undefined) throw new Error("sandbox runner is missing")
  return basename(program) === "bwrap" ? "bwrap" : "landlock"
}

async function main(): Promise<void> {
  const closure = process.env.LUNARDEPTH_CLOSURE_ROOT || "/opt/lunardepth"
  const workspace = process.env.LUNARDEPTH_SANDBOX_WORKSPACE || "/workspace"
  const statePath = process.env.LUNARDEPTH_SANDBOX_STATE || "/run/lunarwing-shared/runner/sandbox-state.json"
  const overlay = readFileSync(process.env.LUNARDEPTH_EFFECTIVE_OVERLAY || "/run/lunardepth-config/lunarwing-lane.patch.yml", "utf8")
  if (!/id: tool-fs-search\n\s+name: '@lunardepth\/dsh-tool-fs-search'\n\s+disabled: true/.test(overlay)) {
    throw new Error("tool-fs-search is not disabled by the effective overlay")
  }
  const escapePath = "/home/lunardepth/.lunardepth-sandbox-escape"
  if (existsSync(escapePath)) throw new Error("sandbox escape fixture already exists")
  const current = await provider(closure)
  const write = await runProbe(current,
    'set -eu; printf confined-ok > "$1/.lunardepth-write"; test "$(cat "$1/.lunardepth-write")" = confined-ok; temp="$(mktemp "${TMPDIR}/lunardepth-smoke.XXXXXX")"; test -f "$temp"; rm -f "$temp"; rm -f "$1/.lunardepth-write"',
    workspace)
  requireSuccess(write, "workspace and mktemp write")
  const runner = runnerName(write)
  const escape = await runProbe(current, 'printf denied > /home/lunardepth/.lunardepth-sandbox-escape', workspace)
  requireDenied(escape, "outside-workspace write")
  if (existsSync(escapePath)) throw new Error("outside-workspace write reached the host")
  const previousKey = process.env.LUNARDEPTH_API_KEY
  process.env.LUNARDEPTH_API_KEY = "lunardepth-proc-smoke-only-not-a-secret"
  let procDenied = false
  try {
    const proc = await runProbe(current,
      'if /bin/grep -F -l -q lunardepth-proc-smoke-only-not-a-secret /proc/[0-9]*/environ 2>/dev/null; then exit 0; fi; exit 1',
      workspace)
    procDenied = proc.result.status !== 0
  } finally {
    if (previousKey === undefined) delete process.env.LUNARDEPTH_API_KEY
    else process.env.LUNARDEPTH_API_KEY = previousKey
  }
  if (!procDenied) throw new Error("a confined bash call read the harness model credential")
  writeFileSync(statePath, JSON.stringify({
    ready: true,
    runner,
    enforcement: "full",
    workspace_write: true,
    temp_write: true,
    escape_denied: true,
    tool_fs_search_disabled: true,
    proc_credential_denied: true,
  }), { mode: 0o640 })
}

if (process.argv[1]?.endsWith("sandbox_smoke.js") === true) {
  void main().catch(error => {
    console.error(`[lunardepth-sandbox] ${error instanceof Error ? error.message : String(error)}`)
    process.exitCode = 1
  })
}
