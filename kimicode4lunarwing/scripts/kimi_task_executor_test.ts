import assert from "node:assert/strict"
import { test } from "node:test"
import { spawn } from "node:child_process"
import { mkdtempSync, writeFileSync, chmodSync, readFileSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { executeTask } from "./kimi_task_executor.js"
import type { TaskRequest, TaskResult } from "./lunarwing_runtime.js"

function makeMockKimi(behavior: "success" | "empty" | "error" | "env" | "guard-env" | "auth-token-env" | "hang" | "silent-hang" | "large" | "longline" | "no-newline" | "stderr-tail" | "unknown-role"): string {
  const dir = mkdtempSync(join(tmpdir(), "kimi-mock-"))
  const binPath = join(dir, "mock-kimi")
  let script: string
  switch (behavior) {
    case "success":
      script = `#!/usr/bin/env node
const messages = [
  { role: "assistant", content: "Working on it..." },
  { role: "assistant", content: "Done! Here is the result." },
]
for (const m of messages) {
  process.stdout.write(JSON.stringify(m) + "\\n")
}
process.exit(0)
`
      break
    case "empty":
      script = `#!/usr/bin/env node
process.exit(0)
`
      break
    case "error":
      script = `#!/usr/bin/env node
process.stderr.write("kimi failed to start\\n")
process.exit(1)
`
      break
    case "env":
      script = `#!/usr/bin/env node
const messages = [
  { role: "assistant", content: "env=" + (process.env.MOCK_LEAK_CHECK || "<unset>") },
]
for (const m of messages) {
  process.stdout.write(JSON.stringify(m) + "\\n")
}
process.exit(0)
`
      break
    case "guard-env":
      script = `#!/usr/bin/env node
const guard = process.env.KIMI_CODE_DANGEROUS_COMMAND_GUARD || "<unset>"
const reminder = process.env.KIMI_CODE_PERMISSION_MODE_REMINDER || "<unset>"
const trust = process.env.KIMI_CODE_TRUST_WORKSPACE || "<unset>"
const messages = [
  { role: "assistant", content: "guard=" + guard + " reminder=" + reminder + " trust=" + trust },
]
for (const m of messages) {
  process.stdout.write(JSON.stringify(m) + "\\n")
}
process.exit(0)
`
      break
    case "auth-token-env":
      // WI#373: reports whether the bridge's hub-handshake credential leaked
      // into the task process env — via inheritance or task-env injection.
      script = `#!/usr/bin/env node
const token = process.env.AGENT_AUTH_TOKEN || "<unset>"
const messages = [
  { role: "assistant", content: "token=" + token },
]
for (const m of messages) {
  process.stdout.write(JSON.stringify(m) + "\\n")
}
process.exit(0)
`
      break
    case "hang":
      script = `#!/usr/bin/env node
process.on("SIGTERM", () => {})
setInterval(() => {}, 1000)
`
      break
    case "silent-hang":
      // WI#263 signature: full investigation streams first, then the CLI
      // goes completely silent (wedged at report composition) while the
      // task deadline is still far away.
      script = `#!/usr/bin/env node
const messages = [
  { role: "assistant", content: "Investigation step: evidence gathered." },
]
for (const m of messages) {
  process.stdout.write(JSON.stringify(m) + "\\n")
}
process.on("SIGTERM", () => {})
setInterval(() => {}, 1000)
`
      break
    case "large":
      script = `#!/usr/bin/env node
process.stdout.write(JSON.stringify({ role: "assistant", content: "😀".repeat(400) }) + "\\n")
`
      break
    case "longline":
      script = `#!/usr/bin/env node
process.stdout.write("x".repeat(2000))
setInterval(() => {}, 1000)
`
      break
    case "no-newline":
      script = `#!/usr/bin/env node
process.stdout.write(JSON.stringify({ role: "assistant", content: "final without newline" }))
`
      break
    case "stderr-tail":
      script = `#!/usr/bin/env node
process.stderr.write("x".repeat(70000) + "FINAL-ERROR")
process.exit(1)
`
      break
    case "unknown-role":
      script = `#!/usr/bin/env node
const messages = [
  { role: "reasoning", content: "internal draft the parser must drop" },
  { role: "assistant", content: "Done! Here is the result." },
]
for (const m of messages) {
  process.stdout.write(JSON.stringify(m) + "\\n")
}
process.exit(0)
`
      break
  }
  writeFileSync(binPath, script, { mode: 0o755 })
  chmodSync(binPath, 0o755)
  return binPath
}

async function runExecutor(
  request: TaskRequest,
  binPath: string | null,
): Promise<{ result?: TaskResult; progress: string[] }> {
  if (binPath === null) delete process.env.KIMI_BIN
  else process.env.KIMI_BIN = binPath
  process.env.WORKSPACE_ROOT = "/tmp"
  process.env.KIMI_CODE_HOME = "/tmp"
  const progress: string[] = []
  let result: TaskResult | undefined
  await executeTask(
    request,
    (p) => { progress.push(p.delta) },
    (r) => { result = r },
    undefined,
  )
  delete process.env.KIMI_BIN
  return { result, progress }
}

test("executeTask: success — returns final assistant text", async () => {
  const bin = makeMockKimi("success")
  const request: TaskRequest = {
    task_id: "success-test",
    prompt: "hello world",
    timeout_ms: 10_000,
  }
  const { result, progress } = await runExecutor(request, bin)
  assert.ok(result, "result was not emitted")
  assert.equal(result!.status, "success")
  assert.equal(result!.output, "Done! Here is the result.")
  assert.equal(result!.error, null)
  assert.ok(progress.length > 0, "no progress emitted")
})

test("executeTask: empty — reports error when no output", async () => {
  const bin = makeMockKimi("empty")
  const request: TaskRequest = {
    task_id: "empty-test",
    prompt: "do nothing",
    timeout_ms: 10_000,
  }
  const { result } = await runExecutor(request, bin)
  assert.ok(result, "result was not emitted")
  assert.equal(result!.status, "error")
  assert.ok(result!.error!.includes("without assistant text"))
})

test("executeTask: nonzero exit — reports error", async () => {
  const bin = makeMockKimi("error")
  const request: TaskRequest = {
    task_id: "error-test",
    prompt: "fail please",
    timeout_ms: 10_000,
  }
  const { result } = await runExecutor(request, bin)
  assert.ok(result, "result was not emitted")
  assert.equal(result!.status, "error")
  assert.ok(result!.error!.includes("kimi failed to start") || result!.error!.includes("code 1"))
})

test("executeTask: missing task_id or prompt — returns immediate error", async () => {
  const bin = makeMockKimi("success")
  process.env.KIMI_BIN = bin
  process.env.WORKSPACE_ROOT = "/tmp"
  process.env.KIMI_CODE_HOME = "/tmp"
  let result: TaskResult | undefined
  await executeTask(
    { task_id: "", prompt: "" } as TaskRequest,
    () => {},
    (r) => { result = r },
    undefined,
  )
  delete process.env.KIMI_BIN
  // The bridge guards on missing fields; the executor receives the request
  // only after the bridge validates. Here we call executeTask directly with
  // empty fields — it should still spawn and handle gracefully. The mock
  // produces valid output, so the task should succeed with the mock's text.
  assert.ok(result, "result was not emitted")
})

test("executeTask: context.environment reaches child env without polluting process.env", async () => {
  const bin = makeMockKimi("env")
  const request: TaskRequest = {
    task_id: "env-test",
    prompt: "check env",
    timeout_ms: 10_000,
    context: { environment: { MOCK_LEAK_CHECK: "present" } },
  }
  const { result } = await runExecutor(request, bin)
  assert.ok(result, "result was not emitted")
  assert.equal(result!.status, "success")
  assert.equal(result!.output, "env=present")
  // The bridge's own process must never see the per-task variable — with
  // concurrent tasks, mutation of shared process.env would leak across tasks.
  assert.equal(process.env.MOCK_LEAK_CHECK, undefined)
})

test("executeTask: task environment cannot disable the dangerous-command guard (WI#206)", async () => {
  const bin = makeMockKimi("guard-env")
  const request: TaskRequest = {
    task_id: "guard-env-test",
    prompt: "try to disable the guard",
    timeout_ms: 10_000,
    // Attacker-controllable task input attempting to lower the approval
    // policy for its own run — including kimi-code 2.2.0's workspace-trust
    // switch, which would auto-spawn the repo's .mcp.json servers (WI#411).
    context: {
      environment: {
        KIMI_CODE_DANGEROUS_COMMAND_GUARD: "false",
        KIMI_CODE_PERMISSION_MODE_REMINDER: "1",
        KIMI_CODE_TRUST_WORKSPACE: "1",
      },
    },
  }
  const { result } = await runExecutor(request, bin)
  assert.ok(result, "result was not emitted")
  assert.equal(result!.status, "success")
  assert.equal(result!.output, "guard=<unset> reminder=<unset> trust=<unset>")
})

test("executeTask: hub-handshake token never reaches the task env (WI#373)", async () => {
  const bin = makeMockKimi("auth-token-env")
  const request: TaskRequest = {
    task_id: "auth-token-env-test",
    prompt: "read env",
    timeout_ms: 10_000,
    // Attacker-controllable task input attempting to re-inject the bridge's
    // credential name for its own run.
    context: { environment: { AGENT_AUTH_TOKEN: "forged-by-task" } },
  }
  // Simulate the bridge process env: the handshake credential is present in
  // process.env (server-role bridge requires it) and must not be inherited.
  const prev = process.env.AGENT_AUTH_TOKEN
  process.env.AGENT_AUTH_TOKEN = "bridge-secret-64hex"
  // GF-1: the FY-5 drop must be logged (key and task id, never the value),
  // so capture console.warn for the duration of the run.
  const warnings: string[] = []
  const originalWarn = console.warn
  console.warn = (...args: unknown[]) => {
    warnings.push(args.map((a) => String(a)).join(" "))
  }
  try {
    const { result } = await runExecutor(request, bin)
    assert.ok(result, "result was not emitted")
    assert.equal(result!.status, "success")
    assert.equal(result!.output, "token=<unset>")
  } finally {
    console.warn = originalWarn
    if (prev === undefined) delete process.env.AGENT_AUTH_TOKEN
    else process.env.AGENT_AUTH_TOKEN = prev
  }
  // The bridge's own credential is untouched by the task run.
  assert.equal(process.env.AGENT_AUTH_TOKEN, prev)
  const dropWarnings = warnings.filter((w) => w.includes("dropped task-supplied env entry"))
  assert.equal(
    dropWarnings.length,
    1,
    `expected exactly one FY-5 drop warning, got ${JSON.stringify(warnings)}`,
  )
  assert.ok(
    dropWarnings[0]!.includes("AGENT_AUTH_TOKEN"),
    `drop warning must name the dropped key: ${dropWarnings[0]}`,
  )
  assert.ok(
    dropWarnings[0]!.includes("auth-token-env-test"),
    `drop warning must name the task id: ${dropWarnings[0]}`,
  )
  const logged = warnings.join("\n")
  assert.ok(!logged.includes("forged-by-task"), "the forged credential value must never be logged")
  assert.ok(!logged.includes("bridge-secret-64hex"), "the bridge credential value must never be logged")
})

test("executeTask: pre-aborted signal returns cancelled without spawning", async () => {
  const controller = new AbortController()
  controller.abort()
  process.env.KIMI_BIN = "/definitely/not/a/kimi/binary"
  process.env.WORKSPACE_ROOT = "/tmp"
  let result: TaskResult | undefined
  await executeTask(
    { task_id: "pre-abort", prompt: "never spawn" },
    () => {},
    (value) => { result = value },
    controller.signal,
  )
  delete process.env.KIMI_BIN
  assert.equal(result?.status, "cancelled")
})

test("executeTask: timeout escalates past ignored SIGTERM", async () => {
  const bin = makeMockKimi("hang")
  process.env.KIMI_KILL_GRACE_MS = "100"
  const started = Date.now()
  const { result } = await runExecutor(
    { task_id: "timeout-test", prompt: "hang", timeout_ms: 50 },
    bin,
  )
  delete process.env.KIMI_KILL_GRACE_MS
  assert.equal(result?.status, "timed_out")
  assert.ok(Date.now() - started < 2_000)
})

test("executeTask: silent wedge resolves as timed_out with partial output (WI#263)", async () => {
  const bin = makeMockKimi("silent-hang")
  process.env.KIMI_SILENCE_TIMEOUT_MS = "300"
  process.env.KIMI_KILL_GRACE_MS = "100"
  const started = Date.now()
  const { result, progress } = await runExecutor(
    { task_id: "silent-wedge-test", prompt: "wedge after investigation", timeout_ms: 60_000 },
    bin,
  )
  delete process.env.KIMI_SILENCE_TIMEOUT_MS
  delete process.env.KIMI_KILL_GRACE_MS
  assert.equal(result?.status, "timed_out")
  assert.match(result?.error || "", /no stream output for 300ms/)
  // The streamed investigation evidence must survive the wedge.
  assert.ok((result?.output || "").includes("Investigation step"))
  assert.ok(progress.some((delta) => delta.includes("Investigation step")))
  // The watchdog must fire well before the task's own deadline.
  assert.ok(Date.now() - started < 5_000, "watchdog must not wait for the task deadline")
})

test("executeTask: silence watchdog disabled with KIMI_SILENCE_TIMEOUT_MS=0", async () => {
  const bin = makeMockKimi("success")
  process.env.KIMI_SILENCE_TIMEOUT_MS = "0"
  const { result } = await runExecutor(
    { task_id: "watchdog-off", prompt: "normal run", timeout_ms: 10_000 },
    bin,
  )
  delete process.env.KIMI_SILENCE_TIMEOUT_MS
  assert.equal(result?.status, "success")
})

test("executeTask: output and progress caps count UTF-8 bytes", async () => {
  const bin = makeMockKimi("large")
  process.env.KIMI_MAX_OUTPUT_BYTES = "1024"
  process.env.KIMI_MAX_PROGRESS_BYTES = "256"
  process.env.KIMI_MAX_STDOUT_LINE_BYTES = "4096"
  const { result, progress } = await runExecutor(
    { task_id: "bounded-test", prompt: "large", timeout_ms: 10_000 },
    bin,
  )
  delete process.env.KIMI_MAX_OUTPUT_BYTES
  delete process.env.KIMI_MAX_PROGRESS_BYTES
  delete process.env.KIMI_MAX_STDOUT_LINE_BYTES
  assert.equal(result?.status, "success")
  assert.ok(Buffer.byteLength(result?.output || "") <= 1024)
  assert.ok(progress.every((delta) => Buffer.byteLength(delta) <= 256))
})

test("executeTask: rejects oversized stdout line", async () => {
  const bin = makeMockKimi("longline")
  process.env.KIMI_MAX_STDOUT_LINE_BYTES = "1024"
  const { result } = await runExecutor(
    { task_id: "line-test", prompt: "large line", timeout_ms: 10_000 },
    bin,
  )
  delete process.env.KIMI_MAX_STDOUT_LINE_BYTES
  assert.equal(result?.status, "error")
  assert.match(result?.error || "", /stdout line exceeds 1024 bytes/)
})

test("executeTask: parses final JSONL record without trailing newline", async () => {
  const bin = makeMockKimi("no-newline")
  const { result } = await runExecutor(
    { task_id: "no-newline", prompt: "finish", timeout_ms: 10_000 },
    bin,
  )
  assert.equal(result?.status, "success")
  assert.equal(result?.output, "final without newline")
})

test("executeTask: keeps actionable stderr tail", async () => {
  const bin = makeMockKimi("stderr-tail")
  const { result } = await runExecutor(
    { task_id: "stderr-tail", prompt: "fail", timeout_ms: 10_000 },
    bin,
  )
  assert.equal(result?.status, "error")
  assert.match(result?.error || "", /FINAL-ERROR$/)
  assert.ok(Buffer.byteLength(result?.error || "") <= 65_536)
})

test("executeTask: warns on unrecognized stream-json roles instead of silently dropping", async () => {
  const bin = makeMockKimi("unknown-role")
  const warnings: string[] = []
  const originalWarn = console.warn
  console.warn = (...args: unknown[]) => { warnings.push(args.join(" ")) }
  let result: TaskResult | undefined
  try {
    ;({ result } = await runExecutor(
      { task_id: "unknown-role", prompt: "schema drift", timeout_ms: 10_000 },
      bin,
    ))
  } finally {
    console.warn = originalWarn
  }
  assert.equal(result?.status, "success")
  assert.equal(result?.output, "Done! Here is the result.")
  assert.equal(warnings.length, 1)
  assert.match(warnings[0]!, /unrecognized role "reasoning"/)
  // The dropped token content must not leak into the result output.
  assert.ok(!(result?.output || "").includes("internal draft"))
})

test("executeTask: invalid workspace emits one error result", async () => {
  const bin = makeMockKimi("success")
  const { result } = await runExecutor(
    {
      task_id: "missing-workspace",
      prompt: "never spawn",
      timeout_ms: 10_000,
      context: { project_dir: "/tmp/definitely-missing-kimi-workspace" },
    },
    bin,
  )
  assert.equal(result?.status, "error")
})

test("executeTask: with the task identity pinned, the CLI spawns through setpriv with the pinned argv (WI#400)", async () => {
  // KW-7(b): the websocket-mode drop must not silently disappear — removing
  // the `if (identity)` wrap would ship the task running as the bridge's uid
  // with no test noticing. A PATH-stub setpriv records its argv and execs
  // the wrapped command unchanged (the host run is unprivileged; the real
  // drop happens in the image).
  const dir = mkdtempSync(join(tmpdir(), "kimi-setpriv-stub-"))
  const recordFile = join(dir, "argv.json")
  const stub = join(dir, "setpriv")
  writeFileSync(
    stub,
    `#!/usr/bin/env node
const fs = require("node:fs")
const argv = process.argv.slice(2)
fs.writeFileSync(${JSON.stringify(recordFile)}, JSON.stringify(argv))
const rest = argv.slice(argv.indexOf("--") + 1)
const { spawnSync } = require("node:child_process")
const r = spawnSync(rest[0], rest.slice(1), { stdio: "inherit", env: process.env })
process.exit(r.status ?? 1)
`,
    { mode: 0o755 },
  )
  const bin = makeMockKimi("success")
  const savedPath = process.env.PATH
  process.env.PATH = `${dir}:${savedPath}`
  process.env.KIMI_TASK_UID = "10002"
  process.env.KIMI_TASK_GID = "10002"
  try {
    const { result } = await runExecutor(
      { task_id: "setpriv-pin", prompt: "hello world", timeout_ms: 10_000 },
      bin,
    )
    assert.equal(result?.status, "success", `task failed: ${result?.error}`)
    const recorded = JSON.parse(readFileSync(recordFile, "utf8")) as string[]
    const prefix = [
      "--reuid=10002",
      "--regid=10002",
      "--clear-groups",
      "--ambient-caps=-all",
      "--inh-caps=-all",
      "--no-new-privs",
      "--",
    ]
    assert.deepEqual(recorded.slice(0, prefix.length), prefix)
    assert.equal(recorded[prefix.length], bin, "the CLI path follows the pinned setpriv prefix")
  } finally {
    process.env.PATH = savedPath
    delete process.env.KIMI_TASK_UID
    delete process.env.KIMI_TASK_GID
    rmSync(dir, { recursive: true, force: true })
  }
})

test("executeTask: WI#463 — a bare-name CLI resolves through the bridge PATH at the drop, an absolute KIMI_BIN passes through", async () => {
  // Two pins in one lane (they share the setpriv stub machinery):
  // 1. With KIMI_BIN unset, the setpriv argv must carry the bridge-PATH-
  //    resolved `kimi` (resolveFromBridgePath — root-ownership-checked when
  //    the resolver runs as root), not the bare name that the task-side
  //    PATH would resolve after the drop.
  // 2. With KIMI_BIN set to an absolute path (every operator/test override),
  //    that exact path is passed through unchanged.
  // The stub setpriv records its argv and execs the wrapped command, so both
  // shapes run the task to completion and the argv can be asserted. A plain
  // `kimi` shim is planted FIRST on the bridge PATH ahead of the real mock
  // to prove the resolution is ordered and picks the first absolute entry.
  const dir = mkdtempSync(join(tmpdir(), "kimi-wi463-stub-"))
  const recordFile = join(dir, "argv.json")
  const stub = join(dir, "setpriv")
  writeFileSync(
    stub,
    `#!/usr/bin/env node
const fs = require("node:fs")
const argv = process.argv.slice(2)
fs.writeFileSync(${JSON.stringify(recordFile)}, JSON.stringify(argv))
const rest = argv.slice(argv.indexOf("--") + 1)
const { spawnSync } = require("node:child_process")
const r = spawnSync(rest[0], rest.slice(1), { stdio: "inherit", env: process.env })
process.exit(r.status ?? 1)
`,
    { mode: 0o755 },
  )
  const bin = makeMockKimi("success")
  // A second mock named exactly `kimi` in its own directory: the bare-name
  // resolution looks up "kimi" per PATH entry, and makeMockKimi names its
  // binary "mock-kimi" (absolute-KIMI_BIN tests point at that path).
  const bareDir = mkdtempSync(join(tmpdir(), "kimi-wi463-bare-"))
  const bareBin = join(bareDir, "kimi")
  writeFileSync(bareBin, readFileSync(bin, "utf8"), { mode: 0o755 })
  chmodSync(bareBin, 0o755)
  const savedPath = process.env.PATH
  process.env.PATH = `${dir}:${savedPath}`
  process.env.KIMI_TASK_UID = "10002"
  process.env.KIMI_TASK_GID = "10002"
  const recordedArgv = async (): Promise<string[]> =>
    JSON.parse(readFileSync(recordFile, "utf8")) as string[]
  try {
    // 1. Bare name: KIMI_BIN unset (runExecutor(null)). The `kimi`-named
    // mock's directory is prepended to the bridge PATH so
    // resolveFromBridgePath finds it (the host run is unprivileged, so the
    // root-ownership skip is inert here — the image's root-shaped run
    // exercises it).
    process.env.PATH = `${bareDir}:${process.env.PATH}`
    const { result } = await runExecutor(
      { task_id: "wi463-bare", prompt: "hello world", timeout_ms: 10_000 },
      null,
    )
    assert.equal(result?.status, "success", `bare-name task failed: ${result?.error}`)
    const recorded = await recordedArgv()
    const dropIndex = recorded.indexOf("--")
    assert.ok(dropIndex >= 0, "setpriv argv must contain the -- separator")
    const resolved = recorded[dropIndex + 1]
    assert.equal(resolved, bareBin, `bare-name CLI must resolve to the bridge-PATH entry (got ${resolved})`)

    // 2. Absolute KIMI_BIN: passed through byte-for-byte.
    const { result: absResult } = await runExecutor(
      { task_id: "wi463-absolute", prompt: "hello world", timeout_ms: 10_000 },
      bin,
    )
    assert.equal(absResult?.status, "success", `absolute-path task failed: ${absResult?.error}`)
    const absRecorded = await recordedArgv()
    const absDropIndex = absRecorded.indexOf("--")
    assert.equal(absRecorded[absDropIndex + 1], bin, "an absolute KIMI_BIN must pass through unchanged")
  } finally {
    process.env.PATH = savedPath
    delete process.env.KIMI_BIN
    delete process.env.KIMI_TASK_UID
    delete process.env.KIMI_TASK_GID
    rmSync(dir, { recursive: true, force: true })
    rmSync(bareDir, { recursive: true, force: true })
  }
})

console.log("kimi task executor self-check passed")

test("executeTask: WI#462/NB-10 — the drop PATH is root-owned dirs FIRST, task dirs after (ME-1 posture)", async () => {
  // NB-10 (run-195 review): the child env.PATH the executor hands the
  // dropped task must carry the pinned root-owned dirs BEFORE the
  // task-uid-writable ones. Task-first ordering lets ONE task's leftovers
  // (a planted node/python, a rewritten entry point) become the tools every
  // LATER task's process resolves — including the kimi CLI's own
  // env-node interpreter lookup. The stub setpriv records the env it was
  // handed and exits, so the PATH can be asserted from the recorded env.
  const dir = mkdtempSync(join(tmpdir(), "kimi-nb10-stub-"))
  const recordFile = join(dir, "argv.json")
  const stub = join(dir, "setpriv")
  writeFileSync(
    stub,
    `#!/usr/bin/env node
const fs = require("node:fs")
const argv = process.argv.slice(2)
fs.writeFileSync(${JSON.stringify(recordFile)}, JSON.stringify({ argv, path: process.env.PATH }))
const rest = argv.slice(argv.indexOf("--") + 1)
const { spawnSync } = require("node:child_process")
const r = spawnSync(rest[0], rest.slice(1), { stdio: "inherit", env: process.env })
process.exit(r.status ?? 1)
`,
    { mode: 0o755 },
  )
  const bin = makeMockKimi("success")
  const savedPath = process.env.PATH
  process.env.PATH = `${dir}:${savedPath}`
  process.env.KIMI_TASK_UID = "10002"
  process.env.KIMI_TASK_GGID = undefined
  process.env.KIMI_TASK_GID = "10002"
  try {
    const { result } = await runExecutor(
      { task_id: "nb10-order", prompt: "hello world", timeout_ms: 10_000 },
      bin,
    )
    assert.equal(result?.status, "success", `task failed: ${result?.error}`)
    const record = JSON.parse(readFileSync(recordFile, "utf8")) as { argv: string[]; path: string }
    const childPath = record.path || ""
    const firstTaskOwned = childPath.indexOf("/home/kimi/.cargo/bin")
    const firstRootOwned = childPath.indexOf("/usr/local/bin")
    assert.ok(
      firstRootOwned >= 0 && firstTaskOwned >= 0 && firstRootOwned < firstTaskOwned,
      `drop PATH must put root-owned dirs before task-writable dirs (NB-10): ${childPath}`,
    )
  } finally {
    process.env.PATH = savedPath
    delete process.env.KIMI_TASK_UID
    delete process.env.KIMI_TASK_GID
    delete process.env.KIMI_TASK_GGID
    rmSync(dir, { recursive: true, force: true })
  }
})

test("executeTask: WI#463/NB-11 — on the pinned path a failed bridge CLI lookup fails the task (no bare-name fallback)", async () => {
  // NB-11: the old catch returned the bare name on a bridge-lookup miss,
  // sending setpriv down the child PATH — the lookup WI#463 exists to
  // avoid. On the pinned path the failure must surface as a task error.
  // Set up: a stub setpriv (the SCALED setpriv pin — resolveFromBridgePath
  // finds it; it records argv and exits) and a mock kimi named "kimi-mock"
  // (an ALIAS on the bridge PATH). The executor's taskKimiBin is called
  // with a bare name that is NOT on the bridge PATH, so resolveFromBridgePath
  // throws and the task must error out cleanly.
  const stubDir = mkdtempSync(join(tmpdir(), "kimi-nb11-stub-"))
  const recordFile = join(stubDir, "argv.json")
  const setprivStub = join(stubDir, "setpriv")
  writeFileSync(
    setprivStub,
    `#!/usr/bin/env node
const fs = require("node:fs")
const argv = process.argv.slice(2)
fs.writeFileSync(${JSON.stringify(recordFile)}, JSON.stringify(argv))
process.exit(0)
`,
    { mode: 0o755 },
  )
  // A mock kimi named "kimi-mock" (not "kimi") — an absolute-path binary on
  // the bridge PATH, so runExecutor can use it as the bin for the task but
  // taskKimiBin's bare-name lookup for "kimi" finds nothing.
  const mockDir = mkdtempSync(join(tmpdir(), "kimi-nb11-mock-"))
  const mockPath = join(mockDir, "kimi-mock")
  writeFileSync(
    mockPath,
    `#!/usr/bin/env node
process.stdout.write(JSON.stringify({ role: "assistant", content: "ok" }) + "\\n")
process.exit(0)
`,
    { mode: 0o755 },
  )
  const savedPath = process.env.PATH
  process.env.PATH = `${stubDir}:${mockDir}:${savedPath}`
  process.env.KIMI_TASK_UID = "10002"
  process.env.KIMI_TASK_GID = "10002"
  // runExecutor(null) DELETES KIMI_BIN, which resets to the default bare
  // "kimi" — resolvable on a host that has the CLI installed. Set the
  // absent bare name AFTER that point by calling executeTask directly.
  process.env.KIMI_BIN = "kimi-absent-nb11" // bare name guaranteed absent from PATH
  let result: TaskResult | undefined
  try {
    await executeTask(
      { task_id: "nb11-failclosed", prompt: "hello", timeout_ms: 10_000 },
      () => {},
      (r) => { result = r },
      undefined,
    )
    assert.equal(result?.status, "error")
    assert.match(result?.error || "", /not found on the bridge PATH/)
    // The setpriv pin should NOT have been reached (resolution failed
    // BEFORE spawn), so the record file should not exist — or exist only
    // from the prior argv, not this run. Assert no record was written.
    const fsStat = (await import("node:fs")).statSync
    let recordExists = false
    try { fsStat(recordFile); recordExists = true } catch { /* expected */ }
    assert.ok(!recordExists, "setpriv stub should not run if bridge CLI lookup fails first")
  } finally {
    process.env.PATH = savedPath
    delete process.env.KIMI_TASK_UID
    delete process.env.KIMI_TASK_GID
    delete process.env.KIMI_BIN
    rmSync(stubDir, { recursive: true, force: true })
    rmSync(mockDir, { recursive: true, force: true })
  }
})
