/**
 * lunarwing_bridge_test.ts — WI#324 full-path self-test for the opencode2
 * bridge: the real handleTaskRequest wiring (mock v2 SDK) holds the task's
 * relay socket live, an IPC call reaches the hub socket as a tool_call,
 * tool_result/tool_catalog replies through the real handleMessage routing
 * resolve the IPC callers, and the task's finally leaves zero relay tasks.
 *
 * This suite imports the bridge, which imports the executor, which imports
 * the built `@opencode/client/promise` — so it runs in the worker image build
 * (Dockerfile test gate), NOT in the host `worker-opencode2-ts` CI lane,
 * exactly like opencode_task_executor_test.ts.
 *
 * Run: bun scripts/lunarwing_bridge_test.ts
 */

import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import { mkdtempSync, rmSync } from "node:fs"
import { connect, type Socket } from "node:net"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import type { OpenCodeClient } from "@opencode/client/promise"

// WI#324: every handleTaskRequest in this file starts a per-task relay socket
// — keep those out of the shared /tmp/lunarwing-tool-proxy default and restore
// the previous value (JC-1: delete only when originally absent). The bridge
// reads WORKSPACE_ROOT at module load and its workspace resolver realpaths the
// root, so point both at temp dirs BEFORE importing the bridge (imports are
// hoisted in ES modules — dynamic import keeps the env setup first).
const savedProxyDir = process.env.LUNARWING_TOOL_PROXY_DIR
const savedWorkspaceRoot = process.env.WORKSPACE_ROOT
const proxyTestDir = mkdtempSync(join(tmpdir(), "bridge-tool-proxy-test-"))
const workspaceTestDir = mkdtempSync(join(tmpdir(), "bridge-workspace-test-"))
process.env.LUNARWING_TOOL_PROXY_DIR = proxyTestDir
process.env.WORKSPACE_ROOT = workspaceTestDir

const {
  activeTasks,
  handleHubSocketClosed,
  handleMessage,
  handleTaskRequest,
  toolProxyRelay,
} = await import("./lunarwing_bridge.ts")
const { taskSocketPath } = await import("./lunarwing_tool_proxy.ts")
const { createEnvelope } = await import("./lunarwing_runtime.ts")
const { resolveTaskId: resolvePluginTaskId } = await import("./lunarwing_tool_plugin/index.ts")

type BridgeSocketLike = { send(data: string): void }

interface SentFrame {
  type: string
  payload: {
    task_id?: string
    status?: string
    output?: string
  }
}

function fakeSocket(): { socket: BridgeSocketLike; frames: SentFrame[] } {
  const frames: SentFrame[] = []
  return {
    socket: {
      send(data: string) {
        frames.push(JSON.parse(data) as SentFrame)
      },
    },
    frames,
  }
}

async function bounded<T>(promise: Promise<T>, ms = 5_000): Promise<T> {
  return Promise.race([
    promise,
    // KL-12: unref the timeout so a settled race never keeps the process alive
    // (the suite's tail was the full 5 s of the last unreffed timer).
    new Promise<T>((_, reject) => setTimeout(() => reject(new Error("timed out")), ms).unref()),
  ])
}

/**
 * A controllable v2 SDK double: the session stays in flight (prompt pending,
 * event stream parked) until finish() settles both, so the test can drive the
 * task's relay socket while the executor runs. message.list answers the
 * canonical assistant text so the task result is a success.
 */
