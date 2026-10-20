/**
 * Behavioral pin: how <input> and useKeyboard share the Return key.
 *
 * The App drives submission entirely from useKeyboard (plain Enter = send,
 * Ctrl+Enter / Ctrl+S = interrupt + send). These tests pin that:
 * 1. useKeyboard DOES receive plain Return while an <input> is focused.
 * 2. Ctrl+Return carries `ctrl: true` under the kitty keyboard protocol.
 * 3. In legacy (non-kitty) mode the ctrl modifier is NOT reported on
 *    Return — which is exactly why the App also binds Ctrl+S.
 * 4. Typed characters reach the global hook too (App must filter precisely).
 */

import { describe, expect, test } from "bun:test"
import { testRender } from "@opentui/react/test-utils"
import { useKeyboard } from "@opentui/react"
import { useState, type ReactNode } from "react"

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

describe("input + useKeyboard key routing", () => {
  test("plain Return reaches useKeyboard while input is focused", async () => {
    const events: string[] = []
    const setup = await testRender(<KeyProbe events={events} />, { width: 40, height: 8 })
    try {
      await setup.renderOnce()
      setup.mockInput.pressEnter()
      await setup.flush()
      expect(events).toContain("return")
    } finally {
      setup.renderer.destroy()
    }
  })

  test("kitty protocol: Ctrl+Return reports ctrl=true", async () => {
    const events: string[] = []
    const setup = await testRender(<KeyProbe events={events} />, {
      width: 40,
      height: 8,
      kittyKeyboard: true,
    })
    try {
      await setup.renderOnce()
      setup.mockInput.pressEnter({ ctrl: true })
      await setup.flush()
      expect(events).toContain("ctrl+return")
    } finally {
      setup.renderer.destroy()
    }
  })

  test("legacy mode: Ctrl+Return is NOT distinguishable — hence Ctrl+S fallback", async () => {
    const events: string[] = []
    const setup = await testRender(<KeyProbe events={events} />, { width: 40, height: 8 })
    try {
      await setup.renderOnce()
      setup.mockInput.pressEnter({ ctrl: true })
      setup.mockInput.pressKey("s", { ctrl: true })
      await setup.flush()
      expect(events).toContain("return") // modifier lost
      expect(events).not.toContain("ctrl+return")
      expect(events).toContain("ctrl+s") // fallback key carries the modifier
    } finally {
      setup.renderer.destroy()
    }
  })

  test("typed characters reach the global hook too (App must filter precisely)", async () => {
    const events: string[] = []
    const setup = await testRender(<KeyProbe events={events} />, { width: 40, height: 8 })
    try {
      await setup.renderOnce()
      await setup.mockInput.typeText("abc")
      await setup.flush()
      expect(events).toEqual(["a", "b", "c"])
    } finally {
      setup.renderer.destroy()
    }
  })
})
