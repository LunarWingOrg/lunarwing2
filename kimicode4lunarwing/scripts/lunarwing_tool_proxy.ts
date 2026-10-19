/**
 * lunarwing_tool_proxy.ts — bridge-side relay for the WI#324 worker tool proxy.
 *
 * Owns a per-task unix-domain socket that the in-task stdio MCP shim
 * (lunarwing_mcp_server.ts) connects to, and translates the shim's
 * newline-delimited JSON requests into `tool_list` / `tool_call` frames on the
 * task's hub WebSocket. Replies (`tool_catalog` / `tool_result`) arrive on the
 * same hub socket and are routed back to the waiting shim by call id.
 *
 * IPC request shapes (one JSON object per line):
 *   {"op": "list"}
 *   {"op": "call", "call_id": "<ident>", "name": "<tool>", "arguments": {...}}
 * IPC response shapes:
 *   {"ok": true, "tools": [...]}                    (list)
 *   {"ok": true, "status", "output", "error"}       (call)
 *   {"ok": false, "error": "<reason>"}              (any failure)
 *
 * Bounds mirror the hub (external_worker.rs): a serialized tool_call frame
 * must fit 256 KiB; name/call_id are [A-Za-z0-9_-]{1,128}. The relay
 * pre-checks the frame size so an oversized call gets a clean local error
 * instead of a hub-side frame rejection that fails the whole job.
 */

import { createServer, type Server, type Socket } from "node:net"
import { chmodSync, chownSync, closeSync, constants, fchmodSync, fchownSync, lstatSync, mkdirSync, openSync, rmSync } from "node:fs"
import { createHash } from "node:crypto"
import { dirname, join } from "node:path"

import { taskIdentity, type TaskIdentity } from "./task_identity.js"
import {
  createEnvelope,
  isRecord,
  serializeEnvelopeWithinLimit,
  utf8ByteLength,
  type Envelope,
} from "./lunarwing_runtime.js"

// Keep in sync with MAX_TOOL_CALL_FRAME_BYTES / MAX_TOOL_NAME_BYTES /
// MAX_CALL_ID_BYTES in ic/src/orchestrator/external_worker.rs.
export const TOOL_CALL_FRAME_CAP_BYTES = 256 * 1024
const MAX_IDENT = /^[A-Za-z0-9_-]{1,128}$/
// IPC line cap: a request line larger than this can never fit the WS frame
// cap, so the connection is told off and the line is dropped (the peer may
// keep talking).
const MAX_IPC_LINE_BYTES = TOOL_CALL_FRAME_CAP_BYTES

function proxyTimeoutMs(): number {
  const raw = Number.parseInt(process.env.LUNARWING_TOOL_PROXY_TIMEOUT_MS ?? "", 10)
  if (!Number.isFinite(raw)) return 300_000
  return Math.min(900_000, Math.max(1_000, raw))
}

export function toolProxyDir(): string {
  return process.env.LUNARWING_TOOL_PROXY_DIR || "/tmp/lunarwing-tool-proxy"
}

/** WI#400 (run-170 LP-5): the proxy directory is operated on by the root
 * bridge; a symlink or non-directory at its path is refused instead of
 * followed. A missing path is fine (the relay creates it). */
export function refuseSymlinkedProxyDir(dir: string): void {
  let st
  try {
    st = lstatSync(dir)
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === "ENOENT") return
    throw err
  }
  if (st.isSymbolicLink()) throw new Error(`proxy dir ${dir} is a symlink (refused, WI#400 LP-5)`)
  if (!st.isDirectory()) throw new Error(`proxy dir ${dir} is not a directory (refused, WI#400 LP-5)`)
}

/** Filesystem-safe per-task socket path. Task ids come from the hub but are
 * still treated as untrusted input for path construction (hash-suffixed). */
