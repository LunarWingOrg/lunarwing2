import assert from "node:assert/strict"
import { mkdtempSync, rmSync, unlinkSync } from "node:fs"
import { createServer, type AddressInfo } from "node:net"
import { tmpdir } from "node:os"
import { join } from "node:path"
import test from "node:test"
import { WebSocketServer } from "ws"

import { delegateTaskToRunner } from "./task_runner_client.js"
import { createEnvelope, MAX_FRAME_BYTES } from "./lunarwing_runtime.js"
import { startupRefusal, TaskRunner } from "./lunardepth_task_runner.js"

const TOKEN = "test-only-local-token-0123456789abcdef0123456789abcdef"

function testEnvironment(workspace: string): { root: string; socket: string; restore: () => void } {
  const root = mkdtempSync(join(tmpdir(), "lunardepth-runner-test-"))
  const previous = { ...process.env }
  process.env.LUNARDEPTH_LOCAL_AUTH_TOKEN = TOKEN
  delete process.env.LUNARDEPTH_LOCAL_AUTH_TOKEN_FILE
  process.env.LUNARDEPTH_TASK_UID = String(process.geteuid?.())
  process.env.LUNARDEPTH_TASK_GID = String(process.getegid?.())
  process.env.WORKSPACE_ROOT = workspace
  return {
    root,
    socket: join(root, "task-runner.sock"),
    restore: () => {
      process.env = previous
      rmSync(root, { recursive: true, force: true })
    },
  }
}

async function upstream(onTask: (socket: import("ws").WebSocket, payload: Record<string, unknown>) => void) {
  const server = new WebSocketServer({ host: "127.0.0.1", port: 0, path: "/ws/agent" })
  await new Promise<void>(resolve => server.once("listening", resolve))
  const requests: Record<string, unknown>[] = []
  server.on("connection", (socket, request) => {
    assert.equal(request.headers.authorization, `Bearer ${TOKEN}`)
    socket.send(JSON.stringify(createEnvelope("ready", { version: "1", capabilities: [] })))
    socket.on("message", data => {
      const envelope = JSON.parse(data.toString()) as { type: string; payload: Record<string, unknown> }
      if (envelope.type === "task_request") {
        requests.push(envelope.payload)
        onTask(socket, envelope.payload)
      } else if (envelope.type === "cancel") {
        socket.send(JSON.stringify(createEnvelope("task_result", {
          task_id: envelope.payload.task_id,
          status: "cancelled",
          output: "partial",
          error: "Task cancelled",
          duration_ms: 2,
        })))
      }
    })
  })
  const address = server.address() as AddressInfo
  return { server, requests, url: `ws://127.0.0.1:${address.port}/ws/agent` }
}

function success(socket: import("ws").WebSocket, payload: Record<string, unknown>): void {
  socket.send(JSON.stringify(createEnvelope("task_progress", { task_id: payload.task_id, delta: "working", done: false })))
  socket.send(JSON.stringify(createEnvelope("task_result", {
    task_id: payload.task_id,
    status: "success",
    output: "finished",
    error: null,
    duration_ms: 3,
  })))
}

test("startup refuses hub credentials, root, and a missing local token", () => {
  assert.match(startupRefusal({ AGENT_AUTH_TOKEN: "leaked" }, 10005) ?? "", /hub credentials/)
  assert.match(startupRefusal({ LUNARDEPTH_LOCAL_AUTH_TOKEN: TOKEN }, 0) ?? "", /must not run as root/)
  assert.match(startupRefusal({}, 10005) ?? "", /exactly one/)
  assert.equal(startupRefusal({ LUNARDEPTH_LOCAL_AUTH_TOKEN: TOKEN }, 10005), null)
})

