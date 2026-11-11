import { chmodSync, lstatSync, mkdirSync, rmSync, unlinkSync, writeFileSync } from "node:fs"
import { connect, createServer, type Server, type Socket } from "node:net"
import { dirname } from "node:path"
import WebSocket, { type RawData } from "ws"

import {
  createEnvelope,
  DEFAULT_TIMEOUT_MS,
  errorResult,
  isRecord,
  MAX_FRAME_BYTES,
  MAX_OUTPUT_BYTES,
  MAX_PROGRESS_BYTES,
  parseEnvelope,
  readCredentialFile,
  resultPayload,
  serializeBounded,
  SUBPROTOCOL,
  type Envelope,
  type TaskProgress,
  type TaskRequest,
  type TaskResult,
  type TaskStatus,
  validateTaskRequest,
} from "./lunarwing_runtime.js"
import { resolveWorkspacePath } from "./workspace_path.js"

const RUNNER_LINE_CAP = MAX_FRAME_BYTES + 65_536
const LOCAL_GRACE_MS = 10_000
const TERMINAL_STATUSES = new Set(["success", "error", "cancelled", "timed_out"])

export function localAuthToken(env: NodeJS.ProcessEnv = process.env): string {
  const direct = env.LUNARDEPTH_LOCAL_AUTH_TOKEN ?? ""
  const file = env.LUNARDEPTH_LOCAL_AUTH_TOKEN_FILE ?? ""
  if ((direct === "") === (file === "")) {
    throw new Error("exactly one LunarDepth local token value or file is required")
  }
  const token = file === "" ? direct : readCredentialFile(file)
  if (token.length < 32 || /[\x00-\x20\x7f]/.test(token)) throw new Error("invalid LunarDepth local token")
  return token
}

export function startupRefusal(env: NodeJS.ProcessEnv, uid: number | null): string | null {
  if (env.AGENT_AUTH_TOKEN || env.AGENT_AUTH_TOKEN_FILE) return "hub credentials must not reach the task container"
  if (uid === 0) return "the LunarDepth task runner must not run as root"
  try {
    localAuthToken(env)
  } catch (error) {
    return error instanceof Error ? error.message : String(error)
  }
  return null
}

interface UpstreamEvents {
  onProgress: (progress: TaskProgress) => void
  onResult: (result: TaskResult) => void
  onError: (error: Error) => void
}

export class LunarDepthClient {
  private socket: WebSocket | undefined
  private ready = false
  private settled = false
  private taskId = ""

  constructor(private readonly url: string, private readonly token: string) {}

  async execute(request: TaskRequest, signal: AbortSignal, events: UpstreamEvents): Promise<void> {
    if (signal.aborted) throw new Error("task cancelled before LunarDepth connection")
    this.taskId = request.task_id
    const socket = new WebSocket(this.url, SUBPROTOCOL, {
      headers: { Authorization: `Bearer ${this.token}` },
      maxPayload: MAX_FRAME_BYTES,
      handshakeTimeout: 10_000,
    })
    this.socket = socket
    const deadline = setTimeout(() => this.stop(request.task_id), (request.timeout_ms ?? DEFAULT_TIMEOUT_MS) + LOCAL_GRACE_MS)
    const abort = () => this.stop(request.task_id)
    signal.addEventListener("abort", abort, { once: true })
    try {
      await new Promise<void>((resolve, reject) => {
        const eventsWithSettlement: UpstreamEvents = {
          onProgress: events.onProgress,
          onResult: result => {
            if (this.finish(socket, resolve)) events.onResult(result)
          },
          onError: error => {
            if (this.finish(socket, resolve)) events.onError(error)
          },
        }
        socket.on("message", (data, isBinary) => this.receive(data, isBinary, request, eventsWithSettlement))
        socket.once("unexpected-response", (_request, response) => {
          eventsWithSettlement.onError(new Error(`LunarDepth upgrade returned HTTP ${response.statusCode ?? "unknown"}`))
        })
        socket.once("error", error => eventsWithSettlement.onError(error))
        socket.once("close", () => {
          if (!this.settled) eventsWithSettlement.onError(new Error("LunarDepth disconnected without a terminal result"))
        })
        socket.once("open", () => undefined)
      })
    } finally {
      this.settled = true
      clearTimeout(deadline)
      signal.removeEventListener("abort", abort)
      socket.close()
    }
  }

  private finish(socket: WebSocket, resolve: () => void): boolean {
    if (this.settled) return false
    this.settled = true
    resolve()
    socket.close()
    return true
  }

  private stop(taskId: string): void {
    if (this.settled || this.socket?.readyState !== WebSocket.OPEN) return
    this.send("cancel", { task_id: taskId })
    setTimeout(() => this.socket?.terminate(), LOCAL_GRACE_MS).unref()
  }

