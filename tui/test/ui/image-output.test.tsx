/**
 * Generated images (design 6.4, 3.7 UX-03, 11.3.4; ported from v4:tui/test/image-output.test.tsx).
 *
 * - A generated image renders while the turn is live and survives the sealing final (the vibetui4 regression:
 *   images used to be dropped with the live turn).
 * - A late delta for the sealed turn never resurrects it.
 * - Inverted from v4:tui/test/image-output.test.tsx:196: a later final with no open turn is no longer swallowed;
 *   it is shown as a background message (3.5 rules 5d and 6).
 * - On a daemon that cannot deliver images, an image tool that completed without an image event adds the note.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { NOTES } from "../../src/state/correlate.ts"
import { renderApp, type AppHarness } from "../helpers/render.tsx"
import { FakeTransport, turnId } from "../mocks/fake-transport.ts"
import { TINY_PNG } from "../mocks/mock-gateway.ts"

const CAPTION = "sunset.png"

let app: AppHarness | null = null

afterEach(async () => {
  await app?.dispose()
  app = null
})

describe("generated images", () => {
  test("a generated image renders live and survives the final", async () => {
    const transport = new FakeTransport({ mode: "current" })
    app = await renderApp({ transport, width: 80, height: 30 })
    await app.type("draw me a sunset")
    await app.enter()
    const id = turnId("t-1")
    await app.emit(
      { kind: "turn.started", turnId: id },
      { kind: "turn.activity", turnId: id, activity: { kind: "thinking", text: "painting" } },
      { kind: "turn.image", turnId: id, image: { mime: "image/png", bytes: TINY_PNG, caption: CAPTION } },
    )
    const live = await app.render()
    expect(live).toContain(CAPTION)
    expect(app.store.getState().turns[id]?.phase).toBe("running")

    await app.emit({ kind: "turn.final", turnId: id, text: "" }, { kind: "turn.finished", turnId: id, outcome: "replied" })
    const sealed = await app.render()
    expect(app.store.getState().turns[id]?.phase).toBe("done")
    expect(sealed).toContain(CAPTION)

    // A late chunk for the sealed turn does not resurrect it.
    await app.emit({ kind: "turn.delta", turnId: id, text: "zombie chunk" })
    // Inverted v4:196: a final with no open turn is shown as a background message, not dropped.
    await app.emit({ kind: "turn.final", turnId: null, text: "duplicate done payload" })
    const after = await app.render()
    expect(after).not.toContain("zombie chunk")
    expect(after).toContain("duplicate done payload")
    expect(after).toContain("(background)")
  })

  test("an image tool without an image event shows the note on a daemon that cannot deliver images", async () => {
    const transport = new FakeTransport({ mode: "legacy" })
    app = await renderApp({ transport, width: 100, height: 30 })
    await app.type("make a logo")
    await app.enter()
    await app.emit(
      { kind: "turn.activity", turnId: null, activity: { kind: "tool_started", text: "image_generate" } },
      { kind: "turn.activity", turnId: null, activity: { kind: "tool_completed", text: "image_generate (812ms)", ok: true } },
      { kind: "turn.final", turnId: null, text: "Here is your logo." },
    )
    const frame = await app.render()
    expect(frame).toContain("Here is your logo.")
    expect(frame).toContain(NOTES.imageNotDelivered)
  })
})