test("delegates a real socket task with supported fields and one terminal result", async () => {
  const workspace = mkdtempSync(join(tmpdir(), "lunardepth-workspace-"))
  const fixture = testEnvironment(workspace)
  const fake = await upstream(success)
  process.env.LUNARDEPTH_WS_URL = fake.url
  const runner = new TaskRunner(workspace)
  await runner.listen(fixture.socket)
  const progress: string[] = []
  try {
    const result = await delegateTaskToRunner(fixture.socket, {
      task_id: "job-success",
      prompt: "run",
      timeout_ms: 1000,
      context: { project_dir: workspace, conversation_history: [{ role: "user", content: "before" }] },
    }, value => progress.push(value.delta), new AbortController().signal)
    assert.equal(result.status, "success")
    assert.equal(result.output, "finished")
    assert.deepEqual(progress, ["working"])
    assert.equal(fake.requests.length, 1)
    assert.equal(fake.requests[0]?.task_id, "job-success")
    assert.equal(fake.requests[0]?.timeout_ms, 1000)
    assert.deepEqual(fake.requests[0]?.context, {
      project_dir: workspace,
      conversation_history: [{ role: "user", content: "before" }],
    })
    assert.equal(await (await import("./lunardepth_task_runner.js")).runnerHealthcheck(fixture.socket), 0)
  } finally {
    runner.close()
    fake.server.close()
    fixture.restore()
    rmSync(workspace, { recursive: true, force: true })
  }
})

test("forwards cancellation and preserves upstream partial output", async () => {
  const workspace = mkdtempSync(join(tmpdir(), "lunardepth-workspace-"))
  const fixture = testEnvironment(workspace)
  const fake = await upstream((socket, payload) => {
    socket.send(JSON.stringify(createEnvelope("task_progress", { task_id: payload.task_id, delta: "partial", done: false })))
  })
  process.env.LUNARDEPTH_WS_URL = fake.url
  const runner = new TaskRunner(workspace)
  await runner.listen(fixture.socket)
  const controller = new AbortController()
  try {
    const pending = delegateTaskToRunner(fixture.socket,
      { task_id: "job-cancel", prompt: "wait", timeout_ms: 60_000 }, () => controller.abort(), controller.signal)
    const result = await pending
    assert.equal(result.status, "cancelled")
    assert.equal(result.output, "partial")
  } finally {
    runner.close()
    fake.server.close()
    fixture.restore()
    rmSync(workspace, { recursive: true, force: true })
  }
})

test("turns an upstream disconnect into an explicit error", async () => {
  const workspace = mkdtempSync(join(tmpdir(), "lunardepth-workspace-"))
  const fixture = testEnvironment(workspace)
  const fake = await upstream(socket => socket.close())
  process.env.LUNARDEPTH_WS_URL = fake.url
  const runner = new TaskRunner(workspace)
  await runner.listen(fixture.socket)
  try {
    const result = await delegateTaskToRunner(fixture.socket,
      { task_id: "job-disconnect", prompt: "run", timeout_ms: 1000 }, () => undefined, new AbortController().signal)
    assert.equal(result.status, "error")
    assert.match(result.error ?? "", /disconnected|closed/)
  } finally {
    runner.close()
    fake.server.close()
    fixture.restore()
    rmSync(workspace, { recursive: true, force: true })
  }
})

test("rejects a runner socket that disappears during connect validation", async () => {
  const workspace = mkdtempSync(join(tmpdir(), "lunardepth-workspace-"))
  const fixture = testEnvironment(workspace)
  const server = createServer(connection => {
    unlinkSync(fixture.socket)
    connection.destroy()
  })
  await new Promise<void>(resolve => server.listen(fixture.socket, () => resolve()))
  try {
    await assert.rejects(delegateTaskToRunner(fixture.socket,
      { task_id: "job-vanished-socket", prompt: "run", timeout_ms: 1000 },
      () => undefined, new AbortController().signal), /ENOENT|task runner path/)
  } finally {
    await new Promise<void>(resolve => server.close(() => resolve()))
    fixture.restore()
    rmSync(workspace, { recursive: true, force: true })
  }
})

test("rejects an oversized serialized upstream task envelope", async () => {
  const workspace = mkdtempSync(join(tmpdir(), "lunardepth-workspace-"))
  const fixture = testEnvironment(workspace)
  const fake = await upstream(() => undefined)
  process.env.LUNARDEPTH_WS_URL = fake.url
  const runner = new TaskRunner(workspace)
  await runner.listen(fixture.socket)
  try {
    const result = await delegateTaskToRunner(fixture.socket, {
      task_id: "job-oversized-envelope",
      prompt: "x".repeat(MAX_FRAME_BYTES),
      timeout_ms: 1000,
    }, () => undefined, new AbortController().signal)
    assert.equal(result.status, "error")
    assert.match(result.error ?? "", /outgoing envelope exceeds the 16 MiB frame limit/)
    assert.equal(fake.requests.length, 0)
  } finally {
    runner.close()
    fake.server.close()
    fixture.restore()
    rmSync(workspace, { recursive: true, force: true })
  }
})
