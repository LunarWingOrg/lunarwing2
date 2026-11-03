/**
 * The voice status line and outcome notices (design 7.4, 5.4, 5.4.1, UX-03, 11.3.4) in the real app: toggle and
 * hold labels differ, the REC timer shows at 80x24, a busy retry counts down, a fallback success names its source
 * and reason, an offline gateway refuses a PTT start with the reason, and the end of the chain raises one notice
 * and keeps the clip.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { ManualClock } from "@opentui/core/testing"
import { TransportError } from "../../src/transport/errors.ts"
import { renderApp, TEST_START_MS, type AppHarness, type RenderAppOptions } from "../helpers/render.tsx"
import { FakeTransport } from "../mocks/fake-transport.ts"
import { FakeRecorder } from "../mocks/fake-recorder.ts"
import { FakeSource } from "../mocks/fake-sources.ts"

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

async function start(clock: ManualClock, recorder: FakeRecorder, sources: FakeSource[], opts: RenderAppOptions = {}): Promise<AppHarness> {
  app = await renderApp({ clock, recorders: [recorder], sources, width: 80, height: 24, ...opts })
  await app.advance(100, 50)
  return app
}

/** Tap F9, record `ms` of tone, tap F9 again. */
async function dictate(harness: AppHarness, recorder: FakeRecorder, ms = 1_500): Promise<void> {
  await harness.press("F9")
  recorder.play(ms)
  await harness.press("F9")
  await harness.advance(100, 50)
}

function notices(harness: AppHarness): string[] {
  return harness.store.getState().notices.map((notice) => notice.text)
}

describe("voice status", () => {
  test("toggle and hold labels differ", async () => {
    const clock = manualClock()
    const recorder = new FakeRecorder({ clock })
    const harness = await start(clock, recorder, [new FakeSource({ id: "lunarvoice", clock })])
    await harness.press("F9")
    recorder.play(1_000)
    let frame = await harness.render()
    expect(frame).toMatch(/REC 0:0\d - F9 stop - Esc cancel/u)
    await harness.escape()

    // Held inside tmux: the key repeats as plain presses; repeats decide the mode.
    await harness.advance(500)
    await harness.press("F9")
    for (let index = 0; index < 10; index += 1) {
      recorder.play(100)
      await harness.press("F9")
    }
    frame = await harness.render()
    expect(harness.store.getState().voice.mode).toBe("hold-inferred")
    expect(frame).toMatch(/REC 0:0\d - release to stop - Esc cancel/u)
  })

  test("the REC timer is visible at 80x24", async () => {
    const clock = manualClock()
    const recorder = new FakeRecorder({ clock })
    const harness = await start(clock, recorder, [new FakeSource({ id: "lunarvoice", clock })])
    await harness.press("F9")
    recorder.play(7_100)
    const frame = await harness.render()
    expect(frame).toContain("REC 0:07 - F9 stop - Esc cancel")
    expect(frame.split("\n").some((row) => row.startsWith("│REC 0:07"))).toBe(true)
  })

  test("a busy retry counts down", async () => {
    const clock = manualClock()
    const recorder = new FakeRecorder({ clock })
    const source = new FakeSource({ id: "lunarvoice", clock, replies: [{ error: "busy" }, { text: "done at last" }] })
    const harness = await start(clock, recorder, [source])
    await dictate(harness, recorder)
    const frame = await harness.render()
    expect(frame).toMatch(/lunarvoice busy - retry 1\/3 in 1 s/u)
    await harness.advance(1_500)
    expect(harness.runtime.composer.getText()).toBe("done at last")
  })

  test("a fallback success names the source and the reason", async () => {
    const clock = manualClock()
    const recorder = new FakeRecorder({ clock })
    const lunarvoice = new FakeSource({ id: "lunarvoice", clock, defaultReply: { error: "unavailable", message: "sidecar down" } })
    const hex = new FakeSource({ id: "hex-app", clock, defaultReply: { text: "from hex" } })
    const harness = await start(clock, recorder, [lunarvoice, hex])
    await dictate(harness, recorder)
    await harness.advance(500)
    expect(harness.runtime.composer.getText()).toBe("from hex")
    const notice = notices(harness).find((text) => text.startsWith("transcribed by"))
    expect(notice).toMatch(/^transcribed by hex-app \(lunarvoice: .+\)$/u)
  })

  test("an offline gateway refuses a PTT start with the reason", async () => {
    const clock = manualClock()
    const recorder = new FakeRecorder({ clock })
    const transport = new FakeTransport()
    transport.connectScript.push(new TransportError("auth", "the gateway rejected the token"))
    const harness = await start(clock, recorder, [], { transport, buildSources: true })
    expect(harness.store.getState().conn.kind).toBe("offline")
    await harness.press("F9")
    await harness.advance(100, 50)
    expect(harness.store.getState().voice.phase).toBe("idle")
    expect(notices(harness).some((text) => text.startsWith("voice unavailable") && text.includes("not connected"))).toBe(true)
    expect(recorder.startCalls).toHaveLength(0)
    const frame = await harness.render()
    expect(frame).toContain("(no mic)")
  })

  test("unavailable at the end of the chain raises one notice and keeps the clip", async () => {
    const clock = manualClock()
    const recorder = new FakeRecorder({ clock })
    const source = new FakeSource({ id: "lunarvoice", clock, defaultReply: { error: "unavailable", message: "sidecar down" } })
    const harness = await start(clock, recorder, [source])
    await dictate(harness, recorder)
    await harness.advance(500)
    const failures = notices(harness).filter((text) => text.startsWith("voice failed"))
    expect(failures).toHaveLength(1)
    expect(failures[0]).toContain("Alt+R retry, Esc discard")
    expect(harness.store.getState().voice.retained).not.toBeNull()
    expect(harness.runtime.composer.getText()).toBe("")
  })

  test("the kept-clip notice shows its keys at 80 columns and stays while the clip is kept; Alt+R takes it away", async () => {
    const clock = manualClock()
    const recorder = new FakeRecorder({ clock })
    const lunarvoice = new FakeSource({ id: "lunarvoice", clock, defaultReply: { error: "unavailable", message: "sidecar down" } })
    const hexApp = new FakeSource({ id: "hex-app", clock, defaultReply: { error: "unavailable", message: "hex-app not running" } })
    const hexEmbedded = new FakeSource({ id: "hex-embedded", clock, defaultReply: { error: "unavailable", message: "hex-embedded not installed" } })
    const harness = await start(clock, recorder, [lunarvoice, hexApp, hexEmbedded])
    await dictate(harness, recorder)
    await harness.advance(500)
    let frame = await harness.render()
    const row = frame.split("\n").find((line) => line.includes("voice failed")) ?? ""
    expect(row).toContain("Alt+R retry, Esc discard")
    await harness.advance(30_000, 1_000)
    expect(notices(harness).some((text) => text.includes("Alt+R retry, Esc discard"))).toBe(true)
    frame = await harness.render()
    expect(frame).toContain("Alt+R retry, Esc discard")

    lunarvoice.reply({ text: "kept words" })
    await harness.press("r", { meta: true })
    await harness.advance(500)
    expect(notices(harness).some((text) => text.includes("Alt+R retry"))).toBe(false)
    expect(harness.runtime.composer.getText()).toBe("kept words")
  })
})
