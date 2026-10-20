/**
 * Full-app smoke test: mounts the real <App> against a mock daemon socket
 * and drives one streamed turn end-to-end, including the never-wipe done
 * frame invariant through the actual UI wiring.
 *
 * Note on pacing: socket-driven state updates land outside act(), and the
 * test renderer paints the input one pass late — so every step is paced
 * with sleep+flush polling rather than strict frame counts.
 */

import { afterAll, beforeAll, describe, expect, test } from "bun:test"
import { createServer, type Server, type Socket } from "node:net"
import { mkdtempSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { testRender } from "@opentui/react/test-utils"
import type { TestRendererSetup } from "@opentui/core/testing"
import type { ReplMessage } from "../src/protocol.ts"
import { App } from "../src/app.tsx"

const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms))

let server: Server
let client: Socket | null = null
let socketPath: string
/** Messages received by the mock daemon (client → server). */
let receivedMessages: string[] = []

beforeAll(async () => {
  const dir = mkdtempSync(join(tmpdir(), "lw-app-smoke-"))
  socketPath = join(dir, "lunarwing.sock")
  server = createServer((socket) => {
    client = socket
    let buffer = ""
    socket.on("data", (chunk: Buffer) => {
      buffer += chunk.toString("utf8")
      let newline = buffer.indexOf("\n")
      while (newline !== -1) {
        const line = buffer.slice(0, newline).trim()
        buffer = buffer.slice(newline + 1)
        if (line.length > 0) {
          const message = JSON.parse(line) as ReplMessage
          if (typeof message === "object" && "Connect" in message) {
            send({
              Response: {
                content: "welcome to mock daemon",
                session_id: "smoke-1",
                is_complete: true,
              },
            })
          }
          if (typeof message === "object" && "Message" in message) {
            receivedMessages.push(message.Message.content)
            void replyTurn()
          }
        }
        newline = buffer.indexOf("\n")
      }
    })
  })
  await new Promise<void>((resolve) => server.listen(socketPath, resolve))
})

afterAll(async () => {
  client?.destroy()
  await new Promise<void>((resolve) => server.close(() => resolve()))
})

function send(message: ReplMessage): void {
  client?.write(`${JSON.stringify(message)}\n`)
}

/** Simulate a streamed turn: thinking → chunks → done (empty payload). */
async function replyTurn(): Promise<void> {
  send({ Status: { kind: "thinking", content: "pondering", session_id: "smoke-1" } })
  await sleep(30)
  send({ Status: { kind: "stream_chunk", content: "streamed ", session_id: "smoke-1" } })
  await sleep(30)
  send({ Status: { kind: "stream_chunk", content: "answer text", session_id: "smoke-1" } })
  await sleep(30)
  // Done frame with EMPTY payload: must keep the accumulated chunks.
  send({ Response: { content: "", session_id: "smoke-1", is_complete: true } })
}

async function waitForFrame(
  setup: TestRendererSetup,
  predicate: (frame: string) => boolean,
  tries = 30,
): Promise<string> {
  for (let i = 0; i < tries; i++) {
    await sleep(20)
    await setup.flush()
    const frame = setup.captureCharFrame()
    if (predicate(frame)) {
      return frame
    }
  }
  return `TIMEOUT: ${setup.captureCharFrame()}`
}

describe("App end-to-end smoke", () => {
  test("welcome renders, message sends, streamed turn survives empty done frame", async () => {
    const setup = await testRender(<App socketPath={socketPath} agentName="LunarWingTest" />, {
      width: 60,
      height: 16,
    })
    try {
      await setup.renderOnce()
      const welcome = await waitForFrame(setup, (frame) => frame.includes("welcome to mock daemon"))
      expect(welcome.startsWith("TIMEOUT")).toBe(false)

      // Type with flush pacing so each char commits before the next.
      for (const character of "hello daemon") {
        await setup.mockInput.typeText(character)
        await setup.flush()
      }
      setup.mockInput.pressEnter()
      await setup.flush()

      // The daemon received the full text.
      const sent = await waitForFrame(setup, () => receivedMessages.includes("hello daemon"))
      expect(sent.startsWith("TIMEOUT")).toBe(false)

      // Live status and streamed text appear; the empty done frame must
      // NOT wipe the accumulated turn text (hard invariant).
      const sealed = await waitForFrame(setup, (frame) => frame.includes("streamed answer text"))
      expect(sealed.startsWith("TIMEOUT")).toBe(false)

      // After the done frame lands, the turn is sealed and we're ready
      // again — with the streamed text still on screen.
      const done = await waitForFrame(
        setup,
        (frame) => frame.includes("ready") && frame.includes("streamed answer text"),
      )
      expect(done.startsWith("TIMEOUT")).toBe(false)
    } finally {
      setup.renderer.destroy()
    }
  })
})
