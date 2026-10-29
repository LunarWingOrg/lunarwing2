import assert from "node:assert/strict"
import { mkdtempSync, rmSync, statSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { after, test } from "node:test"
import type { OpencodeClient } from "@opencode-ai/sdk/v2/client"

import { TaskRunner, startupRefusal } from "./task_runner"
import { delegateTaskToRunner, sendTaskPromptToRunner } from "./task_runner_client"
import type { TaskRequest } from "./lunarwing_runtime"

const scratchDirs: string[] = []
function scratch(): string {
  const dir = mkdtempSync(join(tmpdir(), "opencode-runner-"))
  scratchDirs.push(dir)
  return dir
}
after(() => {
  for (const dir of scratchDirs) rmSync(dir, { recursive: true, force: true })
})

function makeSdk(options: { pendingPrompt?: boolean; firstPromptDelayMs?: number } = {}) {
  const prompts: string[] = []
  const sdk = {
    session: {
      create: async () => ({ data: { id: "session-1" } }),
      prompt: async (...args: unknown[]) => {
        const input = args[0] as { parts: Array<{ text: string }> }
        prompts.push(input.parts[0]!.text)
        if (prompts.length === 1 && options.firstPromptDelayMs) {
          await new Promise((resolve) => setTimeout(resolve, options.firstPromptDelayMs))
        }
        if (options.pendingPrompt) await new Promise(() => {})
        return { data: { info: {}, parts: [{ type: "text", text: `done ${prompts.length}` }] } }
      },
      abort: async () => ({ data: true }),
    },
    event: {
      subscribe: async () => ({
        stream: (async function* () {
          if (!options.pendingPrompt) {
            yield { type: "session.idle", properties: { sessionID: "session-1" } }
          } else {
            await new Promise(() => {})
          }
        })(),
      }),
    },
    permission: { reply: async () => ({ data: true }) },
    v2: { session: { permission: { reply: async () => ({ data: undefined }) } } },
  } as unknown as OpencodeClient
  return { sdk, prompts }
}

async function startRunner(sdk: OpencodeClient): Promise<{ runner: TaskRunner; path: string }> {
  const runner = new TaskRunner(sdk)
  const path = await runner.listen(join(scratch(), "runner.sock"))
  assert.ok(path, "runner must listen")
  return { runner, path }
}

function request(overrides: Partial<TaskRequest> = {}): TaskRequest {
  return { task_id: "task-1", prompt: "start", ...overrides }
}

test("runner health probes require a bounded live pong and propagate CLI failure", async () => {
  const { createServer } = await import("node:net")
  const { spawn } = await import("node:child_process")
  const { runnerHealthcheck } = await import("./task_runner")
  for (const [label, reply, expected] of [
    ["pong", '{"ok":true,"op":"pong"}\n', 0],
    ["invalid", 'null\n', 1],
    ["oversized", "x".repeat(2048), 1],
    ["unterminated", '{"ok":true,"op":"pong"}', 1],
    ["silent", null, 1],
  ] as const) {
    const path = join(scratch(), "health.sock")
    const server = createServer((socket) => {
      socket.on("error", () => {})
      socket.once("data", () => { if (reply !== null) socket.end(reply) })
    })
    await new Promise<void>((resolve) => server.listen(path, resolve))
    try {
      assert.equal(await runnerHealthcheck(path), expected, label)
      if (label === "pong" || label === "invalid") {
        const child = spawn(process.execPath, [new URL("./task_runner.ts", import.meta.url).pathname, "--healthcheck", path], { stdio: "ignore" })
        assert.equal(await new Promise((resolve, reject) => {
          child.once("error", reject)
          child.once("exit", resolve)
        }), expected, `${label} CLI exit status`)
      }
    } finally {
      await new Promise<void>((resolve) => server.close(() => resolve()))
    }
  }
})

test("runner refuses credentials before its uid gate", () => {
  const refusal = startupRefusal({
    listenFd: null,
    socketPath: "/tmp/runner.sock",
    env: { AGENT_AUTH_TOKEN: "forbidden" },
    uid: 0,
  })
  assert.match(String(refusal), /hub credential/)
})

test("delegated run and follow-up reach the same OpenCode session", async () => {
  process.env.LUNARWING_TASK_RUNNER_REMOTE = "1"
  const { sdk, prompts } = makeSdk({ firstPromptDelayMs: 500 })
  const { runner, path } = await startRunner(sdk)
  try {
    const pending = delegateTaskToRunner(path, request(), undefined, () => {})
    let forwarded = false
    for (let attempt = 0; attempt < 100 && !forwarded; attempt++) {
      forwarded = sendTaskPromptToRunner({ task_id: "task-1", seq: 1, prompt: "follow up", done: true })
      if (!forwarded) await new Promise((resolve) => setTimeout(resolve, 10))
    }
    assert.equal(forwarded, true, "runner connection never accepted follow-up forwarding")
    const result = await pending
    assert.equal(result.status, "success", result.error ?? "")
    assert.equal(result.output, "done 1\n\ndone 2")
    assert.equal(prompts.length, 2)
    assert.match(prompts[0]!, /start/)
    assert.match(prompts[1]!, /follow up/)
  } finally {
    runner.close()
    delete process.env.LUNARWING_TASK_RUNNER_REMOTE
  }
})

test("delegated cancellation settles with a terminal cancelled result", async () => {
  const { sdk } = makeSdk({ pendingPrompt: true })
  const { runner, path } = await startRunner(sdk)
  const controller = new AbortController()
  try {
    const pending = delegateTaskToRunner(path, request({ task_id: "cancel-me" }), undefined, () => {}, controller.signal)
    setTimeout(() => controller.abort(), 100)
    const result = await pending
    assert.equal(result.status, "cancelled")
    assert.equal(result.task_id, "cancel-me")
  } finally {
    runner.close()
  }
})

test("cron workspaces are validated by the bridge and created by the runner", async () => {
  const { existsSync } = await import("node:fs")
  const { resolveTaskWorkspace } = await import("./workspace_path")
  const root = scratch()
  const previousRoot = process.env.WORKSPACE_ROOT
  process.env.WORKSPACE_ROOT = root
  const { runner, path } = await startRunner(makeSdk().sdk)
  const context = { project_dir: "scheduled/repository", cron_tick: { event: "cron.tick" } }
  try {
    assert.equal(resolveTaskWorkspace(context, root, undefined, false), join(root, context.project_dir))
    assert.equal(existsSync(join(root, "scheduled")), false)
    const result = await delegateTaskToRunner(path, request({ context }), undefined, () => {})
    assert.equal(result.status, "success", result.error ?? "")
    assert.equal(statSync(join(root, context.project_dir)).uid, process.getuid?.())
    const invalid = await delegateTaskToRunner(path, request({
      task_id: "invalid-cron", context: { ...context, project_dir: "../escape" },
    }), undefined, () => {})
    assert.equal(invalid.status, "error")
    assert.match(invalid.error ?? "", /Invalid workspace path for cron tick/)
  } finally {
    runner.close()
    if (previousRoot === undefined) delete process.env.WORKSPACE_ROOT
    else process.env.WORKSPACE_ROOT = previousRoot
  }
})

test("runner clients reject malformed frames and keep control refusals non-terminal", async () => {
  const { createServer } = await import("node:net")
  const { delegateTaskToRunner: delegateV2 } = await import("../../opencode2.4lunarwing/scripts/task_runner_client")
  const accepted = { ok: true, op: "accepted", task_id: "task-1" }
  const result = { op: "result", result: { task_id: "task-1", status: "success", output: "correct", duration_ms: 1 } }
  for (const delegate of [delegateTaskToRunner, delegateV2]) {
    for (const [frames, error] of [
      [[accepted, { op: "prompt_refused", ok: false, error: "queue full" }, result], null],
      [[accepted, { ...result, result: { ...result.result, task_id: "foreign" } }, result], null],
      [[accepted, { op: "progress", progress: { task_id: "task-1", delta: "x", done: "wrong" } }], /malformed progress/],
      [[accepted, null], /non-object/],
      [[accepted], /closed without a result/],
    ] as const) {
      const path = join(scratch(), "peer.sock")
      const server = createServer((socket) => {
        socket.on("error", () => {})
        socket.once("data", () => socket.end(frames.map(frame => JSON.stringify(frame)).join("\n") + "\n"))
      })
      await new Promise<void>(resolve => server.listen(path, resolve))
      try {
        const pending = delegate(path, request(), undefined, () => {})
        if (error) await assert.rejects(pending, error)
        else assert.equal((await pending).output, "correct")
      } finally {
        await new Promise<void>(resolve => server.close(() => resolve()))
      }
    }
    await assert.rejects(delegate("/missing.sock", request(), { FORBIDDEN: "grant" }, () => {}), /does not accept extra environment/)
  }
})

test("shared runner sockets allow bridge-group access", async () => {
  process.env.LUNARWING_TASK_RUNNER_REMOTE = "1"
  const { runner, path } = await startRunner(makeSdk().sdk)
  try {
    assert.equal(statSync(path).mode & 0o777, 0o660)
  } finally {
    runner.close()
    delete process.env.LUNARWING_TASK_RUNNER_REMOTE
  }
})
