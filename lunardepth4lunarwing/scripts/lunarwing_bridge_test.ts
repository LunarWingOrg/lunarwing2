import assert from "node:assert/strict"
import { mkdirSync, mkdtempSync, rmSync } from "node:fs"
import { connect, createServer, type Server } from "node:net"
import { tmpdir } from "node:os"
import { join } from "node:path"
import test from "node:test"
import WebSocket from "ws"

import { BridgeServer, bridgeStartupRefusal } from "./lunarwing_bridge.js"
import { createEnvelope, type Envelope, SUBPROTOCOL } from "./lunarwing_runtime.js"

const TOKEN = "test-only-hub-token-0123456789abcdef0123456789abcdef"

async function rawUpgrade(port: number, path: string, authorization: string): Promise<string> {
  return new Promise<string>((resolve, reject) => {
    const socket = connect({ host: "127.0.0.1", port })
    let response = ""
    socket.setTimeout(2_000, () => {
      socket.destroy()
      reject(new Error("timed out waiting for raw upgrade response"))
    })
    socket.on("connect", () => socket.write(
      `GET ${path} HTTP/1.1\r\nHost: 127.0.0.1:${port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n` +
      `Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n` +
      `Sec-WebSocket-Protocol: ${SUBPROTOCOL}\r\nAuthorization: ${authorization}\r\n\r\n`,
    ))
    socket.on("data", data => {
      response += data.toString()
      if (response.includes("\r\n\r\n")) {
        resolve(response)
        socket.destroy()
      }
    })
    socket.on("end", () => resolve(response))
    socket.on("error", reject)
  })
}

interface Fixture {
  bridge: BridgeServer
  runner: Server
  root: string
  url: string
  requests: Record<string, unknown>[]
}

async function fixture(): Promise<Fixture> {
  const root = mkdtempSync(join(tmpdir(), "lunardepth-bridge-test-"))
  mkdirSync(join(root, "project"))
  const runnerSocket = join(root, "runner.sock")
  const requests: Record<string, unknown>[] = []
  const runner = createServer(connection => {
    let buffer = ""
    connection.on("data", data => {
      buffer += data.toString()
      const newline = buffer.indexOf("\n")
      if (newline < 0) return
      const line = buffer.slice(0, newline)
      buffer = ""
      const message = JSON.parse(line) as { op: string; request: Record<string, unknown> }
      if (message.op !== "run") return
      requests.push(message.request)
      connection.write(`${JSON.stringify({ ok: true, op: "accepted", task_id: message.request.task_id })}\n`)
      connection.write(`${JSON.stringify({ op: "progress", progress: { task_id: message.request.task_id, delta: "working", done: false } })}\n`)
      connection.write(`${JSON.stringify({ op: "result", result: {
        task_id: message.request.task_id,
        status: "success",
        output: "done",
        error: null,
        duration_ms: 5,
      } })}\n`)
      connection.end()
    })
  })
  await new Promise<void>(resolve => runner.listen(runnerSocket, resolve))
  process.env.LUNARDEPTH_TASK_UID = String(process.geteuid?.())
  process.env.LUNARDEPTH_TASK_GID = String(process.getegid?.())
  process.env.LUNARWING_WORKER_ID = "test-lunardepth"
  const bridge = new BridgeServer({
    token: TOKEN,
    host: "127.0.0.1",
    port: 0,
    path: "/ws/agent",
    workspaceRoot: root,
    hostWorkspaceRoot: "/srv/daemon-workspace",
    runnerSocket,
    stateFile: join(root, "bridge-state.json"),
    workerId: "test-lunardepth",
  })
  await bridge.listen()
  return { bridge, runner, root, url: `ws://127.0.0.1:${bridge.port}/ws/agent`, requests }
}

async function waitFor(messages: Envelope[], type: string, skip = 0): Promise<Envelope> {
  const deadline = Date.now() + 2_000
  while (Date.now() < deadline) {
    const found = messages.find((message, index) => index >= skip && message.type === type)
    if (found !== undefined) return found
    await new Promise(resolve => setTimeout(resolve, 5))
  }
  throw new Error(`timed out waiting for ${type}`)
}

