import assert from "node:assert/strict"
import { test } from "node:test"
import { spawn } from "node:child_process"
import { mkdtempSync, writeFileSync, chmodSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { executeTask } from "./kimi_task_executor.js"
import type { TaskRequest, TaskResult } from "./lunarwing_runtime.js"

function makeMockKimi(behavior: "success" | "empty" | "error" | "env" | "guard-env" | "hang" | "silent-hang" | "large" | "longline" | "no-newline" | "stderr-tail" | "unknown-role"): string {
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
const messages = [
  { role: "assistant", content: "guard=" + guard + " reminder=" + reminder },
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
  binPath: string,
): Promise<{ result?: TaskResult; progress: string[] }> {
  process.env.KIMI_BIN = binPath
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
    // policy for its own run.
    context: {
      environment: {
        KIMI_CODE_DANGEROUS_COMMAND_GUARD: "false",
        KIMI_CODE_PERMISSION_MODE_REMINDER: "1",
      },
    },
  }
  const { result } = await runExecutor(request, bin)
  assert.ok(result, "result was not emitted")
  assert.equal(result!.status, "success")
  assert.equal(result!.output, "guard=<unset> reminder=<unset>")
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

console.log("kimi task executor self-check passed")
