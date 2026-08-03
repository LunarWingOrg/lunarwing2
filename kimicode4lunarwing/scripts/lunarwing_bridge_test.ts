import assert from "node:assert/strict"
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process"
import { mkdtempSync } from "node:fs"
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

function startBridge(port: number, token = ""): ChildProcessWithoutNullStreams {
  const stateDir = mkdtempSync(join(tmpdir(), "kimi-bridge-"))
  return spawn(process.execPath, [bridgePath], {
    env: {
      ...process.env,
      AGENT_AUTH_TOKEN: token,
      WS_BIND_HOST: "127.0.0.1",
      WS_PORT: String(port),
      WS_STATE_FILE: join(stateDir, "state.json"),
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

async function connect(port: number, token = ""): Promise<WebSocket> {
  return await new Promise<WebSocket>((resolve, reject) => {
    const headers: Record<string, string> = {}
    if (token) headers.Authorization = `Bearer ${token}`
    const ws = new WebSocket(`ws://127.0.0.1:${port}/ws/agent`, SUBPROTOCOL, {
      headers,
    })
    ws.once("open", () => resolve(ws))
    ws.once("error", reject)
  })
}

test("bridge enforces a configured token before upgrade", async () => {
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

test("bridge server mode accepts connections without an auth token", async () => {
  const port = await freePort()
  const child = startBridge(port)
  try {
    await waitForListening(child)
    const ws = await connect(port)
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
