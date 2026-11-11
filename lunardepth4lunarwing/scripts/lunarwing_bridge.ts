import { createServer, type IncomingMessage, type Server as HttpServer } from "node:http"
import type { Duplex } from "node:stream"
import WebSocket, { WebSocketServer, type RawData } from "ws"

import { delegateTaskToRunner } from "./task_runner_client.js"
import {
  bearerTokenMatches,
  boundedCloseReason,
  createEnvelope,
  errorResult,
  MAX_FRAME_BYTES,
  parseEnvelope,
  readCredentialFile,
  resultPayload,
  serializeBounded,
  SUBPROTOCOL,
  type Envelope,
  type TaskRequest,
  type TaskResult,
  validateTaskRequest,
  WORKER_VERSION,
  writeWsState,
} from "./lunarwing_runtime.js"
import { resolveWorkspacePath } from "./workspace_path.js"

interface BridgeOptions {
  token: string
  host: string
  port: number
  path: string
  workspaceRoot: string
  hostWorkspaceRoot?: string
  runnerSocket: string
  stateFile: string
  workerId: string
}

interface ActiveTask {
  id: string
  connection: HubConnection
  controller: AbortController
  workspace: string
  cancelRequested: boolean
}

export function bridgeStartupRefusal(env: NodeJS.ProcessEnv, uid: number | null): string | null {
  if (uid === 0) return "the LunarDepth bridge must not run as root"
  if (env.LUNARDEPTH_API_KEY || env.LUNARDEPTH_API_KEY_FILE) return "the model credential must not reach the bridge"
  if (env.LUNARDEPTH_LOCAL_AUTH_TOKEN || env.LUNARDEPTH_LOCAL_AUTH_TOKEN_FILE) return "the task-local credential must not reach the bridge"
  if (!env.AGENT_AUTH_TOKEN && !env.AGENT_AUTH_TOKEN_FILE) return "the bridge hub credential is required"
  return null
}

function rejectUpgrade(socket: Duplex, status: string): void {
  socket.end(`HTTP/1.1 ${status}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n`, () => socket.destroy())
}

export class BridgeServer {
  private readonly http: HttpServer
  private readonly webSockets: WebSocketServer
  private readonly connections = new Set<HubConnection>()
  private active: ActiveTask | undefined
  private closing = false

  constructor(private readonly options: BridgeOptions) {
    this.webSockets = new WebSocketServer({
      noServer: true,
      maxPayload: MAX_FRAME_BYTES,
      handleProtocols: protocols => protocols.has(SUBPROTOCOL) ? SUBPROTOCOL : false,
    })
    this.webSockets.on("connection", socket => this.accept(socket))
    this.http = createServer((_request, response) => {
      response.writeHead(404, { "content-type": "application/json" })
      response.end('{"error":"not found"}')
    })
    this.http.on("upgrade", (request, socket, head) => this.upgrade(request, socket, head))
  }

  async listen(): Promise<void> {
    await new Promise<void>((resolve, reject) => {
      this.http.once("error", reject)
      this.http.listen(this.options.port, this.options.host, () => resolve())
    })
    this.publishState()
  }

  get port(): number {
    const address = this.http.address()
    if (address === null || typeof address === "string") throw new Error("bridge listener has no TCP port")
    return address.port
  }

  private upgrade(request: IncomingMessage, socket: Duplex, head: Buffer): void {
    const authenticated = bearerTokenMatches(request.headers.authorization, this.options.token)
    if (!authenticated) {
      rejectUpgrade(socket, "401 Unauthorized")
      return
    }
    if (this.closing) {
      rejectUpgrade(socket, "400 Bad Request")
      return
    }
    let pathname: string
    try {
      pathname = new URL(request.url ?? "/", "http://bridge.invalid").pathname
    } catch {
      rejectUpgrade(socket, "400 Bad Request")
      return
    }
    const protocols = request.headers["sec-websocket-protocol"]
    const offered = (Array.isArray(protocols) ? protocols.join(",") : protocols ?? "").split(",").map(value => value.trim())
    if (pathname !== this.options.path || !offered.includes(SUBPROTOCOL)) {
      rejectUpgrade(socket, pathname !== this.options.path ? "404 Not Found" : "400 Bad Request")
      return
    }
    try {
      this.webSockets.handleUpgrade(request, socket, head, webSocket => this.webSockets.emit("connection", webSocket))
    } catch {
      if (!socket.destroyed) rejectUpgrade(socket, "400 Bad Request")
    }
  }

  private accept(socket: WebSocket): void {
    const connection = new HubConnection(socket, this)
    this.connections.add(connection)
    this.publishState()
    socket.once("close", () => {
      if (this.active?.connection === connection) this.active.controller.abort()
      this.connections.delete(connection)
      this.publishState()
    })
  }

  receive(connection: HubConnection, envelope: Envelope): void {
    switch (envelope.type) {
      case "ping":
        connection.send("pong", {})
        return
      case "task_request":
        this.startTaskPayload(envelope.payload, connection)
        return
      case "cancel":
        this.cancelTask(envelope.payload.task_id, connection)
        return
      case "task_prompt":
      case "tool_call":
      case "tool_list":
      case "tool_result":
      case "tool_catalog":
        throw new Error(`unsupported hub frame: ${envelope.type}`)
      default:
        return
    }
  }

  private startTaskPayload(payload: Record<string, unknown>, connection: HubConnection): void {
    const taskId = typeof payload.task_id === "string" ? payload.task_id : "unknown"
    try {
      this.startTask(validateTaskRequest(payload), connection)
    } catch (error) {
      connection.sendResult(errorResult(taskId, error))
    }
  }

