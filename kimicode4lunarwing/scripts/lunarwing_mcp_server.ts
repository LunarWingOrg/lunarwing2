/**
 * lunarwing_mcp_server.ts — thin stdio MCP server that exposes the tenant's
 * allowlisted LunarWing tools (WI#324 design item 2) to the worker's CLI.
 *
 * The Kimi CLI spawns this process per task session and speaks MCP over
 * stdio (newline-delimited JSON-RPC 2.0). Each request is relayed over the
 * per-task unix socket the bridge created (LUNARWING_TOOL_PROXY_SOCKET),
 * tagged with the job id (LUNARWING_TASK_ID); the bridge forwards it as a
 * tool_list / tool_call frame on the hub WebSocket and routes the reply back.
 *
 * Without the proxy env (e.g. `kimi` CLI mode outside a dispatched task) the
 * server stays well-behaved and advertises zero tools.
 *
 * Tool surface: whatever the hub grants the worker (deny-by-default). MCP
 * clients see them as mcp__lunarwing__<name>.
 */

import { connect } from "node:net"
import { randomUUID } from "node:crypto"

const SERVER_NAME = "lunarwing-tool-proxy"
const SERVER_VERSION = "1.0.0"
// Echoed back to the client when it proposes a version; we speak the
// tools-only subset common to all MCP revisions.
const DEFAULT_PROTOCOL_VERSION = "2024-11-05"
// An inbound line larger than this can never fit the relay's 256 KiB IPC
// budget with room to spare; cap the stdin buffer so a runaway peer cannot
// grow it without bound (MR !1549 EF-9).
const MAX_STDIN_LINE_BYTES = 1024 * 1024

function requestTimeoutMs(): number {
  const raw = Number.parseInt(process.env.LUNARWING_TOOL_PROXY_TIMEOUT_MS ?? "", 10)
  if (!Number.isFinite(raw)) return 300_000
  return Math.min(900_000, Math.max(1_000, raw))
}

interface RelayResponse {
  ok: boolean
  tools?: unknown[]
  status?: string
  output?: string
  error?: string | null
}

function proxyConfigured(): { socketPath: string; taskId: string } | null {
  const socketPath = process.env.LUNARWING_TOOL_PROXY_SOCKET || ""
  const taskId = process.env.LUNARWING_TASK_ID || ""
  if (!socketPath || !taskId) return null
  return { socketPath, taskId }
}

/** One relay request over a fresh IPC connection (NDJSON, single line each way). */
function relayRequest(request: Record<string, unknown>): Promise<RelayResponse> {
  const config = proxyConfigured()
  if (!config) {
    return Promise.resolve({ ok: false, error: "tool proxy is not configured for this task" })
  }
  return new Promise((resolve) => {
    const conn = connect(config.socketPath)
    let buffer = ""
    let settled = false
    const finish = (response: RelayResponse) => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      conn.destroy()
      resolve(response)
    }
    const timer = setTimeout(() => {
      finish({ ok: false, error: `tool proxy request timed out after ${requestTimeoutMs()}ms` })
    }, requestTimeoutMs())
    conn.on("connect", () => {
      conn.write(JSON.stringify(request) + "\n")
    })
    conn.on("data", (chunk) => {
      buffer += chunk.toString("utf8")
      const newline = buffer.indexOf("\n")
      if (newline < 0) return
      const line = buffer.slice(0, newline).trim()
      try {
        finish(JSON.parse(line) as RelayResponse)
      } catch {
        finish({ ok: false, error: "tool proxy returned invalid JSON" })
      }
    })
    conn.on("error", (err) => {
      finish({ ok: false, error: `tool proxy socket error: ${err.message}` })
    })
    conn.on("close", () => {
      finish({ ok: false, error: "tool proxy socket closed before a reply" })
    })
  })
}

interface JsonRpcRequest {
  jsonrpc?: string
  id?: string | number | null
  method?: string
  params?: Record<string, unknown>
}

function writeResult(id: string | number | null, result: unknown): void {
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: id ?? null, result }) + "\n")
}

function writeError(id: string | number | null, code: number, message: string): void {
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: id ?? null, error: { code, message } }) + "\n")
}

