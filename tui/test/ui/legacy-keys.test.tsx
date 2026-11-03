/**
 * Keys inside tmux (design 3.10.4; review finding media-ux-6). tmux, and every terminal without the kitty keyboard
 * protocol (KKP), delivers the legacy encoding: there kitty sends Ctrl+- as "-", Ctrl+. as "." and Ctrl+Enter as a
 * plain CR, while Ctrl+_ / Ctrl+/ / Ctrl+Shift+- arrive as 0x1F and Ctrl+Y as 0x19. So every KKP-only key has an
 * alternative that tmux delivers, the alternatives work under KKP as well, and the F1 help, the footer and the
 * README mark the KKP-only keys.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { footerHint } from "../../src/controller/footer-hint.ts"
import { KEYMAP } from "../../src/controller/keymap.ts"
import { buildPanel } from "../../src/controller/panel-content.ts"
import type { Action } from "../../src/state/actions.ts"
import { createInitialState, type AppState, type TurnId, type UiState } from "../../src/state/model.ts"
import { reduce } from "../../src/state/reducer.ts"
import type { WireApproval } from "../../src/transport/types.ts"
import { renderApp, type AppHarness } from "../helpers/render.tsx"
import { fakeServerInfo } from "../mocks/fake-transport.ts"

let app: AppHarness | null = null

afterEach(async () => {
  await app?.dispose()
  app = null
})

/** Every key KEYMAP marks as needing KKP. */
const KKP_ONLY = KEYMAP.flatMap((entry) => [...(entry.kkpOnly ?? [])])

describe("KEYMAP", () => {
  test("the KKP-only keys are marked, and each of their actions has a key that arrives in the legacy encoding", () => {
    expect(new Set(KKP_ONLY)).toEqual(new Set(["Shift+Enter", "Ctrl+Enter", "Alt+]", "Ctrl+-", "Ctrl+."]))
    for (const entry of KEYMAP) {
      const kkpOnly = entry.kkpOnly ?? []
      for (const key of kkpOnly) expect(entry.keys).toContain(key)
      if (kkpOnly.length > 0) expect(entry.keys.filter((key) => !kkpOnly.includes(key)).length).toBeGreaterThan(0)
    }
    const keysOf = (action: string) => KEYMAP.find((entry) => entry.action === action)?.keys
    expect(keysOf("undo")).toEqual(["Ctrl+_", "Ctrl+/", "Ctrl+-"])
    expect(keysOf("redo")).toEqual(["Ctrl+Y", "Ctrl+."])
    expect(keysOf("interrupt_send")).toEqual(["Ctrl+S", "Ctrl+Enter"])
    // No alternative takes a key another entry already has.
    const all = KEYMAP.flatMap((entry) => entry.keys)
    expect(all.length).toBe(new Set(all).size)
  })
})

describe("legacy encoding (inside tmux)", () => {
  test("Ctrl+_ (0x1F) undoes and Ctrl+Y (0x19) redoes", async () => {
    app = await renderApp()
    await app.type("abc")
    await app.press("\x1f")
    expect(app.runtime.composer.getText().length).toBeLessThan(3)
    await app.press("\x19")
    expect(app.runtime.composer.getText()).toBe("abc")
  })

  test("a draft cleared with Ctrl+C comes back with Ctrl+_ and goes again with Ctrl+Y", async () => {
    app = await renderApp()
    await app.type("my long draft")
    await app.press("c", { ctrl: true })
    expect(app.runtime.composer.getText()).toBe("")
    await app.press("\x1f")
    expect(app.runtime.composer.getText()).toBe("my long draft")
    await app.press("\x19")
    expect(app.runtime.composer.getText()).toBe("")
  })
})

