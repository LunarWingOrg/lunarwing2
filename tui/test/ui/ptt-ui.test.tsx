/**
 * Push-to-talk through the real app (design 7.4, 7.5, 11.3.4): one ManualClock shared by the renderer, the voice
 * controller and the fake recorder (which advances it 20 ms per played chunk), and a scripted fake source.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { ManualClock } from "@opentui/core/testing"
import { locate } from "../helpers/frames.ts"
import { renderApp, TEST_START_MS, type AppHarness } from "../helpers/render.tsx"
import { FakeRecorder } from "../mocks/fake-recorder.ts"
import { FakeSource } from "../mocks/fake-sources.ts"

let app: AppHarness | null = null

afterEach(async () => {
  await app?.dispose()
  app = null
})

interface Rig {
  app: AppHarness
  clock: ManualClock
  recorder: FakeRecorder
  source: FakeSource
}

async function rig(opts: { kittyKeyboard?: boolean; source?: FakeSource } = {}): Promise<Rig> {
  const clock = new ManualClock()
  clock.setTime(TEST_START_MS)
  const recorder = new FakeRecorder({ clock })
  const source = opts.source ?? new FakeSource({ id: "lunarvoice", clock, defaultReply: { text: "check the ports registry" } })
  const harness = await renderApp({ clock, recorders: [recorder], sources: [source], kittyKeyboard: opts.kittyKeyboard ?? false, width: 100, height: 24 })
  app = harness
  return { app: harness, clock, recorder, source }
}

const F9 = "\x1b[20~"
const F9_RELEASE = "\x1b[20;1:3~"

function stdin(harness: AppHarness, data: string | number[]): void {
  harness.setup.renderer.stdin.emit("data", typeof data === "string" ? Buffer.from(data) : Buffer.from(data))
}

async function micPosition(harness: AppHarness): Promise<{ x: number; y: number }> {
  const frame = await harness.render()
  const at = locate(frame, "( mic )")
  if (at === null) throw new Error(`mic widget not found:\n${frame}`)
  return { x: at.x + 2, y: at.y }
}

describe("push-to-talk in the app", () => {
  test("F9 toggle inserts the transcript into the composer, and the composer never receives F9", async () => {
    const { app: harness, recorder, source } = await rig()
    await harness.type("note:")
    await harness.press("F9")
    recorder.play(1_200)
    await harness.settle()
    expect(harness.store.getState().voice.mode).toBe("toggle")
    recorder.play(800)
    await harness.press("F9")
    await harness.advance(200, 50)
    expect(source.calls).toHaveLength(1)
    expect(harness.runtime.composer.getText()).toBe("note: check the ports registry")
    expect(harness.store.getState().voice.phase).toBe("idle")
  })

  test("NUL (Ctrl+Space inside tmux) behaves like the push-to-talk key", async () => {
    const { app: harness, recorder, source } = await rig()
    stdin(harness, [0x00])
    await harness.settle()
    recorder.play(1_200)
    stdin(harness, [0x00])
    await harness.advance(200, 50)
    expect(source.calls).toHaveLength(1)
    expect(harness.runtime.composer.getText()).toBe("check the ports registry")
  })

  test("a KKP release ends a hold", async () => {
    const { app: harness, recorder, source } = await rig({ kittyKeyboard: true })
    stdin(harness, F9)
    await harness.settle()
    recorder.play(700)
    stdin(harness, F9_RELEASE)
    await harness.advance(200, 50)
    expect(harness.store.getState().ui.kkpRelease).toBe(true)
    expect(source.calls).toHaveLength(1)
    expect(harness.runtime.composer.getText()).toBe("check the ports registry")
  })

  test("a KKP Ctrl+Space hold ends when Ctrl is lifted before Space; the plain-space repeats never reach the composer", async () => {
    // kitty (flags 7): press ESC[32;5u, repeat ESC[32;5:2u; with Ctrl lifted the repeats are plain " " and the
    // release is ESC[32;1:3u, without the ctrl modifier.
    const { app: harness, recorder, source } = await rig({ kittyKeyboard: true })
    stdin(harness, F9)
    await harness.settle()
    stdin(harness, F9_RELEASE) // a tap: toggle mode, and KKP releases are now confirmed
    recorder.play(600)
    await harness.press("F9")
    await harness.advance(200, 50)
    expect(source.calls).toHaveLength(1)
    harness.runtime.composer.clear()

    stdin(harness, "\x1b[32;5u")
    await harness.settle()
    recorder.play(400)
    stdin(harness, "\x1b[32;5:2u")
    stdin(harness, "\x1b[32;5:2u")
    await harness.settle()
    expect(harness.store.getState().voice.mode).toBe("hold-key")
    stdin(harness, "   ")
    await harness.settle()
    recorder.play(400)
    stdin(harness, "\x1b[32;1:3u")
    await harness.advance(200, 50)
    expect(harness.store.getState().voice.phase).toBe("idle")
    expect(source.calls).toHaveLength(2)
    expect(harness.runtime.composer.getText()).toBe("check the ports registry")
  })

  test("a space released while an F9 recording runs is not taken for the PTT release", async () => {
    const { app: harness, recorder } = await rig({ kittyKeyboard: true })
    stdin(harness, F9)
    await harness.settle()
    recorder.play(400)
    stdin(harness, "\x1b[32;1:3u")
    await harness.advance(100, 50)
    expect(harness.store.getState().voice.phase).toBe("recording")
  })

  test("a mouse hold on the widget ends on a release outside the widget", async () => {
    const { app: harness, recorder, source } = await rig()
    const mic = await micPosition(harness)
    await harness.setup.mockMouse.pressDown(mic.x, mic.y)
    await harness.settle()
    recorder.play(700)
    await harness.settle()
    expect(harness.store.getState().voice.mode).toBe("hold-mouse")
    await harness.setup.mockMouse.release(60, 3)
    await harness.advance(200, 50)
    expect(source.calls).toHaveLength(1)
  })

  test("a mouse click toggles", async () => {
    const { app: harness, recorder, source } = await rig()
    const mic = await micPosition(harness)
    await harness.setup.mockMouse.click(mic.x, mic.y)
    await harness.settle()
    recorder.play(1_000)
    await harness.settle()
    expect(harness.store.getState().voice.mode).toBe("toggle")
    const frame = await harness.render()
    expect(frame).toMatch(/REC 0:0\d - F9 stop - Esc cancel/u)
    // The widget keeps its place; its label is now "REC 0:01".
    await harness.setup.mockMouse.click(mic.x, mic.y)
    await harness.advance(200, 50)
    expect(source.calls).toHaveLength(1)
  })

  test("blur ends a mouse hold", async () => {
    const { app: harness, recorder, source } = await rig()
    const mic = await micPosition(harness)
    await harness.setup.mockMouse.pressDown(mic.x, mic.y)
    await harness.settle()
    recorder.play(700)
    stdin(harness, "\x1b[O")
    await harness.advance(200, 50)
    expect(harness.store.getState().ui.blurred).toBe(true)
    expect(source.calls).toHaveLength(1)
  })

  test("Esc during transcribing aborts the request", async () => {
    const clock = new ManualClock()
    clock.setTime(TEST_START_MS)
    const source = new FakeSource({ id: "lunarvoice", clock, defaultReply: { hang: true } })
    const { app: harness, recorder } = await rig({ source })
    await harness.press("F9")
    recorder.play(1_200)
    await harness.press("F9")
    await harness.advance(100, 50)
    expect(source.calls).toHaveLength(1)
    const frame = await harness.render()
    expect(frame).toContain("transcribing")
    expect(frame).toContain("[ .... ]")
    await harness.escape()
    expect(source.calls[0]?.signal.aborted).toBe(true)
    expect(harness.store.getState().voice.queue).toHaveLength(0)
    expect(harness.runtime.composer.getText()).toBe("")
  })

  test("the VU meter renders levels from the fake recorder", async () => {
    const { app: harness, recorder } = await rig()
    await harness.press("F9")
    recorder.play(600)
    await harness.settle()
    const frame = await harness.render()
    // The composer row: [mic widget "REC 0:00", 10 columns][12 VU cells][textarea].
    const row = frame.split("\n").find((line) => line.startsWith("│REC ")) ?? ""
    expect(row).toMatch(/^│REC 0:0\d {2}[=|]+\.*/u)
  })
})
