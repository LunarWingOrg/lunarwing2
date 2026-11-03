/**
 * Layout and resize (design 3.12.1, WI#458 item 6, C5, C8, 11.3.4): the header is row 0, the footer the last
 * row, the composer border is visible with the newest turn right above it, at every size; live multi-line text
 * is fully visible; manual scroll-up is kept while content arrives and PageDown to the end follows again.
 */
import { afterEach, describe, expect, test } from "bun:test"
import type { PendingImage } from "../../src/state/model.ts"
import { lastContentRow, rowOf, rows } from "../helpers/frames.ts"
import { renderApp, withMic, type AppHarness } from "../helpers/render.tsx"
import { FakeTransport, turnId } from "../mocks/fake-transport.ts"
import { TINY_PNG } from "../mocks/mock-gateway.ts"

const PAGE_UP = "\x1b[5~"
const PAGE_DOWN = "\x1b[6~"

let app: AppHarness | null = null

afterEach(async () => {
  await app?.dispose()
  app = null
})

/** Row of the composer's top border: the last "┌" row above the bottom "└" border of the composer. */
function composerTop(frame: string): number {
  const all = rows(frame)
  const bottom = all.findLastIndex((row) => row.startsWith("└"))
  return all.slice(0, bottom).findLastIndex((row) => row.startsWith("┌"))
}

function expectInvariants(frame: string, height: number, newest: string): void {
  const all = rows(frame)
  expect(all).toHaveLength(height)
  expect(all[0]).toStartWith("LunarWing derpy")
  const top = composerTop(frame)
  expect(top).toBeGreaterThan(0)
  const bottom = all.findLastIndex((row) => row.startsWith("└"))
  expect(bottom).toBeGreaterThan(top)
  const newestRow = rowOf(frame, newest)
  expect(newestRow).toBeGreaterThan(0)
  expect(newestRow).toBeLessThan(top)
  // The newest turn sits right above the composer (one blank separator row at most).
  expect(top - newestRow).toBeLessThanOrEqual(3)
  if (height >= 10) {
    expect(bottom).toBe(height - 2)
    expect(lastContentRow(frame)).toBe(height - 1)
    expect(all[height - 1]).toStartWith("F9 dictate")
  }
}

async function oneTurn(harness: AppHarness, reply: string): Promise<void> {
  await harness.type("ping")
  await harness.enter()
  const state = harness.store.getState()
  const id = state.order.at(-1) ?? turnId("t-1")
  await harness.emit({ kind: "turn.final", turnId: id, text: reply }, { kind: "turn.finished", turnId: id, outcome: "replied" })
}

describe("layout", () => {
  test("80x24, then 50x12 and 120x40: header, transcript, composer and footer keep their places", async () => {
    // A working mic: the idle footer row starts with its push-to-talk key (expectInvariants).
    app = await renderApp({ ...withMic(), transport: new FakeTransport(), width: 80, height: 24 })
    await oneTurn(app, "the newest answer")
    expectInvariants(await app.render(), 24, "the newest answer")

    app.setup.resize(50, 12)
    expectInvariants(await app.render(), 12, "the newest answer")
    expect(app.store.getState().view).toEqual({ width: 50, height: 12 })

    app.setup.resize(120, 40)
    expectInvariants(await app.render(), 40, "the newest answer")
  })

  test("a three-line live reply is fully visible (C5)", async () => {
    const transport = new FakeTransport()
    app = await renderApp({ transport, width: 80, height: 24 })
    await app.type("count")
    await app.enter()
    const id = turnId("t-1")
    await app.emit({ kind: "turn.started", turnId: id }, { kind: "turn.delta", turnId: id, text: "first line\nsecond line\nthird line" })
    const frame = await app.render()
    const first = rowOf(frame, "first line")
    expect(rowOf(frame, "second line")).toBe(first + 1)
    expect(rowOf(frame, "third line")).toBe(first + 2)
    expect(rowOf(frame, "third line")).toBeLessThan(composerTop(frame))
  })

  test("manual scroll-up is kept while content arrives; PageDown to the end follows again", async () => {
    const transport = new FakeTransport()
    app = await renderApp({ transport, width: 80, height: 24 })
    for (let index = 0; index < 30; index += 1) {
      await app.emit({ kind: "background", text: `background message ${index}` })
    }
    let frame = await app.render()
    expect(frame).toContain("background message 29")

    await app.press(PAGE_UP)
    await app.press(PAGE_UP)
    frame = await app.render()
    expect(frame).not.toContain("background message 29")

    await app.emit({ kind: "background", text: "background message 30" })
    frame = await app.render()
    expect(frame).not.toContain("background message 30")

    for (let index = 0; index < 6; index += 1) await app.press(PAGE_DOWN)
    frame = await app.render()
    expect(frame).toContain("background message 30")
    await app.emit({ kind: "background", text: "background message 31" })
    frame = await app.render()
    expect(frame).toContain("background message 31")
  })

  test("at 50x12 pending attachments collapse to one line and the composer stays visible", async () => {
    app = await renderApp({ width: 50, height: 12 })
    const image: PendingImage = { id: "img-1", mime: "image/png", bytes: TINY_PNG, filename: "shot.png", width: 1, height: 1, source: "file" }
    app.store.dispatch({ type: "composer/attach", image, at: app.clock.now() })
    const frame = await app.render()
    expect(frame).toContain("[1 image]")
    expect(composerTop(frame)).toBeGreaterThan(rowOf(frame, "[1 image]"))
    expect(rows(frame)).toHaveLength(12)
  })
})
