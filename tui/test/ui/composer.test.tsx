/**
 * The composer (design 3.10.5, UX-01, 11.3.4): a word-wrapping textarea of 1-6 rows where Enter sends, Ctrl+J and
 * Alt+Enter insert a newline, Ctrl+_ undoes (also inside tmux), clears are undoable and recallable, and the byte
 * counter appears above 80 % of the content limit.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { renderApp, type AppHarness } from "../helpers/render.tsx"
import { FakeTransport } from "../mocks/fake-transport.ts"
import { rows } from "../helpers/frames.ts"

let app: AppHarness | null = null

afterEach(async () => {
  await app?.dispose()
  app = null
})

/** Rows between the composer's top and bottom border (inner text rows). */
function composerRows(frame: string): string[] {
  const all = rows(frame)
  const bottom = all.findLastIndex((row) => row.startsWith("└"))
  const top = all.slice(0, bottom).findLastIndex((row) => row.startsWith("┌"))
  return all.slice(top + 1, bottom)
}

describe("composer", () => {
  test("a 120 s transcript wraps and stays fully reviewable", async () => {
    app = await renderApp({ width: 80, height: 30 })
    const words = Array.from({ length: 330 }, (_, index) => `word${index}`)
    const transcript = words.join(" ")
    app.runtime.composer.insertText(transcript, { separate: true })
    const frame = await app.render()
    expect(app.runtime.composer.getText()).toBe(transcript)
    expect(app.store.getState().composer.lines).toBeGreaterThan(6)
    expect(composerRows(frame)).toHaveLength(6)
    // Nothing is cut: the end of the text is on screen (the cursor follows it) and wrapped by words.
    expect(frame).toContain("word329")
  })

  test("every typed word is on screen and the right border stays (80 and 40 columns, a single long line too)", async () => {
    for (const width of [80, 40, 200]) {
      app = await renderApp({ width, height: 24 })
      const words = Array.from({ length: 20 }, (_, index) => `${String.fromCharCode(97 + index).repeat(4)}`)
      const text = width === 200 ? "x".repeat(width - 5) : words.join(" ")
      app.runtime.composer.insertText(text, { separate: true })
      const frame = await app.render()
      const inner = composerRows(frame)
      // every row of the composer still ends with its border: nothing is drawn past it
      for (const row of inner) expect(row.endsWith("│")).toBe(true)
      const shown = inner.map((row) => row.slice(1, -1)).join(" ")
      if (width === 200) expect(shown.replace(/[^x]/gu, "")).toBe(text)
      else for (const word of words) expect(shown).toContain(word)
      await app.dispose()
      app = null
    }
  })

  test("a dictated sentence is fully visible while a second recording runs (the VU meter takes 12 columns)", async () => {
    app = await renderApp({ width: 80, height: 24 })
    const sentence = "please check the ports registry for tenant derpy and then restart the health metrics listener on the host"
    app.runtime.composer.insertText(sentence, { separate: true })
    app.store.dispatch({ type: "voice/state", patch: { phase: "recording", mode: "toggle", startedAt: app.clock.now() }, at: app.clock.now() })
    const frame = await app.render()
    const inner = composerRows(frame)
    for (const row of inner) expect(row.endsWith("│")).toBe(true)
    const shown = inner.map((row) => row.slice(1, -1)).join(" ").replace(/\s+/gu, " ")
    for (const word of sentence.split(" ")) expect(shown).toContain(word)
  })

  test("the masked credential line keeps its character count inside the border at 40 columns", async () => {
    app = await renderApp({ width: 40, height: 24 })
    await app.emit({ kind: "turn.auth_required", turnId: null, extensionName: "github-enterprise" })
    app.runtime.credential.push("ghp_0123456789abcdefghijk")
    app.store.dispatch({ type: "composer/credential_chars", chars: app.runtime.credential.length, at: app.clock.now() })
    const frame = await app.render()
    const inner = composerRows(frame)
    expect(inner.some((row) => row.includes("**** (25 chars)"))).toBe(true)
    for (const row of inner) expect(row.endsWith("│")).toBe(true)
    expect(frame).not.toContain("ghp_")
  })

  test("a multi-line paste keeps its line breaks", async () => {
    app = await renderApp()
    await app.paste("line one\nline two\nline three")
    const frame = await app.render()
    expect(app.runtime.composer.getText()).toBe("line one\nline two\nline three")
    const inner = composerRows(frame).join("\n")
    expect(inner).toContain("line one")
    expect(inner).toContain("line three")
  })

  test("Enter sends; Ctrl+J (LF) inserts a newline", async () => {
    const transport = new FakeTransport()
    app = await renderApp({ transport })
    await app.type("first")
    await app.press("\n")
    await app.type("second")
    expect(transport.sentTexts).toEqual([])
    await app.enter()
    expect(transport.sentTexts).toEqual(["first\nsecond"])
  })

  test("Alt+Enter (ESC CR) inserts a newline", async () => {
    const transport = new FakeTransport()
    app = await renderApp({ transport })
    await app.type("a")
    await app.press("\x1b\r")
    await app.type("b")
    expect(app.runtime.composer.getText()).toBe("a\nb")
    expect(transport.sentTexts).toEqual([])
  })

  test("Ctrl+_ (0x1F, what Ctrl+- sends inside tmux) undoes", async () => {
    app = await renderApp()
    await app.type("abc")
    await app.press("\x1f")
    expect(app.runtime.composer.getText().length).toBeLessThan(3)
  })

  test("a Ctrl+C clear is undoable and recallable with Up", async () => {
    app = await renderApp()
    await app.type("my long draft")
    await app.press("c", { ctrl: true })
    expect(app.runtime.composer.getText()).toBe("")
    expect(app.store.getState().composer.history).toEqual(["my long draft"])

    await app.press("\x1f")
    expect(app.runtime.composer.getText()).toBe("my long draft")

    await app.press("c", { ctrl: true })
    await app.press("ARROW_UP")
    expect(app.runtime.composer.getText()).toBe("my long draft")
    await app.press("ARROW_DOWN")
    expect(app.runtime.composer.getText()).toBe("")
  })

  test("Up recalls sent messages in order", async () => {
    const transport = new FakeTransport()
    app = await renderApp({ transport })
    await app.type("one")
    await app.enter()
    await app.type("two")
    await app.enter()
    await app.press("ARROW_UP")
    expect(app.runtime.composer.getText()).toBe("two")
    await app.press("ARROW_UP")
    expect(app.runtime.composer.getText()).toBe("one")
  })

  test("the byte counter appears above 80 % of the content limit", async () => {
    const transport = new FakeTransport({ limits: { maxContentBytes: 10_240 } })
    app = await renderApp({ transport })
    app.runtime.composer.insertText("x".repeat(7_000))
    let frame = await app.render()
    expect(frame).not.toContain("KiB / 10 KiB")
    app.runtime.composer.insertText("y".repeat(2_000))
    frame = await app.render()
    expect(frame).toContain("9 KiB / 10 KiB")
  })

  test("text over the limit is refused at send, never truncated", async () => {
    const transport = new FakeTransport({ limits: { maxContentBytes: 1_024 } })
    app = await renderApp({ transport })
    app.runtime.composer.insertText("z".repeat(2_000))
    await app.enter()
    expect(transport.sentTexts).toEqual([])
    expect(app.runtime.composer.getText()).toHaveLength(2_000)
    expect(app.store.getState().notices.at(-1)?.text).toContain("the limit is 1 KiB")
  })
})
