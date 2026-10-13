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
  parseTaskPrompt,
  writeWsState,
  MAX_CONTROL_QUEUE_PER_TASK,
  SUBPROTOCOL,
  WORKER_ID,
  WORKER_VERSION,
  CAPABILITY_TASK_PROMPT,
  type Envelope,
  type FollowupQueue,
  type TaskPromptAck,
  type TaskRequest,
  type WsState,
} from "./lunarwing_runtime"

// A single malformed frame or a stray async error must not take the whole
// worker process down (which would drop every in-flight task). Log and survive.
process.on("unhandledRejection", (reason) => {
  console.error("[bridge] unhandledRejection (surviving):", reason)
})
import { executeTask } from "./opencode_task_executor"
import type { OpencodeClient } from "@opencode-ai/sdk/v2/client"
import { resolveTaskWorkspace } from "./workspace_path"

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

// WI#328 runtime control channel: queued `task_prompt` follow-ups per running
// task. The executor drains a task's queue at turn boundaries (session idle),
// never mid-call — mirroring the daemon's "no mid-prompt injection" contract.
// The queue object is shared with the executor, which closes it when the task
// finishes (or a done prompt is consumed) so late frames are refused. The
// bridge closes it even earlier — as soon as a done=true prompt is ACCEPTED
// (MR !1537 review DW-4) — and every frame is bound to the socket that owns
// the task (DW-8), so one connection cannot feed another task's session.
const activePromptQueues = new Map<string, FollowupQueue>()

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

// ── Message Handling ──────────────────────────────────────────────────────────

