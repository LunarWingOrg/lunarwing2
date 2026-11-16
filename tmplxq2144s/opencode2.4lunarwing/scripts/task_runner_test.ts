import assert from "node:assert/strict"
import { mkdtempSync, rmSync, statSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { after, test } from "node:test"
import type { OpenCodeClient } from "@opencode/client/promise"

import { TaskRunner, startupRefusal } from "./task_runner"
import { delegateTaskToRunner } from "./task_runner_client"
import type { TaskRequest } from "./lunarwing_runtime"

const scratchDirs: string[] = []
function scratch(): string {
  const dir = mkdtempSync(join(tmpdir(), "opencode2-runner-"))
  scratchDirs.push(dir)
  return dir
}
const initialWorkspaceRoot = process.env.WORKSPACE_ROOT
const testWorkspaceRoot = scratch()
process.env.WORKSPACE_ROOT = testWorkspaceRoot
after(() => {
  if (initialWorkspaceRoot === undefined) delete process.env.WORKSPACE_ROOT
  else process.env.WORKSPACE_ROOT = initialWorkspaceRoot
  for (const dir of scratchDirs) rmSync(dir, { recursive: true, force: true })
})

function makeSdk(options: { pending?: boolean } = {}): OpenCodeClient {
  const messages = [{ type: "assistant", content: [{ type: "text", text: " delegated output " }] }]
  return {
    session: {
      create: async () => ({ id: "session-1" }),
      prompt: async () => {
        if (options.pending) await new Promise(() => {})
        return { id: "inbox-1", sessionID: "session-1", type: "user" }
      },
      interrupt: async () => ({ data: true }),
    },
    permission: { reply: async () => ({ data: true }) },
    message: {
      list: async () => ({ data: options.pending ? [] : messages, cursor: {} }),
    },
    event: {
      subscribe: () => (async function* () {
        if (options.pending) {
          await new Promise(() => {})
        } else {
          yield { type: "session.execution.succeeded", data: { sessionID: "session-1" } }
        }
      })(),
    },
  } as unknown as OpenCodeClient
}

async function startRunner(sdk: OpenCodeClient): Promise<{ runner: TaskRunner; path: string }> {
  const runner = new TaskRunner(sdk)
  const path = await runner.listen(join(scratch(), "runner.sock"))
  assert.ok(path, "runner must listen")
  return { runner, path }
}

function request(overrides: Partial<TaskRequest> = {}): TaskRequest {
  return { task_id: "task-1", prompt: "start", context: { project_dir: testWorkspaceRoot }, ...overrides }
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
    env: { AGENT_AUTH_TOKEN_FILE: "/forbidden" },
    uid: 0,
  })
  assert.match(String(refusal), /hub credential/)
})

test("delegated execution returns canonical assistant output", async () => {
  const { runner, path } = await startRunner(makeSdk())
  try {
    const result = await delegateTaskToRunner(path, request(), undefined, () => {})
    assert.equal(result.status, "success", result.error ?? "")
    assert.equal(result.output, "delegated output")
  } finally {
    runner.close()
  }
})

test("delegated cancellation settles with a terminal cancelled result", async () => {
  const { runner, path } = await startRunner(makeSdk({ pending: true }))
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
  const { runner, path } = await startRunner(makeSdk())
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

test("runner clients reject malformed frames and ignore foreign task ids", async () => {
  const { createServer } = await import("node:net")
  const accepted = { ok: true, op: "accepted", task_id: "task-1" }
  const result = { op: "result", result: { task_id: "task-1", status: "success", output: "correct", duration_ms: 1 } }
  for (const [frames, error] of [
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
      const pending = delegateTaskToRunner(path, request(), undefined, () => {})
      if (error) await assert.rejects(pending, error)
      else assert.equal((await pending).output, "correct")
    } finally {
      await new Promise<void>(resolve => server.close(() => resolve()))
    }
  }
  await assert.rejects(delegateTaskToRunner("/missing.sock", request(), { FORBIDDEN: "grant" }, () => {}), /does not accept extra environment/)
})

test("shared runner sockets allow bridge-group access", async () => {
  process.env.LUNARWING_TASK_RUNNER_REMOTE = "1"
  const { runner, path } = await startRunner(makeSdk())
  try {
    assert.equal(statSync(path).mode & 0o777, 0o660)
  } finally {
    runner.close()
    delete process.env.LUNARWING_TASK_RUNNER_REMOTE
  }
})