function controlledSdk() {
  let wakeEvents: (() => void) | undefined
  const pendingEvents: unknown[] = []
  const pushEvent = (event: unknown) => {
    pendingEvents.push(event)
    wakeEvents?.()
  }
  let resolvePrompt: (() => void) | undefined
  const promptGate = new Promise<void>((resolve) => {
    resolvePrompt = resolve
  })
  let resolveCreated: (() => void) | undefined
  const created = new Promise<void>((resolve) => {
    resolveCreated = resolve
  })
  const createArgs: unknown[] = []
  const iterator = {
    next: async (): Promise<IteratorResult<unknown>> => {
      for (;;) {
        const event = pendingEvents.shift()
        if (event !== undefined) return { done: false, value: event }
        await new Promise<void>((resolve) => {
          wakeEvents = resolve
        })
      }
    },
    return: async (): Promise<IteratorResult<unknown>> => ({ done: true, value: undefined }),
  }
  const sdk = {
    session: {
      create: async (...args: unknown[]) => {
        createArgs.push(args)
        resolveCreated?.()
        return { id: "session-1" }
      },
      prompt: async () => {
        await promptGate
        return { id: "inbox-1", sessionID: "session-1", type: "user" }
      },
      interrupt: async () => {},
    },
    event: {
      subscribe: () => ({ [Symbol.asyncIterator]: () => iterator }),
    },
    message: {
      list: async () => ({
        data: [{ type: "assistant", content: [{ type: "text", text: "DONE" }] }],
      }),
    },
  }
  const finish = () => {
    resolvePrompt?.()
    pushEvent({ type: "session.execution.succeeded", data: { sessionID: "session-1" } })
  }
  return { sdk: sdk as unknown as OpenCodeClient, created, createArgs, finish }
}

function ipcCall(socketPath: string, request: Record<string, unknown>): Promise<Record<string, unknown>> {
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
      reject(new Error("ipc call timed out"))
    }, 2_000).unref()
  })
}

