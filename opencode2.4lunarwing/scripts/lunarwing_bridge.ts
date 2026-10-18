/**
 * lunarwing_bridge.ts — WebSocket bridge between LunarWing agents and the
 * opencode headless server. Implements the lunarwing-agent-v1 subprotocol.
 *
 * Supports two roles:
 *   server — listens for inbound agent connections (default)
 *   client — connects outbound to a hub
 */

import {
  createEnvelope,
  parseEnvelope,
  writeWsState,
  SUBPROTOCOL,
  WORKER_ID,
  WORKER_VERSION,
  type Envelope,
  type TaskRequest,
  type WsState,
} from "./lunarwing_runtime"

// A single malformed frame or a stray async error must not take the whole
// worker process down (which would drop every in-flight task). Log and survive.
process.on("unhandledRejection", (reason) => {
  console.error("[bridge] unhandledRejection (surviving):", reason)
})
import { executeTask } from "./opencode_task_executor"
// The SDK type for the test seam only, derived from executeTask so this
// file (outside the WI#369 tsgo gate) never references the client package.
type OpenCodeClient = NonNullable<Parameters<typeof executeTask>[4]>
import { resolveTaskWorkspace, workspacePathsOverlap } from "./workspace_path"
import { ToolProxyRelay, type TaskProxyHandle } from "./lunarwing_tool_proxy.ts"

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

// ── Active Tasks ──────────────────────────────────────────────────────────────

type BridgeSocket = { send(data: string): void }
type ActiveTask = { controller: AbortController; socket: BridgeSocket; workspace: string }

const activeTasks = new Map<string, ActiveTask>()
const activeWorkspaces = new Map<string, string>()

// WI#324: one tool-proxy relay per bridge process (per-task IPC servers and
// per-hub-socket pending maps).
const toolProxyRelay = new ToolProxyRelay()

function taskWorkspace(request: TaskRequest): string {
  return resolveTaskWorkspace(request.context, WORKSPACE_ROOT)
}

function abortSocketTasks(socket: BridgeSocket): void {
  for (const [taskID, task] of activeTasks) {
    if (task.socket !== socket) continue
    console.warn(`[bridge] task ${taskID}: connection closed; cancelling`)
    task.controller.abort()
  }
}

/**
 * A hub control socket closed. Fail every in-flight task on it and drop the
 * relay's pending map for that socket, so reconnects never accumulate stale
 * pending state and a request that arrives between the close and the task's
 * teardown is never sent into the dead socket (JU-3; the kimi lane pins the
 * same with its spawned-bridge test). Without `dropHubSocket`, the aborted
 * task's `finally` still fails its waiter later through `closeTask`, but the
 * dead socket's pending map stays in `pendingBySocket` for the process
 * lifetime — the EF-9 leak — and the close-to-teardown window leaves an EV-4
 * gap.
 */
export function handleHubSocketClosed(socket: BridgeSocket): void {
  abortSocketTasks(socket)
  toolProxyRelay.dropHubSocket(socket)
}

// ── Message Handling ──────────────────────────────────────────────────────────

function sendReady(ws: BridgeSocket): void {
  const msg = createEnvelope("ready", {
    worker_id: WORKER_ID,
    version: WORKER_VERSION,
    mode: "websocket",
  })
  ws.send(JSON.stringify(msg))
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
        void handleTaskRequest(envelope.payload as unknown as TaskRequest, ws).catch((error) => {
          console.error("[bridge] asynchronous task handler failed:", error)
        })
        break

      case "cancel":
        handleCancel(envelope.payload as { task_id: string })
        break

      case "tool_result":
      case "tool_catalog":
        // WI#324: hub replies to this worker's tool_call / tool_list frames.
        toolProxyRelay.handleHubFrame(ws, envelope)
        break

      case "ping":
        ws.send(JSON.stringify(createEnvelope("pong", {})))
        break

      default:
        console.warn("[bridge] unknown message type:", envelope.type)
    }
  } catch (err) {
    console.error(`[bridge] handler error for "${envelope.type}":`, err)
  }
}

