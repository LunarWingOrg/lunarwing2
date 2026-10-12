/**
 * lunarwing_mcp_server_test.ts — tests for the WI#324 stdio MCP shim.
 *
 * Spawns the real shim as a child process (stdio = MCP JSON-RPC) with its
 * proxy env pointed at a fake bridge IPC relay, and asserts the MCP-facing
 * behavior: initialize handshake, tools/list mapping (hub `parameters` ->
 * MCP `inputSchema`), tools/call success/denied/error mapping, and the
 * zero-tools fallback when the proxy env is absent.
 */

import assert from "node:assert/strict"
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process"
import { mkdtempSync, rmSync } from "node:fs"
import { createServer, type Server, type Socket } from "node:net"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import { after, before, test } from "node:test"

const shimPath = fileURLToPath(new URL("./lunarwing_mcp_server.js", import.meta.url))

/** Fake bridge-side relay: answers IPC lines with scripted responses and
 * records the requests it saw. */
class FakeRelay {
  server: Server | null = null
  socketPath = ""
  readonly requests: Array<Record<string, unknown>> = []
  listResponse: Record<string, unknown> = { ok: true, tools: [] }
  callResponse: Record<string, unknown> = { ok: true, status: "success", output: "ok", error: null }

  async start(dir: string): Promise<void> {
    this.socketPath = join(dir, "relay.sock")
    this.server = createServer((conn: Socket) => {
      let buffer = ""
      conn.on("data", (chunk) => {
        buffer += chunk.toString("utf8")
        let newline = buffer.indexOf("\n")
        while (newline >= 0) {
          const line = buffer.slice(0, newline).trim()
          buffer = buffer.slice(newline + 1)
          if (line) {
            const request = JSON.parse(line) as Record<string, unknown>
            this.requests.push(request)
            const response = request.op === "list" ? this.listResponse : this.callResponse
            conn.write(JSON.stringify(response) + "\n")
          }
          newline = buffer.indexOf("\n")
        }
      })
    })
    await new Promise<void>((resolve, reject) => {
      this.server!.once("error", reject)
      this.server!.listen(this.socketPath, resolve)
    })
  }

  async stop(): Promise<void> {
    await new Promise<void>((resolve) => this.server?.close(() => resolve()))
  }
}

function startShim(env: Record<string, string>): ChildProcessWithoutNullStreams {
  return spawn(process.execPath, [shimPath], {
    env: { ...process.env, ...env },
    stdio: ["pipe", "pipe", "pipe"],
  })
}

/** Send one JSON-RPC request line and await its response line. */
function rpc(
  child: ChildProcessWithoutNullStreams,
  request: Record<string, unknown>,
): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("shim rpc timed out")), 10_000)
    const onData = (chunk: Buffer) => {
      const text = chunk.toString("utf8")
      for (const line of text.split("\n")) {
        if (!line.trim()) continue
        const parsed = JSON.parse(line) as Record<string, unknown>
        if (parsed.id === request.id) {
          clearTimeout(timer)
          child.stdout.off("data", onData)
          resolve(parsed)
          return
        }
      }
    }
    child.stdout.on("data", onData)
    child.stdin.write(JSON.stringify(request) + "\n")
  })
}

let workDir = ""
let relay: FakeRelay

before(async () => {
  workDir = mkdtempSync(join(tmpdir(), "mcp-shim-test-"))
  relay = new FakeRelay()
  await relay.start(workDir)
})

after(async () => {
  await relay.stop()
  rmSync(workDir, { recursive: true, force: true })
})

test("initialize echoes the client protocol version", async () => {
  const shim = startShim({})
  try {
    const response = await rpc(shim, {
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: { protocolVersion: "2025-06-18" },
    })
    const result = response.result as Record<string, unknown>
    assert.equal(result.protocolVersion, "2025-06-18")
    const serverInfo = result.serverInfo as Record<string, unknown>
    assert.equal(serverInfo.name, "lunarwing-tool-proxy")
  } finally {
    shim.kill()
  }
})

