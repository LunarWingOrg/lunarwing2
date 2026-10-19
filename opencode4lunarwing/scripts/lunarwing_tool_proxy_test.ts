/**
 * lunarwing_tool_proxy_test.ts — unit tests for the WI#324 bridge-side relay
 * (opencode lane).
 *
 * Drives a real per-task unix socket with a fake hub socket, asserting the
 * tool_call/tool_list frames the relay emits and the IPC replies the in-task
 * plugin receives once hub tool_result/tool_catalog frames are routed back.
 * Plain-script style like the rest of this lane (bun scripts/*_test.ts).
 *
 * Run: bun scripts/lunarwing_tool_proxy_test.ts
 */

import assert from "node:assert/strict"
import { mkdtempSync, rmSync, statSync, writeFileSync } from "node:fs"
import { connect, type Socket } from "node:net"
import { tmpdir } from "node:os"
import { join } from "node:path"

import { createEnvelope } from "./lunarwing_runtime.ts"
import {
  taskSocketPath,
  TOOL_CALL_FRAME_CAP_BYTES,
  ToolProxyRelay,
} from "./lunarwing_tool_proxy.ts"

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

// MR !1700 review JC-1: save the variables before overriding and restore them
// afterwards — delete only when they were absent, so a preset (including an
// empty string) survives the suite.
const savedProxyDir = process.env.LUNARWING_TOOL_PROXY_DIR
const savedProxyTimeout = process.env.LUNARWING_TOOL_PROXY_TIMEOUT_MS

const workDir = mkdtempSync(join(tmpdir(), "tool-proxy-test-"))
process.env.LUNARWING_TOOL_PROXY_DIR = workDir
process.env.LUNARWING_TOOL_PROXY_TIMEOUT_MS = "2000"
const relay = new ToolProxyRelay()

function restoreEnv(): void {
  if (savedProxyDir === undefined) delete process.env.LUNARWING_TOOL_PROXY_DIR
  else process.env.LUNARWING_TOOL_PROXY_DIR = savedProxyDir
  if (savedProxyTimeout === undefined) delete process.env.LUNARWING_TOOL_PROXY_TIMEOUT_MS
  else process.env.LUNARWING_TOOL_PROXY_TIMEOUT_MS = savedProxyTimeout
}

async function settle(ms = 100): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms))
}

// ── call: emits a hub-bound tool_call frame and routes the reply by call_id ──
{
  const hub = new FakeHubSocket()
  const handle = await relay.startTask("task-1", hub)
  assert.ok(handle, "task proxy should start")

  const responsePromise = ipcRequest(handle.socketPath, {
    op: "call",
    call_id: "call-abc",
    name: "memory_read",
    arguments: { path: "notes/x.md" },
  })
  await settle()
  assert.equal(hub.sent.length, 1)
  const frame = JSON.parse(hub.sent[0]) as { type: string; payload: Record<string, unknown> }
  assert.equal(frame.type, "tool_call")
  assert.equal(frame.payload.task_id, "task-1")
  assert.equal(frame.payload.call_id, "call-abc")
  assert.equal(frame.payload.name, "memory_read")
  assert.deepEqual(frame.payload.arguments, { path: "notes/x.md" })

  // The hub answers on the same socket; the pending request resolves.
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
}

// ── list: sends tool_list and resolves on tool_catalog ───────────────────────
{
  const hub = new FakeHubSocket()
  const handle = await relay.startTask("task-2", hub)
  assert.ok(handle)

  const responsePromise = ipcRequest(handle.socketPath, { op: "list" })
  await settle()
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
}

// ── invalid identifiers and oversized frames are rejected locally ────────────
{
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

  // JU-4: the frame pre-check on its own — a request line just UNDER the IPC
  // line cap whose serialized tool_call frame crosses the hub budget once the
  // envelope adds id/type/timestamp/task_id. Without the relay-side frame
  // check this reaches the hub, which rejects the frame and fails the job.
  const nearCap = { op: "call", call_id: "c3", name: "memory_write", arguments: { content: "" } }
  const baseLine = Buffer.byteLength(JSON.stringify(nearCap))
  nearCap.arguments.content = "x".repeat(TOOL_CALL_FRAME_CAP_BYTES - 64 - baseLine)
  assert.ok(Buffer.byteLength(JSON.stringify(nearCap)) <= TOOL_CALL_FRAME_CAP_BYTES)
  const wouldBe = createEnvelope("tool_call", {
    task_id: "task-3",
    call_id: "c3",
    name: "memory_write",
    arguments: nearCap.arguments,
  })
  assert.ok(Buffer.byteLength(JSON.stringify(wouldBe)) > TOOL_CALL_FRAME_CAP_BYTES)
  const overFrame = await ipcRequest(handle.socketPath, nearCap)
  assert.equal(overFrame.ok, false)
  assert.match(String(overFrame.error), /byte hub budget/)

  // Nothing reached the hub for any of these.
  assert.equal(hub.sent.length, 0)

  const notJson = await ipcRequest(handle.socketPath, {})
  // op missing -> explicit error, still no hub traffic
  assert.equal(notJson.ok, false)
  assert.equal(hub.sent.length, 0)
  handle.close()
}

// ── hub-frame routing is scoped to the owning socket ─────────────────────────
{
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
  await settle()
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
}

