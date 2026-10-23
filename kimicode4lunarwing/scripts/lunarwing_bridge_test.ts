import assert from "node:assert/strict"
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process"
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { createServer } from "node:net"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import { test } from "node:test"
import { WebSocket, WebSocketServer } from "ws"

import { SUBPROTOCOL } from "./lunarwing_runtime.js"

const bridgePath = fileURLToPath(new URL("./lunarwing_bridge.js", import.meta.url))

// killBridge waits for the child to exit after SIGTERM. Without waiting, a
// lingering child process keeps a stdio pipe open and the node test runner
// hangs until the 120 s timeout (pipeline #6252 kimi candidate 127798:
// 101 pass, 0 fail, 1 cancelled — the one hang).

async function freePort(): Promise<number> {
  const server = createServer()
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve))
  const address = server.address()
  assert.ok(address && typeof address !== "string")
  const port = address.port
  await new Promise<void>((resolve, reject) => server.close((error) => error ? reject(error) : resolve()))
  return port
}

function startBridge(port: number, token: string, extraEnv: Record<string, string> = {}): ChildProcessWithoutNullStreams {
  const stateDir = mkdtempSync(join(tmpdir(), "kimi-bridge-"))
  const child = spawn(process.execPath, [bridgePath], {
    env: {
      ...process.env,
      AGENT_AUTH_TOKEN: token,
      WS_BIND_HOST: "127.0.0.1",
      WS_PORT: String(port),
      WS_STATE_FILE: join(stateDir, "state.json"),
      ...extraEnv,
    },
    stdio: ["pipe", "pipe", "pipe"],
  })
  child.once("exit", () => {
    rmSync(stateDir, { recursive: true, force: true })
  })
  return child
}

async function killBridge(child: ChildProcessWithoutNullStreams): Promise<void> {
  if (child.killed) return
  const exited = new Promise<void>((resolve) => child.once("exit", () => resolve()))
  child.kill("SIGTERM")
  // Escalate to SIGKILL if the bridge doesn't exit within 3 s (e.g. a task
  // child keeping the event loop alive). SIGKILL cannot be caught so the
  // process exits immediately.
  const force = setTimeout(() => {
    if (!child.killed) child.kill("SIGKILL")
  }, 3000)
  force.unref()
  await exited
  clearTimeout(force)
}

async function waitForListening(child: ChildProcessWithoutNullStreams): Promise<void> {
  await new Promise<void>((resolve, reject) => {
    let output = ""
    child.stdout.on("data", (chunk: Buffer) => {
      output += chunk.toString("utf8")
      if (output.includes("server listening")) resolve()
    })
    child.once("exit", (code) => reject(new Error(`bridge exited before listening: ${code}`)))
    child.once("error", reject)
  })
}

async function connect(port: number, token: string): Promise<WebSocket> {
  return await new Promise<WebSocket>((resolve, reject) => {
    const ws = new WebSocket(`ws://127.0.0.1:${port}/ws/agent`, SUBPROTOCOL, {
      headers: { Authorization: `Bearer ${token}` },
    })
    ws.once("open", () => resolve(ws))
    ws.once("error", reject)
  })
}

test("bridge requires token and rejects wrong bearer before upgrade", async () => {
  const port = await freePort()
  const child = startBridge(port, "correct-token")
  try {
    await waitForListening(child)
    await assert.rejects(connect(port, "wrong-token"), /401/)
    const ws = await connect(port, "correct-token")
    const ready = await new Promise<Record<string, unknown>>((resolve, reject) => {
      ws.once("message", (data) => resolve(JSON.parse(data.toString())))
      ws.once("error", reject)
    })
    assert.equal(ready.type, "ready")
    ws.close()
  } finally {
    await killBridge(child)
  }
})

test("bridge server mode refuses empty auth token", async () => {
  const child = startBridge(await freePort(), "")
  const code = await new Promise<number | null>((resolve, reject) => {
    child.once("exit", resolve)
    child.once("error", reject)
  })
  assert.notEqual(code, 0)
})

