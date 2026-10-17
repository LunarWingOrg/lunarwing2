/**
 * lunarwing_mcp_server_test.ts — tests for the WI#324 stdio MCP shim.
 *
 * Spawns the real shim as a child process (stdio = MCP JSON-RPC) with its
 * proxy env pointed at a fake bridge IPC relay, and asserts the MCP-facing
 * behavior: initialize handshake, tools/list mapping (hub `parameters` ->
 * MCP `inputSchema`), tools/call success/denied/error mapping, and the
 * zero-tools fallback when the proxy env is absent. The final test drives
 * the real shim through the real ToolProxyRelay to a mock hub (the run-84
 * full-path gap): every other surface scripts one side of the shim<->relay
 * IPC contract, so only that test fails when the two drift apart.
 */

import assert from "node:assert/strict"
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process"
import { mkdtempSync, rmSync } from "node:fs"
import { createServer, type Server, type Socket } from "node:net"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import { after, before, test } from "node:test"

import { createEnvelope } from "./lunarwing_runtime.js"
import { ToolProxyRelay } from "./lunarwing_tool_proxy.js"

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

test("an oversized request line is answered once with its recovered id", async () => {
  // MR !1561 EV-3: pre-fix the 1 MiB stdin cap replied with `id: null` (and
  // let the dropped line's tail parse as its own line, producing a second
  // id-null error plus a -32700), so the CLI waited out its own MCP timeout
  // for the dropped call. The shim must now discard up to the newline and
  // answer exactly once with the id recovered from the line's top-level
  // edges — id-first or SDK order.
  const shim = startShim({})
  try {
    const replies: Array<Record<string, unknown>> = []
    shim.stdout.on("data", (chunk: Buffer) => {
      for (const line of chunk.toString("utf8").split("\n")) {
        if (!line.trim()) continue
        replies.push(JSON.parse(line) as Record<string, unknown>)
      }
    })
    const padding = "x".repeat(3 * 1024 * 1024)
    // id-first shape.
    shim.stdin.write(
      JSON.stringify({
        jsonrpc: "2.0",
        id: 42,
        method: "tools/call",
        params: { name: "memory_read", arguments: { path: padding } },
      }) + "\n",
    )
    // SDK order: jsonrpc/id serialized last.
    shim.stdin.write(
      `{"method":"tools/call","params":{"name":"memory_read","arguments":{"path":"${padding}"}},"jsonrpc":"2.0","id":"sdk-9"}` +
        "\n",
    )
    const pong = await rpc(shim, { jsonrpc: "2.0", id: 7, method: "ping" })
    assert.deepEqual(pong, { jsonrpc: "2.0", id: 7, result: {} })
    const errors = replies.filter((reply) => reply.error !== undefined)
    assert.equal(errors.length, 2, `expected exactly one error per oversized line: ${JSON.stringify(errors)}`)
    const byId = new Map(errors.map((reply) => [reply.id, reply.error as { code: number; message: string }]))
    assert.equal(byId.get(42)?.code, -32600, "the id-first line must be answered with its id")
    assert.equal(byId.get("sdk-9")?.code, -32600, "the SDK-order line must be answered with its id")
    assert.match(byId.get(42)?.message ?? "", /1 MiB/)
    assert.match(byId.get("sdk-9")?.message ?? "", /1 MiB/)
  } finally {
    shim.kill()
  }
})

test("oversized requests never recover nested argument ids (FE-2)", async () => {
  const shim = startShim({})
  try {
    const replies: Array<Record<string, unknown>> = []
    let buffer = ""
    shim.stdout.on("data", (chunk: Buffer) => {
      buffer += chunk.toString("utf8")
      let newline = buffer.indexOf("\n")
      while (newline >= 0) {
        const line = buffer.slice(0, newline)
        buffer = buffer.slice(newline + 1)
        if (line.trim()) replies.push(JSON.parse(line) as Record<string, unknown>)
        newline = buffer.indexOf("\n")
      }
    })
    const padding = "x".repeat(3 * 1024 * 1024)
    // Both use SDK key order, with the real id at the line's end. A nested
    // jsonrpc/id pair at the arguments' head must not beat that top-level id
    // if the head regex loses its start anchor.
    shim.stdin.write(JSON.stringify({
      method: "tools/call",
      params: { name: "memory_read", arguments: { jsonrpc: "2.0", id: 99, path: padding } },
      jsonrpc: "2.0",
      id: 42,
    }) + "\n")
    // A nested pair at the arguments' tail is still not the top-level id;
    // removing the tail regex's end anchor would incorrectly recover 98.
    shim.stdin.write(JSON.stringify({
      method: "tools/call",
      params: { name: "memory_read", arguments: { path: padding, jsonrpc: "2.0", id: 98 } },
      jsonrpc: "2.0",
      id: 43,
    }) + "\n")
    // The in-order ping is a barrier: inspect every preceding error, rather
    // than waiting for an id a regressed shim may never answer.
    const pong = await rpc(shim, { jsonrpc: "2.0", id: 7, method: "ping" })
    assert.deepEqual(pong, { jsonrpc: "2.0", id: 7, result: {} })
    const errors = replies.filter((reply) => reply.error !== undefined)
    assert.deepEqual(errors.map((reply) => reply.id), [42, 43])
    assert.deepEqual(errors.map((reply) => (reply.error as { code: number }).code), [-32600, -32600])
  } finally {
    shim.kill()
  }
})

