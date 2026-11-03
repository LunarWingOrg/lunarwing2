/**
 * state/layout.ts computeLayout (design 3.12.1, 11.3.2; C8 and WI#458 item 6 at the pure level).
 */
import { describe, expect, test } from "bun:test"
import { computeLayout, layoutForState, type Layout, type LayoutInput } from "../../src/state/layout.ts"
import { createInitialState } from "../../src/state/model.ts"

const IDLE: Omit<LayoutInput, "width" | "height"> = { notices: 0, approvalPending: false, pendingImages: 0, composerLines: 1 }

function total(layout: Layout): number {
  return layout.header + layout.transcript + layout.approval + layout.notices + layout.attachments + layout.composer + layout.footer
}

describe("computeLayout", () => {
  test("rows at 80x24", () => {
    const layout = computeLayout({ width: 80, height: 24, ...IDLE })
    expect(layout).toEqual({
      header: 1,
      transcript: 19,
      approval: 0,
      notices: 0,
      attachments: 0,
      composer: 3,
      footer: 1,
      imageColumns: 56,
    })
    expect(total(layout)).toBe(24)

    const busy = computeLayout({ width: 80, height: 24, notices: 5, approvalPending: true, pendingImages: 2, composerLines: 2 })
    // 1 + 8 + 2 + 9 + 4 + 1 + 3 = 28 > 24: the footer goes first, then the image previews; the prompt keeps 8 rows
    expect(busy).toMatchObject({ approval: 8, notices: 2, attachments: 1, composer: 4, footer: 0, transcript: 8 })
    expect(total(busy)).toBe(24)
  })

  test("composer grows to 6 rows and the transcript shrinks", () => {
    const sizes = [1, 2, 3, 4, 5, 6, 9].map((composerLines) => computeLayout({ width: 80, height: 24, ...IDLE, composerLines }))
    expect(sizes.map((layout) => layout.composer)).toEqual([3, 4, 5, 6, 7, 8, 8])
    expect(sizes.map((layout) => layout.transcript)).toEqual([19, 18, 17, 16, 15, 14, 14])
    for (const layout of sizes) expect(total(layout)).toBe(24)
    // at most 3 text rows below 16 terminal rows
    expect(computeLayout({ width: 80, height: 15, ...IDLE, composerLines: 6 }).composer).toBe(5)
    expect(computeLayout({ width: 80, height: 16, ...IDLE, composerLines: 6 }).composer).toBe(8)
  })

  test("attachment bar collapses below 16 rows", () => {
    expect(computeLayout({ width: 80, height: 16, ...IDLE, pendingImages: 2 }).attachments).toBe(9)
    expect(computeLayout({ width: 80, height: 15, ...IDLE, pendingImages: 2 }).attachments).toBe(1)
    expect(computeLayout({ width: 80, height: 40, ...IDLE, pendingImages: 0 }).attachments).toBe(0)
  })

  test("approval compacts below 14 rows", () => {
    expect(computeLayout({ width: 80, height: 24, ...IDLE, approvalPending: true }).approval).toBe(8)
    expect(computeLayout({ width: 80, height: 13, ...IDLE, approvalPending: true }).approval).toBe(3)
    // 8 rows still fit at 15 once the footer is dropped; at 14 the transcript minimum compacts the prompt as well
    expect(computeLayout({ width: 80, height: 15, ...IDLE, approvalPending: true })).toMatchObject({ approval: 8, footer: 0, transcript: 3 })
    expect(computeLayout({ width: 80, height: 14, ...IDLE, approvalPending: true }).approval).toBe(3)
    for (let height = 10; height <= 40; height += 1) {
      const layout = computeLayout({ width: 80, height, notices: 2, approvalPending: true, pendingImages: 1, composerLines: 6 })
      expect(layout.transcript).toBeGreaterThanOrEqual(3)
      expect(total(layout)).toBe(height)
    }
    expect(computeLayout({ width: 80, height: 13, ...IDLE, notices: 3 }).notices).toBe(1)
    expect(computeLayout({ width: 80, height: 14, ...IDLE, notices: 3 }).notices).toBe(2)
  })

  test("transcript ≥ 3 rows at 40x10", () => {
    const quiet = computeLayout({ width: 40, height: 10, ...IDLE })
    expect(quiet).toMatchObject({ header: 1, composer: 3, footer: 1, transcript: 5, imageColumns: 32 })

    const crowded = computeLayout({ width: 40, height: 10, notices: 4, approvalPending: true, pendingImages: 3, composerLines: 6 })
    expect(crowded.transcript).toBeGreaterThanOrEqual(3)
    expect(total(crowded)).toBeLessThanOrEqual(10)
    // footer dropped, approval compact, composer down to one text row; the composer and the prompt stay
    expect(crowded).toMatchObject({ footer: 0, approval: 3, composer: 3 })

    // below 10 rows the footer is gone even without pressure
    expect(computeLayout({ width: 40, height: 9, ...IDLE }).footer).toBe(0)
    // a degenerate size still yields a usable shape
    const tiny = computeLayout({ width: 1, height: 1, ...IDLE })
    expect(tiny.transcript).toBe(3)
    expect(tiny.imageColumns).toBe(16)
  })

  test("layoutForState reads the store (credential mode shows one masked line)", () => {
    const state = createInitialState({ profile: "p", tenant: null, view: { width: 100, height: 30 }, at: 0 })
    const typing = { ...state, composer: { ...state.composer, lines: 4 } }
    expect(layoutForState(typing).composer).toBe(6)
    const credential = {
      ...typing,
      composer: {
        ...typing.composer,
        credential: { extensionName: "github", threadId: null, turnId: null, startedAt: 0, chars: 3, escArmedAt: null },
      },
    }
    expect(layoutForState(credential).composer).toBe(3)
    expect(layoutForState(state).imageColumns).toBe(56)
  })
})