describe("kitty keyboard protocol", () => {
  test("the alternatives work under KKP too: Ctrl+/ and Ctrl+Shift+- undo, Ctrl+Y redoes; Ctrl+- and Ctrl+. as before", async () => {
    app = await renderApp({ kittyKeyboard: true })
    await app.type("abc")
    await app.press("/", { ctrl: true })
    expect(app.runtime.composer.getText().length).toBeLessThan(3)
    await app.press("y", { ctrl: true })
    expect(app.runtime.composer.getText()).toBe("abc")
    await app.press("-", { ctrl: true, shift: true })
    expect(app.runtime.composer.getText().length).toBeLessThan(3)
    await app.press(".", { ctrl: true })
    expect(app.runtime.composer.getText()).toBe("abc")
    await app.press("-", { ctrl: true })
    expect(app.runtime.composer.getText().length).toBeLessThan(3)
    // nothing was typed by any of these keys
    await app.press("y", { ctrl: true })
    expect(app.runtime.composer.getText()).toBe("abc")
  })
})

const T0 = 1_000_000

function state(ui: Partial<UiState>, ...actions: Action[]): AppState {
  const initial = createInitialState({ profile: "test", tenant: "derpy", view: { width: 160, height: 30 }, ui, at: T0 })
  return [{ type: "conn/ready", info: fakeServerInfo(), at: T0 } as Action, ...actions].reduce(reduce, initial)
}

describe("help and footer", () => {
  test("F1 help marks each KKP-only key and says what (KKP) means", () => {
    for (const ui of [{ tmux: true }, { tmux: false, kkpRelease: true }]) {
      const lines = buildPanel("help", state(ui), { width: 120, now: T0, pttKeys: ["f9", "ctrl+space"], voice: null })
      const text = lines.join("\n")
      for (const key of KKP_ONLY) expect(text).toContain(`${key} (KKP)`)
      expect(text).toContain("Ctrl+_, Ctrl+/, Ctrl+- (KKP)")
      expect(text).toContain("Ctrl+Y, Ctrl+. (KKP)")
      expect(text).toContain("Ctrl+S, Ctrl+Enter (KKP)")
      expect(lines.some((line) => line.includes("(KKP): only with the kitty keyboard protocol"))).toBe(true)
      // the keys that arrive in the legacy encoding are never marked
      for (const key of ["Ctrl+S", "Ctrl+_", "Ctrl+/", "Ctrl+Y", "Alt+J", "Ctrl+J", "Alt+Enter", "Ctrl+Space"]) {
        expect(text).not.toContain(`${key} (KKP)`)
      }
    }
  })

  test("the footer offers only keys that arrive where it is shown", () => {
    const approval = (id: string): Action => {
      const wire: WireApproval = { id, threadId: null, toolName: "shell", description: "d", parameters: "{}", allowAlways: true, truncated: false, source: "live" }
      return { type: "evt", event: { kind: "turn.approval", turnId: null, approval: wire }, at: T0 }
    }
    const running: Action = { type: "turn/created", turnId: "t-1" as TurnId, text: "hi", images: [], at: T0 }
    const rows = (ui: Partial<UiState>) =>
      [
        state(ui),
        state(ui, running),
        state(ui, approval("a"), approval("b")),
        state(ui, { type: "voice/state", patch: { phase: "recording", mode: "toggle", startedAt: T0 }, at: T0 }),
      ].map((s) => footerHint(s, { now: T0 }))
    for (const ui of [{ tmux: true, kkpRelease: true }, { tmux: false, kkpRelease: false }]) {
      for (const row of rows(ui)) for (const key of KKP_ONLY) expect([row, row.includes(key)]).toEqual([row, false])
    }
    // Only where KKP is live outside tmux does a KKP-only key appear: the next-approval key.
    expect(rows({ tmux: false, kkpRelease: true }).join("\n")).toContain("Alt+] next")
  })

  test("the README key table marks the same keys", () => {
    const readme = readFileSync(join(import.meta.dir, "..", "..", "README.md"), "utf8")
    const table = readme.slice(readme.indexOf("## Keys and the composer"), readme.indexOf("The composer wraps words"))
    for (const key of KKP_ONLY) expect(table).toContain(`${key} (KKP)`)
    expect(table).not.toContain("also inside tmux |")
    expect(table).toContain("Ctrl+Y")
  })
})
