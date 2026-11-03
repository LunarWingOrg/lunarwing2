/**
 * Key routing (design 3.10.4, 11.3.4; ported from v4:tui/test/input-keys.test.tsx).
 *
 * The first four cases pin OpenTUI behaviour the keymap relies on, with a bare probe: the global useKeyboard
 * handler sees plain Return even while an editable is focused (so global keys run before the composer); Ctrl+Return
 * reports ctrl only under the kitty keyboard protocol, which is why Ctrl+S is the portable interrupt-and-send key;
 * typed characters reach the global handler too, so the keymap must claim keys precisely. (The vibetui4 header
 * described an <input> composer driven from useKeyboard; the composer is now a <textarea> whose own key bindings
 * submit on Enter, controller/composer-handle.ts.)
 *
 * The other cases run the real app: F9 and NUL never reach the composer, Ctrl+S interrupts before it sends, and
 * Alt+Y/Alt+A are claimed only while an approval is pending.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { useKeyboard } from "@opentui/react"
import { testRender } from "@opentui/react/test-utils"
import { act, useState, type ReactNode } from "react"
import { FakeTransport, turnId } from "../mocks/fake-transport.ts"
import { destroySetup, renderApp, type AppHarness } from "../helpers/render.tsx"

function KeyProbe(props: { events: string[] }): ReactNode {
  const [, setValue] = useState("")
  useKeyboard((key) => {
    const tag = key.ctrl ? "ctrl+" : ""
    props.events.push(`${tag}${key.name}`)
  })
  return (
    <box style={{ border: true }}>
      <input placeholder="type here" focused onInput={setValue} />
    </box>
  )
}

describe("OpenTUI key routing the keymap relies on", () => {
  test("plain Return reaches useKeyboard while an editable is focused", async () => {
    const events: string[] = []
    const setup = await testRender(<KeyProbe events={events} />, { width: 40, height: 8 })
    try {
      await setup.renderOnce()
      act(() => setup.mockInput.pressEnter())
      await setup.renderOnce()
      expect(events).toContain("return")
    } finally {
      await destroySetup(setup)
    }
  })

  test("kitty protocol: Ctrl+Return reports ctrl=true", async () => {
    const events: string[] = []
    const setup = await testRender(<KeyProbe events={events} />, { width: 40, height: 8, kittyKeyboard: true })
    try {
      await setup.renderOnce()
      act(() => setup.mockInput.pressEnter({ ctrl: true }))
      await setup.renderOnce()
      expect(events).toContain("ctrl+return")
    } finally {
      await destroySetup(setup)
    }
  })

  test("legacy mode: Ctrl+Return is not distinguishable, hence the Ctrl+S key", async () => {
    const events: string[] = []
    const setup = await testRender(<KeyProbe events={events} />, { width: 40, height: 8 })
    try {
      await setup.renderOnce()
      act(() => {
        setup.mockInput.pressEnter({ ctrl: true })
        setup.mockInput.pressKey("s", { ctrl: true })
      })
      await setup.renderOnce()
      expect(events).toContain("return")
      expect(events).not.toContain("ctrl+return")
      expect(events).toContain("ctrl+s")
    } finally {
      await destroySetup(setup)
    }
  })

  test("typed characters reach the global hook too (the keymap must claim precisely)", async () => {
    const events: string[] = []
    const setup = await testRender(<KeyProbe events={events} />, { width: 40, height: 8 })
    try {
      await setup.renderOnce()
      await act(async () => {
        await setup.mockInput.typeText("abc")
      })
      await setup.renderOnce()
      expect(events).toEqual(["a", "b", "c"])
    } finally {
      await destroySetup(setup)
    }
  })
})

describe("app keys", () => {
  let app: AppHarness | null = null

  afterEach(async () => {
    await app?.dispose()
    app = null
  })

  test("F9 and NUL never reach the composer", async () => {
    app = await renderApp()
    await app.type("ab")
    await app.press("F9")
    app.setup.renderer.stdin.emit("data", Buffer.from([0x00]))
    await app.settle()
    await app.type("c")
    expect(app.runtime.composer.getText()).toBe("abc")
  })

  test("Ctrl+S sends the interrupt, then the message, in order", async () => {
    const transport = new FakeTransport()
    app = await renderApp({ transport })
    await app.type("use staging instead")
    await app.press("s", { ctrl: true })
    expect(transport.log.filter((m) => m === "interrupt" || m === "send")).toEqual(["interrupt", "send"])
    expect(transport.sentTexts).toEqual(["use staging instead"])
    const state = app.store.getState()
    const [control, message] = state.order.map((id) => state.turns[id])
    expect(control?.origin).toBe("control")
    expect(control?.control?.kind).toBe("interrupt")
    expect(message?.input?.text).toBe("use staging instead")
    expect(app.runtime.composer.getText()).toBe("")
  })

  test("Ctrl+S on an empty composer sends only the interrupt", async () => {
    const transport = new FakeTransport()
    app = await renderApp({ transport })
    await app.press("s", { ctrl: true })
    expect(transport.log.filter((m) => m === "interrupt" || m === "send")).toEqual(["interrupt"])
  })

  test("Alt+Y answers only while a prompt is pending; otherwise Alt+A is the textarea's line start", async () => {
    const transport = new FakeTransport()
    app = await renderApp({ transport })
    await app.type("hello")
    await app.press("a", { meta: true })
    await app.type("X")
    expect(app.runtime.composer.getText()).toBe("Xhello")
    await app.press("y", { meta: true })
    expect(transport.callsOf("approve")).toHaveLength(0)

    await app.emit({
      kind: "turn.approval",
      turnId: turnId("t-9"),
      approval: { id: "req-1", threadId: null, toolName: "shell", description: "Run a command", parameters: "{}", allowAlways: true, truncated: false, source: "live" },
    })
    await app.advance(1_000)
    await app.press("y", { meta: true })
    expect(transport.callsOf("approve").map((call) => call.req.decision)).toEqual(["approve"])
    expect(app.runtime.composer.getText()).toBe("Xhello")
  })
})