  private receive(data: RawData, isBinary: boolean, request: TaskRequest, events: UpstreamEvents): void {
    if (this.settled) return
    if (isBinary) {
      events.onError(new Error("LunarDepth sent a binary frame"))
      return
    }
    try {
      const envelope = parseEnvelope(rawText(data))
      if (envelope.type === "ready") {
        if (this.ready) throw new Error("LunarDepth sent ready twice")
        this.ready = true
        this.send("task_request", requestPayload(request))
        return
      }
      if (!this.ready || this.socket === undefined) throw new Error("LunarDepth sent a frame before ready")
      if (envelope.type === "ping") {
        this.send("pong", {})
        return
      }
      if (envelope.type === "task_progress") {
        events.onProgress(progressPayload(envelope, request.task_id))
        return
      }
      if (envelope.type === "task_result") {
        events.onResult(taskResultPayload(envelope, request.task_id))
        return
      }
      if (envelope.type === "pong") return
      throw new Error(`unsupported LunarDepth frame: ${envelope.type}`)
    } catch (error) {
      events.onError(error instanceof Error ? error : new Error(String(error)))
    }
  }

  private send(type: string, payload: Record<string, unknown>): void {
    if (this.socket?.readyState === WebSocket.OPEN) this.socket.send(serializeBounded(createEnvelope(type, payload)))
  }
}

function requestPayload(request: TaskRequest): Record<string, unknown> {
  return {
    task_id: request.task_id,
    prompt: request.prompt,
    timeout_ms: request.timeout_ms ?? DEFAULT_TIMEOUT_MS,
    context: {
      project_dir: request.context?.project_dir,
      conversation_history: request.context?.conversation_history ?? [],
    },
  }
}

function progressPayload(envelope: Envelope, taskId: string): TaskProgress {
  const payload = envelope.payload
  if (payload.task_id !== taskId || typeof payload.delta !== "string" ||
      Buffer.byteLength(payload.delta, "utf8") > MAX_PROGRESS_BYTES) {
    throw new Error("invalid or oversized LunarDepth progress")
  }
  return { task_id: taskId, delta: payload.delta, done: false }
}

function taskResultPayload(envelope: Envelope, taskId: string): TaskResult {
  const payload = envelope.payload
  if (payload.task_id !== taskId || typeof payload.status !== "string" || !TERMINAL_STATUSES.has(payload.status) ||
      typeof payload.output !== "string" || Buffer.byteLength(payload.output, "utf8") > MAX_OUTPUT_BYTES ||
      (payload.error !== null && typeof payload.error !== "string") ||
      typeof payload.duration_ms !== "number" || !Number.isSafeInteger(payload.duration_ms) || payload.duration_ms < 0) {
    throw new Error("invalid or oversized LunarDepth terminal result")
  }
  return {
    task_id: taskId,
    status: payload.status as TaskStatus,
    output: payload.output,
    error: payload.error as string | null,
    duration_ms: payload.duration_ms,
  }
}

function rawText(data: RawData): string {
  const buffer = Array.isArray(data) ? Buffer.concat(data) : data instanceof ArrayBuffer ? Buffer.from(data) : data
  return buffer.toString("utf8")
}

interface ActiveRun {
  taskId: string
  connection: Socket
  controller: AbortController
}

export class TaskRunner {
  private readonly server: Server
  private active: ActiveRun | undefined
  private closed = false
  private socketPath = ""

  constructor(private readonly workspaceRoot = process.env.WORKSPACE_ROOT || "/workspace") {
    this.server = createServer(connection => this.accept(connection))
  }

  async listen(path: string): Promise<void> {
    this.prepareSocket(path)
    await new Promise<void>((resolve, reject) => {
      this.server.once("error", reject)
      this.server.listen(path, () => resolve())
    })
    chmodSync(path, 0o660)
    this.socketPath = path
  }

  private prepareSocket(path: string): void {
    const directory = dirname(path)
    mkdirSync(directory, { recursive: true, mode: 0o711 })
    const directoryStat = lstatSync(directory)
    if (!directoryStat.isDirectory() || directoryStat.isSymbolicLink()) throw new Error("runner directory must be a real directory")
    try {
      const stat = lstatSync(path)
      if (!stat.isSocket() || stat.uid !== process.geteuid?.()) throw new Error("stale runner path is not a task-owned socket")
      unlinkSync(path)
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error
    }
  }

  private accept(connection: Socket): void {
    let buffer = Buffer.alloc(0)
    connection.on("error", () => connection.destroy())
    connection.on("data", (chunk: Buffer) => {
      buffer = Buffer.concat([buffer, chunk])
      let newline = buffer.indexOf(10)
      while (newline >= 0) {
        if (newline > RUNNER_LINE_CAP) {
          this.write(connection, { ok: false, error: "runner line too large" })
          connection.destroy()
          return
        }
        const line = buffer.subarray(0, newline).toString("utf8")
        buffer = buffer.subarray(newline + 1)
        if (line.length > 0) void this.handleLine(connection, line)
        newline = buffer.indexOf(10)
      }
      if (buffer.length > RUNNER_LINE_CAP) connection.destroy()
    })
    connection.on("close", () => {
      if (this.active?.connection === connection) this.active.controller.abort()
    })
  }