test("cron first tick creates the default directory and executes through the real bridge", async () => {
  const root = mkdtempSync(join(tmpdir(), "kimi-cron-bridge-"))
  const bin = join(root, "mock-kimi")
  writeFileSync(bin, `#!/usr/bin/env node
require("node:fs").writeFileSync("state", "first tick")
console.log(JSON.stringify({ role: "assistant", content: "done" }))
`, { mode: 0o755 })
  const port = await freePort()
  const child = startBridge(port, "cron-test-token", { WORKSPACE_ROOT: root, KIMI_BIN: bin })
  let ws: WebSocket | undefined
  try {
    await waitForListening(child)
    ws = await connect(port, "cron-test-token")
    const result = new Promise<Record<string, unknown>>((resolve, reject) => {
      ws!.on("message", data => {
        const message = JSON.parse(data.toString())
        if (message.type === "task_result") resolve(message.payload)
      })
      ws!.once("error", reject)
    })
    assert.equal(existsSync(join(root, "missions")), false)
    ws.send(JSON.stringify({ type: "task_request", payload: {
      task_id: "cron-first", prompt: "tick", timeout_ms: 10000,
      context: { project_dir: "missions/pr-digest", cron_tick: { event: "cron.tick" } },
    }}))
    assert.equal((await result).status, "success")
    assert.equal(readFileSync(join(root, "missions/pr-digest/state"), "utf8"), "first tick")
  } finally {
    ws?.close()
    await killBridge(child)
    rmSync(root, { recursive: true, force: true })
  }
})

test("a sabotaged tool-proxy directory neither fails the task nor leaks its slot (EF-2)", async () => {
  // MR !1549 EF-2: the proxy dir lives on a same-uid shared /tmp tree; another
  // process can replace it with a plain file. startTask must degrade to
  // "no tools", the task must still report a task_result, and the
  // concurrency slot must be released for the next task.
  const root = mkdtempSync(join(tmpdir(), "kimi-proxy-sabotage-"))
  const bin = join(root, "mock-kimi")
  writeFileSync(bin, `#!/usr/bin/env node
const sock = process.env.LUNARWING_TOOL_PROXY_SOCKET
console.log(JSON.stringify({ role: "assistant", content: sock ? "HAS_PROXY_ENV" : "NO_PROXY_ENV" }))
`, { mode: 0o755 })
  const proxyDirFile = join(root, "proxy-as-file")
  writeFileSync(proxyDirFile, "not a directory")
  const port = await freePort()
  const child = startBridge(port, "sabotage-token", {
    WORKSPACE_ROOT: root,
    KIMI_BIN: bin,
    LUNARWING_TOOL_PROXY_DIR: proxyDirFile,
  })
  let ws: WebSocket | undefined
  try {
    await waitForListening(child)
    ws = await connect(port, "sabotage-token")
    const runTask = (taskId: string) => new Promise<Record<string, unknown>>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`task ${taskId} never reported a result`)), 15000)
      ws!.on("message", function onMessage(data) {
        const message = JSON.parse(data.toString())
        if (message.type === "task_result" && message.payload.task_id === taskId) {
          clearTimeout(timer)
          ws!.off("message", onMessage)
          resolve(message.payload)
        }
      })
      ws!.once("error", reject)
      ws!.send(JSON.stringify({ type: "task_request", payload: {
        task_id: taskId, prompt: "hi", timeout_ms: 10000,
      }}))
    })
    const first = await runTask("sabotage-1")
    assert.equal(first.status, "success")
    assert.match(String(first.output), /NO_PROXY_ENV/,
      "proxy setup failure must degrade to a tool-less task, not a throw")
    // The slot was released: a second task on the same socket also completes.
    const second = await runTask("sabotage-2")
    assert.equal(second.status, "success")
    assert.match(String(second.output), /NO_PROXY_ENV/)
  } finally {
    ws?.close()
    await killBridge(child)
    rmSync(root, { recursive: true, force: true })
  }
})