// ── closing a task fails its in-flight calls ─────────────────────────────────
{
  const hub = new FakeHubSocket()
  const handle = await relay.startTask("task-5", hub)
  assert.ok(handle)

  const responsePromise = ipcRequest(handle.socketPath, {
    op: "call",
    call_id: "call-pending",
    name: "memory_read",
    arguments: {},
  })
  await settle()
  assert.equal(hub.sent.length, 1)
  handle.close()
  const response = await responsePromise
  assert.equal(response.ok, true)
  // The relay synthesizes an error payload so the caller never hangs.
  assert.equal(typeof response.error, "string")
}

// ── EF-2: startTask returns null instead of throwing on a sabotaged dir ──────
{
  // MR !1549 EF-2: the proxy dir is a same-uid shared /tmp tree — a task
  // process can replace it with a plain file or chmod it. The relay must
  // degrade to "task runs without tools", never throw into the bridge.
  const filePath = join(workDir, "proxy-as-file")
  writeFileSync(filePath, "not a directory")
  process.env.LUNARWING_TOOL_PROXY_DIR = filePath
  try {
    const hub = new FakeHubSocket()
    const handle = await relay.startTask("task-sabotaged", hub)
    assert.equal(handle, null)
    assert.equal(relay.activeTaskCount, 0)
    assert.equal(hub.sent.length, 0)
  } finally {
    process.env.LUNARWING_TOOL_PROXY_DIR = workDir
  }
}

// ── EF-2: closeTask never throws even when the socket path cannot be removed ─
{
  const sub = mkdtempSync(join(workDir, "close-sabotage-"))
  process.env.LUNARWING_TOOL_PROXY_DIR = sub
  const hub = new FakeHubSocket()
  const handle = await relay.startTask("task-close-sabotage", hub)
  assert.ok(handle)
  // Replace the proxy dir with a plain file: rmSync(socketPath) then fails
  // (ENOTDIR on node, EFAULT under bun), which closeTask must swallow
  // (teardown continues).
  rmSync(sub, { recursive: true, force: true })
  writeFileSync(sub, "not a directory")
  try {
    handle.close()
    assert.equal(relay.activeTaskCount, 0)
  } finally {
    process.env.LUNARWING_TOOL_PROXY_DIR = workDir
    rmSync(sub, { force: true })
  }
}

// ── EF-9: dropHubSocket fails pending waiters and frees the pending map ──────
{
  const hub = new FakeHubSocket()
  const handle = await relay.startTask("task-drop", hub)
  assert.ok(handle)

  try {
    const responsePromise = ipcRequest(handle.socketPath, {
      op: "call",
      call_id: "call-drop",
      name: "memory_read",
      arguments: {},
    })
    await settle()
    assert.equal(hub.sent.length, 1)
    relay.dropHubSocket(hub)
    const response = await responsePromise
    assert.equal(response.ok, true)
    assert.match(String(response.error), /hub connection closed/)
    // A second drop is a no-op — the entry is gone, not lingering empty.
    relay.dropHubSocket(hub)
  } finally {
    handle.close()
  }
}

// ── EV-4: a request after dropHubSocket fails at once, re-creating nothing ───
{
  const hub = new FakeHubSocket()
  const handle = await relay.startTask("task-drop-final", hub)
  assert.ok(handle)
  try {
    relay.dropHubSocket(hub)
    const started = Date.now()
    const response = await ipcRequest(handle.socketPath, {
      op: "call",
      call_id: "call-after-drop",
      name: "memory_read",
      arguments: {},
    })
    const elapsed = Date.now() - started
    assert.equal(response.ok, true)
    assert.match(String(response.error), /hub connection closed/)
    assert.ok(elapsed < 1000, `must fail at once, not wait out the relay timeout (${elapsed} ms)`)
    assert.equal(hub.sent.length, 0, "nothing may be sent into the dropped socket")
  } finally {
    handle.close()
  }
}

// ── socket path is filesystem-safe and unique per task id ────────────────────
{
  const a = taskSocketPath("../../../etc/passwd")
  const b = taskSocketPath("task/../..\nforge")
  assert.ok(a.startsWith(workDir))
  assert.ok(b.startsWith(workDir))
  assert.notEqual(a, b)
  assert.ok(!a.includes(".."))
  assert.ok(!b.includes("\n"))
}

// ── KW-6: the relay creates the proxy dir bridge-owned 0711 — a task uid can
// traverse it and reach its own (chowned) socket, but can never rename,
// delete, or plant entries in it. The chown half runs only as root with a
// pinned identity (in-image); the directory mode is pinned at creation on
// every run. Point the relay at a NOT-yet-existing subdir so creation (and
// its mode) is the relay's own work.
{
  const sub = join(workDir, "kw6-proxy")
  process.env.LUNARWING_TOOL_PROXY_DIR = sub
  try {
    const hub = new FakeHubSocket()
    const handle = await relay.startTask("task-kw6", hub)
    assert.ok(handle, "task proxy should start")
    assert.equal(statSync(sub).mode & 0o777, 0o711)
    handle!.close()
  } finally {
    process.env.LUNARWING_TOOL_PROXY_DIR = workDir
    rmSync(sub, { recursive: true, force: true })
  }
}

restoreEnv()
rmSync(workDir, { recursive: true, force: true })
console.log("opencode tool-proxy relay self-check passed")