export function taskSocketPath(taskId: string): string {
  const safe = taskId.replace(/[^A-Za-z0-9_-]/g, "_").slice(0, 64)
  const digest = createHash("sha256").update(taskId, "utf8").digest("hex").slice(0, 16)
  return join(toolProxyDir(), `${safe}-${digest}.sock`)
}

interface PendingWaiter {
  kind: "call" | "catalog"
  taskId: string
  resolve: (payload: Record<string, unknown>) => void
  timer: NodeJS.Timeout
}

interface TaskProxy {
  taskId: string
  server: Server
  socketPath: string
  sockets: Set<Socket>
  closed: boolean
}

/**
 * One relay per bridge process. Tracks pending hub replies per hub socket
 * (the hub's tool_result/tool_catalog frames carry call_id but no task_id, so
 * routing is scoped by the socket the reply arrived on) and one IPC server
 * per active task.
 */
export class ToolProxyRelay {
  private readonly pendingBySocket = new Map<object, Map<string, PendingWaiter>>()
  // MR !1561 EV-4: dropped hub sockets stay dropped — a request arriving on
  // a task's still-open IPC server after the drop must fail at once instead
  // of re-creating the pending map and sending into the closed socket.
  private readonly droppedHubSockets = new WeakSet<object>()
  private readonly tasks = new Map<string, TaskProxy>()

  /** Start the per-task IPC server. Returns the socket path, or null when the
   * server could not be started (the task then simply runs without tools).
   * Never throws: the filesystem setup runs on a shared, same-uid /tmp tree
   * that a concurrent task process could sabotage (MR !1549 EF-2). */
  async startTask(taskId: string, hubSocket: { send(data: string): void }): Promise<TaskProxyHandle | null> {
    if (this.tasks.has(taskId)) return null
    const socketPath = taskSocketPath(taskId)
    try {
      // WI#400 (run-170 LP-5): the root bridge creates, chowns and chmods this
      // directory. A symlink planted at the path (shared /tmp, a task uid can
      // pre-create it) must be refused, never followed.
      refuseSymlinkedProxyDir(dirname(socketPath))
      mkdirSync(dirname(socketPath), { recursive: true, mode: 0o711 })
      refuseSymlinkedProxyDir(dirname(socketPath))
      // A stale socket file from a crashed earlier task would make listen()
      // fail with EADDRINUSE; remove it (the path is hash-bound to this task).
      rmSync(socketPath, { force: true })
    } catch (err) {
      console.error(
        `[tool-proxy] task ${taskId}: proxy dir setup failed (task runs without tools):`,
        err instanceof Error ? err.message : err,
      )
      return null
    }

    const task: TaskProxy = {
      taskId,
      server: undefined as unknown as Server,
      socketPath,
      sockets: new Set(),
      closed: false,
    }
    const server = createServer((conn) => this.acceptConnection(task, hubSocket, conn))
    task.server = server
    const started = await new Promise<boolean>((resolve) => {
      server.once("error", (err) => {
        console.error(`[tool-proxy] task ${taskId}: listen failed:`, err.message)
        resolve(false)
      })
      server.listen(socketPath, () => resolve(true))
    })
    if (!started) return null
    this.tasks.set(taskId, task)
    // WI#400: the bridge runs as container root, so the socket is
    // root-owned and the task uid could not connect to it. Hand the
    // per-task SOCKET to the pinned task identity, but keep the DIRECTORY
    // root-owned 0711 (run-164 KW-6): the task uid can traverse it and
    // reach its own socket, but can never rename, delete, or plant entries
    // in a directory the root bridge keeps writing per-task sockets into.
    let identity: TaskIdentity | null = null
    try {
      identity = taskIdentity()
    } catch (err) {
      // A half-pinned identity is a configuration error; the executor's own
      // taskIdentity() call fails the spawn closed — here the task just
      // runs without tools.
      console.error(
        `[tool-proxy] task ${taskId}: task identity error (task runs without tools):`,
        err instanceof Error ? err.message : err,
      )
      this.closeTask(taskId)
      return null
    }
    if (identity && typeof process.getuid === "function" && process.getuid() === 0) {
      try {
        chownSync(socketPath, identity.uid, identity.gid)
        // Re-assert the directory contract on every task start: a leftover
        // task-owned directory from an older shape (or manual probing)
        // returns to root-owned 0711.
        // The ownership re-assert runs on an O_NOFOLLOW directory descriptor,
        // so a symlink swapped in after the check above cannot redirect it.
        const dirFd = openSync(dirname(socketPath), constants.O_RDONLY | constants.O_DIRECTORY | constants.O_NOFOLLOW)
        try {
          fchownSync(dirFd, 0, 0)
          fchmodSync(dirFd, 0o711)
        } finally {
          closeSync(dirFd)
        }
      } catch (err) {
        console.error(
          `[tool-proxy] task ${taskId}: proxy chown failed (task runs without tools):`,
          err instanceof Error ? err.message : err,
        )
        this.closeTask(taskId)
        return null
      }
    }
    return {
      socketPath,
      close: () => this.closeTask(taskId),
    }
  }

