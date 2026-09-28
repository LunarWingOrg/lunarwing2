/**
 * lunarwing_bridge.ts — WebSocket bridge between LunarWing agents and the
 * Kimi Code CLI. Implements the lunarwing-agent-v1 subprotocol.
 *
 * Supports two roles:
 *   server — listens for inbound agent connections (default)
 *   client — connects outbound to a hub
 */

import { WebSocketServer, WebSocket } from "ws"

import {
  bearerTokenMatches,
  createEnvelope,
  isRecord,
  parseEnvelope,
  serializeEnvelopeWithinLimit,
  validateTaskRequest,
  writeWsState,
  SUBPROTOCOL,
  WORKER_ID,
  WORKER_VERSION,
  type Envelope,
  type TaskProgress,
  type TaskRequest,
  type TaskResult,
  type WsState,
} from "./lunarwing_runtime.js"
import { executeTask } from "./kimi_task_executor.js"
import { resolveWorkspacePath, workspacePathsOverlap } from "./workspace_path.js"

// A single malformed frame or a stray async error must not take the whole
// worker process down (which would drop every in-flight task). Log and survive.
process.on("unhandledRejection", (reason) => {
  console.error("[bridge] unhandledRejection (surviving):", reason)
})

// ── Configuration ─────────────────────────────────────────────────────────────

const WS_ROLE = process.env.WS_ROLE || "server"
const WS_BIND_HOST = process.env.WS_BIND_HOST || "0.0.0.0"
const WS_PORT = parseInt(process.env.WS_PORT || "9090", 10)
const WS_PATH = process.env.WS_PATH || "/ws/agent"
const WS_URL = process.env.WS_URL || ""
const AGENT_AUTH_TOKEN = process.env.AGENT_AUTH_TOKEN || ""
const RECONNECT_MS = parseInt(process.env.RECONNECT_MS || "3000", 10)
const PING_INTERVAL_MS = parseInt(process.env.PING_INTERVAL_MS || "20000", 10)
const WORKSPACE_ROOT = process.env.WORKSPACE_ROOT || "/workspace"

function envInteger(name: string, fallback: number, min: number, max: number): number {
  const raw = process.env[name]
  if (raw === undefined || raw === "") return fallback
  if (!/^\d+$/.test(raw)) throw new Error(`${name} must be an integer between ${min} and ${max}`)
  const value = Number(raw)
  if (!Number.isSafeInteger(value) || value < min || value > max) {
    throw new Error(`${name} must be an integer between ${min} and ${max}`)
  }
  return value
}

const WS_MAX_PAYLOAD_BYTES = envInteger("WS_MAX_PAYLOAD_BYTES", 1_048_576, 1_024, 16_777_216)
const KIMI_MAX_TASK_ID_BYTES = envInteger("KIMI_MAX_TASK_ID_BYTES", 256, 1, 4_096)
const KIMI_MAX_PROMPT_BYTES = envInteger("KIMI_MAX_PROMPT_BYTES", 524_288, 1, 8_388_608)
const KIMI_MAX_ENVIRONMENT_BYTES = envInteger("KIMI_MAX_ENVIRONMENT_BYTES", 65_536, 0, 1_048_576)
const KIMI_MAX_CONCURRENT_TASKS = envInteger("KIMI_MAX_CONCURRENT_TASKS", 8, 1, 128)
const KIMI_MAX_TIMEOUT_MS = envInteger("KIMI_MAX_TIMEOUT_MS", 14_400_000, 1_000, 86_400_000)

// ── Active Tasks ──────────────────────────────────────────────────────────────

type BridgeSocket = { send(data: string): void }
type ActiveTask = { controller: AbortController; socket: BridgeSocket; workspace: string }

const activeTasks = new Map<string, ActiveTask>()
const activeWorkspaces = new Map<string, string>()

function taskWorkspace(request: TaskRequest): string {
  return resolveWorkspacePath(request.context?.project_dir || WORKSPACE_ROOT, WORKSPACE_ROOT)
}

function abortSocketTasks(socket: BridgeSocket): void {
  for (const [taskID, task] of activeTasks) {
    if (task.socket !== socket) continue
    console.warn(`[bridge] task ${taskID}: connection closed; cancelling`)
    task.controller.abort()
  }
}

// ── Message Handling ──────────────────────────────────────────────────────────

function sendReady(ws: BridgeSocket): void {
  sendEnvelope(ws, "ready", {
    worker_id: WORKER_ID,
    version: WORKER_VERSION,
    mode: "websocket",
  })
}

function sendEnvelope(ws: BridgeSocket, type: string, payload: Record<string, unknown>): void {
  ws.send(serializeEnvelopeWithinLimit(createEnvelope(type, payload), WS_MAX_PAYLOAD_BYTES))
}

