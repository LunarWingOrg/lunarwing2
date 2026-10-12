import assert from "node:assert/strict"
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process"
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { createServer } from "node:net"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import { test } from "node:test"
import { WebSocket } from "ws"

import { SUBPROTOCOL } from "./lunarwing_runtime.js"

const bridgePath = fileURLToPath(new URL("./lunarwing_bridge.js", import.meta.url))

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
  return spawn(process.execPath, [bridgePath], {
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
    child.kill("SIGTERM")
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
    child.kill("SIGTERM")
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
    child.kill("SIGTERM")
    rmSync(root, { recursive: true, force: true })
  }
})