  /** Route an inbound hub frame (tool_result / tool_catalog) to its waiter. */
  handleHubFrame(hubSocket: object, envelope: Envelope): boolean {
    const pending = this.pendingBySocket.get(hubSocket)
    if (!pending || !isRecord(envelope.payload)) return false
    if (envelope.type === "tool_result") {
      const callId = typeof envelope.payload.call_id === "string" ? envelope.payload.call_id : ""
      const waiter = pending.get(callId)
      if (!waiter || waiter.kind !== "call") return false
      pending.delete(callId)
      clearTimeout(waiter.timer)
      waiter.resolve(envelope.payload)
      return true
    }
    if (envelope.type === "tool_catalog") {
      // tool_list carries no call_id; at most one catalog request is in
      // flight per task, keyed synthetically.
      for (const [key, waiter] of pending) {
        if (waiter.kind !== "catalog") continue
        pending.delete(key)
        clearTimeout(waiter.timer)
        waiter.resolve(envelope.payload)
        return true
      }
    }
    return false
  }

  /** Close the task's IPC server and fail every call it still has in flight. */
  closeTask(taskId: string): void {
    const task = this.tasks.get(taskId)
    if (!task || task.closed) return
    task.closed = true
    this.tasks.delete(taskId)
    // Stop accepting new connections; live connections end themselves once
    // their (final) response is written — writeIpc ends after each reply —
    // so pending waiters are resolved BEFORE any teardown and their error
    // responses still reach the shim. A conn with no pending request is the
    // shim's next connect() retry; it gets ECONNREFUSED from the closed
    // server, which the shim surfaces as a tool error.
    task.server.close()
    try {
      rmSync(task.socketPath, { force: true })
    } catch (err) {
      // Best effort (EF-2): a sabotaged proxy dir must not throw out of task
      // teardown — the finally in the bridge still has to release the slot.
      console.error(
        `[tool-proxy] task ${taskId}: socket cleanup failed:`,
        err instanceof Error ? err.message : err,
      )
    }
    for (const [, perSocket] of this.pendingBySocket) {
      for (const [key, waiter] of perSocket) {
        if (waiter.taskId !== taskId) continue
        perSocket.delete(key)
        clearTimeout(waiter.timer)
        waiter.resolve({ error: "task ended before the tool reply arrived" })
      }
    }
  }

  /** Number of tasks with a live IPC server (test introspection). */
  get activeTaskCount(): number {
    return this.tasks.size
  }

