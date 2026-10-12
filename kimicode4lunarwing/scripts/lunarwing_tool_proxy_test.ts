/**
 * lunarwing_tool_proxy_test.ts — unit tests for the WI#324 bridge-side relay.
 *
 * Drives a real per-task unix socket with a fake hub socket, asserting the
 * tool_call/tool_list frames the relay emits and the IPC replies the shim
 * receives once hub tool_result/tool_catalog frames are routed back.
 */

import assert from "node:assert/strict"
import { mkdtempSync, rmSync } from "node:fs"
import { connect, type Socket } from "node:net"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { after, before, test } from "node:test"

import { createEnvelope } from "./lunarwing_runtime.js"
import {
  proxyEnvForTask,
  taskSocketPath,
  TOOL_CALL_FRAME_CAP_BYTES,
  ToolProxyRelay,
} from "./lunarwing_tool_proxy.js"

class FakeHubSocket {
  readonly sent: string[] = []
  send(data: string): void {
    this.sent.push(data)
  }
}

function ipcRequest(socketPath: string, request: Record<string, unknown>): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    const conn: Socket = connect(socketPath)
    let buffer = ""
    conn.on("connect", () => conn.write(JSON.stringify(request) + "\n"))
    conn.on("data", (chunk) => {
      buffer += chunk.toString("utf8")
      const newline = buffer.indexOf("\n")
      if (newline < 0) return
      conn.destroy()
      try {
        resolve(JSON.parse(buffer.slice(0, newline)) as Record<string, unknown>)
      } catch (err) {
        reject(err instanceof Error ? err : new Error(String(err)))
      }
    })
    conn.on("error", reject)
    setTimeout(() => {
      conn.destroy()
      reject(new Error("ipc request timed out"))
    }, 10_000).unref()
  })
}

let workDir = ""
let relay: ToolProxyRelay

before(() => {
  workDir = mkdtempSync(join(tmpdir(), "tool-proxy-test-"))
  process.env.LUNARWING_TOOL_PROXY_DIR = workDir
  process.env.LUNARWING_TOOL_PROXY_TIMEOUT_MS = "2000"
  relay = new ToolProxyRelay()
})

after(() => {
  rmSync(workDir, { recursive: true, force: true })
  delete process.env.LUNARWING_TOOL_PROXY_DIR
  delete process.env.LUNARWING_TOOL_PROXY_TIMEOUT_MS
})

test("call emits a hub-bound tool_call frame and routes the reply by call_id", async () => {
  const hub = new FakeHubSocket()
  const handle = await relay.startTask("task-1", hub)
  assert.ok(handle, "task proxy should start")

  const responsePromise = ipcRequest(handle.socketPath, {
    op: "call",
    call_id: "call-abc",
    name: "memory_read",
    arguments: { path: "notes/x.md" },
  })
  // Let the relay process the request, then inspect the WS frame it sent.
  await new Promise((resolve) => setTimeout(resolve, 100))
  assert.equal(hub.sent.length, 1)
  const frame = JSON.parse(hub.sent[0]) as { type: string; payload: Record<string, unknown> }
  assert.equal(frame.type, "tool_call")
  assert.equal(frame.payload.task_id, "task-1")
  assert.equal(frame.payload.call_id, "call-abc")
  assert.equal(frame.payload.name, "memory_read")
  assert.deepEqual(frame.payload.arguments, { path: "notes/x.md" })

  // The hub answers on the same socket; the shim's pending request resolves.
  const answered = relay.handleHubFrame(
    hub,
    createEnvelope("tool_result", {
      call_id: "call-abc",
      status: "success",
      output: "file contents",
      error: null,
    }),
  )
  assert.equal(answered, true)
  const response = await responsePromise
  assert.equal(response.ok, true)
  assert.equal(response.status, "success")
  assert.equal(response.output, "file contents")

  // Late replies for unknown call ids are not routed.
  assert.equal(
    relay.handleHubFrame(hub, createEnvelope("tool_result", { call_id: "nope", status: "error" })),
    false,
  )
  handle.close()
  assert.equal(relay.activeTaskCount, 0)
})