test("task MCP shim reaches hub tools through the bridge tool-proxy relay", async () => {
  // WI#324 full worker-side path: hub task_request -> bridge starts a per-task
  // proxy socket -> the task's CLI (mock kimi standing in for the MCP shim)
  // issues a proxied call over the unix socket -> bridge emits tool_call on the
  // hub WS -> hub tool_result routes back by call_id -> task output carries it.
  const root = mkdtempSync(join(tmpdir(), "kimi-proxy-bridge-"))
  const bin = join(root, "mock-kimi")
  writeFileSync(bin, `#!/usr/bin/env node
const net = require("node:net")
const sock = process.env.LUNARWING_TOOL_PROXY_SOCKET
const taskId = process.env.LUNARWING_TASK_ID
if (!sock || !taskId) {
  console.log(JSON.stringify({ role: "assistant", content: "NO_PROXY_ENV" }))
  process.exit(0)
}
const conn = net.connect(sock)
conn.on("connect", () => {
  conn.write(JSON.stringify({
    op: "call", call_id: "abc123", name: "memory_read",
    arguments: { path: "notes/x.md" },
  }) + "\\n")
})
let buf = ""
conn.on("data", (d) => {
  buf += d.toString()
  const i = buf.indexOf("\\n")
  if (i < 0) return
  const resp = JSON.parse(buf.slice(0, i))
  const text = resp.ok ? "tool:" + resp.output : "tool-error:" + resp.error
  console.log(JSON.stringify({ role: "assistant", content: text }))
  process.exit(0)
})
setTimeout(() => {
  console.log(JSON.stringify({ role: "assistant", content: "PROXY_TIMEOUT" }))
  process.exit(0)
}, 8000)
`, { mode: 0o755 })
  const proxyDir = join(root, "proxy")
  const port = await freePort()
  const child = startBridge(port, "proxy-test-token", {
    WORKSPACE_ROOT: root,
    KIMI_BIN: bin,
    LUNARWING_TOOL_PROXY_DIR: proxyDir,
  })
  let ws: WebSocket | undefined
  try {
    await waitForListening(child)
    ws = await connect(port, "proxy-test-token")
    const done = new Promise<Record<string, unknown>>((resolve, reject) => {
      ws!.on("message", data => {
        const message = JSON.parse(data.toString())
        if (message.type === "tool_call") {
          // The hub side: frame must be bound to the dispatched task id.
          assert.equal(message.payload.task_id, "proxy-task")
          assert.equal(message.payload.name, "memory_read")
          assert.equal(message.payload.call_id, "abc123")
          ws!.send(JSON.stringify({
            id: "hub-reply", type: "tool_result",
            timestamp: new Date().toISOString(),
            payload: {
              call_id: "abc123", status: "success",
              output: "proxied-content", error: null,
            },
          }))
        }
        if (message.type === "task_result") resolve(message.payload)
      })
      ws!.once("error", reject)
    })
    ws.send(JSON.stringify({ type: "task_request", payload: {
      task_id: "proxy-task", prompt: "read a note", timeout_ms: 30000,
    }}))
    const result = await done
    assert.equal(result.status, "success")
    assert.match(String(result.output), /tool:proxied-content/)
  } finally {
    ws?.close()
    await killBridge(child)
    rmSync(root, { recursive: true, force: true })
  }
})

test("a closed hub socket fails the task's in-flight tool call at once", async () => {
  // MR !1571 run-87 residual gap: the bridge's two dropHubSocket call sites
  // (both close handlers) were unpinned — deleting both left every suite
  // green. The mock CLI below ignores the abort's SIGTERM, so the only thing
  // that can answer its parked IPC call quickly is the relay's dropHubSocket
  // wired into the close handler: without it the waiter parks until the
  // proxy timeout and the abort's SIGKILL (kill grace, default 7 s) reaps
  // the CLI first — no marker with the closed-hub text ever appears.
  const root = mkdtempSync(join(tmpdir(), "kimi-proxy-drop-"))
  const bin = join(root, "mock-kimi")
  const marker = join(root, "ipc-reply.json")
  writeFileSync(bin, `#!/usr/bin/env node
const fs = require("node:fs")
const net = require("node:net")
process.on("SIGTERM", () => {})
const conn = net.connect(process.env.LUNARWING_TOOL_PROXY_SOCKET)
conn.on("connect", () => {
  conn.write(JSON.stringify({
    op: "call", call_id: "drop1", name: "memory_read", arguments: {},
  }) + "\\n")
})
let buf = ""
conn.on("data", (d) => {
  buf += d.toString()
  const i = buf.indexOf("\\n")
  if (i < 0) return
  fs.writeFileSync(${JSON.stringify(marker)}, buf.slice(0, i))
  process.exit(0)
})
`, { mode: 0o755 })
  const proxyDir = join(root, "proxy")
  const port = await freePort()
  const child = startBridge(port, "drop-test-token", {
    WORKSPACE_ROOT: root,
    KIMI_BIN: bin,
    LUNARWING_TOOL_PROXY_DIR: proxyDir,
    // Keep the proxy timeout far above the observation window so only the
    // dropHubSocket wiring can produce the marker in time.
    LUNARWING_TOOL_PROXY_TIMEOUT_MS: "60000",
  })
  let ws: WebSocket | undefined
  try {
    await waitForListening(child)
    ws = await connect(port, "drop-test-token")
    const sawCall = new Promise<void>((resolve, reject) => {
      ws!.on("message", (data) => {
        const message = JSON.parse(data.toString())
        if (message.type === "tool_call" && message.payload.call_id === "drop1") resolve()
      })
      ws!.once("error", reject)
    })
    ws.send(JSON.stringify({ type: "task_request", payload: {
      task_id: "drop-task", prompt: "read a note", timeout_ms: 30000,
    }}))
    // Park the call in the relay: the hub observes the frame but never
    // answers, then closes the connection.
    await sawCall
    ws.close()
    const deadline = Date.now() + 5000
    let body = ""
    while (Date.now() < deadline) {
      if (existsSync(marker)) {
        body = readFileSync(marker, "utf8")
        break
      }
      await new Promise((resolve) => setTimeout(resolve, 50))
    }
    assert.notEqual(body, "", "the IPC call must be answered promptly after the hub socket closed")
    const reply = JSON.parse(body) as { status?: unknown; error?: unknown }
    assert.equal(reply.status, "error")
    assert.match(String(reply.error), /hub connection closed/)
  } finally {
    ws?.close()
    await killBridge(child)
    rmSync(root, { recursive: true, force: true })
  }
})