function sendReady(ws: BridgeSocket): void {
  const msg = createEnvelope("ready", {
    worker_id: WORKER_ID,
    version: WORKER_VERSION,
    mode: "websocket",
    // WI#328: this worker consumes task_prompt follow-ups. Workers that omit
    // the capability get an explicit daemon-side rejection instead of
    // silently dropping the frame.
    capabilities: [CAPABILITY_TASK_PROMPT],
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
        handleCancel(envelope.payload as { task_id: string }, ws)
        break

      case "task_prompt":
        handleTaskPrompt(envelope.payload, ws)
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
  sdkOverride?: OpencodeClient,
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
    ws.send(JSON.stringify(createEnvelope("task_result", {
      task_id: request.task_id, status: "error", output: "",
      error: String(error), duration_ms: 0,
    })))
    return
  }
  const workspaceTask = activeWorkspaces.get(workspace)
  if (workspaceTask) {
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
  const promptQueue: FollowupQueue = { prompts: [], closed: false, seenSeqs: new Set() }
  activePromptQueues.set(request.task_id, promptQueue)

  try {
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
      promptQueue,
    )
  } finally {
    activeTasks.delete(request.task_id)
    activePromptQueues.delete(request.task_id)
    if (activeWorkspaces.get(workspace) === request.task_id) activeWorkspaces.delete(workspace)
  }
}

function handleCancel(payload: { task_id: string }, ws: BridgeSocket): void {
  const task = activeTasks.get(payload.task_id)
  if (task && task.socket !== ws) {
    // DW-8: a socket may only cancel its OWN tasks. Cross-socket cancel is
    // refused loudly; the owning daemon's task keeps running.
    console.warn(`[bridge] cancel: task ${payload.task_id} belongs to a different connection`)
    return
  }
  if (task) {
    console.log(`[bridge] task ${payload.task_id}: cancelled`)
    task.controller.abort()
  } else {
    console.warn(`[bridge] cancel: task ${payload.task_id} not found`)
  }
}

/**
 * WI#328: queue a daemon follow-up prompt for a running task and ack it.
 * The ack means "the worker received and queued it" — the executor injects
 * queued prompts at the next turn boundary (session idle), never mid-call.
 *
 * Refusals (acked accepted=false so the daemon's audit log reflects the drop
 * instead of a false delivery, MR !1525 DI-4):
 *  - malformed payload (DW-11: non-integer/non-positive seq, empty or
 *    oversized prompt, NUL bytes, non-boolean done),
 *  - unknown or finished task, or a queue already closed (final drain done,
 *    or a done=true prompt already accepted — DW-4),
 *  - a frame arriving on a socket that does not own the task (DW-8),
 *  - more than MAX_CONTROL_QUEUE_PER_TASK prompts waiting (DW-11).
 * While admission is open, a re-delivered seq is deduped (accepted=true,
 * not enqueued twice). A closed queue refuses every frame, even redeliveries.
 */
function handleTaskPrompt(payload: unknown, ws: BridgeSocket): void {
  const prompt = parseTaskPrompt(payload)
  // Preserve valid correlation fields even when the instruction is invalid;
  // otherwise the daemon cannot match the refusal to its delivered seq.
  const record = payload !== null && typeof payload === "object"
    ? payload as Record<string, unknown> : undefined
  const taskId = typeof record?.task_id === "string" ? record.task_id : ""
  const seq = typeof record?.seq === "number" && Number.isSafeInteger(record.seq) && record.seq > 0
    ? record.seq : 0
  const refuse = (reason: string): void => {
    console.warn(`[bridge] task_prompt refused: ${reason}`)
    const ack: TaskPromptAck = { task_id: taskId, seq, accepted: false }
    // Spread reifies the interface into an object-literal type so it stays
    // assignable to createEnvelope's Record<string, unknown> under tsc.
    ws.send(JSON.stringify(createEnvelope("task_prompt_ack", { ...ack })))
  }
  if (!prompt) {
    refuse("malformed payload")
    return
  }
  const queue = activePromptQueues.get(prompt.task_id)
  const owner = activeTasks.get(prompt.task_id)
  if (!queue || !owner) {
    refuse("unknown or finished task")
    return
  }
  if (owner.socket !== ws) {
    // DW-8: the frame arrived on a different connection than the task's
    // owner. Refuse it — the owning daemon never sees its audit ack either
    // way, and the foreign session is not fed.
    refuse("task belongs to a different connection")
    return
  }
  if (queue.closed) {
    refuse("control queue closed")
    return
  }
  const seen = queue.seenSeqs ?? new Set<number>()
  queue.seenSeqs = seen
  if (seen.has(prompt.seq)) {
    // At-least-once redelivery: ack again without queueing a duplicate.
    const ack: TaskPromptAck = { task_id: prompt.task_id, seq: prompt.seq, accepted: true }
    ws.send(JSON.stringify(createEnvelope("task_prompt_ack", { ...ack })))
    return
  }
  if (queue.prompts.length >= MAX_CONTROL_QUEUE_PER_TASK) {
    refuse("per-task control queue limit reached")
    return
  }
  seen.add(prompt.seq)
  queue.prompts.push(prompt)
  if (prompt.done) {
    // DW-4: `done` closes the channel the moment it is ACCEPTED, not when
    // the executor happens to consume it — a prompt accepted after this
    // point would be queued behind a wrap-up and silently never run.
    queue.closed = true
  }
  console.log(
    `[bridge] task ${prompt.task_id}: follow-up prompt seq=${prompt.seq} queued${prompt.done ? " (wrap-up; queue closed)" : ""}`,
  )
  const ack: TaskPromptAck = { task_id: prompt.task_id, seq: prompt.seq, accepted: true }
  ws.send(JSON.stringify(createEnvelope("task_prompt_ack", { ...ack })))
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
        abortSocketTasks(ws)
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
  const uri = WS_URL || "ws://agent-hub:9000/opencode"

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
        abortSocketTasks(bridgeSocket)
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
// Exported for the self-test (lunarwing_bridge_test.ts, MR !1537 DW-9); the
// listener roles only start when this file is the entrypoint, so importing
// it in a test does not bind a port.

export { handleCancel, handleMessage, handleTaskRequest, handleTaskPrompt, sendReady, activePromptQueues, activeTasks }

// tsc has no type for Bun's import.meta.main; the cast keeps `tsc --noEmit`
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