  /** Hub socket closed: fail every waiter parked on it and drop the map, so
   * reconnects never accumulate stale pending state (MR !1549 EF-9). Tasks'
   * IPC servers stay up; their next request just fails to reach the hub.
   * The socket is remembered as dropped (MR !1561 EV-4): a later request on
   * the still-open task IPC resolves immediately with the same error instead
   * of re-creating the pending map and waiting out the relay timeout. */
  dropHubSocket(hubSocket: object): void {
    this.droppedHubSockets.add(hubSocket)
    const pending = this.pendingBySocket.get(hubSocket)
    if (!pending) return
    this.pendingBySocket.delete(hubSocket)
    for (const [, waiter] of pending) {
      clearTimeout(waiter.timer)
      waiter.resolve({ error: "hub connection closed before the tool reply arrived" })
    }
  }

  private acceptConnection(
    task: TaskProxy,
    hubSocket: { send(data: string): void },
    conn: Socket,
  ): void {
    task.sockets.add(conn)
    conn.on("close", () => task.sockets.delete(conn))
    conn.on("error", () => conn.destroy())
    let buffer = Buffer.alloc(0)
    conn.on("data", (chunk: Buffer) => {
      buffer = Buffer.concat([buffer, chunk])
      let newline = buffer.indexOf(0x0a)
      while (newline >= 0) {
        if (newline > MAX_IPC_LINE_BYTES) {
          this.writeIpc(conn, { ok: false, error: "request line exceeds the 256 KiB frame budget" })
          buffer = buffer.subarray(newline + 1)
          newline = buffer.indexOf(0x0a)
          continue
        }
        const line = buffer.subarray(0, newline).toString("utf8").trim()
        buffer = buffer.subarray(newline + 1)
        if (line.length > 0) void this.handleIpcLine(task, hubSocket, conn, line)
        newline = buffer.indexOf(0x0a)
      }
      if (buffer.length > MAX_IPC_LINE_BYTES) {
        this.writeIpc(conn, { ok: false, error: "request line exceeds the 256 KiB frame budget" })
        conn.destroy()
      }
    })
  }

  private writeIpc(conn: Socket, response: Record<string, unknown>): void {
    if (conn.destroyed) return
    try {
      // One response per request, then half-close: the shim opens a fresh
      // connection per request, and end-after-flush guarantees the reply
      // bytes leave the kernel buffer even when the task is closing.
      conn.write(JSON.stringify(response) + "\n", () => conn.end())
    } catch {
      // Peer vanished; nothing useful to do.
    }
  }

  private async handleIpcLine(
    task: TaskProxy,
    hubSocket: { send(data: string): void },
    conn: Socket,
    line: string,
  ): Promise<void> {
    let request: unknown
    try {
      request = JSON.parse(line)
    } catch {
      this.writeIpc(conn, { ok: false, error: "request is not valid JSON" })
      return
    }
    if (!isRecord(request) || typeof request.op !== "string") {
      this.writeIpc(conn, { ok: false, error: "request must be an object with an op field" })
      return
    }
    if (task.closed) {
      this.writeIpc(conn, { ok: false, error: "task has ended" })
      return
    }
    if (request.op === "list") {
      const payload = await this.requestCatalog(task, hubSocket)
      if (payload === null) {
        this.writeIpc(conn, { ok: false, error: "tool_catalog request failed or timed out" })
        return
      }
      const tools = Array.isArray(payload.tools) ? payload.tools : []
      this.writeIpc(conn, { ok: true, tools, error: payload.error ?? null })
      return
    }
    if (request.op === "call") {
      await this.handleCall(task, hubSocket, conn, request)
      return
    }
    this.writeIpc(conn, { ok: false, error: `unknown op ${JSON.stringify(request.op).slice(0, 64)}` })
  }

