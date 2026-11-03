/**
 * The info panel (design 3.12.3, UX-04, 11.3.4): F1 opens help and Esc closes the panel before it cancels
 * dictation; /voice status lists every source with its reason; /thread list renders in the panel; PageDown
 * scrolls the panel.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { ManualClock } from "@opentui/core/testing"
import type { ThreadRow } from "../../src/controller/panel-content.ts"
import { renderApp, TEST_START_MS, type AppHarness } from "../helpers/render.tsx"
import { FakeTransport, FAKE_THREAD_ID } from "../mocks/fake-transport.ts"
import { FakeRecorder } from "../mocks/fake-recorder.ts"
import { FakeSource } from "../mocks/fake-sources.ts"

const PAGE_DOWN = "\x1b[6~"

let app: AppHarness | null = null

afterEach(async () => {
  await app?.dispose()
  app = null
})

function manualClock(): ManualClock {
  const clock = new ManualClock()
  clock.setTime(TEST_START_MS)
  return clock
}

class ListingTransport extends FakeTransport {
  threads: ThreadRow[] = [
    { id: FAKE_THREAD_ID, title: "ports registry cleanup", state: "Idle", turnCount: 4, updatedAt: "2026-11-03T08:15:00Z", assistant: false },
    { id: "11111111-2222-4333-8444-555555555555", title: null, state: "Idle", turnCount: 12, updatedAt: "2026-11-02T21:00:00Z", assistant: true },
  ]

  async listThreads(): Promise<ThreadRow[]> {
    return this.threads
  }
}

describe("info panel", () => {
  test("F1 opens help, and Esc closes it before cancelling dictation", async () => {
    const clock = manualClock()
    const recorder = new FakeRecorder({ clock })
    const source = new FakeSource({ id: "lunarvoice", clock })
    app = await renderApp({ clock, recorders: [recorder], sources: [source], width: 100, height: 30 })
    await app.press("F9")
    recorder.play(400)
    await app.settle()
    expect(app.store.getState().voice.phase).toBe("recording")

    await app.press("F1")
    let frame = await app.render()
    expect(app.store.getState().panel?.kind).toBe("help")
    expect(frame).toContain("push-to-talk")
    expect(frame).toContain("Ctrl+J, Alt+Enter, Shift+Enter")

    await app.escape()
    frame = await app.render()
    expect(app.store.getState().panel).toBeNull()
    expect(app.store.getState().voice.phase).toBe("recording")

    await app.escape()
    expect(app.store.getState().voice.phase).toBe("idle")
    expect(recorder.cancels).toBe(1)
  })

  test("/voice status lists every source with its reason", async () => {
    const clock = manualClock()
    const lunarvoice = new FakeSource({ id: "lunarvoice", clock })
    const hexApp = new FakeSource({ id: "hex-app", clock, available: false, reason: "Hex not running" })
    app = await renderApp({ clock, recorders: [new FakeRecorder({ clock })], sources: [lunarvoice, hexApp], width: 100, height: 40 })
    await app.advance(500)
    await app.type("/voice status")
    await app.enter()
    await app.advance(500)
    const frame = await app.render()
    expect(app.store.getState().panel?.kind).toBe("voice")
    expect(frame).toContain("lunarvoice (fake lunarvoice): available")
    expect(frame).toContain("hex-app (fake hex-app): unavailable: Hex not running")
    expect(frame).toContain("hex-embedded: not in this build")
    expect(frame).toContain("Recorder: opentui")
  })

  test("/thread list renders in the panel", async () => {
    app = await renderApp({ transport: new ListingTransport(), width: 100, height: 30 })
    await app.type("/thread list")
    await app.enter()
    const frame = await app.render()
    expect(app.store.getState().panel?.kind).toBe("threads")
    expect(frame).toContain("* 00000000 ports registry cleanup - Idle, 4 turns, 2026-11-03 08:15")
    expect(frame).toContain("11111111 [assistant] - Idle, 12 turns")
  })

  test("/thread <prefix> switches to the matching thread", async () => {
    const transport = new ListingTransport()
    app = await renderApp({ transport })
    await app.type("/thread 1111")
    await app.enter()
    expect(transport.callsOf("selectThread").map((call) => call.spec)).toEqual([{ kind: "id", id: "11111111-2222-4333-8444-555555555555" }])
    expect(app.store.getState().threadId).toBe("11111111-2222-4333-8444-555555555555")
  })

  test("PageDown scrolls the panel", async () => {
    app = await renderApp({ width: 90, height: 16 })
    await app.press("F1")
    let frame = await app.render()
    const firstBefore = frame.includes("Keys")
    expect(firstBefore).toBe(true)
    await app.press(PAGE_DOWN)
    frame = await app.render()
    expect(app.store.getState().panel?.scroll).toBeGreaterThan(0)
    expect(frame).not.toContain("│ Keys")
  })
})
