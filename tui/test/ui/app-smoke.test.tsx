/**
 * Full-app smoke test (ported from v4:tui/test/app-smoke.test.tsx, design 11.3.4): the real <App> and runtime
 * against a FakeTransport. The welcome block renders, a message is sent, and a streamed reply survives an empty
 * final (the never-wipe invariant, now through state/turn-text.ts). Time is a ManualClock; nothing sleeps.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { FakeTransport, turnId } from "../mocks/fake-transport.ts"
import { renderApp, withMic, type AppHarness } from "../helpers/render.tsx"
import { rowOf } from "../helpers/frames.ts"

let app: AppHarness | null = null

afterEach(async () => {
  await app?.dispose()
  app = null
})

describe("App end-to-end smoke", () => {
  test("welcome renders, message sends, streamed turn survives an empty final", async () => {
    const transport = new FakeTransport({ mode: "current" })
    // A working mic: the idle footer offers its push-to-talk key ("F9 dictate", asserted below).
    app = await renderApp({ ...withMic(), transport, width: 60, height: 16 })
    const welcome = await app.render()
    expect(welcome).toContain("LunarWing derpy")
    expect(welcome).toContain("F1 help")
    expect(welcome).toContain("exact")

    await app.type("hello daemon")
    await app.enter()
    expect(transport.sentTexts).toEqual(["hello daemon"])
    const id = turnId("t-1")
    expect(transport.callsOf("send")[0]?.req.turnId).toBe(id)

    await app.emit(
      { kind: "turn.started", turnId: id },
      { kind: "turn.delta", turnId: id, text: "streamed " },
      { kind: "turn.delta", turnId: id, text: "answer text" },
    )
    const live = await app.render()
    expect(live).toContain("streamed answer text")

    // The empty final closes the turn without wiping the streamed text.
    await app.emit({ kind: "turn.final", turnId: id, text: "" }, { kind: "turn.finished", turnId: id, outcome: "replied" })
    const done = await app.render()
    expect(done).toContain("streamed answer text")
    expect(app.store.getState().turns[id]?.phase).toBe("done")
    expect(done).toContain("you ▏ hello daemon")
    // The composer is empty again and the idle footer is back.
    expect(done).toContain("F9 dictate")
    expect(rowOf(done, "you ▏ hello daemon")).toBeLessThan(rowOf(done, "streamed answer text"))
  })
})