test("tools/list maps the hub catalog into MCP shape", async () => {
  relay.listResponse = {
    ok: true,
    tools: [
      {
        name: "memory_read",
        description: "Read a workspace file",
        parameters: { type: "object", properties: { path: { type: "string" } } },
      },
      { name: "knowledge_search", description: "Search notes", parameters: null },
    ],
  }
  const shim = startShim({
    LUNARWING_TOOL_PROXY_SOCKET: relay.socketPath,
    LUNARWING_TASK_ID: "job-1",
  })
  try {
    const response = await rpc(shim, { jsonrpc: "2.0", id: 2, method: "tools/list" })
    const result = response.result as { tools: Array<Record<string, unknown>> }
    assert.equal(result.tools.length, 2)
    assert.equal(result.tools[0].name, "memory_read")
    assert.deepEqual(result.tools[0].inputSchema, {
      type: "object",
      properties: { path: { type: "string" } },
    })
    // A null parameters payload becomes a valid empty object schema.
    assert.deepEqual(result.tools[1].inputSchema, { type: "object", properties: {} })
    assert.deepEqual(relay.requests.at(-1), { op: "list" })
  } finally {
    shim.kill()
  }
})

test("tools/call success, denied and transport failure map to MCP results", async () => {
  const shim = startShim({
    LUNARWING_TOOL_PROXY_SOCKET: relay.socketPath,
    LUNARWING_TASK_ID: "job-2",
  })
  try {
    relay.callResponse = { ok: true, status: "success", output: "file contents", error: null }
    const ok = await rpc(shim, {
      jsonrpc: "2.0",
      id: 3,
      method: "tools/call",
      params: { name: "memory_read", arguments: { path: "notes/x.md" } },
    })
    let result = ok.result as { content: Array<{ text: string }>; isError?: boolean }
    assert.equal(result.isError, false)
    assert.equal(result.content[0].text, "file contents")
    const sent = relay.requests.at(-1) as { op: string; name: string; call_id: string }
    assert.equal(sent.op, "call")
    assert.equal(sent.name, "memory_read")
    assert.match(sent.call_id, /^[A-Za-z0-9_-]{1,128}$/)

    relay.callResponse = {
      ok: true,
      status: "denied",
      output: "",
      error: "tool 'shell' is not in worker 'kimi' tool_allowlist (deny-by-default)",
    }
    const denied = await rpc(shim, {
      jsonrpc: "2.0",
      id: 4,
      method: "tools/call",
      params: { name: "shell", arguments: {} },
    })
    result = denied.result as { content: Array<{ text: string }>; isError?: boolean }
    assert.equal(result.isError, true)
    assert.match(result.content[0].text, /tool_allowlist/)

    // Shim-side transport failure (relay socket gone) is an MCP tool error,
    // not a JSON-RPC protocol error.
    const orphan = startShim({
      LUNARWING_TOOL_PROXY_SOCKET: join(workDir, "missing.sock"),
      LUNARWING_TASK_ID: "job-3",
    })
    try {
      const failed = await rpc(orphan, {
        jsonrpc: "2.0",
        id: 5,
        method: "tools/call",
        params: { name: "memory_read", arguments: {} },
      })
      result = failed.result as { content: Array<{ text: string }>; isError?: boolean }
      assert.equal(result.isError, true)
      assert.match(result.content[0].text, /tool proxy/)
    } finally {
      orphan.kill()
    }
  } finally {
    shim.kill()
  }
})

test("without the proxy env the shim advertises zero tools", async () => {
  const shim = startShim({
    LUNARWING_TOOL_PROXY_SOCKET: "",
    LUNARWING_TASK_ID: "",
  })
  try {
    const response = await rpc(shim, { jsonrpc: "2.0", id: 6, method: "tools/list" })
    const result = response.result as { tools: unknown[] }
    assert.equal(result.tools.length, 0)
    // And the fake relay was never touched by this shim.
    const before = relay.requests.length
    await new Promise((resolve) => setTimeout(resolve, 100))
    assert.equal(relay.requests.length, before)
  } finally {
    shim.kill()
  }
})