test("list sends tool_list and resolves on tool_catalog", async () => {
  const hub = new FakeHubSocket()
  const handle = await relay.startTask("task-2", hub)
  assert.ok(handle)

  const responsePromise = ipcRequest(handle.socketPath, { op: "list" })
  await new Promise((resolve) => setTimeout(resolve, 100))
  assert.equal(hub.sent.length, 1)
  const frame = JSON.parse(hub.sent[0]) as { type: string; payload: Record<string, unknown> }
  assert.equal(frame.type, "tool_list")
  assert.equal(frame.payload.task_id, "task-2")

  relay.handleHubFrame(
    hub,
    createEnvelope("tool_catalog", {
      tools: [{ name: "memory_read", description: "read", parameters: { type: "object" } }],
    }),
  )
  const response = await responsePromise
  assert.equal(response.ok, true)
  const tools = response.tools as Array<{ name: string }>
  assert.equal(tools.length, 1)
  assert.equal(tools[0].name, "memory_read")
  handle.close()
})

test("invalid identifiers and oversized frames are rejected locally", async () => {
  const hub = new FakeHubSocket()
  const handle = await relay.startTask("task-3", hub)
  assert.ok(handle)

  const badName = await ipcRequest(handle.socketPath, {
    op: "call",
    call_id: "c1",
    name: "memory read",
    arguments: {},
  })
  assert.equal(badName.ok, false)
  assert.match(String(badName.error), /name/)

  const badCallId = await ipcRequest(handle.socketPath, {
    op: "call",
    call_id: "has spaces",
    name: "memory_read",
    arguments: {},
  })
  assert.equal(badCallId.ok, false)

  const oversized = await ipcRequest(handle.socketPath, {
    op: "call",
    call_id: "c2",
    name: "memory_write",
    arguments: { content: "x".repeat(TOOL_CALL_FRAME_CAP_BYTES) },
  })
  assert.equal(oversized.ok, false)
  assert.match(String(oversized.error), /256|budget|exceeds/)

  // Nothing reached the hub for any of these.
  assert.equal(hub.sent.length, 0)

  const notJson = await ipcRequest(handle.socketPath, {})
  // op missing -> explicit error, still no hub traffic
  assert.equal(notJson.ok, false)
  assert.equal(hub.sent.length, 0)
  handle.close()
})

test("hub-frame routing is scoped to the owning socket", async () => {
  const hubA = new FakeHubSocket()
  const hubB = new FakeHubSocket()
  const handle = await relay.startTask("task-4", hubA)
  assert.ok(handle)

  const responsePromise = ipcRequest(handle.socketPath, {
    op: "call",
    call_id: "call-scoped",
    name: "memory_read",
    arguments: {},
  })
  await new Promise((resolve) => setTimeout(resolve, 100))
  assert.equal(hubA.sent.length, 1)

  // A reply arriving on the WRONG hub socket must not resolve the call.
  const routed = relay.handleHubFrame(
    hubB,
    createEnvelope("tool_result", { call_id: "call-scoped", status: "success", output: "x" }),
  )
  assert.equal(routed, false)
  relay.handleHubFrame(
    hubA,
    createEnvelope("tool_result", { call_id: "call-scoped", status: "success", output: "right" }),
  )
  const response = await responsePromise
  assert.equal(response.output, "right")
  handle.close()
})

test("closing a task fails its in-flight calls", async () => {
  const hub = new FakeHubSocket()
  const handle = await relay.startTask("task-5", hub)
  assert.ok(handle)

  const responsePromise = ipcRequest(handle.socketPath, {
    op: "call",
    call_id: "call-pending",
    name: "memory_read",
    arguments: {},
  })
  await new Promise((resolve) => setTimeout(resolve, 100))
  assert.equal(hub.sent.length, 1)
  handle.close()
  const response = await responsePromise
  assert.equal(response.ok, true)
  // The relay synthesizes an error payload so the shim never hangs.
  assert.equal(typeof response.error, "string")
})

test("socket path is filesystem-safe and unique per task id", () => {
  const a = taskSocketPath("../../../etc/passwd")
  const b = taskSocketPath("task/../..\nforge")
  assert.ok(a.startsWith(workDir))
  assert.ok(b.startsWith(workDir))
  assert.notEqual(a, b)
  assert.ok(!a.includes(".."))
  assert.ok(!b.includes("\n"))
})

test("proxyEnvForTask carries the binding the shim reads", () => {
  const env = proxyEnvForTask("task-9", "/tmp/x.sock")
  assert.equal(env.LUNARWING_TASK_ID, "task-9")
  assert.equal(env.LUNARWING_TOOL_PROXY_SOCKET, "/tmp/x.sock")
})