test("a line crossing the cap together with its newline takes the in-loop path (FE-6)", async () => {
  // MR !1571 run-90 FE-6: the in-loop oversized branch (a COMPLETE line over
  // the 1 MiB cap, answered with recoverRequestIdFromEdges(line, line)) was
  // unexercised — every oversized test sent 3 MiB lines, which always exceed
  // the buffer before their newline arrives and so take the
  // unterminated-buffer discard path. Write a line just over the cap in two
  // parts: the first stays under the cap (buffered; no newline, so the
  // discard path cannot trip), the second carries the tail plus the
  // terminating newline, so the whole line is sliced and answered inside the
  // line loop. A nested id-first pair at the arguments' head must not beat
  // the real top-level id (SDK order at the line's end).
  const shim = startShim({})
  try {
    const replies: Array<Record<string, unknown>> = []
    shim.stdout.on("data", (chunk: Buffer) => {
      for (const line of chunk.toString("utf8").split("\n")) {
        if (!line.trim()) continue
        replies.push(JSON.parse(line) as Record<string, unknown>)
      }
    })
    const head =
      `{"method":"tools/call","params":{"name":"memory_read","arguments":{"jsonrpc":"2.0","id":99,"path":"`
    const tail = `"}},"jsonrpc":"2.0","id":44}`
    // 1 MiB + 100 chars total: over the cap, but the first write (all but the
    // last 200 chars) stays under it.
    const padding = "x".repeat(1024 * 1024 + 100 - head.length - tail.length)
    const line = head + padding + tail
    shim.stdin.write(line.slice(0, line.length - 200))
    await new Promise((resolve) => setTimeout(resolve, 150))
    shim.stdin.write(line.slice(line.length - 200) + "\n")
    // The in-order ping is a barrier: inspect every preceding error, rather
    // than waiting for an id a regressed shim may never answer.
    const pong = await rpc(shim, { jsonrpc: "2.0", id: 7, method: "ping" })
    assert.deepEqual(pong, { jsonrpc: "2.0", id: 7, result: {} })
    const errors = replies.filter((reply) => reply.error !== undefined)
    assert.deepEqual(errors.map((reply) => reply.id), [44])
    assert.deepEqual(errors.map((reply) => (reply.error as { code: number }).code), [-32600])
  } finally {
    shim.kill()
  }
})

/** Mock hub end of the control channel: records the WS frames the relay
 * emits; replies are injected through the relay's handleHubFrame. */
class CapturingHubSocket {
  readonly sent: string[] = []
  send(data: string): void {
    this.sent.push(data)
  }
}

/** Poll until the relay has emitted `count` hub frames (the shim's relay
 * request is asynchronous, so a fixed sleep can race the IPC round-trip). */
async function waitForFrames(hub: CapturingHubSocket, count: number): Promise<void> {
  const deadline = Date.now() + 5_000
  while (hub.sent.length < count) {
    if (Date.now() > deadline) {
      throw new Error(`relay emitted ${hub.sent.length} hub frames, expected ${count}`)
    }
    await new Promise((resolve) => setTimeout(resolve, 25))
  }
}