async function handleMessage(data: string, ws: BridgeSocket): Promise<void> {
  const envelope = parseEnvelope(data)
  if (!envelope) {
    console.error("[bridge] failed to parse message:", data.slice(0, 200))
    return
  }

  // A handler throwing (e.g. dereferencing a null/malformed payload) must not
  // crash the process or drop other in-flight tasks — log and keep serving.
  try {
    switch (envelope.type) {
      case "task_request":
        void handleTaskRequest(envelope.payload, ws).catch((error) => {
          console.error("[bridge] asynchronous task handler failed:", error)
        })
        break

      case "cancel":
        handleCancel(envelope.payload, ws)
        break

      case "ping":
        sendEnvelope(ws, "pong", {})
        break

      default:
        console.warn("[bridge] unknown message type:", envelope.type)
    }
  } catch (err) {
    console.error(`[bridge] handler error for "${envelope.type}":`, err)
  }
}

async function handleTaskRequest(
  payload: unknown,
  ws: BridgeSocket,
): Promise<void> {
  const validated = validateTaskRequest(payload, {
    maxTaskIdBytes: KIMI_MAX_TASK_ID_BYTES,
    maxPromptBytes: KIMI_MAX_PROMPT_BYTES,
    maxEnvironmentBytes: KIMI_MAX_ENVIRONMENT_BYTES,
    maxTimeoutMs: KIMI_MAX_TIMEOUT_MS,
  })
  if (!validated.request) {
    sendEnvelope(ws, "task_result", {
      task_id: validated.taskId,
      status: "error",
      output: "",
      error: validated.error || "Invalid task request",
      duration_ms: 0,
    })
    return
  }
  const request = validated.request

  console.log(`[bridge] task ${request.task_id}: starting`)

  if (activeTasks.has(request.task_id)) {
    // Never overwrite first task's controller. Explicit error prevents a
    // reconnecting caller from waiting forever on old socket's result.
    sendEnvelope(ws, "task_result", {
      task_id: request.task_id,
      status: "error",
      output: "",
      error: `Task ${request.task_id} is already in flight`,
      duration_ms: 0,
    })
    console.warn(`[bridge] task ${request.task_id}: rejected duplicate request`)
    return
  }

  if (activeTasks.size >= KIMI_MAX_CONCURRENT_TASKS) {
    sendEnvelope(ws, "task_result", {
      task_id: request.task_id,
      status: "error",
      output: "",
      error: `Worker concurrency limit reached (${KIMI_MAX_CONCURRENT_TASKS})`,
      duration_ms: 0,
    })
    return
  }

  let workspace: string
  try {
    workspace = taskWorkspace(request)
  } catch (error) {
    sendEnvelope(ws, "task_result", {
      task_id: request.task_id,
      status: "error",
      output: "",
      error: error instanceof Error ? error.message : "Invalid workspace path",
      duration_ms: 0,
    })
    return
  }
  const workspaceOwner = [...activeWorkspaces.entries()].find(([active]) =>
    workspacePathsOverlap(active, workspace)
  )
  if (workspaceOwner) {
    const [, workspaceTask] = workspaceOwner
    sendEnvelope(ws, "task_result", {
      task_id: request.task_id,
      status: "error",
      output: "",
      error:
        `Workspace ${workspace} is already being modified by task ${workspaceTask}. ` +
        "Cancel or finish that task before dispatching another task to this workspace.",
      duration_ms: 0,
    })
    console.warn(
      `[bridge] task ${request.task_id}: rejected because workspace is owned by ${workspaceTask}`,
    )
    return
  }

  const controller = new AbortController()
  activeTasks.set(request.task_id, { controller, socket: ws, workspace })
  activeWorkspaces.set(workspace, request.task_id)

  try {
    await executeTask(
      request,
      (progress: TaskProgress) => {
        sendEnvelope(ws, "task_progress", progress as unknown as Record<string, unknown>)
      },
      (result: TaskResult) => {
        sendEnvelope(ws, "task_result", result as unknown as Record<string, unknown>)
        console.log(`[bridge] task ${request.task_id}: ${result.status} (${result.duration_ms}ms)`)
      },
      controller.signal,
      workspace,
    )
  } finally {
    activeTasks.delete(request.task_id)
    if (activeWorkspaces.get(workspace) === request.task_id) activeWorkspaces.delete(workspace)
  }
}

function handleCancel(payload: unknown, ws: BridgeSocket): void {
  if (!isRecord(payload) || typeof payload.task_id !== "string" || payload.task_id.length === 0) {
    console.warn("[bridge] cancel: invalid payload")
    return
  }
  const task = activeTasks.get(payload.task_id)
  if (task?.socket === ws) {
    console.log(`[bridge] task ${payload.task_id}: cancelled`)
    task.controller.abort()
  } else if (task) {
    console.warn(`[bridge] cancel: task ${payload.task_id} belongs to another connection`)
  } else {
    console.warn(`[bridge] cancel: task ${payload.task_id} not found`)
  }
}

// ── Server Role ───────────────────────────────────────────────────────────────

