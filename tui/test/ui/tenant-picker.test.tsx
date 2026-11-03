/**
 * The tenant picker (design 3.11.5, UX-10, 11.3.4): an OpenTUI <select> listing tenant, URL, token kind and
 * expiry; the lastTenant entry is preselected; Enter picks, Esc quits (null).
 */
import { describe, expect, test } from "bun:test"
import { ManualClock } from "@opentui/core/testing"
import { testRender } from "@opentui/react/test-utils"
import { act } from "react"
import type { TenantChoice } from "../../src/config/tenant-select.ts"
import { MONO_THEME } from "../../src/components/theme.ts"
import { TenantPickerView, tenantOptions } from "../../src/components/tenant-picker.tsx"
import { destroySetup } from "../helpers/render.tsx"

const choices: TenantChoice[] = [
  { name: "applejack", description: "http://127.0.0.1:10010 - client token - no expiry", url: "http://127.0.0.1:10010", tokenKind: "client", expiresAt: null },
  { name: "derpy", description: "http://127.0.0.1:10000 - master token - expires 2026-12-01", url: "http://127.0.0.1:10000", tokenKind: "master", expiresAt: "2026-12-01T00:00:00Z" },
  { name: "rarity", description: "http://127.0.0.1:10020 - client token - no expiry", url: "http://127.0.0.1:10020", tokenKind: "client", expiresAt: null },
]

interface Picker {
  picked: (string | null)[]
  setup: Awaited<ReturnType<typeof testRender>>
  clock: ManualClock
}

async function picker(preselect: string | null): Promise<Picker> {
  const picked: (string | null)[] = []
  const clock = new ManualClock()
  const setup = await testRender(
    <TenantPickerView choices={choices} preselect={preselect} theme={MONO_THEME} onDone={(tenant) => picked.push(tenant)} width={80} />,
    { width: 80, height: 16, exitOnCtrlC: false, clock },
  )
  await setup.renderOnce()
  return { picked, setup, clock }
}

describe("tenant picker", () => {
  test("lists every tenant with URL, token kind and expiry", async () => {
    const { setup } = await picker(null)
    try {
      const frame = setup.captureCharFrame()
      expect(frame).toContain("choose a tenant (Enter picks, Esc quits)")
      for (const choice of choices) expect(frame).toContain(choice.name)
      expect(frame).toContain("master token - expires 2026-12-01")
      expect(tenantOptions(choices).map((option) => option.value)).toEqual(["applejack", "derpy", "rarity"])
    } finally {
      await destroySetup(setup)
    }
  })

  test("NO_COLOR (MONO_THEME): no chromatic colour and no explicit white text, the selection included", async () => {
    // The <select> defaults are white text and a yellow (#FFFF00) selection on #334455.
    const { setup } = await picker("derpy")
    try {
      const spans = setup.captureSpans().lines.flatMap((line) => line.spans.filter((span) => span.text.trim().length > 0))
      const chromatic = spans.filter((span) => {
        const [r, g, b, a] = span.fg.toInts()
        const [br, bg, bb, ba] = span.bg.toInts()
        return (a > 0 && (r !== g || g !== b)) || (ba > 0 && (br !== bg || bg !== bb))
      })
      expect(chromatic.map((span) => span.text)).toEqual([])
      expect(spans.filter((span) => span.fg.intent !== "default").map((span) => span.text)).toEqual([])
      expect(spans.some((span) => span.text.includes("derpy"))).toBe(true)
    } finally {
      await destroySetup(setup)
    }
  })

  test("Enter picks the preselected lastTenant", async () => {
    const { setup, picked } = await picker("derpy")
    try {
      act(() => setup.mockInput.pressEnter())
      await setup.renderOnce()
      expect(picked).toEqual(["derpy"])
    } finally {
      await destroySetup(setup)
    }
  })

  test("arrow keys move, Enter picks", async () => {
    const { setup, picked } = await picker("derpy")
    try {
      act(() => setup.mockInput.pressArrow("down"))
      act(() => setup.mockInput.pressEnter())
      await setup.renderOnce()
      expect(picked).toEqual(["rarity"])
    } finally {
      await destroySetup(setup)
    }
  })

  test("Esc quits with null", async () => {
    const { setup, picked, clock } = await picker("derpy")
    try {
      act(() => setup.mockInput.pressEscape())
      // A lone ESC is told from an escape sequence by the input parser's 20 ms timeout.
      act(() => clock.advance(25))
      await setup.renderOnce()
      expect(picked).toEqual([null])
    } finally {
      await destroySetup(setup)
    }
  })
})
