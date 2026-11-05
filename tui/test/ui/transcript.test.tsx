/**
 * Transcript records and the header (design 3.3, 3.5, 3.6.1, 3.12.2, 11.3.4): every phase is a word ("queued #1",
 * "still running; the result will arrive here", "interrupted", "failed: ..."), controls are one line with their
 * ack, the header names the connection state and "second window"; Ctrl+C twice on an empty composer quits; while
 * not connected Enter keeps the text.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { NOTES } from "../../src/state/correlate.ts"
import { renderApp, type AppHarness } from "../helpers/render.tsx"
import { FakeTransport, turnId } from "../mocks/fake-transport.ts"

let app: AppHarness | null = null

afterEach(async () => {
  await app?.dispose()
  app = null
})

describe("turn labels", () => {
  test("a second message while one runs shows queued #1", async () => {
    const transport = new FakeTransport()
    app = await renderApp({ transport, width: 100, height: 30 })
    await app.type("first")
    await app.enter()
    await app.emit({ kind: "turn.started", turnId: turnId("t-1") })
    await app.type("second")
    await app.enter()
    const frame = await app.render()
    expect(frame).toContain("queued #1")
    expect(frame).toContain("working, 0 s")
  })

  test("detached shows background_running; the late final seals it", async () => {
    const transport = new FakeTransport()
    app = await renderApp({ transport, width: 100, height: 30 })
    await app.type("long job")
    await app.enter()
    const id = turnId("t-1")
    await app.emit({ kind: "turn.started", turnId: id }, { kind: "turn.finished", turnId: id, outcome: "detached" })
    let frame = await app.render()
    expect(frame).toContain(NOTES.detached)
    await app.emit({ kind: "turn.final", turnId: id, text: "job finished" }, { kind: "turn.finished", turnId: id, outcome: "replied" })
    frame = await app.render()
    expect(frame).toContain("job finished")
    expect(frame).not.toContain(NOTES.detached)
  })

  test("an interrupt is one line with its ack; the stopped turn reads interrupted", async () => {
    const transport = new FakeTransport()
    app = await renderApp({ transport, width: 100, height: 30 })
    await app.type("count to a million")
    await app.enter()
    const turn = turnId("t-1")
    await app.emit({ kind: "turn.started", turnId: turn }, { kind: "turn.delta", turnId: turn, text: "1 2 3" })
    await app.type("/stop")
    await app.enter()
    const control = turnId("t-2")
    await app.emit(
      { kind: "turn.finished", turnId: turn, outcome: "interrupted" },
      { kind: "turn.final", turnId: control, text: "Interrupted." },
      { kind: "turn.finished", turnId: control, outcome: "replied" },
    )
    const frame = await app.render()
    expect(frame).toContain("/interrupt -> Interrupted.")
    expect(frame).toContain("1 2 3")
    expect(frame).toContain("  interrupted")
  })

  test("a failed turn shows its error, never as assistant text", async () => {
    const transport = new FakeTransport()
    app = await renderApp({ transport, width: 100, height: 30 })
    await app.type("do it")
    await app.enter()
    const id = turnId("t-1")
    await app.emit({ kind: "turn.error", turnId: id, code: "agent_error", message: "the tool crashed", retryable: false })
    const frame = await app.render()
    expect(frame).toContain("failed: the tool crashed")
    expect(frame).not.toContain("lunarwing ▏ the tool crashed")
  })
})

describe("speaker gutter", () => {
  test("live reasoning is one updating multiline block, separate from the answer and hidden after completion", async () => {
    app = await renderApp({ width: 100, height: 30 })
    await app.type("think carefully")
    await app.enter()
    const id = turnId("t-1")
    await app.emit({ kind: "turn.reasoning", turnId: id, text: "previous snapshot" })
    expect(await app.render()).toContain("reasoning: previous snapshot")
    await app.emit(
      { kind: "turn.activity", turnId: id, activity: { kind: "tool_started", text: "http" } },
      { kind: "turn.reasoning", turnId: id, text: "updated snapshot\nlatest tail" },
    )
    const frame = await app.render()
    expect(frame).not.toContain("previous snapshot")
    expect(frame.match(/reasoning:/g)).toHaveLength(1)
    expect(frame).toContain("reasoning: updated snapshot")
    expect(frame).toMatch(/\n +latest tail/)
    expect(frame).toContain(": http")
    expect(frame).not.toContain("lunarwing ▏")
    await app.emit(
      { kind: "turn.final", turnId: id, text: "final answer" },
      { kind: "turn.finished", turnId: id, outcome: "replied" },
      { kind: "turn.reasoning", turnId: id, text: "late snapshot" },
    )
    const done = await app.render()
    expect(done).toContain("lunarwing ▏ final answer")
    expect(done).not.toContain("reasoning:")
    expect(done).not.toContain("late snapshot")
  })

  test("continuation lines of a multi-line or wrapped message start under the text, not at column 0", async () => {
    const transport = new FakeTransport()
    app = await renderApp({ transport, width: 60, height: 30 })
    app.runtime.composer.insertText("first line of a message\nsecond line here\nthird", { separate: true })
    await app.enter()
    const reply = "echo: " + Array.from({ length: 16 }, (_, index) => `word${index}`).join(" ")
    await app.emit(
      { kind: "turn.activity", turnId: turnId("t-1"), activity: { kind: "tool_completed", text: "shell: " + "x".repeat(70), ok: true } },
      { kind: "turn.final", turnId: turnId("t-1"), text: reply },
      { kind: "turn.finished", turnId: turnId("t-1"), outcome: "replied" },
    )
    const rows = (await app.render()).split("\n")
    const first = rows.findIndex((row) => row.startsWith("you ▏ first line of a message"))
    expect(first).toBeGreaterThanOrEqual(0)
    expect(rows[first + 1]).toStartWith(" ".repeat(6) + "second line here")
    expect(rows[first + 2]).toStartWith(" ".repeat(6) + "third")
    const answer = rows.findIndex((row) => row.startsWith("lunarwing ▏ echo: word0"))
    expect(answer).toBeGreaterThan(first)
    expect(rows[answer + 1]).toMatch(/^ {12}word\d+/u)
    // an indented activity line keeps its indentation when it wraps
    const activity = rows.findIndex((row) => row.startsWith("  : shell:"))
    expect(activity).toBeGreaterThan(answer)
    expect(rows[activity + 1]).toMatch(/^ {2}x+/u)
  })
})

describe("header and quitting", () => {
  test("the header names a second window and other-thread events", async () => {
    app = await renderApp({ primary: false, width: 120, height: 24 })
    await app.emit({ kind: "other_thread", threadId: "00000000-0000-4000-8000-0000000000aa" })
    const frame = await app.render()
    const header = frame.split("\n")[0] ?? ""
    expect(header).toContain("LunarWing derpy | ready | engine v2 | exact")
    expect(header).toContain("second window")
    expect(header).toContain("+1 in other threads")
  })

  test("Ctrl+C twice on an empty composer quits; once only warns", async () => {
    app = await renderApp()
    await app.press("c", { ctrl: true })
    expect(app.quits).toBe(0)
    expect(app.store.getState().notices.at(-1)?.text).toBe("press Ctrl+C again to quit")
    await app.advance(2_500)
    await app.press("c", { ctrl: true })
    expect(app.quits).toBe(0)
    await app.press("c", { ctrl: true })
    await app.settle()
    expect(app.quits).toBe(1)
  })

  test("while not connected Enter keeps the text", async () => {
    const transport = new FakeTransport()
    transport.connectScript.push("hang")
    app = await renderApp({ transport })
    await app.type("hello?")
    await app.enter()
    expect(transport.sentTexts).toEqual([])
    expect(app.runtime.composer.getText()).toBe("hello?")
    expect(app.store.getState().notices.at(-1)?.text).toContain("not connected")
    const frame = await app.render()
    expect(frame.split("\n")[0]).toContain("connecting")
  })
})
