/**
 * Generated-image pipeline: a daemon `image_generated` status frame must render
 * inline in the transcript and SURVIVE the turn-sealing done frame.
 *
 * Regression: images were accumulated into the live turn and then discarded by
 * `setLive(null)` at the end of the turn, so a generated image was decoded and
 * thrown away — visible for zero frames. These tests pin the handoff from the
 * live turn to the sealed transcript item.
 *
 * The renderer is asked for `protocol: "auto"`; under the test renderer that
 * resolves outside Kitty/Sixel, so the char frame is assertable through the
 * caption line each image block carries.
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
import { Transcript } from "../src/components/transcript.tsx"

const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms))

/** 1x1 RGBA PNG — the smallest thing the image loader accepts. */
const PNG_BASE64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg=="
const PNG_DATA_URL = `data:image/png;base64,${PNG_BASE64}`
const PNG_BYTES = new Uint8Array(Buffer.from(PNG_BASE64, "base64"))

const IMAGE_CAPTION = "sunset.png"

let server: Server
let client: Socket | null = null
let socketPath: string
let receivedMessages: string[] = []

beforeAll(async () => {
  const dir = mkdtempSync(join(tmpdir(), "lw-image-output-"))
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
                session_id: "img-1",
                is_complete: true,
              },
            })
          }
          if (typeof message === "object" && "Message" in message) {
            receivedMessages.push(message.Message.content)
            void replyWithImage()
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

/** thinking → image_generated → done with an EMPTY payload. */
async function replyWithImage(): Promise<void> {
  send({ Status: { kind: "thinking", content: "painting", session_id: "img-1" } })
  await sleep(30)
  send({
    Status: {
      kind: "image_generated",
      content: IMAGE_CAPTION,
      data: { data_url: PNG_DATA_URL, path: `/tmp/${IMAGE_CAPTION}` },
      session_id: "img-1",
    },
  })
  await sleep(60)
  send({ Response: { content: "", session_id: "img-1", is_complete: true } })
}

async function waitForFrame(
  setup: TestRendererSetup,
  predicate: (frame: string) => boolean,
  tries = 40,
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

describe("Transcript inline images", () => {
  test("a sealed transcript item renders its image caption", async () => {
    const setup = await testRender(
      <Transcript
        items={[
          {
            id: "a1",
            kind: "assistant",
            text: "here is the picture",
            images: [{ id: "g1", bytes: PNG_BYTES, caption: IMAGE_CAPTION }],
          },
        ]}
        live={null}
        height={24}
        width={80}
      />,
      { width: 80, height: 24 },
    )
    try {
      await setup.renderOnce()
      const frame = setup.captureCharFrame()
      expect(frame).toContain("here is the picture")
      expect(frame).toContain(IMAGE_CAPTION)
    } finally {
      setup.renderer.destroy()
    }
  })
})

describe("App generated-image pipeline", () => {
  test("image_generated renders live and survives the done frame", async () => {
    receivedMessages = []
    const setup = await testRender(<App socketPath={socketPath} agentName="LunarWingTest" />, {
      width: 80,
      height: 30,
    })
    try {
      await setup.renderOnce()
      const welcome = await waitForFrame(setup, (frame) =>
        frame.includes("welcome to mock daemon"),
      )
      expect(welcome.startsWith("TIMEOUT")).toBe(false)

      for (const character of "draw me a sunset") {
        await setup.mockInput.typeText(character)
        await setup.flush()
      }
      setup.mockInput.pressEnter()
      await setup.flush()

      const sent = await waitForFrame(setup, () => receivedMessages.includes("draw me a sunset"))
      expect(sent.startsWith("TIMEOUT")).toBe(false)

      // The image arrives mid-turn and must be visible while the turn is live.
      const liveFrame = await waitForFrame(setup, (frame) => frame.includes(IMAGE_CAPTION))
      expect(liveFrame.startsWith("TIMEOUT")).toBe(false)

      // After the EMPTY done frame seals the turn, the image must still be
      // there AND the app must be ready again. This is the regression: the
      // sealed item used to carry no images and setLive(null) binned them.
      const sealed = await waitForFrame(
        setup,
        (frame) => frame.includes("ready") && frame.includes(IMAGE_CAPTION),
      )
      expect(sealed.startsWith("TIMEOUT")).toBe(false)
    } finally {
      setup.renderer.destroy()
    }
  })
})