  private startTask(request: TaskRequest, connection: HubConnection): void {
    if (this.active !== undefined) {
      connection.sendResult(errorResult(request.task_id, "a LunarDepth task is already active on this worker pair"))
      return
    }
    const workspace = resolveWorkspacePath(request.context?.project_dir, this.options.workspaceRoot, this.options.hostWorkspaceRoot)
    request.context = { ...request.context, project_dir: workspace }
    const active: ActiveTask = {
      id: request.task_id,
      connection,
      controller: new AbortController(),
      workspace,
      cancelRequested: false,
    }
    this.active = active
    void this.runTask(active, request)
  }

  private async runTask(active: ActiveTask, request: TaskRequest): Promise<void> {
    let result: TaskResult
    try {
      result = await delegateTaskToRunner(this.options.runnerSocket, request,
        progress => active.connection.send("task_progress", { ...progress }), active.controller.signal)
      if (active.cancelRequested && result.status === "error") result = { ...result, status: "cancelled" }
    } catch (error) {
      result = errorResult(request.task_id, error, active.cancelRequested ? "cancelled" : "error")
    } finally {
      if (this.active === active) this.active = undefined
    }
    active.connection.sendResult(result)
  }

  private cancelTask(value: unknown, connection: HubConnection): void {
    if (typeof value !== "string" || this.active?.connection !== connection || this.active.id !== value) {
      throw new Error("cancel does not name this connection's active task")
    }
    this.active.cancelRequested = true
    this.active.controller.abort()
  }

  private publishState(): void {
    writeWsState(this.options.stateFile, {
      timestamp: new Date().toISOString(),
      ready: !this.closing,
      role: "split-bridge",
      listening: !this.closing,
      bind_host: this.options.host,
      port: this.options.port,
      path: this.options.path,
      connections: this.connections.size,
    })
  }

  async close(): Promise<void> {
    if (this.closing) return
    this.closing = true
    this.active?.controller.abort()
    await Promise.all([...this.connections].map(connection => connection.close()))
    await new Promise<void>(resolve => this.webSockets.close(() => resolve()))
    await new Promise<void>(resolve => this.http.close(() => resolve()))
    this.publishState()
  }
}

class HubConnection {
  readonly closed: Promise<void>
  private resolveClosed: (() => void) | undefined

  constructor(private readonly socket: WebSocket, private readonly owner: BridgeServer) {
    this.closed = new Promise<void>(resolve => { this.resolveClosed = resolve })
    socket.once("close", () => this.resolveClosed?.())
    socket.on("message", (data, isBinary) => this.receive(data, isBinary))
    socket.on("error", () => socket.terminate())
    this.send("ready", {
      worker_id: process.env.LUNARWING_WORKER_ID || "lunardepth",
      version: WORKER_VERSION,
      mode: "websocket",
      capabilities: [],
    })
  }

  private receive(data: RawData, isBinary: boolean): void {
    if (isBinary) {
      this.socket.close(1003, "text messages required")
      return
    }
    try {
      const text = Array.isArray(data) ? Buffer.concat(data).toString("utf8") :
        data instanceof ArrayBuffer ? Buffer.from(data).toString("utf8") : data.toString("utf8")
      this.owner.receive(this, parseEnvelope(text))
    } catch (error) {
      this.socket.close(1008, boundedCloseReason(error))
    }
  }

  send(type: string, payload: Record<string, unknown>): void {
    if (this.socket.readyState !== WebSocket.OPEN) return
    this.socket.send(serializeBounded(createEnvelope(type, payload)))
  }

  sendResult(result: TaskResult): void {
    try {
      this.send("task_result", resultPayload(result))
    } catch {
      this.send("task_result", resultPayload(errorResult(result.task_id, "task result exceeds the hub frame limit")))
    }
  }

  async close(): Promise<void> {
    this.socket.close(1001, "worker shutting down")
    await this.closed
  }
}

function optionsFromEnv(): BridgeOptions {
  const token = process.env.AGENT_AUTH_TOKEN_FILE ? readCredentialFile(process.env.AGENT_AUTH_TOKEN_FILE) :
    process.env.AGENT_AUTH_TOKEN ?? ""
  if (token.length < 32) throw new Error("bridge hub credential is missing or too short")
  return {
    token,
    host: process.env.WS_BIND_HOST || "0.0.0.0",
    port: Number(process.env.WS_PORT ?? 9_090),
    path: process.env.WS_PATH || "/ws/agent",
    workspaceRoot: process.env.WORKSPACE_ROOT || "/workspace",
    hostWorkspaceRoot: process.env.WORKSPACE_HOST_ROOT,
    runnerSocket: process.env.LUNARDEPTH_RUNNER_SOCKET || "/run/lunarwing-shared/runner/task-runner.sock",
    stateFile: process.env.WS_STATE_FILE || "/tmp/lunardepth_ws_state.json",
    workerId: process.env.LUNARWING_WORKER_ID || "lunardepth",
  }
}

export async function main(): Promise<number> {
  const refusal = bridgeStartupRefusal(process.env, process.geteuid?.() ?? null)
  if (refusal !== null) {
    console.error(`[lunardepth-bridge] ${refusal}`)
    return 1
  }
  const bridge = new BridgeServer(optionsFromEnv())
  await bridge.listen()
  const shutdown = () => { void bridge.close() }
  process.once("SIGTERM", shutdown)
  process.once("SIGINT", shutdown)
  await new Promise(() => undefined)
  return 0
}

if (process.argv[1]?.endsWith("lunarwing_bridge.js") === true) {
  void main().then(code => { process.exitCode = code }, error => {
    console.error(`[lunardepth-bridge] ${error instanceof Error ? error.message : String(error)}`)
    process.exitCode = 1
  })
}