function startServer(): void {
  if (!AGENT_AUTH_TOKEN) throw new Error("AGENT_AUTH_TOKEN is required in server mode")
  let connectionCount = 0
  let listening = false

  function updateState(): void {
    writeWsState({
      timestamp: new Date().toISOString(),
      ready: listening,
      role: "server",
      listening,
      bind_host: WS_BIND_HOST,
      port: WS_PORT,
      path: WS_PATH,
      connections: connectionCount,
    })
  }

  const server = new WebSocketServer({
    host: WS_BIND_HOST,
    port: WS_PORT,
    path: WS_PATH,
    maxPayload: WS_MAX_PAYLOAD_BYTES,
    verifyClient: (info, callback) => {
      if (bearerTokenMatches(info.req.headers.authorization, AGENT_AUTH_TOKEN)) {
        callback(true)
      } else {
        callback(false, 401, "Unauthorized")
      }
    },
    // Validate subprotocol: accept only the primary name. The removed
    // `ironclaw-agent-v1` alias is not accepted.
    handleProtocols: (protocols: Set<string>): string | false => {
      if (protocols.has(SUBPROTOCOL)) return SUBPROTOCOL
      return false
    },
  })

  server.on("connection", (ws: WebSocket) => {
    connectionCount++
    updateState()
    console.log(`[bridge] agent connected (${connectionCount} active)`)
    sendReady(ws as unknown as BridgeSocket)

    let pingTimer: NodeJS.Timeout | undefined
    if (PING_INTERVAL_MS > 0) {
      pingTimer = setInterval(() => {
        if (ws.readyState === WebSocket.OPEN) {
          ws.ping()
        }
      }, PING_INTERVAL_MS)
    }

    ws.on("message", (data: Buffer | ArrayBuffer | Buffer[], isBinary: boolean) => {
      const text = isBinary
        ? Buffer.from(data as ArrayBuffer).toString("utf-8")
        : (data as Buffer).toString("utf-8")
      void handleMessage(text, ws as unknown as BridgeSocket)
    })

    ws.on("close", () => {
      abortSocketTasks(ws as unknown as BridgeSocket)
      if (pingTimer) clearInterval(pingTimer)
      connectionCount--
      updateState()
      console.log(`[bridge] agent disconnected (${connectionCount} active)`)
    })

    ws.on("error", (err) => {
      console.error("[bridge] socket error:", err)
    })
  })

  server.on("error", (err) => {
    listening = false
    updateState()
    console.error("[bridge] server error:", err)
    process.exit(1)
  })

  server.on("listening", () => {
    listening = true
    updateState()
    console.log(`[bridge] server listening on ws://${WS_BIND_HOST}:${WS_PORT}${WS_PATH}`)
    console.log(`[bridge] subprotocol: ${SUBPROTOCOL}`)
    console.log("[bridge] auth: enabled")
  })
  updateState()
}

// ── Client Role ───────────────────────────────────────────────────────────────

async function startClient(): Promise<void> {
  const uri = WS_URL || "ws://agent-hub:9000/kimi"

  function updateState(ready: boolean): void {
    writeWsState({
      timestamp: new Date().toISOString(),
      ready,
      role: "client",
      connections: ready ? 1 : 0,
      connected_to: uri,
    })
  }

  async function connect(): Promise<void> {
    console.log(`[bridge] connecting to ${uri}...`)

    const headers: Record<string, string> = {}
    if (AGENT_AUTH_TOKEN) {
      headers["Authorization"] = `Bearer ${AGENT_AUTH_TOKEN}`
    }

    try {
      const ws = new WebSocket(uri, [SUBPROTOCOL], {
        headers,
        maxPayload: WS_MAX_PAYLOAD_BYTES,
      })
      const bridgeSocket: BridgeSocket = { send: (data) => ws.send(data) }

      ws.addEventListener("open", () => {
        console.log(`[bridge] connected to hub`)
        updateState(true)
        sendReady(bridgeSocket)
      })

      ws.addEventListener("message", (event: WebSocket.MessageEvent) => {
        const data = typeof event.data === "string" ? event.data : ""
        void handleMessage(data, bridgeSocket)
      })

      ws.addEventListener("close", (event: WebSocket.CloseEvent) => {
        abortSocketTasks(bridgeSocket)
        console.log(`[bridge] disconnected from hub: ${event.code} ${event.reason}`)
        updateState(false)
        scheduleReconnect()
      })

      ws.addEventListener("error", (event: WebSocket.ErrorEvent) => {
        console.error("[bridge] WebSocket error:", event)
        updateState(false)
      })
    } catch (err) {
      console.error("[bridge] connection failed:", err)
      updateState(false)
      scheduleReconnect()
    }
  }

  function scheduleReconnect(): void {
    console.log(`[bridge] reconnecting in ${RECONNECT_MS}ms...`)
    setTimeout(connect, RECONNECT_MS)
  }

  updateState(false)
  await connect()

  await new Promise(() => {})
}

// ── Main ──────────────────────────────────────────────────────────────────────

if (WS_ROLE === "server") {
  startServer()
} else if (WS_ROLE === "client") {
  void startClient()
} else {
  console.error(`[bridge] unknown WS_ROLE: ${WS_ROLE}`)
  process.exit(1)
}