  private async handleCall(
    task: TaskProxy,
    hubSocket: { send(data: string): void },
    conn: Socket,
    request: Record<string, unknown>,
  ): Promise<void> {
    const callId = request.call_id
    const name = request.name
    if (typeof callId !== "string" || !MAX_IDENT.test(callId)) {
      this.writeIpc(conn, { ok: false, error: "call_id must be [A-Za-z0-9_-]{1,128}" })
      return
    }
    if (typeof name !== "string" || !MAX_IDENT.test(name)) {
      this.writeIpc(conn, { ok: false, error: "name must be [A-Za-z0-9_-]{1,128}" })
      return
    }
    const args = request.arguments === undefined ? null : request.arguments
    const frame = createEnvelope("tool_call", {
      task_id: task.taskId,
      call_id: callId,
      name,
      arguments: args,
    })
    let serialized: string
    try {
      serialized = serializeEnvelopeWithinLimit(frame, TOOL_CALL_FRAME_CAP_BYTES)
    } catch {
      this.writeIpc(conn, {
        ok: false,
        error: `tool_call frame exceeds the ${TOOL_CALL_FRAME_CAP_BYTES}-byte hub budget`,
      })
      return
    }
    const payload = await this.sendAndWait(hubSocket, {
      key: callId,
      kind: "call",
      taskId: task.taskId,
      serialized,
    })
    if (payload === null) {
      this.writeIpc(conn, { ok: false, error: "tool_result wait failed or timed out" })
      return
    }
    this.writeIpc(conn, {
      ok: true,
      status: typeof payload.status === "string" ? payload.status : "error",
      output: typeof payload.output === "string" ? payload.output : "",
      error: typeof payload.error === "string" ? payload.error : null,
    })
  }

  private async requestCatalog(
    task: TaskProxy,
    hubSocket: { send(data: string): void },
  ): Promise<Record<string, unknown> | null> {
    const perSocket = this.pendingBySocket.get(hubSocket)
    const catalogKey = `catalog:${task.taskId}`
    if (perSocket?.has(catalogKey)) return null // one catalog request in flight per task
    const frame = createEnvelope("tool_list", { task_id: task.taskId })
    let serialized: string
    try {
      serialized = serializeEnvelopeWithinLimit(frame, TOOL_CALL_FRAME_CAP_BYTES)
    } catch {
      return null
    }
    return this.sendAndWait(hubSocket, { key: catalogKey, kind: "catalog", taskId: task.taskId, serialized })
  }

  private sendAndWait(
    hubSocket: { send(data: string): void },
    request: { key: string; kind: "call" | "catalog"; taskId: string; serialized: string },
  ): Promise<Record<string, unknown> | null> {
    return new Promise((resolve) => {
      if (this.droppedHubSockets.has(hubSocket)) {
        // The hub connection is already gone (EV-4): fail at once instead of
        // re-creating the pending map and waiting out the relay timeout.
        resolve({ error: "hub connection closed before the tool reply arrived" })
        return
      }
      let perSocket = this.pendingBySocket.get(hubSocket)
      if (!perSocket) {
        perSocket = new Map()
        this.pendingBySocket.set(hubSocket, perSocket)
      }
      if (perSocket.has(request.key)) {
        resolve(null) // duplicate call_id in flight; the shim must not reuse ids
        return
      }
      const timer = setTimeout(() => {
        const pending = this.pendingBySocket.get(hubSocket)
        if (pending?.delete(request.key)) {
          resolve(null)
        }
      }, proxyTimeoutMs())
      perSocket.set(request.key, {
        kind: request.kind,
        taskId: request.taskId,
        resolve: (payload) => resolve(payload),
        timer,
      })
      try {
        hubSocket.send(request.serialized)
      } catch (err) {
        clearTimeout(timer)
        this.pendingBySocket.get(hubSocket)?.delete(request.key)
        console.error("[tool-proxy] hub send failed:", err)
        resolve(null)
      }
    })
  }
}

export interface TaskProxyHandle {
  socketPath: string
  close(): void
}

/** Environment the task's MCP shim needs, for injection into the CLI child. */
export function proxyEnvForTask(taskId: string, socketPath: string): Record<string, string> {
  return {
    LUNARWING_TASK_ID: taskId,
    LUNARWING_TOOL_PROXY_SOCKET: socketPath,
  }
}

/** Byte length helper re-exported for the shim/tests. */
export { utf8ByteLength }