  private async handleLine(connection: Socket, line: string): Promise<void> {
    try {
      const value: unknown = JSON.parse(line)
      if (!isRecord(value) || typeof value.op !== "string") throw new Error("runner request requires an op")
      if (value.op === "ping") {
        this.write(connection, { ok: true, op: "pong", uid: process.geteuid?.() })
        return
      }
      if (value.op === "cancel") {
        this.cancel(connection, value)
        return
      }
      if (value.op !== "run") throw new Error("unsupported runner operation")
      await this.run(connection, value)
    } catch (error) {
      this.write(connection, { ok: false, error: error instanceof Error ? error.message : String(error) })
    }
  }

  private cancel(connection: Socket, value: Record<string, unknown>): void {
    if (this.active?.connection !== connection || this.active.taskId !== value.task_id) {
      throw new Error("cancel does not name this connection's active task")
    }
    this.active.controller.abort()
    this.write(connection, { ok: true, op: "cancelled", task_id: value.task_id })
  }

  private async run(connection: Socket, value: Record<string, unknown>): Promise<void> {
    if (this.active !== undefined) throw new Error("a LunarDepth task is already active on this worker pair")
    if (value.extra_env !== undefined) throw new Error("per-job environment is unsupported")
    const request = validateTaskRequest(value.request)
    request.context = {
      ...request.context,
      project_dir: resolveWorkspacePath(request.context?.project_dir, this.workspaceRoot),
    }
    const controller = new AbortController()
    this.active = { taskId: request.task_id, connection, controller }
    this.write(connection, { ok: true, op: "accepted", task_id: request.task_id })
    const client = new LunarDepthClient(process.env.LUNARDEPTH_WS_URL || "ws://127.0.0.1:19091/ws/agent", localAuthToken())
    let terminal: TaskResult | undefined
    try {
      await client.execute(request, controller.signal, {
        onProgress: progress => this.write(connection, { op: "progress", progress }),
        onResult: result => { terminal = result },
        onError: error => { terminal = errorResult(request.task_id, error) },
      })
      this.write(connection, { op: "result", result: terminal ?? errorResult(request.task_id, "task ended without a result") })
      connection.end()
    } finally {
      if (this.active?.connection === connection) this.active = undefined
    }
  }

  private write(connection: Socket, value: Record<string, unknown>): void {
    if (!connection.destroyed) {
      const text = JSON.stringify(value)
      if (Buffer.byteLength(text, "utf8") <= RUNNER_LINE_CAP) connection.write(`${text}\n`)
      else if (value.op === "result" && isRecord(value.result)) {
        const result = errorResult(String(value.result.task_id ?? ""), "task result exceeds the runner line limit")
        connection.write(`${JSON.stringify({ op: "result", result })}\n`)
      }
    }
  }

  close(): void {
    if (this.closed) return
    this.closed = true
    this.active?.controller.abort()
    this.server.close()
    if (this.socketPath !== "") rmSync(this.socketPath, { force: true })
  }
}

export async function runnerHealthcheck(path: string): Promise<number> {
  if (path === "") return 1
  return await new Promise(resolve => {
    const socket = connect(path)
    const finish = (code: number) => {
      socket.destroy()
      resolve(code)
    }
    const timer = setTimeout(() => finish(1), 2_000)
    socket.once("error", () => { clearTimeout(timer); finish(1) })
    socket.once("data", data => {
      clearTimeout(timer)
      try {
        const value: unknown = JSON.parse(data.toString("utf8").split("\n")[0] ?? "")
        finish(isRecord(value) && value.ok === true && value.op === "pong" ? 0 : 1)
      } catch {
        finish(1)
      }
    })
    socket.write('{"op":"ping"}\n')
  })
}

export async function main(argv = process.argv.slice(2)): Promise<number> {
  if (argv[0] === "--healthcheck") return await runnerHealthcheck(argv[1] ?? process.env.LUNARDEPTH_RUNNER_SOCKET ?? "")
  const refusal = startupRefusal(process.env, process.geteuid?.() ?? null)
  if (refusal !== null) {
    console.error(`[lunardepth-runner] ${refusal}`)
    return 1
  }
  const path = argv[0] ?? process.env.LUNARDEPTH_RUNNER_SOCKET ?? "/run/lunarwing-shared/runner/task-runner.sock"
  const runner = new TaskRunner()
  await runner.listen(path)
  const statePath = process.env.LUNARDEPTH_RUNNER_STATE_FILE ?? `${dirname(path)}/task-runner-state.json`
  const heartbeat = () => writeFileSync(statePath, JSON.stringify({ ready: true, heartbeat_at: new Date().toISOString() }), { mode: 0o640 })
  heartbeat()
  const interval = setInterval(heartbeat, 10_000)
  const shutdown = () => {
    clearInterval(interval)
    runner.close()
    rmSync(statePath, { force: true })
    process.exitCode = 0
  }
  process.once("SIGTERM", shutdown)
  process.once("SIGINT", shutdown)
  await new Promise(() => undefined)
  return 0
}

if (process.argv[1]?.endsWith("lunardepth_task_runner.js") === true) {
  void main().then(code => { process.exitCode = code }, error => {
    console.error(`[lunardepth-runner] ${error instanceof Error ? error.message : String(error)}`)
    process.exitCode = 1
  })
}