test("the real shim drives the real relay to a hub (run-84 full-path gap)", async () => {
  // Run-84 gap, open since MR !1561: the bridge full-path test uses a mock
  // kimi that speaks raw IPC in place of the shim, and every other test in
  // this file scripts a fake relay — so nothing pinned the real shim against
  // the real ToolProxyRelay. Here the shim child runs with its proxy env
  // pointed at a real per-task relay socket; the hub side is a capturing
  // mock whose tool_catalog/tool_result replies are injected through the
  // relay's handleHubFrame, exactly as the bridge routes them.
  const previousProxyDir = process.env.LUNARWING_TOOL_PROXY_DIR
  process.env.LUNARWING_TOOL_PROXY_DIR = workDir
  const relay = new ToolProxyRelay()
  const hub = new CapturingHubSocket()
  const handle = await relay.startTask("shim-relay-task", hub)
  assert.ok(handle, "the real relay must start its per-task socket")
  const shim = startShim({
    LUNARWING_TOOL_PROXY_SOCKET: handle.socketPath,
    LUNARWING_TASK_ID: "shim-relay-task",
  })
  try {
    // initialize is answered locally by the shim: no relay traffic.
    const init = await rpc(shim, {
      jsonrpc: "2.0",
      id: 11,
      method: "initialize",
      params: { protocolVersion: "2025-06-18" },
    })
    assert.equal((init.result as Record<string, unknown>).protocolVersion, "2025-06-18")
    assert.equal(hub.sent.length, 0)

    // tools/list: shim IPC {op:list} -> relay tool_list frame -> catalog reply.
    const listPromise = rpc(shim, { jsonrpc: "2.0", id: 12, method: "tools/list" })
    await waitForFrames(hub, 1)
    const listFrame = JSON.parse(hub.sent[0]) as { type: string; payload: Record<string, unknown> }
    assert.equal(listFrame.type, "tool_list")
    assert.equal(listFrame.payload.task_id, "shim-relay-task")
    const routedCatalog = relay.handleHubFrame(
      hub,
      createEnvelope("tool_catalog", {
        tools: [{
          name: "memory_read",
          description: "Read a workspace file",
          parameters: { type: "object", properties: { path: { type: "string" } } },
        }],
      }),
    )
    assert.equal(routedCatalog, true, "the catalog reply must route to the shim's list waiter")
    const list = await listPromise
    const tools = (list.result as { tools: Array<Record<string, unknown>> }).tools
    assert.equal(tools.length, 1)
    assert.equal(tools[0].name, "memory_read")
    assert.deepEqual(tools[0].inputSchema, {
      type: "object",
      properties: { path: { type: "string" } },
    })

    // tools/call: shim IPC {op:call} -> relay tool_call frame -> result reply.
    const callPromise = rpc(shim, {
      jsonrpc: "2.0",
      id: 13,
      method: "tools/call",
      params: { name: "memory_read", arguments: { path: "notes/x.md" } },
    })
    await waitForFrames(hub, 2)
    const callFrame = JSON.parse(hub.sent[1]) as { type: string; payload: Record<string, unknown> }
    assert.equal(callFrame.type, "tool_call")
    assert.equal(callFrame.payload.task_id, "shim-relay-task")
    assert.equal(callFrame.payload.name, "memory_read")
    assert.deepEqual(callFrame.payload.arguments, { path: "notes/x.md" })
    assert.match(String(callFrame.payload.call_id), /^[A-Za-z0-9_-]{1,128}$/)
    const routedResult = relay.handleHubFrame(
      hub,
      createEnvelope("tool_result", {
        call_id: callFrame.payload.call_id,
        status: "success",
        output: "proxied-contents",
        error: null,
      }),
    )
    assert.equal(routedResult, true, "the result reply must route by call_id")
    const called = await callPromise
    const callResult = called.result as { content: Array<{ text: string }>; isError?: boolean }
    assert.equal(callResult.isError, false)
    assert.equal(callResult.content[0].text, "proxied-contents")

    // A hub denial travels the same path and surfaces as an MCP tool error.
    const deniedPromise = rpc(shim, {
      jsonrpc: "2.0",
      id: 14,
      method: "tools/call",
      params: { name: "shell", arguments: {} },
    })
    await waitForFrames(hub, 3)
    const deniedFrame = JSON.parse(hub.sent[2]) as { payload: Record<string, unknown> }
    assert.equal(deniedFrame.payload.name, "shell")
    relay.handleHubFrame(
      hub,
      createEnvelope("tool_result", {
        call_id: deniedFrame.payload.call_id,
        status: "denied",
        output: "",
        error: "tool 'shell' is not in worker 'kimi' tool_allowlist (deny-by-default)",
      }),
    )
    const denied = await deniedPromise
    const deniedResult = denied.result as { content: Array<{ text: string }>; isError?: boolean }
    assert.equal(deniedResult.isError, true)
    assert.match(deniedResult.content[0].text, /tool_allowlist/)
  } finally {
    shim.kill()
    handle.close()
    if (previousProxyDir === undefined) {
      delete process.env.LUNARWING_TOOL_PROXY_DIR
    } else {
      process.env.LUNARWING_TOOL_PROXY_DIR = previousProxyDir
    }
  }
})