async function handleTaskRequest(
  request: TaskRequest,
  ws: BridgeSocket,
  sdkOverride?: OpenCodeClient,
): Promise<void> {
  if (!request.task_id || !request.prompt) {
    const errMsg = createEnvelope("task_result", {
      task_id: request.task_id || "unknown",
      status: "error",
      output: "",
      error: "Missing task_id or prompt",
      duration_ms: 0,
    })
    ws.send(JSON.stringify(errMsg))
    return
  }

  console.log(`[bridge] task ${request.task_id}: starting`)

  if (activeTasks.has(request.task_id)) {
    // A task with this id is already in flight (e.g. an orchestrator resend
    // after a reconnect). Ignore the duplicate instead of overwriting the first
    // task's AbortController — otherwise a later cancel targets the wrong task
    // and the shared map entry gets deleted out from under the running one. The
    // in-flight task's task_result will satisfy the resend.
    console.warn(`[bridge] task ${request.task_id}: already in flight, ignoring duplicate request`)
    return
  }

  let workspace: string
  try {
    workspace = taskWorkspace(request)
  } catch (error) {
    const result = createEnvelope("task_result", {
      task_id: request.task_id,
      status: "error",
      output: "",
      error: error instanceof Error ? error.message : "Invalid workspace path",
      duration_ms: 0,
    })
    ws.send(JSON.stringify(result))
    console.warn(`[bridge] task ${request.task_id}: rejected invalid workspace path`)
    return
  }
  const workspaceOwner = [...activeWorkspaces.entries()].find(([active]) =>
    workspacePathsOverlap(active, workspace),
  )
  if (workspaceOwner) {
    const [, workspaceTask] = workspaceOwner
    const result = createEnvelope("task_result", {
      task_id: request.task_id,
      status: "error",
      output: "",
      error:
        `Workspace ${workspace} is already being modified by task ${workspaceTask}. ` +
        "Cancel or finish that task before dispatching another task to this workspace.",
      duration_ms: 0,
    })
    ws.send(JSON.stringify(result))
    console.warn(
      `[bridge] task ${request.task_id}: rejected because workspace is owned by ${workspaceTask}`,
    )
    return
  }

  const controller = new AbortController()
  activeTasks.set(request.task_id, { controller, socket: ws, workspace })
  activeWorkspaces.set(workspace, request.task_id)

  // WI#324: give the task's serve-side plugin a local tool-proxy socket so it
  // can reach the hub over this connection. A listen/fs failure never fails
  // the task — it just runs without LunarWing tools (hub stays
  // deny-by-default). startTask runs INSIDE the try (MR !1549 EF-2) so an
  // unexpected throw still hits the finally below and the concurrency slot
  // and workspace lock are released. Keep startTask/closeTask throw-free
  // (pinned by the relay's EF-2 tests).
  let toolProxy: TaskProxyHandle | null = null
  try {
    toolProxy = await toolProxyRelay.startTask(request.task_id, ws)
    await executeTask(
      request,
      (progress) => {
        const msg = createEnvelope("task_progress", progress as unknown as Record<string, unknown>)
        ws.send(JSON.stringify(msg))
      },
      (result) => {
        const msg = createEnvelope("task_result", result as unknown as Record<string, unknown>)
        ws.send(JSON.stringify(msg))
        console.log(`[bridge] task ${request.task_id}: ${result.status} (${result.duration_ms}ms)`)
      },
      controller.signal,
      sdkOverride,
    )
  } finally {
    toolProxy?.close()
    activeTasks.delete(request.task_id)
    if (activeWorkspaces.get(workspace) === request.task_id) activeWorkspaces.delete(workspace)
  }
}

function handleCancel(payload: { task_id: string }): void {
  const task = activeTasks.get(payload.task_id)
  if (task) {
    console.log(`[bridge] task ${payload.task_id}: cancelled`)
    task.controller.abort()
  } else {
    console.warn(`[bridge] cancel: task ${payload.task_id} not found`)
  }
}

// ── Server Role ───────────────────────────────────────────────────────────────

