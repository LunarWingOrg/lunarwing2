/**
 * NO_COLOR (design 3.12.2, UX-15, 11.3.4): with NO_COLOR set (color = "auto") every cell's foreground and
 * background is achromatic, and every state still carries its text label (APPROVAL, (no mic), OFFLINE, notice
 * levels). The same frame with colour on does contain chromatic colours, so the check can fail.
 */
import { afterEach, describe, expect, test } from "bun:test"
import type { CapturedFrame, RGBA } from "@opentui/core"
import { colorEnabled, resolveTheme, MONO_THEME } from "../../src/components/theme.ts"
import { TransportError } from "../../src/transport/errors.ts"
import type { WireApproval } from "../../src/transport/types.ts"
import { renderApp, type AppHarness, type RenderAppOptions } from "../helpers/render.tsx"
import { FakeTransport, turnId } from "../mocks/fake-transport.ts"

let app: AppHarness | null = null

afterEach(async () => {
  await app?.dispose()
  app = null
})

const approval: WireApproval = {
  id: "req-1",
  threadId: null,
  toolName: "shell",
  description: "Run a command",
  parameters: '{"command":"ls"}',
  allowAlways: true,
  truncated: false,
  source: "live",
}

function chromatic(color: RGBA): boolean {
  const [r, g, b, a] = color.toInts()
  return a > 0 && (r !== g || g !== b)
}

/** Text of every span drawn with a chromatic foreground or background. */
function colouredSpans(frame: CapturedFrame): string[] {
  return frame.lines.flatMap((line) =>
    line.spans.filter((span) => span.text.trim().length > 0 && (chromatic(span.fg) || chromatic(span.bg))).map((span) => span.text),
  )
}

/** Text of every non-blank span not drawn in the terminal's default foreground (OpenTUI's own default is white). */
function explicitForeground(frame: CapturedFrame): string[] {
  return frame.lines.flatMap((line) =>
    line.spans.filter((span) => span.text.trim().length > 0 && span.fg.intent !== "default").map((span) => `${span.text} ${span.fg.toString()}`),
  )
}

async function busyScreen(opts: RenderAppOptions): Promise<AppHarness> {
  const transport = new FakeTransport()
  const harness = await renderApp({ transport, width: 100, height: 30, ...opts })
  await harness.type("hello")
  await harness.enter()
  await harness.emit(
    { kind: "turn.final", turnId: turnId("t-1"), text: "a reply" },
    { kind: "turn.finished", turnId: turnId("t-1"), outcome: "replied" },
    { kind: "turn.approval", turnId: turnId("t-1"), approval },
    { kind: "notice", level: "error", source: "server", text: "something failed" },
  )
  await harness.render()
  return harness
}

describe("NO_COLOR", () => {
  test("resolveTheme honours NO_COLOR only in auto mode", () => {
    expect(colorEnabled({ color: "auto", noColor: "1" })).toBe(false)
    expect(colorEnabled({ color: "auto", noColor: "" })).toBe(true)
    expect(colorEnabled({ color: "always", noColor: "1" })).toBe(true)
    expect(colorEnabled({ color: "never" })).toBe(false)
    expect(resolveTheme({ color: "auto", noColor: "1" })).toBe(MONO_THEME)
  })

  test("frames carry no colour and every state keeps its label", async () => {
    app = await busyScreen({ noColor: "1" })
    const frame = app.frame()
    expect(colouredSpans(app.setup.captureSpans())).toEqual([])
    expect(frame).toContain("APPROVAL: shell")
    expect(frame).toContain("(no mic)")
    expect(frame).toContain("error: something failed")
    expect(frame).toContain("you ▏ hello")
    expect(frame).toContain("lunarwing ▏ a reply")
  })

  test("every glyph uses the terminal's default foreground, so a light terminal background stays readable", async () => {
    // Without an fg OpenTUI paints explicit white (RGB 255,255,255): invisible on a light background.
    app = await busyScreen({ noColor: "1" })
    await app.type("typed text")
    await app.render()
    expect(explicitForeground(app.setup.captureSpans())).toEqual([])
    await app.press("v", { meta: true })
    await app.render()
    expect(app.store.getState().panel?.kind).toBe("approval")
    expect(explicitForeground(app.setup.captureSpans())).toEqual([])
    await app.escape()
    await app.press("F1")
    await app.render()
    expect(explicitForeground(app.setup.captureSpans())).toEqual([])
  })

  test("the colour theme does use explicit colours (the default-foreground check can fail)", async () => {
    app = await busyScreen({})
    await app.type("typed text")
    await app.render()
    expect(explicitForeground(app.setup.captureSpans()).length).toBeGreaterThan(0)
  })

  test("the same screen with colour on is coloured (the check can fail)", async () => {
    app = await busyScreen({})
    expect(colouredSpans(app.setup.captureSpans()).length).toBeGreaterThan(0)
  })

  test("OFFLINE is a word in the header", async () => {
    const transport = new FakeTransport()
    transport.connectScript.push(new TransportError("forbidden", "port 10000 is held by uid 1008, expected 1010 (derpy)"))
    app = await renderApp({ transport, noColor: "1", width: 100, height: 24 })
    const frame = await app.render()
    expect(frame.split("\n")[0]).toContain("OFFLINE")
    expect(frame).toContain("offline: port 10000 is held by uid 1008, expected 1010 (derpy) - /transport reconnect")
    expect(colouredSpans(app.setup.captureSpans())).toEqual([])
  })
})