async function handleRequest(request: JsonRpcRequest): Promise<void> {
  const id = request.id === undefined ? null : request.id
  const method = request.method
  if (typeof method !== "string") {
    if (id !== null) writeError(id, -32600, "request missing method")
    return
  }
  // Notifications (no id) never get a response.
  const isNotification = request.id === undefined || request.id === null
  switch (method) {
    case "initialize": {
      const requested = request.params?.protocolVersion
      writeResult(id, {
        protocolVersion:
          typeof requested === "string" && requested.length > 0 && requested.length <= 64
            ? requested
            : DEFAULT_PROTOCOL_VERSION,
        capabilities: { tools: {} },
        serverInfo: { name: SERVER_NAME, version: SERVER_VERSION },
      })
      return
    }
    case "notifications/initialized":
    case "notifications/cancelled":
      return
    case "ping":
      if (!isNotification) writeResult(id, {})
      return
    case "tools/list": {
      const relay = await relayRequest({ op: "list" })
      if (!relay.ok) {
        // A proxy that is absent/misconfigured must not wedge the CLI: an
        // empty catalog is the safe answer (deny-by-default hub-side anyway).
        writeResult(id, { tools: [] })
        return
      }
      const tools = (relay.tools ?? [])
        .filter(
          (tool): tool is { name: string; description?: unknown; parameters?: unknown } =>
            typeof tool === "object" && tool !== null &&
            typeof (tool as { name?: unknown }).name === "string",
        )
        .map((tool) => ({
          name: tool.name,
          description: typeof tool.description === "string" ? tool.description : "",
          inputSchema:
            typeof tool.parameters === "object" && tool.parameters !== null
              ? tool.parameters
              : { type: "object", properties: {} },
        }))
      writeResult(id, { tools })
      return
    }
    case "tools/call": {
      const params = request.params ?? {}
      const name = typeof params.name === "string" ? params.name : ""
      const args = params.arguments === undefined ? {} : params.arguments
      if (!name) {
        writeError(id, -32602, "tools/call requires a name")
        return
      }
      const relay = await relayRequest({
        op: "call",
        call_id: randomUUID(),
        name,
        arguments: args,
      })
      if (!relay.ok) {
        writeResult(id, {
          content: [{ type: "text", text: relay.error ?? "tool proxy failure" }],
          isError: true,
        })
        return
      }
      if (relay.status === "success") {
        writeResult(id, { content: [{ type: "text", text: relay.output ?? "" }], isError: false })
        return
      }
      // "denied" and "error" both surface as MCP tool errors; the hub's reason
      // text is operator-facing and sanitized, so pass it through verbatim.
      const reason = relay.error || relay.output || `tool call ${relay.status ?? "failed"}`
      writeResult(id, { content: [{ type: "text", text: reason }], isError: true })
      return
    }
    default:
      if (!isNotification) writeError(id, -32601, `method not supported: ${method.slice(0, 64)}`)
  }
}

async function main(): Promise<void> {
  if (!proxyConfigured()) {
    console.error(
      `[${SERVER_NAME}] LUNARWING_TOOL_PROXY_SOCKET/LUNARWING_TASK_ID unset; advertising zero tools`,
    )
  }
  let buffer = ""
  process.stdin.setEncoding("utf8")
  process.stdin.on("data", (chunk: string) => {
    buffer += chunk
    let newline = buffer.indexOf("\n")
    while (newline >= 0) {
      const line = buffer.slice(0, newline).trim()
      buffer = buffer.slice(newline + 1)
      if (line.length > MAX_STDIN_LINE_BYTES) {
        writeError(null, -32600, "request exceeds the 1 MiB line budget")
      } else if (line.length > 0) {
        let parsed: JsonRpcRequest
        try {
          parsed = JSON.parse(line) as JsonRpcRequest
        } catch {
          writeError(null, -32700, "parse error")
          newline = buffer.indexOf("\n")
          continue
        }
        void handleRequest(parsed).catch((err) => {
          console.error(`[${SERVER_NAME}] handler failure:`, err)
        })
      }
      newline = buffer.indexOf("\n")
    }
    if (buffer.length > MAX_STDIN_LINE_BYTES) {
      // A line this long without a terminator can never succeed at the
      // relay; drop it instead of growing the buffer without bound.
      console.error(`[${SERVER_NAME}] dropping oversized unterminated stdin line (${buffer.length} bytes)`)
      writeError(null, -32600, "request exceeds the 1 MiB line budget")
      buffer = ""
    }
  })
  // Keep the process alive until the CLI closes stdin.
  await new Promise<void>((resolve) => {
    process.stdin.on("end", resolve)
    process.stdin.on("close", resolve)
  })
}

void main()