test("bridge startup rejects leaked task and model credentials", () => {
  assert.match(bridgeStartupRefusal({ AGENT_AUTH_TOKEN: TOKEN, LUNARDEPTH_API_KEY: "leaked" }, 10105) ?? "", /model credential/)
  assert.match(bridgeStartupRefusal({ AGENT_AUTH_TOKEN: TOKEN, LUNARDEPTH_LOCAL_AUTH_TOKEN: "leaked" }, 10105) ?? "", /task-local/)
  assert.match(bridgeStartupRefusal({ AGENT_AUTH_TOKEN: TOKEN }, 0) ?? "", /must not run as root/)
  assert.equal(bridgeStartupRefusal({ AGENT_AUTH_TOKEN: TOKEN }, 10105), null)
})

test("requires bearer authentication before the WebSocket upgrade", async () => {
  const current = await fixture()
  try {
    const socket = new WebSocket(current.url, SUBPROTOCOL, { headers: { Authorization: "Bearer wrong" } })
    socket.on("error", () => undefined)
    const response = await new Promise<number>(resolve => {
      socket.once("unexpected-response", (_request, incoming) => resolve(incoming.statusCode ?? 0))
    })
    assert.equal(response, 401)
    socket.terminate()
  } finally {
    await current.bridge.close()
    current.runner.close()
    rmSync(current.root, { recursive: true, force: true })
  }
})

test("malformed upgrade targets are rejected without stopping the bridge", async () => {
  const current = await fixture()
  try {
    assert.match(await rawUpgrade(current.bridge.port, "//", `Bearer ${TOKEN}`), /^HTTP\/1\.1 400/)
    assert.match(await rawUpgrade(current.bridge.port, "//", "Bearer wrong"), /^HTTP\/1\.1 401/)
    const socket = new WebSocket(current.url, SUBPROTOCOL, { headers: { Authorization: `Bearer ${TOKEN}` } })
    const messages: Envelope[] = []
    socket.on("message", data => messages.push(JSON.parse(data.toString()) as Envelope))
    await new Promise<void>(resolve => socket.once("open", resolve))
    socket.send(JSON.stringify(createEnvelope("ping", {})))
    await waitFor(messages, "pong")
    socket.close()
  } finally {
    await current.bridge.close()
    current.runner.close()
    rmSync(current.root, { recursive: true, force: true })
  }
})

test("serves the daemon context and rejects grants without disconnecting", async () => {
  const current = await fixture()
  const socket = new WebSocket(current.url, SUBPROTOCOL, { headers: { Authorization: `Bearer ${TOKEN}` } })
  const messages: Envelope[] = []
  socket.on("message", data => messages.push(JSON.parse(data.toString()) as Envelope))
  try {
    await new Promise<void>(resolve => socket.once("open", resolve))
    const ready = await waitFor(messages, "ready")
    assert.equal(ready.payload.worker_id, "test-lunardepth")
    assert.equal(ready.payload.version, "lunardepth-worker-1.0.0")
    assert.deepEqual(ready.payload.capabilities, [])
    socket.send(JSON.stringify(createEnvelope("ping", {})))
    await waitFor(messages, "pong")
    socket.send(JSON.stringify(createEnvelope("task_request", {
      task_id: "hub-job",
      prompt: "run",
      timeout_ms: 1000,
      context: {
        project_dir: "/srv/daemon-workspace/project",
        conversation_history: [],
        environment: {},
        user_id: "daemon-user",
        metadata: {},
      },
    })))
    const result = await waitFor(messages, "task_result")
    assert.equal(result.payload.status, "success")
    assert.equal(result.payload.output, "done")
    assert.equal(current.requests.length, 1)
    assert.equal(current.requests[0]?.context && typeof current.requests[0].context === "object", true)
    const context = current.requests[0]?.context as { project_dir?: string }
    assert.equal(context.project_dir, join(current.root, "project"))
    const rejectedIndex = messages.length
    socket.send(JSON.stringify(createEnvelope("task_request", {
      task_id: "grant-job",
      prompt: "run",
      context: { environment: { API_KEY: "must-not-be-forwarded" } },
    })))
    const rejected = await waitFor(messages, "task_result", rejectedIndex)
    assert.equal(rejected.payload.task_id, "grant-job")
    assert.equal(rejected.payload.status, "error")
    assert.match(String(rejected.payload.error), /environment and credential grants/)
    const pongIndex = messages.length
    socket.send(JSON.stringify(createEnvelope("ping", {})))
    await waitFor(messages, "pong", pongIndex)
  } finally {
    socket.close()
    await current.bridge.close()
    current.runner.close()
    rmSync(current.root, { recursive: true, force: true })
  }
})