function startServer(): void {
  let connectionCount = 0

  function updateState(ready: boolean): void {
    writeWsState({
      timestamp: new Date().toISOString(),
      ready,
      role: "server",
      listening: true,
      bind_host: WS_BIND_HOST,
      port: WS_PORT,
      path: WS_PATH,
      connections: connectionCount,
    })
  }

  const server = Bun.serve({
    hostname: WS_BIND_HOST,
    port: WS_PORT,
    fetch(req, server) {
      const url = new URL(req.url)

      if (url.pathname !== WS_PATH) {
        return new Response("Not Found", { status: 404 })
      }

      if (AGENT_AUTH_TOKEN) {
        const authHeader = req.headers.get("authorization") || ""
        const token = authHeader.replace(/^Bearer\s+/i, "")
        if (token !== AGENT_AUTH_TOKEN) {
          return new Response("Unauthorized", { status: 401 })
        }
      }

      // Validate subprotocol: accept only the primary name and echo back the
      // matched offered value. The removed `ironclaw-agent-v1` alias is not
      // accepted.
      const offered = (req.headers.get("sec-websocket-protocol") || "")
        .split(",")
        .map((p) => p.trim())
      const matched = offered.includes(SUBPROTOCOL) ? SUBPROTOCOL : null
      if (!matched) {
        return new Response(`Subprotocol ${SUBPROTOCOL} required`, { status: 400 })
      }

      const upgraded = server.upgrade(req, {
        headers: { "sec-websocket-protocol": matched },
      })
      if (!upgraded) {
        return new Response("WebSocket upgrade failed", { status: 500 })
      }
      return undefined
    },
    websocket: {
      open(ws) {
        connectionCount++
        updateState(true)
        console.log(`[bridge] agent connected (${connectionCount} active)`)
        sendReady(ws)
      },
      async message(ws, message) {
        const data = typeof message === "string" ? message : new TextDecoder().decode(message)
        await handleMessage(data, ws)
      },
      close(ws, code, reason) {
        handleHubSocketClosed(ws)
        connectionCount--
        updateState(connectionCount > 0)
        console.log(`[bridge] agent disconnected: ${code} ${reason} (${connectionCount} active)`)
      },
    },
  })

  updateState(true)
  console.log(`[bridge] server listening on ws://${WS_BIND_HOST}:${WS_PORT}${WS_PATH}`)
  console.log(`[bridge] subprotocol: ${SUBPROTOCOL}`)
  console.log(`[bridge] auth: ${AGENT_AUTH_TOKEN ? "enabled" : "disabled (dev mode)"}`)
}

// ── Client Role ───────────────────────────────────────────────────────────────

async function startClient(): Promise<void> {
  const uri = WS_URL || "ws://agent-hub:9000/opencode2"

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
    headers["Sec-WebSocket-Protocol"] = SUBPROTOCOL

    try {
      const ws = new WebSocket(uri, {
        headers,
      })
      const bridgeSocket: BridgeSocket = { send: (data) => ws.send(data) }

      ws.addEventListener("open", () => {
        console.log(`[bridge] connected to hub`)
        updateState(true)
        sendReady(bridgeSocket)
      })

      ws.addEventListener("message", async (event) => {
        const data = typeof event.data === "string" ? event.data : ""
        await handleMessage(data, bridgeSocket)
      })

      ws.addEventListener("close", (event) => {
        handleHubSocketClosed(bridgeSocket)
        console.log(`[bridge] disconnected from hub: ${event.code} ${event.reason}`)
        updateState(false)
        scheduleReconnect()
      })

      ws.addEventListener("error", (event) => {
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
//
// Exported for the self-test (lunarwing_bridge_test.ts); the listener roles
// only start when this file is the entrypoint, so importing it in a test does
// not bind a port.

export { handleCancel, handleMessage, handleTaskRequest, sendReady, activeTasks, toolProxyRelay }

// Bun's import.meta.main has no shared type; the cast keeps type checkers
// clean without pulling in Bun types.
const isEntrypoint = (import.meta as unknown as { main?: boolean }).main === true

if (isEntrypoint) {
  if (WS_ROLE === "server") {
    startServer()
  } else if (WS_ROLE === "client") {
    startClient()
  } else {
    console.error(`[bridge] unknown WS_ROLE: ${WS_ROLE}`)
    process.exit(1)
  }
}