test("a closed hub socket fails the task's in-flight tool call at once (client role)", async () => {
  // MR !1582 run-97 FT-1: the server-role test above pins only the
  // server-mode dropHubSocket call site — deleting the client-mode one (the
  // close listener in startClient) left every suite green. Same shape with
  // the roles reversed: a ws server plays the hub, the bridge dials out to
  // it (WS_ROLE=client), the hub sends the task after `ready` and closes on
  // the parked tool_call, and only the client close handler's dropHubSocket
  // wiring can answer the mock CLI's IPC call promptly with the closed-hub
  // error (without it the waiter parks until the proxy timeout and the
  // abort's SIGKILL reaps the SIGTERM-ignoring CLI first — no marker).
  const root = mkdtempSync(join(tmpdir(), "kimi-proxy-drop-client-"))
  const bin = join(root, "mock-kimi")
  const marker = join(root, "ipc-reply.json")
  writeFileSync(bin, `#!/usr/bin/env node
const fs = require("node:fs")
const net = require("node:net")
process.on("SIGTERM", () => {})
const conn = net.connect(process.env.LUNARWING_TOOL_PROXY_SOCKET)
conn.on("connect", () => {
  conn.write(JSON.stringify({
    op: "call", call_id: "cdrop1", name: "memory_read", arguments: {},
  }) + "\\n")
})
let buf = ""
conn.on("data", (d) => {
  buf += d.toString()
  const i = buf.indexOf("\\n")
  if (i < 0) return
  fs.writeFileSync(${JSON.stringify(marker)}, buf.slice(0, i))
  process.exit(0)
})
`, { mode: 0o755 })
  const proxyDir = join(root, "proxy")
  const hubPort = await freePort()
  const hub = new WebSocketServer({
    host: "127.0.0.1",
    port: hubPort,
    path: "/kimi",
    handleProtocols: (protocols: Set<string>): string | false =>
      protocols.has(SUBPROTOCOL) ? SUBPROTOCOL : false,
  })
  await new Promise<void>((resolve) => hub.once("listening", resolve))
  const child = startBridge(await freePort(), "client-drop-token", {
    WS_ROLE: "client",
    WS_URL: `ws://127.0.0.1:${hubPort}/kimi`,
    WORKSPACE_ROOT: root,
    KIMI_BIN: bin,
    LUNARWING_TOOL_PROXY_DIR: proxyDir,
    // Keep the proxy timeout far above the observation window so only the
    // dropHubSocket wiring can produce the marker in time.
    LUNARWING_TOOL_PROXY_TIMEOUT_MS: "60000",
  })
  try {
    const sawCall = new Promise<WebSocket>((resolve, reject) => {
      hub.on("connection", (hubSide) => {
        hubSide.on("message", (data) => {
          const message = JSON.parse(data.toString())
          if (message.type === "ready") {
            hubSide.send(JSON.stringify({ type: "task_request", payload: {
              task_id: "client-drop-task", prompt: "read a note", timeout_ms: 30000,
            }}))
          }
          if (message.type === "tool_call" && message.payload.call_id === "cdrop1") {
            resolve(hubSide)
          }
        })
        hubSide.once("error", reject)
      })
    })
    // Park the call in the relay: the hub observes the frame but never
    // answers, then closes the connection. Closing the server afterwards
    // keeps the bridge's reconnect from dispatching a second task.
    const hubSide = await sawCall
    hubSide.close()
    hub.close()
    const deadline = Date.now() + 5000
    let body = ""
    while (Date.now() < deadline) {
      if (existsSync(marker)) {
        body = readFileSync(marker, "utf8")
        break
      }
      await new Promise((resolve) => setTimeout(resolve, 50))
    }
    assert.notEqual(body, "", "the IPC call must be answered promptly after the hub socket closed")
    const reply = JSON.parse(body) as { status?: unknown; error?: unknown }
    assert.equal(reply.status, "error")
    assert.match(String(reply.error), /hub connection closed/)
  } finally {
    hub.close()
    await killBridge(child)
    rmSync(root, { recursive: true, force: true })
  }
})
