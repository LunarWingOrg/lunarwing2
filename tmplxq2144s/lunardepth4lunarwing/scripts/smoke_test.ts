import WebSocket from "ws"

import { createEnvelope, parseEnvelope, SUBPROTOCOL, type TaskResult } from "./lunarwing_runtime.js"

async function main(): Promise<void> {
  const url = process.env.SMOKE_WS_URL
  const token = process.env.SMOKE_TOKEN
  if (!url || !token) throw new Error("SMOKE_WS_URL and SMOKE_TOKEN are required")
  const runTask = process.env.SMOKE_TASK === "1"
  const expected = process.env.SMOKE_EXPECT || "LunarDepth smoke OK"
  const socket = new WebSocket(url, SUBPROTOCOL, {
    headers: { Authorization: `Bearer ${token}` },
    maxPayload: 16 * 1024 * 1024,
    handshakeTimeout: 10_000,
  })
  const deadline = setTimeout(() => socket.terminate(), 60_000)
  try {
    await new Promise<void>((resolve, reject) => {
      let taskSent = false
      socket.on("message", data => {
        try {
          const envelope = parseEnvelope(data.toString())
          if (envelope.type === "ready") {
            if (JSON.stringify(envelope.payload.capabilities) !== "[]") throw new Error("optional capabilities advertised")
            socket.send(JSON.stringify(createEnvelope("ping", {})))
            return
          }
          if (envelope.type === "pong" && runTask && !taskSent) {
            taskSent = true
            socket.send(JSON.stringify(createEnvelope("task_request", {
              task_id: `smoke-${Date.now()}`,
              prompt: "Reply with exactly: LunarDepth smoke OK",
              timeout_ms: 30_000,
              context: { project_dir: process.env.WORKSPACE_ROOT || "/workspace", conversation_history: [] },
            })))
            return
          }
          if (envelope.type === "pong" && !runTask) resolve()
          if (envelope.type === "task_result") {
            const result = envelope.payload as unknown as TaskResult
            if (result.status !== "success" || !result.output.includes(expected)) {
              throw new Error(`unexpected task result status ${result.status}`)
            }
            resolve()
          }
        } catch (error) {
          reject(error instanceof Error ? error : new Error(String(error)))
        }
      })
      socket.once("error", reject)
      socket.once("close", () => reject(new Error("smoke connection closed before completion")))
    })
  } finally {
    clearTimeout(deadline)
    socket.close()
  }
}

if (process.argv[1]?.endsWith("smoke_test.js") === true) {
  void main().then(() => undefined, error => {
    console.error(`[lunardepth-smoke] ${error instanceof Error ? error.message : String(error)}`)
    process.exitCode = 1
  })
}
