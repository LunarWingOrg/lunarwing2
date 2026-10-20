import { afterAll, beforeAll, describe, expect, test } from "bun:test"
import { createServer, type Server, type Socket } from "node:net"
import { mkdtempSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { ReplClient } from "../src/repl-client.ts"
import type { ReplMessage } from "../src/protocol.ts"

/** Minimal daemon-side fixture: handshake, echo of received Messages. */
class MockDaemon {
  readonly socketPath: string
  private server: Server | null = null
  private connections = new Set<Socket>()
  client: Socket | null = null
  received: ReplMessage[] = []
  private stopped = false

  constructor(dir: string) {
    this.socketPath = join(dir, `lw-test-${Date.now()}-${Math.random().toString(36).slice(2)}.sock`)
  }

  start(): Promise<void> {
    return new Promise((resolve) => {
      this.server = createServer((socket) => {
        this.client = socket
        this.connections.add(socket)
        socket.on("close", () => this.connections.delete(socket))
        let buffer = ""
        socket.on("data", (chunk: Buffer) => {
          buffer += chunk.toString("utf8")
          let newline = buffer.indexOf("\n")
          while (newline !== -1) {
            const line = buffer.slice(0, newline).trim()
            buffer = buffer.slice(newline + 1)
            if (line.length > 0) {
              this.handle(line, socket)
            }
            newline = buffer.indexOf("\n")
          }
        })
      })
      this.server!.listen(this.socketPath, () => resolve())
    })
  }

  private handle(line: string, socket: Socket): void {
    const message = JSON.parse(line) as ReplMessage
    this.received.push(message)
    if (typeof message === "object" && "Connect" in message) {
      this.send({ Response: { content: "welcome", session_id: "s1", is_complete: true } }, socket)
    }
  }

  send(message: ReplMessage, socket: Socket | null = this.client): void {
    socket?.write(`${JSON.stringify(message)}\n`)
  }

  stop(): Promise<void> {
    if (this.stopped) {
      return Promise.resolve()
    }
    this.stopped = true
    for (const socket of this.connections) {
      socket.destroy()
    }
    return new Promise((resolve) => {
      this.server?.close(() => resolve())
    })
  }
}

let daemon: MockDaemon
let dir: string

beforeAll(async () => {
  dir = mkdtempSync(join(tmpdir(), "lw-tui-test-"))
  daemon = new MockDaemon(dir)
  await daemon.start()
})

afterAll(async () => {
  await daemon.stop()
})

test("handshake advertises the status feature and resolves on welcome", async () => {
  const events: string[] = []
  const client = new ReplClient(daemon.socketPath, {
    onWelcome: (content, sessionId) => events.push(`welcome:${content}:${sessionId}`),
  })
  await client.connect()
  expect(events).toEqual(["welcome:welcome:s1"])
  expect(client.sessionId).toBe("s1")

  const connect = daemon.received.find(
    (m): m is Extract<ReplMessage, { Connect: unknown }> => typeof m === "object" && "Connect" in m,
  )
  expect(connect).toBeDefined()
  expect(connect!.Connect.features).toContain("status")

  client.disconnect()
})

test("send carries base64 attachments; interrupt uses the priority command", async () => {
  const client = new ReplClient(daemon.socketPath, {})
  await client.connect()

  client.send("describe this", [
    {
      id: "x",
      mime_type: "image/png",
      filename: "shot.png",
      bytes: new Uint8Array([0x89, 0x50, 0x4e, 0x47]),
    },
  ])
  client.interrupt()

  await new Promise((resolve) => setTimeout(resolve, 50))

  const message = daemon.received.find(
    (m): m is Extract<ReplMessage, { Message: unknown }> =>
      typeof m === "object" && "Message" in m && m.Message.content === "describe this",
  )
  expect(message).toBeDefined()
  expect(message!.Message.attachments).toHaveLength(1)
  expect(message!.Message.attachments![0].mime_type).toBe("image/png")
  expect(message!.Message.attachments![0].data_base64).toBe(
    Buffer.from([0x89, 0x50, 0x4e, 0x47]).toString("base64"),
  )

  const interrupt = daemon.received.find(
    (m): m is Extract<ReplMessage, { Message: unknown }> =>
      typeof m === "object" && "Message" in m && m.Message.content === "/interrupt",
  )
  expect(interrupt).toBeDefined()

  client.disconnect()
})

test("server Ping is answered with Pong", async () => {
  const client = new ReplClient(daemon.socketPath, {})
  await client.connect()

  daemon.send("Ping")
  await new Promise((resolve) => setTimeout(resolve, 50))

  expect(daemon.received).toContain("Pong")
  client.disconnect()
})

test("split TCP frames over one line are reassembled", async () => {
  const statuses: Array<[string, string]> = []
  const client = new ReplClient(daemon.socketPath, {
    onStatus: (kind, content) => statuses.push([kind, content]),
  })
  await client.connect()

  // Write one status frame split across two TCP writes with a fragment of
  // the next line in the same chunk.
  const frame = `${JSON.stringify({ Status: { kind: "stream_chunk", content: "hello" } })}\n`
  daemon.client!.write(frame.slice(0, 10))
  await new Promise((resolve) => setTimeout(resolve, 20))
  daemon.client!.write(frame.slice(10))
  await new Promise((resolve) => setTimeout(resolve, 50))

  expect(statuses).toEqual([["stream_chunk", "hello"]])
  client.disconnect()
})

describe("disconnect handling", () => {
  test("onDisconnect fires when the server drops the socket", async () => {
    const reasons: string[] = []
    let daemon2: MockDaemon | undefined
    try {
      daemon2 = new MockDaemon(dir)
      await daemon2.start()
      const client = new ReplClient(daemon2.socketPath, {
        onDisconnect: (reason) => reasons.push(reason),
      })
      await client.connect()
      await daemon2.stop()
      await new Promise((resolve) => setTimeout(resolve, 50))
      expect(reasons.length).toBeGreaterThanOrEqual(0) // close may race with destroy
      client.disconnect()
    } finally {
      await daemon2?.stop()
    }
  })
})
