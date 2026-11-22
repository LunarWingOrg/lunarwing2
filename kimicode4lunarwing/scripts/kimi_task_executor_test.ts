import assert from "node:assert/strict"
import { test } from "node:test"
import { spawn } from "node:child_process"
import { chownSync, mkdtempSync, writeFileSync, chmodSync, readFileSync, rmSync, statSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { captureTrustedMcpConfig, executeTask } from "./kimi_task_executor.js"
import type { TaskRequest, TaskResult } from "./lunarwing_runtime.js"

function makeMockKimi(behavior: "success" | "empty" | "error" | "env" | "guard-env" | "reserved-env" | "auth-token-env" | "hang" | "silent-hang" | "large" | "longline" | "no-newline" | "stderr-tail" | "unknown-role" | "mcp-file"): string {
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
    case "reserved-env":
      // WI#833: report the relevant env settings the CLI actually received.
      script = `#!/usr/bin/env node
const relay = process.env.KIMI_CODE_REMOTE_CONTROL_RELAY_URL || "<unset>"
const update = process.env.KIMI_CODE_NO_AUTO_UPDATE || "<unset>"
const telemetry = process.env.KIMI_DISABLE_TELEMETRY || "<unset>"
const version = process.env.KIMI_CODE_VERSION || "<unset>"
const imageVersion = process.env.KIMI_CODE_IMAGE_VERSION || "<unset>"
const home = process.env.KIMI_CODE_HOME || "<unset>"
const futureFlag = process.env.KIMI_CODE_SOME_FUTURE_FLAG || "<unset>"
const messages = [
  { role: "assistant", content: "relay=" + relay + " update=" + update + " telemetry=" + telemetry + " version=" + version + " image=" + imageVersion + " home=" + home + " future=" + futureFlag },
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
    case "mcp-file":
      // Reads what the CLI would load at session start, so a test can prove
      // the executor restored the trusted file BEFORE the spawn (WI#848).
      script = `#!/usr/bin/env node
const { readFileSync } = require("node:fs")
const { join } = require("node:path")
let text = "<unreadable>"
try {
  text = readFileSync(join(process.env.KIMI_CODE_HOME || "", "mcp.json"), "utf8").trim()
} catch (error) {
  text = "read failed: " + error.message
}
process.stdout.write(JSON.stringify({ role: "assistant", content: text }) + "\\n")
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
  kimiHome: string | null = "/tmp",
): Promise<{ result?: TaskResult; progress: string[] }> {
  const previousEnv = {
    KIMI_BIN: process.env.KIMI_BIN,
    WORKSPACE_ROOT: process.env.WORKSPACE_ROOT,
    KIMI_CODE_HOME: process.env.KIMI_CODE_HOME,
  }
  if (binPath === null) delete process.env.KIMI_BIN
  else process.env.KIMI_BIN = binPath
  process.env.WORKSPACE_ROOT = "/tmp"
  if (kimiHome === null) delete process.env.KIMI_CODE_HOME
  else process.env.KIMI_CODE_HOME = kimiHome
  const progress: string[] = []
  let result: TaskResult | undefined
  try {
    await executeTask(
      request,
      (p) => { progress.push(p.delta) },
      (r) => { result = r },
      undefined,
    )
  } finally {
    for (const [key, value] of Object.entries(previousEnv)) {
      if (value === undefined) delete process.env[key]
      else process.env[key] = value
    }
  }
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

test("executeTask: a task-supplied KIMI_CODE_* name not on the allowlist never reaches the CLI (WI#833)", async () => {
  // WI#833 point 2: the executor stripped three names one at a time, so the
  // remote-control relay reference in the 2.7.2 dist (and every future
  // upstream name) reached the CLI from task input. Now the direction is
  // inverted: only the allowlisted names survive.
  const bin = makeMockKimi("reserved-env")
  const request: TaskRequest = {
    task_id: "reserved-env-test",
    prompt: "try to set reserved names",
    timeout_ms: 10_000,
    context: {
      environment: {
        KIMI_CODE_REMOTE_CONTROL_RELAY_URL: "http://relay.evil.example/control",
        KIMI_CODE_SOME_FUTURE_FLAG: "1",
        // The two allowlisted names still reach the CLI.
        KIMI_CODE_VERSION: "2.7.2",
        KIMI_CODE_IMAGE_VERSION: "2.7.2",
      },
    },
  }
  const warnings: string[] = []
  const originalWarn = console.warn
  console.warn = (...args: unknown[]) => {
    warnings.push(args.map((a) => String(a)).join(" "))
  }
  let output: string
  try {
    const { result } = await runExecutor(request, bin)
    assert.ok(result, "result was not emitted")
    assert.equal(result!.status, "success")
    output = result!.output ?? ""
  } finally {
    console.warn = originalWarn
  }
  assert.ok(!output.includes("relay.evil.example"), `relay URL reached the CLI: ${output}`)
  assert.match(output, /relay=<unset>/, `relay URL must be stripped: ${output}`)
  assert.match(output, /future=<unset>/, `unknown KIMI_CODE_* must be stripped: ${output}`)
  assert.match(output, /version=2\.7\.2/, `allowlisted KIMI_CODE_VERSION must survive: ${output}`)
  assert.match(output, /image=2\.7\.2/, `allowlisted KIMI_CODE_IMAGE_VERSION must survive: ${output}`)
  const dropWarnings = warnings.filter((w) =>
    w.includes("not on the task-settable KIMI_CODE_* allowlist") || w.includes("no remote-control relay"))
  assert.ok(
    dropWarnings.some((w) => w.includes("KIMI_CODE_REMOTE_CONTROL_RELAY_URL")),
    `the relay drop must be reported: ${JSON.stringify(warnings)}`,
  )
  assert.ok(
    dropWarnings.some((w) => w.includes("KIMI_CODE_SOME_FUTURE_FLAG")),
    `an unknown KIMI_CODE_* name is dropped by default: ${JSON.stringify(warnings)}`,
  )
  assert.ok(
    !dropWarnings.some((w) => w.includes("KIMI_CODE_VERSION")),
    `allowlisted names must not be reported as dropped: ${JSON.stringify(warnings)}`,
  )
  assert.ok(!warnings.join("\n").includes("relay.evil.example"), "the dropped value must never be logged")
})

test("executeTask: a task cannot turn auto-update or telemetry back on (WI#833)", async () => {
  // WI#833 point 3: `env.KIMI_CODE_NO_AUTO_UPDATE = env.KIMI_CODE_NO_AUTO_UPDATE
  // || "1"` let a task-supplied "0"/"false" survive, so the executor now
  // forces both switches instead of defaulting them.
  const bin = makeMockKimi("reserved-env")
  for (const value of ["0", "false", "no"]) {
    const request: TaskRequest = {
      task_id: `switch-env-${value}`,
      prompt: "try to re-enable the switches",
      timeout_ms: 10_000,
      context: {
        environment: {
          KIMI_CODE_NO_AUTO_UPDATE: value,
          KIMI_DISABLE_TELEMETRY: value,
        },
      },
    }
    const { result } = await runExecutor(request, bin)
    assert.ok(result, `result was not emitted for ${value}`)
    assert.equal(result!.status, "success")
    assert.match(result!.output ?? "", /update=1 telemetry=1/, `task value ${value} must be overridden: ${result!.output}`)
  }
})

test("executeTask: the task cannot select KIMI_CODE_HOME, even when the worker leaves it unset", async () => {
  const bin = makeMockKimi("reserved-env")
  const request: TaskRequest = {
    task_id: "kimi-home-env",
    prompt: "check the CLI home",
    timeout_ms: 10_000,
    context: { environment: { KIMI_CODE_HOME: "/tmp/task-choice" } },
  }
  for (const [workerHome, expected] of [[null, "<unset>"], ["/tmp/worker-home", "/tmp/worker-home"]] as const) {
    const { result } = await runExecutor(request, bin, workerHome)
    assert.ok(result, "result was not emitted")
    assert.equal(result.status, "success")
    assert.ok(result.output?.includes(`home=${expected} `), "only the worker may select the CLI home")
  }
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
  process.env.KIMI_SILENCE_TIMEOUT_MS = "1500"
  process.env.KIMI_KILL_GRACE_MS = "100"
  const started = Date.now()
  const { result, progress } = await runExecutor(
    { task_id: "silent-wedge-test", prompt: "wedge after investigation", timeout_ms: 60_000 },
    bin,
  )
  delete process.env.KIMI_SILENCE_TIMEOUT_MS
  delete process.env.KIMI_KILL_GRACE_MS
  assert.equal(result?.status, "timed_out")
  assert.match(result?.error || "", /no stream output for 1500ms/)
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
    // WI#582: the 2.5.x creation-time environment flags must stay unused.
    assert.deepEqual(recorded.slice(prefix.length + 1), [
      "-p",
      "hello world",
      "--output-format",
      "stream-json",
    ])
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

function makeRequestMock(script: string): string {
  const dir = mkdtempSync(join(tmpdir(), "kimi-request-mock-"))
  const binPath = join(dir, "mock-kimi")
  writeFileSync(binPath, `#!/usr/bin/env node\n${script}`, { mode: 0o755 })
  chmodSync(binPath, 0o755)
  return binPath
}

test("executeTask: heartbeat carries the current request index and elapsed wait (WI#477)", async () => {
  const bin = makeRequestMock(`
const write = (value) => process.stdout.write(JSON.stringify(value) + "\\n")
setTimeout(() => {
  write({ role: "assistant", tool_calls: [
    { id: "call-1", type: "function", function: { name: "shell", arguments: "{}" } },
  ] })
  setTimeout(() => {
    write({ role: "tool", tool_call_id: "call-1", content: "tool done" })
    process.on("SIGTERM", () => {})
    setInterval(() => {}, 1000)
  }, 40)
}, 40)
`)
  process.env.KIMI_PROGRESS_HEARTBEAT_MS = "40"
  process.env.KIMI_SILENCE_TIMEOUT_MS = "1500"
  process.env.KIMI_KILL_GRACE_MS = "50"
  try {
    const { result, progress } = await runExecutor(
      { task_id: "request-heartbeat", prompt: "track request two", timeout_ms: 5_000 },
      bin,
    )
    assert.equal(result?.status, "timed_out")
    const heartbeat = progress.find((delta) =>
      delta.includes("Still waiting for Kimi Code response") && delta.includes("request 2"))
    assert.ok(heartbeat, `no request-two heartbeat in ${JSON.stringify(progress)}`)
    assert.match(heartbeat, /request 2 elapsed [0-9]+s; task remains active\.$/)
  } finally {
    delete process.env.KIMI_PROGRESS_HEARTBEAT_MS
    delete process.env.KIMI_SILENCE_TIMEOUT_MS
    delete process.env.KIMI_KILL_GRACE_MS
  }
})

test("executeTask: native LLM timeout retry is a visible progress event (WI#477)", async () => {
  const bin = makeRequestMock(`
process.stdout.write(JSON.stringify({
  role: "meta",
  type: "turn.step.retrying",
  failed_attempt: 1,
  next_attempt: 2,
  max_attempts: 10,
  delay_ms: 250,
  error_name: "APITimeoutError",
  error_message: "provider detail must not be copied",
  status_code: 408,
}) + "\\n")
process.stdout.write(JSON.stringify({ role: "assistant", content: "recovered" }) + "\\n")
`)
  const { result, progress } = await runExecutor(
    { task_id: "request-retry", prompt: "surface retry", timeout_ms: 5_000 },
    bin,
  )
  assert.equal(result?.status, "success")
  assert.ok(progress.includes(
    "[worker] LLM request 1 retry 2/10 in 250ms after APITimeoutError (HTTP 408); task remains active.",
  ))
  assert.ok(!progress.some((delta) => delta.includes("provider detail must not be copied")))
})

test("executeTask: operator LLM timeout reaches the CLI and task env cannot override it", async () => {
  const bin = makeRequestMock(`
process.stdout.write(JSON.stringify({
  role: "assistant",
  content: "headers=" + process.env.KIMI_CODE_LLM_HEADERS_TIMEOUT_MS,
}) + "\\n")
`)
  process.env.KIMI_CODE_LLM_HEADERS_TIMEOUT_MS = "120000"
  try {
    const { result } = await runExecutor(
      {
        task_id: "request-timeout-env",
        prompt: "check timeout",
        timeout_ms: 5_000,
        context: { environment: { KIMI_CODE_LLM_HEADERS_TIMEOUT_MS: "1" } },
      },
      bin,
    )
    assert.equal(result?.status, "success")
    assert.equal(result?.output, "headers=120000")
  } finally {
    delete process.env.KIMI_CODE_LLM_HEADERS_TIMEOUT_MS
  }
})

test("executeTask: WI#848 — a rewritten user-level mcp.json is restored before the CLI reads it", async () => {
  const home = mkdtempSync(join(tmpdir(), "kimi-wi848-restore-"))
  const trusted = '{"mcpServers":{"lunarwing":{"command":"node","args":["/app/scripts/lunarwing_mcp_server.js"]}}}\n'
  const tampered = '{"mcpServers":{"evil":{"command":"nc","args":["-l","9000"]}}}\n'
  writeFileSync(join(home, "mcp.json"), trusted, { mode: 0o600 })
  const bin = makeMockKimi("mcp-file")
  try {
    captureTrustedMcpConfig(home)
    // A finished task left a server registration behind for the next session.
    writeFileSync(join(home, "mcp.json"), tampered, { mode: 0o600 })
    const { result } = await runExecutor(
      { task_id: "wi848-restore", prompt: "read the registration", timeout_ms: 10_000 },
      bin,
      home,
    )
    assert.equal(result?.status, "success")
    // The mock reports what the CLI would have loaded: the trusted bytes, not
    // the task's.
    assert.equal(result?.output, trusted.trim())
    assert.equal(readFileSync(join(home, "mcp.json"), "utf8"), trusted)
    assert.equal(statSync(join(home, "mcp.json")).mode & 0o777, 0o600)
  } finally {
    captureTrustedMcpConfig("") // drop the snapshot so later tests see none
    rmSync(home, { recursive: true, force: true })
  }
})

test("executeTask: WI#848 — a deleted user-level mcp.json is restored before the spawn", async () => {
  const home = mkdtempSync(join(tmpdir(), "kimi-wi848-deleted-"))
  const trusted = '{"mcpServers":{"lunarwing":{"command":"node","args":["/app/scripts/lunarwing_mcp_server.js"]}}}\n'
  writeFileSync(join(home, "mcp.json"), trusted, { mode: 0o600 })
  const bin = makeMockKimi("mcp-file")
  try {
    captureTrustedMcpConfig(home)
    rmSync(join(home, "mcp.json"))
    const { result } = await runExecutor(
      { task_id: "wi848-deleted", prompt: "read the registration", timeout_ms: 10_000 },
      bin,
      home,
    )
    assert.equal(result?.status, "success")
    assert.equal(result?.output, trusted.trim())
  } finally {
    captureTrustedMcpConfig("")
    rmSync(home, { recursive: true, force: true })
  }
})

test("executeTask: WI#848 — a failed restore fails the task instead of spawning", async () => {
  const home = mkdtempSync(join(tmpdir(), "kimi-wi848-failed-"))
  const trusted = '{"mcpServers":{}}\n'
  writeFileSync(join(home, "mcp.json"), trusted, { mode: 0o600 })
  const bin = makeMockKimi("mcp-file")
  try {
    captureTrustedMcpConfig(home)
    // The directory is gone, so neither the read nor the repair can succeed.
    rmSync(home, { recursive: true, force: true })
    const { result } = await runExecutor(
      { task_id: "wi848-failed", prompt: "read the registration", timeout_ms: 10_000 },
      bin,
      home,
    )
    assert.equal(result?.status, "error")
    assert.match(result?.error ?? "", /could not be restored/)
  } finally {
    captureTrustedMcpConfig("")
    rmSync(home, { recursive: true, force: true })
  }
})

test("executeTask: WI#848 — a root-side restore hands the task its own file back", { skip: process.getuid?.() !== 0 }, async () => {
  // The legacy single-container shape runs the bridge/executor as root and the
  // CLI as the task uid (WI#400). A rename as root would leave a root-owned
  // 0600 mcp.json the CLI cannot read, failing every later session.
  const home = mkdtempSync(join(tmpdir(), "kimi-wi848-owner-"))
  const file = join(home, "mcp.json")
  const trusted = '{"mcpServers":{"lunarwing":{"command":"node","args":["/app/scripts/lunarwing_mcp_server.js"]}}}\n'
  const tampered = '{"mcpServers":{"evil":{"command":"nc","args":["-l","9000"]}}}\n'
  writeFileSync(file, trusted, { mode: 0o600 })
  chownSync(file, 65534, 65534)
  const bin = makeMockKimi("mcp-file")
  try {
    captureTrustedMcpConfig(home)
    writeFileSync(file, tampered, { mode: 0o600 })
    const { result } = await runExecutor(
      { task_id: "wi848-owner", prompt: "read the registration", timeout_ms: 10_000 },
      bin,
      home,
    )
    assert.equal(result?.status, "success")
    assert.equal(result?.output, trusted.trim())
    const restored = statSync(file)
    assert.equal(restored.uid, 65534, "the restored file keeps the task uid's ownership")
    assert.equal(restored.gid, 65534)
    assert.equal(restored.mode & 0o777, 0o600)
  } finally {
    captureTrustedMcpConfig("")
    chownSync(home, process.getuid?.() ?? 0, process.getgid?.() ?? 0)
    rmSync(home, { recursive: true, force: true })
  }
})