async function waitForFrame(frames: SentFrame[], type: string): Promise<SentFrame> {
  const deadline = Date.now() + 2_000
  for (;;) {
    const hit = frames.find((frame) => frame.type === type)
    if (hit) return hit
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${type}`)
    await new Promise((resolve) => setTimeout(resolve, 10))
  }
}

// JU-3: a closed hub socket must drop the relay's pending map for that socket
// and fail the task's in-flight call at once — not through the later closeTask
// (which leaves an EF-9 leak and an EV-4 gap for a request arriving between the
// close and teardown). Pins `dropHubSocket` inside `handleHubSocketClosed`. The
// call in each close handler is pinned only in the kimi lane's spawned-bridge
// tests; this lane has none (KL-9, optional per the KB-2 precedent) — a spawned
// bridge here would run the real executor, which needs a live opencode serve.
{
  const a = fakeSocket()
  const handle = await toolProxyRelay.startTask("hub-close-drop", a.socket)
  assert.ok(handle)
  try {
    const parked = ipcCall(handle.socketPath, { op: "call", call_id: "c-drop", name: "memory_read", arguments: {} })
    await waitForFrame(a.frames, "tool_call")
    handleHubSocketClosed(a.socket)
    const reply = await bounded(parked)
    assert.match(String(reply.error), /hub connection closed/)
    const sentBefore = a.frames.length
    const late = await bounded(ipcCall(handle.socketPath, { op: "call", call_id: "c-late", name: "memory_read", arguments: {} }))
    assert.match(String(late.error), /hub connection closed/)
    assert.equal(a.frames.length, sentBefore, "nothing is sent into the closed hub socket")
  } finally {
    handle.close()
  }
}

// KB-1 (JU-3 follow-up): a closed hub socket also cancels the tasks it owns,
// and only those — the abortSocketTasks half of handleHubSocketClosed. Dropping
// the abort leaves a task spending model turns for a hub that is gone; dropping
// the ownership check lets one connection's close cancel tasks owned by
// another.
let kb1Counter = 0
function registerTask(socket: BridgeSocketLike): { taskId: string; controller: AbortController } {
  const taskId = `kb1-${kb1Counter++}`
  const controller = new AbortController()
  activeTasks.set(taskId, { controller, socket, workspace: `/ws/${taskId}` })
  return { taskId, controller }
}

{
  const a = fakeSocket()
  const b = fakeSocket()
  const mine = registerTask(a.socket)
  const other = registerTask(b.socket)
  handleHubSocketClosed(a.socket)
  assert.equal(mine.controller.signal.aborted, true, "the closed socket's task is cancelled")
  assert.equal(other.controller.signal.aborted, false, "another socket's task keeps running")
  activeTasks.delete(mine.taskId)
  activeTasks.delete(other.taskId)
}

// WI#324 full path: a task's relay socket is live for exactly the task's
// lifetime, and hub tool_result/tool_catalog frames arriving through the REAL
// handleMessage routing resolve the IPC callers (the path the serve-side
// plugin uses).
{
  const a = fakeSocket()
  const mock = controlledSdk()
  const taskId = "integration-tool-proxy"
  const execution = handleTaskRequest(
    { task_id: taskId, prompt: "build", timeout_ms: 5_000 },
    a.socket,
    mock.sdk,
  )

  // The plugin connects to the per-task socket while the task runs. startTask
  // runs before executeTask, so once the session create call is observed the
  // relay is listening.
  await bounded(mock.created)
  const socketPath = taskSocketPath(taskId)
  const callPromise = ipcCall(socketPath, {
    op: "call",
    call_id: "c-1",
    name: "memory_read",
    arguments: { path: "notes/x.md" },
  })
  const callFrame = (await waitForFrame(a.frames, "tool_call")).payload as unknown as {
    task_id: string
    call_id: string
    name: string
  }
  assert.equal(callFrame.task_id, taskId)
  assert.equal(callFrame.call_id, "c-1")
  assert.equal(callFrame.name, "memory_read")

  // An unroutable result frame is ignored without disturbing the pending call.
  await handleMessage(
    JSON.stringify(createEnvelope("tool_result", { call_id: "nobody", status: "error" })),
    a.socket,
  )
  await handleMessage(
    JSON.stringify(createEnvelope("tool_result", {
      call_id: "c-1", status: "success", output: "proxied contents", error: null,
    })),
    a.socket,
  )
  const callReply = await bounded(callPromise)
  assert.equal(callReply.ok, true)
  assert.equal(callReply.output, "proxied contents")

  const listPromise = ipcCall(socketPath, { op: "list" })
  await waitForFrame(a.frames, "tool_list")
  await handleMessage(
    JSON.stringify(createEnvelope("tool_catalog", {
      tools: [{ name: "memory_read", description: "read", parameters: { type: "object" } }],
    })),
    a.socket,
  )
  const listReply = await bounded(listPromise)
  assert.equal(listReply.ok, true)
  assert.equal((listReply.tools as Array<{ name: string }>)[0]!.name, "memory_read")

  mock.finish()
  await bounded(execution)
  const result = a.frames.find((frame) => frame.type === "task_result")!
  assert.equal(result.payload.status, "success")
  assert.equal(
    toolProxyRelay.activeTaskCount,
    0,
    "the finally closes the task's relay socket",
  )

  // KL-6: the executor's session.create title is the cross-file contract the
  // serve-side plugin binds on — the plugin's own parser must map the captured
  // title straight back to the task id.
  const createInput = mock.createArgs[0] as [{ title?: unknown }]
  const title = createInput[0]?.title
  assert.equal(title, `lunarwing-opencode2-${taskId}`)
  assert.equal(
    await resolvePluginTaskId(
      { session: { get: async () => ({ title, parentID: undefined }) } },
      new Map(),
      "session-1",
    ),
    taskId,
  )
}

// KL-7: the entrypoint guard must refuse an unknown WS_ROLE (exit 1, stderr
// names the role). An inverted/absent guard passes every import-time suite
// while shipping a worker that never binds a listener.
{
  const child = spawnSync(
    process.execPath,
    ["run", fileURLToPath(new URL("./lunarwing_bridge.ts", import.meta.url))],
    {
      env: { ...process.env, WS_ROLE: "lw-selftest-unknown-role" },
      encoding: "utf8",
      timeout: 20_000,
    },
  )
  assert.equal(child.status, 1, `expected exit 1, got status ${child.status} (stderr: ${child.stderr})`)
  assert.match(child.stderr, /unknown WS_ROLE/)
}

if (savedProxyDir === undefined) delete process.env.LUNARWING_TOOL_PROXY_DIR
else process.env.LUNARWING_TOOL_PROXY_DIR = savedProxyDir
if (savedWorkspaceRoot === undefined) delete process.env.WORKSPACE_ROOT
else process.env.WORKSPACE_ROOT = savedWorkspaceRoot
rmSync(proxyTestDir, { recursive: true, force: true })
rmSync(workspaceTestDir, { recursive: true, force: true })
console.log("opencode2 bridge self-check passed")
